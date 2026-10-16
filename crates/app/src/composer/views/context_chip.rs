//! Port of upstream/apps/web/src/components/ContextChip.tsx
//!
//! The inline pill for one piece of chat context (a file, mention, terminal
//! excerpt, pull request, skill...). Metrics are in em of the chip's own font,
//! which is `0.86em` of the text around it, so the chip scales with the
//! prompt font size. Each kind keeps one restrained color identity: every
//! accent shares a lightness (0.62) and only hue (and a little chroma)
//! carries identity.
//!
//! Interactivity follows from `on_click`: a clickable chip gets the pointer
//! cursor and the hover tint; a plain one is static. Not ported: the
//! `focus-visible` outline (GPUI chips are not focusable), `useRender`'s
//! `render` prop (wrap the chip instead) and `transition-colors`.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, relative, rems,
};

use crate::theme::color::{MixSpace, color, color_mix, with_alpha};
use crate::theme::tokens::TEXT_3XS;
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName, Tooltip};

/// `contextChipVariants` `kind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ContextChipKind {
    #[default]
    Neutral,
    Image,
    Video,
    File,
    Mention,
    Terminal,
    Element,
    PreviewAnnotation,
    ReviewComment,
    PullRequest,
    PrOpen,
    PrDraft,
    PrMerged,
    PrClosed,
    Skill,
    Thread,
    Citation,
}

impl ContextChipKind {
    /// The `--context-chip-accent` of the kind; `None` is `neutral`.
    pub fn accent(self) -> Option<Hsla> {
        let value = match self {
            Self::Neutral => return None,
            Self::Image => "oklch(0.62 0.16 16)",
            Self::Video => "oklch(0.62 0.16 48)",
            Self::File => "oklch(0.62 0.136 237)",
            Self::Mention => "oklch(0.62 0.11 215)",
            Self::Terminal => "oklch(0.62 0.134 163)",
            Self::Element | Self::PreviewAnnotation => "oklch(0.62 0.134 70)",
            Self::ReviewComment => "oklch(0.62 0.16 292)",
            Self::PullRequest => "oklch(0.62 0.16 277)",
            Self::PrOpen => "oklch(0.62 0.134 163)",
            Self::PrDraft => "oklch(0.62 0.02 259)",
            Self::PrMerged => "oklch(0.62 0.16 292)",
            Self::PrClosed => "oklch(0.62 0.16 16)",
            Self::Skill => "oklch(0.62 0.16 322)",
            Self::Thread => "oklch(0.62 0.12 190)",
            Self::Citation => "oklch(0.62 0.16 259)",
        };
        Some(color(value))
    }

    /// The chip kind of a composer context reference kind (`image`, `file`,
    /// `terminal`...); an unknown kind reads as `neutral`.
    pub fn from_context_kind(kind: &str) -> Self {
        match kind {
            "image" => Self::Image,
            "video" => Self::Video,
            "file" => Self::File,
            "mention" => Self::Mention,
            "terminal" => Self::Terminal,
            "element" => Self::Element,
            "preview-annotation" => Self::PreviewAnnotation,
            "review-comment" => Self::ReviewComment,
            "pull-request" => Self::PullRequest,
            "skill" => Self::Skill,
            "thread" => Self::Thread,
            "citation" => Self::Citation,
            _ => Self::Neutral,
        }
    }
}

/// `contextChipVariants` `state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextChipState {
    /// `border-dashed`, plain foreground text.
    Unresolved,
    /// Destructive tint.
    Invalid,
}

/// The resolved colors of a chip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextChipColors {
    pub border: Hsla,
    pub border_hover: Hsla,
    pub background: Hsla,
    pub background_hover: Hsla,
    pub foreground: Hsla,
    pub dashed: bool,
}

/// The compound variants: kind colors first, state colors win over any kind.
pub fn context_chip_colors(
    kind: ContextChipKind,
    state: Option<ContextChipState>,
    theme: &AppTheme,
) -> ContextChipColors {
    let mut colors = match kind.accent() {
        // `neutral`: `border-border/70 bg-accent/40 text-foreground`.
        None => ContextChipColors {
            border: with_alpha(theme.border, 0.7).into(),
            border_hover: with_alpha(theme.border, 0.7).into(),
            background: with_alpha(theme.accent, 0.4).into(),
            background_hover: with_alpha(theme.accent, 0.4).into(),
            foreground: theme.foreground,
            dashed: false,
        },
        // `--context-chip-border`: accent 34% over the contrast border (48% on
        // hover); `--context-chip-foreground`: accent 22% over the contrast
        // foreground; `bg-(--context-chip-accent)/11` (`/17` on hover).
        Some(accent) => ContextChipColors {
            border: color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into(),
            border_hover: color_mix(MixSpace::Oklab, accent, 0.48, theme.border).into(),
            background: with_alpha(accent, 0.11).into(),
            background_hover: with_alpha(accent, 0.17).into(),
            foreground: color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into(),
            dashed: false,
        },
    };
    match state {
        Some(ContextChipState::Unresolved) => {
            colors.dashed = true;
            colors.foreground = theme.foreground;
        }
        Some(ContextChipState::Invalid) => {
            let border = with_alpha(theme.destructive, 0.35).into();
            colors.border = border;
            colors.border_hover = border;
            colors.background = with_alpha(theme.destructive, 0.08).into();
            colors.background_hover = colors.background;
            colors.foreground = theme.destructive;
        }
        None => {}
    }
    colors
}

