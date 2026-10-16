//! The part of upstream/apps/web/src/components/pullRequest/guide/GuideExcerpt.tsx (and
//! `useGuideDiffFiles.ts`'s file read) that opens the unchanged lines around an excerpt: the
//! `loadDiffFiles` an excerpt hands the diff renderer, whose bars above and below the lines
//! expand into the file.
//!
//! The renderer's expansion is the plan computed here: from the excerpt's rows and the file's new
//! contents, which lines lie above and below, what they are called on each side, and how many are
//! still hidden. The old side is [`excerpt_expansion_old_contents`](crate::state::helpers::review_guide_tour::excerpt_expansion_old_contents)'s:
//! the new file with only the excerpt's own lines put back, so a slice across hunks has nothing
//! consistent to expand into and [`plan_excerpt_expansion`] gives `None`.

use crate::state::helpers::review_guide_tour::{DiffRow, DiffRowKind};

/// How many lines one press of a bar opens (`@pierre/diffs`' `expansionLineCount`).
pub const EXPANSION_LINES: usize = 100;

/// How far the reader has opened each side of one excerpt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expansion {
    pub up: usize,
    pub down: usize,
}

impl Expansion {
    /// One press of the bar above, which opens at most what is still hidden.
    pub fn open_up(self, hidden: usize) -> Self {
        Self {
            up: self.up + hidden.min(EXPANSION_LINES),
            ..self
        }
    }

    pub fn open_down(self, hidden: usize) -> Self {
        Self {
            down: self.down + hidden.min(EXPANSION_LINES),
            ..self
        }
    }
}

/// What an excerpt draws around its own rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionPlan {
    /// Unchanged lines opened above the excerpt, in file order.
    pub above: Vec<DiffRow>,
    /// Unchanged lines opened below it.
    pub below: Vec<DiffRow>,
    /// Lines still behind the bar above.
    pub hidden_above: usize,
    /// Lines still behind the bar below.
    pub hidden_below: usize,
}

fn context_row(new_line: i64, old_delta: i64, text: &str, hunk: usize) -> DiffRow {
    DiffRow {
        kind: DiffRowKind::Context,
        old_line: Some(new_line + old_delta),
        new_line: Some(new_line),
        old_at: new_line + old_delta + 1,
        new_at: new_line + 1,
        text: text.trim_end_matches('\n').to_owned(),
        hunk,
    }
}

/// Lines above the excerpt are always countable from the diff alone (the lines before its first
/// row); `new_contents` is only needed to show them. `None` when the excerpt spans hunks.
pub fn hidden_above(shown: &[DiffRow], expansion: Expansion) -> Option<usize> {
    let head = shown.first()?;
    if shown.iter().any(|row| row.hunk != head.hunk) {
        return None;
    }
    let before = (head.new_at - 1).max(0) as usize;
    Some(before.saturating_sub(expansion.up))
}

