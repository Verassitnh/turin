#![allow(clippy::type_complexity)]
//! Port of upstream/apps/web/src/components/composerContextPresentation.tsx
//! (+ composerContextPresentation.test.ts)
//!
//! How each kind of composer context reads as a chip, and the draft-side
//! records behind a context reference. The editor knows a reference by
//! `(kind, contextId, label)`; the composer hands it
//! [`ComposerDraftContextRecords`] to look the payload up and
//! [`ComposerContextActions`] for what a chip can do. Upstream passes both
//! through React contexts; here they are arguments of
//! [`composer_context_reference_chip`].
//!
//! Not ported: `ReadOnlySourcePreview` (a review comment's diff is plain mono
//! text), `PullRequestGlyph` (the pull request icons are lucide glyphs) and
//! the terminal chip's scrollable `pre` focus ring.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, Image, InteractiveElement as _, IntoElement as _, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, px, rems,
};
use turin_contracts::composer_context::{ComposerContextRecord, ThreadContextRecord};

use crate::components::chat::markdown::ChatMarkdown;
use crate::components::context_presentation_registry::{
    ContextDetails, ContextPresentationDefinition, ContextPresentationHandler,
    ContextPresentationRegistry, create_context_presentation_registry,
};
use crate::composer::attachment_files::format_attachment_size;
use crate::composer::attachment_upload_state::{
    AttachmentUploadState, format_attachment_upload_progress,
};
use crate::composer::composer_context_records::{
    ComposerContextAttachment, ComposerFileDraft, ComposerImageDraft, PreviewAnnotationPayload,
    ReviewCommentContext, TerminalContextDraft, file_context_reference, image_context_reference,
    is_pull_request_summary_context, normalize_terminal_context_text,
    preview_annotation_context_id, preview_annotation_context_label,
    pull_request_context_display_state, pull_request_context_kind_label, review_comment_context_id,
    review_comment_context_label, terminal_context_reference, uploaded_attachment_context_record,
};
use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};
use crate::composer::views::context_chip::ContextChipKind;
use crate::composer::views::context_chip_parts::{
    ContextChipPopover, ContextChipShell, FileChip, ImageChipButton, ImageChipPreview,
    PullRequestChip, pull_request_chip_kind, unresolved_chip,
};
use crate::composer::views::thread_context_chip::{ThreadContextChip, ThreadShellTitle};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme as _, AppTheme};
use crate::ui::IconName;

/// Draft-side payload behind a context reference chip. Each kind keeps its
/// existing draft shape; the editor only needs a way to look one up by id.
#[derive(Debug, Clone, PartialEq)]
pub enum ComposerDraftContextRecord {
    Terminal(TerminalContextDraft),
    ReviewComment(ReviewCommentContext),
    PreviewAnnotation {
        record: PreviewAnnotationPayload,
        /// The crop, which the draft stores as an image sharing the
        /// annotation's id.
        screenshot: Option<ComposerImageAttachment>,
    },
    Image {
        record: ComposerImageAttachment,
        upload: Option<AttachmentUploadState>,
    },
    File {
        record: ComposerFileAttachment,
        upload: Option<AttachmentUploadState>,
    },
    Thread(ThreadContextRecord),
}

type OpenById = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What a chip can do beyond showing itself; the composer supplies the
/// handlers (`ComposerContextActions`).
#[derive(Clone)]
pub struct ComposerContextActions {
    pub environment_id: Option<String>,
    pub cwd: Option<String>,
    pub expand_image: OpenById,
    pub expand_video: OpenById,
    pub open_file: OpenById,
    pub open_mention: OpenById,
    /// Opens a pull request by its URL.
    pub open_pull_request: OpenById,
    /// `useThreadShell`: the live shell of an attached thread.
    pub thread_shell: Rc<dyn Fn(&ThreadContextRecord) -> Option<ThreadShellTitle>>,
    /// The router `Link` of a thread chip.
    pub open_thread: Rc<dyn Fn(&ThreadContextRecord, &mut Window, &mut App)>,
}

