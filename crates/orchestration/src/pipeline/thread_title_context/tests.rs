//! Ports of upstream provider-core `threadTitleContext.test.ts` and the
//! `formatThreadTitleContext` block of `ThreadTitleRegenerationService.test.ts`.

use super::*;
use ThreadTitleRole::{Assistant, Reasoning, System, User};

fn msg(role: ThreadTitleRole, text: &str) -> ThreadTitleMessage<'_> {
    ThreadTitleMessage {
        role,
        text,
        attachments: &[],
    }
}

fn with<'a>(
    role: ThreadTitleRole,
    text: &'a str,
    attachments: &'a [ChatAttachment],
) -> ThreadTitleMessage<'a> {
    ThreadTitleMessage {
        role,
        text,
        attachments,
    }
}

fn attachment(id: &str) -> ChatAttachment {
    serde_json::from_value(json!({
        "type": "image", "id": id, "name": format!("{id}.png"),
        "mimeType": "image/png", "sizeBytes": 1,
    }))
    .unwrap()
}

fn names(context: &ThreadTitleContext) -> Vec<String> {
    context
        .attachments
        .iter()
        .map(|attachment| attachment_fields(attachment).name.to_owned())
        .collect()
}

#[test]
fn keeps_a_users_scope_change_despite_long_assistant_output() {
    let old = "Old findings. ".repeat(2_000);
    let details = "Implementation details. ".repeat(2_000);
    let result = format_thread_title_context(&[
        msg(User, "Review QR sharing"),
        msg(Assistant, &old),
        msg(
            User,
            "Focus on pairing expiry instead. Keep remote access working.",
        ),
        msg(Assistant, &details),
        msg(User, "Merge it when green."),
    ]);
    assert!(js_len(&result.message) <= 8_000);
    assert!(result.message.contains("USER:\nReview QR sharing"));
    assert!(
        result
            .message
            .contains("USER:\nFocus on pairing expiry instead. Keep remote access working.")
    );
    assert!(result.message.contains("USER:\nMerge it when green."));
    assert!(
        result
            .message
            .contains("ASSISTANT:\nImplementation details.")
    );
}

#[test]
fn retains_both_ends_and_role_labels_in_long_user_messages() {
    let long = format!(
        "Fix Android pairing. {}Keep iOS behavior.",
        "logs ".repeat(3_000)
    );
    let result = format_thread_title_context(&[
        msg(System, "System instructions"),
        msg(User, &long),
        msg(Assistant, "Found the cause."),
    ]);
    assert!(result.message.contains("USER:\nFix Android pairing."));
    assert!(result.message.contains("Keep iOS behavior."));
    assert!(result.message.contains("ASSISTANT:\nFound the cause."));
    assert!(!result.message.contains("System instructions"));
    assert_eq!(result.message.matches("USER:").count(), 1);
}

#[test]
fn preserves_short_conversations_unchanged_and_handles_tiny_budgets() {
    assert_eq!(
        format_thread_title_context(&[
            msg(User, "Fix pairing"),
            msg(Assistant, "The QR token expired."),
        ])
        .message,
        "USER:\nFix pairing\n\nASSISTANT:\nThe QR token expired."
    );
    let hundred = "x".repeat(100);
    assert_eq!(limit_title_message(&hundred, 0), "");
    for budget in 1..40 {
        assert!(len(&limit_title_message(&hundred, budget)) <= budget);
    }
    assert_eq!(
        format_thread_title_context(&[]),
        ThreadTitleContext {
            message: String::new(),
            attachments: vec![],
        }
    );
}

#[test]
fn omits_reasoning_traces_from_generated_titles() {
    assert_eq!(
        format_thread_title_context(&[
            msg(User, "Fix pairing"),
            msg(Reasoning, "Consider token expiry, then the QR payload."),
            msg(Assistant, "The QR token expired."),
        ])
        .message,
        "USER:\nFix pairing\n\nASSISTANT:\nThe QR token expired."
    );
}

#[test]
fn builds_a_newest_first_digest_skipping_system_messages_and_empty_sections() {
    let shot = [attachment("shot")];
    let context = format_thread_title_context(&[
        msg(User, "First question"),
        msg(System, "Hidden instructions"),
        msg(Assistant, ""),
        with(Assistant, "Second answer", &shot),
    ]);
    assert_eq!(
        context.message,
        "USER:\nFirst question\n\nASSISTANT:\nSecond answer\n[Attachments: shot.png]"
    );
    assert_eq!(names(&context), ["shot.png"]);
}

#[test]
fn pins_the_first_user_message_ahead_of_the_retained_tail_once_content_stops_fitting() {
    let first = format!("Ancient context that anchors the topic {}", "x".repeat(600));
    let assistant = "y".repeat(6_000);
    let last = "z".repeat(1_500);
    let context = format_thread_title_context(&[
        msg(User, &first),
        msg(Assistant, &assistant),
        msg(User, &last),
    ]);
    assert!(
        context
            .message
            .contains("USER:\nAncient context that anchors the topic")
    );
    assert!(context.message.contains("[Earlier content truncated]\n\n"));
    assert!(context.message.contains(&"y".repeat(100)));
}

#[test]
fn truncates_an_oversized_pinned_first_user_message() {
    let first = format!("Topic anchor {}", "a".repeat(4_000));
    let assistant = "y".repeat(9_000);
    let last = "z".repeat(1_500);
    let context = format_thread_title_context(&[
        msg(User, &first),
        msg(Assistant, &assistant),
        msg(User, &last),
    ]);
    assert!(context.message.contains("USER:\nTopic anchor"));
    assert!(context.message.contains("[Content truncated]"));
    assert!(context.message.contains("[Earlier content truncated]\n\n"));
}

#[test]
fn retains_at_most_four_attachments_from_the_newest_messages() {
    let older = [attachment("a"), attachment("b")];
    let newer = [attachment("c"), attachment("d"), attachment("e")];
    let context =
        format_thread_title_context(&[with(User, "older", &older), with(User, "newer", &newer)]);
    assert_eq!(names(&context), ["a.png", "c.png", "d.png", "e.png"]);
}

#[test]
fn surrogate_pairs_are_never_split() {
    let text = "😀".repeat(100);
    let limited = limit_title_message(&text, 30);
    assert!(len(&limited) <= 30);
    assert!(limited.contains(TRUNCATED));
}

#[test]
fn citations_render_as_their_text_and_comment() {
    let href = "t3-citation://v1/env-1/thread-1/message-1?text=quoted+words&start=0&end=12&prefix=&suffix=&comment=why%3F";
    let prompt = format!("See [Assistant quote]({href}) please");
    assert_eq!(
        assistant_citations_to_plain_text(&prompt),
        "See quoted words\nComment: why? please"
    );
    let no_comment = "[Assistant quote](t3-citation://v1/env-1/thread-1/message-1?text=hi&start=0&end=2&prefix=&suffix=)";
    assert_eq!(assistant_citations_to_plain_text(no_comment), "hi");
    // An unparseable href keeps its source.
    let broken =
        "[Assistant quote](t3-citation://v1/env-1/thread-1?text=hi&start=0&end=2&prefix=&suffix=)";
    assert_eq!(assistant_citations_to_plain_text(broken), broken);
}
