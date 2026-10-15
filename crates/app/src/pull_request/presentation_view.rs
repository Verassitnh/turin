//! The JSX half of upstream/apps/web/src/components/pullRequest/pullRequestPresentation.tsx
//! and pullRequestIcons.tsx: the glyphs, tags and avatars every pull request surface
//! assembles. The pure half (labels, tones, rollups) is `pull_request_presentation`; its Tailwind
//! tone strings are resolved to theme colors by [`tone_color`].

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, Rems, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled,
    Transformation, Window, div, percentage, svg, px, rems, rgb,
};
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheckStatus, PullRequestChecksState, PullRequestLabel,
    PullRequestMergeability, PullRequestReviewDecision, PullRequestState,
};

use super::pull_request_list_logic::pull_request_label_color;
use super::pull_request_presentation::{
    PullRequestReviewOutcome, pull_request_checks_state_presentation,
    pull_request_review_outcome_label, pull_request_review_outcome_tone_class_name,
    resolve_pull_request_conflict, resolve_pull_request_state,
};
use crate::state::helpers::diff_count::format_diff_count;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_2XS, TEXT_3XS, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Badge, BadgeSize, BadgeVariant, Icon, IconName, Tooltip};

// ── Tailwind tone strings → theme colors ────────────────────────────────────

/// Tailwind v4 palette entries the tone strings name.
fn palette(name: &str, shade: u16) -> Option<Hsla> {
    let hex = match (name, shade) {
        ("emerald", 300) => 0x5ee9b5,
        ("emerald", 400) => 0x00d492,
        ("emerald", 500) => 0x00bc7d,
        ("emerald", 600) => 0x009966,
        ("zinc", 400) => 0x9f9fa9,
        ("zinc", 500) => 0x71717b,
        ("red", 300) => 0xffa2a2,
        ("red", 600) => 0xe7000b,
        ("violet", 300) => 0xc4b4ff,
        ("violet", 600) => 0x7f22fe,
        ("amber", 400) => 0xffb900,
        ("amber", 500) => 0xfe9a00,
        ("amber", 600) => 0xe17100,
        _ => return None,
    };
    Some(rgb(hex).into())
}

/// Resolves one `text-*` utility (`text-emerald-600`, `text-muted-foreground/70`,
/// `text-destructive`) against the theme. `None` for anything else.
fn text_utility(utility: &str, theme: &AppTheme) -> Option<Hsla> {
    let rest = utility.strip_prefix("text-")?;
    let (name, alpha) = match rest.split_once('/') {
        Some((name, alpha)) => (name, alpha.parse::<f32>().ok().map(|a| a / 100.)),
        None => (rest, None),
    };
    let base = match name {
        "destructive" => theme.destructive,
        "muted-foreground" => theme.muted_foreground,
        "foreground" => theme.foreground,
        "primary" => theme.primary,
        other => {
            let (palette_name, shade) = other.rsplit_once('-')?;
            palette(palette_name, shade.parse().ok()?)?
        }
    };
    Some(match alpha {
        Some(alpha) => with_alpha(base, alpha).into(),
        None => base,
    })
}

/// The color of a presentation tone string such as
/// `"text-emerald-600 dark:text-emerald-300/90"`: the `dark:` utility wins in a dark theme.
pub fn tone_color(tone: &str, theme: &AppTheme) -> Hsla {
    let mut light = None;
    let mut dark = None;
    for utility in tone.split_whitespace() {
        match utility.strip_prefix("dark:") {
            Some(utility) => dark = text_utility(utility, theme),
            None => light = text_utility(utility, theme),
        }
    }
    let chosen = if theme.is_dark() {
        dark.or(light)
    } else {
        light
    };
    chosen.unwrap_or(theme.muted_foreground)
}