impl Default for ComposerContextActions {
    fn default() -> Self {
        let none: OpenById = Rc::new(|_, _, _| {});
        Self {
            environment_id: None,
            cwd: None,
            expand_image: none.clone(),
            expand_video: none.clone(),
            open_file: none.clone(),
            open_mention: none.clone(),
            open_pull_request: none,
            thread_shell: Rc::new(|_| None),
            open_thread: Rc::new(|_, _, _| {}),
        }
    }
}

pub type ComposerDraftContextRecords = HashMap<String, ComposerDraftContextRecord>;

/// `uploadedContextRecordFromDraft`: the wire record of an attachment that is
/// already uploaded, so a paste of its chip recovers it.
pub fn uploaded_context_record_from_draft(
    entry: &ComposerDraftContextRecord,
) -> Option<ComposerContextRecord> {
    match entry {
        ComposerDraftContextRecord::Image { record, upload } => uploaded_attachment_context_record(
            &ComposerContextAttachment::Image(image_draft(record)),
            upload.as_ref(),
        ),
        ComposerDraftContextRecord::File { record, upload } => uploaded_attachment_context_record(
            &ComposerContextAttachment::File(file_draft(record)),
            upload.as_ref(),
        ),
        _ => None,
    }
}

fn image_draft(image: &ComposerImageAttachment) -> ComposerImageDraft {
    ComposerImageDraft {
        id: image.id.clone(),
        name: image.name.clone(),
        mime_type: image.mime_type.clone(),
        size_bytes: image.size_bytes,
    }
}

fn file_draft(file: &ComposerFileAttachment) -> ComposerFileDraft {
    ComposerFileDraft {
        id: file.id.clone(),
        name: file.name.clone(),
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        uploaded_attachment_id: file.uploaded_attachment_id.clone(),
        upload_environment_id: file.upload_environment_id.clone(),
    }
}

/// The draft pieces `composerContextRecordsFromDraft` indexes.
#[derive(Default)]
pub struct DraftContextInput<'a> {
    pub terminal_contexts: &'a [TerminalContextDraft],
    pub review_comments: &'a [ReviewCommentContext],
    pub preview_annotations: &'a [PreviewAnnotationPayload],
    pub thread_contexts: &'a [ThreadContextRecord],
    pub images: &'a [ComposerImageAttachment],
    pub files: &'a [ComposerFileAttachment],
    pub uploads_by_image_id: Option<&'a HashMap<String, AttachmentUploadState>>,
}

/// `composerContextRecordsFromDraft`, keyed by wire context id so a
/// reference resolves to its own backing draft even when producer ids
/// collide across kinds.
pub fn composer_context_records_from_draft(
    input: &DraftContextInput<'_>,
) -> ComposerDraftContextRecords {
    let upload_of = |id: &str| {
        input
            .uploads_by_image_id
            .and_then(|uploads| uploads.get(id))
            .cloned()
    };
    let mut records = ComposerDraftContextRecords::new();
    for record in input.images {
        records.insert(
            image_context_reference(&image_draft(record))
                .context_id
                .as_str()
                .to_owned(),
            ComposerDraftContextRecord::Image {
                record: record.clone(),
                upload: upload_of(&record.id),
            },
        );
    }
    for record in input.files {
        records.insert(
            file_context_reference(&file_draft(record))
                .context_id
                .as_str()
                .to_owned(),
            ComposerDraftContextRecord::File {
                record: record.clone(),
                upload: upload_of(&record.id),
            },
        );
    }
    for record in input.terminal_contexts {
        records.insert(
            terminal_context_reference(record)
                .context_id
                .as_str()
                .to_owned(),
            ComposerDraftContextRecord::Terminal(record.clone()),
        );
    }
    for record in input.review_comments {
        records.insert(
            review_comment_context_id(&record.id).as_str().to_owned(),
            ComposerDraftContextRecord::ReviewComment(record.clone()),
        );
    }
    for record in input.preview_annotations {
        records.insert(
            preview_annotation_context_id(&record.id)
                .as_str()
                .to_owned(),
            ComposerDraftContextRecord::PreviewAnnotation {
                record: record.clone(),
                screenshot: input
                    .images
                    .iter()
                    .find(|image| image.id == record.id)
                    .cloned(),
            },
        );
    }
    for record in input.thread_contexts {
        records.insert(
            record.context_id.as_str().to_owned(),
            ComposerDraftContextRecord::Thread(record.clone()),
        );
    }
    records
}

