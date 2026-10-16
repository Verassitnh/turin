//! Port of upstream/apps/web/src/components/chat/ComposerTasksBadge.tsx
//!
//! The agent's task list in the composer: a collapsed tab (or an inline row
//! inside the banner stack) showing the current task, `done/total` and up to
//! ten progress segments, and the drawer that expands the full list with each
//! step's status and duration.
//!
//! The row is the control (`ComposerBanner.Row render={<button/>}`); the
//! toggle chevron is decorative. Segments appear at `@min-[560px]`, which the
//! caller supplies as `wide` from the composer body width. Not ported:
//! `onPointerDown preventDefault` (GPUI buttons do not take focus), the
//! `data-*` hooks and the scroll fade of `ComposerBanner.Scroll`.

use std::collections::HashMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, CursorStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};
use turin_shared::orchestration_timing::format_duration;

use crate::components::chat::composer_pending_panels::{
    BannerDensity, BannerVariant, banner_surface_with_density,
};
use crate::composer::views::composer_banner::{
    WindowCallback, banner_row, count_text, small_icon, toggle_icon,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_SM, TEXT_3XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName};

/// `@min-[560px]` of the composer banner container.
pub const TASK_SEGMENTS_MIN_WIDTH_PX: f32 = 560.;

const MAX_TASK_SEGMENTS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerTasksProgress {
    pub step: String,
    pub completed_steps: usize,
    pub total_steps: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerTaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerTaskStep {
    pub duration_ms: Option<f64>,
    pub step: String,
    pub status: ComposerTaskStatus,
}

/// What the composer is given to draw (`activeTasksProgress` +
/// `activeTaskSteps`).
#[derive(Debug, Clone, PartialEq)]
pub struct ComposerTasks {
    pub progress: ComposerTasksProgress,
    pub steps: Vec<ComposerTaskStep>,
}

impl ComposerTasks {
    /// `activeTasksProgress.totalSteps > 0`: an empty plan shows no tab.
    pub fn is_visible(&self) -> bool {
        self.progress.total_steps > 0
    }
}

/// `taskStatusLabels`.
pub fn task_status_label(status: ComposerTaskStatus) -> &'static str {
    match status {
        ComposerTaskStatus::Pending => "Pending",
        ComposerTaskStatus::InProgress => "Running",
        ComposerTaskStatus::Completed => "Completed",
    }
}

/// `keyedTaskSteps`: steps may repeat a title, so the key adds the
/// occurrence.
pub fn keyed_task_steps(steps: &[ComposerTaskStep]) -> Vec<(String, &ComposerTaskStep)> {
    let mut occurrences: HashMap<&str, usize> = HashMap::new();
    steps
        .iter()
        .map(|step| {
            let occurrence = occurrences.entry(step.step.as_str()).or_insert(0);
            let key = format!("{}:{}", step.step, occurrence);
            *occurrence += 1;
            (key, step)
        })
        .collect()
}

/// `TaskSegments` renders nothing for one step or more than ten.
pub fn task_segments_shown(step_count: usize) -> bool {
    step_count > 1 && step_count <= MAX_TASK_SEGMENTS
}

/// The row's `aria-label`.
pub fn tasks_badge_label(expanded: bool, progress: &ComposerTasksProgress) -> String {
    format!(
        "{}: {} of {} complete. Current task: {}",
        if expanded { "Collapse tasks" } else { "Tasks" },
        progress.completed_steps,
        progress.total_steps,
        progress.step
    )
}

/// The task list's `aria-label`.
pub fn tasks_list_label(progress: &ComposerTasksProgress) -> String {
    format!(
        "Task list. {} of {} complete.",
        progress.completed_steps, progress.total_steps
    )
}

/// The duration cell: the measured duration, else `now` while running.
pub fn task_duration_text(step: &ComposerTaskStep) -> Option<String> {
    match step.duration_ms {
        Some(duration) => Some(format_duration(duration)),
        None if step.status == ComposerTaskStatus::InProgress => Some("now".to_owned()),
        None => None,
    }
}

fn segment_color(status: ComposerTaskStatus, theme: &AppTheme) -> Hsla {
    match status {
        ComposerTaskStatus::Completed => theme.success,
        ComposerTaskStatus::InProgress => theme.primary,
        ComposerTaskStatus::Pending => with_alpha(theme.muted_foreground, 0.25).into(),
    }
}