fn glyph_icon(glyph: &str) -> IconName {
    match glyph {
        "draft" => IconName::GitPullRequestDraft,
        "closed" => IconName::GitPullRequestClosed,
        "merged" => IconName::GitMerge,
        "conflicting" => IconName::TriangleAlert,
        "circle-check" => IconName::CircleCheck,
        "circle-x" => IconName::CircleX,
        "circle-dot" => IconName::CircleDot,
        "stack" => IconName::Layers,
        "link" => IconName::Link2,
        "unlink" => IconName::Unlink2,
        _ => IconName::GitPullRequestArrow,
    }
}

// ── Tooltip helper ──────────────────────────────────────────────────────────

/// A span that shows `text` on hover (`Tooltip` > `TooltipTrigger render={<span/>}`).
pub fn with_tooltip(
    id: impl Into<gpui_kit::ElementId>,
    text: impl Into<SharedString>,
    child: impl IntoElement,
) -> impl IntoElement {
    let text: SharedString = text.into();
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .child(child)
        .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
}

// ── Spinner ─────────────────────────────────────────────────────────────────

/// `ui/spinner.tsx`: a loader glyph turning once a second.
#[derive(IntoElement)]
pub struct Spinner {
    size: Rems,
    color: Option<Hsla>,
    id: SharedString,
}

impl Spinner {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            size: rems(1.),
            color: None,
            id: id.into(),
        }
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.app_theme().muted_foreground);
        svg()
            .path(IconName::LoaderCircle.path())
            .flex_shrink_0()
            .size(self.size)
            .text_color(color)
            .with_animation(
                SharedString::from(format!("spin-{}", self.id)),
                Animation::new(Duration::from_secs(1)).repeat(),
                |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
            )
    }
}

// ── State / conflict glyphs ─────────────────────────────────────────────────

/// `PullRequestStateGlyph`: the lifecycle icon at `size-4`.
#[derive(IntoElement)]
pub struct PullRequestStateGlyph {
    state: PullRequestState,
    is_draft: bool,
    size: Rems,
}

