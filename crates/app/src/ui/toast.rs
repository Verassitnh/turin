//! Port of upstream/apps/web/src/components/ui/toast.tsx
//!
//! The toast manager (`toastManager`, `anchoredToastManager`), the stacked
//! viewport (`Toasts`), anchored toasts (`AnchoredToasts`), the card body
//! (`ToastBodyContent`, `CopyErrorButton`, `ToastExpandableSection`,
//! `ToastDescriptionAndExpandable`), the corner dismiss orb, swipe to
//! dismiss, and the visible-only auto-dismiss.
//!
//! Base UI's toast store is [`ToastStore`]: newest first, a limit of three,
//! `ending` toasts keep their slot until the exit motion ends, hovering or
//! swiping expands the stack and pauses the timers. The pure rules live in
//! [`super::toast_logic`]. GPUI has no element transforms, so each toast is a
//! fixed-size absolute card whose frame (offset, width, height, opacity) is
//! tweened on the upstream `.5s cubic-bezier(.22,1,.36,1)` curve.
//!
//! Hosts mount [`toasts`] and [`anchored_toasts`] over the window root and
//! call [`init`] once at startup.

use crate::theme::color::solid_over;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, AppContext as _, Bounds, BoxShadow,
    ClickEvent, ClipboardItem, Context, CursorStyle, DispatchPhase, Div, Entity, FontWeight,
    Global, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Subscription, Transformation, Window, anchored, canvas,
    deferred, div, percentage, point, px, rems, svg,
};

use super::animated_size::cubic_bezier;
use super::button::{Button, ButtonSize, ButtonVariant};
use super::icon::{Icon, IconName};
use super::toast_logic::{
    DEFAULT_TOAST_TIMEOUT_MS, SwipeDirection, SwipeEnd, SwipeTracker, TOAST_LIMIT, TOAST_MOTION_MS,
    ThreadToastScope, ToastActionChildren, ToastActionLayout, ToastActionVariant,
    ToastBodyDescriptor, ToastBodyInput, ToastFrame, ToastFrameInput, ToastLayoutInput,
    ToastPosition, ToastPriority, ToastTransitionStatus, ToastType, VisibleDismissTimer,
    build_visible_toast_layout, derive_toast_body, error_description_is_clamped,
    should_hide_collapsed_toast_content, should_render_thread_scoped_toast, toast_ending_frame,
    toast_inset, toast_motion_frame, toast_resting_frame, toast_starting_frame,
    toast_viewport_width,
};
use super::tooltip::{Tooltip, dropdown_glass_background, dropdown_glass_border};
use crate::components::drag_region::no_drag;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    self, RADIUS_DEFAULT, RADIUS_LG, RADIUS_MD, TEXT_SM, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT,
};
use crate::theme::{ActiveAppTheme, AppTheme};
use turin_contracts::environment::ScopedThreadRef;

pub type ToastId = u64;

type Callback = Rc<dyn Fn(&mut Window, &mut App)>;
type CloseCallback = Rc<dyn Fn(&mut App)>;
type ExpandableContent = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

fn toast_icon(type_: ToastType) -> IconName {
    match type_ {
        ToastType::Error => IconName::CircleAlert,
        ToastType::Info => IconName::Info,
        ToastType::Loading => IconName::LoaderCircle,
        ToastType::Success => IconName::CircleCheck,
        ToastType::Warning => IconName::TriangleAlert,
    }
}

fn icon_color(type_: ToastType, theme: &AppTheme) -> gpui_kit::Hsla {
    match type_ {
        ToastType::Error => theme.destructive,
        ToastType::Info => theme.info,
        ToastType::Success => theme.success,
        ToastType::Warning => theme.warning,
        // `in-data-[type=loading]:opacity-80`
        ToastType::Loading => with_alpha(theme.foreground, 0.8).into(),
    }
}

fn button_variant(variant: ToastActionVariant) -> ButtonVariant {
    match variant {
        ToastActionVariant::Default => ButtonVariant::Default,
        ToastActionVariant::Destructive => ButtonVariant::Destructive,
        ToastActionVariant::DestructiveOutline => ButtonVariant::DestructiveOutline,
        ToastActionVariant::Ghost => ButtonVariant::Ghost,
        ToastActionVariant::Link => ButtonVariant::Link,
        ToastActionVariant::Outline => ButtonVariant::Outline,
        ToastActionVariant::Secondary => ButtonVariant::Secondary,
    }
}

// ---- toast data -----------------------------------------------------------

/// A button in a toast: the primary `actionProps`, `additionalActions`, or
/// `secondaryActionProps`.
#[derive(Clone)]
pub struct ToastActionSpec {
    pub id: SharedString,
    pub label: SharedString,
    pub on_click: Option<Callback>,
}

impl ToastActionSpec {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            on_click: None,
        }
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

#[derive(Clone, Default)]
pub struct ExpandableLabels {
    pub expand: Option<SharedString>,
    pub collapse: Option<SharedString>,
}

