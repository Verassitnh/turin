//! `fix_process_path` against the real process environment, with a fake login
//! shell. One test in this file: it edits the process environment, which no
//! other thread of this test binary reads.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn spawned_commands_find_binaries_on_the_login_shell_path() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("shell-only-bin");
    std::fs::create_dir(&bin).unwrap();
    let write_script = |path: &std::path::Path, body: &str| {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_script(&bin.join("claude"), "#!/bin/sh\necho fake-claude\n");
    let shell = dir.path().join("fake-shell");
    write_script(
        &shell,
        &format!(
            "#!/bin/sh\n\
             echo noise\n\
             printf '%s\\n' '__T3CODE_ENV_PATH_START__' '{}:/usr/bin' '__T3CODE_ENV_PATH_END__'\n",
            bin.display()
        ),
    );

    // SAFETY: the only test in this binary, so no other thread touches the environment.
    unsafe {
        std::env::set_var("SHELL", &shell);
        std::env::set_var("PATH", "/usr/bin:/bin");
    }
    assert!(
        Command::new("claude").output().is_err(),
        "invisible before the fix"
    );

    turin_shared::os_jank::fix_process_path();

    let path = std::env::var("PATH").unwrap();
    assert!(path.starts_with(&format!("{}:", bin.display())), "{path}");
    let output = Command::new("claude")
        .output()
        .expect("claude resolves on the merged PATH");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "fake-claude"
    );
}
