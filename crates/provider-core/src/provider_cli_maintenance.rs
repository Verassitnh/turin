//! Maintenance definitions for the four kept CLIs: codex, claude, opencode and
//! cursor-agent. Each one's native-updater path predicate is ported from its
//! driver file (`CodexDriver.ts`, `ClaudeDriver.ts`, `provider-opencode/.../driver.ts`).
//! Cursor has no npm package and no native updater here, so it is manual-only,
//! as upstream's `provider-cursor/src/server/driver.ts` declares.

use turin_contracts::provider_instance::ProviderDriverKind;

use crate::{
    host_process::ProcessEnv,
    maintenance::ProviderMaintenanceCapabilities,
    maintenance_resolver::make_manual_only_provider_maintenance_capabilities,
    package_managed_maintenance::{
        NativeUpdate, PackageManagedProviderMaintenanceDefinition, normalize_command_path,
    },
};

/// `isCodexStandaloneCommandPath`: the standalone tree the Codex installer writes.
fn is_codex_standalone_command_path(command_path: &str) -> bool {
    normalize_command_path(command_path).contains("/packages/standalone/")
}

/// `isClaudeNativeCommandPath`: the native installer's launcher and data directory.
fn is_claude_native_command_path(command_path: &str) -> bool {
    let normalized = normalize_command_path(command_path);
    normalized.ends_with("/.local/bin/claude")
        || normalized.ends_with("/.local/bin/claude.exe")
        || normalized.contains("/.local/share/claude/")
}

/// `isOpenCodeNativeCommandPath`: the install script's `~/.opencode/bin` launcher.
fn is_opencode_native_command_path(command_path: &str) -> bool {
    let normalized = normalize_command_path(command_path);
    normalized.ends_with("/.opencode/bin/opencode")
        || normalized.ends_with("/.opencode/bin/opencode.exe")
}

/// `codex update` replaces the standalone tree under `CODEX_HOME`. That tree
/// lives in the shared home even when an auth-overlay shadow home is in use, so
/// the updater runs against `shared_home_path` rather than the instance's home.
pub fn codex_maintenance_definition(
    shared_home_path: &str,
) -> PackageManagedProviderMaintenanceDefinition {
    PackageManagedProviderMaintenanceDefinition {
        provider: ProviderDriverKind::new("codex").expect("valid driver kind"),
        npm_package_name: "@openai/codex".into(),
        native_update: Some(NativeUpdate {
            args: vec!["update".into()],
            is_command_path: Some(is_codex_standalone_command_path),
            env: Some(ProcessEnv::from([(
                "CODEX_HOME".to_owned(),
                shared_home_path.to_owned(),
            )])),
        }),
    }
}

pub fn claude_maintenance_definition() -> PackageManagedProviderMaintenanceDefinition {
    PackageManagedProviderMaintenanceDefinition {
        provider: ProviderDriverKind::new("claudeAgent").expect("valid driver kind"),
        npm_package_name: "@anthropic-ai/claude-code".into(),
        native_update: Some(NativeUpdate {
            args: vec!["update".into()],
            is_command_path: Some(is_claude_native_command_path),
            env: None,
        }),
    }
}

/// OpenCode 1.x ships as `opencode-ai` and 2.x as `@opencode/cli`. An install is
/// only ever updated within its own package: 2.x converts the shared database
/// in place, so T3 never moves a 1.x install onto 2.x or back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeGeneration {
    V1,
    V2,
}

pub fn opencode_maintenance_definition(
    generation: OpenCodeGeneration,
) -> PackageManagedProviderMaintenanceDefinition {
    PackageManagedProviderMaintenanceDefinition {
        provider: ProviderDriverKind::new("opencode").expect("valid driver kind"),
        npm_package_name: match generation {
            OpenCodeGeneration::V1 => "opencode-ai",
            OpenCodeGeneration::V2 => "@opencode/cli",
        }
        .into(),
        native_update: Some(NativeUpdate {
            args: vec!["upgrade".into()],
            is_command_path: Some(is_opencode_native_command_path),
            env: None,
        }),
    }
}

/// cursor-agent is manual-only: no npm package, no updater T3 can prove.
pub fn cursor_manual_capabilities() -> ProviderMaintenanceCapabilities {
    make_manual_only_provider_maintenance_capabilities(
        ProviderDriverKind::new("cursor").expect("valid driver kind"),
    )
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn codex_standalone_installs_are_recognized_by_their_tree() {
        assert!(is_codex_standalone_command_path(
            "C:\\Users\\me\\.codex\\packages\\standalone\\current\\bin\\codex.exe"
        ));
        assert!(!is_codex_standalone_command_path(
            "/usr/local/lib/node_modules/@openai/codex/bin/codex.js"
        ));
    }

    #[test]
    fn claude_native_installs_are_recognized_by_their_launcher_or_data_dir() {
        assert!(is_claude_native_command_path("/Users/me/.local/bin/claude"));
        assert!(is_claude_native_command_path(
            "/Users/me/.local/share/claude/versions/2.1.0/claude"
        ));
        assert!(!is_claude_native_command_path("/usr/local/bin/claude"));
    }

    #[test]
    fn opencode_native_installs_are_recognized_by_their_launcher() {
        assert!(is_opencode_native_command_path(
            "/Users/me/.opencode/bin/opencode"
        ));
        assert!(!is_opencode_native_command_path("/usr/local/bin/opencode"));
    }

    #[test]
    fn codex_definition_pins_the_native_updater_to_the_shared_home() {
        let definition = codex_maintenance_definition("/Users/me/.codex");
        assert_eq!(definition.npm_package_name, "@openai/codex");
        let native = definition.native_update.unwrap();
        assert_eq!(native.args, ["update"]);
        assert_eq!(native.env.unwrap()["CODEX_HOME"], "/Users/me/.codex");
    }

    #[test]
    fn opencode_generations_update_only_their_own_package() {
        assert_eq!(
            opencode_maintenance_definition(OpenCodeGeneration::V1).npm_package_name,
            "opencode-ai"
        );
        assert_eq!(
            opencode_maintenance_definition(OpenCodeGeneration::V2).npm_package_name,
            "@opencode/cli"
        );
    }

    #[test]
    fn cursor_is_manual_only_without_a_package() {
        let capabilities = cursor_manual_capabilities();
        assert_eq!(capabilities.package_name, None);
        assert_eq!(capabilities.update, None);
    }
}