/// `uploadStatusSuffix`.
fn upload_status_suffix(upload: Option<&AttachmentUploadState>) -> Option<String> {
    match upload? {
        AttachmentUploadState::Uploading { progress, .. } => {
            Some(format_attachment_upload_progress(*progress))
        }
        AttachmentUploadState::Failed { .. } => Some("upload failed".to_owned()),
        AttachmentUploadState::Ready(_) => None,
    }
}

/// `attachmentTooltip`: name, size and, for a failed upload, the reason.
fn attachment_tooltip(
    name: &str,
    size_bytes: u64,
    upload: Option<&AttachmentUploadState>,
) -> String {
    let mut lines = vec![name.to_owned(), format_attachment_size(size_bytes)];
    if let Some(AttachmentUploadState::Failed { reason, .. }) = upload {
        lines.push(String::new());
        lines.push(reason.clone());
    }
    lines.join("\n")
}

/// `previewAnnotationTooltip`.
pub fn preview_annotation_tooltip(annotation: &PreviewAnnotationPayload) -> String {
    let title = annotation
        .page_title
        .as_deref()
        .map(turin_contracts::base_schemas::js_trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&annotation.page_url);
    let mut lines = vec![title.to_owned()];
    let comment = turin_contracts::base_schemas::js_trim(&annotation.comment);
    if !comment.is_empty() {
        lines.push(String::new());
        lines.push(comment.to_owned());
    }
    let plural =
        |count: usize, noun: &str| format!("{count} {noun}{}", if count == 1 { "" } else { "s" });
    let mut targets = Vec::new();
    if !annotation.elements.is_empty() {
        targets.push(plural(annotation.elements.len(), "element"));
    }
    if !annotation.regions.is_empty() {
        targets.push(plural(annotation.regions.len(), "region"));
    }
    if !annotation.strokes.is_empty() {
        targets.push(plural(annotation.strokes.len(), "drawing"));
    }
    if !annotation.style_changes.is_empty() {
        targets.push(plural(annotation.style_changes.len(), "style change"));
    }
    if !targets.is_empty() {
        lines.push(String::new());
        lines.push(targets.join(", "));
    }
    lines.join("\n")
}

/// The line range a terminal excerpt covers.
pub fn terminal_line_range_text(line_start: u64, line_end: u64) -> String {
    if line_start == line_end {
        format!("Line {line_start}")
    } else {
        format!("Lines {line_start}–{line_end}")
    }
}

/// `hasTerminalContextText` negated.
fn is_terminal_context_expired(context: &TerminalContextDraft) -> bool {
    normalize_terminal_context_text(&context.text).is_empty()
}

struct RenderContext {
    label: String,
    id: SharedString,
    actions: ComposerContextActions,
}

fn unresolved_context_chip(context: &RenderContext) -> AnyElement {
    unresolved_chip(
        context.id.clone(),
        context.label.clone(),
        "This context is no longer available. Remove it or attach it again.",
    )
    .into_any_element()
}

/// `ContextChip` of the registry file: a popover or tooltip by capability.
#[allow(clippy::too_many_arguments)]
fn details_chip(
    id: SharedString,
    kind: ContextChipKind,
    icon: IconName,
    label: String,
    kind_label: String,
    details_mode: ContextDetails,
    details: Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>,
    tooltip: String,
) -> AnyElement {
    if details_mode == ContextDetails::Popover {
        return ContextChipPopover {
            id,
            kind,
            icon,
            accessible_label: format!("{kind_label}, {label}").into(),
            label: label.into(),
            details,
        }
        .into_any_element();
    }
    ContextChipShell {
        id,
        icon,
        label: label.into(),
        kind,
        state: None,
        tooltip: (details_mode == ContextDetails::Tooltip).then(|| tooltip.into()),
    }
    .into_any_element()
}

