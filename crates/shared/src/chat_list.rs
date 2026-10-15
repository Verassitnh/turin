//! Port of upstream/packages/shared/src/chatList.ts

pub const CHAT_LIST_ANCHOR_OFFSET: i64 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatListAnchoredEndSpace {
    pub anchor_index: usize,
    pub anchor_offset: i64,
}

pub struct ChatListAnchorOptions {
    pub anchor_offset: Option<i64>,
}

impl Default for ChatListAnchorOptions {
    fn default() -> Self {
        Self { anchor_offset: None }
    }
}

pub fn resolve_chat_list_anchored_end_space<Item, AnchorId>(
    items: &[Item],
    anchor_id: Option<&AnchorId>,
    get_anchor_id: impl Fn(&Item) -> Option<&AnchorId>,
    options: ChatListAnchorOptions,
) -> Option<ChatListAnchoredEndSpace>
where
    AnchorId: PartialEq,
{
    let anchor_id = anchor_id?;

    for (index, item) in items.iter().enumerate() {
        let Some(item_anchor_id) = get_anchor_id(item) else {
            continue;
        };

        if item_anchor_id == anchor_id {
            return Some(ChatListAnchoredEndSpace {
                anchor_index: index,
                anchor_offset: options.anchor_offset.unwrap_or(CHAT_LIST_ANCHOR_OFFSET),
            });
        }
        return None;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Row {
        id: &'static str,
        anchorable: bool,
    }

    const ROWS: [Row; 3] = [
        Row { id: "first", anchorable: true },
        Row { id: "ignored", anchorable: false },
        Row { id: "latest", anchorable: true },
    ];

    fn get_anchor_id(row: &Row) -> Option<&'static str> {
        row.anchorable.then_some(row.id)
    }

    #[test]
    fn anchors_only_the_first_eligible_row() {
        assert_eq!(
            resolve_chat_list_anchored_end_space(&ROWS, Some("first"), get_anchor_id, Default::default()),
            Some(ChatListAnchoredEndSpace { anchor_index: 0, anchor_offset: CHAT_LIST_ANCHOR_OFFSET })
        );
    }

    #[test]
    fn allows_a_surface_to_keep_the_anchor_below_its_own_header() {
        assert_eq!(
            resolve_chat_list_anchored_end_space(
                &ROWS,
                Some("first"),
                get_anchor_id,
                ChatListAnchorOptions { anchor_offset: Some(132) }
            ),
            Some(ChatListAnchoredEndSpace { anchor_index: 0, anchor_offset: 132 })
        );
    }

    #[test]
    fn does_not_reserve_end_space_for_later_eligible_rows() {
        assert_eq!(
            resolve_chat_list_anchored_end_space(&ROWS, Some("latest"), get_anchor_id, Default::default()),
            None
        );
    }

    #[test]
    fn skips_ineligible_rows_before_the_first_anchor() {
        assert_eq!(
            resolve_chat_list_anchored_end_space(&ROWS[1..], Some("latest"), get_anchor_id, Default::default()),
            Some(ChatListAnchoredEndSpace { anchor_index: 1, anchor_offset: CHAT_LIST_ANCHOR_OFFSET })
        );
    }

    #[test]
    fn ignores_ineligible_rows_and_missing_anchors() {
        assert_eq!(
            resolve_chat_list_anchored_end_space(&ROWS, Some("ignored"), get_anchor_id, Default::default()),
            None
        );
        assert_eq!(
            resolve_chat_list_anchored_end_space(&ROWS, Some("missing"), get_anchor_id, Default::default()),
            None
        );
        assert_eq!(
            resolve_chat_list_anchored_end_space(&ROWS, None, get_anchor_id, Default::default()),
            None
        );
    }
}