impl PullRequestStateGlyph {
    pub fn new(state: PullRequestState, is_draft: bool) -> Self {
        Self {
            state,
            is_draft,
            size: rems(1.),
        }
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for PullRequestStateGlyph {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let presentation = resolve_pull_request_state(self.state, self.is_draft);
        let color = tone_color(presentation.tone_class_name, theme);
        with_tooltip(
            "pr-state-glyph",
            presentation.label,
            Icon::new(glyph_icon(presentation.glyph))
                .size(self.size)
                .color(color),
        )
    }
}

/// `PullRequestConflictGlyph`: the warning that rides the state glyph's corner.
#[derive(IntoElement)]
pub struct PullRequestConflictGlyph {
    state: PullRequestState,
    is_draft: bool,
    mergeability: Option<PullRequestMergeability>,
    base_branch: Option<SharedString>,
    size: Rems,
}

impl PullRequestConflictGlyph {
    pub fn new(
        state: PullRequestState,
        is_draft: bool,
        mergeability: Option<PullRequestMergeability>,
        base_branch: Option<SharedString>,
    ) -> Self {
        Self {
            state,
            is_draft,
            mergeability,
            base_branch,
            size: rems(1.),
        }
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for PullRequestConflictGlyph {
    fn render(self, _: &mut Window, cx: &mut App) -> AnyElement {
        let theme = cx.app_theme();
        let Some(presentation) = resolve_pull_request_conflict(
            self.state,
            self.is_draft,
            self.mergeability,
            self.base_branch.as_deref(),
        ) else {
            return div().into_any_element();
        };
        let color = tone_color(presentation.tone_class_name, theme);
        with_tooltip(
            "pr-conflict-glyph",
            presentation.label,
            Icon::new(glyph_icon(presentation.glyph))
                .size(self.size)
                .color(color),
        )
        .into_any_element()
    }
}

// ── Checks and reviews ──────────────────────────────────────────────────────

/// Tone and icon of one check's status (`CHECK_STATUS_PRESENTATION`).
pub fn check_status_icon(status: PullRequestCheckStatus, theme: &AppTheme) -> (IconName, Hsla) {
    match status {
        PullRequestCheckStatus::Pending => (IconName::LoaderCircle, tone_color("text-amber-500", theme)),
        PullRequestCheckStatus::ActionRequired => (
            IconName::CircleDot,
            tone_color("text-amber-600 dark:text-amber-400/90", theme),
        ),
        PullRequestCheckStatus::Success => (
            IconName::CircleCheck,
            tone_color("text-emerald-600 dark:text-emerald-300/90", theme),
        ),
        PullRequestCheckStatus::Failure | PullRequestCheckStatus::Cancelled => {
            (IconName::CircleX, theme.destructive)
        }
        PullRequestCheckStatus::Skipped | PullRequestCheckStatus::Neutral => (
            IconName::CircleDashed,
            tone_color("text-muted-foreground/70", theme),
        ),
    }
}

/// `PullRequestCheckStatusIcon`: `size-3.5`; a running check turns.
#[derive(IntoElement)]
pub struct PullRequestCheckStatusIcon {
    status: PullRequestCheckStatus,
    id: SharedString,
}

impl PullRequestCheckStatusIcon {
    pub fn new(id: impl Into<SharedString>, status: PullRequestCheckStatus) -> Self {
        Self {
            status,
            id: id.into(),
        }
    }
}

impl RenderOnce for PullRequestCheckStatusIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> AnyElement {
        let (icon, color) = check_status_icon(self.status, cx.app_theme());
        if self.status == PullRequestCheckStatus::Pending {
            Spinner::new(self.id)
                .size(rems(0.875))
                .color(color)
                .into_any_element()
        } else {
            Icon::new(icon)
                .size(rems(0.875))
                .color(color)
                .into_any_element()
        }
    }
}

/// The rollup a listing row carries, as a `size-3.5` glyph with its headline on hover.
#[derive(IntoElement)]
pub struct PullRequestChecksStateGlyph {
    state: PullRequestChecksState,
    size: Rems,
}

impl PullRequestChecksStateGlyph {
    pub fn new(state: PullRequestChecksState) -> Self {
        Self {
            state,
            size: rems(0.875),
        }
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for PullRequestChecksStateGlyph {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let presentation = pull_request_checks_state_presentation(self.state);
        let color = tone_color(presentation.tone_class_name, theme);
        with_tooltip(
            "pr-checks-state-glyph",
            presentation.label,
            Icon::new(glyph_icon(presentation.glyph))
                .size(self.size)
                .color(color),
        )
    }
}

/// `PullRequestReviewDecisionGlyph`.
#[derive(IntoElement)]
pub struct PullRequestReviewDecisionGlyph {
    decision: PullRequestReviewDecision,
}

impl PullRequestReviewDecisionGlyph {
    pub fn new(decision: PullRequestReviewDecision) -> Self {
        Self { decision }
    }
}

impl RenderOnce for PullRequestReviewDecisionGlyph {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let (icon, label, color) = match self.decision {
            PullRequestReviewDecision::Approved => (
                IconName::UserCheck,
                "Approved",
                tone_color("text-emerald-600 dark:text-emerald-300/90", theme),
            ),
            PullRequestReviewDecision::ChangesRequested => (
                IconName::UserRoundX,
                "Changes requested",
                tone_color("text-amber-600/90 dark:text-amber-400/80", theme),
            ),
            PullRequestReviewDecision::ReviewRequired => (
                IconName::UserRound,
                "Awaiting review",
                tone_color("text-muted-foreground/60", theme),
            ),
        };
        with_tooltip(
            "pr-review-decision-glyph",
            label,
            Icon::new(icon).size(rems(0.875)).color(color),
        )
    }
}

/// Tone and icon of a review verdict (`REVIEW_OUTCOME_PRESENTATION`).
pub fn review_outcome_icon(outcome: PullRequestReviewOutcome, theme: &AppTheme) -> (IconName, Hsla) {
    let icon = match outcome {
        PullRequestReviewOutcome::Approved => IconName::CircleCheck,
        PullRequestReviewOutcome::ChangesRequested => IconName::CircleX,
        PullRequestReviewOutcome::Dismissed => IconName::CircleDashed,
    };
    (
        icon,
        tone_color(pull_request_review_outcome_tone_class_name(outcome), theme),
    )
}

/// `PullRequestReviewOutcomeBadge`.
#[derive(IntoElement)]
pub struct PullRequestReviewOutcomeBadge {
    outcome: PullRequestReviewOutcome,
}

impl PullRequestReviewOutcomeBadge {
    pub fn new(outcome: PullRequestReviewOutcome) -> Self {
        Self { outcome }
    }
}

impl RenderOnce for PullRequestReviewOutcomeBadge {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let variant = match self.outcome {
            PullRequestReviewOutcome::Approved => BadgeVariant::Success,
            PullRequestReviewOutcome::ChangesRequested => BadgeVariant::Error,
            PullRequestReviewOutcome::Dismissed => BadgeVariant::Outline,
        };
        Badge::new()
            .size(BadgeSize::Sm)
            .variant(variant)
            .child(Icon::new(match self.outcome {
                PullRequestReviewOutcome::Approved => IconName::CircleCheck,
                PullRequestReviewOutcome::ChangesRequested => IconName::CircleX,
                PullRequestReviewOutcome::Dismissed => IconName::CircleDashed,
            })
            .size(rems(0.75)))
            .child(pull_request_review_outcome_label(self.outcome))
    }
}

// ── Labels, actors, stats ───────────────────────────────────────────────────

/// `PullRequestLabelChip`: a flat tinted tag in the label's own color.
#[derive(IntoElement)]
pub struct PullRequestLabelChip {
    label: PullRequestLabel,
    size: BadgeSize,
    max_width: Option<Pixels>,
    trailing: Option<SharedString>,
}

impl PullRequestLabelChip {
    pub fn new(label: PullRequestLabel) -> Self {
        Self {
            label,
            size: BadgeSize::Sm,
            max_width: Some(px(160.)),
            trailing: None,
        }
    }