fn render_terminal(
    entry: &ComposerDraftContextRecord,
    context: &RenderContext,
    definition: &ContextPresentationDefinition,
) -> AnyElement {
    let ComposerDraftContextRecord::Terminal(terminal) = entry else {
        return unresolved_context_chip(context);
    };
    let label = crate::composer::composer_context_records::format_terminal_context_label(terminal);
    let expired = is_terminal_context_expired(terminal);
    let details_mode = definition.capabilities.details;
    if !expired && !terminal.text.is_empty() && details_mode == ContextDetails::Popover {
        let terminal = terminal.clone();
        return ContextChipPopover {
            id: context.id.clone(),
            kind: ContextChipKind::Terminal,
            icon: IconName::Terminal,
            accessible_label: format!("Terminal excerpt, {label}").into(),
            label: label.into(),
            details: Rc::new(move |_, cx| terminal_details(&terminal, cx.app_theme())),
        }
        .into_any_element();
    }
    ContextChipShell {
        id: context.id.clone(),
        icon: IconName::Terminal,
        label: label.clone().into(),
        kind: ContextChipKind::Terminal,
        state: expired.then_some(crate::composer::views::context_chip::ContextChipState::Invalid),
        tooltip: if expired {
            Some(
                format!(
                    "Terminal context expired. Remove and re-add {label} to include it in your message."
                )
                .into(),
            )
        } else if details_mode == ContextDetails::None {
            None
        } else {
            Some(terminal.text.clone().into())
        },
    }
    .into_any_element()
}

/// The popover body of a terminal excerpt (`TerminalContextInlineChip`).
fn terminal_details(terminal: &TerminalContextDraft, theme: &AppTheme) -> AnyElement {
    div()
        .overflow_hidden()
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(with_alpha(theme.border, 0.7))
        .bg(with_alpha(theme.background, 0.8))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(with_alpha(theme.border, 0.7))
                .child(
                    crate::ui::Icon::new(IconName::Terminal)
                        .size(rems(1.))
                        .color(theme.success),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(TEXT_SM.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(terminal.terminal_label.clone()),
                )
                .child(
                    div()
                        .ml_auto()
                        .flex_shrink_0()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.secondary_label)
                        .child(terminal_line_range_text(
                            terminal.line_start,
                            terminal.line_end,
                        )),
                ),
        )
        .child(
            div()
                .id("composer-terminal-excerpt")
                .max_h(rems(20.))
                .overflow_scroll()
                .bg(theme.muted)
                .p_3()
                .font_family(theme.fonts.mono.clone())
                .text_size(TEXT_XS.size)
                .line_height(rems(1.25 * 1.0))
                .text_color(theme.foreground)
                .whitespace_nowrap()
                .child(terminal.text.clone()),
        )
        .into_any_element()
}

fn render_image(
    entry: &ComposerDraftContextRecord,
    context: &RenderContext,
    _: &ContextPresentationDefinition,
) -> AnyElement {
    let ComposerDraftContextRecord::Image { record, upload } = entry else {
        return unresolved_context_chip(context);
    };
    let expand = context.actions.expand_image.clone();
    let id = record.id.clone();
    ImageChipButton {
        id: context.id.clone(),
        name: record.name.clone(),
        preview: Some(ImageChipPreview {
            id: record.id.clone(),
            mime_type: record.mime_type.clone(),
            bytes: record.data.clone(),
        }),
        size: format_attachment_size(record.size_bytes),
        suffix: upload_status_suffix(upload.as_ref()),
        on_click: Rc::new(move |window, cx| expand(&id, window, cx)),
    }
    .into_any_element()
}