/// The lines the excerpt's bars have opened, and what is left behind them.
pub fn plan_excerpt_expansion(
    shown: &[DiffRow],
    new_contents: &str,
    expansion: Expansion,
) -> Option<ExpansionPlan> {
    let head = shown.first()?;
    let last = shown.last()?;
    if shown.iter().any(|row| row.hunk != head.hunk) {
        return None;
    }
    let new_lines: Vec<&str> = new_contents.split_inclusive('\n').collect();
    let hunk = head.hunk;

    // Above: the file's lines `before - up + 1 ..= before`, numbered on the old side by the
    // offset the first row has between its two sides.
    let before = (head.new_at - 1).max(0) as usize;
    let up = expansion.up.min(before);
    let head_delta = head.old_at - head.new_at;
    let above: Vec<DiffRow> = ((before - up + 1)..=before)
        .filter_map(|line| {
            new_lines
                .get(line - 1)
                .map(|text| context_row(line as i64, head_delta, text, hunk))
        })
        .collect();

    // Below: from the line after the excerpt's last new-side line.
    let next_new = last.new_at + i64::from(last.new_line.is_some());
    let next_old = last.old_at + i64::from(last.old_line.is_some());
    let tail_delta = next_old - next_new;
    let start = (next_new - 1).max(0) as usize;
    let remaining = new_lines.len().saturating_sub(start);
    let down = expansion.down.min(remaining);
    let below: Vec<DiffRow> = (start..start + down)
        .map(|index| context_row(index as i64 + 1, tail_delta, new_lines[index], hunk))
        .collect();

    Some(ExpansionPlan {
        above,
        below,
        hidden_above: before - up,
        hidden_below: remaining - down,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn row(
        kind: DiffRowKind,
        old: Option<i64>,
        new: Option<i64>,
        at: (i64, i64),
        text: &str,
    ) -> DiffRow {
        DiffRow {
            kind,
            old_line: old,
            new_line: new,
            old_at: at.0,
            new_at: at.1,
            text: text.to_owned(),
            hunk: 0,
        }
    }

    /// Lines 5..=6 of the new file, replacing old line 5: a hunk that starts on line 5.
    fn shown() -> Vec<DiffRow> {
        vec![
            row(DiffRowKind::Deletion, Some(5), None, (5, 5), "old five"),
            row(DiffRowKind::Addition, None, Some(5), (6, 5), "new five"),
            row(DiffRowKind::Context, Some(6), Some(6), (6, 6), "six"),
        ]
    }

    fn file() -> String {
        (1..=10)
            .map(|line| match line {
                5 => "new five\n".to_owned(),
                _ => format!("line {line}\n"),
            })
            .collect()
    }

    #[test]
    fn nothing_is_open_until_a_bar_is_pressed() {
        let plan = plan_excerpt_expansion(&shown(), &file(), Expansion::default()).unwrap();
        assert!(plan.above.is_empty() && plan.below.is_empty());
        // Four lines lie above the excerpt, and four below its last line.
        assert_eq!(plan.hidden_above, 4);
        assert_eq!(plan.hidden_below, 4);
        assert_eq!(hidden_above(&shown(), Expansion::default()), Some(4));
    }

    #[test]
    fn opening_above_shows_the_lines_before_with_both_numberings() {
        let plan = plan_excerpt_expansion(&shown(), &file(), Expansion { up: 2, down: 0 }).unwrap();
        let lines: Vec<(Option<i64>, Option<i64>, &str)> = plan
            .above
            .iter()
            .map(|row| (row.old_line, row.new_line, row.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![(Some(3), Some(3), "line 3"), (Some(4), Some(4), "line 4")]
        );
        assert_eq!(plan.hidden_above, 2);
    }

    #[test]
    fn opening_below_continues_from_the_excerpts_last_line() {
        let plan = plan_excerpt_expansion(&shown(), &file(), Expansion { up: 0, down: 3 }).unwrap();
        let numbers: Vec<(i64, i64)> = plan
            .below
            .iter()
            .map(|row| (row.old_line.unwrap(), row.new_line.unwrap()))
            .collect();
        assert_eq!(numbers, vec![(7, 7), (8, 8), (9, 9)]);
        assert_eq!(plan.below[0].text, "line 7");
        assert_eq!(plan.hidden_below, 1);
    }

    #[test]
    fn a_bar_opens_at_most_what_is_hidden_and_at_most_one_page() {
        assert_eq!(Expansion::default().open_up(4).up, 4);
        assert_eq!(Expansion::default().open_down(1_000).down, EXPANSION_LINES);
        assert_eq!(Expansion { up: 100, down: 7 }.open_up(30).up, 130);
    }

    #[test]
    fn opening_past_the_file_stops_at_its_ends() {
        let plan =
            plan_excerpt_expansion(&shown(), &file(), Expansion { up: 50, down: 50 }).unwrap();
        assert_eq!(plan.above.len(), 4);
        assert_eq!(plan.below.len(), 4);
        assert_eq!((plan.hidden_above, plan.hidden_below), (0, 0));
    }

    #[test]
    fn an_old_side_that_drifted_keeps_its_own_numbers_below() {
        // Another hunk above removed two lines, so old numbers run two ahead of new ones.
        let rows = vec![row(DiffRowKind::Context, Some(8), Some(6), (8, 6), "six")];
        let plan = plan_excerpt_expansion(&rows, &file(), Expansion { up: 1, down: 1 }).unwrap();
        assert_eq!(plan.above[0].old_line, Some(7));
        assert_eq!(plan.above[0].new_line, Some(5));
        assert_eq!(plan.below[0].old_line, Some(9));
        assert_eq!(plan.below[0].new_line, Some(7));
    }

    #[test]
    fn a_slice_across_hunks_has_nothing_to_expand_into() {
        let mut rows = shown();
        rows[2].hunk = 1;
        assert!(plan_excerpt_expansion(&rows, &file(), Expansion::default()).is_none());
        assert_eq!(hidden_above(&rows, Expansion::default()), None);
        assert!(plan_excerpt_expansion(&[], &file(), Expansion::default()).is_none());
    }
}
