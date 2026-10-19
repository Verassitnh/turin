//! Port of upstream/apps/web/src/components/usage/UsageLimitsPooled.tsx
//!
//! Accounts pooled per provider: what is open across all of them, who resets
//! next, and how much of the pool that hands back. Answers "can I keep going"
//! before "on which account".
//!
//! Differences: the segment popover opens on hover or click with a short
//! close delay and anchors above the segment (Base UI's `openOnHover` popover
//! has no GPUI equivalent); the spent share is hatched with GPUI's slash
//! pattern instead of a `repeating-linear-gradient`; the email placeholder is
//! not blurred; and the `@2xl/pool` container query is measured from the
//! previous frame's bounds.

use crate::theme::color::solid_over;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, CursorStyle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, anchored, canvas, deferred, div, pattern_slash, point, px, relative, rems,
};
use turin_contracts::model::provider_display_name;
use turin_contracts::provider_usage_limits::ServerProviderUsageWindow;
use turin_shared::usage_limits::{
    CHATGPT_USAGE_URL, LimitAccount, LimitPool, LimitPoolMember, LimitPoolWindow,
    collect_external_usage_links, collect_limit_accounts, collect_limit_notices,
    collect_limit_pools, cursor_usage_window_details, display_limit_windows, format_resets_in,
    remaining_percent,
};

use super::backend::{LimitsEnvironment, UsageBackend, limit_presentations};
use super::redacted_text::redacted_placeholder;
use super::usage_limits::{
    bar_color, format_upcoming_timestamp, open_reset_credit_dialog, pace_icon, reset_credit_status,
    reset_credits_summary,
};
use super::usage_ui::alpha;
use crate::components::provider_instance_icon::provider_instance_icon;
use crate::theme::color::{color, with_alpha};
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_XL, TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::timestamp_format::TimestampFormat;
use crate::ui::alert::{Alert, AlertVariant};
use crate::ui::tooltip::dropdown_glass_border;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// `@2xl/pool`: the container width where each segment carries its own label.
const POOL_WIDE_AT: f32 = 42. * 16.;
/// `w-72`.
const SEGMENT_POPOVER_WIDTH: f32 = 288.;
/// `sideOffset={6}`.
const SEGMENT_POPOVER_GAP: f32 = 6.;

/// `someone@example.com` → `SE`: enough to tell accounts apart, too little to identify one.
pub fn account_initials(email: &str) -> String {
    let (local, domain) = email.split_once('@').unwrap_or((email, ""));
    let first = |part: &str| {
        part.chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default()
    };
    let initials = format!("{}{}", first(local), first(domain)).to_uppercase();
    if initials.is_empty() {
        "?".to_owned()
    } else {
        initials
    }
}

/// A stable hue per email, so the same account gets the same chip on every visit.
pub fn account_hue(email: &str) -> u32 {
    let mut hash: i32 = 0;
    for unit in email.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    (i64::from(hash).unsigned_abs() % 360) as u32
}

