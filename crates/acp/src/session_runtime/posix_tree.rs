//! POSIX process tree ownership and teardown.
//!
//! Port of upstream/packages/provider-acp/src/server/AcpSessionRuntime.ts (POSIX section).

use std::collections::{HashMap, HashSet, VecDeque};

#[cfg(target_os = "linux")]
use std::fs;

use tokio::time::{Duration, sleep};

use crate::session_runtime::error::AcpProcessGroupTerminationError;
use crate::session_runtime::types::{
    AcpOwnedPosixProcess, AcpPosixOwnershipRoot, AcpPosixProcessIdentity,
    AcpPosixProcessTreeController,
};

/// Linux zombies keep pid/starttime until reaped; they are not live survivors.
pub fn posix_process_is_zombie(process: &AcpPosixProcessIdentity) -> bool {
    process.state.as_deref() == Some("Z")
}

fn process_identity_key(process: &impl ProcessIdentityLike) -> String {
    format!(
        "{}:{}",
        process.identity_pid(),
        process.identity_start_time()
    )
}

trait ProcessIdentityLike {
    fn identity_pid(&self) -> i32;
    fn identity_start_time(&self) -> &str;
}

impl ProcessIdentityLike for AcpPosixProcessIdentity {
    fn identity_pid(&self) -> i32 {
        self.pid
    }
    fn identity_start_time(&self) -> &str {
        &self.start_time
    }
}

impl ProcessIdentityLike for AcpOwnedPosixProcess {
    fn identity_pid(&self) -> i32 {
        self.pid
    }
    fn identity_start_time(&self) -> &str {
        &self.start_time
    }
}

impl ProcessIdentityLike for &AcpPosixProcessIdentity {
    fn identity_pid(&self) -> i32 {
        self.pid
    }
    fn identity_start_time(&self) -> &str {
        &self.start_time
    }
}

impl ProcessIdentityLike for &AcpOwnedPosixProcess {
    fn identity_pid(&self) -> i32 {
        self.pid
    }
    fn identity_start_time(&self) -> &str {
        &self.start_time
    }
}

fn same_posix_process_identity(
    expected: &impl ProcessIdentityLike,
    observed: Option<&AcpPosixProcessIdentity>,
) -> bool {
    observed.is_some_and(|observed| {
        observed.pid == expected.identity_pid()
            && observed.start_time == expected.identity_start_time()
    })
}

fn process_depth(
    process: &AcpPosixProcessIdentity,
    ledger_by_pid: &HashMap<i32, AcpOwnedPosixProcess>,
) -> usize {
    let mut depth = 0;
    let mut current = process.clone();
    let mut seen = HashSet::new();
    while seen.insert(current.pid) {
        let Some(parent) = ledger_by_pid.get(&current.ppid) else {
            break;
        };
        depth += 1;
        current = AcpPosixProcessIdentity {
            executable: parent.executable.clone(),
            pgid: parent.pgid,
            pid: parent.pid,
            ppid: parent.ppid,
            sid: parent.sid,
            state: parent.state.clone(),
            start_time: parent.start_time.clone(),
        };
    }
    depth
}

#[cfg(target_os = "linux")]
fn read_linux_process_identity(pid: i32) -> Option<AcpPosixProcessIdentity> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let command_end = stat.rfind(')')?;
    let fields: Vec<_> = stat[command_end + 2..].trim().split_whitespace().collect();
    let state = fields.first()?.to_string();
    let ppid = fields.get(1)?.parse().ok()?;
    let pgid = fields.get(2)?.parse().ok()?;
    let sid = fields.get(3)?.parse().ok()?;
    let start_time = fields.get(19)?.to_string();
    let executable = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| path.to_str().map(str::to_string));
    Some(AcpPosixProcessIdentity {
        executable,
        pgid,
        pid,
        ppid,
        sid,
        start_time,
        state: Some(state),
    })
}

#[cfg(target_os = "linux")]
fn read_linux_child_pids(pid: i32) -> Vec<i32> {
    let task_dir = format!("/proc/{pid}/task");
    let Ok(entries) = fs::read_dir(&task_dir) else {
        return Vec::new();
    };
    let task_re = regex::Regex::new(r"^[1-9][0-9]*$").unwrap();
    let mut child_pids = HashSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !task_re.is_match(&name) {
            continue;
        }
        let children_path = format!("/proc/{pid}/task/{name}/children");
        let Ok(contents) = fs::read_to_string(children_path) else {
            continue;
        };
        for token in contents.split_whitespace() {
            if let Ok(child_pid) = token.parse::<i32>() {
                child_pids.insert(child_pid);
            }
        }
    }
    child_pids.into_iter().collect()
}