/// `ThreadToastData`.
#[derive(Clone, Default)]
pub struct ThreadToastData {
    pub scope: Option<ThreadToastScope>,
    pub leading_icon: Option<IconName>,
    pub tooltip_style: bool,
    pub on_close: Option<CloseCallback>,
    pub dismiss_after_visible_ms: Option<i64>,
    pub hide_copy_button: bool,
    pub additional_actions: Vec<ToastActionSpec>,
    pub secondary_action: Option<ToastActionSpec>,
    pub secondary_action_variant: Option<ToastActionVariant>,
    /// Extra body shown after toggling "Show details".
    pub expandable_content: Option<ExpandableContent>,
    pub expandable_labels: ExpandableLabels,
    /// The summary and label act as one text disclosure (no separate chevron row).
    pub expandable_description_trigger: bool,
    pub action_layout: Option<ToastActionLayout>,
    pub action_variant: Option<ToastActionVariant>,
}

/// `positionerProps` of an anchored toast.
#[derive(Clone, Copy)]
pub struct ToastAnchor {
    pub bounds: Bounds<Pixels>,
    pub side_offset: f32,
}

#[derive(Clone, Default)]
pub struct ToastOptions {
    pub title: SharedString,
    pub description: Option<SharedString>,
    pub type_: Option<ToastType>,
    /// Milliseconds; `0` never dismisses. `None` is Base UI's 5000.
    pub timeout: Option<u64>,
    pub priority: Option<ToastPriority>,
    pub action: Option<ToastActionSpec>,
    pub data: ThreadToastData,
    pub anchor: Option<ToastAnchor>,
}

impl ToastOptions {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn type_(mut self, type_: ToastType) -> Self {
        self.type_ = Some(type_);
        self
    }

    pub fn timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout = Some(timeout_ms);
        self
    }

    pub fn action(mut self, action: ToastActionSpec) -> Self {
        self.action = Some(action);
        self
    }

    pub fn data(mut self, data: ThreadToastData) -> Self {
        self.data = data;
        self
    }

    pub fn anchor(mut self, anchor: ToastAnchor) -> Self {
        self.anchor = Some(anchor);
        self
    }
}

impl ToastOptions {
    /// `actionProps.children` as the logic module reads it.
    fn action_children(&self) -> ToastActionChildren {
        match &self.action {
            Some(action) => ToastActionChildren::Text(action.label.to_string()),
            None => ToastActionChildren::Absent,
        }
    }

    fn body_descriptor(&self, action: &ToastActionChildren) -> ToastBodyDescriptor {
        derive_toast_body(ToastBodyInput {
            type_: self.type_,
            description: self.description.as_deref(),
            action: Some(action),
            action_layout: self.data.action_layout,
            action_variant: self.data.action_variant,
            secondary_action_variant: self.data.secondary_action_variant,
            hide_copy_button: self.data.hide_copy_button,
            additional_action_count: self.data.additional_actions.len(),
            has_secondary_action: self.data.secondary_action.is_some(),
        })
    }
}

// ---- store ----------------------------------------------------------------

struct Motion {
    from: ToastFrame,
    to: ToastFrame,
    started: Instant,
}

struct ToastItem {
    id: ToastId,
    options: ToastOptions,
    status: Option<ToastTransitionStatus>,
    swipe_dismissed: Option<SwipeDirection>,
    /// Measured card height including the border. `0` once ending, as in Base UI.
    layout_height: Option<f64>,
    measured_height: f64,
    limited: bool,
    motion: Option<Motion>,
    swipe: Option<SwipeTracker>,
    /// Where a swipe dismissal let go; the exit continues from here.
    exit_swipe: (f64, f64),
    timeout_remaining_ms: Option<i64>,
    visible_timer: Option<VisibleDismissTimer>,
}

pub struct ToastStore {
    position: ToastPosition,
    toasts: Vec<ToastItem>,
    next_id: ToastId,
    hovered: HashSet<(ToastId, bool)>,
    active_thread: Option<ScopedThreadRef>,
    ticking: bool,
    started: Instant,
    last_tick: Instant,
}

impl ToastStore {
    pub fn new(position: ToastPosition) -> Self {
        let now = Instant::now();
        Self {
            position,
            toasts: Vec::new(),
            next_id: 1,
            hovered: HashSet::new(),
            active_thread: None,
            ticking: false,
            started: now,
            last_tick: now,
        }
    }

    pub fn position(&self) -> ToastPosition {
        self.position
    }

    /// The thread the route shows (`useActiveThreadRefFromRoute`).
    pub fn set_active_thread(&mut self, thread: Option<ScopedThreadRef>, cx: &mut Context<Self>) {
        if self.active_thread != thread {
            self.active_thread = thread;
            cx.notify();
        }
    }

    /// Dev hook for the gallery: keep the stack expanded as if hovered.
    pub fn pin_expanded(&mut self, cx: &mut Context<Self>) {
        self.hovered.insert((0, false));
        cx.notify();
    }

    fn expanded(&self) -> bool {
        !self.hovered.is_empty() || self.toasts.iter().any(|toast| toast.swipe.is_some())
    }