    pub fn size(mut self, size: BadgeSize) -> Self {
        self.size = size;
        self
    }

    /// `className="max-w-48"`: the widest the pill may grow before the name is cut.
    pub fn max_width(mut self, width: Pixels) -> Self {
        self.max_width = Some(width);
        self
    }

    /// Children ride after the name, for an overflow count.
    pub fn trailing(mut self, text: impl Into<SharedString>) -> Self {
        self.trailing = Some(text.into());
        self
    }
}

impl RenderOnce for PullRequestLabelChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let color = pull_request_label_color(self.label.color.as_deref());
        let variant = match color {
            Some(hex) => parse_hex(&hex).map_or(BadgeVariant::Secondary, BadgeVariant::Label),
            None => BadgeVariant::Secondary,
        };
        Badge::new()
            .size(self.size)
            .variant(variant)
            .justify_start()
            .min_w_0()
            .flex_shrink_1()
            .when_some(self.max_width, |this, width| this.max_w(width))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(self.label.name.as_str().to_owned())),
            )
            .when_some(self.trailing, |this, text| this.child(text))
    }
}

fn parse_hex(hex: &str) -> Option<Hsla> {
    let digits = hex.strip_prefix('#')?;
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some(rgb(value).into())
}

/// `PullRequestActorAvatar`: the host's avatar, or the login's initial on a muted disc.
/// Avatar images are not fetched (no network in the view layer), so the initial is always shown.
#[derive(IntoElement)]
pub struct PullRequestActorAvatar {
    login: SharedString,
    size: Pixels,
}

impl PullRequestActorAvatar {
    pub fn new(actor: Option<&PullRequestActor>) -> Self {
        Self {
            login: SharedString::from(actor.map_or("ghost", |actor| actor.login.as_str()).to_owned()),
            size: px(16.),
        }
    }

    /// `size-3.5` and the like.
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for PullRequestActorAvatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let initial: String = self.login.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(self.size)
            .rounded_full()
            .bg(theme.muted)
            .text_color(theme.muted_foreground)
            .text_size(TEXT_3XS.size)
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .child(initial)
    }
}

