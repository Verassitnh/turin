//! Linux cgroup helpers for ACP session spawn containment.
//!
//! Port of upstream/packages/provider-acp/src/server/AcpSessionRuntime.ts (cgroup section).

use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use tokio::time::{Duration, sleep};
use turin_shared::agent_scope::RAISE_OOM_SCORE_LINE;
use uuid::Uuid;

use crate::session_runtime::error::AcpProcessGroupTerminationError;
use crate::session_runtime::types::{
    AcpLinuxCgroupController, AcpLinuxCgroupLease, AcpPosixOwnershipRoot, AcpPosixProcessIdentity,
    AcpPosixProcessTreeController,
};

fn decode_mount_info_path(value: &str) -> String {
    let re = Regex::new(r"\\([0-7]{3})").unwrap();
    re.replace_all(value, |caps: &regex::Captures| {
        let octal = caps.get(1).unwrap().as_str();
        let code = u8::from_str_radix(octal, 8).unwrap_or(0);
        String::from(char::from(code))
    })
    .into_owned()
}

/// Port of upstream `parseUnifiedCgroupPath`.
pub fn parse_unified_cgroup_path(contents: &str) -> Option<String> {
    let re = Regex::new(r"(?m)^0::(/.*)$").unwrap();
    let matches: Vec<_> = re.captures_iter(contents).collect();
    if matches.len() == 1 {
        Some(matches[0][1].to_string())
    } else {
        None
    }
}

fn normalize_posix_path(value: &str) -> String {
    let is_absolute = value.starts_with('/');
    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir | Component::RootDir => {}
            Component::Prefix(_) => {
                parts.push(component.as_os_str().to_string_lossy().into_owned())
            }
        }
    }
    if is_absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