    fn now_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }

    /// `toastManager.add`. Returns the new id.
    pub fn add(&mut self, options: ToastOptions, cx: &mut Context<Self>) -> ToastId {
        let id = self.next_id;
        self.next_id += 1;
        let duration = options.timeout.unwrap_or(DEFAULT_TOAST_TIMEOUT_MS);
        let timeout_remaining_ms =
            (options.type_ != Some(ToastType::Loading) && duration > 0).then_some(duration as i64);
        let visible_timer = options
            .data
            .dismiss_after_visible_ms
            .and_then(VisibleDismissTimer::start);
        self.toasts.insert(
            0,
            ToastItem {
                id,
                options,
                status: None,
                swipe_dismissed: None,
                layout_height: None,
                measured_height: 0.,
                limited: false,
                motion: None,
                swipe: None,
                exit_swipe: (0., 0.),
                timeout_remaining_ms,
                visible_timer,
            },
        );
        self.relimit();
        self.ensure_ticking(cx);
        cx.notify();
        id
    }

    /// `toastManager.update`.
    pub fn update(
        &mut self,
        id: ToastId,
        update: impl FnOnce(&mut ToastOptions),
        cx: &mut Context<Self>,
    ) {
        let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) else {
            return;
        };
        // Updates to a closing toast must not block its dismissal.
        if toast.status == Some(ToastTransitionStatus::Ending) {
            return;
        }
        update(&mut toast.options);
        let duration = toast.options.timeout.unwrap_or(DEFAULT_TOAST_TIMEOUT_MS);
        toast.timeout_remaining_ms = (toast.options.type_ != Some(ToastType::Loading)
            && duration > 0)
            .then_some(duration as i64);
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// `toastManager.close`.
    pub fn close(&mut self, id: ToastId, cx: &mut Context<Self>) {
        self.close_with(id, None, cx);
    }

    fn close_with(&mut self, id: ToastId, swipe: Option<SwipeDirection>, cx: &mut Context<Self>) {
        let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) else {
            return;
        };
        if toast.status == Some(ToastTransitionStatus::Ending) {
            return;
        }
        toast.status = Some(ToastTransitionStatus::Ending);
        toast.swipe_dismissed = swipe;
        toast.layout_height = Some(0.);
        toast.timeout_remaining_ms = None;
        toast.visible_timer = None;
        toast.swipe = None;
        self.hovered.remove(&(id, false));
        self.hovered.remove(&(id, true));
        self.relimit();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(TOAST_MOTION_MS + 60))
                .await;
            this.update(cx, |this, cx| {
                this.toasts.retain(|toast| toast.id != id);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Oldest toasts past the limit are `limited` (drawn at opacity 0).
    fn relimit(&mut self) {
        let mut active = 0;
        for toast in &mut self.toasts {
            if toast.status == Some(ToastTransitionStatus::Ending) {
                continue;
            }
            toast.limited = active >= TOAST_LIMIT;
            active += 1;
        }
    }

    fn set_hovered(&mut self, key: (ToastId, bool), hovered: bool, cx: &mut Context<Self>) {
        let was_expanded = self.expanded();
        if hovered {
            self.hovered.insert(key);
        } else {
            self.hovered.remove(&key);
        }
        if was_expanded != self.expanded() {
            cx.notify();
        }
    }

    fn set_height(&mut self, id: ToastId, height: f64, cx: &mut Context<Self>) {
        let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) else {
            return;
        };
        if (toast.measured_height - height).abs() < 0.5 {
            return;
        }
        toast.measured_height = height;
        if toast.status != Some(ToastTransitionStatus::Ending) {
            toast.layout_height = Some(height);
        }
        cx.notify();
    }

    fn begin_swipe(
        &mut self,
        id: ToastId,
        position: gpui_kit::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let directions = self.position.swipe_directions();
        let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) else {
            return;
        };
        if toast.status.is_some() || toast.swipe.is_some() {
            return;
        }
        toast.swipe = Some(SwipeTracker::new(
            directions,
            f64::from(position.x),
            f64::from(position.y),
        ));
        cx.notify();
    }

    fn swipe_move(&mut self, position: gpui_kit::Point<Pixels>, cx: &mut Context<Self>) {
        let mut moved = false;
        for toast in &mut self.toasts {
            if let Some(swipe) = &mut toast.swipe {
                swipe.pointer_move(f64::from(position.x), f64::from(position.y));
                moved = true;
            }
        }
        if moved {
            cx.notify();
        }
    }

    fn swipe_end(&mut self, cx: &mut Context<Self>) {
        let ended = self
            .toasts
            .iter_mut()
            .filter_map(|toast| toast.swipe.take().map(|swipe| (toast.id, swipe)))
            .collect::<Vec<_>>();
        for (id, swipe) in ended {
            match swipe.pointer_up() {
                SwipeEnd::Dismiss(direction) => {
                    if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) {
                        toast.exit_swipe = swipe.offset;
                    }
                    self.close_with(id, Some(direction), cx);
                }
                SwipeEnd::Restore => {}
            }
        }
        cx.notify();
    }

    fn has_timers(&self) -> bool {
        self.toasts.iter().any(|toast| {
            toast.status.is_none()
                && (toast.timeout_remaining_ms.is_some() || toast.visible_timer.is_some())
        })
    }

    /// Runs the dismiss timers while any toast has one. Visible-only timers
    /// follow the app's focus (`ThreadToastVisibleAutoDismiss`); Base UI's
    /// timeouts also wait while the stack is hovered or swiped.
    fn ensure_ticking(&mut self, cx: &mut Context<Self>) {
        if self.ticking || !self.has_timers() {
            return;
        }
        self.ticking = true;
        self.last_tick = Instant::now();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let keep = this.update(cx, |this, cx| this.tick(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let elapsed_ms = now.duration_since(self.last_tick).as_millis() as i64;
        self.last_tick = now;
        let focused = cx.active_window().is_some();
        let timeouts_run = focused && !self.expanded();
        let now_ms = self.now_ms();
        let mut to_close = Vec::new();
        for toast in &mut self.toasts {
            if toast.status.is_some() {
                continue;
            }
            if let Some(remaining) = &mut toast.timeout_remaining_ms
                && timeouts_run
            {
                *remaining -= elapsed_ms;
                if *remaining <= 0 {
                    to_close.push(toast.id);
                    continue;
                }
            }
            if let Some(timer) = &mut toast.visible_timer {
                if focused {
                    if timer.start_running(now_ms) {
                        to_close.push(toast.id);
                    } else if let Some(started) = timer.started_at_ms
                        && now_ms - started >= timer.remaining_ms
                    {
                        timer.remaining_ms = 0;
                        timer.started_at_ms = None;
                        to_close.push(toast.id);
                    }
                } else {
                    timer.pause(now_ms);
                }
            }
        }
        for id in to_close {
            self.close(id, cx);
        }
        let keep = self.has_timers();
        if !keep {
            self.ticking = false;
        }
        keep
    }

    /// Advances every toast's motion to `targets` and returns the frames to draw.
    fn step_motion(
        &mut self,
        targets: &[(ToastId, ToastFrame, ToastFrame, bool)],
        now: Instant,
    ) -> (Vec<ToastFrame>, bool) {
        let mut animating = false;
        let mut frames = Vec::with_capacity(targets.len());
        for (id, target, starting, immediate) in targets {
            let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == *id) else {
                frames.push(*target);
                continue;
            };
            let frame = match &mut toast.motion {
                None => {
                    // Enter from the starting style, unless it is already on screen.
                    toast.motion = Some(Motion {
                        from: *starting,
                        to: *target,
                        started: now,
                    });
                    *starting
                }
                Some(motion) => {
                    let elapsed = now.duration_since(motion.started).as_secs_f64() * 1000.;
                    let current = toast_motion_frame(motion.from, motion.to, elapsed);
                    if *immediate {
                        *motion = Motion {
                            from: *target,
                            to: *target,
                            started: now,
                        };
                        *target
                    } else if !motion.to.near(*target) {
                        *motion = Motion {
                            from: current,
                            to: *target,
                            started: now,
                        };
                        current
                    } else {
                        current
                    }
                }
            };
            if let Some(motion) = &toast.motion
                && now.duration_since(motion.started).as_millis() < u128::from(TOAST_MOTION_MS)
            {
                animating = true;
            }
            frames.push(frame);
        }
        (frames, animating)
    }
}

