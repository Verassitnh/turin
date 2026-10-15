//! A modal dialog frame (upstream components/ui/{dialog,alert-dialog}.tsx +
//! dialog-styles.ts): `dialog-backdrop` fill, a `dialog-glass rounded-2xl
//! border` popup with `p-6` header and an optional muted footer.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Rems, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    deferred, div, px, relative, rems,
};

use super::view::SettingsView;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_2XL, TEXT_SM};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant};

type DialogAction = Rc<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>)>;

pub struct ConfirmSpec {
    pub title: SharedString,
    pub description: SharedString,
    pub confirm_label: SharedString,
    pub destructive: bool,
    pub on_confirm: DialogAction,
}

/// The dialogs the settings pages open.
pub enum Dialog {
    Confirm(ConfirmSpec),
    /// `AddUsageLimitSourceDialog`.
    AddHub,
    /// The Projects page's action editor.
    ScriptEditor,
    /// `BackgroundActivityAdvancedDialog`.
    BackgroundActivity,
    /// The Add provider wizard.
    AddProvider,
}

/// `DialogPopup` chrome around `children` (header / panel / footer).
pub fn dialog_frame(
    theme: &AppTheme,
    max_width: Rems,
    children: Vec<AnyElement>,
    on_dismiss: impl Fn(&mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    let dark = theme.is_dark();
    let backdrop = with_alpha(theme.background, if dark { 0.64 } else { 0.60 });
    // GPUI has no backdrop blur, so the glass fill is painted opaque.
    let popup_bg = theme.background;
    let border = if dark {
        with_alpha(tokens::tw::WHITE, 0.08)
    } else {
        with_alpha(theme.foreground, 0.10)
    };
    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .bg(backdrop)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                on_dismiss(window, cx)
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(max_width)
                    .max_h(relative(0.92))
                    .min_h_0()
                    .rounded(RADIUS_2XL)
                    .border_1()
                    .border_color(border)
                    .bg(popup_bg)
                    .shadow(tokens::shadow_dialog(dark))
                    .text_color(theme.popover_foreground)
                    // Clicks inside the popup must not reach the dismiss layer.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(children),
            ),
    )
    .with_priority(10)
    .into_any_element()
}

/// `DialogPanel`: the scrolling body between header and footer.
pub fn dialog_panel(id: &'static str, child: impl IntoElement) -> AnyElement {
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_6()
        .pb_6()
        .child(child)
        .into_any_element()
}

