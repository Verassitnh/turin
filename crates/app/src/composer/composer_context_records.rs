//! Port of upstream/apps/web/src/lib/composerContextRecords.ts
//!
//! Builds the wire records behind a draft's inline references, and the reverse
//! for reading a message. Draft shapes stay what the producing panels emit;
//! only the send boundary converts.
//!
//! Three helpers this file calls are not ported as their own modules, so their
//! functions live here: `toKindScopedComposerContextId` and
//! `producerIdFromComposerContextId` (`apps/web/src/lib/composerContextReferences.ts`),
//! `normalizeElementContextSelection` (`elementContext.ts`), and
//! `formatTerminalContextLabel` / `normalizeTerminalContextText` (`terminalContext.ts`).
//! The review draft includes `pullRequest`; `diff/review_comment_context.rs`
//! omitted that field.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use chrono::SecondsFormat;
use regex::Regex;
use turin_contracts::base_schemas::{EnvironmentId, MessageId, ThreadId, TrimmedNonEmptyString};
use turin_contracts::composer_context::{
    COMPOSER_CONTEXT_REVIEW_DIFF_MAX_CHARS, COMPOSER_CONTEXT_REVIEW_TEXT_MAX_CHARS,
    ComposerContextId, ComposerContextRecord, ComposerContextRecordVersion, ContextAttachmentId,
    ElementContextDetails, ElementContextSource, FileContextKind, FileContextRecord,
    ImageContextKind, ImageContextRecord, KnownComposerContextRecord, OrchestrationMessageContext,
    PreviewAnnotationContextKind, PreviewAnnotationContextRecord, PreviewAnnotationStyleChange,
    PullRequestContextMetadata, ReviewCommentContextKind, ReviewCommentContextRecord,
    TerminalContextKind, TerminalContextRecord, ThreadContextKind, ThreadContextRecord,
};
use turin_contracts::composer_context_clipboard::{
    ComposerContextClipboardFragment, ComposerContextClipboardSource,
    ComposerContextClipboardVersion,
};
use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::source_control::ChangeRequestState;
use turin_shared::composer_context_clipboard::encode_composer_context_fragment;
use turin_shared::composer_context_legacy::upgrade_legacy_context_message;
use turin_shared::composer_context_references::{
    collect_composer_context_references, sanitize_composer_context_label,
};
use turin_shared::js_string::{replace_js_whitespace_runs, slice_utf16, utf16_len};

use crate::composer::attachment_upload_state::AttachmentUploadState;

const PREVIEW_LABEL_MAX_CHARS: usize = 48;
const TRUNCATION_MARKER: &str = "\n… truncated …";
const ELEMENT_HTML_LIMIT: usize = 4000;
const ELEMENT_STYLES_LIMIT: usize = 4000;

static NON_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9_-]+").expect("static regex"));
static PR_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^PR #([0-9]+)$").expect("static regex"));
static STYLE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?): ([\s\S]*?) → ([\s\S]*)$").expect("static regex"));

/// `toComposerContextId` / `toKindScopedComposerContextId`.
pub fn to_kind_scoped_composer_context_id(kind: &str, producer_id: &str) -> ComposerContextId {
    to_composer_context_id(&format!("{kind}_{producer_id}"))
}

/// `producerIdFromComposerContextId`.
pub fn producer_id_from_composer_context_id(kind: &str, context_id: &str) -> String {
    let prefix = format!("{kind}_");
    context_id
        .strip_prefix(&prefix)
        .unwrap_or(context_id)
        .to_owned()
}

fn to_composer_context_id(producer_id: &str) -> ComposerContextId {
    if is_raw_context_id(producer_id) {
        return derived_context_id(producer_id);
    }
    let slugged = NON_ID.replace_all(producer_id, "-");
    let trimmed = slugged.trim_matches('-');
    let slug = slice_utf16(trimmed, 0, 48);
    let slug = if slug.is_empty() { "ctx" } else { slug };
    derived_context_id(&format!("{slug}-{}", fnv1a64(producer_id)))
}

/// `value` is a raw id that passed `is_raw_context_id`, or a slug of
/// `[A-Za-z0-9_-]` (at most 48 units) plus a hash: both satisfy the contract's
/// id pattern and length, so the check cannot fail.
fn derived_context_id(value: &str) -> ComposerContextId {
    parse_context_id(value).expect("derived context ids satisfy the id pattern")
}

