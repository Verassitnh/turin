//! Port of `upstream/packages/provider-opencode/src/server/OpenCodeServerLedger.ts`.
//!
//! A local `opencode serve` runs in its own process group, so it outlives a
//! T3 server that is SIGKILLed. Each spawn is recorded under the state
//! directory and removed after a graceful stop. The next start stops whatever
//! a dead server left behind.
//!
//! [`OpenCodeServerLedger::make`] does not reap. [`OpenCodeServerLedger::start`]
//! reaps in the background so startup is not blocked. Windows is not recorded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;

use crate::process_group::{SIGKILL, SIGTERM, process_group_exists, signal_process_group};

const STOP_POLL_ATTEMPTS: u32 = 40;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    pid: i64,
    #[serde(rename = "startTime")]
    start_time: String,
}

/// Unknown keys are ignored, as upstream's `Schema.Struct` decode does. A newer
/// build's entry must still be stopped, not deleted with its group running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    version: u64,
    pgid: i64,
    pid: i64,
    #[serde(rename = "startTime")]
    start_time: String,
    command: String,
    port: i64,
    #[serde(rename = "stateDir")]
    state_dir: String,
    owner: Identity,
}

#[derive(Debug, Clone)]
struct Observed {
    pid: u32,
    pgid: u32,
    start_time: String,
    command: String,
    zombie: bool,
}

struct Inner {
    state_dir: String,
    directory: PathBuf,
    owner: Option<Identity>,
    recordable: bool,
    /// The background sweep started by [`OpenCodeServerLedger::start`]. Held
    /// here so it is never detached. The sweep keeps a clone of the ledger, so
    /// the abort runs when the last reference drops, which is after the sweep.
    reap: parking_lot::Mutex<Option<AbortOnDropHandle<()>>>,
}

#[derive(Clone)]
pub struct OpenCodeServerLedger {
    inner: Arc<Inner>,
}

/// Deletes the ledger file after a graceful stop. Dropping it does not delete
/// the file: a crash must leave the entry for the next start.
pub struct ServerLease {
    path: PathBuf,
    forgotten: AtomicBool,
}

impl ServerLease {
    pub fn forget(&self) {
        if self.forgotten.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("Could not forget a stopped OpenCode server: {err}");
        }
    }
}

impl OpenCodeServerLedger {
    /// Records servers under `state_dir`. Does not reap.
    pub async fn make(state_dir: impl Into<PathBuf>) -> Self {
        Self::make_with_owner(state_dir, std::process::id()).await
    }

    pub async fn make_with_owner(state_dir: impl Into<PathBuf>, owner_pid: u32) -> Self {
        let state_dir = state_dir.into();
        let state_dir_string = state_dir.to_string_lossy().into_owned();
        let directory = state_dir.join("opencode-servers");
        let recordable = cfg!(any(target_os = "linux", target_os = "macos"));
        let owner = if recordable {
            observe(owner_pid).await.map(|observed| Identity {
                pid: i64::from(owner_pid),
                start_time: observed.start_time,
            })
        } else {
            None
        };
        Self {
            inner: Arc::new(Inner {
                state_dir: state_dir_string,
                directory,
                owner,
                recordable,
                reap: parking_lot::Mutex::new(None),
            }),
        }
    }

    /// `make`, then reap recorded servers whose owner is gone. The reap runs
    /// in the background.
    pub async fn start(state_dir: impl Into<PathBuf>) -> Self {
        let ledger = Self::make(state_dir).await;
        let background = ledger.clone();
        let sweep = tokio::spawn(async move {
            background.reap_orphans().await;
        });
        *ledger.inner.reap.lock() = Some(AbortOnDropHandle::new(sweep));
        ledger
    }

    /// Records a spawned group. `args` is the argv after the binary, for
    /// example `["serve", "--hostname=127.0.0.1", "--port=4096"]`.
    /// The returned lease forgets the entry; call it only after the group has
    /// been signaled.
    pub async fn track(&self, pid: u32, port: u16, args: &[String]) -> Option<ServerLease> {
        if !self.inner.recordable || pid <= 1 {
            return None;
        }
        match self.track_inner(pid, port, args).await {
            Ok(lease) => lease,
            Err(err) => {
                tracing::warn!("Could not record an OpenCode server process: {err}");
                None
            }
        }
    }

