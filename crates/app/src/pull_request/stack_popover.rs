//! Port of upstream/apps/web/src/components/pullRequest/PullRequestStackMenu.tsx (the detail
//! header's stack button), PullRequestStackPopover.tsx (the list row's trigger),
//! PullRequestStackHeader.tsx, PullRequestStackLayers.tsx and PullRequestStackLayerContent.tsx.
//!
//! Not ported: "Merge stack" / "Rebase stack" and their confirmation dialog (the source has no
//! stack action: `run_action` carries neither the stack number nor the expected heads), and the
//! "Retry stack lookup" button (the store holds no stack error).

use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Entity, FontWeight, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, rems,
};
use turin_contracts::pull_request::{
    PullRequestRef, PullRequestStack, PullRequestStackLayer, PullRequestStackMembership,
};

use super::detail_context::DetailContext;
use super::detail_layout::TriggerBox;
use super::dropdown::dropdown;
use super::presentation_view::{PullRequestStateGlyph, tone_color};
use super::pull_request_presentation::resolve_pull_request_state;
use super::view_model::PullRequestStore;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

pub type SelectLayer = Rc<dyn Fn(&PullRequestRef, &mut App)>;

/// `PullRequestStackLayerContent`: the state glyph, the title (or branch) over `#n · branch · state`.
fn layer_content(layer: &PullRequestStackLayer, compact: bool, theme: &AppTheme) -> AnyElement {
    let state = resolve_pull_request_state(layer.state, layer.is_draft.unwrap_or(false));
    let title = layer
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| layer.head_branch.as_str())
        .to_owned();
    let detail = if compact {
        format!("#{} · {}", layer.number.get(), state.label)
    } else {
        format!(
            "#{} · {} · {}",
            layer.number.get(),
            layer.head_branch.as_str(),
            state.label
        )
    };
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(PullRequestStateGlyph::new(layer.state, layer.is_draft.unwrap_or(false)))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .truncate()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.foreground)
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(TEXT_XS.size)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(detail)),
                ),
        )
        .into_any_element()
}

/// `PullRequestStackHeader` + `PullRequestStackLayers`: the header label, the layers from the top
/// of the stack down, the base the bottom one lands on.
pub fn stack_menu_items(
    mut menu: PopupMenu,
    stack: &PullRequestStack,
    notice: Option<&str>,
    stale: bool,
    reference: &PullRequestRef,
    on_select: Option<SelectLayer>,
    theme: &AppTheme,
) -> PopupMenu {
    let header = format!("Stack #{}", stack.number.get());
    menu = match notice {
        Some(_) => menu.label(format!(
            "{header}  ·  {}",
            if stale { "May be stale" } else { "Refreshing…" }
        )),
        None => menu.label(header),
    };
    for layer in stack.layers.iter().rev() {
        let current = layer.number == reference.number;
        let target = PullRequestRef {
            number: layer.number,
            ..reference.clone()
        };
        let row_layer = layer.clone();
        let row_theme = theme.clone();
        let mut item = PopupMenuItem::element(move |_, _| layer_row(&row_layer, current, &row_theme));
        if let Some(on_select) = on_select.clone() {
            item = item.on_click(move |_, _, cx| on_select(&target, cx));
        } else {
            item = item.disabled(true);
        }
        menu = menu.item(item);
    }
    menu.label(format!("↳ {}", stack.base.as_str()))
}

/// The layer row with the check on the one being read.
fn layer_row(layer: &PullRequestStackLayer, current: bool, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(layer_content(layer, false, theme))
        .when(current, |this| this.child(Icon::new(IconName::Check).size(rems(0.875))))
        .into_any_element()
}

/// `PullRequestStackMenu`: the stack button of the detail header — layers glyph and `n/total`.
pub fn stack_menu(
    ctx: &DetailContext,
    stack: &PullRequestStack,
    notice: Option<&str>,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme().clone();
    let position = stack
        .layers
        .iter()
        .position(|layer| layer.number == ctx.reference.number)
        .map_or(0, |index| index + 1);
    let total = stack.layers.len();
    let tooltip = format!(
        "View stack #{}, layer {position} of {total}{}",
        stack.number.get(),
        notice.map_or_else(String::new, |notice| format!(" · {notice}"))
    );
    let button = Button::new("pr-stack-menu")
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::Xs)
        .icon(IconName::Layers)
        .label(format!("{position}/{total}"))
        .tooltip(Tooltip::new(tooltip));
    let stack = stack.clone();
    let reference = ctx.reference.clone();
    let select = ctx.nav.select_pull_request.clone();
    let notice = notice.map(str::to_owned);
    dropdown("pr-stack-menu", Anchor::TopLeft, button, move |menu, _, _| {
        stack_menu_items(
            menu,
            &stack,
            notice.as_deref(),
            false,
            &reference,
            Some(select.clone()),
            &theme,
        )
    })
    .into_any_element()
}

/// `PullRequestStackPopover`: a listing row's `n/size` trigger. The stack is read from the store
/// when the menu opens (pressing the trigger asks the source for the pull request), so a page of
/// rows costs nothing at rest.
#[derive(IntoElement)]
pub struct PullRequestStackPopover {
    store: Entity<PullRequestStore>,
    reference: PullRequestRef,
    membership: PullRequestStackMembership,
    on_select: SelectLayer,
}

impl PullRequestStackPopover {
    pub fn new(
        store: Entity<PullRequestStore>,
        reference: PullRequestRef,
        membership: PullRequestStackMembership,
        on_select: SelectLayer,
    ) -> Self {
        Self {
            store,
            reference,
            membership,
            on_select,
        }
    }
}

impl RenderOnce for PullRequestStackPopover {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let membership = self.membership;
        let id = SharedString::from(format!(
            "pr-stack-{}-{}",
            self.reference.number.get(),
            membership.number.get()
        ));
        let label = format!(
            "View stack #{}, layer {} of {}",
            membership.number.get(),
            membership.position.get(),
            membership.size.get()
        );
        let trigger = TriggerBox::new(
            id.clone(),
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_size(TEXT_XS.size)
                .font_weight(FontWeight::NORMAL)
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Layers).size(rems(0.75)))
                .child(format!(
                    "{}/{}",
                    membership.position.get(),
                    membership.size.get()
                ))
                .into_any_element(),
        )
        .tooltip(Tooltip::new(label));
        let store = self.store;
        let reference = self.reference;
        let on_select = self.on_select;
        // The source reads the stack along with the detail; asking at build time is cheap once
        // it is cached, and the menu is built when it opens.
        dropdown(id, Anchor::TopLeft, trigger, move |menu, _, cx| {
            let theme = cx.app_theme().clone();
            if let Some(source) = store.read(cx).source() {
                source.open_detail(&reference, cx);
            }
            let stack = store
                .read(cx)
                .detail(&reference)
                .and_then(|snapshot| snapshot.stack.clone());
            match stack {
                Some(stack) => stack_menu_items(
                    menu,
                    &stack,
                    None,
                    false,
                    &reference,
                    Some(on_select.clone()),
                    &theme,
                ),
                None => menu
                    .label(format!("Stack #{}", membership.number.get()))
                    .label("This pull request is no longer in a stack."),
            }
        })
    }
}

#[allow(dead_code)]
fn _unused() {
    let _ = tone_color;
}