// ---- managers -------------------------------------------------------------

struct ToastManagers {
    stack: Entity<ToastStore>,
    anchored: Entity<ToastStore>,
}

impl Global for ToastManagers {}

/// Creates the two managers (`toastManager` at `position`, and
/// `anchoredToastManager`).
pub fn init(position: ToastPosition, cx: &mut App) {
    let stack = cx.new(|_| ToastStore::new(position));
    let anchored = cx.new(|_| ToastStore::new(ToastPosition::TopRight));
    cx.set_global(ToastManagers { stack, anchored });
}

/// `toastManager`.
pub fn toast_manager(cx: &App) -> Entity<ToastStore> {
    cx.global::<ToastManagers>().stack.clone()
}

/// `anchoredToastManager`.
pub fn anchored_toast_manager(cx: &App) -> Entity<ToastStore> {
    cx.global::<ToastManagers>().anchored.clone()
}

// ---- body -----------------------------------------------------------------

/// `focus-visible:ring-2 ring-ring ring-offset-1 ring-offset-background`.
/// GPUI has no inset rings, so the band is drawn outside the box with two
/// zero-blur shadows (the wider ring colour first, the offset band over it).
fn focus_ring(theme: &AppTheme) -> Vec<BoxShadow> {
    let band = |spread: f32, color: Hsla| BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(spread),
        inset: false,
    };
    vec![band(3., theme.ring), band(1., theme.background)]
}

/// `toastCornerOrbClass`, the dismiss-only circle on the card corner.
fn corner_orb(
    id: ToastId,
    store: &Entity<ToastStore>,
    on_close: Option<CloseCallback>,
    theme: &AppTheme,
) -> Stateful<Div> {
    let store = store.clone();
    let ring = focus_ring(theme);
    div()
        .id(SharedString::from(format!("toast-{id}-dismiss")))
        .on_mouse_down(MouseButton::Left, no_drag)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(rems(1.5))
        .rounded_full()
        .border_1()
        .border_color(with_alpha(theme.border, 0.6))
        .bg(solid_over(theme.popover, 0.92, theme.background))
        .text_color(theme.muted_foreground)
        .shadow(tokens::shadow_sm(0.1))
        .cursor(CursorStyle::PointingHand)
        .hover(|style| style.bg(theme.popover).text_color(theme.foreground))
        .tab_index(0)
        .focus_visible(move |style| style.shadow(ring))
        .on_click(move |_: &ClickEvent, _, cx| {
            // `handleToastDismissClick`: the data callback, then close.
            if let Some(on_close) = &on_close {
                on_close(cx);
            }
            store.update(cx, |store, cx| store.close(id, cx));
        })
        .child(Icon::new(IconName::X).size(rems(0.75)))
}

