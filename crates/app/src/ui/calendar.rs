//! Port of upstream/apps/web/src/components/ui/calendar.tsx
//!
//! Desktop cell size is `--spacing(9)` (36px). The week starts on Sunday, matching
//! DayPicker's default. Outside days stay visible unless `showOutsideDays` is
//! false. Today draws a 3px primary dot. A selected day is `bg-primary`.
//!
//! Not ported: the month/year dropdowns and the focus ring.

use std::rc::Rc;

use chrono::{Datelike, Duration, NaiveDate};
use gpui_kit::{
    App, ClickEvent, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use super::icon::{Icon, IconName};
use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_SM, TEXT_XS};

pub const CELL_SIZE: f32 = 36.;

const WEEKDAYS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalendarCell {
    pub date: NaiveDate,
    pub outside: bool,
}

/// Weeks covering `month` (1-12). Leading and trailing days are `None` when
/// outside days are hidden, otherwise they are marked `outside`.
pub fn month_cells(year: i32, month: u32, show_outside_days: bool) -> Vec<Option<CalendarCell>> {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("month");
    let lead = first.weekday().num_days_from_sunday() as i64;
    let days = days_in_month(year, month) as i64;
    let rows = (lead + days + 6) / 7;
    let start = first - Duration::days(lead);
    (0..rows * 7)
        .map(|index| {
            let date = start + Duration::days(index);
            let outside = date.month() != month;
            if outside && !show_outside_days {
                None
            } else {
                Some(CalendarCell { date, outside })
            }
        })
        .collect()
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    (next.expect("next month") - Duration::days(1)).day()
}

pub fn shift_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let index = year * 12 + (month as i32 - 1) + delta;
    let year = index.div_euclid(12);
    let month = index.rem_euclid(12) as u32 + 1;
    (year, month)
}

type Pick = Rc<dyn Fn(NaiveDate, &mut Window, &mut App)>;
type MonthChange = Rc<dyn Fn(i32, u32, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Calendar {
    year: i32,
    month: u32,
    show_outside_days: bool,
    selected: Option<NaiveDate>,
    today: Option<NaiveDate>,
    on_select: Option<Pick>,
    on_month: Option<MonthChange>,
}

impl Calendar {
    pub fn new(year: i32, month: u32) -> Self {
        Self {
            year,
            month,
            show_outside_days: true,
            selected: None,
            today: None,
            on_select: None,
            on_month: None,
        }
    }

    pub fn show_outside_days(mut self, show: bool) -> Self {
        self.show_outside_days = show;
        self
    }

    pub fn selected(mut self, date: NaiveDate) -> Self {
        self.selected = Some(date);
        self
    }

    pub fn today(mut self, date: NaiveDate) -> Self {
        self.today = Some(date);
        self
    }

    pub fn on_select(
        mut self,
        handler: impl Fn(NaiveDate, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    pub fn on_month(mut self, handler: impl Fn(i32, u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_month = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Calendar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let caption = NaiveDate::from_ymd_opt(self.year, self.month, 1)
            .map(|date| date.format("%B %Y").to_string())
            .unwrap_or_default();
        let cells = month_cells(self.year, self.month, self.show_outside_days);
        let mut grid = div().grid().grid_cols(7u16);
        for cell in cells {
            let Some(cell) = cell else {
                grid = grid.child(div().size(px(CELL_SIZE)));
                continue;
            };
            let selected = self.selected == Some(cell.date);
            let today = self.today == Some(cell.date);
            let mut day = div()
                .id(SharedString::from(cell.date.format("%Y-%m-%d").to_string()))
                .on_mouse_down(MouseButton::Left, no_drag)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .size(px(CELL_SIZE))
                .rounded(RADIUS_LG)
                .text_size(TEXT_SM.size)
                .cursor(gpui_kit::CursorStyle::PointingHand);
            if selected {
                day = day.bg(theme.primary).text_color(theme.primary_foreground);
            } else if cell.outside {
                day = day.text_color(with_alpha(theme.muted_foreground, 0.72));
            } else {
                day = day.text_color(theme.foreground);
            }
            if !selected {
                day = day.hover(|style| style.bg(theme.accent));
            }
            if let Some(on_select) = self.on_select.clone() {
                let date = cell.date;
                day = day.on_click(move |_: &ClickEvent, window, cx| on_select(date, window, cx));
            }
            day = day.child(cell.date.day().to_string());
            if today {
                let dot = if selected {
                    theme.primary_foreground
                } else {
                    theme.primary
                };
                day = day.child(div().size(px(3.)).rounded_full().bg(dot));
            }
            grid = grid.child(day);
        }
        let previous = shift_month(self.year, self.month, -1);
        let next = shift_month(self.year, self.month, 1);
        let on_month = self.on_month;
        div()
            .flex()
            .flex_col()
            .w_auto()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(CELL_SIZE))
                    .child(month_button(
                        "calendar-prev",
                        IconName::ChevronLeft,
                        previous,
                        on_month.clone(),
                    ))
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(caption)),
                    )
                    .child(month_button(
                        "calendar-next",
                        IconName::ChevronRight,
                        next,
                        on_month,
                    )),
            )
            .child({
                let mut row = div().grid().grid_cols(7u16);
                for name in WEEKDAYS {
                    row = row.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(CELL_SIZE))
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(with_alpha(theme.muted_foreground, 0.72))
                            .child(name),
                    );
                }
                row
            })
            .child(grid)
    }
}

fn month_button(
    id: &str,
    icon: IconName,
    target: (i32, u32),
    on_month: Option<MonthChange>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let mut button = div()
        .id(SharedString::from(id.to_string()))
        .on_mouse_down(MouseButton::Left, no_drag)
        .flex()
        .items_center()
        .justify_center()
        .size(px(CELL_SIZE))
        .rounded(RADIUS_LG)
        .cursor(gpui_kit::CursorStyle::PointingHand)
        .child(Icon::new(icon).size(gpui_kit::rems(1.)));
    if let Some(on_month) = on_month {
        button = button
            .on_click(move |_: &ClickEvent, window, cx| on_month(target.0, target.1, window, cx));
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn october_2026_starts_on_thursday() {
        let cells = month_cells(2026, 10, true);
        assert_eq!(cells.len(), 35);
        let first = cells[0].unwrap();
        assert_eq!(first.date, NaiveDate::from_ymd_opt(2026, 9, 27).unwrap());
        assert!(first.outside);
        assert_eq!(
            cells[4].unwrap().date,
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
        );
        assert!(!cells[4].unwrap().outside);
        assert_eq!(
            cells[34].unwrap().date,
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()
        );
    }

    #[test]
    fn hidden_outside_days_leave_empty_cells() {
        let cells = month_cells(2026, 10, false);
        assert!(cells[..4].iter().all(Option::is_none));
        assert!(cells[4].is_some());
    }

    #[test]
    fn month_shift_wraps_the_year() {
        assert_eq!(shift_month(2026, 1, -1), (2025, 12));
        assert_eq!(shift_month(2026, 12, 1), (2027, 1));
    }
}
