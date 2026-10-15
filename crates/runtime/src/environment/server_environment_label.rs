//! Port of upstream/apps/server/src/environment/ServerEnvironmentLabel.ts
//!
//! The human label for this machine: the friendly host name when the OS has
//! one (macOS ComputerName, Linux PRETTY_HOSTNAME), else the hostname, else
//! the working directory's base name. Host access goes through [`HostProbe`]
//! so the fallback order can be tested without the real machine.

use std::path::Path;
use std::process::Command;

/// The host facts the label reads. [`SystemHost`] is the real one.
pub trait HostProbe {
    /// `HostProcess.Platform`, in `std::env::consts::OS` spelling.
    fn platform(&self) -> &str;
    /// Contents of `/etc/machine-info`, or `None` when absent or unreadable.
    fn machine_info(&self) -> Option<String>;
    /// Stdout of a successful label probe, or `None` when it fails or cannot start.
    fn run_probe(&self, probe: &str, command: &str, args: &[&str]) -> Option<String>;
    /// `HostProcess.Hostname`.
    fn hostname(&self) -> Option<String>;
}

/// The real host: `std::env::consts::OS`, `/etc/machine-info`, `scutil`,
/// `hostnamectl`, and `gethostname(3)`.
pub struct SystemHost;

impl HostProbe for SystemHost {
    fn platform(&self) -> &str {
        std::env::consts::OS
    }

    fn machine_info(&self) -> Option<String> {
        let machine_info_path = Path::new("/etc/machine-info");
        match std::fs::read_to_string(machine_info_path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::debug!(
                    path = %machine_info_path.display(),
                    error = %error,
                    "failed to read environment-label file"
                );
                None
            }
        }
    }

    fn run_probe(&self, probe: &str, command: &str, args: &[&str]) -> Option<String> {
        match Command::new(command).args(args).output() {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            Ok(_) => None,
            Err(error) => {
                tracing::debug!(
                    probe,
                    executable = command,
                    argument_count = args.len(),
                    error = %error,
                    "failed to run environment-label probe"
                );
                None
            }
        }
    }

    fn hostname(&self) -> Option<String> {
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer is writable for its full length, and gethostname
        // NUL-terminates on success.
        let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if status != 0 {
            return None;
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf8_lossy(&buffer[..end]).into_owned())
    }
}

/// `normalizeLabel`: trimmed, `None` when empty.
fn normalize_label(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// `parseMachineInfoValue`: finds `KEY=value` in `/etc/machine-info`, skipping
/// comments, and strips one pair of matching surrounding quotes.
fn parse_machine_info_value(raw: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for line in raw.split('\n').map(|line| line.trim_end_matches('\r')) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.starts_with(&prefix) {
            continue;
        }
        let value = trimmed[prefix.len()..].trim();
        let quoted = (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
            || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2);
        if quoted {
            return normalize_label(Some(&value[1..value.len() - 1]));
        }
        return normalize_label(Some(value));
    }
    None
}

/// `resolveFriendlyHostLabel`.
fn resolve_friendly_host_label(host: &impl HostProbe) -> Option<String> {
    match host.platform() {
        "macos" => normalize_label(
            host.run_probe("macos-computer-name", "scutil", &["--get", "ComputerName"])
                .as_deref(),
        ),
        "linux" => {
            let machine_info = normalize_label(host.machine_info().as_deref());
            if let Some(pretty) = machine_info
                .as_deref()
                .and_then(|info| parse_machine_info_value(info, "PRETTY_HOSTNAME"))
            {
                return Some(pretty);
            }
            normalize_label(
                host.run_probe("linux-pretty-hostname", "hostnamectl", &["--pretty"])
                    .as_deref(),
            )
        }
        _ => None,
    }
}

