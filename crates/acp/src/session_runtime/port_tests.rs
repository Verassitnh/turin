//! Tests ported from upstream AcpSessionRuntime.processTree.test.ts and AcpJsonRpcConnection.test.ts.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::session_runtime::{
    AcpAuthMethod, AcpLinuxCgroupLease, AcpOwnedPosixProcess, AcpPosixOwnershipRoot,
    AcpPosixProcessIdentity, AcpPosixProcessTreeController, CgroupPopulatedState,
    SweepStaleLinuxCgroupSiblingsOptions, capture_posix_ownership_ledger, is_safe_cgroup_path,
    make_posix_process_tree_controller, observe_posix_ownership_ledger, parse_cgroup2_mount,
    parse_cgroup2_mounts, parse_unified_cgroup_path, select_acp_agent_auth_method,
    signal_linux_cgroup_root_term, sweep_stale_linux_cgroup_siblings,
};

#[cfg(target_os = "linux")]
use crate::session_runtime::wrap_command_for_linux_cgroup;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

fn identity(
    pid: i32,
    ppid: i32,
    pgid: i32,
    sid: i32,
    start_time: &str,
    state: Option<&str>,
) -> AcpPosixProcessIdentity {
    AcpPosixProcessIdentity {
        executable: Some(format!("/proc/{pid}/exe")),
        pgid,
        pid,
        ppid,
        sid,
        start_time: start_time.to_string(),
        state: state.map(str::to_string),
    }
}

struct TestController {
    pub(crate) processes: Mutex<HashMap<i32, AcpPosixProcessIdentity>>,
    signals: Mutex<Vec<String>>,
}

impl TestController {
    fn new(processes: Vec<AcpPosixProcessIdentity>) -> Self {
        Self {
            processes: Mutex::new(processes.into_iter().map(|p| (p.pid, p)).collect()),
            signals: Mutex::new(Vec::new()),
        }
    }

    fn signals(&self) -> Vec<String> {
        self.signals.lock().unwrap().clone()
    }
}

impl AcpPosixProcessTreeController for TestController {
    fn child_pids_of(&self, pid: i32) -> Vec<i32> {
        self.processes
            .lock()
            .unwrap()
            .values()
            .filter(|process| process.ppid == pid)
            .map(|process| process.pid)
            .collect()
    }

    fn children_of(&self, pid: i32) -> Vec<AcpPosixProcessIdentity> {
        self.processes
            .lock()
            .unwrap()
            .values()
            .filter(|process| process.ppid == pid)
            .cloned()
            .collect()
    }

    fn identity(&self, pid: i32) -> Option<AcpPosixProcessIdentity> {
        self.processes.lock().unwrap().get(&pid).cloned()
    }

    fn snapshot(
        &self,
    ) -> Result<Vec<AcpPosixProcessIdentity>, crate::session_runtime::AcpProcessGroupTerminationError>
    {
        Ok(self.processes.lock().unwrap().values().cloned().collect())
    }

    fn signal_process(&self, pid: i32, signal: i32) {
        let signal_name = match signal {
            libc::SIGTERM => "SIGTERM",
            libc::SIGKILL => "SIGKILL",
            _ => "SIGNAL",
        };
        self.signals
            .lock()
            .unwrap()
            .push(format!("process:{pid}:{signal_name}"));
        self.processes.lock().unwrap().remove(&pid);
    }
}

fn server() -> AcpPosixProcessIdentity {
    let pid = std::process::id() as i32;
    identity(pid, 1, pid, pid, "server", Some("S"))
}

