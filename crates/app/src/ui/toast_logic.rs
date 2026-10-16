//! Port of upstream/apps/web/src/components/ui/toast.logic.ts,
//! toastHelpers.ts, and the pure layout/body rules in toast.tsx
//! (`deriveToastBodyDescriptor`, `errorDescriptionClampClass`, the collapsed
//! and expanded stack transforms, and the visible-dismiss timer).

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ThreadId};
use turin_shared::js_string::utf16_len;

/// `--toast-peek` / `--toast-gap`: `--spacing(3)` = 12px at the default rem.
pub const TOAST_PEEK_PX: f64 = 12.;
pub const TOAST_GAP_PX: f64 = 12.;

/// Long error bodies clamp visually; the clipboard still gets the full string.
pub const ERROR_DESCRIPTION_CLAMP_MIN_CHARS: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastTransitionStatus {
    Starting,
    Ending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastType {
    Error,
    Info,
    Loading,
    Success,
    Warning,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastActionVariant {
    #[default]
    Default,
    Destructive,
    DestructiveOutline,
    Ghost,
    Link,
    Outline,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastActionLayout {
    Inline,
    StackedEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastPriority {
    Low,
    High,
}

/// `actionProps.children`. Absent, null, false, and `""` are not a visible CTA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastActionChildren {
    Absent,
    Null,
    False,
    Empty,
    Text(String),
}

impl ToastActionChildren {
    pub fn is_visible(&self) -> bool {
        matches!(self, Self::Text(text) if !text.is_empty())
    }
}

/// `hasVisibleToastAction`. `None` is a missing or non-object payload.
pub fn has_visible_toast_action(action_props: Option<&ToastActionChildren>) -> bool {
    action_props.is_some_and(ToastActionChildren::is_visible)
}

pub fn should_hide_collapsed_toast_content(
    visible_toast_index: usize,
    visible_toast_count: usize,
) -> bool {
    if visible_toast_count <= 1 {
        return false;
    }
    visible_toast_index > 0
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToastLayoutInput<T> {
    pub toast: T,
    pub height: Option<f64>,
    pub transition_status: Option<ToastTransitionStatus>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleToastLayoutItem<T> {
    pub toast: T,
    pub visible_index: usize,
    pub offset_y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleToastLayout<T> {
    pub frontmost_height: f64,
    pub items: Vec<VisibleToastLayoutItem<T>>,
}

fn normalize_toast_height(height: Option<f64>) -> f64 {
    match height {
        Some(height) if height.is_finite() && height > 0. => height,
        _ => 0.,
    }
}

pub fn build_visible_toast_layout<T>(
    visible_toasts: Vec<ToastLayoutInput<T>>,
) -> VisibleToastLayout<T> {
    let mut full_index = 0usize;
    let mut full_offset_y = 0.;
    let mut live_index = 0usize;
    let mut live_offset_y = 0.;

    let items = visible_toasts
        .iter()
        .map(|toast| {
            let height = normalize_toast_height(toast.height);
            if toast.transition_status == Some(ToastTransitionStatus::Ending) {
                let item = (full_index, full_offset_y);
                full_offset_y += height;
                full_index += 1;
                item
            } else {
                let item = (live_index, live_offset_y);
                full_offset_y += height;
                full_index += 1;
                live_offset_y += height;
                live_index += 1;
                item
            }
        })
        .collect::<Vec<_>>();

    let frontmost_live = visible_toasts
        .iter()
        .find(|toast| toast.transition_status != Some(ToastTransitionStatus::Ending));

    VisibleToastLayout {
        frontmost_height: normalize_toast_height(frontmost_live.and_then(|toast| toast.height)),
        items: visible_toasts
            .into_iter()
            .zip(items)
            .map(
                |(toast, (visible_index, offset_y))| VisibleToastLayoutItem {
                    toast: toast.toast,
                    visible_index,
                    offset_y,
                },
            )
            .collect(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadToastScope {
    pub thread_ref: Option<ScopedThreadRef>,
    pub thread_id: Option<ThreadId>,
}

pub fn should_render_thread_scoped_toast(
    data: Option<&ThreadToastScope>,
    active_thread_ref: Option<&ScopedThreadRef>,
) -> bool {
    if let Some(thread_ref) = data.and_then(|data| data.thread_ref.as_ref()) {
        return active_thread_ref.is_some_and(|active| {
            active.environment_id == thread_ref.environment_id
                && active.thread_id == thread_ref.thread_id
        });
    }
    let Some(toast_thread_id) = data.and_then(|data| data.thread_id.as_ref()) else {
        return true;
    };
    active_thread_ref.is_some_and(|active| &active.thread_id == toast_thread_id)
}

/// Collapsed stack: `scale = max(0, 1 - index * 0.1)`.
pub fn toast_collapsed_scale(index: usize) -> f64 {
    (1. - index as f64 * 0.1).max(0.)
}

/// Collapsed `translateY` at the top (`swipe` is 0). Bottom negates it.
pub fn toast_collapsed_translate_y(index: usize, frontmost_height: f64, top: bool) -> f64 {
    let scale = toast_collapsed_scale(index);
    let shrink = 1. - scale;
    let offset = index as f64 * TOAST_PEEK_PX + shrink * frontmost_height;
    if top { offset } else { -offset }
}

/// Expanded `translateY` (`--toast-calc-offset-y` with swipe 0).
pub fn toast_expanded_translate_y(offset_y: f64, index: usize, top: bool) -> f64 {
    let gap = index as f64 * TOAST_GAP_PX;
    if top { offset_y + gap } else { -offset_y - gap }
}

pub fn error_description_is_clamped(type_: Option<ToastType>, description: Option<&str>) -> bool {
    type_ == Some(ToastType::Error)
        && description.is_some_and(|text| utf16_len(text) >= ERROR_DESCRIPTION_CLAMP_MIN_CHARS)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastBodyInput<'a> {
    pub type_: Option<ToastType>,
    pub description: Option<&'a str>,
    pub action: Option<&'a ToastActionChildren>,
    pub action_layout: Option<ToastActionLayout>,
    pub action_variant: Option<ToastActionVariant>,
    pub secondary_action_variant: Option<ToastActionVariant>,
    pub hide_copy_button: bool,
    pub additional_action_count: usize,
    pub has_secondary_action: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastBodyDescriptor {
    pub icon: Option<ToastType>,
    pub stacked_action_layout: bool,
    pub action_variant: ToastActionVariant,
    pub secondary_action_variant: ToastActionVariant,
    pub copy_error_text: Option<String>,
    pub has_trailing_controls: bool,
    /// `pr-6` when trailing controls exist, otherwise `pr-10`.
    pub inline_end_pad_is_pr6: bool,
}

pub fn derive_toast_body(input: ToastBodyInput<'_>) -> ToastBodyDescriptor {
    let stacked_action_layout = has_visible_toast_action(input.action)
        && input.action_layout == Some(ToastActionLayout::StackedEnd);
    let copy_error_text = if input.type_ == Some(ToastType::Error)
        && input.description.is_some()
        && !input.hide_copy_button
    {
        input.description.map(str::to_owned)
    } else {
        None
    };
    let has_trailing_controls = copy_error_text.is_some()
        || has_visible_toast_action(input.action)
        || input.additional_action_count > 0
        || input.has_secondary_action;
    ToastBodyDescriptor {
        icon: input.type_,
        stacked_action_layout,
        action_variant: input.action_variant.unwrap_or_default(),
        secondary_action_variant: input
            .secondary_action_variant
            .unwrap_or(ToastActionVariant::Outline),
        copy_error_text,
        has_trailing_controls,
        inline_end_pad_is_pr6: has_trailing_controls,
    }
}

/// `hiddenToastActionProps`: a defined empty action so an update can clear a CTA.
pub fn hidden_toast_action_props() -> ToastActionChildren {
    ToastActionChildren::Null
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackedThreadToast {
    pub type_: ToastType,
    pub title: String,
    pub description: Option<String>,
    pub timeout: Option<u64>,
    pub priority: Option<ToastPriority>,
    pub action_props: Option<ToastActionChildren>,
    pub action_variant: Option<ToastActionVariant>,
    pub action_layout: ToastActionLayout,
}

pub struct StackedThreadToastOptions {
    pub type_: ToastType,
    pub title: String,
    pub description: Option<String>,
    pub timeout: Option<u64>,
    pub priority: Option<ToastPriority>,
    pub action_props: Option<ToastActionChildren>,
    pub action_variant: Option<ToastActionVariant>,
}

/// `stackedThreadToast`: `actionLayout` is always `stacked-end`, applied last.
pub fn stacked_thread_toast(options: StackedThreadToastOptions) -> StackedThreadToast {
    StackedThreadToast {
        type_: options.type_,
        title: options.title,
        description: options.description,
        timeout: options.timeout,
        priority: options.priority,
        action_props: options.action_props,
        action_variant: options.action_variant,
        action_layout: ToastActionLayout::StackedEnd,
    }
}

/// Visible-only auto-dismiss. Pauses while the window is hidden or blurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleDismissTimer {
    pub remaining_ms: i64,
    pub started_at_ms: Option<i64>,
    pub closed: bool,
}

impl VisibleDismissTimer {
    pub fn start(dismiss_after_visible_ms: i64) -> Option<Self> {
        if dismiss_after_visible_ms <= 0 {
            return None;
        }
        Some(Self {
            remaining_ms: dismiss_after_visible_ms,
            started_at_ms: None,
            closed: false,
        })
    }

    pub fn pause(&mut self, now_ms: i64) {
        let Some(started) = self.started_at_ms else {
            return;
        };
        self.remaining_ms = (self.remaining_ms - (now_ms - started)).max(0);
        self.started_at_ms = None;
    }

    /// Returns true when the timer should close the toast immediately.
    pub fn start_running(&mut self, now_ms: i64) -> bool {
        if self.closed || self.started_at_ms.is_some() {
            return false;
        }
        if self.remaining_ms <= 0 {
            self.closed = true;
            return true;
        }
        self.started_at_ms = Some(now_ms);
        false
    }
}

pub fn thread_ref(environment_id: &str, thread_id: &str) -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: EnvironmentId::new(environment_id).expect("environment id"),
        thread_id: ThreadId::new(thread_id).expect("thread id"),
    }
}

// ---- viewport, swipe, and motion rules (toast.tsx `Toasts`, Base UI `ToastRoot`) ----

/// Base UI `ToastProvider` defaults.
pub const DEFAULT_TOAST_TIMEOUT_MS: u64 = 5000;
pub const TOAST_LIMIT: usize = 3;

/// `max-w-90`.
pub const TOAST_MAX_WIDTH_PX: f64 = 360.;
/// `--toast-inset`: `--spacing(4)`, and `--spacing(8)` from the `sm` breakpoint (640px).
pub fn toast_inset(window_width: f64) -> f64 {
    if window_width >= 640. { 32. } else { 16. }
}

/// `w-[calc(100%-var(--toast-inset)*2)] max-w-90`.
pub fn toast_viewport_width(window_width: f64) -> f64 {
    (window_width - toast_inset(window_width) * 2.).clamp(0., TOAST_MAX_WIDTH_PX)
}

/// `[transition:transform_.5s_cubic-bezier(.22,1,.36,1),opacity_.5s,height_.15s]`.
pub const TOAST_MOTION_MS: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl ToastPosition {
    pub fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopCenter | Self::TopRight)
    }

    pub fn is_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }

    pub fn is_right(self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }

    pub fn is_center(self) -> bool {
        matches!(self, Self::TopCenter | Self::BottomCenter)
    }

    /// `swipeDirection` on `Toast.Root`.
    pub fn swipe_directions(self) -> Vec<SwipeDirection> {
        let vertical = if self.is_top() {
            SwipeDirection::Up
        } else {
            SwipeDirection::Down
        };
        if self.is_center() {
            vec![vertical]
        } else if self.is_left() {
            vec![SwipeDirection::Left, vertical]
        } else {
            vec![SwipeDirection::Right, vertical]
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

const SWIPE_THRESHOLD: f64 = 40.;
const REVERSE_CANCEL_THRESHOLD: f64 = 10.;
const MIN_DRAG_THRESHOLD: f64 = 1.;
const OPPOSITE_DIRECTION_DAMPING_FACTOR: f64 = 0.5;

/// `getDisplacement`: movement along `direction`.
pub fn swipe_displacement(direction: SwipeDirection, dx: f64, dy: f64) -> f64 {
    match direction {
        SwipeDirection::Up => -dy,
        SwipeDirection::Down => dy,
        SwipeDirection::Left => -dx,
        SwipeDirection::Right => dx,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockedAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwipeEnd {
    Dismiss(SwipeDirection),
    Restore,
}

/// Base UI's swipe-to-dismiss gesture on one toast: axis lock, intended
/// direction, change-of-mind cancel, damped motion against the allowed
/// directions, and the 40px release threshold.
#[derive(Clone, Debug)]
pub struct SwipeTracker {
    directions: Vec<SwipeDirection>,
    start: (f64, f64),
    cancel_baseline: (f64, f64),
    last: (f64, f64),
    intended: Option<SwipeDirection>,
    max_displacement: f64,
    cancelled: bool,
    locked: Option<LockedAxis>,
    first_move: bool,
    /// The offset applied to the toast, in pixels.
    pub offset: (f64, f64),
    pub is_real_swipe: bool,
}

impl SwipeTracker {
    pub fn new(directions: Vec<SwipeDirection>, x: f64, y: f64) -> Self {
        Self {
            directions,
            start: (x, y),
            cancel_baseline: (x, y),
            last: (x, y),
            intended: None,
            max_displacement: 0.,
            cancelled: false,
            locked: None,
            first_move: true,
            offset: (0., 0.),
            is_real_swipe: false,
        }
    }

    fn allows(&self, direction: SwipeDirection) -> bool {
        self.directions.contains(&direction)
    }

    fn damp(&self, dx: f64, dy: f64) -> (f64, f64) {
        let damp = |value: f64| {
            if value > 0. {
                value.powf(OPPOSITE_DIRECTION_DAMPING_FACTOR)
            } else {
                -(value.abs().powf(OPPOSITE_DIRECTION_DAMPING_FACTOR))
            }
        };
        let (left, right) = (
            self.allows(SwipeDirection::Left),
            self.allows(SwipeDirection::Right),
        );
        let (up, down) = (
            self.allows(SwipeDirection::Up),
            self.allows(SwipeDirection::Down),
        );
        let mut x = dx;
        let mut y = dy;
        if !left && !right {
            x = damp(dx);
        } else {
            if !right && dx > 0. {
                x = damp(dx);
            }
            if !left && dx < 0. {
                x = damp(dx);
            }
        }
        if !up && !down {
            y = damp(dy);
        } else {
            if !down && dy > 0. {
                y = damp(dy);
            }
            if !up && dy < 0. {
                y = damp(dy);
            }
        }
        (x, y)
    }

    /// `handlePointerMove`. Returns the new toast offset.
    pub fn pointer_move(&mut self, x: f64, y: f64) -> (f64, f64) {
        if self.first_move {
            // Start from the first move to absorb the press-to-move delay.
            self.start = (x, y);
            self.first_move = false;
        }
        let (movement_x, movement_y) = (x - self.last.0, y - self.last.1);
        self.last = (x, y);
        if (movement_y < 0. && y > self.cancel_baseline.1)
            || (movement_y > 0. && y < self.cancel_baseline.1)
        {
            self.cancel_baseline.1 = y;
        }
        if (movement_x < 0. && x > self.cancel_baseline.0)
            || (movement_x > 0. && x < self.cancel_baseline.0)
        {
            self.cancel_baseline.0 = x;
        }
        let delta_x = x - self.start.0;
        let delta_y = y - self.start.1;
        let cancel_dx = x - self.cancel_baseline.0;
        let cancel_dy = y - self.cancel_baseline.1;
        // State set during this event is not visible until the next one.
        let locked = self.locked;
        if !self.is_real_swipe && delta_x.hypot(delta_y) >= MIN_DRAG_THRESHOLD {
            self.is_real_swipe = true;
            if self.locked.is_none() {
                let horizontal =
                    self.allows(SwipeDirection::Left) || self.allows(SwipeDirection::Right);
                let vertical = self.allows(SwipeDirection::Up) || self.allows(SwipeDirection::Down);
                if horizontal && vertical {
                    self.locked = Some(if delta_x.abs() > delta_y.abs() {
                        LockedAxis::Horizontal
                    } else {
                        LockedAxis::Vertical
                    });
                }
            }
        }
        match self.intended {
            None => {
                let candidate = match locked {
                    Some(LockedAxis::Vertical) => {
                        if delta_y > 0. {
                            Some(SwipeDirection::Down)
                        } else if delta_y < 0. {
                            Some(SwipeDirection::Up)
                        } else {
                            None
                        }
                    }
                    Some(LockedAxis::Horizontal) => {
                        if delta_x > 0. {
                            Some(SwipeDirection::Right)
                        } else if delta_x < 0. {
                            Some(SwipeDirection::Left)
                        } else {
                            None
                        }
                    }
                    None => {
                        if delta_x.abs() >= delta_y.abs() {
                            Some(if delta_x > 0. {
                                SwipeDirection::Right
                            } else {
                                SwipeDirection::Left
                            })
                        } else {
                            Some(if delta_y > 0. {
                                SwipeDirection::Down
                            } else {
                                SwipeDirection::Up
                            })
                        }
                    }
                };
                if let Some(candidate) = candidate.filter(|candidate| self.allows(*candidate)) {
                    self.intended = Some(candidate);
                    self.max_displacement = swipe_displacement(candidate, delta_x, delta_y);
                }
            }
            Some(direction) => {
                let displacement = swipe_displacement(direction, cancel_dx, cancel_dy);
                let one_axis = !(self.allows(SwipeDirection::Left)
                    && self.allows(SwipeDirection::Right))
                    && !(self.allows(SwipeDirection::Up) && self.allows(SwipeDirection::Down));
                if displacement > SWIPE_THRESHOLD {
                    self.cancelled = false;
                } else if one_axis
                    && self.max_displacement - displacement >= REVERSE_CANCEL_THRESHOLD
                {
                    self.cancelled = true;
                }
            }
        }
        let (damped_x, damped_y) = self.damp(delta_x, delta_y);
        let horizontal = self.allows(SwipeDirection::Left) || self.allows(SwipeDirection::Right);
        let vertical = self.allows(SwipeDirection::Up) || self.allows(SwipeDirection::Down);
        let mut offset = (0., 0.);
        match locked {
            Some(LockedAxis::Horizontal) => {
                if horizontal {
                    offset.0 += damped_x;
                }
            }
            Some(LockedAxis::Vertical) => {
                if vertical {
                    offset.1 += damped_y;
                }
            }
            None => {
                if horizontal {
                    offset.0 += damped_x;
                }
                if vertical {
                    offset.1 += damped_y;
                }
            }
        }
        self.offset = offset;
        offset
    }

    /// `handleSwipeEnd` on pointer release.
    pub fn pointer_up(&self) -> SwipeEnd {
        if self.cancelled {
            return SwipeEnd::Restore;
        }
        let (dx, dy) = self.offset;
        for direction in &self.directions {
            let past = match direction {
                SwipeDirection::Right => dx > SWIPE_THRESHOLD,
                SwipeDirection::Left => dx < -SWIPE_THRESHOLD,
                SwipeDirection::Down => dy > SWIPE_THRESHOLD,
                SwipeDirection::Up => dy < -SWIPE_THRESHOLD,
            };
            if past {
                return SwipeEnd::Dismiss(*direction);
            }
        }
        SwipeEnd::Restore
    }
}

/// Where a toast is drawn: `dx` is the horizontal translate, `edge` the
/// distance from the viewport's anchored edge (top or bottom), and the
/// width/height come from the collapsed scale or the measured content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToastFrame {
    pub dx: f64,
    pub edge: f64,
    pub width: f64,
    pub height: f64,
    pub opacity: f64,
}

impl ToastFrame {
    pub fn lerp(self, to: Self, t: f64) -> Self {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Self {
            dx: mix(self.dx, to.dx),
            edge: mix(self.edge, to.edge),
            width: mix(self.width, to.width),
            height: mix(self.height, to.height),
            opacity: mix(self.opacity, to.opacity),
        }
    }

    pub fn near(self, other: Self) -> bool {
        (self.dx - other.dx).abs() < 0.25
            && (self.edge - other.edge).abs() < 0.25
            && (self.width - other.width).abs() < 0.25
            && (self.height - other.height).abs() < 0.25
            && (self.opacity - other.opacity).abs() < 0.01
    }
}

/// Everything `Toasts` feeds one root's transform for a single toast.
#[derive(Clone, Copy, Debug)]
pub struct ToastFrameInput {
    pub position: ToastPosition,
    pub visible_index: usize,
    pub offset_y: f64,
    pub frontmost_height: f64,
    /// The toast's own measured content height.
    pub height: f64,
    pub viewport_width: f64,
    pub expanded: bool,
    pub limited: bool,
    /// Pixels of swipe movement applied while dragging.
    pub swipe: (f64, f64),
}

/// The resting frame: `data-[position*=top]:transform-[translateX(swipe) translateY(..) scale(..)]`
/// collapsed, `--toast-calc-offset-y` expanded. Behind collapsed cards are
/// drawn at the frontmost height, scaled.
pub fn toast_resting_frame(input: ToastFrameInput) -> ToastFrame {
    let top = input.position.is_top();
    let index = input.visible_index;
    let behind_collapsed = index > 0 && !input.expanded;
    let (dx, swipe_y) = input.swipe;
    // Swipe y is a screen-space movement; the edge distance grows downward at the top.
    let swipe_edge = if top { swipe_y } else { -swipe_y };
    let (edge, width, height) = if input.expanded {
        let edge = toast_expanded_translate_y(input.offset_y, index, true);
        (edge + swipe_edge, input.viewport_width, input.height)
    } else {
        let scale = toast_collapsed_scale(index);
        let height = if behind_collapsed {
            input.frontmost_height
        } else {
            input.height
        };
        let edge = toast_collapsed_translate_y(index, input.frontmost_height, true);
        (
            edge + swipe_edge,
            input.viewport_width * scale,
            height * scale,
        )
    };
    // `transform-origin: top` / `bottom` centres the scaled card horizontally.
    let center_shift = (input.viewport_width - width) / 2.;
    ToastFrame {
        dx: dx + center_shift,
        edge,
        width,
        height,
        opacity: if input.limited { 0. } else { 1. },
    }
}

/// `data-starting-style`: top-right slides in from the right, other tops
/// slide down from above, bottoms slide up from below.
pub fn toast_starting_frame(
    rest: ToastFrame,
    position: ToastPosition,
    viewport_width: f64,
    inset: f64,
) -> ToastFrame {
    if position.is_top() && position.is_right() {
        return ToastFrame {
            dx: rest.dx + viewport_width + inset,
            ..rest
        };
    }
    ToastFrame {
        edge: -(rest.height + inset),
        ..rest
    }
}

/// `data-ending-style`: fade out while sliding off. A swipe dismissal keeps
/// going in its direction from where the pointer let go (`rest` already
/// carries the swipe movement).
pub fn toast_ending_frame(
    rest: ToastFrame,
    position: ToastPosition,
    viewport_width: f64,
    inset: f64,
    swipe: Option<SwipeDirection>,
) -> ToastFrame {
    let top = position.is_top();
    let out = viewport_width + inset;
    let down = rest.height + inset;
    // Screen-space "down" moves away from the top edge and towards the bottom one.
    let toward_screen_bottom =
        |edge: f64, amount: f64| if top { edge + amount } else { edge - amount };
    let mut frame = ToastFrame {
        opacity: 0.,
        ..rest
    };
    match swipe {
        Some(SwipeDirection::Left) => frame.dx = rest.dx - out,
        Some(SwipeDirection::Right) => frame.dx = rest.dx + out,
        Some(SwipeDirection::Up) => frame.edge = toward_screen_bottom(rest.edge, -down),
        Some(SwipeDirection::Down) => frame.edge = toward_screen_bottom(rest.edge, down),
        None if top && position.is_right() => frame.dx = rest.dx + out,
        None => frame.edge = toward_screen_bottom(rest.edge, down),
    }
    frame
}

/// The frame drawn `elapsed_ms` into a motion toward `to`.
pub fn toast_motion_frame(from: ToastFrame, to: ToastFrame, elapsed_ms: f64) -> ToastFrame {
    let t = (elapsed_ms / TOAST_MOTION_MS as f64).clamp(0., 1.);
    from.lerp(
        to,
        f64::from(super::animated_size::cubic_bezier(
            0.22, 1., 0.36, 1., t as f32,
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(id: &str, height: Option<f64>, ending: bool) -> ToastLayoutInput<String> {
        ToastLayoutInput {
            toast: id.to_owned(),
            height,
            transition_status: ending.then_some(ToastTransitionStatus::Ending),
        }
    }

    fn row(item: &VisibleToastLayoutItem<String>) -> (String, usize, f64) {
        (item.toast.clone(), item.visible_index, item.offset_y)
    }

    #[test]
    fn treats_a_labeled_action_as_visible() {
        assert!(has_visible_toast_action(Some(&ToastActionChildren::Text(
            "Update".into()
        ))));
    }

    #[test]
    fn hides_an_explicit_empty_action_used_to_clear_a_previous_cta() {
        assert!(!has_visible_toast_action(Some(&ToastActionChildren::Null)));
        assert!(!has_visible_toast_action(Some(&ToastActionChildren::Empty)));
        assert!(!has_visible_toast_action(None));
        assert!(!has_visible_toast_action(Some(&ToastActionChildren::False)));
        assert!(!has_visible_toast_action(Some(
            &ToastActionChildren::Absent
        )));
    }

    #[test]
    fn keeps_a_single_visible_toast_readable() {
        assert!(!should_hide_collapsed_toast_content(0, 1));
    }

    #[test]
    fn keeps_the_front_most_toast_readable_in_a_visible_stack() {
        assert!(!should_hide_collapsed_toast_content(0, 3));
    }

    #[test]
    fn hides_non_front_toasts_until_the_stack_is_expanded() {
        assert!(should_hide_collapsed_toast_content(1, 3));
    }

    #[test]
    fn computes_indices_and_offsets_from_the_visible_subset() {
        let layout = build_visible_toast_layout(vec![
            layout("a", Some(48.), false),
            layout("b", Some(72.), false),
            layout("c", Some(24.), false),
        ]);
        assert_eq!(layout.frontmost_height, 48.);
        let rows: Vec<_> = layout.items.iter().map(row).collect();
        assert_eq!(
            rows,
            vec![
                ("a".into(), 0, 0.),
                ("b".into(), 1, 48.),
                ("c".into(), 2, 120.),
            ]
        );
    }

    #[test]
    fn reflows_live_toasts_forward_when_the_front_toast_is_dismissed() {
        let layout = build_visible_toast_layout(vec![
            layout("a", Some(48.), true),
            layout("b", Some(72.), false),
            layout("c", Some(24.), false),
        ]);
        assert_eq!(layout.frontmost_height, 72.);
        let rows: Vec<_> = layout.items.iter().map(row).collect();
        assert_eq!(
            rows,
            vec![
                ("a".into(), 0, 0.),
                ("b".into(), 0, 0.),
                ("c".into(), 1, 72.),
            ]
        );
    }

    #[test]
    fn keeps_a_non_front_ending_toast_at_its_current_slot() {
        let layout = build_visible_toast_layout(vec![
            layout("a", Some(48.), false),
            layout("b", Some(72.), true),
            layout("c", Some(24.), false),
        ]);
        assert_eq!(layout.frontmost_height, 48.);
        let rows: Vec<_> = layout.items.iter().map(row).collect();
        assert_eq!(
            rows,
            vec![
                ("a".into(), 0, 0.),
                ("b".into(), 1, 48.),
                ("c".into(), 1, 48.),
            ]
        );
    }

    #[test]
    fn treats_missing_heights_as_zero() {
        let layout = build_visible_toast_layout(vec![
            layout("a", None, false),
            layout("b", None, false),
            layout("c", Some(30.), false),
        ]);
        assert_eq!(layout.frontmost_height, 0.);
        let rows: Vec<_> = layout
            .items
            .iter()
            .map(|item| (item.toast.clone(), item.offset_y))
            .collect();
        assert_eq!(
            rows,
            vec![("a".into(), 0.), ("b".into(), 0.), ("c".into(), 0.)]
        );
    }

    #[test]
    fn rejects_non_finite_and_non_positive_heights() {
        assert_eq!(normalize_toast_height(Some(f64::NAN)), 0.);
        assert_eq!(normalize_toast_height(Some(f64::INFINITY)), 0.);
        assert_eq!(normalize_toast_height(Some(-4.)), 0.);
        assert_eq!(normalize_toast_height(Some(0.)), 0.);
    }

    fn active() -> ScopedThreadRef {
        thread_ref("environment-a", "thread-1")
    }

    #[test]
    fn renders_a_toast_scoped_to_the_active_thread_ref() {
        let active = active();
        let data = ThreadToastScope {
            thread_ref: Some(active.clone()),
            thread_id: None,
        };
        assert!(should_render_thread_scoped_toast(
            Some(&data),
            Some(&active)
        ));
    }

    #[test]
    fn hides_a_scoped_toast_when_the_environment_differs() {
        let active = active();
        let data = ThreadToastScope {
            thread_ref: Some(thread_ref("environment-b", "thread-1")),
            thread_id: None,
        };
        assert!(!should_render_thread_scoped_toast(
            Some(&data),
            Some(&active)
        ));
    }

    #[test]
    fn keeps_legacy_thread_id_scoped_toasts_working() {
        let active = active();
        let data = ThreadToastScope {
            thread_ref: None,
            thread_id: Some(ThreadId::new("thread-1").unwrap()),
        };
        assert!(should_render_thread_scoped_toast(
            Some(&data),
            Some(&active)
        ));
    }

    #[test]
    fn unscoped_toasts_always_render() {
        assert!(should_render_thread_scoped_toast(None, None));
        assert!(should_render_thread_scoped_toast(
            Some(&ThreadToastScope {
                thread_ref: None,
                thread_id: None,
            }),
            None
        ));
    }

    #[test]
    fn collapsed_stack_peeks_and_scales() {
        assert_eq!(toast_collapsed_scale(0), 1.);
        assert_eq!(toast_collapsed_scale(1), 0.9);
        assert_eq!(toast_collapsed_scale(10), 0.);
        assert_eq!(toast_collapsed_translate_y(0, 48., true), 0.);
        // CSS: index * peek + (1 - scale) * height. `0.1 * 48` is not the same
        // f64 bits as `(1 - (1 - 0.1)) * 48`.
        let shrink = 1. - toast_collapsed_scale(1);
        assert_eq!(
            toast_collapsed_translate_y(1, 48., true),
            12. + shrink * 48.
        );
        assert_eq!(
            toast_collapsed_translate_y(1, 48., false),
            -(12. + shrink * 48.)
        );
    }

    #[test]
    fn expanded_stack_uses_offset_plus_gap() {
        assert_eq!(toast_expanded_translate_y(48., 1, true), 60.);
        assert_eq!(toast_expanded_translate_y(48., 1, false), -60.);
    }

    #[test]
    fn clamps_long_error_descriptions_by_utf16_length() {
        assert!(!error_description_is_clamped(
            Some(ToastType::Error),
            Some("short")
        ));
        let long = "e".repeat(180);
        assert!(error_description_is_clamped(
            Some(ToastType::Error),
            Some(&long)
        ));
        assert!(!error_description_is_clamped(
            Some(ToastType::Info),
            Some(&long)
        ));
        // One emoji is two UTF-16 units. 90 emoji reach the clamp; 89 do not.
        let almost = "😀".repeat(89);
        let exact = "😀".repeat(90);
        assert_eq!(utf16_len(&almost), 178);
        assert!(!error_description_is_clamped(
            Some(ToastType::Error),
            Some(&almost)
        ));
        assert!(error_description_is_clamped(
            Some(ToastType::Error),
            Some(&exact)
        ));
    }

    #[test]
    fn body_descriptor_matches_trailing_controls_and_copy() {
        let action = ToastActionChildren::Text("Retry".into());
        let stacked = derive_toast_body(ToastBodyInput {
            type_: Some(ToastType::Error),
            description: Some("boom"),
            action: Some(&action),
            action_layout: Some(ToastActionLayout::StackedEnd),
            action_variant: None,
            secondary_action_variant: None,
            hide_copy_button: false,
            additional_action_count: 0,
            has_secondary_action: false,
        });
        assert!(stacked.stacked_action_layout);
        assert_eq!(stacked.copy_error_text.as_deref(), Some("boom"));
        assert!(stacked.has_trailing_controls);
        assert!(stacked.inline_end_pad_is_pr6);
        assert_eq!(stacked.action_variant, ToastActionVariant::Default);
        assert_eq!(
            stacked.secondary_action_variant,
            ToastActionVariant::Outline
        );

        let plain = derive_toast_body(ToastBodyInput {
            type_: Some(ToastType::Success),
            description: Some("done"),
            action: None,
            action_layout: None,
            action_variant: None,
            secondary_action_variant: None,
            hide_copy_button: false,
            additional_action_count: 0,
            has_secondary_action: false,
        });
        assert!(!plain.has_trailing_controls);
        assert!(!plain.inline_end_pad_is_pr6);
        assert!(plain.copy_error_text.is_none());
    }

    #[test]
    fn hidden_toast_action_is_a_defined_empty_payload() {
        assert_eq!(hidden_toast_action_props(), ToastActionChildren::Null);
        let without = stacked_thread_toast(StackedThreadToastOptions {
            type_: ToastType::Loading,
            title: "Updating".into(),
            description: None,
            timeout: None,
            priority: None,
            action_props: None,
            action_variant: None,
        });
        assert!(without.action_props.is_none());
        assert_eq!(without.action_layout, ToastActionLayout::StackedEnd);
        let with = stacked_thread_toast(StackedThreadToastOptions {
            type_: ToastType::Loading,
            title: "Updating".into(),
            description: None,
            timeout: None,
            priority: None,
            action_props: Some(hidden_toast_action_props()),
            action_variant: None,
        });
        assert_eq!(with.action_props, Some(ToastActionChildren::Null));
    }

    #[test]
    fn visible_dismiss_pauses_and_closes_when_remaining_hits_zero() {
        assert!(VisibleDismissTimer::start(0).is_none());
        let mut timer = VisibleDismissTimer::start(1000).unwrap();
        assert!(!timer.start_running(10));
        assert_eq!(timer.started_at_ms, Some(10));
        timer.pause(400);
        assert_eq!(timer.remaining_ms, 610);
        assert!(timer.started_at_ms.is_none());
        timer.remaining_ms = 0;
        assert!(timer.start_running(500));
        assert!(timer.closed);
    }

    #[test]
    fn swipe_directions_follow_the_position() {
        use SwipeDirection::*;
        assert_eq!(ToastPosition::TopRight.swipe_directions(), vec![Right, Up]);
        assert_eq!(ToastPosition::TopLeft.swipe_directions(), vec![Left, Up]);
        assert_eq!(
            ToastPosition::BottomRight.swipe_directions(),
            vec![Right, Down]
        );
        assert_eq!(ToastPosition::TopCenter.swipe_directions(), vec![Up]);
        assert_eq!(ToastPosition::BottomCenter.swipe_directions(), vec![Down]);
    }

    #[test]
    fn the_viewport_is_inset_and_capped() {
        assert_eq!(toast_inset(1100.), 32.);
        assert_eq!(toast_inset(500.), 16.);
        assert_eq!(toast_viewport_width(1100.), 360.);
        assert_eq!(toast_viewport_width(380.), 348.);
    }

    #[test]
    fn a_swipe_past_the_threshold_dismisses_in_that_direction() {
        let mut swipe = SwipeTracker::new(ToastPosition::TopRight.swipe_directions(), 100., 100.);
        swipe.pointer_move(100., 100.);
        swipe.pointer_move(120., 101.);
        swipe.pointer_move(160., 102.);
        assert_eq!(swipe.pointer_up(), SwipeEnd::Dismiss(SwipeDirection::Right));
        assert_eq!(swipe.offset.0, 60.);
    }

    #[test]
    fn a_short_swipe_restores_the_toast() {
        let mut swipe = SwipeTracker::new(ToastPosition::TopRight.swipe_directions(), 100., 100.);
        swipe.pointer_move(100., 100.);
        swipe.pointer_move(130., 100.);
        assert_eq!(swipe.pointer_up(), SwipeEnd::Restore);
    }

    #[test]
    fn swiping_up_locks_the_vertical_axis() {
        let mut swipe = SwipeTracker::new(ToastPosition::TopRight.swipe_directions(), 100., 100.);
        swipe.pointer_move(100., 100.);
        swipe.pointer_move(102., 90.);
        // The lock lands after the first real movement, as React state does.
        let (x, y) = swipe.pointer_move(103., 40.);
        assert_eq!(x, 0.);
        assert_eq!(y, -60.);
        assert_eq!(swipe.pointer_up(), SwipeEnd::Dismiss(SwipeDirection::Up));
    }

    #[test]
    fn motion_against_the_allowed_direction_is_damped() {
        let mut swipe = SwipeTracker::new(ToastPosition::TopRight.swipe_directions(), 100., 100.);
        swipe.pointer_move(100., 100.);
        let (x, _) = swipe.pointer_move(36., 100.);
        // 64px left of a right-swipe toast: -sqrt(64).
        assert_eq!(x, -8.);
        assert_eq!(swipe.pointer_up(), SwipeEnd::Restore);
    }

    #[test]
    fn reversing_a_one_way_swipe_cancels_it() {
        let mut swipe = SwipeTracker::new(ToastPosition::TopCenter.swipe_directions(), 100., 100.);
        swipe.pointer_move(100., 100.);
        swipe.pointer_move(100., 40.);
        swipe.pointer_move(100., 55.);
        assert_eq!(swipe.pointer_up(), SwipeEnd::Restore);
    }

    fn frame_input(index: usize, expanded: bool) -> ToastFrameInput {
        ToastFrameInput {
            position: ToastPosition::TopRight,
            visible_index: index,
            offset_y: index as f64 * 50.,
            frontmost_height: 50.,
            height: 50.,
            viewport_width: 360.,
            expanded,
            limited: false,
            swipe: (0., 0.),
        }
    }

    #[test]
    fn a_collapsed_stack_peeks_behind_the_front_toast() {
        let front = toast_resting_frame(frame_input(0, false));
        assert_eq!((front.edge, front.width, front.height), (0., 360., 50.));
        let behind = toast_resting_frame(frame_input(1, false));
        // 1 * 12px peek + (1 - 0.9) * 50px; scaled to 90% and centred.
        assert_eq!(behind.edge, 12. + 0.1 * 50.);
        assert!((behind.width - 324.).abs() < 1e-9);
        assert!((behind.height - 45.).abs() < 1e-9);
        assert!((behind.dx - 18.).abs() < 1e-9);
        // Its bottom edge sits one peek below the front toast.
        assert!((behind.edge + behind.height - (12. + 50.)).abs() < 1e-9);
    }

    #[test]
    fn an_expanded_stack_uses_full_width_offsets_and_gaps() {
        let second = toast_resting_frame(frame_input(1, true));
        assert_eq!(
            (second.edge, second.width, second.dx),
            (50. + 12., 360., 0.)
        );
    }

    #[test]
    fn limited_toasts_are_transparent() {
        let mut input = frame_input(3, false);
        input.limited = true;
        assert_eq!(toast_resting_frame(input).opacity, 0.);
    }

    #[test]
    fn entering_and_leaving_follow_the_position() {
        let rest = toast_resting_frame(frame_input(0, false));
        let start = toast_starting_frame(rest, ToastPosition::TopRight, 360., 32.);
        assert_eq!(start.dx, 360. + 32.);
        let above = toast_starting_frame(rest, ToastPosition::TopCenter, 360., 32.);
        assert_eq!(above.edge, -(50. + 32.));
        let gone = toast_ending_frame(rest, ToastPosition::TopRight, 360., 32., None);
        assert_eq!((gone.dx, gone.opacity), (392., 0.));
        let swiped = toast_ending_frame(
            ToastFrame { dx: 70., ..rest },
            ToastPosition::TopRight,
            360.,
            32.,
            Some(SwipeDirection::Right),
        );
        assert_eq!(swiped.dx, 70. + 392.);
        let up = toast_ending_frame(
            ToastFrame { edge: -45., ..rest },
            ToastPosition::TopRight,
            360.,
            32.,
            Some(SwipeDirection::Up),
        );
        assert_eq!(up.edge, -45. - (50. + 32.));
    }

    #[test]
    fn motion_eases_from_the_start_to_the_target() {
        let from = ToastFrame {
            dx: 100.,
            edge: 0.,
            width: 360.,
            height: 50.,
            opacity: 0.,
        };
        let to = ToastFrame {
            dx: 0.,
            opacity: 1.,
            ..from
        };
        assert_eq!(toast_motion_frame(from, to, 0.), from);
        assert_eq!(toast_motion_frame(from, to, 5000.), to);
        let mid = toast_motion_frame(from, to, 100.);
        assert!(mid.dx < 50. && mid.dx > 0., "fast start: {}", mid.dx);
    }
}