fn render_file(
    entry: &ComposerDraftContextRecord,
    context: &RenderContext,
    _: &ContextPresentationDefinition,
) -> AnyElement {
    let ComposerDraftContextRecord::File { record, upload } = entry else {
        return unresolved_context_chip(context);
    };
    let needs_reattach = record.needs_reattach();
    let suffix = if needs_reattach {
        Some("attach again".to_owned())
    } else {
        upload_status_suffix(upload.as_ref())
    };
    let is_video = turin_shared::video::video_mime_type(&record.name, &record.mime_type).is_some();
    let on_open = (!needs_reattach).then(|| {
        let actions = context.actions.clone();
        let id = record.id.clone();
        Rc::new(move |window: &mut Window, cx: &mut App| {
            if is_video {
                (actions.expand_video)(&id, window, cx)
            } else {
                (actions.open_file)(&id, window, cx)
            }
        }) as Rc<dyn Fn(&mut Window, &mut App)>
    });
    FileChip {
        id: context.id.clone(),
        name: record.name.clone(),
        size: format_attachment_size(record.size_bytes),
        is_video,
        tooltip: if needs_reattach {
            format!(
                "{} was not saved with this draft. Attach it again to send it.",
                record.name
            )
        } else {
            attachment_tooltip(&record.name, record.size_bytes, upload.as_ref())
        },
        suffix,
        disabled: false,
        error: matches!(upload, Some(AttachmentUploadState::Failed { .. })),
        unresolved: needs_reattach,
        on_open,
    }
    .into_any_element()
}

fn review_comment_details(
    comment: &ReviewCommentContext,
    id: &str,
    theme: &AppTheme,
) -> AnyElement {
    let text = turin_contracts::base_schemas::js_trim(&comment.text);
    let diff = turin_contracts::base_schemas::js_trim(&comment.diff);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .overflow_hidden()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(with_alpha(theme.border, 0.7))
        .bg(with_alpha(theme.background, 0.7))
        .p_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .truncate()
                        .text_size(TEXT_XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(comment.file_path.clone()),
                )
                .child(
                    div()
                        .text_size(TEXT_2XS.size)
                        .text_color(theme.secondary_label)
                        .child(format!(
                            "{} · {}",
                            comment.section_title, comment.range_label
                        )),
                ),
        )
        .when(!text.is_empty(), |this| {
            this.child(ChatMarkdown::new(
                SharedString::from(format!("{id}-markdown")),
                text.to_owned(),
            ))
        })
        .when(!diff.is_empty(), |this| {
            this.child(
                div()
                    .id(SharedString::from(format!("{id}-diff")))
                    .h(rems(16.))
                    .min_h_0()
                    .overflow_scroll()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .p_2()
                    .font_family(theme.fonts.mono.clone())
                    .text_size(TEXT_XS.size)
                    .whitespace_nowrap()
                    .child(diff.to_owned()),
            )
        })
        .into_any_element()
}

fn render_review_comment(
    entry: &ComposerDraftContextRecord,
    context: &RenderContext,
    definition: &ContextPresentationDefinition,
) -> AnyElement {
    let ComposerDraftContextRecord::ReviewComment(comment) = entry else {
        return unresolved_context_chip(context);
    };
    let is_pull_request = is_pull_request_summary_context(comment);
    let display_state = pull_request_context_display_state(comment);
    let chip_kind = pull_request_chip_kind(display_state);
    if is_pull_request && let Some(metadata) = comment.pull_request.clone() {
        let open = context.actions.open_pull_request.clone();
        return PullRequestChip {
            id: context.id.clone(),
            metadata,
            label: review_comment_context_label(comment).into(),
            kind_label: pull_request_context_kind_label(comment),
            kind: chip_kind,
            on_open: open,
        }
        .into_any_element();
    }
    let kind_label = if is_pull_request {
        pull_request_context_kind_label(comment)
    } else {
        "Review comment".to_owned()
    };
    let label = review_comment_context_label(comment);
    let details_comment = comment.clone();
    let details_id = context.id.to_string();
    details_chip(
        context.id.clone(),
        if is_pull_request {
            chip_kind
        } else {
            ContextChipKind::ReviewComment
        },
        if is_pull_request {
            IconName::GitPullRequestArrow
        } else {
            IconName::MessageCircle
        },
        label,
        kind_label,
        definition.capabilities.details,
        Rc::new(move |_, cx| review_comment_details(&details_comment, &details_id, cx.app_theme())),
        comment.text.clone(),
    )
}