#[test]
fn parses_unified_cgroup_paths_and_escaped_cgroup2_mountinfo() {
    assert_eq!(
        parse_unified_cgroup_path("0::/user.slice/app.scope\n").as_deref(),
        Some("/user.slice/app.scope")
    );
    assert!(parse_unified_cgroup_path("0::/one\n0::/two\n").is_none());
    assert!(is_safe_cgroup_path("/user.slice/app.scope"));
    assert!(!is_safe_cgroup_path("/user.slice/../escape"));
    assert!(!is_safe_cgroup_path("/user.slice/app.scope (deleted)"));
    assert_eq!(
        parse_cgroup2_mount(
            "42 31 0:37 /user.slice /sys/fs/cgroup/user\\040mount rw - cgroup2 cgroup2 rw\n"
        ),
        Some(crate::session_runtime::Cgroup2Mount {
            mount_point: "/sys/fs/cgroup/user mount".to_string(),
            root: "/user.slice".to_string(),
        })
    );
    assert!(parse_cgroup2_mount("42 31 0:37 / /sys/fs/cgroup rw - tmpfs tmpfs rw\n").is_none());
    assert_eq!(
        parse_cgroup2_mounts(
            "41 31 0:36 /other /sys/fs/cgroup/other rw - cgroup2 cgroup2 rw\n\
         42 31 0:37 /user.slice /sys/fs/cgroup/user rw - cgroup2 cgroup2 rw\n"
        )
        .len(),
        2
    );
}