/// Port of upstream `isSafeCgroupPath`.
pub fn is_safe_cgroup_path(value: &str) -> bool {
    value.starts_with('/') && !value.contains("(deleted)") && normalize_posix_path(value) == value
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cgroup2Mount {
    pub mount_point: String,
    pub root: String,
}

/// Port of upstream `parseCgroup2Mounts`.
pub fn parse_cgroup2_mounts(contents: &str) -> Vec<Cgroup2Mount> {
    let mut mounts = Vec::new();
    for line in contents.split('\n') {
        let Some((mount, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        let fs_type = filesystem.split(' ').next().unwrap_or("");
        if fs_type != "cgroup2" {
            continue;
        }
        let fields: Vec<_> = mount.split(' ').collect();
        if fields.len() < 5 {
            continue;
        }
        mounts.push(Cgroup2Mount {
            mount_point: decode_mount_info_path(fields[4]),
            root: decode_mount_info_path(fields[3]),
        });
    }
    mounts
}

/// Port of upstream `parseCgroup2Mount`.
pub fn parse_cgroup2_mount(contents: &str) -> Option<Cgroup2Mount> {
    parse_cgroup2_mounts(contents).into_iter().next()
}

/// Port of upstream `wrapCommandForLinuxCgroup`.
pub fn wrap_command_for_linux_cgroup(
    lease: &dyn AcpLinuxCgroupLease,
    command: &str,
    args: &[String],
) -> (String, Vec<String>) {
    let script = [
        "lease_path=$1; expected=$2; shift 2",
        RAISE_OOM_SCORE_LINE,
        r#"printf "%s\n" "$$" > "$lease_path/cgroup.procs" || exit 125"#,
        "actual=",
        "while IFS= read -r line; do",
        r#"  case "$line" in 0::*) [ -z "$actual" ] || exit 126; actual=${line#0::};; esac"#,
        "done < /proc/self/cgroup || exit 125",
        r#"[ "$actual" = "$expected" ] || exit 126"#,
        "unset ELECTRON_RUN_AS_NODE T3_ACP_CGROUP_WRAPPER",
        "trap 'exit 125' 0",
        r#"[ -f "$1" ] && [ -x "$1" ] || exit 125"#,
        r#"exec "$@""#,
    ]
    .join("\n");
    let mut wrapped_args = vec![
        "-c".to_string(),
        script,
        "t3-acp-cgroup-wrapper".to_string(),
        lease.path().to_string(),
        lease.relative_path().to_string(),
        command.to_string(),
    ];
    wrapped_args.extend(args.iter().cloned());
    ("/bin/sh".to_string(), wrapped_args)
}

/// Port of upstream `resolveLinuxCgroupTargetCommand`.
pub fn resolve_linux_cgroup_target_command(
    command: &str,
    cwd: &Path,
    environment: &HashMap<String, String>,
) -> Option<PathBuf> {
    let path_env = environment
        .get("PATH")
        .map(String::as_str)
        .unwrap_or("/usr/bin:/bin");
    let candidates: Vec<PathBuf> = if command.contains(std::path::MAIN_SEPARATOR) {
        vec![cwd.join(command)]
    } else {
        path_env
            .split(':')
            .map(|entry| {
                let base = if entry.is_empty() {
                    cwd.to_path_buf()
                } else if Path::new(entry).is_absolute() {
                    PathBuf::from(entry)
                } else {
                    cwd.join(entry)
                };
                base.join(command)
            })
            .collect()
    };
    for candidate in candidates {
        let Ok(meta) = fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
        }
    }
    None
}

const STALE_ACP_CGROUP_SIBLING: &str = r"^t3-acp-(\d+)-";

pub type CgroupPopulatedReader<'a> = dyn Fn(&str) -> Option<CgroupPopulatedState> + 'a;
pub type CgroupSiblingRemover<'a> = dyn Fn(&str) -> Result<(), io::Error> + 'a;

/// Port of upstream `SweepStaleLinuxCgroupSiblingsOptions`.
pub struct SweepStaleLinuxCgroupSiblingsOptions<'a> {
    pub current_pid: Option<i32>,
    pub is_process_alive: Option<&'a dyn Fn(i32) -> bool>,
    pub read_populated: Option<&'a CgroupPopulatedReader<'a>>,
    pub remove: Option<&'a CgroupSiblingRemover<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupPopulatedState {
    Empty,
    Populated,
}

impl CgroupPopulatedState {
    fn from_digit(d: &str) -> Option<Self> {
        match d {
            "0" => Some(Self::Empty),
            "1" => Some(Self::Populated),
            _ => None,
        }
    }
}

fn default_cgroup_owner_is_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let rc = unsafe { kill(pid, 0) };
        rc == 0 || io::Error::last_os_error().raw_os_error() != Some(3)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn default_read_cgroup_populated(sibling_path: &str) -> Option<CgroupPopulatedState> {
    let events = fs::read_to_string(Path::new(sibling_path).join("cgroup.events")).ok()?;
    let re = Regex::new(r"(?:^|\n)populated ([01])(?:\n|$)").unwrap();
    re.captures(&events)
        .and_then(|caps| CgroupPopulatedState::from_digit(&caps[1]))
}

/// Best-effort removal of empty `t3-acp-<dead-pid>-*` sibling leases under a parent cgroup.
pub fn sweep_stale_linux_cgroup_siblings(
    parent_path: &str,
    options: SweepStaleLinuxCgroupSiblingsOptions<'_>,
) {
    let current_pid = options
        .current_pid
        .unwrap_or_else(|| std::process::id() as i32);
    let is_process_alive = options
        .is_process_alive
        .unwrap_or(&default_cgroup_owner_is_alive);
    let read_populated = options
        .read_populated
        .unwrap_or(&default_read_cgroup_populated);
    let remove = options.remove.unwrap_or(&|sibling_path: &str| {
        fs::remove_dir(sibling_path)?;
        Ok(())
    });
    let sibling_re = Regex::new(STALE_ACP_CGROUP_SIBLING).unwrap();
    let Ok(entries) = fs::read_dir(parent_path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(caps) = sibling_re.captures(&name) else {
            continue;
        };
        let Ok(owner_pid) = caps[1].parse::<i32>() else {
            continue;
        };
        if owner_pid <= 1 || owner_pid == current_pid {
            continue;
        }
        if is_process_alive(owner_pid) {
            continue;
        }
        let sibling_path = Path::new(parent_path).join(entry.file_name());
        if sibling_path.parent().and_then(|p| p.to_str()) != Some(parent_path) {
            continue;
        }
        let sibling_path = sibling_path.to_string_lossy();
        if read_populated(&sibling_path) != Some(CgroupPopulatedState::Empty) {
            continue;
        }
        let _ = remove(&sibling_path);
    }
}

struct LiveLinuxCgroupLease {
    child_path: PathBuf,
    child_relative: String,
}

impl AcpLinuxCgroupLease for LiveLinuxCgroupLease {
    fn contains(&self, pid: i32) -> bool {
        let Ok(contents) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
            return false;
        };
        parse_unified_cgroup_path(&contents).as_deref() == Some(self.child_relative.as_str())
    }

    fn exists(&self) -> bool {
        self.child_path.exists()
    }

    fn path(&self) -> &str {
        self.child_path.to_str().unwrap_or("")
    }

    fn relative_path(&self) -> &str {
        &self.child_relative
    }

    fn kill(&self) -> Result<(), io::Error> {
        fs::write(self.child_path.join("cgroup.kill"), b"1\n")
    }

    fn populated(&self) -> Result<bool, io::Error> {
        let events = fs::read_to_string(self.child_path.join("cgroup.events"))?;
        let re = Regex::new(r"(?:^|\n)populated ([01])(?:\n|$)").unwrap();
        let state = re
            .captures(&events)
            .and_then(|caps| CgroupPopulatedState::from_digit(&caps[1]))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no populated state"))?;
        Ok(matches!(state, CgroupPopulatedState::Populated))
    }

    fn remove(&self) -> Result<(), io::Error> {
        fn nested_dirs(path: &Path) -> io::Result<Vec<PathBuf>> {
            let mut dirs = Vec::new();
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    let child = entry.path();
                    dirs.extend(nested_dirs(&child)?);
                    dirs.push(child);
                }
            }
            Ok(dirs)
        }
        for directory in nested_dirs(&self.child_path)? {
            let _ = fs::remove_dir(&directory);
        }
        fs::remove_dir(&self.child_path)
    }
}

