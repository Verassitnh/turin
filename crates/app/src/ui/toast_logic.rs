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
}
