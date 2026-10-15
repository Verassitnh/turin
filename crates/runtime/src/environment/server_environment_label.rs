//! Port of upstream/apps/server/src/environment/ServerEnvironmentLabel.ts
//!
//! The human label for this machine: the friendly host name when the OS has
//! one (macOS ComputerName, Linux PRETTY_HOSTNAME), else the hostname, else
//! the working directory's base name.

use std::path::Path;
use std::process::Command;

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

/// `readLinuxMachineInfo`. A missing or unreadable file is logged and treated
/// as absent, as upstream does.
fn read_linux_machine_info() -> Option<String> {
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

/// `runFriendlyLabelCommand`: stdout of a successful probe, or `None`. A probe
/// that cannot start is logged and skipped.
fn run_friendly_label_command(probe: &str, command: &str, args: &[&str]) -> Option<String> {
    match Command::new(command).args(args).output() {
        Ok(output) if output.status.success() => {
            normalize_label(Some(&String::from_utf8_lossy(&output.stdout)))
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

/// `resolveFriendlyHostLabel`, with `platform` in the `std::env::consts::OS`
/// spelling.
fn resolve_friendly_host_label(platform: &str) -> Option<String> {
    match platform {
        "macos" => {
            run_friendly_label_command("macos-computer-name", "scutil", &["--get", "ComputerName"])
        }
        "linux" => {
            let machine_info = normalize_label(read_linux_machine_info().as_deref());
            if let Some(pretty) = machine_info
                .as_deref()
                .and_then(|info| parse_machine_info_value(info, "PRETTY_HOSTNAME"))
            {
                return Some(pretty);
            }
            run_friendly_label_command("linux-pretty-hostname", "hostnamectl", &["--pretty"])
        }
        _ => None,
    }
}

/// `os.hostname()` through `gethostname(3)`.
fn host_name() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is writable for its full length and gethostname
    // NUL-terminates on success.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    normalize_label(Some(&String::from_utf8_lossy(&buffer[..end])))
}

/// `resolveServerEnvironmentLabel`. `cwd_base_name` is the working
/// directory's last path component.
pub fn resolve_server_environment_label(cwd_base_name: &str) -> String {
    if let Some(friendly) = resolve_friendly_host_label(std::env::consts::OS) {
        return friendly;
    }
    if let Some(hostname) = host_name() {
        return hostname;
    }
    normalize_label(Some(cwd_base_name)).unwrap_or_else(|| "T3 environment".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

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
