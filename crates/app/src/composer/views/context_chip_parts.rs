//! Port of upstream/apps/web/src/components/contextChipParts.tsx
//!
//! The chips each context surface assembles: a static chip with a tooltip, a
//! chip that opens its details in a popover, the pull request / image / file
//! chips and the unresolved placeholder.
//!
//! Not ported: `PierreEntryIcon` (the file chip shows the generic `File`
//! glyph), `PullRequestLinkPreview` (the pull request chip's tooltip is the
//! text of `PullRequestContextDetails`), and the CORS retry of the image
//! thumbnail (the bytes are in memory).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Hsla, Image, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, RenderOnce, SharedString, Styled as _, StyledImage as _, Window, div, img,
    px, rems,
};
use turin_contracts::composer_context::PullRequestContextMetadata;

use crate::composer::attachment_files::format_attachment_size;
use crate::composer::composer_context_records::PullRequestContextDisplayState;
use crate::composer::inline_chip::{
    DEFAULT_ATTACHMENT_NAME_CHARS, middle_truncate_attachment_name,
};
use crate::composer::views::chrome::{BoundsCell, floating, glass_surface, track_bounds};
use crate::composer::views::composer_banner::WindowCallback;
use crate::composer::views::context_chip::{ContextChip, ContextChipKind, ContextChipState};
use crate::composer::views::image_thumbnail::gpui_image_format;
use crate::ui::IconName;

/// Builds the details a chip popover shows.
pub type DetailsBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// Opens a pull request by its URL.
pub type OpenUrl = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// `max-w-72`.
const LABEL_MAX_WIDTH: gpui_kit::Pixels = px(288.);

/// `PULL_REQUEST_CHIP_KINDS`: the chip kind of a pull request context in each
/// display state; `None` is the `unknown` state.
pub fn pull_request_chip_kind(state: Option<PullRequestContextDisplayState>) -> ContextChipKind {
    match state {
        Some(PullRequestContextDisplayState::Open) => ContextChipKind::PrOpen,
        Some(PullRequestContextDisplayState::Draft) => ContextChipKind::PrDraft,
        Some(PullRequestContextDisplayState::Merged) => ContextChipKind::PrMerged,
        Some(PullRequestContextDisplayState::Closed) => ContextChipKind::PrClosed,
        None => ContextChipKind::PullRequest,
    }
}

/// A static chip with an optional tooltip; each surface keeps ownership of
/// payload lookup. Newlines in the tooltip are kept.
#[derive(IntoElement)]
pub struct ContextChipShell {
    pub id: SharedString,
    pub icon: IconName,
    pub label: SharedString,
    pub kind: ContextChipKind,
    pub state: Option<ContextChipState>,
    pub tooltip: Option<SharedString>,
}

impl RenderOnce for ContextChipShell {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        ContextChip::new(self.id)
            .kind(self.kind)
            .state(self.state)
            .icon(self.icon)
            .label(self.label)
            .when_some(self.tooltip, |chip, tooltip| chip.tooltip(tooltip))
    }
}

/// The open state of a [`ContextChipPopover`] and where its chip painted.
struct PopoverState {
    open: bool,
    bounds: BoundsCell,
}

/// A chip that opens its details in a popover; the chip itself is the
/// trigger (`side="top"`, `width="lg"`, `padding="compact"`).
#[derive(IntoElement)]
pub struct ContextChipPopover {
    pub id: SharedString,
    pub kind: ContextChipKind,
    pub icon: IconName,
    pub label: SharedString,
    pub accessible_label: SharedString,
    pub details: DetailsBuilder,
}

impl RenderOnce for ContextChipPopover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| PopoverState {
            open: false,
            bounds: Rc::new(Cell::new(Default::default())),
        });
        let (open, bounds) = {
            let state = state.read(cx);
            (state.open, state.bounds.clone())
        };
        let toggle = state.clone();
        let backdrop_state = state;
        let chip = ContextChip::new(self.id.clone())
            .kind(self.kind)
            .icon(self.icon)
            .label(self.label)
            .on_click(move |_, _, cx| {
                toggle.update(cx, |state, cx| {
                    state.open = !state.open;
                    cx.notify();
                });
            });
        let mut root = div()
            .relative()
            .flex_shrink_0()
            .child(track_bounds(&bounds))
            .child(chip);
        if open {
            let content = (self.details)(window, cx);
            let surface = glass_surface(cx)
                .w(rems(24.))
                .max_w(window.viewport_size().width - rems(2.).to_pixels(window.rem_size()))
                .py_2()
                .px_3()
                .child(content);
            root = root
                .child(
                    div()
                        .id(SharedString::from(format!("{}-backdrop", self.id)))
                        .absolute()
                        .top(px(-4000.))
                        .left(px(-4000.))
                        .w(px(8000.))
                        .h(px(8000.))
                        .on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
                            backdrop_state.update(cx, |state, cx| {
                                state.open = false;
                                cx.notify();
                            });
                        }),
                )
                .child(floating(bounds.get(), true, false, surface));
        }
        root
    }
}