fn preview_annotation_details(
    annotation: &PreviewAnnotationPayload,
    screenshot: Option<&ComposerImageAttachment>,
    theme: &AppTheme,
) -> AnyElement {
    let summary = preview_annotation_tooltip(annotation);
    // The draft drops the annotation's own data URL and keeps the crop as an
    // image.
    let crop = screenshot.and_then(|screenshot| {
        crate::composer::views::image_thumbnail::gpui_image_format(&screenshot.mime_type)
            .map(|format| Arc::new(Image::from_bytes(format, screenshot.data.as_ref().clone())))
    });
    div()
        .overflow_hidden()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(with_alpha(theme.border, 0.7))
        .bg(with_alpha(theme.background, 0.7))
        .child(match crop {
            Some(crop) => img(crop)
                .w_full()
                .max_h(rems(16.))
                .bg(theme.muted)
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .border_b_1()
                .border_color(with_alpha(theme.border, 0.7))
                .bg(with_alpha(theme.muted, 0.4))
                .px_3()
                .py_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.secondary_label)
                .child("Screenshot unavailable")
                .into_any_element(),
        })
        .child(
            div()
                .px_3()
                .py(rems(0.625))
                .text_size(TEXT_SM.size)
                .text_color(theme.foreground)
                .child(summary),
        )
        .into_any_element()
}

fn render_preview_annotation(
    entry: &ComposerDraftContextRecord,
    context: &RenderContext,
    definition: &ContextPresentationDefinition,
) -> AnyElement {
    let ComposerDraftContextRecord::PreviewAnnotation { record, screenshot } = entry else {
        return unresolved_context_chip(context);
    };
    let annotation = record.clone();
    let screenshot_for_details = screenshot.clone();
    details_chip(
        context.id.clone(),
        ContextChipKind::PreviewAnnotation,
        IconName::MousePointerClick,
        preview_annotation_context_label(record),
        "Preview annotation".to_owned(),
        definition.capabilities.details,
        Rc::new(move |_, cx| {
            preview_annotation_details(&annotation, screenshot_for_details.as_ref(), cx.app_theme())
        }),
        preview_annotation_tooltip(record),
    )
}

fn render_thread(
    entry: &ComposerDraftContextRecord,
    context: &RenderContext,
    _: &ContextPresentationDefinition,
) -> AnyElement {
    let ComposerDraftContextRecord::Thread(record) = entry else {
        return unresolved_context_chip(context);
    };
    let actions = context.actions.clone();
    let opened = record.clone();
    ThreadContextChip {
        id: context.id.clone(),
        record_title: record.title.clone(),
        shell: (actions.thread_shell)(record),
        on_open: Rc::new(move |window, cx| (actions.open_thread)(&opened, window, cx)),
    }
    .into_any_element()
}

fn fallback(
    _: &str,
    _: Option<&ComposerDraftContextRecord>,
    context: &RenderContext,
) -> AnyElement {
    unresolved_context_chip(context)
}

type Registry = ContextPresentationRegistry<ComposerDraftContextRecord, RenderContext, AnyElement>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        type Renderer = fn(
            &ComposerDraftContextRecord,
            &RenderContext,
            &ContextPresentationDefinition,
        ) -> AnyElement;
        type CanRender = fn(&ComposerDraftContextRecord, &RenderContext) -> bool;
        fn handler(
            kind: &'static str,
            can_render: CanRender,
            render: Renderer,
        ) -> ContextPresentationHandler<ComposerDraftContextRecord, RenderContext, AnyElement>
        {
            ContextPresentationHandler {
                kind,
                can_render: Some(can_render),
                render,
            }
        }
        create_context_presentation_registry(
            vec![
                handler(
                    "terminal",
                    |entry, _| matches!(entry, ComposerDraftContextRecord::Terminal(_)),
                    render_terminal,
                ),
                handler(
                    "image",
                    |entry, _| matches!(entry, ComposerDraftContextRecord::Image { .. }),
                    render_image,
                ),
                handler(
                    "file",
                    |entry, _| matches!(entry, ComposerDraftContextRecord::File { .. }),
                    render_file,
                ),
                handler(
                    "review-comment",
                    |entry, _| matches!(entry, ComposerDraftContextRecord::ReviewComment(_)),
                    render_review_comment,
                ),
                handler(
                    "preview-annotation",
                    |entry, _| {
                        matches!(entry, ComposerDraftContextRecord::PreviewAnnotation { .. })
                    },
                    render_preview_annotation,
                ),
                handler(
                    "thread",
                    |entry, _| matches!(entry, ComposerDraftContextRecord::Thread(_)),
                    render_thread,
                ),
            ],
            &[
                "image",
                "file",
                "terminal",
                "review-comment",
                "preview-annotation",
                "thread",
            ],
            fallback,
        )
        .expect("composer context presentation registry")
    })
}