#[test]
fn sweeps_empty_t3_acp_sibling_leases_owned_by_dead_pids_only() {
    let parent = TempDir::new().unwrap();
    let parent = parent.path();
    let populated_by_path: Mutex<HashMap<PathBuf, CgroupPopulatedState>> =
        Mutex::new(HashMap::new());
    let write_sibling = |name: &str, populated: Option<CgroupPopulatedState>| -> PathBuf {
        let path = parent.join(name);
        fs::create_dir(&path).unwrap();
        if let Some(state) = populated {
            populated_by_path
                .lock()
                .unwrap()
                .insert(path.clone(), state);
        }
        path
    };
    let dead_pid = 2_147_483_646_i32;
    let live_foreign_pid = 2_147_483_645_i32;
    let stale_empty = write_sibling(
        &format!("t3-acp-{dead_pid}-aaaa"),
        Some(CgroupPopulatedState::Empty),
    );
    let current_pid = std::process::id() as i32;
    let current_pid_sibling = write_sibling(
        &format!("t3-acp-{current_pid}-bbbb"),
        Some(CgroupPopulatedState::Empty),
    );
    let live_foreign_sibling = write_sibling(
        &format!("t3-acp-{live_foreign_pid}-cccc"),
        Some(CgroupPopulatedState::Empty),
    );
    let stale_populated = write_sibling(
        &format!("t3-acp-{dead_pid}-dddd"),
        Some(CgroupPopulatedState::Populated),
    );
    let unrelated = write_sibling("other-lease", Some(CgroupPopulatedState::Empty));
    let is_process_alive = |pid: i32| pid == current_pid || pid == live_foreign_pid;
    sweep_stale_linux_cgroup_siblings(
        parent.to_str().unwrap(),
        SweepStaleLinuxCgroupSiblingsOptions {
            current_pid: None,
            is_process_alive: Some(&is_process_alive),
            read_populated: Some(&|path| {
                populated_by_path
                    .lock()
                    .unwrap()
                    .get(&PathBuf::from(path))
                    .copied()
            }),
            remove: Some(&|path| {
                populated_by_path
                    .lock()
                    .unwrap()
                    .remove(&PathBuf::from(path));
                fs::remove_dir(path)?;
                Ok(())
            }),
        },
    );
    assert!(!stale_empty.exists());
    assert!(current_pid_sibling.exists());
    assert!(live_foreign_sibling.exists());
    assert!(stale_populated.exists());
    assert!(unrelated.exists());
    let missing = write_sibling(&format!("t3-acp-{dead_pid}-eeee"), None);
    sweep_stale_linux_cgroup_siblings(
        parent.to_str().unwrap(),
        SweepStaleLinuxCgroupSiblingsOptions {
            current_pid: None,
            is_process_alive: Some(&is_process_alive),
            read_populated: Some(&|path| {
                populated_by_path
                    .lock()
                    .unwrap()
                    .get(&PathBuf::from(path))
                    .copied()
            }),
            remove: Some(&|path| {
                populated_by_path
                    .lock()
                    .unwrap()
                    .remove(&PathBuf::from(path));
                fs::remove_dir(path)?;
                Ok(())
            }),
        },
    );
    assert!(missing.exists());
    assert!(current_pid_sibling.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn contains_packaged_runtime_command_without_requiring_cgroup_delegation() {
    let scratch = TempDir::new().unwrap();
    let lease_path = scratch.path().join("lease with spaces");
    let output_path = scratch.path().join("argv output");
    fs::create_dir(&lease_path).unwrap();
    fs::write(lease_path.join("cgroup.procs"), "").unwrap();
    let relative_path = fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|contents| parse_unified_cgroup_path(&contents));
    assert!(relative_path.is_some());
    struct FakeLease {
        path: PathBuf,
        relative_path: String,
    }
    impl AcpLinuxCgroupLease for FakeLease {
        fn contains(&self, _pid: i32) -> bool {
            false
        }
        fn exists(&self) -> bool {
            true
        }
        fn path(&self) -> &str {
            self.path.to_str().unwrap()
        }
        fn relative_path(&self) -> &str {
            &self.relative_path
        }
        fn kill(&self) -> Result<(), std::io::Error> {
            Ok(())
        }
        fn populated(&self) -> Result<bool, std::io::Error> {
            Ok(false)
        }
        fn remove(&self) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
    let lease = FakeLease {
        path: lease_path.clone(),
        relative_path: relative_path.unwrap(),
    };
    let wrapped = wrap_command_for_linux_cgroup(
        &lease,
        "/bin/sh",
        &[
            "-c".to_string(),
            r#"printf "%s\n" "$0" "$1" "$2" "$3" "${ELECTRON_RUN_AS_NODE-}" "${T3_ACP_CGROUP_WRAPPER-}" > "$4""#
                .to_string(),
            "packaged-target".to_string(),
            "space value".to_string(),
            "single'quote".to_string(),
            "double\"quote".to_string(),
            output_path.to_string_lossy().to_string(),
        ],
    );
    let result = Command::new(&wrapped.0)
        .args(&wrapped.1)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .env("T3_ACP_CGROUP_WRAPPER", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .unwrap();
    assert!(result.success(), "wrapper failed");
    assert_eq!(
        fs::read_to_string(&output_path).unwrap(),
        "packaged-target\nspace value\nsingle'quote\ndouble\"quote\n\n\n"
    );
    assert!(
        fs::read_to_string(lease_path.join("cgroup.procs"))
            .unwrap()
            .trim()
            .chars()
            .all(|ch| ch.is_ascii_digit())
    );

    let mismatch = wrap_command_for_linux_cgroup(
        &FakeLease {
            path: lease_path.clone(),
            relative_path: "/not-the-current-cgroup".to_string(),
        },
        "/bin/sh",
        &[
            "-c".to_string(),
            r#"printf executed > "$0""#.to_string(),
            output_path.to_string_lossy().to_string(),
        ],
    );
    let _ = fs::remove_file(&output_path);
    let mismatch_result = Command::new(&mismatch.0)
        .args(&mismatch.1)
        .status()
        .unwrap();
    assert_eq!(mismatch_result.code(), Some(126));
    assert!(!output_path.exists());

    let missing_target = wrap_command_for_linux_cgroup(&lease, "/nonexistent-t3-probe", &[]);
    let missing_target_result = Command::new(&missing_target.0)
        .args(&missing_target.1)
        .status()
        .unwrap();
    assert_eq!(missing_target_result.code(), Some(125));
}

#[test]
fn sends_term_only_to_exact_captured_root_while_it_remains_in_child_cgroup() {
    let fixture = Arc::new(TestController::new(vec![
        server(),
        identity(100, std::process::id() as i32, 100, 100, "owned", Some("S")),
    ]));
    let owned_root = AcpOwnedPosixProcess {
        executable: Some("/proc/100/exe".to_string()),
        pgid: 100,
        pid: 100,
        ppid: std::process::id() as i32,
        sid: 100,
        state: Some("S".to_string()),
        start_time: "owned".to_string(),
        parent_executable: server().executable.clone(),
        parent_start_time: server().start_time.clone(),
    };
    let root = AcpPosixOwnershipRoot {
        capture_attempted: true,
        value: Some(owned_root),
    };
    struct FakeLease;
    impl AcpLinuxCgroupLease for FakeLease {
        fn contains(&self, _pid: i32) -> bool {
            true
        }
        fn exists(&self) -> bool {
            true
        }
        fn path(&self) -> &str {
            "/test/t3-acp-root"
        }
        fn relative_path(&self) -> &str {
            "/test/t3-acp-root"
        }
        fn kill(&self) -> Result<(), std::io::Error> {
            Ok(())
        }
        fn populated(&self) -> Result<bool, std::io::Error> {
            Ok(true)
        }
        fn remove(&self) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
    signal_linux_cgroup_root_term(fixture.as_ref(), &FakeLease, &root);
    assert_eq!(fixture.signals(), vec!["process:100:SIGTERM"]);

    let reused = Arc::new(TestController::new(vec![
        server(),
        identity(
            100,
            std::process::id() as i32,
            100,
            100,
            "reused",
            Some("S"),
        ),
    ]));
    signal_linux_cgroup_root_term(reused.as_ref(), &FakeLease, &root);
    assert!(reused.signals().is_empty());

    struct NoContainLease;
    impl AcpLinuxCgroupLease for NoContainLease {
        fn contains(&self, _pid: i32) -> bool {
            false
        }
        fn exists(&self) -> bool {
            true
        }
        fn path(&self) -> &str {
            "/test/t3-acp-root"
        }
        fn relative_path(&self) -> &str {
            "/test/t3-acp-root"
        }
        fn kill(&self) -> Result<(), std::io::Error> {
            Ok(())
        }
        fn populated(&self) -> Result<bool, std::io::Error> {
            Ok(true)
        }
        fn remove(&self) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
    let migrated = Arc::new(TestController::new(vec![
        server(),
        identity(100, std::process::id() as i32, 100, 100, "owned", Some("S")),
    ]));
    signal_linux_cgroup_root_term(migrated.as_ref(), &NoContainLease, &root);
    assert!(migrated.signals().is_empty());
}

#[test]
fn never_adopts_replacement_pid_after_initial_root_capture_failed() {
    let fixture = Arc::new(TestController::new(vec![server()]));
    let mut frontier = HashMap::new();
    let mut child_queues = HashMap::new();
    let mut ledger = HashMap::new();
    let mut root = AcpPosixOwnershipRoot::default();
    let mut capture = || {
        observe_posix_ownership_ledger(
            &mut child_queues,
            fixture.as_ref(),
            &mut frontier,
            &mut ledger,
            &mut root,
            100,
            None,
        )
    };
    let err = capture().unwrap_err();
    assert!(
        err.detail
            .contains("ACP process 100 exited before its ownership ledger was captured")
    );
    fixture.processes.lock().unwrap().insert(
        100,
        identity(
            100,
            std::process::id() as i32,
            100,
            100,
            "reused",
            Some("S"),
        ),
    );
    let err = capture().unwrap_err();
    assert!(
        err.detail
            .contains("ACP process 100 exited before its ownership ledger was captured")
    );
    assert!(ledger.is_empty());
}

#[test]
fn admits_live_child_after_free_pid_was_previously_reserved_by_dead_entry() {
    let stale_child = AcpOwnedPosixProcess {
        executable: Some("/proc/200/exe".to_string()),
        pgid: 200,
        pid: 200,
        ppid: 100,
        sid: 200,
        state: Some("S".to_string()),
        start_time: "stale-child".to_string(),
        parent_executable: Some("/proc/100/exe".to_string()),
        parent_start_time: "100".to_string(),
    };
    let live_child = identity(200, 100, 200, 200, "live-child", Some("S"));
    let root = identity(100, std::process::id() as i32, 100, 100, "100", Some("S"));
    let owned_root = AcpOwnedPosixProcess {
        executable: root.executable.clone(),
        pgid: root.pgid,
        pid: root.pid,
        ppid: root.ppid,
        sid: root.sid,
        state: root.state.clone(),
        start_time: root.start_time.clone(),
        parent_executable: server().executable.clone(),
        parent_start_time: "server".to_string(),
    };
    let mut ledger = HashMap::from([
        (
            format!("{}:{}", root.pid, root.start_time),
            owned_root.clone(),
        ),
        (
            format!("{}:{}", stale_child.pid, stale_child.start_time),
            stale_child,
        ),
    ]);
    let controller = TestController::new(vec![server(), root.clone()]);
    capture_posix_ownership_ledger(
        &controller,
        &mut ledger,
        None,
        100,
        Some(std::slice::from_ref(&root)),
    )
    .unwrap();
    assert!(!ledger.values().any(|entry| entry.pid == 200));

    let controller = TestController::new(vec![server(), root.clone(), live_child.clone()]);
    capture_posix_ownership_ledger(
        &controller,
        &mut ledger,
        None,
        100,
        Some(&[root, live_child.clone()]),
    )
    .unwrap();
    let admitted = ledger.values().find(|entry| entry.pid == 200).unwrap();
    assert_eq!(admitted.start_time, "live-child");
}

#[test]
fn admits_live_child_when_stale_ledger_entry_still_occupies_reused_pid() {
    let stale_child = AcpOwnedPosixProcess {
        executable: Some("/proc/200/exe".to_string()),
        pgid: 200,
        pid: 200,
        ppid: 100,
        sid: 200,
        state: Some("S".to_string()),
        start_time: "stale-child".to_string(),
        parent_executable: Some("/proc/100/exe".to_string()),
        parent_start_time: "100".to_string(),
    };
    let live_child = identity(200, 100, 200, 200, "live-child", Some("S"));
    let root = identity(100, std::process::id() as i32, 100, 100, "100", Some("S"));
    let owned_root = AcpOwnedPosixProcess {
        executable: root.executable.clone(),
        pgid: root.pgid,
        pid: root.pid,
        ppid: root.ppid,
        sid: root.sid,
        state: root.state.clone(),
        start_time: root.start_time.clone(),
        parent_executable: server().executable.clone(),
        parent_start_time: "server".to_string(),
    };
    let mut ledger = HashMap::from([
        (format!("{}:{}", root.pid, root.start_time), owned_root),
        (
            format!("{}:{}", stale_child.pid, stale_child.start_time),
            stale_child,
        ),
    ]);
    let root_for_table = identity(100, std::process::id() as i32, 100, 100, "100", Some("S"));
    let controller =
        TestController::new(vec![server(), root_for_table.clone(), live_child.clone()]);
    capture_posix_ownership_ledger(
        &controller,
        &mut ledger,
        None,
        100,
        Some(&[root_for_table, live_child]),
    )
    .unwrap();
    assert!(
        !ledger
            .values()
            .any(|entry| entry.start_time == "stale-child")
    );
    let admitted = ledger.values().find(|entry| entry.pid == 200).unwrap();
    assert_eq!(admitted.start_time, "live-child");
}

#[test]
fn fails_closed_where_stable_posix_identity_provider_is_unavailable() {
    match make_posix_process_tree_controller("darwin") {
        Err(error) => assert_eq!(
            error.detail,
            "Detached ACP descendant ownership is unsupported on darwin"
        ),
        Ok(_) => panic!("expected darwin controller to fail closed"),
    }
}

#[test]
fn selects_explicit_or_agent_managed_authentication_without_choosing_terminal_auth() {
    let methods = vec![
        AcpAuthMethod {
            id: "browser".to_string(),
            method_type: Some("terminal".to_string()),
        },
        AcpAuthMethod {
            id: "api-key".to_string(),
            method_type: Some("agent".to_string()),
        },
    ];
    assert_eq!(
        select_acp_agent_auth_method(Some(&methods), None).map(|method| method.id),
        Some("api-key".to_string())
    );
    assert_eq!(
        select_acp_agent_auth_method(Some(&methods), Some("browser")).map(|method| method.id),
        Some("browser".to_string())
    );
    assert!(select_acp_agent_auth_method(Some(&methods), Some("missing")).is_none());
}
