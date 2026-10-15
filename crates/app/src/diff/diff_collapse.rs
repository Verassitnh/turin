//! Port of upstream/apps/web/src/lib/diffCollapse.ts (+ test).

use std::collections::HashSet;

pub fn are_all_diff_files_collapsed(file_keys: &[String], collapsed: &HashSet<String>) -> bool {
    !file_keys.is_empty() && file_keys.iter().all(|key| collapsed.contains(key))
}

pub fn toggle_all_diff_files(file_keys: &[String], collapsed: &HashSet<String>) -> HashSet<String> {
    if are_all_diff_files_collapsed(file_keys, collapsed) {
        HashSet::new()
    } else {
        file_keys.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<String> {
        vec!["src/app.ts".into(), "src/index.ts".into()]
    }

    #[test]
    fn reports_whether_every_rendered_file_is_collapsed() {
        let keys = keys();
        let all: HashSet<String> = keys.iter().cloned().collect();
        assert!(are_all_diff_files_collapsed(&keys, &all));
        assert!(!are_all_diff_files_collapsed(
            &keys,
            &HashSet::from([keys[0].clone()])
        ));
        assert!(!are_all_diff_files_collapsed(&[], &HashSet::new()));
    }

    #[test]
    fn collapses_all_files_when_any_rendered_file_is_expanded() {
        let keys = keys();
        let all: HashSet<String> = keys.iter().cloned().collect();
        assert_eq!(
            toggle_all_diff_files(&keys, &HashSet::from([keys[0].clone()])),
            all
        );
    }

    #[test]
    fn expands_all_files_when_every_rendered_file_is_collapsed() {
        let keys = keys();
        let all: HashSet<String> = keys.iter().cloned().collect();
        assert_eq!(toggle_all_diff_files(&keys, &all), HashSet::new());
    }
}
