//! Port of upstream/apps/server/src/project/gitCloneProgress.ts
//!
//! Parses one line of `git clone --progress` stderr into the phase and percentage a clone
//! progress bar shows. The stage enum is upstream's `ProjectCloneStage` from
//! `packages/contracts/src/projectClone.ts`, which the Rust contracts crate does not port yet, so
//! it lives here until it does.

use std::sync::LazyLock;

use regex::Regex;

/// Follows git's own clone phases as they appear on stderr, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneStage {
    Connecting,
    Counting,
    Receiving,
    Resolving,
    Checkout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitCloneProgressLine {
    pub stage: CloneStage,
    pub percent: Option<f64>,
    /// Transfer detail after the count, e.g. `12.30 MiB | 5.00 MiB/s`.
    pub detail: Option<String>,
}

static STAGE_PREFIXES: LazyLock<Vec<(Regex, CloneStage)>> = LazyLock::new(|| {
    [
        (r"^remote: Enumerating objects", CloneStage::Counting),
        (r"^remote: Counting objects", CloneStage::Counting),
        (r"^remote: Compressing objects", CloneStage::Counting),
        (r"^Receiving objects", CloneStage::Receiving),
        (r"^Resolving deltas", CloneStage::Resolving),
        (r"^Updating files", CloneStage::Checkout),
        (r"^Checking out files", CloneStage::Checkout),
    ]
    .into_iter()
    .map(|(pattern, stage)| (Regex::new(pattern).unwrap(), stage))
    .collect()
});

static PERCENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r":\s+([0-9]+)%\s+\(([0-9]+)/([0-9]+)\)(?:,\s*(.*?))?\s*(?:,\s*done\.)?\s*$")
        .unwrap()
});

/// Parses one line of `git clone --progress` stderr. Git redraws each counter with a bare `\r`,
/// so callers hand over each redraw as its own line. Lines that are not progress counters (hints,
/// warnings, `Cloning into ...`) return `None` and are left for the error surface.
pub fn parse_git_clone_progress_line(line: &str) -> Option<GitCloneProgressLine> {
    let trimmed = line.trim();
    let stage = STAGE_PREFIXES
        .iter()
        .find(|(pattern, _)| pattern.is_match(trimmed))?
        .1;
    let Some(captures) = PERCENT.captures(trimmed) else {
        return Some(GitCloneProgressLine {
            stage,
            percent: None,
            detail: None,
        });
    };
    let percent: f64 = captures[1].parse().unwrap_or(f64::NAN);
    let raw_detail = captures.get(4).map_or("", |m| m.as_str()).trim();
    // The trailer of a finished line is "done." which carries no information.
    let detail = (!raw_detail.is_empty() && raw_detail != "done.").then(|| raw_detail.to_owned());
    Some(GitCloneProgressLine {
        stage,
        percent: percent.is_finite().then(|| percent.clamp(0.0, 100.0)),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_receiving_counter_with_its_transfer_detail() {
        let line = "Receiving objects:  42% (420/1000), 12.30 MiB | 5.00 MiB/s";
        assert_eq!(
            parse_git_clone_progress_line(line),
            Some(GitCloneProgressLine {
                stage: CloneStage::Receiving,
                percent: Some(42.0),
                detail: Some("12.30 MiB | 5.00 MiB/s".to_owned()),
            })
        );
    }

    #[test]
    fn drops_the_done_trailer_and_clamps_the_percentage() {
        let line = "Checking out files: 100% (5/5), done.";
        assert_eq!(
            parse_git_clone_progress_line(line),
            Some(GitCloneProgressLine {
                stage: CloneStage::Checkout,
                percent: Some(100.0),
                detail: None,
            })
        );
    }

    #[test]
    fn keeps_a_stage_without_a_counter() {
        assert_eq!(
            parse_git_clone_progress_line("remote: Counting objects: "),
            Some(GitCloneProgressLine {
                stage: CloneStage::Counting,
                percent: None,
                detail: None,
            })
        );
    }

    #[test]
    fn leaves_everything_else_for_the_error_surface() {
        assert_eq!(
            parse_git_clone_progress_line("Cloning into 'repo'..."),
            None
        );
        assert_eq!(
            parse_git_clone_progress_line("fatal: repository not found"),
            None
        );
    }
}