/// One child of a chip, laid out by [`ContextChip`].
enum Part {
    /// `[&_svg]:size-[1.17em]`, tinted with the chip's foreground.
    Icon(IconName),
    /// A custom leading element already sized to `1.17em` (an image).
    Element(AnyElement),
    /// `ContextChipLabel`: truncates to the chip's width.
    Label {
        text: SharedString,
        max_width: Option<Pixels>,
    },
    /// `text-3xs text-current`, never shrinks (sizes, upload status).
    Meta(SharedString),
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ContextChip {
    id: ElementId,
    kind: ContextChipKind,
    state: Option<ContextChipState>,
    /// The font size of the text the chip sits in; the chip is `0.86em` of it.
    parent_size: Pixels,
    accent: Option<Hsla>,
    parts: Vec<Part>,
    on_click: Option<ClickHandler>,
    disabled: bool,
    tooltip: Option<SharedString>,
}

impl ContextChip {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            kind: ContextChipKind::Neutral,
            state: None,
            // The composer's prompt text (`text-sm`).
            parent_size: px(14.),
            accent: None,
            parts: Vec::new(),
            on_click: None,
            disabled: false,
            tooltip: None,
        }
    }

    pub fn kind(mut self, kind: ContextChipKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn state(mut self, state: Option<ContextChipState>) -> Self {
        self.state = state;
        self
    }

    pub fn parent_size(mut self, size: Pixels) -> Self {
        self.parent_size = size;
        self
    }

    /// `--context-chip-accent` overridden through `style`, e.g. an image's
    /// average color.
    pub fn accent(mut self, accent: Option<Hsla>) -> Self {
        self.accent = accent;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.parts.push(Part::Icon(icon));
        self
    }

    pub fn element(mut self, element: impl IntoElement) -> Self {
        self.parts.push(Part::Element(element.into_any_element()));
        self
    }

    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.parts.push(Part::Label {
            text: text.into(),
            max_width: None,
        });
        self
    }

    /// `ContextChipLabel className="max-w-72"` and friends.
    pub fn label_max(mut self, text: impl Into<SharedString>, max_width: Pixels) -> Self {
        self.parts.push(Part::Label {
            text: text.into(),
            max_width: Some(max_width),
        });
        self
    }

    pub fn meta(mut self, text: impl Into<SharedString>) -> Self {
        self.parts.push(Part::Meta(text.into()));
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// `disabled:cursor-default`: the click is ignored.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }
}

impl RenderOnce for ContextChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let mut colors = context_chip_colors(self.kind, self.state, &theme);
        if let (Some(accent), None) = (self.accent, self.state) {
            colors = ContextChipColors {
                border: color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into(),
                border_hover: color_mix(MixSpace::Oklab, accent, 0.48, theme.border).into(),
                background: with_alpha(accent, 0.11).into(),
                background_hover: with_alpha(accent, 0.17).into(),
                foreground: color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into(),
                dashed: false,
            };
        }
        let em = self.parent_size * 0.86;
        let interactive = self.on_click.is_some() && !self.disabled;
        let foreground = colors.foreground;
        let children: Vec<AnyElement> = self
            .parts
            .into_iter()
            .map(|part| match part {
                Part::Icon(icon) => Icon::new(icon)
                    .size(rems(f32::from(em * 1.17) / 16.))
                    .color(foreground)
                    .into_any_element(),
                Part::Element(element) => element,
                Part::Label { text, max_width } => div()
                    .min_w_0()
                    .when_some(max_width, |this, width| this.max_w(width))
                    .truncate()
                    .line_height(relative(1.375))
                    .child(text)
                    .into_any_element(),
                Part::Meta(text) => div()
                    .flex_shrink_0()
                    .text_size(TEXT_3XS.size)
                    .child(text)
                    .into_any_element(),
            })
            .collect();
        let tooltip = self.tooltip;
        let handler = self.on_click;
        div()
            .id(self.id)
            .flex()
            .flex_shrink_0()
            .max_w_full()
            .h(em * 1.41)
            .items_center()
            .gap(em * 0.33)
            .rounded(em * 0.5)
            .border_1()
            .when(colors.dashed, |this| this.border_dashed())
            .border_color(colors.border)
            .bg(colors.background)
            .text_color(foreground)
            .px(em * 0.5)
            .font_weight(FontWeight::MEDIUM)
            .text_size(em)
            .line_height(relative(1.))
            .when(interactive, |this| {
                let border_hover = colors.border_hover;
                let background_hover = colors.background_hover;
                this.cursor(CursorStyle::PointingHand)
                    .hover(move |style| style.border_color(border_hover).bg(background_hover))
            })
            .when(self.disabled, |this| this.cursor(CursorStyle::Arrow))
            .when_some(handler.filter(|_| !self.disabled), |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .when_some(tooltip, |this, text| {
                this.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            })
            .children(children)
    }
}