fn is_raw_context_id(value: &str) -> bool {
    let len = utf16_len(value);
    (1..=128).contains(&len)
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Two FNV-1a passes, UTF-16 code units, low 32 bits (`Math.imul` + `>>> 0`).
fn fnv1a64(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut forward = 0x811c_9dc5_u32;
    let mut reverse = 0x9dc5_811c_u32;
    for index in 0..units.len() {
        forward ^= u32::from(units[index]);
        forward = forward.wrapping_mul(0x0100_0193);
        reverse ^= u32::from(units[units.len() - 1 - index]);
        reverse = reverse.wrapping_mul(0x0100_0193);
    }
    format!("{forward:08x}{reverse:08x}")
}

/// A draft that cannot become a context record: a blank required field or an
/// attachment id the contract rejects. Builders return it instead of
/// panicking, so the send boundary can tell the user what to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContextRecordError(pub String);

/// Context ids are built from a validated raw id or a slug plus hash, so this
/// cannot fail for them; the error path exists for the type's sake.
fn parse_context_id(value: &str) -> Result<ComposerContextId, ContextRecordError> {
    ComposerContextId::new(value)
        .map_err(|err| ContextRecordError(format!("Invalid context id {value:?}: {err}")))
}

fn parse_attachment_id(value: &str) -> Result<ContextAttachmentId, ContextRecordError> {
    ContextAttachmentId::new(value)
        .map_err(|err| ContextRecordError(format!("Invalid attachment id {value:?}: {err}")))
}

fn parse_nonempty(field: &str, value: &str) -> Result<TrimmedNonEmptyString, ContextRecordError> {
    TrimmedNonEmptyString::new(value)
        .map_err(|_| ContextRecordError(format!("The {field} must not be blank.")))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn clamp_context_text(value: &str, max: usize) -> String {
    if utf16_len(value) <= max {
        return value.to_owned();
    }
    let marker_len = utf16_len(TRUNCATION_MARKER);
    let keep = max.saturating_sub(marker_len);
    format!("{}{TRUNCATION_MARKER}", slice_utf16(value, 0, keep))
}

fn basename(file_path: &str) -> &str {
    file_path.rsplit(['\\', '/']).next().unwrap_or(file_path)
}

/// Fields `reviewCommentContextLabel` reads. The diff port's review struct
/// has no `pullRequest`, so both the draft and the wire record implement this.
pub trait ReviewCommentPresentation {
    fn section_id(&self) -> &str;
    fn file_path(&self) -> &str;
    fn range_label(&self) -> &str;
    fn diff(&self) -> &str;
    fn pull_request(&self) -> Option<&PullRequestContextMetadata>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCommentContext {
    pub id: String,
    pub section_id: String,
    pub section_title: String,
    pub file_path: String,
    pub start_index: u64,
    pub end_index: u64,
    pub range_label: String,
    pub text: String,
    pub diff: String,
    pub fence_language: Option<String>,
    pub pull_request: Option<PullRequestContextMetadata>,
}

impl ReviewCommentPresentation for ReviewCommentContext {
    fn section_id(&self) -> &str {
        &self.section_id
    }
    fn file_path(&self) -> &str {
        &self.file_path
    }
    fn range_label(&self) -> &str {
        &self.range_label
    }
    fn diff(&self) -> &str {
        &self.diff
    }
    fn pull_request(&self) -> Option<&PullRequestContextMetadata> {
        self.pull_request.as_ref()
    }
}

impl ReviewCommentPresentation for ReviewCommentContextRecord {
    fn section_id(&self) -> &str {
        self.section_id.as_str()
    }
    fn file_path(&self) -> &str {
        self.file_path.as_str()
    }
    fn range_label(&self) -> &str {
        &self.range_label
    }
    fn diff(&self) -> &str {
        &self.diff
    }
    fn pull_request(&self) -> Option<&PullRequestContextMetadata> {
        self.pull_request.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestContextDisplayState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PullRequestContextDisplayState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Draft => "draft",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

pub fn review_comment_context_label(comment: &impl ReviewCommentPresentation) -> String {
    if let Some(number) = pull_request_context_number(comment)
        && is_pull_request_summary_context(comment)
    {
        return format!("#{number}");
    }
    let range_label = presented_range_label(comment.range_label());
    format!("{} {range_label}", basename(comment.file_path()))
}

/// `^([+-])(\d+)(?: to \1(\d+))?$`. The regex crate has no backreferences.
fn presented_range_label(range_label: &str) -> String {
    let mut chars = range_label.chars();
    let Some(sign) = chars.next() else {
        return range_label.to_owned();
    };
    if sign != '+' && sign != '-' {
        return range_label.to_owned();
    }
    let rest = chars.as_str();
    let (start, after) = split_ascii_digits(rest);
    if start.is_empty() {
        return range_label.to_owned();
    }
    let before = if sign == '-' { " (before)" } else { "" };
    let Some(tail) = after.strip_prefix(" to ") else {
        return if after.is_empty() {
            format!("L{start}{before}")
        } else {
            range_label.to_owned()
        };
    };
    let Some(tail) = tail.strip_prefix(sign) else {
        return range_label.to_owned();
    };
    let (end, leftover) = split_ascii_digits(tail);
    if end.is_empty() || !leftover.is_empty() {
        return range_label.to_owned();
    }
    format!("L{start} to L{end}{before}")
}

fn split_ascii_digits(value: &str) -> (&str, &str) {
    let end = value
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    value.split_at(end)
}

pub fn is_pull_request_summary_context(comment: &impl ReviewCommentPresentation) -> bool {
    if comment.pull_request().is_some() {
        return true;
    }
    comment.section_id().starts_with("pull-request:")
        && turin_contracts::base_schemas::js_trim(comment.diff()).is_empty()
        && PR_PATH.is_match(comment.file_path())
}

fn pull_request_context_number(comment: &impl ReviewCommentPresentation) -> Option<u64> {
    if let Some(pull_request) = comment.pull_request() {
        return Some(pull_request.number.get());
    }
    PR_PATH
        .captures(comment.file_path())
        .and_then(|caps| caps.get(1))
        .and_then(|number| number.as_str().parse().ok())
}

pub fn pull_request_context_display_state(
    comment: &impl ReviewCommentPresentation,
) -> Option<PullRequestContextDisplayState> {
    let pull_request = comment.pull_request()?;
    Some(display_state(pull_request.state, pull_request.is_draft))
}

fn display_state(state: ChangeRequestState, is_draft: bool) -> PullRequestContextDisplayState {
    if state == ChangeRequestState::Open && is_draft {
        PullRequestContextDisplayState::Draft
    } else {
        match state {
            ChangeRequestState::Open => PullRequestContextDisplayState::Open,
            ChangeRequestState::Merged => PullRequestContextDisplayState::Merged,
            ChangeRequestState::Closed => PullRequestContextDisplayState::Closed,
        }
    }
}

pub fn pull_request_context_kind_label(comment: &impl ReviewCommentPresentation) -> String {
    let Some(state) = pull_request_context_display_state(comment) else {
        return "Pull request".to_owned();
    };
    let name = state.as_str();
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return "Pull request".to_owned();
    };
    format!("{}{} pull request", first.to_uppercase(), chars.as_str())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickedElementStackFrame {
    pub function_name: Option<String>,
    pub file_name: Option<String>,
    pub line_number: Option<u64>,
    pub column_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickedElementPayload {
    pub page_url: String,
    pub page_title: Option<String>,
    pub tag_name: String,
    pub selector: Option<String>,
    pub html_preview: String,
    pub component_name: Option<String>,
    pub source: Option<PickedElementStackFrame>,
    pub stack: Vec<PickedElementStackFrame>,
    pub styles: String,
    pub picked_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewElementTarget {
    pub id: String,
    pub rect: PreviewRect,
    pub element: PickedElementPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewRegionTarget {
    pub id: String,
    pub rect: PreviewRect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewStrokeTarget {
    pub id: String,
    pub color: String,
    pub width: f64,
    pub points: Vec<PreviewPoint>,
    pub bounds: PreviewRect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewStyleChange {
    pub target_id: String,
    pub selector: Option<String>,
    pub property: String,
    pub previous_value: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationPayload {
    pub id: String,
    pub page_url: String,
    pub page_title: Option<String>,
    pub comment: String,
    pub elements: Vec<PreviewElementTarget>,
    pub regions: Vec<PreviewRegionTarget>,
    pub strokes: Vec<PreviewStrokeTarget>,
    pub style_changes: Vec<PreviewStyleChange>,
    pub screenshot: Option<()>,
    pub created_at: String,
}

pub fn preview_annotation_context_label(annotation: &PreviewAnnotationPayload) -> String {
    let comment = replace_js_whitespace_runs(
        turin_contracts::base_schemas::js_trim(&annotation.comment),
        " ",
    );
    if !comment.is_empty() {
        if utf16_len(&comment) > PREVIEW_LABEL_MAX_CHARS {
            return format!("{}…", slice_utf16(&comment, 0, PREVIEW_LABEL_MAX_CHARS - 1));
        }
        return comment;
    }
    annotation
        .page_title
        .as_deref()
        .map(turin_contracts::base_schemas::js_trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Preview annotation")
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerContextReference {
    pub kind: &'static str,
    pub context_id: ComposerContextId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalContextDraft {
    pub id: String,
    pub thread_id: ThreadId,
    pub created_at: String,
    pub terminal_id: String,
    pub terminal_label: String,
    pub line_start: u64,
    pub line_end: u64,
    pub text: String,
}

pub fn normalize_terminal_context_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_matches('\n').to_owned()
}

pub fn format_terminal_context_label(selection: &TerminalContextDraft) -> String {
    let range = if selection.line_start == selection.line_end {
        format!("line {}", selection.line_start)
    } else {
        format!("lines {}-{}", selection.line_start, selection.line_end)
    };
    format!("{} {range}", selection.terminal_label)
}

pub fn terminal_context_reference(context: &TerminalContextDraft) -> ComposerContextReference {
    ComposerContextReference {
        kind: "terminal",
        context_id: to_kind_scoped_composer_context_id("terminal", &context.id),
        label: format_terminal_context_label(context),
    }
}

pub fn review_comment_context_id(comment_id: &str) -> ComposerContextId {
    to_kind_scoped_composer_context_id("review-comment", comment_id)
}

pub fn preview_annotation_context_id(annotation_id: &str) -> ComposerContextId {
    to_kind_scoped_composer_context_id("preview-annotation", annotation_id)
}

pub fn review_comment_context_reference(
    comment: &ReviewCommentContext,
) -> ComposerContextReference {
    ComposerContextReference {
        kind: "review-comment",
        context_id: review_comment_context_id(&comment.id),
        label: review_comment_context_label(comment),
    }
}

pub fn preview_annotation_context_reference(
    annotation: &PreviewAnnotationPayload,
) -> ComposerContextReference {
    ComposerContextReference {
        kind: "preview-annotation",
        context_id: preview_annotation_context_id(&annotation.id),
        label: preview_annotation_context_label(annotation),
    }
}

fn thread_context_id(thread_id: &str) -> ComposerContextId {
    to_kind_scoped_composer_context_id("thread", thread_id)
}

pub fn thread_context_reference(record: &ThreadContextRecord) -> ComposerContextReference {
    ComposerContextReference {
        kind: "thread",
        context_id: record.context_id.clone(),
        label: record.label.clone(),
    }
}

pub fn thread_context_record(reference: &ScopedThreadRef, title: &str) -> ThreadContextRecord {
    let label = sanitize_composer_context_label(title, "thread");
    ThreadContextRecord {
        version: ComposerContextRecordVersion,
        kind: ThreadContextKind,
        context_id: thread_context_id(reference.thread_id.as_str()),
        label: label.clone(),
        environment_id: reference.environment_id.clone(),
        thread_id: reference.thread_id.clone(),
        title: label,
    }
}

pub fn terminal_context_record(
    context: &TerminalContextDraft,
) -> Result<TerminalContextRecord, ContextRecordError> {
    let label =
        sanitize_composer_context_label(&format_terminal_context_label(context), "terminal");
    Ok(TerminalContextRecord {
        version: ComposerContextRecordVersion,
        context_id: to_kind_scoped_composer_context_id("terminal", &context.id),
        kind: TerminalContextKind,
        label,
        terminal_id: parse_nonempty("terminal id", &context.terminal_id)?,
        terminal_label: parse_nonempty("terminal label", &context.terminal_label)?,
        line_start: context.line_start,
        line_end: context.line_end,
        text: normalize_terminal_context_text(&context.text),
    })
}

pub fn review_comment_context_record(
    comment: &ReviewCommentContext,
) -> Result<ReviewCommentContextRecord, ContextRecordError> {
    Ok(ReviewCommentContextRecord {
        version: ComposerContextRecordVersion,
        context_id: review_comment_context_id(&comment.id),
        kind: ReviewCommentContextKind,
        label: sanitize_composer_context_label(
            &review_comment_context_label(comment),
            "review-comment",
        ),
        section_id: parse_nonempty("review section id", &comment.section_id)?,
        section_title: comment.section_title.clone(),
        file_path: parse_nonempty("review file path", &comment.file_path)?,
        start_index: comment.start_index,
        end_index: comment.end_index,
        range_label: comment.range_label.clone(),
        text: clamp_context_text(&comment.text, COMPOSER_CONTEXT_REVIEW_TEXT_MAX_CHARS),
        diff: clamp_context_text(&comment.diff, COMPOSER_CONTEXT_REVIEW_DIFF_MAX_CHARS),
        fence_language: comment.fence_language.clone(),
        pull_request: comment.pull_request.clone(),
    })
}

fn preview_target_summary(annotation: &PreviewAnnotationPayload) -> String {
    let plural =
        |count: usize, noun: &str| format!("{count} {noun}{}", if count == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if !annotation.elements.is_empty() {
        parts.push(plural(annotation.elements.len(), "selected element"));
    }
    if !annotation.regions.is_empty() {
        parts.push(plural(annotation.regions.len(), "marked region"));
    }
    if !annotation.strokes.is_empty() {
        parts.push(plural(annotation.strokes.len(), "drawing"));
    }
    parts.join(", ")
}

/// `normalizeElementContextSelection`.
pub fn normalize_element_context_selection(
    raw: &PickedElementPayload,
) -> Option<ElementContextDetails> {
    let page_url = turin_contracts::base_schemas::js_trim(&raw.page_url);
    let tag_name = turin_contracts::base_schemas::js_trim(&raw.tag_name).to_lowercase();
    if page_url.is_empty() || tag_name.is_empty() {
        return None;
    }
    let stack_frame = raw.source.as_ref().or_else(|| raw.stack.first());
    Some(ElementContextDetails {
        page_url: page_url.to_owned(),
        page_title: raw
            .page_title
            .as_deref()
            .map(|title| turin_contracts::base_schemas::js_trim(title).to_owned()),
        // Already non-empty above; a type-level rejection drops the element.
        tag_name: parse_nonempty("element tag name", &tag_name).ok()?,
        selector: optional_trimmed(raw.selector.as_deref()),
        html_preview: truncate_element(
            &normalize_terminal_context_text(&raw.html_preview),
            ELEMENT_HTML_LIMIT,
        ),
        component_name: optional_trimmed(raw.component_name.as_deref()),
        source: stack_frame.map(normalize_stack_frame),
        styles: truncate_element(
            &normalize_terminal_context_text(&raw.styles),
            ELEMENT_STYLES_LIMIT,
        ),
    })
}

fn optional_trimmed(value: Option<&str>) -> Option<String> {
    let trimmed = turin_contracts::base_schemas::js_trim(value.unwrap_or(""));
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn normalize_stack_frame(frame: &PickedElementStackFrame) -> ElementContextSource {
    ElementContextSource {
        function_name: optional_trimmed(frame.function_name.as_deref()),
        file_name: optional_trimmed(frame.file_name.as_deref()),
        line_number: frame.line_number,
        column_number: frame.column_number,
    }
}

fn truncate_element(value: &str, limit: usize) -> String {
    if utf16_len(value) <= limit {
        return value.to_owned();
    }
    format!("{}…", slice_utf16(value, 0, limit.saturating_sub(1)))
}

pub fn preview_annotation_context_record(
    annotation: &PreviewAnnotationPayload,
    screenshot_context_id: Option<&str>,
) -> PreviewAnnotationContextRecord {
    let targets: Vec<(&str, ElementContextDetails)> = annotation
        .elements
        .iter()
        .filter_map(|target| {
            normalize_element_context_selection(&target.element)
                .map(|element| (target.id.as_str(), element))
        })
        .collect();
    PreviewAnnotationContextRecord {
        version: ComposerContextRecordVersion,
        context_id: preview_annotation_context_id(&annotation.id),
        kind: PreviewAnnotationContextKind,
        label: sanitize_composer_context_label(
            &preview_annotation_context_label(annotation),
            "preview-annotation",
        ),
        annotation_id: annotation.id.clone(),
        page_url: annotation.page_url.clone(),
        page_title: annotation.page_title.clone(),
        comment: turin_contracts::base_schemas::js_trim(&annotation.comment).to_owned(),
        target_summary: preview_target_summary(annotation),
        style_changes: annotation
            .style_changes
            .iter()
            .map(|change| {
                let previous = if change.previous_value.is_empty() {
                    "(unset)"
                } else {
                    change.previous_value.as_str()
                };
                format!("{}: {previous} → {}", change.property, change.value)
            })
            .collect(),
        elements: (!targets.is_empty())
            .then(|| targets.iter().map(|(_, element)| element.clone()).collect()),
        element_ids: (!targets.is_empty())
            .then(|| targets.iter().map(|(id, _)| (*id).to_owned()).collect()),
        region_count: (!annotation.regions.is_empty()).then_some(annotation.regions.len() as u64),
        stroke_count: (!annotation.strokes.is_empty()).then_some(annotation.strokes.len() as u64),
        style_change_details: Some(
            annotation
                .style_changes
                .iter()
                .map(|change| PreviewAnnotationStyleChange {
                    target_id: change.target_id.clone(),
                    selector: change.selector.clone(),
                    property: change.property.clone(),
                    previous_value: change.previous_value.clone(),
                    value: change.value.clone(),
                })
                .collect(),
        ),
        screenshot_context_id: screenshot_context_id
            .map(|id| to_kind_scoped_composer_context_id("image", id)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerImageDraft {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerFileDraft {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub uploaded_attachment_id: Option<String>,
    pub upload_environment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerContextAttachment {
    Image(ComposerImageDraft),
    File(ComposerFileDraft),
}

impl ComposerContextAttachment {
    fn id(&self) -> &str {
        match self {
            Self::Image(image) => &image.id,
            Self::File(file) => &file.id,
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Self::Image(_) => "image",
            Self::File(_) => "file",
        }
    }
    fn name(&self) -> &str {
        match self {
            Self::Image(image) => &image.name,
            Self::File(file) => &file.name,
        }
    }
    fn mime_type(&self) -> &str {
        match self {
            Self::Image(image) => &image.mime_type,
            Self::File(file) => &file.mime_type,
        }
    }
    fn size_bytes(&self) -> u64 {
        match self {
            Self::Image(image) => image.size_bytes,
            Self::File(file) => file.size_bytes,
        }
    }
    fn persisted_id(&self) -> Option<&str> {
        match self {
            Self::File(file) => file.uploaded_attachment_id.as_deref(),
            Self::Image(_) => None,
        }
    }
}

pub fn image_context_reference(image: &ComposerImageDraft) -> ComposerContextReference {
    ComposerContextReference {
        kind: "image",
        context_id: to_kind_scoped_composer_context_id("image", &image.id),
        label: image.name.clone(),
    }
}

pub fn file_context_reference(file: &ComposerFileDraft) -> ComposerContextReference {
    ComposerContextReference {
        kind: "file",
        context_id: to_kind_scoped_composer_context_id("file", &file.id),
        label: file.name.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundComposerAttachment {
    pub attachment: ComposerContextAttachment,
    pub attachment_id: String,
}

pub fn uploaded_attachment_context_record(
    attachment: &ComposerContextAttachment,
    upload: Option<&AttachmentUploadState>,
) -> Option<ComposerContextRecord> {
    let attachment_id = attachment
        .persisted_id()
        .map(str::to_owned)
        .or_else(|| match upload {
            Some(AttachmentUploadState::Ready(ready)) => Some(ready.attachment_id.clone()),
            _ => None,
        })?;
    // An id the contract rejects means there is no record to offer.
    attachment_context_record(&BoundComposerAttachment {
        attachment: attachment.clone(),
        attachment_id,
    })
    .ok()
}

pub fn attachment_context_record(
    bound: &BoundComposerAttachment,
) -> Result<ComposerContextRecord, ContextRecordError> {
    let attachment = &bound.attachment;
    let context_id = to_kind_scoped_composer_context_id(attachment.kind(), attachment.id());
    let label = sanitize_composer_context_label(attachment.name(), attachment.kind());
    let attachment_id = parse_attachment_id(&bound.attachment_id)?;
    let name = parse_nonempty("attachment name", attachment.name())?;
    let mime_type = parse_nonempty("attachment type", attachment.mime_type())?;
    let size_bytes = attachment.size_bytes();
    match attachment {
        ComposerContextAttachment::Image(_) => Ok(ComposerContextRecord::Known(
            KnownComposerContextRecord::Image(ImageContextRecord {
                version: ComposerContextRecordVersion,
                context_id,
                label,
                kind: ImageContextKind,
                attachment_id,
                name,
                mime_type,
                size_bytes,
            }),
        )),
        ComposerContextAttachment::File(_) => Ok(ComposerContextRecord::Known(
            KnownComposerContextRecord::File(FileContextRecord {
                version: ComposerContextRecordVersion,
                context_id,
                label,
                kind: FileContextKind,
                attachment_id,
                name,
                mime_type,
                size_bytes,
            }),
        )),
    }
}

pub struct MessageContextInput<'a> {
    pub terminal_contexts: &'a [TerminalContextDraft],
    pub review_comments: &'a [ReviewCommentContext],
    pub preview_annotations: &'a [PreviewAnnotationPayload],
    pub thread_contexts: &'a [ThreadContextRecord],
    pub attachments: &'a [BoundComposerAttachment],
}

/// `buildMessageContext`. A record that cannot be built (a blank required
/// field, an attachment id the contract rejects) fails the whole context, so
/// the send boundary reports it instead of sending a message that lost a chip.
pub fn build_message_context(
    input: MessageContextInput<'_>,
) -> Result<Option<OrchestrationMessageContext>, ContextRecordError> {
    let screenshot_ids: HashSet<&str> = input
        .attachments
        .iter()
        .filter_map(|bound| match &bound.attachment {
            ComposerContextAttachment::Image(image) => Some(image.id.as_str()),
            ComposerContextAttachment::File(_) => None,
        })
        .collect();
    let mut records = Vec::new();
    for context in input.terminal_contexts {
        records.push(ComposerContextRecord::Known(
            KnownComposerContextRecord::Terminal(terminal_context_record(context)?),
        ));
    }
    for comment in input.review_comments {
        records.push(ComposerContextRecord::Known(
            KnownComposerContextRecord::ReviewComment(review_comment_context_record(comment)?),
        ));
    }
    records.extend(
        input
            .thread_contexts
            .iter()
            .cloned()
            .map(|record| ComposerContextRecord::Known(KnownComposerContextRecord::Thread(record))),
    );
    records.extend(input.preview_annotations.iter().map(|annotation| {
        let screenshot = screenshot_ids
            .contains(annotation.id.as_str())
            .then_some(annotation.id.as_str());
        ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(
            preview_annotation_context_record(annotation, screenshot),
        ))
    }));
    for bound in input.attachments {
        records.push(attachment_context_record(bound)?);
    }
    Ok(if records.is_empty() {
        None
    } else {
        Some(OrchestrationMessageContext {
            version: ComposerContextRecordVersion,
            records,
        })
    })
}

pub fn as_known_context_record(
    record: &ComposerContextRecord,
) -> Option<&KnownComposerContextRecord> {
    match record {
        ComposerContextRecord::Known(known) => Some(known),
        ComposerContextRecord::Unknown(_) => None,
    }
}

pub fn composer_context_import_lookup_ids(
    record: &KnownComposerContextRecord,
) -> Vec<ComposerContextId> {
    let kind = known_kind(record);
    let context_id = known_context_id(record);
    let destination = to_kind_scoped_composer_context_id(
        kind,
        &producer_id_from_composer_context_id(kind, context_id.as_str()),
    );
    if destination == *context_id {
        vec![context_id.clone()]
    } else {
        vec![destination, context_id.clone()]
    }
}

fn known_kind(record: &KnownComposerContextRecord) -> &'static str {
    match record {
        KnownComposerContextRecord::Image(_) => "image",
        KnownComposerContextRecord::File(_) => "file",
        KnownComposerContextRecord::Terminal(_) => "terminal",
        KnownComposerContextRecord::Element(_) => "element",
        KnownComposerContextRecord::PreviewAnnotation(_) => "preview-annotation",
        KnownComposerContextRecord::ReviewComment(_) => "review-comment",
        KnownComposerContextRecord::Mention(_) => "mention",
        KnownComposerContextRecord::Skill(_) => "skill",
        KnownComposerContextRecord::Thread(_) => "thread",
    }
}

fn known_context_id(record: &KnownComposerContextRecord) -> &ComposerContextId {
    match record {
        KnownComposerContextRecord::Image(record) => &record.context_id,
        KnownComposerContextRecord::File(record) => &record.context_id,
        KnownComposerContextRecord::Terminal(record) => &record.context_id,
        KnownComposerContextRecord::Element(record) => &record.context_id,
        KnownComposerContextRecord::PreviewAnnotation(record) => &record.context_id,
        KnownComposerContextRecord::ReviewComment(record) => &record.context_id,
        KnownComposerContextRecord::Mention(record) => &record.context_id,
        KnownComposerContextRecord::Skill(record) => &record.context_id,
        KnownComposerContextRecord::Thread(record) => &record.context_id,
    }
}

pub struct ResolvedUserMessageContext {
    pub text: String,
    pub records: Vec<ComposerContextRecord>,
    pub records_by_id: HashMap<String, ComposerContextRecord>,
}

pub struct UserMessageContextInput<'a> {
    pub text: &'a str,
    pub context: Option<&'a OrchestrationMessageContext>,
}

pub fn resolve_user_message_context(
    message: UserMessageContextInput<'_>,
) -> ResolvedUserMessageContext {
    let (text, records) = if let Some(context) = message.context {
        (message.text.to_owned(), context.records.clone())
    } else {
        let upgraded = upgrade_legacy_context_message(message.text);
        (upgraded.text, upgraded.records)
    };
    let records_by_id = records
        .iter()
        .map(|record| (record.context_id().as_str().to_owned(), record.clone()))
        .collect();
    ResolvedUserMessageContext {
        text,
        records,
        records_by_id,
    }
}

pub struct SelectedMessageContext<'a> {
    pub markdown: &'a str,
    pub records: &'a [ComposerContextRecord],
    pub environment_id: &'a EnvironmentId,
    pub thread_id: Option<&'a ThreadId>,
    pub message_id: &'a MessageId,
}

pub fn selected_message_context_fragment(input: SelectedMessageContext<'_>) -> Option<String> {
    let selected: HashSet<String> = collect_composer_context_references(input.markdown)
        .into_iter()
        .map(|occurrence| occurrence.context_id.as_str().to_owned())
        .collect();
    let records: Vec<ComposerContextRecord> = input
        .records
        .iter()
        .filter(|record| selected.contains(record.context_id().as_str()))
        .cloned()
        .collect();
    if records.is_empty() {
        return None;
    }
    encode_composer_context_fragment(&ComposerContextClipboardFragment {
        version: ComposerContextClipboardVersion,
        source: ComposerContextClipboardSource {
            environment_id: input.environment_id.clone(),
            thread_id: input.thread_id.cloned(),
            message_id: Some(input.message_id.clone()),
        },
        records,
    })
}

pub fn terminal_context_draft_from_record(
    record: &TerminalContextRecord,
    thread_id: ThreadId,
) -> TerminalContextDraft {
    TerminalContextDraft {
        id: producer_id_from_composer_context_id("terminal", record.context_id.as_str()),
        thread_id,
        created_at: now_iso(),
        terminal_id: record.terminal_id.as_str().to_owned(),
        terminal_label: record.terminal_label.as_str().to_owned(),
        line_start: record.line_start,
        line_end: record.line_end,
        text: record.text.clone(),
    }
}

pub fn review_comment_from_record(record: &ReviewCommentContextRecord) -> ReviewCommentContext {
    ReviewCommentContext {
        id: producer_id_from_composer_context_id("review-comment", record.context_id.as_str()),
        section_id: record.section_id.as_str().to_owned(),
        section_title: record.section_title.clone(),
        file_path: record.file_path.as_str().to_owned(),
        start_index: record.start_index,
        end_index: record.end_index,
        range_label: record.range_label.clone(),
        text: record.text.clone(),
        diff: record.diff.clone(),
        fence_language: record.fence_language.clone(),
        pull_request: record.pull_request.clone(),
    }
}

fn zero_rect() -> PreviewRect {
    PreviewRect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    }
}

pub fn preview_annotation_from_record(
    record: &PreviewAnnotationContextRecord,
) -> PreviewAnnotationPayload {
    let picked_at = now_iso();
    let elements = record
        .elements
        .iter()
        .flatten()
        .enumerate()
        .map(|(index, element)| PreviewElementTarget {
            id: record
                .element_ids
                .as_ref()
                .and_then(|ids| ids.get(index))
                .cloned()
                .unwrap_or_else(|| format!("{}-element-{}", record.context_id, index + 1)),
            rect: zero_rect(),
            element: PickedElementPayload {
                page_url: element.page_url.clone(),
                page_title: element.page_title.clone(),
                tag_name: element.tag_name.as_str().to_owned(),
                selector: element.selector.clone(),
                html_preview: element.html_preview.clone(),
                component_name: element.component_name.clone(),
                source: element
                    .source
                    .as_ref()
                    .map(|source| PickedElementStackFrame {
                        function_name: source.function_name.clone(),
                        file_name: source.file_name.clone(),
                        line_number: source.line_number,
                        column_number: source.column_number,
                    }),
                stack: Vec::new(),
                styles: element.styles.clone(),
                picked_at: picked_at.clone(),
            },
        })
        .collect();
    let region_count = record.region_count.unwrap_or(0) as usize;
    let stroke_count = record.stroke_count.unwrap_or(0) as usize;
    let fallback_target = record
        .element_ids
        .as_ref()
        .and_then(|ids| ids.first())
        .cloned()
        .unwrap_or_else(|| format!("{}-element-1", record.context_id));
    let style_changes = if let Some(details) = &record.style_change_details {
        details
            .iter()
            .map(|change| PreviewStyleChange {
                target_id: change.target_id.clone(),
                selector: change.selector.clone(),
                property: change.property.clone(),
                previous_value: change.previous_value.clone(),
                value: change.value.clone(),
            })
            .collect()
    } else {
        record
            .style_changes
            .iter()
            .filter_map(|change| parse_style_change(change, &fallback_target))
            .collect()
    };
    PreviewAnnotationPayload {
        id: if record.annotation_id.is_empty() {
            producer_id_from_composer_context_id("preview-annotation", record.context_id.as_str())
        } else {
            record.annotation_id.clone()
        },
        page_url: record.page_url.clone(),
        page_title: record.page_title.clone(),
        comment: record.comment.clone(),
        elements,
        regions: (0..region_count)
            .map(|index| PreviewRegionTarget {
                id: format!("{}-region-{}", record.context_id, index + 1),
                rect: zero_rect(),
            })
            .collect(),
        strokes: (0..stroke_count)
            .map(|index| PreviewStrokeTarget {
                id: format!("{}-stroke-{}", record.context_id, index + 1),
                color: String::new(),
                width: 0.0,
                points: Vec::new(),
                bounds: zero_rect(),
            })
            .collect(),
        style_changes,
        screenshot: None,
        created_at: picked_at,
    }
}

fn parse_style_change(change: &str, fallback_target: &str) -> Option<PreviewStyleChange> {
    let caps = STYLE_LINE.captures(change)?;
    let previous = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    Some(PreviewStyleChange {
        target_id: fallback_target.to_owned(),
        selector: None,
        property: caps.get(1).map(|m| m.as_str()).unwrap_or("").to_owned(),
        previous_value: if previous == "(unset)" {
            String::new()
        } else {
            previous.to_owned()
        },
        value: caps.get(3).map(|m| m.as_str()).unwrap_or("").to_owned(),
    })
}

pub fn is_same_composer_context_payload(
    left: &ComposerContextRecord,
    right: &ComposerContextRecord,
) -> bool {
    if left.kind() != right.kind() {
        return false;
    }
    stable_payload(left) == stable_payload(right)
}

fn stable_payload(record: &ComposerContextRecord) -> String {
    let mut value = serde_json::to_value(record).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("label");
        object.remove("contextId");
    }
    serde_json::to_string(&sort_deep(value)).unwrap_or_default()
}

/// Wire record → draft store entry for context paste import.
pub fn composer_context_draft_from_record(
    record: &ComposerContextRecord,
    thread_id: ThreadId,
) -> Option<crate::composer::views::composer_context_presentation::ComposerDraftContextRecord> {
    use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};
    use crate::composer::views::composer_context_presentation::ComposerDraftContextRecord;

    let known = as_known_context_record(record)?;
    Some(match known {
        KnownComposerContextRecord::Terminal(record) => {
            ComposerDraftContextRecord::Terminal(terminal_context_draft_from_record(
                record, thread_id,
            ))
        }
        KnownComposerContextRecord::ReviewComment(record) => {
            ComposerDraftContextRecord::ReviewComment(review_comment_from_record(record))
        }
        KnownComposerContextRecord::Thread(record) => {
            ComposerDraftContextRecord::Thread(record.clone())
        }
        KnownComposerContextRecord::PreviewAnnotation(record) => {
            ComposerDraftContextRecord::PreviewAnnotation {
                record: preview_annotation_from_record(record),
                screenshot: None,
            }
        }
        KnownComposerContextRecord::Image(record) => ComposerDraftContextRecord::Image {
            record: ComposerImageAttachment::new(
                producer_id_from_composer_context_id("image", record.context_id.as_str()),
                record.name.as_str(),
                record.mime_type.as_str(),
                Vec::new(),
            ),
            upload: None,
        },
        KnownComposerContextRecord::File(record) => ComposerDraftContextRecord::File {
            record: ComposerFileAttachment {
                id: producer_id_from_composer_context_id("file", record.context_id.as_str()),
                name: record.name.to_string(),
                mime_type: record.mime_type.to_string(),
                size_bytes: record.size_bytes,
                data: None,
                uploaded_attachment_id: Some(record.attachment_id.as_str().to_owned()),
                upload_environment_id: None,
            },
            upload: None,
        },
        KnownComposerContextRecord::Element(_)
        | KnownComposerContextRecord::Mention(_)
        | KnownComposerContextRecord::Skill(_) => return None,
    })
}

fn sort_deep(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_deep).collect())
        }
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, nested)| (key, sort_deep(nested)))
                    .collect(),
            )
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "composer_context_records_tests.rs"]
mod tests;