fn spinner(color: gpui_kit::Hsla, id: ToastId) -> impl IntoElement {
    svg()
        .path(IconName::LoaderCircle.path())
        .flex_shrink_0()
        .size(rems(1.))
        .text_color(color)
        .with_animation(
            SharedString::from(format!("toast-spin-{id}")),
            Animation::new(Duration::from_secs(1)).repeat(),
            |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

/// `CopyErrorButton`.
fn copy_error_button(
    id: ToastId,
    text: SharedString,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let theme = cx.app_theme().clone();
    let copied = window.use_keyed_state(
        SharedString::from(format!("toast-{id}-copied")),
        cx,
        |_, _| false,
    );
    let is_copied = *copied.read(cx);
    let label = if is_copied {
        "Copied error"
    } else {
        "Copy error"
    };
    // `text-muted-foreground/80 hover:bg-transparent hover:text-muted-foreground`;
    // the check is `text-success` in both states.
    let (icon, color, hover_color) = if is_copied {
        (IconName::Check, theme.success, theme.success)
    } else {
        (
            IconName::Copy,
            with_alpha(theme.muted_foreground, 0.8).into(),
            theme.muted_foreground,
        )
    };
    Button::new(SharedString::from(format!("toast-{id}-copy")))
        .size(ButtonSize::IconMicro)
        .variant(ButtonVariant::GhostMuted)
        .fg(color)
        .hover_fg(hover_color)
        .hover_bg(with_alpha(theme.muted_foreground, 0.))
        .icon(icon)
        .icon_size(rems(0.75))
        .rounded(RADIUS_MD)
        .tooltip(Tooltip::new(label))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
            copied.update(cx, |state, cx| {
                *state = true;
                cx.notify();
            });
            let copied = copied.downgrade();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                copied
                    .update(cx, |state, cx| {
                        *state = false;
                        cx.notify();
                    })
                    .ok();
            })
            .detach();
        })
}

/// `toastExpandablePanelClassName`.
fn expandable_panel(id: ToastId, content: AnyElement) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("toast-{id}-panel")))
        .mt_2()
        .max_h(rems(10.))
        .min_h_0()
        .pr(px(2.))
        .overflow_y_scroll()
        .child(content)
}

