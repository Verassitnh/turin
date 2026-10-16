//! Port of upstream/apps/web/src/components/chat/ThreadDetailsCard.tsx
//!
//! One card owns its placement and folds content only when that content cannot fit: the content
//! renders at a density (`full`, `compact`, `essential`), each density's height is measured once
//! it has been shown, and [`resolve_thread_details_card_density`] picks the richest density the
//! card's height holds. The popover presentation (`ThreadPanelPresentation`) is not ported: the
//! layout either places the card inline or there is no card.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, canvas, div, px,
};

use super::thread_details_card_layout::{
    ThreadDetailsCardContentHeights, ThreadDetailsCardDensity, ThreadDetailsCardLayout,
    resolve_thread_details_card_density,
};
use crate::theme::ActiveAppTheme;
use crate::ui::scroll_area_hidden;
use crate::ui::tooltip::{dropdown_glass_background, dropdown_glass_border};

/// `useState({ key, heights, fullContentHeight })`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadDetailsCardMeasurements {
    /// `measurementKey`: heights belong to one thread and card width.
    pub key: String,
    pub heights: Heights,
    pub full_content_height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Heights {
    pub full: f64,
    pub compact: f64,
}

impl ThreadDetailsCardMeasurements {
    fn heights_for(&self, key: &str) -> Heights {
        if self.key == key {
            self.heights
        } else {
            Heights::default()
        }
    }

    /// `cardBottom`: where the card ends, reported to the chat canvas so chat moves over only as
    /// far as the card reaches. `None` until the full content has been measured.
    pub fn card_bottom(&self, key: &str, layout: &ThreadDetailsCardLayout) -> Option<f64> {
        (self.key == key && self.full_content_height > 0.)
            .then(|| layout.y + self.full_content_height.min(layout.height))
    }
}

/// The measurement step of upstream's layout effect: record the height this density measured,
/// and the full content height when the density is full.
pub fn record_measurement(
    current: &ThreadDetailsCardMeasurements,
    key: &str,
    density: ThreadDetailsCardDensity,
    fitting_height: f64,
    full_height: f64,
) -> Option<ThreadDetailsCardMeasurements> {
    if density == ThreadDetailsCardDensity::Essential {
        return None;
    }
    let mut heights = current.heights_for(key);
    let full_content_height = if density == ThreadDetailsCardDensity::Full {
        full_height
    } else if current.key == key {
        current.full_content_height
    } else {
        0.
    };
    match density {
        ThreadDetailsCardDensity::Full => heights.full = fitting_height,
        ThreadDetailsCardDensity::Compact => heights.compact = fitting_height,
        ThreadDetailsCardDensity::Essential => unreachable!(),
    }
    let next = ThreadDetailsCardMeasurements {
        key: key.to_owned(),
        heights,
        full_content_height,
    };
    (next != *current).then_some(next)
}

type CardContent = Rc<dyn Fn(ThreadDetailsCardDensity, &mut Window, &mut App) -> AnyElement>;

#[derive(IntoElement)]
pub struct ThreadDetailsCard {
    pub layout: ThreadDetailsCardLayout,
    /// `measurementKey`: `environment:thread:width`.
    pub measurement_key: String,
    pub measurements: Entity<ThreadDetailsCardMeasurements>,
    /// `children(density)`.
    pub content: CardContent,
}

