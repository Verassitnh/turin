//! `nextProjectScriptId` and the script helpers of projectScriptEditor.ts used
//! by the Projects page's Actions list.

/// `nextProjectScriptId`: a slug of the name, unique among `existing`.
pub fn next_project_script_id(name: &str, existing: &[String]) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    let base = slug.trim_matches('-');
    let base = if base.is_empty() { "script" } else { base };
    if !existing.iter().any(|id| id == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !existing.contains(candidate))
        .expect("an unused suffix exists")
}

/// `commandForProjectScript`.
pub fn command_for_project_script(script_id: &str) -> String {
    format!("script.{script_id}.run")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_the_name_and_avoids_collisions() {
        assert_eq!(next_project_script_id("Run Tests!", &[]), "run-tests");
        assert_eq!(
            next_project_script_id("Run Tests", &["run-tests".into(), "run-tests-2".into()]),
            "run-tests-3"
        );
        assert_eq!(next_project_script_id("???", &[]), "script");
        assert_eq!(command_for_project_script("lint"), "script.lint.run");
    }
}