/// `ToastExpandableSection`: a text button that reveals the extra body.
fn expandable_section(
    id: ToastId,
    labels: &ExpandableLabels,
    content: AnyElement,
    open: bool,
    toggle: Callback,
    theme: &AppTheme,
) -> impl IntoElement {
    let label = if open {
        labels
            .collapse
            .clone()
            .unwrap_or_else(|| "Hide details".into())
    } else {
        labels
            .expand
            .clone()
            .unwrap_or_else(|| "Show details".into())
    };
    let muted = theme.muted_foreground;
    let foreground = theme.foreground;
    let ring = focus_ring(theme);
    div()
        .flex()
        .flex_col()
        .items_start()
        .min_w_0()
        .child(
            div()
                .id(SharedString::from(format!("toast-{id}-expand")))
                .on_mouse_down(MouseButton::Left, no_drag)
                .flex()
                .items_center()
                .gap_1()
                .rounded(RADIUS_MD)
                .py(px(2.))
                .text_size(TEXT_XS.size)
                .line_height(TEXT_XS.line_height)
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .cursor(CursorStyle::PointingHand)
                .hover(move |style| style.text_color(foreground))
                .tab_index(0)
                .focus_visible(move |style| style.shadow(ring))
                .on_click(move |_, window, cx| toggle(window, cx))
                .child(
                    Icon::new(if open {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .size(rems(0.875))
                    .color(with_alpha(muted, 0.8).into()),
                )
                .child(label),
        )
        .when(open, |this| this.child(expandable_panel(id, content)))
}

/// The card body, `ToastBodyContent` inside `Toast.Content`.
fn toast_body(
    id: ToastId,
    options: &ToastOptions,
    hide_content: bool,
    tooltip_style: bool,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let theme = cx.app_theme().clone();
    if tooltip_style {
        // `px-2 py-1`, title only (`text-xs` from the root).
        return div()
            .px_2()
            .py_1()
            .child(div().min_w_0().child(options.title.clone()));
    }
    let action_children = options.action_children();
    let body = options.body_descriptor(&action_children);
    let stacked = body.stacked_action_layout;
    let end_pad = if body.inline_end_pad_is_pr6 {
        rems(1.5)
    } else {
        rems(2.5)
    };
    let description = options.description.clone();
    let clamped = error_description_is_clamped(options.type_, description.as_deref());
    let expand_open = window.use_keyed_state(
        SharedString::from(format!("toast-{id}-expand-open")),
        cx,
        |_, _| false,
    );
    let open = *expand_open.read(cx);
    let toggle: Callback = {
        let expand_open = expand_open.clone();
        Rc::new(move |_, cx| {
            expand_open.update(cx, |open, cx| {
                *open = !*open;
                cx.notify();
            });
        })
    };
    let expandable = options
        .data
        .expandable_content
        .as_ref()
        .map(|content| content(window, cx));

    let line_height = TEXT_SM.line_height;
    let icon = if let Some(leading) = options.data.leading_icon {
        Some(
            div()
                .flex()
                .h(line_height)
                .w(rems(1.))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .child(Icon::new(leading).size(rems(1.)))
                .into_any_element(),
        )
    } else {
        options.type_.map(|type_| {
            let color = icon_color(type_, &theme);
            let glyph = if type_ == ToastType::Loading {
                spinner(color, id).into_any_element()
            } else {
                Icon::new(toast_icon(type_))
                    .size(rems(1.))
                    .color(color)
                    .into_any_element()
            };
            div()
                .flex()
                .h(line_height)
                .w(rems(1.))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .child(glyph)
                .into_any_element()
        })
    };

    let description_el = description.clone().map(|text| {
        let mut line = div()
            .min_w_0()
            .text_color(theme.muted_foreground)
            .child(text);
        if clamped {
            line = line.line_clamp(4);
        }
        line
    });

    let mut column = div()
        .flex()
        .flex_col()
        .min_h_0()
        .min_w_0()
        .gap(px(2.))
        .when(stacked, |this| this.pr(rems(1.25)))
        .when(!stacked, |this| this.flex_1())
        .child(
            div().flex().min_w_0().gap_2().children(icon).child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::MEDIUM)
                    .child(options.title.clone()),
            ),
        );
    match expandable {
        None => {
            column = column.children(description_el);
        }
        Some(content) if !options.data.expandable_description_trigger => {
            column = column.children(description_el).child(expandable_section(
                id,
                &options.data.expandable_labels,
                content,
                open,
                toggle,
                &theme,
            ));
        }
        Some(content) => {
            // The summary and the chevron are one disclosure.
            let label = if open {
                options
                    .data
                    .expandable_labels
                    .collapse
                    .clone()
                    .unwrap_or_else(|| "Hide details".into())
            } else {
                options
                    .data
                    .expandable_labels
                    .expand
                    .clone()
                    .unwrap_or_else(|| "Show details".into())
            };
            let hover = with_alpha(theme.muted, 0.4);
            let toggle_click = toggle.clone();
            let ring = focus_ring(&theme);
            column = column
                .child(
                    div()
                        .id(SharedString::from(format!("toast-{id}-disclosure")))
                        .on_mouse_down(MouseButton::Left, no_drag)
                        .flex()
                        .min_w_0()
                        .w_full()
                        .items_start()
                        .gap(rems(0.375))
                        .rounded(RADIUS_DEFAULT)
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |style| style.bg(hover))
                        .tab_index(0)
                        .focus_visible(move |style| style.shadow(ring))
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                        .on_click(move |_, window, cx| toggle_click(window, cx))
                        .child(div().min_w_0().flex_1().children(description_el))
                        .child(
                            div().mt(px(2.)).child(
                                Icon::new(if open {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(rems(0.875))
                                .color(with_alpha(theme.muted_foreground, 0.8).into()),
                            ),
                        ),
                )
                .when(open, |this| this.child(expandable_panel(id, content)));
        }
    }

    let mut content = div()
        .flex()
        .min_h_0()
        .pl(rems(0.875))
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .when(hide_content, |this| this.invisible());
    content = if stacked {
        content.flex_col().gap_2().py(rems(0.625)).pr(rems(0.875))
    } else {
        content
            .items_center()
            .justify_between()
            .gap(rems(0.375))
            .py_3()
            .pr(end_pad)
    }
    .child(column);

    if body.has_trailing_controls {
        let secondary_variant = button_variant(body.secondary_action_variant);
        let mut controls = div()
            .flex()
            .items_center()
            .gap(rems(0.375))
            .when(stacked, |this| this.w_full().justify_end())
            .when(!stacked, |this| this.flex_shrink_0());
        if let Some(text) = body.copy_error_text.clone() {
            controls = controls.child(copy_error_button(id, text.into(), window, cx));
        }
        for action in options
            .data
            .additional_actions
            .iter()
            .chain(options.data.secondary_action.iter())
        {
            controls = controls.child(action_button(action, secondary_variant, id));
        }
        if let Some(action) = options
            .action
            .as_ref()
            .filter(|action| !action.label.is_empty())
        {
            controls = controls.child(
                action_button(action, button_variant(body.action_variant), id).flex_shrink_0(),
            );
        }
        content = content.child(controls);
    }
    content
}

fn action_button(action: &ToastActionSpec, variant: ButtonVariant, id: ToastId) -> Button {
    let on_click = action.on_click.clone();
    let mut button = Button::new(SharedString::from(format!("toast-{id}-{}", action.id)))
        .size(ButtonSize::Xs)
        .variant(variant)
        .label(action.label.clone());
    if let Some(on_click) = on_click {
        button = button.on_click(move |_, window, cx| on_click(window, cx));
    }
    button
}

// ---- stacked viewport -----------------------------------------------------

/// `Toasts`: the stacked viewport. Mount it over the window root.
pub struct ToastViewport {
    store: Entity<ToastStore>,
    _observe: Subscription,
}

pub fn toasts(cx: &mut App) -> Entity<ToastViewport> {
    let store = toast_manager(cx);
    cx.new(|cx| ToastViewport {
        _observe: cx.observe(&store, |_, _, cx| cx.notify()),
        store,
    })
}

struct VisibleToast {
    id: ToastId,
    options: ToastOptions,
    visible_index: usize,
    frame: ToastFrame,
    hide_content: bool,
}

impl Render for ToastViewport {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let window_width = f64::from(window.viewport_size().width);
        let inset = toast_inset(window_width);
        let width = toast_viewport_width(window_width);
        let store = self.store.clone();
        let (position, expanded, active_thread, scoped) = {
            let state = store.read(cx);
            let scoped = state
                .toasts
                .iter()
                .filter(|toast| {
                    should_render_thread_scoped_toast(
                        toast.options.data.scope.as_ref(),
                        state.active_thread.as_ref(),
                    )
                })
                .map(|toast| toast.id)
                .collect::<Vec<_>>();
            (
                state.position,
                state.expanded(),
                state.active_thread.clone(),
                scoped,
            )
        };
        let _ = active_thread;
        let is_top = position.is_top();
        let layout = {
            let state = store.read(cx);
            build_visible_toast_layout(
                state
                    .toasts
                    .iter()
                    .filter(|toast| scoped.contains(&toast.id))
                    .map(|toast| ToastLayoutInput {
                        toast: toast.id,
                        height: toast.layout_height,
                        transition_status: toast.status,
                    })
                    .collect(),
            )
        };
        let count = layout.items.len();
        let now = Instant::now();
        let mut targets = Vec::with_capacity(count);
        let mut visible = Vec::with_capacity(count);
        {
            let state = store.read(cx);
            for item in &layout.items {
                let Some(toast) = state.toasts.iter().find(|toast| toast.id == item.toast) else {
                    continue;
                };
                let swipe = toast
                    .swipe
                    .as_ref()
                    .map_or(toast.exit_swipe, |swipe| swipe.offset);
                let rest = toast_resting_frame(ToastFrameInput {
                    position,
                    visible_index: item.visible_index,
                    offset_y: item.offset_y,
                    frontmost_height: layout.frontmost_height,
                    height: toast.measured_height,
                    viewport_width: width,
                    expanded,
                    limited: toast.limited,
                    swipe,
                });
                let ending = toast.status == Some(ToastTransitionStatus::Ending);
                let target = if ending {
                    toast_ending_frame(rest, position, width, inset, toast.swipe_dismissed)
                } else {
                    rest
                };
                let starting = toast_starting_frame(rest, position, width, inset);
                targets.push((item.toast, target, starting, toast.swipe.is_some()));
                visible.push(VisibleToast {
                    id: toast.id,
                    options: toast.options.clone(),
                    visible_index: item.visible_index,
                    frame: target,
                    hide_content: should_hide_collapsed_toast_content(item.visible_index, count)
                        && !expanded,
                });
            }
        }
        let (frames, animating) = store.update(cx, |state, _| state.step_motion(&targets, now));
        if animating {
            window.request_animation_frame();
        }
        let swiping = store
            .read(cx)
            .toasts
            .iter()
            .any(|toast| toast.swipe.is_some());

        // Frontmost paints last, so draw the stack back to front.
        let mut cards = Vec::with_capacity(count);
        for (toast, frame) in visible.into_iter().zip(frames).rev() {
            cards.push(toast_card(
                &toast, frame, is_top, expanded, &store, &theme, window, cx,
            ));
        }

        let left = if position.is_right() {
            window_width - inset - width
        } else if position.is_left() {
            inset
        } else {
            (window_width - width) / 2.
        };
        let mut root = div()
            .absolute()
            .left(px(left as f32))
            .w(px(width as f32))
            .h_0()
            .children(cards);
        root = if is_top {
            // `--toast-header-offset: var(--workspace-topbar-height)`
            root.top(px(inset as f32) + WORKSPACE_TOPBAR_HEIGHT)
        } else {
            root.bottom(px(inset as f32))
        };
        if swiping {
            let move_store = store.clone();
            let up_store = store.clone();
            root = root.child(
                canvas(
                    |_, _, _| {},
                    move |_: Bounds<Pixels>, _, window, _| {
                        let store = move_store.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                store.update(cx, |store, cx| store.swipe_move(event.position, cx));
                            }
                        });
                        let store = up_store.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                store.update(cx, |store, cx| store.swipe_end(cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            );
        }
        deferred(root).with_priority(3)
    }
}

#[allow(clippy::too_many_arguments)]
fn toast_card(
    toast: &VisibleToast,
    frame: ToastFrame,
    is_top: bool,
    expanded: bool,
    store: &Entity<ToastStore>,
    theme: &AppTheme,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let id = toast.id;
    let body = toast_body(id, &toast.options, toast.hide_content, false, window, cx);
    let measure = store.clone();
    let hover = store.clone();
    let gap_hover = store.clone();
    let swipe_store = store.clone();
    let on_close = toast.options.data.on_close.clone();
    let mut card = div()
        .on_children_prepainted(move |bounds, _, cx| {
            let Some(content) = bounds.first() else {
                return;
            };
            // Content height plus the 1px border on each side.
            let height = f64::from(content.size.height) + 2.;
            measure.update(cx, |store, cx| store.set_height(id, height, cx));
        })
        .id(SharedString::from(format!("toast-{id}")))
        .absolute()
        .left(px(frame.dx as f32))
        .w(px(frame.width as f32))
        .when(
            toast.visible_index == 0 || expanded || frame.height > 0.,
            |this| this.h(px((frame.height) as f32)),
        )
        .opacity(frame.opacity as f32)
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(dropdown_glass_border(theme))
        .bg(dropdown_glass_background(theme))
        .text_color(theme.popover_foreground)
        .shadow(tokens::shadow_xl(0.25))
        .on_hover(move |hovered, _, cx| {
            hover.update(cx, |store, cx| store.set_hovered((id, false), *hovered, cx));
        })
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
            swipe_store.update(cx, |store, cx| store.begin_swipe(id, event.position, cx));
        });
    card = if is_top {
        card.top(px(frame.edge as f32))
    } else {
        card.bottom(px(frame.edge as f32))
    };
    card = card.child(body);
    // `after:` gap fill keeps the hover alive across the gap between cards.
    card = card.child(
        div()
            .id(SharedString::from(format!("toast-{id}-gap")))
            .absolute()
            .left_0()
            .w_full()
            .h(px(tokens_gap() as f32 + 1.))
            .when(is_top, |this| this.top_full())
            .when(!is_top, |this| this.bottom_full())
            .on_hover(move |hovered, _, cx| {
                gap_hover.update(cx, |store, cx| store.set_hovered((id, true), *hovered, cx));
            }),
    );
    card = card.child(
        div()
            .absolute()
            .top(rems(-0.375))
            .right(rems(-0.375))
            .child(corner_orb(id, store, on_close, theme)),
    );
    card.into_any_element()
}

