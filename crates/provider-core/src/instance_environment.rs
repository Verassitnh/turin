//! Port of upstream/packages/provider-core/src/server/instanceEnvironment.ts.

use std::collections::HashMap;

use turin_contracts::provider_instance::ProviderInstanceEnvironment;

use crate::path_expansion::expand_home_path;

/// Apply instance overrides without changing the inherited environment.
/// Only newly configured provider homes undergo shell-style tilde expansion.
pub fn merge_provider_instance_environment(
    environment: Option<&ProviderInstanceEnvironment>,
    base_env: &HashMap<String, String>,
    home: &str,
) -> HashMap<String, String> {
    let Some(environment) = environment else {
        return base_env.clone();
    };
    let mut next = base_env.clone();
    for variable in environment {
        let name = variable.name.as_str();
        let value = if matches!(name, "CODEX_HOME" | "CLAUDE_CONFIG_DIR") {
            expand_home_path(&variable.value, home)
        } else {
            variable.value.clone()
        };
        next.insert(name.to_owned(), value);
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::provider_instance::{
        ProviderInstanceEnvironmentVariable, ProviderInstanceEnvironmentVariableName,
    };

    fn vars(pairs: &[(&str, &str)]) -> ProviderInstanceEnvironment {
        pairs
            .iter()
            .map(|(name, value)| ProviderInstanceEnvironmentVariable {
                name: ProviderInstanceEnvironmentVariableName::new(name).unwrap(),
                value: (*value).into(),
                sensitive: false,
                value_redacted: None,
            })
            .collect()
    }

    #[test]
    fn configured_provider_homes_expand_and_inherited_values_do_not() {
        for value in ["~/.account", "~\\.account\\work"] {
            let base = HashMap::from([
                ("CODEX_HOME".into(), "~/.inherited-codex".into()),
                ("CLAUDE_CONFIG_DIR".into(), "~/.inherited-claude".into()),
            ]);
            let result = merge_provider_instance_environment(
                Some(&vars(&[
                    ("CODEX_HOME", value),
                    ("CLAUDE_CONFIG_DIR", value),
                    ("CUSTOM_VALUE", value),
                ])),
                &base,
                "/home/ada",
            );
            assert!(result["CODEX_HOME"].starts_with("/home/ada/"));
            assert_eq!(result["CODEX_HOME"], result["CLAUDE_CONFIG_DIR"]);
            assert_eq!(result["CUSTOM_VALUE"], value);
            assert_eq!(base["CODEX_HOME"], "~/.inherited-codex");
        }
        let inherited = HashMap::from([("CODEX_HOME".into(), "~/.codex".into())]);
        let result = merge_provider_instance_environment(
            Some(&vars(&[("CUSTOM_VALUE", "~/.custom")])),
            &inherited,
            "/home/ada",
        );
        assert_eq!(result["CODEX_HOME"], "~/.codex");
        assert_eq!(result["CUSTOM_VALUE"], "~/.custom");
    }

    #[test]
    fn preserves_empty_overrides() {
        let base = HashMap::from([("ANTHROPIC_API_KEY".into(), "inherited".into())]);
        let result = merge_provider_instance_environment(
            Some(&vars(&[("ANTHROPIC_API_KEY", "")])),
            &base,
            "/home/ada",
        );
        assert_eq!(result["ANTHROPIC_API_KEY"], "");
    }
}