    async fn track_inner(
        &self,
        pid: u32,
        port: u16,
        args: &[String],
    ) -> std::io::Result<Option<ServerLease>> {
        let leader = observe(pid).await;
        let members = if leader
            .as_ref()
            .is_some_and(|candidate| !candidate.zombie && candidate.pgid == pid)
        {
            vec![leader.expect("checked")]
        } else {
            observe_group(pid)
                .await
                .into_iter()
                .filter(|candidate| !candidate.zombie)
                .collect::<Vec<_>>()
        };
        let suffix = format!(" {}", args.join(" "));
        let index = members
            .iter()
            .position(|candidate| candidate.command.ends_with(&suffix))
            .unwrap_or(0);
        let member = if members.is_empty() {
            None
        } else {
            Some(members[index].clone())
        };
        let Some(owner) = self.inner.owner.clone() else {
            self.warn_unrecorded(pid, port);
            return Ok(None);
        };
        let Some(member) = member else {
            self.warn_unrecorded(pid, port);
            return Ok(None);
        };
        let entry = Entry {
            version: 1,
            pgid: i64::from(pid),
            pid: i64::from(member.pid),
            start_time: member.start_time,
            command: member.command,
            port: i64::from(port),
            state_dir: self.inner.state_dir.clone(),
            owner,
        };
        let path = self.inner.directory.join(format!("{pid}.json"));
        let json = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
        write_atomic(&path, &json)?;
        Ok(Some(ServerLease {
            path,
            forgotten: AtomicBool::new(false),
        }))
    }

    fn warn_unrecorded(&self, pid: u32, port: u16) {
        if process_group_exists(pid) {
            tracing::warn!(
                "Could not record an OpenCode server; it will keep running if this server crashes (pid={pid}, port={port})"
            );
        }
    }

    pub async fn reap_orphans(&self) {
        let names = match std::fs::read_dir(&self.inner.directory) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        let mut files = Vec::new();
        for entry in names.flatten() {
            let name = entry.file_name();
            if is_entry_file(&name.to_string_lossy()) {
                files.push(entry.path());
            }
        }
        // A JoinSet aborts its tasks when dropped, so cancelling the sweep also
        // cancels every entry it started.
        let mut joins = JoinSet::new();
        for path in files {
            let ledger = self.clone();
            joins.spawn(async move {
                ledger.reap_entry(&path).await;
            });
        }
        while joins.join_next().await.is_some() {}
    }

    async fn reap_entry(&self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!("Could not clean up a recorded OpenCode server: {err}");
                return;
            }
        };
        if let Some(entry) = decode_entry(&text)
            && entry.state_dir == self.inner.state_dir
        {
            if self.is_running(&entry.owner).await {
                return;
            }
            self.stop_orphan(&entry).await;
        }
        if let Err(err) = std::fs::remove_file(path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("Could not clean up a recorded OpenCode server: {err}");
        }
    }

    async fn is_running(&self, identity: &Identity) -> bool {
        let Ok(pid) = u32::try_from(identity.pid) else {
            return false;
        };
        observe(pid)
            .await
            .is_some_and(|observed| !observed.zombie && observed.start_time == identity.start_time)
    }

    async fn stop_orphan(&self, entry: &Entry) {
        let (Ok(pid), Ok(pgid)) = (u32::try_from(entry.pid), u32::try_from(entry.pgid)) else {
            return;
        };
        let Some(observed) = observe(pid).await else {
            return;
        };
        if observed.zombie
            || observed.start_time != entry.start_time
            || observed.pgid != pgid
            || observed.command != entry.command
        {
            return;
        }
        tracing::info!(
            "Stopping an OpenCode server left by a previous T3 Code server (pid={pgid}, port={})",
            entry.port
        );
        let _ = signal_process_group(pgid, SIGTERM);
        for _ in 0..STOP_POLL_ATTEMPTS {
            if !process_group_exists(pgid) {
                return;
            }
            tokio::time::sleep(STOP_POLL_INTERVAL).await;
        }
        if process_group_exists(pgid) {
            let _ = signal_process_group(pgid, SIGKILL);
        }
    }
}

fn decode_entry(text: &str) -> Option<Entry> {
    let entry: Entry = serde_json::from_str(text).ok()?;
    (entry.version == 1).then_some(entry)
}

fn is_entry_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    !stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit())
}

fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("entry"),
        std::process::id()
    ));
    std::fs::write(&tmp, contents)?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