impl RenderOnce for ThreadDetailsCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        let heights = self
            .measurements
            .read(cx)
            .heights_for(&self.measurement_key);
        let density = resolve_thread_details_card_density(
            self.layout.height,
            ThreadDetailsCardContentHeights {
                full: heights.full,
                compact: heights.compact,
            },
        );
        let content = (self.content)(density, window, cx);
        let measurements = self.measurements.clone();
        let key = self.measurement_key.clone();
        // The single content tree is measured before the scroll viewport clips it, and each
        // observed height is kept so more room restores the detail it can hold.
        let measure = canvas(
            move |bounds, _, cx| {
                // The card's own padding (`py-2`) and border sit around the content.
                let height = f64::from(f32::from(bounds.size.height));
                let current = measurements.read(cx).clone();
                if let Some(next) = record_measurement(&current, &key, density, height, height) {
                    measurements.update(cx, |measurements, cx| {
                        *measurements = next;
                        cx.notify();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .id("thread-details-card")
            .absolute()
            .top(px(self.layout.y as f32))
            .left(px(self.layout.x as f32))
            .w(px(self.layout.width as f32))
            .max_h(px(self.layout.height as f32))
            .rounded_3xl()
            .border_1()
            .border_color(dropdown_glass_border(&theme))
            .bg(dropdown_glass_background(&theme))
            .shadow(crate::theme::tokens::shadow_popover(dark))
            .overflow_hidden()
            .child(
                scroll_area_hidden("thread-details-scroll").child(
                    div()
                        .relative()
                        .w_full()
                        .py_2()
                        .child(measure)
                        .child(content),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "env:thread:280";

    fn layout(height: f64) -> ThreadDetailsCardLayout {
        ThreadDetailsCardLayout {
            x: 700.,
            right: 12.,
            width: 280.,
            y: 12.,
            height,
        }
    }

    fn record(
        current: &ThreadDetailsCardMeasurements,
        density: ThreadDetailsCardDensity,
        height: f64,
    ) -> ThreadDetailsCardMeasurements {
        record_measurement(current, KEY, density, height, height).unwrap_or_else(|| current.clone())
    }

    #[test]
    fn remembers_each_density_and_the_full_content_height() {
        let none = ThreadDetailsCardMeasurements::default();
        let full = record(&none, ThreadDetailsCardDensity::Full, 520.);
        assert_eq!(full.heights.full, 520.);
        assert_eq!(full.full_content_height, 520.);
        // A compact measurement keeps what the full one learned.
        let both = record(&full, ThreadDetailsCardDensity::Compact, 300.);
        assert_eq!(
            both.heights,
            Heights {
                full: 520.,
                compact: 300.
            }
        );
        assert_eq!(both.full_content_height, 520.);
    }

    #[test]
    fn the_essential_density_is_never_measured() {
        let none = ThreadDetailsCardMeasurements::default();
        assert_eq!(
            record_measurement(&none, KEY, ThreadDetailsCardDensity::Essential, 100., 100.),
            None
        );
    }

    #[test]
    fn an_unchanged_measurement_does_not_update() {
        let full = record(
            &ThreadDetailsCardMeasurements::default(),
            ThreadDetailsCardDensity::Full,
            520.,
        );
        assert_eq!(
            record_measurement(&full, KEY, ThreadDetailsCardDensity::Full, 520., 520.),
            None
        );
    }

    #[test]
    fn a_new_thread_or_width_starts_over() {
        let full = record(
            &ThreadDetailsCardMeasurements::default(),
            ThreadDetailsCardDensity::Full,
            520.,
        );
        assert_eq!(full.heights_for("env:other:280"), Heights::default());
        let other = record_measurement(
            &full,
            "env:other:280",
            ThreadDetailsCardDensity::Compact,
            300.,
            300.,
        )
        .unwrap();
        assert_eq!(
            other.heights,
            Heights {
                full: 0.,
                compact: 300.
            }
        );
        assert_eq!(other.full_content_height, 0.);
    }

    #[test]
    fn folds_to_the_density_the_height_holds() {
        let measured = ThreadDetailsCardMeasurements {
            key: KEY.into(),
            heights: Heights {
                full: 520.,
                compact: 300.,
            },
            full_content_height: 520.,
        };
        let density = |height| {
            let heights = measured.heights_for(KEY);
            resolve_thread_details_card_density(
                height,
                ThreadDetailsCardContentHeights {
                    full: heights.full,
                    compact: heights.compact,
                },
            )
        };
        assert_eq!(density(800.), ThreadDetailsCardDensity::Full);
        assert_eq!(density(400.), ThreadDetailsCardDensity::Compact);
        assert_eq!(density(200.), ThreadDetailsCardDensity::Essential);
    }

    #[test]
    fn the_card_ends_at_its_content_or_the_available_height() {
        let measured = ThreadDetailsCardMeasurements {
            key: KEY.into(),
            heights: Heights {
                full: 520.,
                compact: 300.,
            },
            full_content_height: 520.,
        };
        assert_eq!(measured.card_bottom(KEY, &layout(800.)), Some(12. + 520.));
        assert_eq!(measured.card_bottom(KEY, &layout(400.)), Some(12. + 400.));
        assert_eq!(measured.card_bottom("other", &layout(800.)), None);
        assert_eq!(
            ThreadDetailsCardMeasurements::default().card_bottom(KEY, &layout(800.)),
            None
        );
    }
}