fn account_chip(email: &str) -> AnyElement {
    let hue = account_hue(email);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(rems(1.))
        .rounded_full()
        .text_size(TEXT_3XS.size)
        .line_height(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .bg(color(&format!("oklch(0.85 0.08 {hue})")))
        .text_color(color(&format!("oklch(0.35 0.1 {hue})")))
        .child(account_initials(email))
        .into_any_element()
}

fn driver_label(account: &LimitAccount) -> String {
    let driver = account.driver.as_str();
    provider_display_name(driver).unwrap_or(driver).to_owned()
}

fn account_title(account: &LimitAccount) -> String {
    account
        .display_name
        .clone()
        .unwrap_or_else(|| driver_label(account))
}

/// The same mark the model picker uses for a native instance (provider glyph,
/// initials badge, accent); hub accounts have no instance, so they get the chip.
fn account_avatar(account: &LimitAccount, cx: &App) -> Option<AnyElement> {
    let theme = cx.app_theme();
    if account.redeem.is_some() {
        let name = account_title(account);
        let glyph = provider_instance_icon(
            account.driver.as_str(),
            &name,
            rems(1.),
            Some(with_alpha(theme.foreground, 0.8).into()),
            cx,
        );
        return Some(
            div()
                .relative()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(rems(1.25))
                .child(glyph)
                .when(account.display_name.is_some(), |this| {
                    this.child(
                        div()
                            .absolute()
                            .right_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(14.))
                            .min_w(px(14.))
                            .px(px(2.))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.popover)
                            .bg(theme.card)
                            .text_size(crate::theme::tokens::TEXT_4XS.size)
                            .line_height(px(8.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(
                                crate::state::helpers::provider_instance_display::provider_instance_initials(
                                    &name,
                                ),
                            ),
                    )
                })
                .into_any_element(),
        );
    }
    account.email.as_deref().map(account_chip)
}

/// Who an account is, without printing the email: the instance name when there
/// is one, else a two-letter chip. The address itself is revealed on demand in
/// the segment's popover.
fn account_name(account: &LimitAccount, theme: &AppTheme, strong: bool) -> AnyElement {
    let styled = |text: String| {
        let base = base_with(strong, theme);
        base.child(text).into_any_element()
    };
    if let Some(name) = &account.display_name {
        return styled(name.clone());
    }
    if let Some(email) = &account.email {
        return div()
            .flex()
            .min_w_0()
            .items_center()
            .child(account_chip(email))
            .into_any_element();
    }
    styled(driver_label(account))
}

fn base_with(strong: bool, theme: &AppTheme) -> gpui_kit::Div {
    let base = div().min_w_0().truncate();
    if strong {
        base.font_weight(FontWeight::MEDIUM)
            .text_color(theme.foreground)
    } else {
        base
    }
}

/// Redeem state per account: the confirm lives outside the popover, so the
/// outcome needs a home outside it too.
#[derive(Default)]
struct RedeemState {
    busy: bool,
    status: Option<String>,
}

/// Builds the Cursor enable prompt among the pools.
pub type CursorPrompt = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

pub struct LimitsProps {
    /// Already narrowed to the selected environments and visible providers.
    pub environments: Vec<LimitsEnvironment>,
    /// Epoch milliseconds the page advances on explicit refresh.
    pub now: f64,
    pub timestamp_format: TimestampFormat,
    pub cursor_prompt: Option<CursorPrompt>,
}

#[derive(Clone, Default)]
struct WidthProbe {
    wide: Rc<Cell<bool>>,
}

impl WidthProbe {
    fn element(&self) -> AnyElement {
        let wide = self.wide.clone();
        canvas(
            move |bounds, window, _| {
                let now_wide = f32::from(bounds.size.width) >= POOL_WIDE_AT;
                if now_wide != wide.get() {
                    wide.set(now_wide);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

pub struct UsageLimitsPooled {
    backend: Rc<dyn UsageBackend>,
    props: LimitsProps,
    /// The segment whose popover is open.
    popup: Option<String>,
    /// Opened by a click, so leaving does not close it.
    pinned: bool,
    popup_hovered: bool,
    generation: u64,
    probes: HashMap<String, super::usage_ui::BoundsProbe>,
    width_probes: HashMap<String, WidthProbe>,
    revealed: HashSet<String>,
    redeem: HashMap<String, RedeemState>,
    /// First segment drawn, for the screenshot harness.
    first_key: Option<String>,
}

impl UsageLimitsPooled {
    pub fn new(backend: Rc<dyn UsageBackend>, props: LimitsProps) -> Self {
        Self {
            backend,
            props,
            popup: None,
            pinned: false,
            popup_hovered: false,
            generation: 0,
            probes: HashMap::new(),
            width_probes: HashMap::new(),
            revealed: HashSet::new(),
            redeem: HashMap::new(),
            first_key: None,
        }
    }

    /// Screenshot harness: opens the first segment's popover and pins it.
    pub fn demo_open_first(&mut self, cx: &mut Context<Self>) {
        self.popup = self.first_key.clone();
        self.pinned = true;
        cx.notify();
    }

    pub fn set_props(&mut self, props: LimitsProps) {
        self.props = props;
    }

    fn schedule_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            this.update_in(cx, |this, _, cx| {
                if this.generation == generation && !this.pinned && !this.popup_hovered {
                    this.popup = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn hover_segment(
        &mut self,
        key: &str,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.generation += 1;
            if self.popup.as_deref() != Some(key) {
                self.popup = Some(key.to_owned());
                self.pinned = false;
                cx.notify();
            }
        } else if !self.pinned {
            self.schedule_close(window, cx);
        }
    }

    fn hover_popup(&mut self, hovered: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.popup_hovered = hovered;
        if hovered {
            self.generation += 1;
        } else if !self.pinned {
            self.schedule_close(window, cx);
        }
    }

    fn toggle_pin(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.popup.as_deref() == Some(key) && self.pinned {
            self.popup = None;
            self.pinned = false;
        } else {
            self.popup = Some(key.to_owned());
            self.pinned = true;
        }
        cx.notify();
    }

    fn redeem_account(
        &mut self,
        account: &LimitAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = account.redeem.clone() else {
            return;
        };
        if !self.backend.can_manage_providers(&target.environment_id) {
            return;
        }
        let key = account.key.clone();
        let state = self.redeem.entry(key.clone()).or_default();
        state.busy = true;
        state.status = None;
        cx.notify();
        let task = self.backend.consume_reset_credit(
            target.environment_id.clone(),
            target.input.clone(),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, _, cx| {
                let state = this.redeem.entry(key).or_default();
                state.busy = false;
                state.status = Some(reset_credit_status(&result));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn segment_popover(
        &mut self,
        account: &LimitAccount,
        window_data: &ServerProviderUsageWindow,
        restores_percent: Option<f64>,
        now: f64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let remaining = remaining_percent(window_data);
        let resets_in = format_resets_in(window_data, now);
        let where_ = if account.environments.is_empty() {
            account.source_label.clone().unwrap_or_default()
        } else {
            account
                .environments
                .iter()
                .map(|environment| environment.label.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let credits = account
            .redeem
            .as_ref()
            .and(account.limits.reset_credits.as_ref())
            .filter(|credits| credits.available_count > 0)
            .cloned();
        let row = |label: &'static str, value: String| {
            div()
                .flex()
                .gap_x_3()
                .child(
                    div()
                        .w(rems(4.5))
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().min_w_0().text_color(theme.foreground).child(value))
        };
        let divider = alpha(theme.border, 0.6);
        let section = || {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .border_t_1()
                .border_color(divider)
                .pt(rems(0.625))
        };
        let revealed = self.revealed.contains(&account.key);
        let key = account.key.clone();
        let account_for_redeem = account.clone();
        let redeem_state = self.redeem.get(&account.key);
        let busy = redeem_state.is_some_and(|state| state.busy);
        let can_manage = account
            .redeem
            .as_ref()
            .is_some_and(|target| self.backend.can_manage_providers(&target.environment_id));
        let entity = cx.entity();

        div()
            .flex()
            .flex_col()
            .gap(rems(0.625))
            .w(px(SEGMENT_POPOVER_WIDTH))
            .text_size(TEXT_XS.size)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .children(account_avatar(account, cx))
                            .child(div().truncate().child(account_title(account))),
                    )
                    .children(account.email.as_ref().map(|email| {
                        let text = if revealed {
                            email.clone()
                        } else {
                            redacted_placeholder(email)
                        };
                        let tooltip = if revealed {
                            "Click to hide email"
                        } else {
                            "Click to reveal email"
                        };
                        let entity = entity.clone();
                        let key = key.clone();
                        div()
                            .id(SharedString::from(format!("email:{key}")))
                            .w_auto()
                            .min_w_0()
                            .cursor(CursorStyle::PointingHand)
                            .rounded(px(4.))
                            .font_family(theme.fonts.mono.clone())
                            .text_size(TEXT_2XS.size)
                            .text_color(theme.muted_foreground)
                            .hover({
                                let hover = theme.foreground;
                                move |style| style.text_color(hover)
                            })
                            .child(text)
                            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
                            .on_click(move |_, _, cx| {
                                let key = key.clone();
                                entity.update(cx, |this, cx| {
                                    if !this.revealed.remove(&key) {
                                        this.revealed.insert(key);
                                    }
                                    cx.notify();
                                });
                            })
                    })),
            )
            .child(
                section()
                    .children(account.plan.as_ref().map(|plan| row("Plan", plan.clone())))
                    .when(!where_.is_empty(), |this| {
                        this.child(row(
                            if account.environments.is_empty() {
                                "Via"
                            } else {
                                "Signed in"
                            },
                            where_.clone(),
                        ))
                    }),
            )
            .child(
                section()
                    .child(row("Left", format!("{}%", remaining as i64)))
                    .children(window_data.resets_at.as_deref().map(|at| {
                        row(
                            "Resets",
                            format!(
                                "{}{}",
                                format_upcoming_timestamp(
                                    at,
                                    self.props.timestamp_format,
                                    now as i64
                                ),
                                resets_in.as_ref().map_or_else(String::new, |text| format!(
                                    " · {}",
                                    text.replace("resets in ", "in ")
                                ))
                            ),
                        )
                    }))
                    .children(
                        restores_percent
                            .filter(|restores| *restores > 0.)
                            .map(|restores| {
                                row("Restores", format!("+{}% of pool", restores.round() as i64))
                            }),
                    ),
            )
            .children(credits.map(|credits| {
                let entity = entity.clone();
                div()
                    .border_t_1()
                    .border_color(divider)
                    .pt(rems(0.625))
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(reset_credits_summary(&credits, now, true))
                            .child(
                                div().ml_auto().child(
                                    Button::new(SharedString::from(format!("use-reset:{key}")))
                                        .variant(ButtonVariant::Outline)
                                        .size(ButtonSize::Xs)
                                        .label(if busy { "Using…" } else { "Use reset" })
                                        .disabled(busy || !can_manage)
                                        .on_click(move |_, window, cx| {
                                            let entity = entity.clone();
                                            let account = account_for_redeem.clone();
                                            entity.update(cx, |this, cx| {
                                                this.popup = None;
                                                this.pinned = false;
                                                cx.notify();
                                            });
                                            let confirm_entity = entity.clone();
                                            open_reset_credit_dialog(
                                                window,
                                                cx,
                                                false,
                                                move |window, cx| {
                                                    let account = account.clone();
                                                    confirm_entity.update(cx, |this, cx| {
                                                        this.redeem_account(&account, window, cx)
                                                    });
                                                },
                                            );
                                        }),
                                ),
                            ),
                    )
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn pool_segment(
        &mut self,
        pool_key: &str,
        pool: &LimitPoolWindow,
        position: usize,
        member: &LimitPoolMember,
        color: Hsla,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let account = &member.account;
        let window_data = &member.window;
        let now = self.props.now;
        let key = format!("{pool_key}:{}", account.key);
        if self.first_key.is_none() {
            self.first_key = Some(key.clone());
        }
        let remaining = remaining_percent(window_data);
        let resets_in = format_resets_in(window_data, now);
        let credits = account
            .limits
            .reset_credits
            .as_ref()
            .map_or(0, |credits| credits.available_count);
        let index = position + 1;
        let reset = pool
            .resets
            .iter()
            .find(|reset| reset.member.account.key == account.key);
        let probe = self.probes.entry(key.clone()).or_default().clone();
        let show_name = pool.columns.len() > 1;
        let open = self.popup.as_deref() == Some(key.as_str());
        let entity = cx.entity();

        let strip_height = if wide { rems(2.) } else { rems(1.25) };
        let resets_text = resets_in
            .as_ref()
            .map(|text| text.replace("resets in ", "↻ "));
        let segment = div()
            .id(SharedString::from(format!("segment:{key}")))
            .relative()
            .h(strip_height)
            .min_w_0()
            .overflow_hidden()
            .rounded(px(6.))
            .bg(theme.muted)
            .cursor(CursorStyle::PointingHand)
            .when(open, |this| this.border_1().border_color(theme.border))
            .child(probe.element())
            .child(
                // Translucent so the label reads over the fill for any provider colour and theme.
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative((remaining / 100.) as f32))
                    .rounded(px(6.))
                    .bg(alpha(color, 0.35)),
            )
            // The spent share is hatched, not blank: it is what the countdown restores.
            .when(remaining < 100. && reset.is_some(), |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(relative(((100. - remaining) / 100.) as f32))
                        .bg(pattern_slash(alpha(color, 0.2), 1., 5.)),
                )
            })
            .when(!wide, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(TEXT_3XS.size)
                        .line_height(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(with_alpha(theme.foreground, 0.8))
                        .child(index.to_string()),
                )
            })
            .when(wide, |this| {
                this.child(
                    div()
                        .relative()
                        .flex()
                        .h_full()
                        .min_w_0()
                        .items_center()
                        .gap(rems(0.375))
                        .px_2()
                        .text_size(TEXT_XS.size)
                        .when(show_name, |this| {
                            this.child(account_name(account, &theme, true))
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(format!("{}%", remaining as i64)),
                        )
                        .child(
                            // Countdown and badge get their own plate: fill and hatching run under them otherwise.
                            div()
                                .ml_auto()
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap(rems(0.375))
                                .rounded(px(4.))
                                .bg(solid_over(theme.background, 0.85, theme.background))
                                .px(rems(0.375))
                                .py(px(2.))
                                .text_size(TEXT_2XS.size)
                                .text_color(theme.foreground)
                                .children(resets_text.clone())
                                .when(credits > 0, |this| {
                                    this.when(resets_text.is_some(), |this| {
                                        this.child(
                                            div().text_color(theme.muted_foreground).child("·"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(2.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(Icon::new(IconName::Ticket).size(rems(0.75)))
                                            .child(credits.to_string()),
                                    )
                                }),
                        ),
                )
            })
            .on_hover({
                let entity = entity.clone();
                let key = key.clone();
                move |hovered, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.hover_segment(&key, *hovered, window, cx)
                    });
                }
            })
            .on_click({
                let entity = entity.clone();
                let key = key.clone();
                move |_, _, cx| entity.update(cx, |this, cx| this.toggle_pin(&key, cx))
            });

        let popup = open.then(|| {
            let bounds = probe.bounds();
            let content = self.segment_popover(
                account,
                window_data,
                reset.map(|r| r.restores_percent),
                now,
                cx,
            );
            let hover_entity = entity.clone();
            let out_entity = entity.clone();
            deferred(
                anchored()
                    .position(point(
                        bounds.center().x - px(SEGMENT_POPOVER_WIDTH / 2.),
                        bounds.top() - px(SEGMENT_POPOVER_GAP),
                    ))
                    .anchor(Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id(SharedString::from(format!("popup:{key}")))
                            .occlude()
                            .p_4()
                            .rounded(RADIUS_LG)
                            .border_1()
                            .border_color(dropdown_glass_border(&theme))
                            .bg(theme.popover)
                            .shadow(tokens::shadow_popover(theme.is_dark()))
                            .text_color(theme.popover_foreground)
                            .on_hover(move |hovered, window, cx| {
                                hover_entity
                                    .update(cx, |this, cx| this.hover_popup(*hovered, window, cx));
                            })
                            .on_mouse_down_out(move |_, _, cx| {
                                out_entity.update(cx, |this, cx| {
                                    if this.pinned {
                                        this.popup = None;
                                        this.pinned = false;
                                        cx.notify();
                                    }
                                });
                            })
                            .child(content),
                    ),
            )
            .with_priority(30)
            .into_any_element()
        });

        div()
            .flex_1()
            .min_w_0()
            .child(segment)
            .children(popup)
            .into_any_element()
    }

    /// Below the strip at narrow widths: one row per account in bar order,
    /// carrying the text the segment has no room for. Hovering or tapping a row
    /// opens the same popover as its segment.
    fn legend_row(
        &mut self,
        pool_key: &str,
        position: usize,
        member: &LimitPoolMember,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let account = &member.account;
        let now = self.props.now;
        let key = format!("{pool_key}:{}", account.key);
        let remaining = remaining_percent(&member.window);
        let resets_text =
            format_resets_in(&member.window, now).map(|text| text.replace("resets in ", "↻ "));
        let credits = account
            .limits
            .reset_credits
            .as_ref()
            .map_or(0, |credits| credits.available_count);
        let entity = cx.entity();
        div()
            .id(SharedString::from(format!("legend:{key}")))
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .h(rems(1.75))
            .px_2()
            .rounded(px(6.))
            .cursor(CursorStyle::PointingHand)
            .hover({
                let hover = theme.accent;
                move |style| style.bg(hover)
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .size(rems(1.))
                    .rounded(px(4.))
                    .text_size(TEXT_3XS.size)
                    .line_height(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(with_alpha(theme.foreground, 0.8))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .rounded(px(4.))
                            .bg(alpha(color, 0.35)),
                    )
                    .child(div().relative().child((position + 1).to_string())),
            )
            .text_size(TEXT_XS.size)
            .child(account_name(account, &theme, true))
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(format!("{}%", remaining as i64)),
            )
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(rems(0.375))
                    .text_size(TEXT_2XS.size)
                    .text_color(theme.muted_foreground)
                    .children(resets_text.clone())
                    .when(credits > 0, |this| {
                        this.when(resets_text.is_some(), |this| this.child("·"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(Icon::new(IconName::Ticket).size(rems(0.75)))
                                    .child(credits.to_string()),
                            )
                    }),
            )
            .on_hover({
                let entity = entity.clone();
                let key = key.clone();
                move |hovered, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.hover_segment(&key, *hovered, window, cx)
                    });
                }
            })
            .on_click({
                let key = key.clone();
                move |_, _, cx| entity.update(cx, |this, cx| this.toggle_pin(&key, cx))
            })
            .into_any_element()
    }

    /// One pooled window as equal-width segments, one per account, each filled by
    /// the share of that account's quota still open. Equal widths are honest: every
    /// account contributes the same share of the pool, whatever its plan.
    fn pool_bar(
        &mut self,
        pool_key: &str,
        pool: &LimitPoolWindow,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let probe = self
            .width_probes
            .entry(pool_key.to_owned())
            .or_default()
            .clone();
        let wide = probe.wide.get();
        let theme = cx.app_theme().clone();
        let columns: Vec<(usize, LimitPoolMember)> = pool
            .columns
            .iter()
            .enumerate()
            .filter_map(|(position, column)| {
                column.window.clone().map(|window| {
                    (
                        position,
                        LimitPoolMember {
                            account: column.account.clone(),
                            window,
                        },
                    )
                })
            })
            .collect();
        let strip: Vec<AnyElement> = columns
            .iter()
            .map(|(position, member)| {
                self.pool_segment(pool_key, pool, *position, member, color, wide, cx)
            })
            .collect();
        let statuses: Vec<AnyElement> = columns
            .iter()
            .filter_map(|(_, member)| {
                let status = self.redeem.get(&member.account.key)?.status.clone()?;
                Some(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(account_name(&member.account, &theme, true))
                        .child(format!(" {status}"))
                        .into_any_element(),
                )
            })
            .collect();
        let legend: Vec<AnyElement> = if wide {
            Vec::new()
        } else {
            columns
                .iter()
                .map(|(position, member)| self.legend_row(pool_key, *position, member, color, cx))
                .collect()
        };
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(probe.element())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().flex().gap_x_1().children(strip))
                    .children(legend)
                    .children(statuses),
            )
            .into_any_element()
    }

    /// Big pooled number and the segment bar. Accounts keep the same column across
    /// windows; each segment's popover shows its own reset time and share restored.
    fn pool_window_card(
        &mut self,
        driver: &str,
        pool: &LimitPoolWindow,
        color: Hsla,
        label: Option<&str>,
        description: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        // The soonest reset that hands anything back; an untouched account resets to no effect.
        let next_refill = pool.resets.iter().find(|reset| reset.restores_percent > 0.);
        let pool_key = format!("{driver}:{}:{}", pool.kind_key(), pool.id);
        let bar = self.pool_bar(&pool_key, pool, color, cx);
        div()
            .flex()
            .flex_col()
            .gap_y_3()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(alpha(theme.border, 0.6))
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_x_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(rems(11.))
                            .gap_1()
                            .child(
                                div()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(label.map_or_else(|| pool.label.clone(), str::to_owned)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(rems(1.875))
                                            .line_height(rems(2.25))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child(format!("{}%", pool.remaining_percent as i64)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .text_size(TEXT_SM.size)
                                            .text_color(theme.muted_foreground)
                                            .child("left")
                                            .when_some(pool.pace, |this, pace| {
                                                this.child(pace_icon(
                                                    format!("pool-pace:{pool_key}"),
                                                    pace,
                                                    cx,
                                                ))
                                            }),
                                    ),
                            )
                            .when_some(
                                next_refill.filter(|_| pool.columns.len() > 1),
                                |this, refill| {
                                    this.child(
                                        div()
                                            .text_size(TEXT_XS.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(format!(
                                                "↻ +{}%",
                                                refill.restores_percent.round() as i64
                                            )),
                                    )
                                },
                            ),
                    )
                    .child(bar),
            )
            .children(description.map(|description| {
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(description.to_owned())
            }))
            .into_any_element()
    }

    fn pool_section(&mut self, pool: &LimitPool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let driver = pool.driver.as_str().to_owned();
        let label = provider_display_name(&driver).unwrap_or(&driver).to_owned();
        let color = bar_color(&driver).resolve(&theme);
        let windows = display_limit_windows(pool);
        let cards: Vec<AnyElement> = windows
            .into_iter()
            .map(|window| {
                let details = (driver == "cursor")
                    .then(|| cursor_usage_window_details(&window.id))
                    .flatten();
                self.pool_window_card(
                    &driver,
                    window,
                    color,
                    details.map(|details| details.label),
                    details.map(|details| details.description),
                    cx,
                )
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(TEXT_SM.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(provider_instance_icon(
                        &driver,
                        &label,
                        rems(1.),
                        Some(with_alpha(theme.foreground, 0.8).into()),
                        cx,
                    ))
                    .child(label),
            )
            .children(cards)
            .into_any_element()
    }
}

trait WindowKindKey {
    fn kind_key(&self) -> &'static str;
}

impl WindowKindKey for LimitPoolWindow {
    fn kind_key(&self) -> &'static str {
        use turin_contracts::provider_usage_limits::ServerProviderUsageWindowKind::*;
        match self.kind {
            Session => "session",
            Weekly => "weekly",
            Monthly => "monthly",
            Other => "other",
        }
    }
}

impl Render for UsageLimitsPooled {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let presentations = limit_presentations(&self.props.environments);
        let accounts = collect_limit_accounts(&presentations);
        let pools = collect_limit_pools(&accounts, self.props.now);
        let notices = collect_limit_notices(&presentations);
        let links = collect_external_usage_links(&presentations);
        let index_of = |driver: &str| pools.iter().position(|pool| pool.driver.as_str() == driver);
        let cursor_prompt_at = match (index_of("codex"), index_of("claudeAgent")) {
            (None, None) => 0,
            (a, b) => a.into_iter().chain(b).max().map_or(0, |index| index + 1),
        };
        let prompt = self.props.cursor_prompt.clone();
        let has_prompt = prompt.is_some();

        let mut children: Vec<AnyElement> = Vec::new();
        if pools.is_empty() && notices.is_empty() && !has_prompt && links.is_empty() {
            children.push(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("No provider on the selected environments reports subscription limits.")
                    .into_any_element(),
            );
        }
        for (index, pool) in pools.iter().enumerate() {
            if index == cursor_prompt_at
                && let Some(prompt) = &prompt
            {
                children.push(prompt(window, cx));
            }
            children.push(self.pool_section(pool, cx));
        }
        if cursor_prompt_at == pools.len()
            && let Some(prompt) = &prompt
        {
            children.push(prompt(window, cx));
        }
        for link in &links {
            let backend = self.backend.clone();
            let url = link.url.clone();
            let is_chatgpt = link.url == CHATGPT_USAGE_URL;
            children.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .rounded(RADIUS_XL)
                    .border_1()
                    .border_color(theme.border)
                    .p_4()
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .when(is_chatgpt, |this| {
                                this.child(provider_instance_icon("codex", "Codex", rems(1.25), None, cx))
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(TEXT_SM.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(link.label.clone()),
                                    )
                                    .children(if is_chatgpt {
                                        Some("View usage in ChatGPT with your connected account.".to_owned())
                                    } else {
                                        link.message.clone()
                                    }
                                    .map(|text| {
                                        div()
                                            .max_w(rems(36.))
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.muted_foreground)
                                            .child(text)
                                    })),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("manage:{}", link.url)))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::Xs)
                            .label("Manage usage")
                            .icon(IconName::ExternalLink)
                            .on_click(move |_, _, cx| backend.open_external(&url, cx)),
                    )
                    .into_any_element(),
            );
        }
        if !notices.is_empty() {
            let mut alert = Alert::new()
                .variant(AlertVariant::Warning)
                .icon(IconName::TriangleAlert);
            for notice in &notices {
                alert = alert.content(div().min_w_0().child(notice.clone()));
            }
            children.push(alert.into_any_element());
        }

        div().flex().flex_col().gap(rems(2.)).children(children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_pair_the_local_part_and_domain() {
        assert_eq!(account_initials("someone@example.com"), "SE");
        assert_eq!(account_initials("a@b"), "AB");
        assert_eq!(account_initials("@"), "?");
        assert_eq!(account_initials("solo"), "S");
    }

    #[test]
    fn hue_is_stable_and_within_the_wheel() {
        let hue = account_hue("jackson@samwisepsm.com");
        assert!(hue < 360);
        assert_eq!(hue, account_hue("jackson@samwisepsm.com"));
        // JS: hash = (hash * 31 + code) | 0 over "ab".
        assert_eq!(account_hue("ab"), ((97 * 31 + 98) % 360) as u32);
    }
}