async fn observe(pid: u32) -> Option<Observed> {
    #[cfg(target_os = "linux")]
    {
        return observe_linux(pid, None);
    }
    #[cfg(target_os = "macos")]
    {
        return ps_darwin(&["-p", &pid.to_string()])
            .await
            .into_iter()
            .find(|candidate| candidate.pid == pid);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

async fn observe_group(pgid: u32) -> Vec<Observed> {
    #[cfg(target_os = "linux")]
    {
        let names = match std::fs::read_dir("/proc") {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        let mut found = Vec::new();
        for entry in names.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if let Some(observed) = observe_linux(pid, Some(pgid)) {
                found.push(observed);
            }
        }
        return found;
    }
    #[cfg(target_os = "macos")]
    {
        return ps_darwin(&["-A"])
            .await
            .into_iter()
            .filter(|candidate| candidate.pgid == pgid)
            .collect();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pgid;
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn observe_linux(pid: u32, group: Option<u32>) -> Option<Observed> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.split_once(')')?.1.trim_start();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let pgid: u32 = fields.get(2)?.parse().ok()?;
    let start_time = (*fields.get(19)?).to_string();
    if let Some(group) = group
        && pgid != group
    {
        return None;
    }
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let mut command = String::from_utf8_lossy(&cmdline).into_owned();
    if command.ends_with('\0') {
        command.pop();
    }
    command = command.replace('\0', " ");
    Some(Observed {
        pid,
        pgid,
        start_time,
        command,
        zombie: fields.first().is_some_and(|state| *state == "Z"),
    })
}

#[cfg(target_os = "macos")]
async fn ps_darwin(selection: &[&str]) -> Vec<Observed> {
    let mut command = tokio::process::Command::new("/bin/ps");
    command
        .arg("-ww")
        .arg("-o")
        .arg("pid=,pgid=,stat=,lstart=,args=")
        .args(selection)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let output = match tokio::time::timeout(Duration::from_secs(5), command.output()).await {
        Ok(Ok(output)) if output.status.success() => output,
        _ => return Vec::new(),
    };
    parse_darwin_ps(&String::from_utf8_lossy(&output.stdout))
}

fn parse_darwin_ps(output: &str) -> Vec<Observed> {
    output.lines().filter_map(parse_darwin_line).collect()
}

fn parse_darwin_line(line: &str) -> Option<Observed> {
    let line = line.trim_end();
    let mut parts = line.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let pgid: u32 = parts.next()?.parse().ok()?;
    let stat = parts.next()?;
    let mut after_stat = line;
    for _ in 0..3 {
        let field = after_stat.trim_start();
        after_stat = &field[field.find(char::is_whitespace)?..];
    }
    let after_stat = after_stat.trim_start();
    let start_time = darwin_lstart(after_stat)?;
    let command = after_stat[start_time.len()..].trim_start().to_string();
    if command.is_empty() {
        return None;
    }
    Some(Observed {
        pid,
        pgid,
        start_time: start_time.to_string(),
        command,
        zombie: stat.starts_with('Z'),
    })
}

/// `ps -o lstart` under `LC_ALL=C TZ=UTC`, e.g. `Sat Sep  6 20:51:57 2026`.
fn darwin_lstart(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.len() < 24 {
        return None;
    }
    let weekday = text.get(0..3)?;
    if !weekday.bytes().all(|byte| byte.is_ascii_alphabetic()) || bytes.get(3) != Some(&b' ') {
        return None;
    }
    let month = text.get(4..7)?;
    if !month.bytes().all(|byte| byte.is_ascii_alphabetic()) || bytes.get(7) != Some(&b' ') {
        return None;
    }
    let mut index = 8;
    while bytes.get(index) == Some(&b' ') {
        index += 1;
    }
    let day_start = index;
    while bytes.get(index).is_some_and(|byte| byte.is_ascii_digit()) {
        index += 1;
    }
    let day_len = index - day_start;
    if !(1..=2).contains(&day_len) || bytes.get(index) != Some(&b' ') {
        return None;
    }
    index += 1;
    let time = text.get(index..index + 8)?;
    if !(time.as_bytes()[2] == b':' && time.as_bytes()[5] == b':') {
        return None;
    }
    index += 8;
    if bytes.get(index) != Some(&b' ') {
        return None;
    }
    index += 1;
    let year = text.get(index..index + 4)?;
    if !year.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.get(0..index + 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darwin_ps_keeps_the_lstart_and_the_command() {
        let line = "  42 42 Ss   Sat Sep  6 20:51:57 2026 /bin/sh -c sleep serve --port=4096";
        let parsed = parse_darwin_line(line).unwrap();
        assert_eq!(parsed.pid, 42);
        assert_eq!(parsed.pgid, 42);
        assert!(!parsed.zombie);
        assert_eq!(parsed.start_time, "Sat Sep  6 20:51:57 2026");
        assert_eq!(parsed.command, "/bin/sh -c sleep serve --port=4096");
    }

    #[test]
    fn darwin_ps_handles_padded_columns() {
        let line = " 5175  5162 S    Thu Oct 15 13:03:10 2026     sh -c sleep 600";
        let parsed = parse_darwin_line(line).unwrap();
        assert_eq!(parsed.pid, 5175);
        assert_eq!(parsed.pgid, 5162);
        assert_eq!(parsed.start_time, "Thu Oct 15 13:03:10 2026");
        assert_eq!(parsed.command, "sh -c sleep 600");
        // Real `ps` right-aligns pid and pgid to five columns, so one- and four-digit ids pad too.
        let launchd =
            parse_darwin_line("    1     1 Ss   Tue Oct 13 20:41:43 2026     /sbin/launchd")
                .unwrap();
        assert_eq!((launchd.pid, launchd.pgid), (1, 1));
        assert_eq!(launchd.command, "/sbin/launchd");
        let padded =
            parse_darwin_line(" 648   648 Ss   Tue Oct  5 09:12:00 2026 node serve").unwrap();
        assert_eq!((padded.pid, padded.pgid), (648, 648));
    }

    #[cfg(unix)]
    mod live {
        use std::os::unix::process::{CommandExt, ExitStatusExt};

        use super::*;
        use crate::process_group::SIGKILL;

        const SERVE_ARGS: &[&str] = &["serve", "--hostname=127.0.0.1", "--port=4096"];

        struct Group {
            pid: u32,
            child: std::sync::Mutex<Option<std::process::Child>>,
        }

        impl Group {
            fn wait(&self) -> std::process::ExitStatus {
                let mut slot = self.child.lock().expect("group child");
                let mut child = slot.take().expect("child still waiting");
                child.wait().expect("wait")
            }
        }

        impl Drop for Group {
            fn drop(&mut self) {
                let _ = signal_process_group(self.pid, SIGKILL);
                if let Some(mut child) = self.child.lock().expect("group child").take() {
                    let _ = child.wait();
                }
            }
        }

        fn spawn_group(script: &str, args: &[&str]) -> Group {
            let mut command = std::process::Command::new("/bin/sh");
            command.arg("-c").arg(script);
            for arg in args {
                command.arg(arg);
            }
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0);
            let child = command.spawn().expect("spawn group");
            let pid = child.id();
            Group {
                pid,
                child: std::sync::Mutex::new(Some(child)),
            }
        }

        fn serve_args() -> Vec<String> {
            SERVE_ARGS.iter().map(|arg| (*arg).to_string()).collect()
        }

        async fn record_from_dead_server(state_dir: &Path, server_pid: u32) {
            let owner = spawn_group("sleep 600", &[]);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let ledger = OpenCodeServerLedger::make_with_owner(state_dir, owner.pid).await;
            assert!(
                ledger
                    .track(server_pid, 4096, &serve_args())
                    .await
                    .is_some(),
                "the dead server did not record the group"
            );
            let _ = signal_process_group(owner.pid, SIGKILL);
            let _ = owner.wait();
        }

        fn entry_dir(state_dir: &Path) -> PathBuf {
            state_dir.join("opencode-servers")
        }

        #[tokio::test]
        async fn stops_a_group_left_by_a_server_that_died() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let orphan = spawn_group("sleep 600 & wait", SERVE_ARGS);
            tokio::time::sleep(Duration::from_millis(50)).await;
            record_from_dead_server(&state_dir, orphan.pid).await;
            assert_eq!(std::fs::read_dir(entry_dir(&state_dir)).unwrap().count(), 1);

            let restarted = OpenCodeServerLedger::make(&state_dir).await;
            restarted.reap_orphans().await;

            let status = orphan.wait();
            assert_eq!(status.signal(), Some(SIGTERM));
            assert!(!process_group_exists(orphan.pid));
            assert_eq!(std::fs::read_dir(entry_dir(&state_dir)).unwrap().count(), 0);
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        #[tokio::test]
        async fn an_entry_with_extra_keys_is_still_stopped() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let orphan = spawn_group("sleep 600 & wait", SERVE_ARGS);
            tokio::time::sleep(Duration::from_millis(50)).await;
            record_from_dead_server(&state_dir, orphan.pid).await;
            let entry_path = entry_dir(&state_dir).join(format!("{}.json", orphan.pid));
            let mut value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&entry_path).unwrap()).unwrap();
            value["writtenByNewerBuild"] = serde_json::Value::Bool(true);
            std::fs::write(&entry_path, value.to_string()).unwrap();

            let restarted = OpenCodeServerLedger::make(&state_dir).await;
            restarted.reap_orphans().await;

            let status = orphan.wait();
            assert_eq!(status.signal(), Some(SIGTERM));
            assert!(!process_group_exists(orphan.pid));
            assert!(!entry_path.exists());
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        #[tokio::test]
        async fn records_a_group_whose_wrapper_already_exited() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let wrapper = spawn_group(
                r#"/bin/sh -c "sleep 600 & wait" "$0" "$@" & exit 0"#,
                SERVE_ARGS,
            );
            let status = wrapper.wait();
            assert!(status.success());
            assert!(process_group_exists(wrapper.pid));

            record_from_dead_server(&state_dir, wrapper.pid).await;
            assert_eq!(std::fs::read_dir(entry_dir(&state_dir)).unwrap().count(), 1);

            let restarted = OpenCodeServerLedger::make(&state_dir).await;
            restarted.reap_orphans().await;

            assert!(!process_group_exists(wrapper.pid));
            assert_eq!(std::fs::read_dir(entry_dir(&state_dir)).unwrap().count(), 0);
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        #[tokio::test]
        async fn leaves_a_recycled_pid_alone_when_its_start_time_does_not_match() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let unrelated = spawn_group("sleep 600 & wait", SERVE_ARGS);
            tokio::time::sleep(Duration::from_millis(50)).await;
            record_from_dead_server(&state_dir, unrelated.pid).await;
            let entry_path = entry_dir(&state_dir).join(format!("{}.json", unrelated.pid));
            let entry = std::fs::read_to_string(&entry_path).unwrap();
            let rewritten = rewrite_start_time(&entry, "1");
            std::fs::write(&entry_path, rewritten).unwrap();

            let restarted = OpenCodeServerLedger::make(&state_dir).await;
            restarted.reap_orphans().await;

            assert!(process_group_exists(unrelated.pid));
            assert!(!entry_path.exists());
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        #[tokio::test]
        async fn leaves_a_serve_process_on_another_port_alone() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let unrelated = spawn_group("sleep 600 & wait", SERVE_ARGS);
            tokio::time::sleep(Duration::from_millis(50)).await;
            record_from_dead_server(&state_dir, unrelated.pid).await;
            let entry_path = entry_dir(&state_dir).join(format!("{}.json", unrelated.pid));
            let entry = std::fs::read_to_string(&entry_path)
                .unwrap()
                .replace("--port=4096", "--port=4097");
            std::fs::write(&entry_path, entry).unwrap();

            let restarted = OpenCodeServerLedger::make(&state_dir).await;
            restarted.reap_orphans().await;

            assert!(process_group_exists(unrelated.pid));
            assert!(!entry_path.exists());
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        #[tokio::test]
        async fn leaves_the_servers_of_a_running_owner_alone() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let running = OpenCodeServerLedger::make(&state_dir).await;
            let server = spawn_group("sleep 600 & wait", SERVE_ARGS);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let lease = running
                .track(server.pid, 4096, &serve_args())
                .await
                .expect("recorded");

            let other = OpenCodeServerLedger::make(&state_dir).await;
            other.reap_orphans().await;
            assert!(process_group_exists(server.pid));

            lease.forget();
            assert_eq!(std::fs::read_dir(entry_dir(&state_dir)).unwrap().count(), 0);
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        #[tokio::test]
        async fn a_foreign_or_unreadable_entry_is_deleted_without_a_signal() {
            let state_dir = std::env::temp_dir().join(format!(
                "t3-opencode-ledger-{}-{}",
                std::process::id(),
                unique()
            ));
            let directory = entry_dir(&state_dir);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("123.json"),
                r#"{"version":1,"pgid":123,"pid":123,"startTime":"x","command":"sleep","port":1,"stateDir":"/somewhere-else","owner":{"pid":1,"startTime":"y"}}"#,
            )
            .unwrap();
            std::fs::write(directory.join("9.json"), "nope").unwrap();
            let ledger = OpenCodeServerLedger::make(&state_dir).await;
            ledger.reap_orphans().await;
            assert!(!directory.join("123.json").exists());
            assert!(!directory.join("9.json").exists());
            let _ = std::fs::remove_dir_all(&state_dir);
        }

        fn rewrite_start_time(entry: &str, start_time: &str) -> String {
            let mut value: serde_json::Value = serde_json::from_str(entry).unwrap();
            value["startTime"] = serde_json::Value::String(start_time.to_string());
            value.to_string()
        }

        fn unique() -> u64 {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(1);
            NEXT.fetch_add(1, Ordering::Relaxed)
        }
    }
}