pub struct LinuxProcessTreeController;

impl AcpPosixProcessTreeController for LinuxProcessTreeController {
    fn child_pids_of(&self, pid: i32) -> Vec<i32> {
        #[cfg(target_os = "linux")]
        {
            read_linux_child_pids(pid)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            Vec::new()
        }
    }

    fn children_of(&self, pid: i32) -> Vec<AcpPosixProcessIdentity> {
        self.child_pids_of(pid)
            .into_iter()
            .filter_map(|child_pid| self.identity(child_pid))
            .collect()
    }

    fn identity(&self, pid: i32) -> Option<AcpPosixProcessIdentity> {
        #[cfg(target_os = "linux")]
        {
            read_linux_process_identity(pid)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            None
        }
    }

    fn snapshot(&self) -> Result<Vec<AcpPosixProcessIdentity>, AcpProcessGroupTerminationError> {
        #[cfg(target_os = "linux")]
        {
            let proc_re = regex::Regex::new(r"^[1-9][0-9]*$").unwrap();
            let mut out = Vec::new();
            for entry in fs::read_dir("/proc").map_err(|cause| {
                AcpProcessGroupTerminationError::with_cause(
                    "Failed to snapshot ACP process tree",
                    cause,
                )
            })? {
                let entry = entry.map_err(|cause| {
                    AcpProcessGroupTerminationError::with_cause(
                        "Failed to snapshot ACP process tree",
                        cause,
                    )
                })?;
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !proc_re.is_match(&name) {
                    continue;
                }
                if let Ok(pid) = name.parse::<i32>() {
                    if let Some(identity) = read_linux_process_identity(pid) {
                        out.push(identity);
                    }
                }
            }
            Ok(out)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Vec::new())
        }
    }

    fn signal_process(&self, pid: i32, signal: i32) {
        #[cfg(unix)]
        unsafe {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            let _ = kill(pid, signal);
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, signal);
        }
    }
}

/// Port of upstream `makePosixProcessTreeController`.
pub fn make_posix_process_tree_controller(
    platform: &str,
) -> Result<Box<dyn AcpPosixProcessTreeController>, AcpProcessGroupTerminationError> {
    if platform == "linux" {
        return Ok(Box::new(LinuxProcessTreeController));
    }
    Err(AcpProcessGroupTerminationError::new(format!(
        "Detached ACP descendant ownership is unsupported on {platform}"
    )))
}

