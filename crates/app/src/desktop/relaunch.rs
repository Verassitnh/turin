//! "Restart Turin": starts a fresh instance of this app and quits this one.
//!
//! Upstream's desktop shell restarts through Electron's `app.relaunch()`. A bundle is reopened
//! through LaunchServices (`open -n <bundle>`) so it keeps its Dock identity; a bare binary
//! (`cargo run`) runs again as is. The new instance starts a second after this one quits, so the
//! old process has released the database by the time the new one adopts a staged T3 import.

use std::path::Path;
use std::process::{Command, Stdio};

use gpui_kit::App;

/// How long the new instance waits, so this process has quit and closed its database first.
const RESTART_DELAY_SECONDS: &str = "1";

/// The program and arguments that start this app again: `open -n <bundle>` when `exe` sits in
/// an `.app` bundle, else `exe` itself.
pub fn relaunch_program(exe: &Path) -> (String, Vec<String>) {
    let exe_text = exe.to_string_lossy();
    match exe_text.find(".app/Contents/MacOS/") {
        Some(index) => (
            "/usr/bin/open".to_owned(),
            vec!["-n".to_owned(), exe_text[..index + ".app".len()].to_owned()],
        ),
        None => (exe_text.into_owned(), Vec::new()),
    }
}

/// Starts the delayed restart. Fails when this executable's path is unknown or `sh` will not run.
pub fn spawn_relaunch() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let (program, args) = relaunch_program(&exe);
    // `sh -c 'sleep N; exec "$@"' sh <program> <args…>`: the arguments travel as positional
    // parameters, so no path needs quoting.
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("sleep {RESTART_DELAY_SECONDS}; exec \"$@\""))
        .arg("sh")
        .arg(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Restarts the app. Quits only when the new instance could be started.
pub fn relaunch_and_quit(cx: &mut App) {
    match spawn_relaunch() {
        Ok(()) => cx.quit(),
        Err(error) => tracing::error!(%error, "restarting Turin failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_is_reopened_through_launch_services() {
        assert_eq!(
            relaunch_program(Path::new(
                "/Users/me/Applications/Turin.app/Contents/MacOS/turin"
            )),
            (
                "/usr/bin/open".to_owned(),
                vec![
                    "-n".to_owned(),
                    "/Users/me/Applications/Turin.app".to_owned()
                ]
            )
        );
    }

    #[test]
    fn a_bare_binary_runs_again_as_is() {
        assert_eq!(
            relaunch_program(Path::new("/work/target/debug/turin")),
            ("/work/target/debug/turin".to_owned(), Vec::new())
        );
    }
}