/// `ContextChipAction`: an icon action inside a chip, such as editing a
/// citation's comment. `ml-[0.17em] size-[1.17em] rounded-sm`, tinted with the
/// chip's kind (`hover:bg-(--context-chip-accent)/17`).
pub fn context_chip_action(
    id: impl Into<ElementId>,
    icon: IconName,
    parent_size: Pixels,
    kind: ContextChipKind,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let em = parent_size * 0.86;
    let hover = with_alpha(kind.accent().unwrap_or(theme.foreground), 0.17);
    let colors = context_chip_colors(kind, None, theme);
    div()
        .id(id)
        .ml(em * 0.17)
        .flex()
        .flex_shrink_0()
        .size(em * 1.17)
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor(CursorStyle::PointingHand)
        .hover(move |style| style.bg(hover))
        .on_click(move |event, window, cx| on_click(event, window, cx))
        .child(
            Icon::new(icon)
                .size(rems(f32::from(em * 0.85) / 16.))
                .color(colors.foreground),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::appearance_fonts::resolve_appearance_fonts;
    use crate::theme::theme_palettes::ThemeAppearance;

    fn theme(appearance: ThemeAppearance) -> AppTheme {
        AppTheme::resolve(
            None,
            appearance,
            0,
            resolve_appearance_fonts(&Default::default(), |_| true),
        )
    }

    #[test]
    fn every_kind_but_neutral_has_an_accent() {
        let kinds = [
            ContextChipKind::Image,
            ContextChipKind::Video,
            ContextChipKind::File,
            ContextChipKind::Mention,
            ContextChipKind::Terminal,
            ContextChipKind::Element,
            ContextChipKind::PreviewAnnotation,
            ContextChipKind::ReviewComment,
            ContextChipKind::PullRequest,
            ContextChipKind::PrOpen,
            ContextChipKind::PrDraft,
            ContextChipKind::PrMerged,
            ContextChipKind::PrClosed,
            ContextChipKind::Skill,
            ContextChipKind::Thread,
            ContextChipKind::Citation,
        ];
        assert!(ContextChipKind::Neutral.accent().is_none());
        for kind in kinds {
            assert!(kind.accent().is_some(), "{kind:?}");
        }
    }

    #[test]
    fn maps_context_reference_kinds_and_defaults_unknown_ones_to_neutral() {
        assert_eq!(
            ContextChipKind::from_context_kind("image"),
            ContextChipKind::Image
        );
        assert_eq!(
            ContextChipKind::from_context_kind("preview-annotation"),
            ContextChipKind::PreviewAnnotation
        );
        assert_eq!(
            ContextChipKind::from_context_kind("mystery"),
            ContextChipKind::Neutral
        );
    }

    #[test]
    fn state_colors_win_over_any_kind() {
        let theme = theme(ThemeAppearance::Light);
        let unresolved = context_chip_colors(
            ContextChipKind::File,
            Some(ContextChipState::Unresolved),
            &theme,
        );
        assert!(unresolved.dashed);
        assert_eq!(unresolved.foreground, theme.foreground);
        let invalid = context_chip_colors(
            ContextChipKind::File,
            Some(ContextChipState::Invalid),
            &theme,
        );
        assert_eq!(invalid.foreground, theme.destructive);
        assert!(!invalid.dashed);
    }

    #[test]
    fn hover_deepens_a_kind_tint() {
        let theme = theme(ThemeAppearance::Dark);
        let colors = context_chip_colors(ContextChipKind::Skill, None, &theme);
        assert!(colors.background_hover.a > colors.background.a);
        assert_ne!(colors.border, colors.border_hover);
    }
}