fn try_create_linux_cgroup_lease(
    current_relative: &str,
    mount: &Cgroup2Mount,
) -> Option<Box<dyn AcpLinuxCgroupLease>> {
    if !is_safe_cgroup_path(&mount.root) || !is_safe_cgroup_path(&mount.mount_point) {
        return None;
    }
    if !(mount.root == "/"
        || current_relative == mount.root
        || current_relative.starts_with(&format!("{}/", mount.root)))
    {
        return None;
    }
    let mount_relative = if mount.root == "/" {
        current_relative.to_string()
    } else {
        current_relative[mount.root.len()..].to_string()
    };
    let dotted = format!(".{mount_relative}");
    let mut current_path = PathBuf::from(&mount.mount_point);
    for component in Path::new(&dotted).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                current_path.pop();
            }
            Component::Normal(part) => current_path.push(part),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let resolved_mount = PathBuf::from(&mount.mount_point);
    if current_path != resolved_mount && !current_path.starts_with(&resolved_mount) {
        return None;
    }
    sweep_stale_linux_cgroup_siblings(
        current_path.to_str()?,
        SweepStaleLinuxCgroupSiblingsOptions {
            current_pid: None,
            is_process_alive: None,
            read_populated: None,
            remove: None,
        },
    );
    let child_name = format!("t3-acp-{}-{}", std::process::id(), Uuid::new_v4().simple());
    let child_path = current_path.join(&child_name);
    let child_relative = if current_relative.ends_with('/') {
        format!("{current_relative}{child_name}")
    } else {
        format!("{current_relative}/{child_name}")
    };
    if child_path.parent() != Some(current_path.as_path()) {
        return None;
    }
    if fs::create_dir(&child_path).is_err() {
        return None;
    }
    let check = (|| -> io::Result<()> {
        fs::metadata(child_path.join("cgroup.procs"))?;
        fs::metadata(child_path.join("cgroup.kill"))?;
        fs::metadata(child_path.join("cgroup.events"))?;
        let cgroup_type = fs::read_to_string(child_path.join("cgroup.type"))?;
        if cgroup_type.trim() != "domain" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACP cgroup is not a domain cgroup",
            ));
        }
        Ok(())
    })();
    if check.is_err() {
        let _ = fs::remove_dir(&child_path);
        return None;
    }
    Some(Box::new(LiveLinuxCgroupLease {
        child_path,
        child_relative,
    }))
}