/// `resolveServerEnvironmentLabel`. `cwd_base_name` is the working
/// directory's last path component.
pub fn resolve_server_environment_label(host: &impl HostProbe, cwd_base_name: &str) -> String {
    if let Some(friendly) = resolve_friendly_host_label(host) {
        return friendly;
    }
    if let Some(hostname) = normalize_label(host.hostname().as_deref()) {
        return hostname;
    }
    normalize_label(Some(cwd_base_name)).unwrap_or_else(|| "T3 environment".to_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    /// A host whose probes answer from a table. Probes absent from the table fail.
    #[derive(Default)]
    struct FakeHost {
        platform: &'static str,
        machine_info: Option<String>,
        probes: HashMap<&'static str, String>,
        hostname: Option<String>,
        probe_calls: RefCell<Vec<String>>,
    }

    impl HostProbe for FakeHost {
        fn platform(&self) -> &str {
            self.platform
        }
        fn machine_info(&self) -> Option<String> {
            self.machine_info.clone()
        }
        fn run_probe(&self, probe: &str, _command: &str, _args: &[&str]) -> Option<String> {
            self.probe_calls.borrow_mut().push(probe.to_owned());
            self.probes.get(probe).cloned()
        }
        fn hostname(&self) -> Option<String> {
            self.hostname.clone()
        }
    }

    #[test]
    fn uses_the_hostname_when_no_friendly_label_exists() {
        let host = FakeHost {
            platform: "linux",
            hostname: Some("devbox".into()),
            ..Default::default()
        };
        assert_eq!(resolve_server_environment_label(&host, "repo"), "devbox");
    }

    #[test]
    fn prefers_the_macos_computer_name() {
        let host = FakeHost {
            platform: "macos",
            probes: HashMap::from([("macos-computer-name", " Studio Mac \n".to_owned())]),
            hostname: Some("ignored.local".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_server_environment_label(&host, "repo"),
            "Studio Mac"
        );
    }

    #[test]
    fn prefers_linux_pretty_hostname_from_machine_info() {
        let host = FakeHost {
            platform: "linux",
            machine_info: Some("PRETTY_HOSTNAME=\"Lab box\"\n".into()),
            probes: HashMap::from([("linux-pretty-hostname", "other".to_owned())]),
            ..Default::default()
        };
        assert_eq!(resolve_server_environment_label(&host, "repo"), "Lab box");
        assert!(host.probe_calls.borrow().is_empty());
    }

    #[test]
    fn falls_back_to_hostnamectl_on_linux() {
        let host = FakeHost {
            platform: "linux",
            machine_info: Some("# nothing here\n".into()),
            probes: HashMap::from([("linux-pretty-hostname", "Rack 3\n".to_owned())]),
            ..Default::default()
        };
        assert_eq!(resolve_server_environment_label(&host, "repo"), "Rack 3");
    }

    #[test]
    fn falls_back_to_the_hostname_when_friendly_labels_are_unavailable() {
        let host = FakeHost {
            platform: "linux",
            hostname: Some("box-7".into()),
            ..Default::default()
        };
        assert_eq!(resolve_server_environment_label(&host, "repo"), "box-7");
    }

    #[test]
    fn falls_back_to_the_hostname_when_the_probe_command_is_missing() {
        // A probe that cannot start is the same as a probe that returns nothing.
        let host = FakeHost {
            platform: "macos",
            hostname: Some("mini.local".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_server_environment_label(&host, "repo"),
            "mini.local"
        );
    }

    #[test]
    fn continues_to_hostnamectl_after_machine_info_is_unreadable() {
        // An unreadable machine-info reads as absent, so the probe still runs.
        let host = FakeHost {
            platform: "linux",
            machine_info: None,
            probes: HashMap::from([("linux-pretty-hostname", "Fallback Pretty".to_owned())]),
            ..Default::default()
        };
        assert_eq!(
            resolve_server_environment_label(&host, "repo"),
            "Fallback Pretty"
        );
        assert_eq!(
            *host.probe_calls.borrow(),
            vec!["linux-pretty-hostname".to_owned()]
        );
    }

    #[test]
    fn falls_back_to_the_cwd_basename_when_the_hostname_is_blank() {
        let host = FakeHost {
            platform: "unknown",
            hostname: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_server_environment_label(&host, "my-repo"),
            "my-repo"
        );
        assert_eq!(
            resolve_server_environment_label(&host, "  "),
            "T3 environment"
        );
    }

    #[test]
    fn reads_pretty_hostname_with_or_without_quotes() {
        let raw = "# comment\nPRETTY_HOSTNAME=\"Studio Mac\"\nICON_NAME=computer\n";
        assert_eq!(
            parse_machine_info_value(raw, "PRETTY_HOSTNAME").as_deref(),
            Some("Studio Mac")
        );
        assert_eq!(
            parse_machine_info_value("PRETTY_HOSTNAME=  devbox  ", "PRETTY_HOSTNAME").as_deref(),
            Some("devbox")
        );
        assert_eq!(
            parse_machine_info_value("PRETTY_HOSTNAME='lab'", "PRETTY_HOSTNAME").as_deref(),
            Some("lab")
        );
    }

    #[test]
    fn ignores_missing_empty_and_commented_values() {
        assert_eq!(
            parse_machine_info_value("# PRETTY_HOSTNAME=x", "PRETTY_HOSTNAME"),
            None
        );
        assert_eq!(
            parse_machine_info_value("PRETTY_HOSTNAME=\"\"", "PRETTY_HOSTNAME"),
            None
        );
        assert_eq!(parse_machine_info_value("OTHER=x", "PRETTY_HOSTNAME"), None);
    }

    #[test]
    fn normalizes_blank_labels_to_none() {
        assert_eq!(normalize_label(Some("   ")), None);
        assert_eq!(normalize_label(None), None);
        assert_eq!(normalize_label(Some(" box ")).as_deref(), Some("box"));
    }
}