/// `TaskSegments`: `flex w-10 shrink-0 items-center gap-0.5` (`w-20` in the
/// badge), one `h-[3px]` pill per step.
fn task_segments(steps: &[ComposerTaskStep], wide: bool, theme: &AppTheme) -> Option<AnyElement> {
    task_segments_shown(steps.len()).then(|| {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(2.))
            .w(rems(if wide { 5. } else { 2.5 }))
            .children(steps.iter().map(|step| {
                div()
                    .h(px(3.))
                    .min_w_0()
                    .flex_1()
                    .rounded_full()
                    .bg(segment_color(step.status, theme))
            }))
            .into_any_element()
    })
}

/// `TaskSummary`: icon, `Tasks` + current step, count, segments, toggle.
fn task_summary(
    expanded: bool,
    progress: &ComposerTasksProgress,
    steps: &[ComposerTaskStep],
    wide: bool,
    theme: &AppTheme,
) -> AnyElement {
    let content = div()
        .flex()
        .min_w_0()
        .flex_1()
        .items_center()
        .gap_1()
        .child(
            div()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child("Tasks"),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .text_color(with_alpha(theme.foreground, 0.8))
                .child(progress.step.clone()),
        )
        .into_any_element();
    let count_color = if progress.completed_steps >= progress.total_steps {
        theme.success
    } else {
        theme.muted_foreground
    };
    let mut actions = vec![count_text(
        format!("{}/{}", progress.completed_steps, progress.total_steps),
        count_color,
    )];
    if wide {
        actions.extend(task_segments(steps, true, theme));
    }
    actions.push(toggle_icon(expanded));
    banner_row(
        Some(small_icon(IconName::ListTodo, theme)),
        content,
        actions,
    )
    .into_any_element()
}

/// Where the badge sits: the shoulder tab over the composer, or a row in the
/// banner stack / drawer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TasksPlacement {
    Inline,
    Tab,
}

fn tasks_row(
    id: impl Into<SharedString>,
    expanded: bool,
    progress: &ComposerTasksProgress,
    steps: &[ComposerTaskStep],
    wide: bool,
    on_toggle: WindowCallback,
    theme: &AppTheme,
) -> AnyElement {
    let foreground = theme.foreground;
    div()
        .id(gpui_kit::ElementId::Name(id.into()))
        .cursor(CursorStyle::PointingHand)
        .rounded(RADIUS_SM)
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .hover(move |style| style.text_color(foreground))
        .child(task_summary(expanded, progress, steps, wide, theme))
        .into_any_element()
}

/// `ComposerTasksBadge`.
#[derive(IntoElement)]
pub struct ComposerTasksBadge {
    pub expanded: bool,
    pub placement: TasksPlacement,
    pub tasks: ComposerTasks,
    pub wide: bool,
    pub on_toggle: WindowCallback,
}

impl RenderOnce for ComposerTasksBadge {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if !self.tasks.is_visible() {
            return div().into_any_element();
        }
        let theme = cx.app_theme().clone();
        let row = tasks_row(
            "composer-tasks-badge",
            self.expanded,
            &self.tasks.progress,
            &self.tasks.steps,
            self.wide,
            self.on_toggle,
            &theme,
        );
        match self.placement {
            TasksPlacement::Inline => row,
            TasksPlacement::Tab => banner_surface_with_density(
                BannerVariant::Default,
                BannerDensity::Comfortable,
                row,
                window,
                &theme,
            ),
        }
    }
}

/// One `<li>` of the task list: `items-start py-1 pe-2`.
fn task_list_row(step: &ComposerTaskStep, theme: &AppTheme) -> AnyElement {
    let (icon, icon_color, text_color): (IconName, Hsla, Hsla) = match step.status {
        ComposerTaskStatus::Completed => (
            IconName::Check,
            theme.success,
            with_alpha(theme.muted_foreground, 0.55).into(),
        ),
        ComposerTaskStatus::InProgress => (
            IconName::CircleDot,
            theme.primary,
            with_alpha(theme.foreground, 0.9).into(),
        ),
        ComposerTaskStatus::Pending => (
            IconName::Circle,
            with_alpha(theme.muted_foreground, 0.4).into(),
            with_alpha(theme.muted_foreground, 0.7).into(),
        ),
    };
    div()
        .flex()
        .w_full()
        .min_w_0()
        .min_h(rems(1.25))
        .items_start()
        .gap(px(4.))
        .py_1()
        .pr_2()
        .child(
            div()
                .flex()
                .flex_none()
                .w(rems(1.5))
                .h(rems(1.))
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(rems(0.75)).color(icon_color)),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_color(text_color)
                .child(step.step.clone()),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(rems(3.))
                .flex()
                .justify_end()
                .text_size(TEXT_3XS.size)
                .line_height(rems(1.))
                .text_color(with_alpha(theme.muted_foreground, 0.45))
                .children(task_duration_text(step)),
        )
        .into_any_element()
}