fn tokens_gap() -> f64 {
    super::toast_logic::TOAST_GAP_PX
}

// ---- anchored toasts ------------------------------------------------------

/// `AnchoredToasts`: toasts positioned against an element. Mount over the window root.
pub struct AnchoredToastViewport {
    store: Entity<ToastStore>,
    _observe: Subscription,
}

pub fn anchored_toasts(cx: &mut App) -> Entity<AnchoredToastViewport> {
    let store = anchored_toast_manager(cx);
    cx.new(|cx| AnchoredToastViewport {
        _observe: cx.observe(&store, |_, _, cx| cx.notify()),
        store,
    })
}

impl Render for AnchoredToastViewport {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let store = self.store.clone();
        let toasts = {
            let state = store.read(cx);
            state
                .toasts
                .iter()
                .filter(|toast| {
                    should_render_thread_scoped_toast(
                        toast.options.data.scope.as_ref(),
                        state.active_thread.as_ref(),
                    )
                })
                .filter(|toast| toast.options.anchor.is_some())
                .map(|toast| (toast.id, toast.options.clone(), toast.status))
                .collect::<Vec<_>>()
        };
        let mut layers = Vec::with_capacity(toasts.len());
        for (id, options, status) in toasts {
            let Some(anchor) = options.anchor else {
                continue;
            };
            let tooltip_style = options.data.tooltip_style;
            let on_close = options.data.on_close.clone();
            let body = toast_body(id, &options, false, tooltip_style, window, cx);
            let ending = status == Some(ToastTransitionStatus::Ending);
            let mut popup = div()
                .id(SharedString::from(format!("anchored-toast-{id}")))
                .relative()
                .max_w(rems(16.))
                .border_1()
                .border_color(dropdown_glass_border(&theme))
                .bg(dropdown_glass_background(&theme))
                .text_color(theme.popover_foreground)
                .text_size(TEXT_XS.size)
                .line_height(TEXT_XS.line_height)
                .shadow(tokens::shadow_xl(0.25))
                .rounded(if tooltip_style { RADIUS_MD } else { RADIUS_LG })
                .child(body);
            if !tooltip_style {
                popup = popup.child(
                    div()
                        .absolute()
                        .top(rems(-0.375))
                        .right(rems(-0.375))
                        .child(corner_orb(id, &store, on_close, &theme)),
                );
            }
            // `transition-[scale,opacity]` over 150ms; scale needs transforms, so only the fade runs.
            let popup = popup.with_animation(
                SharedString::from(format!("anchored-toast-{id}-{ending}")),
                Animation::new(Duration::from_millis(150))
                    .with_easing(|t| cubic_bezier(0.4, 0., 0.2, 1., t)),
                move |el, delta| el.opacity(if ending { 1. - delta } else { delta }),
            );
            let bounds = anchor.bounds;
            let origin = point(
                bounds.origin.x + bounds.size.width / 2.,
                bounds.origin.y + bounds.size.height + px(anchor.side_offset),
            );
            layers.push(
                deferred(
                    anchored()
                        .position(origin)
                        .anchor(Anchor::TopCenter)
                        .snap_to_window_with_margin(px(8.))
                        .child(popup),
                )
                .with_priority(3)
                .into_any_element(),
            );
        }
        div().absolute().size_0().children(layers)
    }
}