/// What a click on an inline context reference does. The inline chips are
/// painted by the editor, so the click is resolved here instead of by the
/// chip element's own handlers.
#[derive(Debug, Clone, PartialEq)]
pub enum ContextClick {
    ExpandImage(String),
    ExpandVideo(String),
    OpenFile(String),
    OpenPullRequest(String),
    OpenThread(ThreadContextRecord),
    /// The popover body of a terminal excerpt, review comment or preview
    /// annotation (`details: "popover"`).
    Details,
    Nothing,
}

/// `ContextClick` for a reference, from the record behind it.
pub fn context_reference_click(
    context_id: &str,
    records: &ComposerDraftContextRecords,
) -> ContextClick {
    use crate::components::context_presentation_registry::context_presentation_definition;
    let Some(entry) = records.get(context_id) else {
        return ContextClick::Nothing;
    };
    let capability = |kind: &str| context_presentation_definition(kind).capabilities;
    match entry {
        ComposerDraftContextRecord::Image { record, .. } => {
            ContextClick::ExpandImage(record.id.clone())
        }
        ComposerDraftContextRecord::File { record, .. } => {
            if record.needs_reattach() {
                ContextClick::Nothing
            } else if turin_shared::video::video_mime_type(&record.name, &record.mime_type)
                .is_some()
            {
                ContextClick::ExpandVideo(record.id.clone())
            } else {
                ContextClick::OpenFile(record.id.clone())
            }
        }
        ComposerDraftContextRecord::Terminal(terminal) => {
            if !is_terminal_context_expired(terminal)
                && capability("terminal").details == ContextDetails::Popover
            {
                ContextClick::Details
            } else {
                ContextClick::Nothing
            }
        }
        ComposerDraftContextRecord::ReviewComment(comment) => {
            match (
                is_pull_request_summary_context(comment),
                &comment.pull_request,
            ) {
                (true, Some(metadata)) => ContextClick::OpenPullRequest(metadata.url.clone()),
                _ if capability("review-comment").details == ContextDetails::Popover => {
                    ContextClick::Details
                }
                _ => ContextClick::Nothing,
            }
        }
        ComposerDraftContextRecord::PreviewAnnotation { .. } => {
            if capability("preview-annotation").details == ContextDetails::Popover {
                ContextClick::Details
            } else {
                ContextClick::Nothing
            }
        }
        ComposerDraftContextRecord::Thread(record) => ContextClick::OpenThread(record.clone()),
    }
}

/// The popover body of a reference: the details its chip would open.
pub fn composer_context_details(
    entry: &ComposerDraftContextRecord,
    id: &str,
    theme: &AppTheme,
) -> Option<AnyElement> {
    match entry {
        ComposerDraftContextRecord::Terminal(terminal) => Some(terminal_details(terminal, theme)),
        ComposerDraftContextRecord::ReviewComment(comment) => {
            Some(review_comment_details(comment, id, theme))
        }
        ComposerDraftContextRecord::PreviewAnnotation { record, screenshot } => Some(
            preview_annotation_details(record, screenshot.as_ref(), theme),
        ),
        _ => None,
    }
}

/// `ComposerContextReferenceChip`: the compact chip for one reference.
/// Unknown kinds and missing records use the registry fallback.
pub fn composer_context_reference_chip(
    kind: &str,
    context_id: &str,
    label: &str,
    records: &ComposerDraftContextRecords,
    actions: &ComposerContextActions,
) -> AnyElement {
    registry().render(
        kind,
        records.get(context_id),
        &RenderContext {
            label: label.to_owned(),
            id: SharedString::from(format!("composer-context-{context_id}")),
            actions: actions.clone(),
        },
    )
}

#[cfg(test)]
#[path = "composer_context_presentation_tests.rs"]
mod tests;