pub fn observe_posix_ownership_ledger(
    child_queues: &mut HashMap<i32, Vec<i32>>,
    controller: &dyn AcpPosixProcessTreeController,
    frontier: &mut HashMap<i32, AcpOwnedPosixProcess>,
    ledger: &mut HashMap<String, AcpOwnedPosixProcess>,
    root: &mut AcpPosixOwnershipRoot,
    root_pid: i32,
    max_processes: Option<usize>,
) -> Result<(), AcpProcessGroupTerminationError> {
    if root.value.is_none() {
        if root.capture_attempted {
            return Err(AcpProcessGroupTerminationError::new(format!(
                "ACP process {root_pid} exited before its ownership ledger was captured"
            )));
        }
        root.capture_attempted = true;
        let Some(identity) = controller.identity(root_pid) else {
            return Err(AcpProcessGroupTerminationError::new(format!(
                "ACP process {root_pid} exited before its ownership ledger was captured"
            )));
        };
        let parent = controller.identity(identity.ppid);
        let owned_root = AcpOwnedPosixProcess {
            parent_executable: parent.as_ref().and_then(|p| p.executable.clone()),
            parent_start_time: parent
                .as_ref()
                .map(|p| p.start_time.clone())
                .unwrap_or_default(),
            executable: identity.executable.clone(),
            pgid: identity.pgid,
            pid: identity.pid,
            ppid: identity.ppid,
            sid: identity.sid,
            state: identity.state.clone(),
            start_time: identity.start_time.clone(),
        };
        ledger.insert(process_identity_key(&identity), owned_root.clone());
        frontier.insert(identity.pid, owned_root.clone());
        root.value = Some(owned_root);
    }
    let max_processes = max_processes.unwrap_or(usize::MAX).max(1);
    let mut remaining_budget = max_processes;
    let mut pending: VecDeque<(i32, AcpOwnedPosixProcess)> = frontier
        .iter()
        .map(|(pid, owned)| (*pid, owned.clone()))
        .collect();
    let mut visited = HashSet::new();
    while remaining_budget > 0 {
        let Some((parent_pid, parent)) = pending.pop_front() else {
            break;
        };
        frontier.remove(&parent_pid);
        if visited.contains(&parent.pid) {
            continue;
        }
        visited.insert(parent.pid);
        remaining_budget -= 1;
        let observed_parent = controller.identity(parent.pid);
        if !same_posix_process_identity(&parent, observed_parent.as_ref()) {
            child_queues.remove(&parent.pid);
            continue;
        }
        let observed = observed_parent.unwrap();
        let refreshed_parent = AcpOwnedPosixProcess {
            parent_executable: parent.parent_executable.clone(),
            parent_start_time: parent.parent_start_time.clone(),
            executable: observed.executable.clone(),
            pgid: observed.pgid,
            pid: observed.pid,
            ppid: observed.ppid,
            sid: observed.sid,
            state: observed.state.clone(),
            start_time: observed.start_time.clone(),
        };
        ledger.insert(
            process_identity_key(&refreshed_parent),
            refreshed_parent.clone(),
        );
        frontier.insert(refreshed_parent.pid, refreshed_parent.clone());
        let mut queued_children = child_queues.remove(&refreshed_parent.pid);
        if queued_children.is_none() {
            if remaining_budget == 0 {
                continue;
            }
            remaining_budget -= 1;
            queued_children = Some(controller.child_pids_of(refreshed_parent.pid));
        }
        let mut queued_children = queued_children.unwrap_or_default();
        while !queued_children.is_empty() && remaining_budget > 0 {
            let child_pid = queued_children.remove(0);
            remaining_budget -= 1;
            let Some(child) = controller.identity(child_pid) else {
                continue;
            };
            if child.ppid != parent.pid {
                continue;
            }
            let mut reserved_pid = frontier.get(&child.pid).cloned();
            if let Some(ref reserved) = reserved_pid
                && !same_posix_process_identity(reserved, Some(&child))
            {
                frontier.remove(&child.pid);
                ledger.retain(|_, owned| owned.pid != child.pid);
                reserved_pid = None;
            }
            let owned = if let Some(mut merged) = reserved_pid {
                merged.executable = child.executable.clone();
                merged.pgid = child.pgid;
                merged.ppid = child.ppid;
                merged.sid = child.sid;
                merged.state = child.state.clone();
                merged.start_time = child.start_time.clone();
                merged
            } else {
                AcpOwnedPosixProcess {
                    parent_executable: refreshed_parent.executable.clone(),
                    parent_start_time: refreshed_parent.start_time.clone(),
                    executable: child.executable.clone(),
                    pgid: child.pgid,
                    pid: child.pid,
                    ppid: child.ppid,
                    sid: child.sid,
                    state: child.state.clone(),
                    start_time: child.start_time.clone(),
                }
            };
            ledger.insert(process_identity_key(&child), owned.clone());
            frontier.insert(child.pid, owned);
        }
        if queued_children.is_empty() {
            child_queues.remove(&refreshed_parent.pid);
        } else {
            child_queues.insert(refreshed_parent.pid, queued_children);
        }
    }
    Ok(())
}

pub async fn observe_posix_ownership_ledger_continuously(
    child_queues: &mut HashMap<i32, Vec<i32>>,
    controller: &dyn AcpPosixProcessTreeController,
    frontier: &mut HashMap<i32, AcpOwnedPosixProcess>,
    ledger: &mut HashMap<String, AcpOwnedPosixProcess>,
    root: &mut AcpPosixOwnershipRoot,
    root_pid: i32,
) -> Result<(), AcpProcessGroupTerminationError> {
    loop {
        let started_at = tokio::time::Instant::now();
        observe_posix_ownership_ledger(
            child_queues,
            controller,
            frontier,
            ledger,
            root,
            root_pid,
            Some(64),
        )?;
        let target_interval: u64 = if frontier.len() <= 32 {
            25
        } else if frontier.len() <= 128 {
            50
        } else {
            100
        };
        let elapsed = started_at.elapsed().as_millis() as u64;
        let delay = target_interval.saturating_sub(elapsed).max(5);
        sleep(Duration::from_millis(delay)).await;
    }
}

