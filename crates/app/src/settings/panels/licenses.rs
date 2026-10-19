//! Port of OpenSourceLicenses.tsx and the filter / bundle helpers of
//! packages/shared/src/thirdPartyLicenses.ts. The notice manifest comes from
//! the view model (T22 feeds the cargo-generated manifest) instead of an HTTP
//! fetch, so the loading / retry states are not ported.

use crate::theme::color::solid_over;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};

use super::super::controls::{FieldSize, text_field_leading};
use super::super::layout::SettingsSection;
use super::super::view::SettingsView;
use super::super::view_model::LicenseEntry;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

#[derive(Default)]
pub struct LicensesState {
    pub query: String,
    pub search_open: bool,
    pub open_entry: Option<String>,
}

/// `filterThirdPartyLicenseEntries`.
pub fn filter_license_entries<'a>(
    entries: &'a [LicenseEntry],
    query: &str,
) -> Vec<&'a LicenseEntry> {
    let lowered = query.trim().to_lowercase();
    let terms: Vec<&str> = lowered.split_whitespace().collect();
    if terms.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .filter(|entry| {
            let mut parts: Vec<&str> = vec![entry.name.as_str()];
            parts.extend(entry.version.as_deref());
            parts.push(entry.license.as_str());
            parts.extend(entry.bundles.iter().map(String::as_str));
            let searchable = parts.join(" ").to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        })
        .collect()
}

/// `formatLicenseBundles`.
pub fn format_license_bundles(bundles: &[String]) -> String {
    bundles
        .iter()
        .map(|bundle| match bundle.as_str() {
            "android" => "Android",
            "assets" => "Assets",
            "desktop" => "Desktop",
            "device-tools" => "Device tools",
            "ios" => "iOS",
            "mobile" => "Mobile",
            "server" => "Server",
            "web" => "Web",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `thirdPartyLicenseEntryKey`.
fn entry_key(entry: &LicenseEntry) -> String {
    format!(
        "{}:{}:{}",
        entry.kind,
        entry.name,
        entry.version.as_deref().unwrap_or("")
    )
}

impl SettingsView {
    pub(super) fn render_licenses(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let state = &self.panels.licenses;
        let filtered = filter_license_entries(&vm.licenses, &state.query);
        let total = vm.licenses.len();
        let count = div()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(if filtered.len() == total {
                format!("{total} notices")
            } else {
                format!("{} of {total}", filtered.len())
            });
        let search_open = self.panels.licenses.search_open;
        let action = if !search_open {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(count)
                .child(
                    Button::new("licenses-search")
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::IconMicro)
                        .icon(IconName::Search)
                        .icon_size(rems(0.75))
                        .tooltip(Tooltip::new("Search licenses"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.panels.licenses.search_open = true;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            let query = self.panels.licenses.query.clone();
            let input = self.input_live(
                "licenses-query",
                &query,
                "Search licenses",
                window,
                cx,
                |this, text, _, cx| {
                    this.panels.licenses.query = text;
                    cx.notify();
                },
            );
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(count)
                .child(
                    div().w(rems(11.)).child(text_field_leading(
                        &input,
                        None,
                        FieldSize::Sm,
                        false,
                        Some(
                            Icon::new(IconName::Search)
                                .size(rems(0.75))
                                .color(theme.muted_foreground)
                                .into_any_element(),
                        ),
                        cx,
                    )),
                )
                .into_any_element()
        };

        let mut section = SettingsSection::new("Third-party notices").header_action(action);
        if filtered.is_empty() {
            section = section.child(
                div()
                    .px_4()
                    .py_8()
                    .text_center()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("No licenses match that search."),
            );
        }
        let open_key = self.panels.licenses.open_entry.clone();
        for entry in filtered {
            let key = entry_key(entry);
            let open = open_key.as_deref() == Some(key.as_str());
            let toggle_key = key.clone();
            let source = entry.source_url.clone();
            let row = div()
                .flex()
                .min_h(rems(2.25))
                .items_center()
                .hover(|style| style.bg(solid_over(theme.muted, 0.35, theme.background)))
                .child(
                    div()
                        .id(SharedString::from(format!("license-{key}")))
                        .flex()
                        .min_h(rems(2.25))
                        .min_w_0()
                        .flex_1()
                        .items_center()
                        .gap(px(10.))
                        .px_4()
                        .cursor(CursorStyle::PointingHand)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let current = &mut this.panels.licenses.open_entry;
                            *current = if current.as_deref() == Some(toggle_key.as_str()) {
                                None
                            } else {
                                Some(toggle_key.clone())
                            };
                            cx.notify();
                        }))
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(rems(0.875))
                            .color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .flex_1()
                                .items_baseline()
                                .gap_2()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_SM.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(entry.name.clone()),
                                )
                                .children(entry.version.clone().map(|version| {
                                    div()
                                        .flex_shrink_0()
                                        .font_family(theme.fonts.mono.clone())
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(version)
                                })),
                        )
                        .child(
                            div()
                                .max_w(gpui_kit::relative(0.42))
                                .flex_shrink_0()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} · {}",
                                    entry.license,
                                    format_license_bundles(&entry.bundles)
                                )),
                        ),
                )
                .children(source.map(|url| {
                    div().mr_4().flex_shrink_0().child(
                        Button::new(SharedString::from(format!("license-source-{key}")))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconMicro)
                            .icon(IconName::ExternalLink)
                            .icon_size(rems(0.75))
                            .tooltip(Tooltip::new("Project source"))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }));
            section = section.child(div().flex().flex_col().child(row).when(open, |this| {
                this.child(
                    div().px(rems(2.5)).pt_1().pb_4().child(
                        div()
                            .max_w(rems(42.))
                            .font_family(theme.fonts.mono.clone())
                            .text_size(TEXT_XS.size)
                            .line_height(rems(1.25))
                            .text_color(with_alpha(theme.foreground, 0.8))
                            .child(entry.notice_text.clone()),
                    ),
                )
            }));
        }
        vec![section.into_any_element()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, version: &str, license: &str, bundles: &[&str]) -> LicenseEntry {
        LicenseEntry {
            kind: "cargo".into(),
            name: name.into(),
            version: Some(version.into()),
            license: license.into(),
            bundles: bundles.iter().map(|b| (*b).to_owned()).collect(),
            source_url: None,
            notice_text: String::new(),
        }
    }

    #[test]
    fn filters_by_every_term_across_name_version_license_and_bundles() {
        let entries = vec![
            entry("tokio", "1.48.0", "MIT", &["server"]),
            entry("gpui", "0.3.9", "Apache-2.0", &["desktop"]),
        ];
        assert_eq!(filter_license_entries(&entries, "  ").len(), 2);
        assert_eq!(filter_license_entries(&entries, "MIT")[0].name, "tokio");
        assert_eq!(
            filter_license_entries(&entries, "apache desktop")[0].name,
            "gpui"
        );
        assert!(filter_license_entries(&entries, "tokio desktop").is_empty());
    }

    #[test]
    fn labels_known_bundles_and_keeps_unknown_ones() {
        let bundles = vec![
            "ios".to_owned(),
            "device-tools".to_owned(),
            "custom".to_owned(),
        ];
        assert_eq!(
            format_license_bundles(&bundles),
            "iOS, Device tools, custom"
        );
    }
}
