//! Presentation rules of ComposerStashMenu.tsx (`stashEntrySnippet`) and
//! `formatRelativeTimeLabel` (timestampFormat.ts).
//!
//! Port of upstream/apps/web/src/components/chat/ComposerStashMenu.tsx (presentation rules).

use chrono::{DateTime, Local};

use crate::components::chat::timestamp_format::parse_timestamp_date;
use crate::composer::draft_store::PromptStashEntry;

const SNIPPET_MAX_CHARS: usize = 90;

/// One line summarising a stashed prompt: its collapsed text, or a count of
/// what it holds when the text is empty.
pub fn stash_entry_snippet(entry: &PromptStashEntry) -> String {
    let collapsed = entry
        .prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !collapsed.is_empty() {
        // JS `slice` counts UTF-16 units; characters are a close, safe stand-in.
        return if collapsed.chars().count() > SNIPPET_MAX_CHARS {
            format!(
                "{}…",
                collapsed
                    .chars()
                    .take(SNIPPET_MAX_CHARS)
                    .collect::<String>()
            )
        } else {
            collapsed
        };
    }
    let image_count = entry.images.len() + entry.dropped_image_names.len();
    let file_count = entry.files.len();
    let attachment_count = image_count + file_count;
    if attachment_count == 0 {
        return "(empty)".to_owned();
    }
    let label = if image_count > 0 && file_count > 0 {
        "attachment"
    } else if file_count > 0 {
        "file"
    } else {
        "image"
    };
    format!(
        "({attachment_count} {label}{})",
        if attachment_count == 1 { "" } else { "s" }
    )
}

/// Images that did not make it into the entry, whatever the reason.
pub fn missing_image_count(entry: &PromptStashEntry) -> usize {
    entry.dropped_image_names.len() + entry.unreadable_image_names.len()
}

/// `formatRelativeTimeLabel` against an explicit clock.
pub fn relative_time_label_at(iso: &str, now: DateTime<Local>) -> String {
    let Some(date) = parse_timestamp_date(iso) else {
        return String::new();
    };
    let diff = now.signed_duration_since(date);
    if diff.num_milliseconds() < 0 {
        return "just now".to_owned();
    }
    let seconds = diff.num_seconds();
    if seconds < 60 {
        return "just now".to_owned();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

pub fn relative_time_label(iso: &str) -> String {
    relative_time_label_at(iso, Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};

    fn entry(prompt: &str) -> PromptStashEntry {
        PromptStashEntry {
            id: "s".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            prompt: prompt.into(),
            images: Vec::new(),
            files: Vec::new(),
            dropped_image_names: Vec::new(),
            unreadable_image_names: Vec::new(),
            pending_image_count: 0,
        }
    }

    #[test]
    fn collapses_whitespace_and_truncates_long_prompts() {
        assert_eq!(
            stash_entry_snippet(&entry("  fix   the\n bug ")),
            "fix the bug"
        );
        let long = "x".repeat(120);
        let snippet = stash_entry_snippet(&entry(&long));
        assert_eq!(snippet.chars().count(), 91);
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn counts_attachments_when_the_prompt_is_empty() {
        assert_eq!(stash_entry_snippet(&entry("   ")), "(empty)");
        let mut with_image = entry("");
        with_image.images.push(ComposerImageAttachment::new(
            "i",
            "a.png",
            "image/png",
            vec![1],
        ));
        assert_eq!(stash_entry_snippet(&with_image), "(1 image)");
        let mut mixed = with_image.clone();
        mixed.files.push(ComposerFileAttachment {
            id: "f".into(),
            name: "a.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 1,
            data: None,
            uploaded_attachment_id: Some("u".into()),
            upload_environment_id: None,
        });
        assert_eq!(stash_entry_snippet(&mixed), "(2 attachments)");
        let mut files_only = entry("");
        files_only.files = mixed.files.clone();
        assert_eq!(stash_entry_snippet(&files_only), "(1 file)");
    }

    #[test]
    fn dropped_images_count_toward_the_summary_and_the_warning() {
        let mut dropped = entry("");
        dropped.dropped_image_names = vec!["a.png".into(), "b.png".into()];
        dropped.unreadable_image_names = vec!["c.png".into()];
        assert_eq!(stash_entry_snippet(&dropped), "(2 images)");
        assert_eq!(missing_image_count(&dropped), 3);
    }

    #[test]
    fn relative_labels_step_through_minutes_hours_and_days() {
        let now = parse_timestamp_date("2026-01-10T12:00:00.000Z").unwrap();
        assert_eq!(
            relative_time_label_at("2026-01-10T11:59:30.000Z", now),
            "just now"
        );
        assert_eq!(
            relative_time_label_at("2026-01-10T11:55:00.000Z", now),
            "5m ago"
        );
        assert_eq!(
            relative_time_label_at("2026-01-10T09:00:00.000Z", now),
            "3h ago"
        );
        assert_eq!(
            relative_time_label_at("2026-01-07T12:00:00.000Z", now),
            "3d ago"
        );
        assert_eq!(
            relative_time_label_at("2026-01-11T00:00:00.000Z", now),
            "just now"
        );
        assert_eq!(relative_time_label_at("not a date", now), "");
    }
}