pub fn capture_posix_ownership_ledger(
    controller: &dyn AcpPosixProcessTreeController,
    ledger: &mut HashMap<String, AcpOwnedPosixProcess>,
    root: Option<&mut AcpPosixOwnershipRoot>,
    root_pid: i32,
    table: Option<&[AcpPosixProcessIdentity]>,
) -> Result<Vec<AcpPosixProcessIdentity>, AcpProcessGroupTerminationError> {
    let table = if let Some(table) = table {
        table.to_vec()
    } else {
        controller.snapshot()?
    };
    let by_pid: HashMap<i32, AcpPosixProcessIdentity> = table
        .iter()
        .map(|process| (process.pid, process.clone()))
        .collect();
    let mut root_identity = root
        .as_ref()
        .and_then(|root| root.value.clone())
        .or_else(|| {
            ledger
                .values()
                .find(|process| process.pid == root_pid)
                .cloned()
        });
    if root_identity.is_none() {
        let Some(identity) = by_pid.get(&root_pid).cloned() else {
            return Err(AcpProcessGroupTerminationError::new(format!(
                "ACP process {root_pid} exited before its ownership ledger was captured"
            )));
        };
        let owned_root = AcpOwnedPosixProcess {
            parent_executable: by_pid
                .get(&identity.ppid)
                .and_then(|p| p.executable.clone()),
            parent_start_time: by_pid
                .get(&identity.ppid)
                .map(|p| p.start_time.clone())
                .unwrap_or_default(),
            executable: identity.executable.clone(),
            pgid: identity.pgid,
            pid: identity.pid,
            ppid: identity.ppid,
            sid: identity.sid,
            state: identity.state.clone(),
            start_time: identity.start_time.clone(),
        };
        ledger.insert(process_identity_key(&identity), owned_root.clone());
        if let Some(root) = root {
            root.value = Some(owned_root.clone());
        }
        root_identity = Some(owned_root);
    }
    let _ = root_identity;
    ledger.retain(|_, owned| {
        if owned.pid == root_pid {
            return true;
        }
        by_pid
            .get(&owned.pid)
            .is_some_and(|live| same_posix_process_identity(owned, Some(live)))
    });
    let mut children: HashMap<i32, Vec<AcpPosixProcessIdentity>> = HashMap::new();
    let retained_by_pid: HashMap<i32, AcpOwnedPosixProcess> = ledger
        .values()
        .filter(|owned| {
            by_pid
                .get(&owned.pid)
                .is_some_and(|live| same_posix_process_identity(owned, Some(live)))
        })
        .map(|owned| (owned.pid, owned.clone()))
        .collect();
    for process in &table {
        children
            .entry(process.ppid)
            .or_default()
            .push(process.clone());
    }
    let mut pending: VecDeque<AcpOwnedPosixProcess> = ledger
        .values()
        .filter(|owned| {
            by_pid
                .get(&owned.pid)
                .is_some_and(|live| same_posix_process_identity(owned, Some(live)))
        })
        .cloned()
        .collect();
    let mut visited = HashSet::new();
    while let Some(parent) = pending.pop_front() {
        if visited.contains(&parent.pid) {
            continue;
        }
        visited.insert(parent.pid);
        let Some(observed_parent) = by_pid.get(&parent.pid) else {
            continue;
        };
        if !same_posix_process_identity(&parent, Some(observed_parent)) {
            continue;
        }
        let refreshed_parent = AcpOwnedPosixProcess {
            parent_executable: parent.parent_executable.clone(),
            parent_start_time: parent.parent_start_time.clone(),
            executable: observed_parent.executable.clone(),
            pgid: observed_parent.pgid,
            pid: observed_parent.pid,
            ppid: observed_parent.ppid,
            sid: observed_parent.sid,
            state: observed_parent.state.clone(),
            start_time: observed_parent.start_time.clone(),
        };
        ledger.insert(
            process_identity_key(observed_parent),
            refreshed_parent.clone(),
        );
        for process in children.get(&refreshed_parent.pid).into_iter().flatten() {
            let reserved_pid = retained_by_pid.get(&process.pid);
            if reserved_pid
                .is_some_and(|reserved| !same_posix_process_identity(reserved, Some(process)))
            {
                continue;
            }
            let owned = if let Some(mut merged) = reserved_pid.cloned() {
                merged.executable = process.executable.clone();
                merged.pgid = process.pgid;
                merged.ppid = process.ppid;
                merged.sid = process.sid;
                merged.state = process.state.clone();
                merged.start_time = process.start_time.clone();
                merged
            } else {
                AcpOwnedPosixProcess {
                    parent_executable: refreshed_parent.executable.clone(),
                    parent_start_time: refreshed_parent.start_time.clone(),
                    executable: process.executable.clone(),
                    pgid: process.pgid,
                    pid: process.pid,
                    ppid: process.ppid,
                    sid: process.sid,
                    state: process.state.clone(),
                    start_time: process.start_time.clone(),
                }
            };
            ledger.insert(process_identity_key(process), owned.clone());
            pending.push_back(owned);
        }
    }
    Ok(table)
}