pub fn dialog_header(
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    theme: &AppTheme,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_6()
        .child(
            div()
                .text_size(rems(1.25))
                .line_height(relative(1.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(description.map(|description| {
            div()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .text_color(theme.muted_foreground)
                .child(description)
        }))
        .into_any_element()
}

/// `DialogFooter variant="default"`: `border-t bg-muted/72 px-6 py-4`.
pub fn dialog_footer(theme: &AppTheme, children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .px_6()
        .py_4()
        .border_t_1()
        .border_color(theme.border)
        .bg(with_alpha(theme.muted, 0.72))
        .rounded_b(px(17.))
        .children(children)
        .into_any_element()
}

impl SettingsView {
    pub(super) fn render_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        if matches!(self.dialog, Some(Dialog::AddHub)) {
            return Some(self.render_add_hub_dialog(window, cx));
        }
        if matches!(self.dialog, Some(Dialog::AddProvider)) {
            return Some(self.render_add_provider_dialog(window, cx));
        }
        if matches!(self.dialog, Some(Dialog::BackgroundActivity)) {
            return Some(self.render_background_activity_dialog(window, cx));
        }
        if matches!(self.dialog, Some(Dialog::ScriptEditor)) {
            return Some(self.render_script_editor_dialog(window, cx));
        }
        let Some(Dialog::Confirm(spec)) = &self.dialog else {
            return None;
        };
        let on_confirm = spec.on_confirm.clone();
        let cancel = cx.listener(|this, _, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        let confirm = cx.listener(move |this, _, window, cx| {
            this.dialog = None;
            on_confirm(this, window, cx);
            cx.notify();
        });
        let dismiss = self.cb(cx, |this, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        Some(dialog_frame(
            &theme,
            rems(32.),
            vec![
                dialog_header(spec.title.clone(), Some(spec.description.clone()), &theme),
                dialog_footer(
                    &theme,
                    vec![
                        Button::new("dialog-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Default)
                            .label("Cancel")
                            .on_click(cancel)
                            .into_any_element(),
                        Button::new("dialog-confirm")
                            .variant(if spec.destructive {
                                ButtonVariant::Destructive
                            } else {
                                ButtonVariant::Default
                            })
                            .label(spec.confirm_label.clone())
                            .on_click(confirm)
                            .into_any_element(),
                    ],
                ),
            ],
            dismiss,
        ))
    }
}

/// `sourceIdFromUrl`: stable per hub; dots and dashes in the host are kept.
pub fn usage_source_id_from_url(url: &str) -> String {
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in host.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    format!("cliproxy-{}", if slug.is_empty() { "hub" } else { slug })
}

impl SettingsView {
    fn render_add_hub_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let label_text = |text: &'static str| {
            div()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(text)
        };
        let url = self.input(
            "hub-url",
            "",
            "https://hub.example.ts.net:8318",
            window,
            cx,
            |_, _, _, _| {},
        );
        // `SecretInput`: the management key is write-only, like the provider secrets.
        let key = self.secret_input("hub-key", "Not set", window, cx);
        let label = self.input(
            "hub-label",
            "",
            "Defaults to the hub's host name",
            window,
            cx,
            |_, _, _, _| {},
        );
        let read = |this: &Self, name: &str, cx: &gpui_kit::App| {
            this.inputs
                .get(&SharedString::from(name.to_owned()))
                .map(|state| state.read(cx).value().trim().to_owned())
                .unwrap_or_default()
        };
        let can_save =
            !read(self, "hub-url", cx).is_empty() && !read(self, "hub-key", cx).is_empty();
        let close = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            for name in ["hub-url", "hub-key", "hub-label"] {
                if let Some(state) = this.inputs.get(&SharedString::from(name)) {
                    state.update(cx, |state, cx| state.set_value("", window, cx));
                }
            }
            this.dialog = None;
            cx.notify();
        };
        let dismiss = self.cb(cx, move |this, window, cx| close(this, window, cx));
        let environment = self.vm(cx).environment.label.clone();
        let field =
            |name: &'static str,
             input: &gpui_kit::Entity<gpui_kit::component::input::InputState>| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label_text(name))
                    .child(super::controls::text_field(
                        input,
                        None,
                        super::controls::FieldSize::Default,
                        false,
                        cx,
                    ))
            };
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(field("Hub URL", &url))
            .child(field("Management key", &key))
            .child(field("Label (optional)", &label));
        dialog_frame(
            &theme,
            rems(28.),
            vec![
                dialog_header(
                    "Add a CLIProxyAPI hub",
                    Some(
                        format!(
                            "Show the quota of every account the hub pools, next to the providers on {environment}. The key stays on that server."
                        )
                        .into(),
                    ),
                    &theme,
                ),
                dialog_panel("hub-body", body),
                dialog_footer(
                    &theme,
                    vec![
                        Button::new("hub-cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, window, cx| close(this, window, cx)))
                            .into_any_element(),
                        Button::new("hub-save")
                            .label("Add hub")
                            .disabled(!can_save)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let url = read(this, "hub-url", cx);
                                let key = read(this, "hub-key", cx);
                                let label = read(this, "hub-label", cx);
                                if url.is_empty() || key.is_empty() {
                                    return;
                                }
                                let id = usage_source_id_from_url(&url);
                                let mut entry = serde_json::json!({
                                    "kind": "cliproxy",
                                    "url": url,
                                    "managementKey": key,
                                    "enabled": true,
                                });
                                if !label.is_empty() {
                                    entry["label"] = serde_json::Value::String(label);
                                }
                                this.update_server(serde_json::json!({ "usageLimitSources": { id: entry } }), cx);
                                close(this, window, cx);
                            }))
                            .into_any_element(),
                    ],
                ),
            ],
            dismiss,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::usage_source_id_from_url;

    #[test]
    fn derives_a_stable_id_from_the_host() {
        assert_eq!(
            usage_source_id_from_url("https://hub.example.ts.net:8318/x"),
            "cliproxy-hub.example.ts.net-8318"
        );
        assert_eq!(
            usage_source_id_from_url("Foo-Bar.com"),
            "cliproxy-foo-bar.com"
        );
        assert_eq!(usage_source_id_from_url("://"), "cliproxy-hub");
    }
}
