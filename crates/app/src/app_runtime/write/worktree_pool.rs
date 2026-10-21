//! Worktree pools (Turin addition, PORTING.md Deviations): a project whose folder
//! is not a checkout itself but holds several (`~/code/app/{wt1,wt2,...}`). Each
//! new thread claims a checkout no live thread of the project works in; settling,
//! archiving or deleting the thread frees it again.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The checkouts directly inside `root`, in natural order (`wt2` before `wt10`).
/// Empty when `root` is a checkout itself or holds none: not a pool.
pub fn pool_members(root: &Path) -> Vec<PathBuf> {
    if root.join(".git").exists() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut members: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path.join(".git").exists()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    members.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    members
}

/// The first member no live thread holds.
pub fn first_free<'a>(members: &'a [PathBuf], held: &HashSet<String>) -> Option<&'a PathBuf> {
    members.iter().find(|member| {
        let path = member.to_string_lossy();
        !held.contains(path.trim_end_matches('/'))
    })
}

/// Digit runs compare by value, everything else by character.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(ch) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(ch);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order.is_ne() {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "turin-worktree-pool-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_child_checkouts_in_natural_order() {
        let root = scratch("members");
        for name in ["wt10", "wt2", "notes", ".hidden"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::write(root.join("wt10/.git"), "gitdir: x").unwrap();
        std::fs::create_dir_all(root.join("wt2/.git")).unwrap();
        std::fs::create_dir_all(root.join(".hidden/.git")).unwrap();
        let names: Vec<String> = pool_members(&root)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["wt2", "wt10"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_checkout_is_never_a_pool() {
        let root = scratch("checkout");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("sub/.git")).unwrap();
        assert!(pool_members(&root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn claims_the_first_member_nobody_holds() {
        let members = vec![PathBuf::from("/p/a1"), PathBuf::from("/p/a2")];
        let held: HashSet<String> = ["/p/a1".to_owned()].into();
        assert_eq!(first_free(&members, &held), Some(&members[1]));
        let all: HashSet<String> = ["/p/a1".to_owned(), "/p/a2".to_owned()].into();
        assert_eq!(first_free(&members, &all), None);
    }
}