pub struct TerminatePosixOwnedProcessTreeInput<'a> {
    pub controller: &'a dyn AcpPosixProcessTreeController,
    pub discovery_passes: Option<u32>,
    pub grace: Option<Duration>,
    pub ledger: Option<&'a mut HashMap<String, AcpOwnedPosixProcess>>,
    pub root: Option<&'a mut AcpPosixOwnershipRoot>,
    pub root_pid: i32,
}

fn signal_owned_process_tree_phase(
    controller: &dyn AcpPosixProcessTreeController,
    ledger: &mut HashMap<String, AcpOwnedPosixProcess>,
    root: Option<&mut AcpPosixOwnershipRoot>,
    root_pid: i32,
    signal: i32,
    include_root: bool,
) -> Result<(), AcpProcessGroupTerminationError> {
    let table = controller.snapshot()?;
    capture_posix_ownership_ledger(controller, ledger, root, root_pid, Some(&table))?;
    let by_pid: HashMap<i32, AcpPosixProcessIdentity> = table
        .iter()
        .map(|entry| (entry.pid, entry.clone()))
        .collect();
    let current_pid = std::process::id() as i32;
    let current = controller.identity(current_pid).ok_or_else(|| {
        AcpProcessGroupTerminationError::new("Cannot identify the current T3 process group")
    })?;
    let ledger_by_pid: HashMap<i32, AcpOwnedPosixProcess> = ledger
        .values()
        .map(|process| (process.pid, process.clone()))
        .collect();
    let mut pending: Vec<AcpOwnedPosixProcess> = ledger
        .values()
        .filter(|candidate| {
            (include_root || candidate.pid != root_pid)
                && by_pid
                    .get(&candidate.pid)
                    .is_some_and(|live| same_posix_process_identity(candidate, Some(live)))
        })
        .cloned()
        .collect();
    let retained_by_pid: HashMap<i32, AcpOwnedPosixProcess> = ledger
        .values()
        .filter(|owned| {
            by_pid
                .get(&owned.pid)
                .is_some_and(|live| same_posix_process_identity(owned, Some(live)))
        })
        .map(|owned| (owned.pid, owned.clone()))
        .collect();
    let mut signalled = HashSet::new();
    while !pending.is_empty() {
        pending.sort_by(|left, right| {
            process_depth(
                &AcpPosixProcessIdentity {
                    executable: right.executable.clone(),
                    pgid: right.pgid,
                    pid: right.pid,
                    ppid: right.ppid,
                    sid: right.sid,
                    state: right.state.clone(),
                    start_time: right.start_time.clone(),
                },
                &ledger_by_pid,
            )
            .cmp(&process_depth(
                &AcpPosixProcessIdentity {
                    executable: left.executable.clone(),
                    pgid: left.pgid,
                    pid: left.pid,
                    ppid: left.ppid,
                    sid: left.sid,
                    state: left.state.clone(),
                    start_time: left.start_time.clone(),
                },
                &ledger_by_pid,
            ))
        });
        let Some(candidate) = pending.pop() else {
            break;
        };
        if signalled.contains(&candidate.pid) {
            continue;
        }
        let observed = controller.identity(candidate.pid);
        if candidate.pid <= 1
            || candidate.pid == current_pid
            || observed.is_none()
            || observed
                .as_ref()
                .is_some_and(|observed| observed.pgid == current.pgid)
            || observed
                .as_ref()
                .is_some_and(|observed| observed.sid == current.sid)
            || !same_posix_process_identity(&candidate, observed.as_ref())
        {
            continue;
        }
        if let Some(observed) = observed {
            for child in controller.children_of(candidate.pid) {
                if child.ppid != candidate.pid {
                    continue;
                }
                let reserved_pid = retained_by_pid.get(&child.pid);
                if reserved_pid
                    .is_some_and(|reserved| !same_posix_process_identity(reserved, Some(&child)))
                {
                    continue;
                }
                let owned = if let Some(mut merged) = reserved_pid.cloned() {
                    merged.executable = child.executable.clone();
                    merged.pgid = child.pgid;
                    merged.ppid = child.ppid;
                    merged.sid = child.sid;
                    merged.state = child.state.clone();
                    merged.start_time = child.start_time.clone();
                    merged
                } else {
                    AcpOwnedPosixProcess {
                        parent_executable: observed.executable.clone(),
                        parent_start_time: observed.start_time.clone(),
                        executable: child.executable.clone(),
                        pgid: child.pgid,
                        pid: child.pid,
                        ppid: child.ppid,
                        sid: child.sid,
                        state: child.state.clone(),
                        start_time: child.start_time.clone(),
                    }
                };
                ledger.insert(process_identity_key(&child), owned.clone());
                if (include_root || child.pid != root_pid)
                    && !signalled.contains(&child.pid)
                    && same_posix_process_identity(&owned, controller.identity(child.pid).as_ref())
                {
                    pending.push(owned);
                }
            }
            controller.signal_process(candidate.pid, signal);
            signalled.insert(candidate.pid);
        }
    }
    Ok(())
}