/// Port of upstream `makeLinuxCgroupController`.
pub fn make_linux_cgroup_controller() -> impl AcpLinuxCgroupController {
    struct Controller;
    impl AcpLinuxCgroupController for Controller {
        fn create(&self) -> Option<Box<dyn AcpLinuxCgroupLease>> {
            let current_relative = fs::read_to_string("/proc/self/cgroup")
                .ok()
                .and_then(|contents| parse_unified_cgroup_path(&contents))?;
            let mounts = parse_cgroup2_mounts(&fs::read_to_string("/proc/self/mountinfo").ok()?);
            if !current_relative.starts_with('/') || mounts.is_empty() {
                return None;
            }
            if !is_safe_cgroup_path(&current_relative) {
                return None;
            }
            for mount in &mounts {
                if let Some(lease) = try_create_linux_cgroup_lease(&current_relative, mount) {
                    return Some(lease);
                }
            }
            None
        }
    }
    Controller
}

/// Port of upstream `terminateLinuxCgroupLease`.
pub async fn terminate_linux_cgroup_lease(
    lease: &dyn AcpLinuxCgroupLease,
) -> Result<(), AcpProcessGroupTerminationError> {
    fn failure(
        lease: &dyn AcpLinuxCgroupLease,
        name: &str,
        cause: impl std::fmt::Display,
    ) -> AcpProcessGroupTerminationError {
        AcpProcessGroupTerminationError::with_cause(
            format!("Failed to {name} ACP cgroup {}", lease.path()),
            cause,
        )
    }
    let mut last_removal_failure: Option<String> = None;
    for _attempt in 0..200 {
        if !lease.exists() {
            return Ok(());
        }
        match lease.kill() {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if !lease.exists() {
                    return Ok(());
                }
                sleep(Duration::from_millis(10)).await;
                continue;
            }
            Err(err) => return Err(failure(lease, "kill", err)),
        }
        let populated = match lease.populated() {
            Ok(value) => value,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if !lease.exists() {
                    return Ok(());
                }
                sleep(Duration::from_millis(10)).await;
                continue;
            }
            Err(err) => return Err(failure(lease, "read state for", err)),
        };
        if populated {
            last_removal_failure = None;
            sleep(Duration::from_millis(10)).await;
            continue;
        }
        match lease.remove() {
            Ok(()) => last_removal_failure = None,
            Err(err) if err.raw_os_error() == Some(libc::ENOENT) && !lease.exists() => {
                return Ok(());
            }
            Err(err)
                if matches!(
                    err.raw_os_error(),
                    Some(libc::EBUSY) | Some(libc::ENOTEMPTY) | Some(libc::ENOENT)
                ) =>
            {
                last_removal_failure = Some(err.to_string());
            }
            Err(err) => return Err(failure(lease, "remove", err)),
        }
        if !lease.exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
    if let Some(cause) = last_removal_failure {
        return Err(failure(lease, "remove", cause));
    }
    Err(AcpProcessGroupTerminationError::new(format!(
        "ACP cgroup {} remained present after cgroup cleanup",
        lease.path()
    )))
}

fn same_posix_process_identity(
    expected: &AcpPosixProcessIdentity,
    observed: Option<&AcpPosixProcessIdentity>,
) -> bool {
    observed.is_some_and(|observed| {
        observed.pid == expected.pid && observed.start_time == expected.start_time
    })
}

/// Port of upstream `signalLinuxCgroupRootTerm`.
pub fn signal_linux_cgroup_root_term(
    controller: &dyn AcpPosixProcessTreeController,
    lease: &dyn AcpLinuxCgroupLease,
    root: &AcpPosixOwnershipRoot,
) {
    let Some(root_value) = root.value.as_ref() else {
        return;
    };
    let observed = controller.identity(root_value.pid);
    if !same_posix_process_identity(
        &AcpPosixProcessIdentity {
            executable: root_value.executable.clone(),
            pgid: root_value.pgid,
            pid: root_value.pid,
            ppid: root_value.ppid,
            sid: root_value.sid,
            state: root_value.state.clone(),
            start_time: root_value.start_time.clone(),
        },
        observed.as_ref(),
    ) || !lease.contains(root_value.pid)
    {
        return;
    }
    controller.signal_process(root_value.pid, libc::SIGTERM);
}