/// `ComposerTasksContent`: the badge row, and the list when expanded.
#[derive(IntoElement)]
pub struct ComposerTasksContent {
    pub expanded: bool,
    pub tasks: ComposerTasks,
    pub wide: bool,
    pub on_toggle: WindowCallback,
}

impl RenderOnce for ComposerTasksContent {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let row = tasks_row(
            "composer-tasks-badge",
            self.expanded,
            &self.tasks.progress,
            &self.tasks.steps,
            self.wide,
            self.on_toggle,
            &theme,
        );
        div()
            .flex()
            .flex_col()
            .child(row)
            .when(self.expanded, |this| {
                this.child(
                    div()
                        .id("composer-tasks-scroll")
                        .max_h(rems(24.))
                        .overflow_y_scroll()
                        .child(
                            div().flex().flex_col().gap(px(1.)).children(
                                keyed_task_steps(&self.tasks.steps)
                                    .into_iter()
                                    .map(|(_, step)| task_list_row(step, &theme)),
                            ),
                        ),
                )
            })
    }
}

/// `ComposerTasksDrawer`: the expanded list attached above the composer.
#[derive(IntoElement)]
pub struct ComposerTasksDrawer {
    pub tasks: ComposerTasks,
    pub wide: bool,
    pub on_collapse: WindowCallback,
}

impl RenderOnce for ComposerTasksDrawer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let content = ComposerTasksContent {
            expanded: true,
            tasks: self.tasks,
            wide: self.wide,
            on_toggle: self.on_collapse,
        }
        .into_any_element();
        banner_surface_with_density(
            BannerVariant::Default,
            BannerDensity::Default,
            content,
            window,
            &theme,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(title: &str, status: ComposerTaskStatus) -> ComposerTaskStep {
        ComposerTaskStep {
            duration_ms: None,
            step: title.to_owned(),
            status,
        }
    }

    fn progress(done: usize, total: usize, current: &str) -> ComposerTasksProgress {
        ComposerTasksProgress {
            step: current.to_owned(),
            completed_steps: done,
            total_steps: total,
        }
    }

    #[test]
    fn repeated_titles_get_distinct_keys() {
        let steps = [
            step("Run tests", ComposerTaskStatus::Completed),
            step("Fix", ComposerTaskStatus::Pending),
            step("Run tests", ComposerTaskStatus::Pending),
        ];
        let keys: Vec<String> = keyed_task_steps(&steps)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, ["Run tests:0", "Fix:0", "Run tests:1"]);
    }

    #[test]
    fn segments_show_for_two_to_ten_steps() {
        assert!(!task_segments_shown(0));
        assert!(!task_segments_shown(1));
        assert!(task_segments_shown(2));
        assert!(task_segments_shown(10));
        assert!(!task_segments_shown(11));
    }

    #[test]
    fn labels_name_the_progress_and_the_action() {
        let progress = progress(1, 3, "Write the port");
        assert_eq!(
            tasks_badge_label(false, &progress),
            "Tasks: 1 of 3 complete. Current task: Write the port"
        );
        assert_eq!(
            tasks_badge_label(true, &progress),
            "Collapse tasks: 1 of 3 complete. Current task: Write the port"
        );
        assert_eq!(tasks_list_label(&progress), "Task list. 1 of 3 complete.");
    }

    #[test]
    fn status_labels_match_upstream() {
        assert_eq!(task_status_label(ComposerTaskStatus::Pending), "Pending");
        assert_eq!(task_status_label(ComposerTaskStatus::InProgress), "Running");
        assert_eq!(
            task_status_label(ComposerTaskStatus::Completed),
            "Completed"
        );
    }

    #[test]
    fn duration_cell_shows_now_only_for_a_running_step_without_a_duration() {
        let mut running = step("Build", ComposerTaskStatus::InProgress);
        assert_eq!(task_duration_text(&running).as_deref(), Some("now"));
        running.duration_ms = Some(2_500.);
        assert_eq!(task_duration_text(&running).as_deref(), Some("2.5s"));
        assert_eq!(
            task_duration_text(&step("Later", ComposerTaskStatus::Pending)),
            None
        );
    }

    #[test]
    fn an_empty_plan_has_no_tab() {
        let tasks = ComposerTasks {
            progress: progress(0, 0, ""),
            steps: Vec::new(),
        };
        assert!(!tasks.is_visible());
    }
}