pub async fn terminate_posix_owned_process_tree(
    mut input: TerminatePosixOwnedProcessTreeInput<'_>,
) -> Result<(), AcpProcessGroupTerminationError> {
    let mut owned_ledger = HashMap::new();
    let ledger = input.ledger.unwrap_or(&mut owned_ledger);
    let passes = input.discovery_passes.unwrap_or(4).clamp(2, 8);
    for _ in 0..passes {
        signal_owned_process_tree_phase(
            input.controller,
            ledger,
            input.root.as_deref_mut(),
            input.root_pid,
            libc::SIGTERM,
            false,
        )?;
        sleep(Duration::from_millis(10)).await;
    }
    signal_owned_process_tree_phase(
        input.controller,
        ledger,
        input.root.as_deref_mut(),
        input.root_pid,
        libc::SIGTERM,
        true,
    )?;
    let grace = input.grace.unwrap_or(Duration::from_secs(1));
    if grace > Duration::ZERO {
        sleep(grace).await;
    }
    for _ in 0..passes {
        signal_owned_process_tree_phase(
            input.controller,
            ledger,
            input.root.as_deref_mut(),
            input.root_pid,
            libc::SIGKILL,
            true,
        )?;
        sleep(Duration::from_millis(10)).await;
    }
    let final_table = input.controller.snapshot()?;
    capture_posix_ownership_ledger(
        input.controller,
        ledger,
        input.root.as_deref_mut(),
        input.root_pid,
        Some(&final_table),
    )?;
    let survivors: HashMap<i32, AcpPosixProcessIdentity> = final_table
        .into_iter()
        .map(|entry| (entry.pid, entry))
        .collect();
    let residual: Vec<i32> = ledger
        .values()
        .filter_map(|owned| {
            let observed = survivors.get(&owned.pid)?;
            if !same_posix_process_identity(owned, Some(observed)) {
                return None;
            }
            if posix_process_is_zombie(observed) {
                return None;
            }
            Some(owned.pid)
        })
        .collect();
    if !residual.is_empty() {
        return Err(AcpProcessGroupTerminationError::new(format!(
            "ACP process tree {} retained owned processes: {}",
            input.root_pid,
            residual
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}