/// The text a hover on an actor shows: `Name (@login)` or the login.
pub fn actor_tooltip(actor: Option<&PullRequestActor>) -> String {
    match actor {
        Some(actor) => match actor.name.as_deref() {
            Some(name) if name != actor.login.as_str() => format!("{name} (@{})", actor.login),
            _ => actor.login.as_str().to_owned(),
        },
        None => "ghost".to_owned(),
    }
}

/// `PullRequestActorLabel`: an actor as a login beside its avatar, or the avatar alone.
#[derive(IntoElement)]
pub struct PullRequestActorLabel {
    actor: Option<PullRequestActor>,
    avatar_only: bool,
    tooltip: bool,
}

impl PullRequestActorLabel {
    pub fn new(actor: Option<PullRequestActor>) -> Self {
        Self {
            actor,
            avatar_only: false,
            tooltip: true,
        }
    }

    pub fn avatar_only(mut self) -> Self {
        self.avatar_only = true;
        self
    }

    pub fn tooltip(mut self, tooltip: bool) -> Self {
        self.tooltip = tooltip;
        self
    }
}

impl RenderOnce for PullRequestActorLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let login = SharedString::from(
            self.actor
                .as_ref()
                .map_or("ghost", |actor| actor.login.as_str())
                .to_owned(),
        );
        let tooltip_text = SharedString::from(actor_tooltip(self.actor.as_ref()));
        let body = div()
            .flex()
            .min_w_0()
            .items_center()
            .when(!self.avatar_only, |this| this.gap(rems(0.375)))
            .child(PullRequestActorAvatar::new(self.actor.as_ref()))
            .when(!self.avatar_only, |this| {
                this.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(login),
                )
            });
        let element = div().id("pr-actor-label").flex().min_w_0().flex_shrink_1().child(body);
        if self.tooltip {
            element
                .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
                .into_any_element()
        } else {
            element.into_any_element()
        }
    }
}

/// `PullRequestDiffStat`: added and removed lines. Nothing when the host reported none.
#[derive(IntoElement)]
pub struct PullRequestDiffStat {
    additions: u64,
    deletions: u64,
    mono: bool,
}

impl PullRequestDiffStat {
    pub fn new(additions: u64, deletions: u64) -> Self {
        Self {
            additions,
            deletions,
            mono: false,
        }
    }

    /// `className="font-mono"`.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
}

impl RenderOnce for PullRequestDiffStat {
    fn render(self, _: &mut Window, cx: &mut App) -> AnyElement {
        if self.additions == 0 && self.deletions == 0 {
            return div().into_any_element();
        }
        let theme = cx.app_theme();
        div()
            .flex()
            .flex_shrink_0()
            .items_baseline()
            .gap_1()
            .when(self.mono, |this| this.font_family(theme.fonts.mono.clone()))
            .child(
                div()
                    .text_color(theme.diff_addition_foreground)
                    .child(format!("+{}", format_diff_count(self.additions as i64))),
            )
            .child(
                div()
                    .text_color(theme.diff_deletion_foreground)
                    .child(format!("-{}", format_diff_count(self.deletions as i64))),
            )
            .into_any_element()
    }
}

/// `PullRequestMetaLine`: dot-separated metadata. Draws a separator only between the segments
/// that survive, so callers can pass `None` for the ones they skip.
pub fn pull_request_meta_line(
    segments: impl IntoIterator<Item = Option<AnyElement>>,
    theme: &AppTheme,
) -> gpui_kit::Div {
    let mut line = div().flex().min_w_0().items_center().gap(rems(0.375));
    for (index, segment) in segments.into_iter().flatten().enumerate() {
        if index > 0 {
            line = line.child(
                div()
                    .flex_shrink_0()
                    .text_color(with_alpha(theme.muted_foreground, 0.5))
                    .child("·"),
            );
        }
        line = line.child(segment);
    }
    line
}

/// Text size of the dense row meta line (`text-2xs`).
pub fn meta_text_size() -> Pixels {
    px(TEXT_2XS.size.0 * 16.)
}

/// `text-xs`.
pub fn xs_text_size() -> Pixels {
    px(TEXT_XS.size.0 * 16.)
}