/// `PullRequestContextDetails` as text: number and state, title, branches.
pub fn pull_request_details_text(
    metadata: &PullRequestContextMetadata,
    state: PullRequestContextDisplayState,
) -> String {
    let label = match state {
        PullRequestContextDisplayState::Open => "Open",
        PullRequestContextDisplayState::Draft => "Draft",
        PullRequestContextDisplayState::Merged => "Merged",
        PullRequestContextDisplayState::Closed => "Closed",
    };
    format!(
        "Pull request #{} {label}\n{}\n{} → {}",
        metadata.number.get(),
        metadata.title,
        metadata.head_branch,
        metadata.base_branch
    )
}

fn pull_request_icon(state: PullRequestContextDisplayState) -> IconName {
    match state {
        PullRequestContextDisplayState::Open => IconName::GitPullRequestArrow,
        PullRequestContextDisplayState::Draft => IconName::GitPullRequestDraft,
        PullRequestContextDisplayState::Merged => IconName::GitMerge,
        PullRequestContextDisplayState::Closed => IconName::GitPullRequestClosed,
    }
}

#[derive(IntoElement)]
pub struct PullRequestChip {
    pub id: SharedString,
    pub metadata: PullRequestContextMetadata,
    pub label: SharedString,
    pub kind_label: String,
    pub kind: ContextChipKind,
    pub on_open: OpenUrl,
}

impl RenderOnce for PullRequestChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let display_state = {
            use turin_contracts::source_control::ChangeRequestState as State;
            match (self.metadata.state, self.metadata.is_draft) {
                (State::Open, true) => PullRequestContextDisplayState::Draft,
                (State::Open, false) => PullRequestContextDisplayState::Open,
                (State::Merged, _) => PullRequestContextDisplayState::Merged,
                (State::Closed, _) => PullRequestContextDisplayState::Closed,
            }
        };
        let url = self.metadata.url.clone();
        let on_open = self.on_open;
        ContextChip::new(self.id)
            .kind(self.kind)
            .icon(pull_request_icon(display_state))
            .label(self.label)
            .tooltip(pull_request_details_text(&self.metadata, display_state))
            .on_click(move |_, window, cx| on_open(&url, window, cx))
    }
}

/// `averageImageColor` over a 16x16 RGBA sample: alpha-weighted, so
/// transparent pixels do not darken the accent. `None` when fully transparent.
pub fn average_image_color(rgba: &[u8]) -> Option<(u8, u8, u8)> {
    let (mut red, mut green, mut blue, mut alpha) = (0u64, 0u64, 0u64, 0u64);
    for pixel in rgba.as_chunks::<4>().0 {
        let weight = u64::from(pixel[3]);
        red += u64::from(pixel[0]) * weight;
        green += u64::from(pixel[1]) * weight;
        blue += u64::from(pixel[2]) * weight;
        alpha += weight;
    }
    if alpha == 0 {
        return None;
    }
    let mean = |sum: u64| ((sum as f64) / (alpha as f64)).round() as u8;
    Some((mean(red), mean(green), mean(blue)))
}

/// Sample the image once, as upstream does on load: draw it into a 16x16
/// surface and average it. Undecodable bytes keep the default image tone.
pub fn sample_image_accent(bytes: &[u8]) -> Option<Hsla> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let small = decoded
        .resize_exact(16, 16, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let (red, green, blue) = average_image_color(small.as_raw())?;
    Some(gpui_kit::rgb(u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue)).into())
}

/// The bytes behind an image chip's thumbnail.
#[derive(Clone)]
pub struct ImageChipPreview {
    pub id: String,
    pub mime_type: String,
    pub bytes: Arc<Vec<u8>>,
}

struct AccentSample {
    accent: Option<Hsla>,
    started: bool,
}

#[derive(IntoElement)]
pub struct ImageChipButton {
    pub id: SharedString,
    pub name: String,
    pub preview: Option<ImageChipPreview>,
    /// Every attachment chip reports its size; images are no exception.
    pub size: String,
    pub suffix: Option<String>,
    pub on_click: WindowCallback,
}

