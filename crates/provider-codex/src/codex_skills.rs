//! Port of `parseCodexSkillsListResponse` in upstream/apps/server/src/provider/CodexProvider.ts.
use serde_json::Value;
use turin_contracts::TrimmedNonEmptyString;
use turin_provider_core::server_provider::ServerProviderSkill;

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Skills for `cwd`: the entry whose `cwd` matches, or every entry's skills
/// when none matches. Entries without a usable `name`, `path`, or `enabled`
/// are skipped, where upstream's schema would reject the whole response.
pub fn parse_codex_skills_list_response(response: &Value, cwd: &str) -> Vec<ServerProviderSkill> {
    let entries: Vec<&Value> = response
        .get("data")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().collect())
        .unwrap_or_default();
    let matching = entries
        .iter()
        .find(|entry| str_field(entry, "cwd") == Some(cwd));
    let skills: Vec<&Value> = match matching {
        Some(entry) => skills_of(entry).collect(),
        None => entries.iter().copied().flat_map(skills_of).collect(),
    };
    skills.into_iter().filter_map(parse_skill).collect()
}

fn skills_of(entry: &Value) -> impl Iterator<Item = &Value> + '_ {
    entry
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn parse_skill(skill: &Value) -> Option<ServerProviderSkill> {
    let interface = skill.get("interface");
    let short_description = str_field(skill, "shortDescription")
        .or_else(|| interface.and_then(|interface| str_field(interface, "shortDescription")));
    Some(ServerProviderSkill {
        name: tne(str_field(skill, "name")?)?,
        description: str_field(skill, "description").and_then(tne),
        path: tne(str_field(skill, "path")?)?,
        scope: str_field(skill, "scope").and_then(tne),
        enabled: skill.get("enabled")?.as_bool()?,
        display_name: interface
            .and_then(|interface| str_field(interface, "displayName"))
            .and_then(tne),
        short_description: short_description.and_then(tne),
        user_invocation_only: None,
        user_invocable: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matching_cwd_wins_over_other_entries() {
        let response = json!({"data": [
            {"cwd": "/other", "skills": [{"name": "a", "path": "/a", "enabled": true}]},
            {"cwd": "/work", "skills": [{"name": "b", "path": "/b", "enabled": false}]}
        ]});
        let skills = parse_codex_skills_list_response(&response, "/work");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name.as_str(), "b");
        assert!(!skills[0].enabled);
    }

    #[test]
    fn unmatched_cwd_flattens_every_entry() {
        let response = json!({"data": [
            {"cwd": "/other", "skills": [{"name": "a", "path": "/a", "enabled": true}]},
            {"cwd": "/more", "skills": [{"name": "b", "path": "/b", "enabled": true}]}
        ]});
        let names: Vec<String> = parse_codex_skills_list_response(&response, "/work")
            .into_iter()
            .map(|skill| skill.name.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn interface_fields_fill_display_and_short_description() {
        let response = json!({"data": [{"cwd": "/work", "skills": [{
            "name": "review",
            "path": "/skills/review",
            "enabled": true,
            "scope": "user",
            "description": "Long copy",
            "shortDescription": "Top-level short",
            "interface": {"displayName": "Review", "shortDescription": "Nested short"}
        }]}]});
        let skill = &parse_codex_skills_list_response(&response, "/work")[0];
        assert_eq!(
            skill.display_name.as_ref().map(|v| v.as_str()),
            Some("Review")
        );
        assert_eq!(
            skill.short_description.as_ref().map(|v| v.as_str()),
            Some("Top-level short")
        );
        assert_eq!(skill.scope.as_ref().map(|v| v.as_str()), Some("user"));
    }

    #[test]
    fn skills_missing_required_fields_are_skipped() {
        let response = json!({"data": [{"cwd": "/work", "skills": [
            {"name": "no-enabled", "path": "/x"},
            {"name": "ok", "path": "/y", "enabled": true}
        ]}]});
        let skills = parse_codex_skills_list_response(&response, "/work");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name.as_str(), "ok");
    }
}
