//! Port of upstream/apps/web/src/lib/orchestrationV2Timeline.ts
//! (and orchestrationV2Timeline.test.ts).
//!
//! Row lists keyed by `source_item_id` that keep `position` dense and stable
//! while a streamed item is removed or replaced.

/// The two fields upstream's `Row extends { position, sourceItemId }` reads.
pub trait PositionedRow: Clone {
    type Id: PartialEq;

    fn position(&self) -> usize;
    fn set_position(&mut self, position: usize);
    fn source_item_id(&self) -> &Self::Id;
}

/// Drops `source_item_id` and renumbers the rest from zero, so the position gap
/// closes before a streamed item is reinserted.
pub fn remove_and_renumber_timeline_item<Row: PositionedRow>(
    rows: &[Row],
    source_item_id: &Row::Id,
) -> Vec<Row> {
    rows.iter()
        .filter(|row| row.source_item_id() != source_item_id)
        .enumerate()
        .map(|(position, row)| {
            let mut row = row.clone();
            row.set_position(position);
            row
        })
        .collect()
}

/// Replaces the row with the same `source_item_id` in place, keeping its
/// position, so a streaming item does not move behind newer ones. A new item
/// is appended at the next position.
pub fn upsert_timeline_item_at_stable_position<Row: PositionedRow>(
    rows: &[Row],
    item: &Row,
) -> Vec<Row> {
    let existing_index = rows
        .iter()
        .position(|row| row.source_item_id() == item.source_item_id());
    let Some(existing_index) = existing_index else {
        let mut appended = rows.to_vec();
        let mut item = item.clone();
        item.set_position(rows.len());
        appended.push(item);
        return appended;
    };
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            if index == existing_index {
                let mut replacement = item.clone();
                replacement.set_position(row.position());
                replacement
            } else {
                row.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        position: usize,
        source_item_id: &'static str,
        text: &'static str,
    }

    impl PositionedRow for Row {
        type Id = &'static str;

        fn position(&self) -> usize {
            self.position
        }
        fn set_position(&mut self, position: usize) {
            self.position = position;
        }
        fn source_item_id(&self) -> &&'static str {
            &self.source_item_id
        }
    }

    fn row(position: usize, source_item_id: &'static str, text: &'static str) -> Row {
        Row {
            position,
            source_item_id,
            text,
        }
    }

    #[test]
    fn remove_closes_the_position_gap_before_a_streamed_item_is_reinserted() {
        let remaining = remove_and_renumber_timeline_item(
            &[
                row(0, "first", ""),
                row(1, "streaming", ""),
                row(2, "last", ""),
            ],
            &"streaming",
        );
        assert_eq!(remaining, vec![row(0, "first", ""), row(1, "last", "")]);
        let mut positions: Vec<usize> = remaining.iter().map(|row| row.position).collect();
        positions.dedup();
        assert_eq!(positions.len(), remaining.len());
    }

    #[test]
    fn upsert_replaces_a_streaming_item_without_moving_it_behind_newer_items() {
        let updated = upsert_timeline_item_at_stable_position(
            &[
                row(0, "reasoning", "partial"),
                row(1, "subagent", "running"),
            ],
            &row(2, "reasoning", "completed"),
        );
        assert_eq!(
            updated,
            vec![
                row(0, "reasoning", "completed"),
                row(1, "subagent", "running")
            ]
        );
    }

    #[test]
    fn upsert_appends_a_genuinely_new_item_at_the_next_position() {
        let updated = upsert_timeline_item_at_stable_position(
            &[row(0, "reasoning", "")],
            &row(99, "assistant", ""),
        );
        assert_eq!(
            updated,
            vec![row(0, "reasoning", ""), row(1, "assistant", "")]
        );
    }
}