impl RenderOnce for ImageChipButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sample = window.use_keyed_state(
            SharedString::from(format!(
                "{}-accent-{}",
                self.id,
                self.preview.as_ref().map_or("none", |p| p.id.as_str())
            )),
            cx,
            |_, _| AccentSample {
                accent: None,
                started: false,
            },
        );
        if let Some(preview) = self.preview.as_ref()
            && !sample.read(cx).started
        {
            sample.update(cx, |sample, _| sample.started = true);
            let bytes = preview.bytes.clone();
            let weak = sample.downgrade();
            cx.spawn(async move |cx| {
                let accent = cx
                    .background_spawn(async move { sample_image_accent(&bytes) })
                    .await;
                weak.update(cx, |sample, cx| {
                    sample.accent = accent;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let accent = sample.read(cx).accent;
        let thumbnail: AnyElement = match self.preview.as_ref().and_then(|preview| {
            gpui_image_format(&preview.mime_type).map(|format| (preview, format))
        }) {
            Some((preview, format)) => img(Arc::new(Image::from_bytes(
                format,
                preview.bytes.as_ref().clone(),
            )))
            .size(rems(1.17 * 0.86 * 14. / 16.))
            .flex_shrink_0()
            .rounded(px(4.))
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
            None => div().into_any_element(),
        };
        let has_preview = self.preview.is_some();
        let on_click = self.on_click;
        let mut chip = ContextChip::new(self.id)
            .kind(ContextChipKind::Image)
            .accent(accent);
        chip = if has_preview {
            chip.element(thumbnail)
        } else {
            chip.icon(IconName::Image)
        };
        chip = chip
            .label_max(
                middle_truncate_attachment_name(&self.name, DEFAULT_ATTACHMENT_NAME_CHARS),
                LABEL_MAX_WIDTH,
            )
            .meta(self.size);
        if let Some(suffix) = self.suffix {
            chip = chip.meta(suffix);
        }
        chip.on_click(move |_, window, cx| on_click(window, cx))
    }
}

#[derive(IntoElement)]
pub struct FileChip {
    pub id: SharedString,
    pub name: String,
    pub size: String,
    pub is_video: bool,
    pub tooltip: String,
    pub suffix: Option<String>,
    pub disabled: bool,
    pub error: bool,
    pub unresolved: bool,
    pub on_open: Option<WindowCallback>,
}

impl RenderOnce for FileChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let state = if self.unresolved {
            Some(ContextChipState::Unresolved)
        } else if self.error {
            Some(ContextChipState::Invalid)
        } else {
            None
        };
        let mut chip = ContextChip::new(self.id)
            .kind(if self.is_video {
                ContextChipKind::Video
            } else {
                ContextChipKind::File
            })
            .state(state)
            .icon(if self.is_video {
                IconName::Film
            } else {
                IconName::File
            })
            .label_max(
                middle_truncate_attachment_name(&self.name, DEFAULT_ATTACHMENT_NAME_CHARS),
                LABEL_MAX_WIDTH,
            )
            .meta(self.size)
            .tooltip(self.tooltip)
            .disabled(self.disabled);
        if let Some(suffix) = self.suffix {
            chip = chip.meta(suffix);
        }
        if let Some(on_open) = self.on_open {
            chip = chip.on_click(move |_, window, cx| on_open(window, cx));
        }
        chip
    }
}

/// `UnresolvedChip`: the placeholder for context that is no longer available.
pub fn unresolved_chip(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    tooltip: impl Into<SharedString>,
) -> ContextChipShell {
    ContextChipShell {
        id: id.into(),
        icon: IconName::CircleDashed,
        label: label.into(),
        kind: ContextChipKind::Neutral,
        state: Some(ContextChipState::Unresolved),
        tooltip: Some(tooltip.into()),
    }
}

/// The size label an attachment chip reports.
pub fn attachment_chip_size(size_bytes: u64) -> String {
    format_attachment_size(size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_each_display_state_to_its_chip_kind() {
        assert_eq!(
            pull_request_chip_kind(Some(PullRequestContextDisplayState::Open)),
            ContextChipKind::PrOpen
        );
        assert_eq!(
            pull_request_chip_kind(Some(PullRequestContextDisplayState::Draft)),
            ContextChipKind::PrDraft
        );
        assert_eq!(
            pull_request_chip_kind(Some(PullRequestContextDisplayState::Merged)),
            ContextChipKind::PrMerged
        );
        assert_eq!(
            pull_request_chip_kind(Some(PullRequestContextDisplayState::Closed)),
            ContextChipKind::PrClosed
        );
        assert_eq!(pull_request_chip_kind(None), ContextChipKind::PullRequest);
    }

    #[test]
    fn averages_an_image_by_alpha_so_transparent_pixels_do_not_darken_it() {
        // One opaque red pixel and one fully transparent black pixel.
        let rgba = [200, 20, 40, 255, 0, 0, 0, 0];
        assert_eq!(average_image_color(&rgba), Some((200, 20, 40)));
    }

    #[test]
    fn a_fully_transparent_image_has_no_average() {
        assert_eq!(average_image_color(&[1, 2, 3, 0, 4, 5, 6, 0]), None);
    }

    #[test]
    fn weights_partly_transparent_pixels() {
        // 255 and 85 alpha: the opaque pixel carries three quarters of the mean.
        let rgba = [100, 0, 0, 255, 200, 0, 0, 85];
        assert_eq!(average_image_color(&rgba), Some((125, 0, 0)));
    }

    #[test]
    fn undecodable_bytes_keep_the_default_tone() {
        assert!(sample_image_accent(b"not an image").is_none());
    }
}
