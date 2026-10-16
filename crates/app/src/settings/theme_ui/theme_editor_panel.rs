//! Port of upstream/apps/web/src/components/settings/ThemeEditorPanel.tsx
//!
//! The draft model of the editor (everything `ThemeEditorPanel` keeps in
//! `useState` plus its save rules) lives here and is testable without a
//! window; `ThemeEditorPanel` the view is a floating panel painted from it.
//! DOM-only parts (pointer-driven element inspection, `MutationObserver`
//! refreshes, scrolling a role into view) are adapted in the view.

use serde_json::{Map, Value};

use crate::theme::theme_palette::{
    get_standard_theme_colors, get_theme_colors_for_mode, get_theme_modes,
};
use crate::theme::theme_palette_library::{
    THEME_FILE_VERSION, create_vivid_theme_colors, get_color_role, get_custom_themes,
    install_custom_theme, is_theme_color, parse_theme_file, remove_custom_theme, set_color_role,
    theme_colors_to_camel_map, theme_id_from_name, update_custom_theme, update_theme_color_family,
};
use crate::theme::theme_palettes::{
    THEME_COLOR_ROLES, ThemeAppearance, ThemeColors, ThemeDefinition, ThemeVariants,
};

fn theme_variants_json(variants: &ThemeVariants) -> Map<String, Value> {
    let mut map = Map::new();
    if let Some(light) = &variants.light {
        map.insert(
            "light".into(),
            Value::Object(theme_colors_to_camel_map(light)),
        );
    }
    if let Some(dark) = &variants.dark {
        map.insert(
            "dark".into(),
            Value::Object(theme_colors_to_camel_map(dark)),
        );
    }
    map
}

/// The `parseThemeFile` input for `theme`, with `variants` replacing its own.
fn theme_file_json(theme: &ThemeDefinition, variants: Map<String, Value>) -> Map<String, Value> {
    let mut file = Map::new();
    file.insert("version".into(), THEME_FILE_VERSION.into());
    file.insert("id".into(), theme.id.to_string().into());
    file.insert("name".into(), theme.label.to_string().into());
    file.insert(
        "appearance".into(),
        match theme.appearance {
            ThemeAppearance::Light => "light",
            ThemeAppearance::Dark => "dark",
        }
        .into(),
    );
    file.insert(
        "colors".into(),
        Value::Object(theme_colors_to_camel_map(&theme.colors)),
    );
    file.insert("variants".into(), Value::Object(variants));
    file
}

// ── Role groups ──────────────────────────────────────────────────────

/// `THEME_EDITOR_SIMPLE_ROLES`.
pub const THEME_EDITOR_SIMPLE_ROLES: [&str; 2] = ["canvas", "accent"];

/// `ThemeEditorColorFamily`: one picker that drives several roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeEditorColorFamily {
    pub id: &'static str,
    pub label: &'static str,
    pub role: &'static str,
    pub roles: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeEditorRoleGroup {
    pub id: &'static str,
    pub title: &'static str,
    pub families: &'static [ThemeEditorColorFamily],
}

macro_rules! family {
    ($id:expr, $label:expr, $role:expr, [$($roles:expr),+ $(,)?]) => {
        ThemeEditorColorFamily {
            id: $id,
            label: $label,
            role: $role,
            roles: &[$($roles),+],
        }
    };
}

/// `THEME_EDITOR_ROLE_GROUPS` (roles in snake_case, as `ThemeColors` fields).
pub const THEME_EDITOR_ROLE_GROUPS: &[ThemeEditorRoleGroup] = &[
    ThemeEditorRoleGroup {
        id: "foundation",
        title: "Foundation",
        families: &[
            family!(
                "background",
                "Background",
                "canvas",
                ["canvas", "chrome", "toolbar"]
            ),
            family!("surface", "Surface", "surface", ["surface"]),
            family!(
                "raised-surface",
                "Raised surface",
                "surface_raised",
                ["surface_raised"]
            ),
            family!("overlay", "Overlay", "surface_overlay", ["surface_overlay"]),
            family!(
                "text",
                "Text",
                "text",
                ["text", "toolbar_foreground", "toolbar_control_foreground"]
            ),
            family!(
                "muted-text",
                "Muted text",
                "muted_foreground",
                [
                    "text_muted",
                    "muted_foreground",
                    "placeholder",
                    "secondary_label",
                    "icon_muted",
                    "sidebar_muted_foreground",
                ]
            ),
            family!(
                "border",
                "Border",
                "border",
                ["border", "toolbar_border", "sidebar_border"]
            ),
            family!("input", "Input", "input", ["input"]),
        ],
    },
    ThemeEditorRoleGroup {
        id: "brand-content",
        title: "Brand & content",
        families: &[
            family!(
                "subtle-surface",
                "Subtle surface",
                "secondary",
                [
                    "secondary",
                    "secondary_foreground",
                    "muted",
                    "toolbar_control"
                ]
            ),
            family!(
                "highlight-surface",
                "Highlight surface",
                "accent_surface",
                [
                    "accent_surface",
                    "accent_surface_foreground",
                    "toolbar_control_hover"
                ]
            ),
            family!(
                "accent",
                "Accent",
                "accent",
                [
                    "accent",
                    "accent_foreground",
                    "focus",
                    "update",
                    "update_foreground",
                    "update_surface",
                    "terminal_cursor",
                ]
            ),
            family!(
                "action",
                "Action",
                "message_action",
                [
                    "message_action",
                    "message_action_foreground",
                    "message_action_hover"
                ]
            ),
            family!(
                "message-surface",
                "Message surface",
                "message_surface",
                ["message_surface", "message_foreground"]
            ),
            family!(
                "code-surface",
                "Code surface",
                "code_background",
                ["code_background", "code_foreground"]
            ),
        ],
    },
    ThemeEditorRoleGroup {
        id: "search",
        title: "Search",
        families: &[
            family!(
                "search-matches",
                "Search matches",
                "search_match_background",
                ["search_match_background", "search_match_foreground"]
            ),
            family!(
                "current-match",
                "Current match",
                "search_match_active_background",
                [
                    "search_match_active_background",
                    "search_match_active_foreground"
                ]
            ),
        ],
    },
    ThemeEditorRoleGroup {
        id: "context",
        title: "Context",
        families: &[
            family!(
                "sidebar-background",
                "Sidebar background",
                "sidebar",
                ["sidebar", "sidebar_foreground"]
            ),
            family!(
                "sidebar-controls",
                "Sidebar controls",
                "sidebar_control_surface",
                ["sidebar_control_surface"]
            ),
            family!(
                "sidebar-selection",
                "Sidebar selection",
                "sidebar_row_selected",
                [
                    "sidebar_row_hover",
                    "sidebar_row_active",
                    "sidebar_row_selected"
                ]
            ),
            family!(
                "terminal-background",
                "Terminal background",
                "terminal_background",
                [
                    "terminal_background",
                    "terminal_foreground",
                    "terminal_selection",
                    "terminal_scrollbar",
                    "terminal_scrollbar_hover",
                ]
            ),
        ],
    },
    ThemeEditorRoleGroup {
        id: "status",
        title: "Status",
        families: &[
            family!(
                "error",
                "Error",
                "error",
                ["error", "error_foreground", "error_surface"]
            ),
            family!(
                "warning",
                "Warning",
                "warning",
                ["warning", "warning_foreground", "warning_surface"]
            ),
        ],
    },
];

/// `getThemeEditorColorFamily`.
pub fn get_theme_editor_color_family(role: &str) -> Option<&'static ThemeEditorColorFamily> {
    THEME_EDITOR_ROLE_GROUPS
        .iter()
        .flat_map(|group| group.families.iter())
        .find(|family| family.roles.contains(&role))
}

// ── Draft state ──────────────────────────────────────────────────────

/// Both appearances' palettes (`ThemeEditorColorsByAppearance`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeEditorColorsByAppearance {
    pub light: ThemeColors,
    pub dark: ThemeColors,
}

impl ThemeEditorColorsByAppearance {
    pub fn get(&self, appearance: ThemeAppearance) -> &ThemeColors {
        match appearance {
            ThemeAppearance::Light => &self.light,
            ThemeAppearance::Dark => &self.dark,
        }
    }

    pub fn get_mut(&mut self, appearance: ThemeAppearance) -> &mut ThemeColors {
        match appearance {
            ThemeAppearance::Light => &mut self.light,
            ThemeAppearance::Dark => &mut self.dark,
        }
    }

    /// `getThemeEditorColorsByAppearance`: a draft with no source theme starts
    /// as the standard T3 Code look -- the palette on screen when no theme is
    /// installed -- so creating from the default theme changes nothing until
    /// the user edits a color.
    pub fn standard() -> Self {
        Self {
            light: get_standard_theme_colors(ThemeAppearance::Light),
            dark: get_standard_theme_colors(ThemeAppearance::Dark),
        }
    }
}

/// `isThemeEditorColor`.
pub fn is_theme_editor_color(value: &str) -> bool {
    is_theme_color(value.trim())
}

/// `getManagedEditorColors`: the editor keeps the user's exact picks and
/// derives the rest through the perceptual vivid engine, so a two-color theme
/// carries its own identity.
pub fn get_managed_editor_colors(appearance: ThemeAppearance, colors: &ThemeColors) -> ThemeColors {
    let defaults = get_standard_theme_colors(appearance);
    create_vivid_theme_colors(
        appearance,
        if is_theme_editor_color(colors.canvas.as_str()) {
            colors.canvas.as_str()
        } else {
            defaults.canvas.as_str()
        },
        if is_theme_editor_color(colors.accent.as_str()) {
            colors.accent.as_str()
        } else {
            defaults.accent.as_str()
        },
    )
}

/// What `onSaved` receives besides the theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThemeSavedContext {
    pub created: bool,
    /// Set when a create merged its palette into an existing theme.
    pub merged_appearance: Option<ThemeAppearance>,
}

/// The editor's local state (the `useState` cluster of `ThemeEditorPanel`).
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeEditorDraft {
    pub name: String,
    pub active_appearance: ThemeAppearance,
    pub is_advanced: bool,
    pub colors_by_appearance: ThemeEditorColorsByAppearance,
    pub simple_colors_dirty_light: bool,
    pub simple_colors_dirty_dark: bool,
    pub should_regenerate_guided_colors: bool,
    pub error: Option<String>,
    pub role_query: String,
    pub is_inspecting: bool,
    pub selected_role: Option<&'static str>,
    pub usage_count: Option<usize>,
}

fn appearance_name(appearance: ThemeAppearance) -> &'static str {
    match appearance {
        ThemeAppearance::Light => "light",
        ThemeAppearance::Dark => "dark",
    }
}

fn other_appearance(appearance: ThemeAppearance) -> ThemeAppearance {
    match appearance {
        ThemeAppearance::Light => ThemeAppearance::Dark,
        ThemeAppearance::Dark => ThemeAppearance::Light,
    }
}

fn static_role(role: &str) -> Option<&'static str> {
    THEME_COLOR_ROLES
        .iter()
        .copied()
        .find(|candidate| *candidate == role)
}

impl ThemeEditorDraft {
    /// The seeding effect: editing works on the theme itself; creating starts
    /// from the theme that is currently in use, so tuning what you already run
    /// is an edit away instead of a rebuild from the defaults.
    pub fn seeded(
        editing_theme: Option<&ThemeDefinition>,
        seed_theme: Option<&ThemeDefinition>,
        seed_name: Option<&str>,
        initial_appearance: ThemeAppearance,
    ) -> Self {
        let source_theme = editing_theme.or(seed_theme);
        let mut colors = ThemeEditorColorsByAppearance::standard();
        let active_appearance = match source_theme {
            Some(source) => {
                if get_theme_colors_for_mode(source, initial_appearance).is_some() {
                    initial_appearance
                } else {
                    source.appearance
                }
            }
            None => initial_appearance,
        };
        if let Some(source) = source_theme {
            *colors.get_mut(source.appearance) = source.colors.clone();
            for appearance in [ThemeAppearance::Light, ThemeAppearance::Dark] {
                let variant = match appearance {
                    ThemeAppearance::Light => source.variants.light.as_ref(),
                    ThemeAppearance::Dark => source.variants.dark.as_ref(),
                };
                if let Some(variant_colors) = variant {
                    *colors.get_mut(appearance) = variant_colors.clone();
                }
            }
        }
        // Themes saved by the guided editor carry the managed flag; anything
        // else (imports, hand-edited files, older saves) opens in advanced mode
        // so guided regeneration cannot silently discard hand-tuned colors. A
        // seeded new theme follows the same rule: its palette is only safe to
        // regenerate when the guided editor produced it.
        let unmanaged_source = source_theme.is_some_and(|source| !source.managed);
        Self {
            name: editing_theme
                .map(|theme| theme.label.to_string())
                .or_else(|| seed_name.map(str::to_string))
                .unwrap_or_default(),
            active_appearance,
            is_advanced: unmanaged_source,
            colors_by_appearance: colors,
            simple_colors_dirty_light: false,
            simple_colors_dirty_dark: false,
            // An unmanaged palette needs conversion when the user opts into the
            // guided editor. Merely revealing Advanced for a managed/default
            // draft must stay read-only until a color changes.
            should_regenerate_guided_colors: unmanaged_source,
            error: None,
            role_query: String::new(),
            is_inspecting: false,
            selected_role: None,
            usage_count: None,
        }
    }

    pub fn colors(&self) -> &ThemeColors {
        self.colors_by_appearance.get(self.active_appearance)
    }

    /// The draft the live app wears while the editor is open.
    pub fn preview(&self) -> (&ThemeColors, ThemeAppearance) {
        (self.colors(), self.active_appearance)
    }

    fn simple_dirty(&self, appearance: ThemeAppearance) -> bool {
        match appearance {
            ThemeAppearance::Light => self.simple_colors_dirty_light,
            ThemeAppearance::Dark => self.simple_colors_dirty_dark,
        }
    }

    fn set_simple_dirty(&mut self, appearance: ThemeAppearance, dirty: bool) {
        match appearance {
            ThemeAppearance::Light => self.simple_colors_dirty_light = dirty,
            ThemeAppearance::Dark => self.simple_colors_dirty_dark = dirty,
        }
    }

    // A name an installed theme already uses combines instead of failing:
    // creating adds the new palette to that theme, and renaming an existing
    // theme onto it folds the edited palette in and retires the old entry --
    // light "My Theme" plus a dark "My Theme" become one theme with both modes.
    // Labels are matched as well as derived ids: a rename keeps a theme's
    // original id, so its label is the only name a user can see and retype.
    pub fn merge_target(&self, editing_theme: Option<&ThemeDefinition>) -> Option<ThemeDefinition> {
        let name_target_id = theme_id_from_name(&self.name);
        let normalized_name = self.name.trim().to_lowercase();
        if normalized_name.is_empty() {
            return None;
        }
        get_custom_themes().into_iter().find(|theme| {
            Some(theme.id.as_str()) != editing_theme.map(|editing| editing.id.as_str())
                && (theme.id.as_str() == name_target_id
                    || theme.label.trim().to_lowercase() == normalized_name)
        })
    }

    /// The appearance a mode button would produce can be blocked two ways: the
    /// merge target already has that palette, or the theme being edited never
    /// had it (adding one is a create-with-same-name away).
    pub fn appearance_lock_reason(
        &self,
        appearance: ThemeAppearance,
        editing_theme: Option<&ThemeDefinition>,
        merge_target: Option<&ThemeDefinition>,
    ) -> Option<String> {
        if let Some(editing) = editing_theme
            && !get_theme_modes(editing).contains(&appearance)
        {
            return Some(format!(
                "\u{201c}{}\u{201d} has no {} palette. Create a theme with the same name to add one.",
                editing.label,
                appearance_name(appearance)
            ));
        }
        if editing_theme.is_none()
            && let Some(target) = merge_target
            && get_theme_modes(target).contains(&appearance)
        {
            return Some(format!(
                "\u{201c}{}\u{201d} already has a {} palette.",
                target.label,
                appearance_name(appearance)
            ));
        }
        None
    }

    /// Typing a name: most save failures are name collisions; retyping is the
    /// fix, so the stale message goes with the old name. Typing a name whose
    /// theme already owns the selected appearance flips the draft to the free
    /// side, so the merge affordance works without a manual toggle. Both
    /// sides taken leaves the selection alone; save is blocked with an
    /// explanation instead.
    pub fn set_name(&mut self, name: impl Into<String>, editing_theme: Option<&ThemeDefinition>) {
        self.name = name.into();
        self.error = None;
        if editing_theme.is_some() {
            return;
        }
        let Some(target) = self.merge_target(None) else {
            return;
        };
        let taken = get_theme_modes(&target);
        if taken.len() != 1 {
            return;
        }
        if taken.contains(&self.active_appearance) {
            self.active_appearance = other_appearance(taken[0]);
        }
    }

    /// Switches the visible appearance when the lock rules allow it.
    pub fn set_active_appearance(
        &mut self,
        appearance: ThemeAppearance,
        editing_theme: Option<&ThemeDefinition>,
    ) -> bool {
        let merge_target = self.merge_target(editing_theme);
        if self
            .appearance_lock_reason(appearance, editing_theme, merge_target.as_ref())
            .is_some()
        {
            return false;
        }
        self.active_appearance = appearance;
        true
    }

    /// `updateColor`.
    pub fn update_color(&mut self, role: &str, value: &str) {
        let appearance = self.active_appearance;
        let manages_simple_role = !self.is_advanced
            && THEME_EDITOR_SIMPLE_ROLES.contains(&role)
            && is_theme_editor_color(value);
        let current = self.colors_by_appearance.get(appearance).clone();
        let mut next_colors = current.clone();
        set_color_role(&mut next_colors, role, value.to_string().into());
        let updated = if self.is_advanced {
            update_theme_color_family(appearance, current, role, value)
        } else if manages_simple_role {
            get_managed_editor_colors(appearance, &next_colors)
        } else {
            next_colors
        };
        *self.colors_by_appearance.get_mut(appearance) = updated;
        if manages_simple_role {
            self.set_simple_dirty(appearance, true);
        }
        if self.is_advanced {
            self.should_regenerate_guided_colors = true;
        }
    }

    /// `selectThemeRole`.
    pub fn select_theme_role(&mut self, role: &str) {
        let visible_role = get_theme_editor_color_family(role)
            .map(|family| family.role)
            .or_else(|| static_role(role));
        self.selected_role = visible_role;
        if let Some(visible) = visible_role
            && !THEME_EDITOR_SIMPLE_ROLES.contains(&visible)
        {
            self.is_advanced = true;
            self.role_query.clear();
        }
    }

    /// `toggleThemeRole`.
    pub fn toggle_theme_role(&mut self, role: &str) {
        let role = static_role(role);
        self.selected_role = if self.selected_role == role {
            None
        } else {
            role
        };
    }

    /// `clearInspectorSelection`.
    pub fn clear_inspector_selection(&mut self) {
        self.selected_role = None;
        self.usage_count = None;
        self.is_inspecting = false;
    }

    /// The roles whose usage the inspector spotlights for the selection.
    pub fn selected_highlight_roles(&self) -> Vec<&'static str> {
        let Some(selected) = self.selected_role else {
            return Vec::new();
        };
        if self.is_advanced {
            return get_theme_editor_color_family(selected)
                .map(|family| family.roles.to_vec())
                .unwrap_or_else(|| vec![selected]);
        }
        if THEME_EDITOR_SIMPLE_ROLES.contains(&selected) {
            let colors = self.colors();
            let selected_value = get_color_role(colors, selected)
                .map(|value| value.trim().to_lowercase())
                .unwrap_or_default();
            return THEME_COLOR_ROLES
                .iter()
                .copied()
                .filter(|role| {
                    get_color_role(colors, role)
                        .is_some_and(|value| value.trim().to_lowercase() == selected_value)
                })
                .collect();
        }
        vec![selected]
    }

    /// `handleAdvancedChange`.
    pub fn handle_advanced_change(
        &mut self,
        checked: bool,
        editing_theme: Option<&ThemeDefinition>,
    ) {
        self.is_advanced = checked;
        if checked {
            return;
        }
        if self
            .selected_role
            .is_some_and(|role| !THEME_EDITOR_SIMPLE_ROLES.contains(&role))
        {
            self.selected_role = None;
        }
        if !self.should_regenerate_guided_colors {
            return;
        }

        // Regenerate every appearance the theme will save, not just the visible
        // one, so the palettes shown after toggling match what gets saved.
        let managed_appearances: Vec<ThemeAppearance> = match editing_theme {
            Some(editing) if get_theme_modes(editing).len() > 1 => {
                vec![ThemeAppearance::Light, ThemeAppearance::Dark]
            }
            _ => vec![self.active_appearance],
        };
        for appearance in managed_appearances {
            self.set_simple_dirty(appearance, true);
            let regenerated =
                get_managed_editor_colors(appearance, self.colors_by_appearance.get(appearance));
            *self.colors_by_appearance.get_mut(appearance) = regenerated;
        }
        self.should_regenerate_guided_colors = false;
    }

    /// The label of the save button.
    pub fn submit_label(
        &self,
        editing_theme: Option<&ThemeDefinition>,
        merge_target: Option<&ThemeDefinition>,
    ) -> String {
        match (editing_theme.is_some(), merge_target) {
            (true, Some(target)) => format!("Merge into \u{201c}{}\u{201d}", target.label),
            (true, None) => "Save changes".to_string(),
            (false, Some(_)) => format!("Add {} palette", appearance_name(self.active_appearance)),
            (false, None) => "Create theme".to_string(),
        }
    }

    /// `handleSubmit`. Returns whether the editor should close. The error, if
    /// any, lands in [`Self::error`].
    pub fn submit(
        &mut self,
        editing_theme: Option<&ThemeDefinition>,
        on_saved: &mut dyn FnMut(&ThemeDefinition, ThemeSavedContext) -> bool,
    ) -> bool {
        if self.name.trim().is_empty() {
            self.error = Some("Name your theme first.".to_string());
            return false;
        }
        let is_editing = editing_theme.is_some();
        match self.try_submit(editing_theme, on_saved) {
            Ok(closed) => closed,
            Err(message) => {
                self.error = Some(if message.is_empty() {
                    if is_editing {
                        "Could not save the theme.".to_string()
                    } else {
                        "Could not create the theme.".to_string()
                    }
                } else {
                    message
                });
                false
            }
        }
    }

    fn try_submit(
        &mut self,
        editing_theme: Option<&ThemeDefinition>,
        on_saved: &mut dyn FnMut(&ThemeDefinition, ThemeSavedContext) -> bool,
    ) -> Result<bool, String> {
        let merge_target = self.merge_target(editing_theme);
        let taken_appearances = merge_target
            .as_ref()
            .map(get_theme_modes)
            .unwrap_or_default();
        // Only regenerate palettes the user actually touched in guided mode, so
        // untouched appearances save exactly what the editor displayed.
        let colors_for_save = if self.is_advanced {
            self.colors_by_appearance.clone()
        } else {
            ThemeEditorColorsByAppearance {
                light: if self.simple_colors_dirty_light {
                    get_managed_editor_colors(
                        ThemeAppearance::Light,
                        &self.colors_by_appearance.light,
                    )
                } else {
                    self.colors_by_appearance.light.clone()
                },
                dark: if self.simple_colors_dirty_dark {
                    get_managed_editor_colors(
                        ThemeAppearance::Dark,
                        &self.colors_by_appearance.dark,
                    )
                } else {
                    self.colors_by_appearance.dark.clone()
                },
            }
        };

        let mut merged_appearance: Option<ThemeAppearance> = None;
        let mut retired_theme: Option<ThemeDefinition> = None;
        let saved_theme: ThemeDefinition;
        if let (Some(editing), Some(target)) = (editing_theme, merge_target.as_ref()) {
            // Renamed onto another installed theme: this theme's palettes fold
            // into it and the edited entry retires, so both cards become one.
            // Colliding palettes cannot merge -- neither side should be
            // silently overwritten.
            let edited_modes = get_theme_modes(editing);
            if let Some(collision) = edited_modes
                .iter()
                .find(|mode| taken_appearances.contains(mode))
            {
                self.error = Some(format!(
                    "\u{201c}{}\u{201d} already has a {} palette. Pick another name.",
                    target.label,
                    appearance_name(*collision)
                ));
                return Ok(false);
            }
            merged_appearance = edited_modes.first().copied();
            let mut variants = theme_variants_json(&target.variants);
            for mode in &edited_modes {
                variants.insert(
                    appearance_name(*mode).to_string(),
                    Value::Object(theme_colors_to_camel_map(colors_for_save.get(*mode))),
                );
            }
            let mut file = theme_file_json(target, variants);
            if target.managed && !self.is_advanced {
                file.insert("managed".into(), Value::Bool(true));
            }
            let mut parsed = parse_theme_file(Value::Object(file))?;
            if let Some(collection) = &target.collection {
                parsed.collection = Some(collection.clone());
            }
            saved_theme = update_custom_theme(parsed)?;
            retired_theme = Some(editing.clone());
            if let Err(cause) = remove_custom_theme(&editing.id) {
                // The merge already persisted. Leaving it while the edited
                // theme survives would collide on every retry, so the target
                // goes back to its pre-merge palettes before the failure
                // surfaces. Storage failing wholesale is reported by the cause.
                let _ = update_custom_theme(target.clone());
                return Err(cause);
            }
        } else if let Some(editing) = editing_theme {
            let base_appearance = editing.appearance;
            let variant_appearance = other_appearance(base_appearance);
            let mut file = Map::new();
            file.insert("version".into(), THEME_FILE_VERSION.into());
            file.insert("id".into(), editing.id.to_string().into());
            file.insert("name".into(), self.name.clone().into());
            file.insert("appearance".into(), appearance_name(base_appearance).into());
            file.insert(
                "colors".into(),
                Value::Object(theme_colors_to_camel_map(
                    colors_for_save.get(base_appearance),
                )),
            );
            if get_theme_modes(editing).len() > 1 {
                let mut variants = Map::new();
                variants.insert(
                    appearance_name(variant_appearance).to_string(),
                    Value::Object(theme_colors_to_camel_map(
                        colors_for_save.get(variant_appearance),
                    )),
                );
                file.insert("variants".into(), Value::Object(variants));
            }
            if !self.is_advanced {
                file.insert("managed".into(), Value::Bool(true));
            }
            let mut parsed = parse_theme_file(Value::Object(file))?;
            if let Some(collection) = &editing.collection {
                parsed.collection = Some(collection.clone());
            }
            saved_theme = update_custom_theme(parsed)?;
        } else if let Some(target) = merge_target.as_ref() {
            if taken_appearances.contains(&self.active_appearance) {
                self.error = Some(format!(
                    "\u{201c}{}\u{201d} already has light and dark palettes. Pick another name.",
                    target.label
                ));
                return Ok(false);
            }
            // The new palette joins the existing theme as its other mode; its
            // stored palettes are untouched. The guided (managed) flag only
            // survives when every palette in the theme came from the guided
            // editor.
            merged_appearance = Some(self.active_appearance);
            let mut variants = theme_variants_json(&target.variants);
            variants.insert(
                appearance_name(self.active_appearance).to_string(),
                Value::Object(theme_colors_to_camel_map(
                    colors_for_save.get(self.active_appearance),
                )),
            );
            let mut file = theme_file_json(target, variants);
            if target.managed && !self.is_advanced {
                file.insert("managed".into(), Value::Bool(true));
            }
            let mut parsed = parse_theme_file(Value::Object(file))?;
            if let Some(collection) = &target.collection {
                parsed.collection = Some(collection.clone());
            }
            saved_theme = update_custom_theme(parsed)?;
        } else {
            let mut file = Map::new();
            file.insert("version".into(), THEME_FILE_VERSION.into());
            file.insert("name".into(), self.name.clone().into());
            file.insert(
                "appearance".into(),
                appearance_name(self.active_appearance).into(),
            );
            file.insert(
                "colors".into(),
                Value::Object(theme_colors_to_camel_map(
                    colors_for_save.get(self.active_appearance),
                )),
            );
            if !self.is_advanced {
                file.insert("managed".into(), Value::Bool(true));
            }
            saved_theme = install_custom_theme(parse_theme_file(Value::Object(file))?)?;
        }

        let context = ThemeSavedContext {
            created: editing_theme.is_none() && merged_appearance.is_none(),
            merged_appearance,
        };
        if !on_saved(&saved_theme, context) {
            if editing_theme.is_none() && merged_appearance.is_none() {
                // Roll the install back so a retry can run it again instead of
                // failing on the already-taken theme id. Storage failing
                // wholesale is covered by the error below.
                let _ = remove_custom_theme(&saved_theme.id);
            } else if let (Some(target), Some(_)) = (merge_target.as_ref(), merged_appearance) {
                // Put the pre-merge definitions back for the same reason.
                let _ = update_custom_theme(target.clone());
                if let Some(retired) = &retired_theme {
                    let _ = install_custom_theme(retired.clone());
                }
            }
            self.error = Some("Theme saved, but it could not be made active. Try again.".into());
            return Ok(false);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::theme::THEME_GLOBALS_TEST_LOCK;
    use crate::theme::theme_palette_library::{
        InMemoryThemeLibraryStorage, invalidate_custom_themes, set_theme_library_storage,
    };

    /// A fresh in-memory theme library for the duration of a test.
    fn library() -> parking_lot::MutexGuard<'static, ()> {
        let guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        guard
    }

    fn install(name: &str, appearance: &str, canvas: &str, managed: bool) -> ThemeDefinition {
        install_custom_theme(
            parse_theme_file(json!({
                "version": 1,
                "name": name,
                "appearance": appearance,
                "colors": { "canvas": canvas, "accent": "#5b6cff" },
                "managed": managed,
            }))
            .expect("theme"),
        )
        .expect("install")
    }

    fn accept(_: &ThemeDefinition, _: ThemeSavedContext) -> bool {
        true
    }

    fn draft(name: &str, appearance: ThemeAppearance) -> ThemeEditorDraft {
        let mut draft = ThemeEditorDraft::seeded(None, None, None, appearance);
        draft.set_name(name, None);
        draft
    }

    fn modes(theme: &ThemeDefinition) -> Vec<ThemeAppearance> {
        get_theme_modes(theme)
    }

    #[test]
    fn role_groups_cover_every_role_once_through_their_families() {
        let mut seen = std::collections::HashSet::new();
        for group in THEME_EDITOR_ROLE_GROUPS {
            for family in group.families {
                assert!(family.roles.contains(&family.role), "{}", family.id);
                for role in family.roles {
                    assert!(THEME_COLOR_ROLES.contains(role), "unknown role {role}");
                    assert!(seen.insert(*role), "{role} appears in two families");
                }
            }
        }
        // Every role a user can pick is reachable through some family.
        for role in THEME_COLOR_ROLES {
            assert!(seen.contains(role), "{role} has no family");
        }
        assert_eq!(
            get_theme_editor_color_family("toolbar").map(|f| f.label),
            Some("Background")
        );
        assert_eq!(get_theme_editor_color_family("not_a_role"), None);
    }

    #[test]
    fn a_new_draft_starts_as_the_standard_look_in_guided_mode() {
        let draft = ThemeEditorDraft::seeded(None, None, None, ThemeAppearance::Dark);
        assert_eq!(draft.active_appearance, ThemeAppearance::Dark);
        assert!(!draft.is_advanced);
        assert!(!draft.should_regenerate_guided_colors);
        assert_eq!(draft.name, "");
        assert_eq!(
            draft.colors_by_appearance,
            ThemeEditorColorsByAppearance::standard()
        );
    }

    #[test]
    fn seeding_from_an_unmanaged_theme_opens_advanced_and_prefers_the_requested_appearance() {
        let _guard = library();
        let theme = install("Paper", "light", "#f8fbff", false);
        let draft = ThemeEditorDraft::seeded(Some(&theme), None, None, ThemeAppearance::Dark);
        // A light-only theme cannot show dark: the draft follows the theme.
        assert_eq!(draft.active_appearance, ThemeAppearance::Light);
        assert!(draft.is_advanced);
        assert!(draft.should_regenerate_guided_colors);
        assert_eq!(draft.name, "Paper");
        assert_eq!(draft.colors_by_appearance.light, theme.colors);
        // The unseeded appearance keeps the standard palette.
        assert_eq!(
            draft.colors_by_appearance.dark,
            get_standard_theme_colors(ThemeAppearance::Dark)
        );
    }

    #[test]
    fn seeding_from_a_managed_theme_stays_guided_and_a_seed_name_prefills_the_name() {
        let _guard = library();
        let theme = install("Aurora", "dark", "#101827", true);
        let draft = ThemeEditorDraft::seeded(
            None,
            Some(&theme),
            Some("Aurora copy"),
            ThemeAppearance::Dark,
        );
        assert!(!draft.is_advanced);
        assert_eq!(draft.name, "Aurora copy");
        assert_eq!(draft.colors_by_appearance.dark, theme.colors);
    }

    #[test]
    fn a_guided_color_edit_regenerates_the_palette_from_the_two_seeds() {
        let mut draft = draft("Aurora", ThemeAppearance::Light);
        let before = draft.colors().clone();
        draft.update_color("canvas", "#f8fbff");
        assert!(draft.simple_colors_dirty_light);
        assert!(!draft.simple_colors_dirty_dark);
        let after = draft.colors();
        assert_ne!(after.sidebar, before.sidebar);
        assert_ne!(after.canvas, before.canvas);
        // Half-typed values are kept verbatim without regenerating.
        let regenerated = after.clone();
        draft.update_color("accent", "#12");
        assert_eq!(draft.colors().accent.as_str(), "#12");
        assert_eq!(draft.colors().sidebar, regenerated.sidebar);
    }

    #[test]
    fn an_advanced_edit_updates_the_color_family_and_arms_guided_regeneration() {
        let mut draft = draft("Aurora", ThemeAppearance::Light);
        draft.is_advanced = true;
        draft.update_color("terminal_background", "#102030");
        assert!(draft.should_regenerate_guided_colors);
        assert!(!draft.simple_colors_dirty_light);
        assert_ne!(
            draft.colors().terminal_background,
            get_standard_theme_colors(ThemeAppearance::Light).terminal_background
        );
    }

    #[test]
    fn turning_advanced_off_regenerates_only_after_advanced_edits() {
        let mut draft = draft("Aurora", ThemeAppearance::Dark);
        draft.handle_advanced_change(true, None);
        assert!(draft.is_advanced);
        // Revealing Advanced alone is read-only.
        let untouched = draft.colors().clone();
        draft.handle_advanced_change(false, None);
        assert_eq!(draft.colors(), &untouched);
        assert!(!draft.simple_colors_dirty_dark);

        draft.handle_advanced_change(true, None);
        draft.update_color("error", "#aa2222");
        draft.selected_role = Some("error");
        draft.handle_advanced_change(false, None);
        assert!(!draft.should_regenerate_guided_colors);
        assert!(draft.simple_colors_dirty_dark);
        assert_eq!(draft.selected_role, None);
    }

    #[test]
    fn selecting_a_non_simple_role_reveals_advanced_and_clears_the_filter() {
        let mut draft = draft("x", ThemeAppearance::Light);
        draft.role_query = "side".into();
        draft.select_theme_role("sidebar_row_hover");
        assert_eq!(draft.selected_role, Some("sidebar_row_selected"));
        assert!(draft.is_advanced);
        assert_eq!(draft.role_query, "");

        draft.toggle_theme_role("sidebar_row_selected");
        assert_eq!(draft.selected_role, None);
        draft.toggle_theme_role("sidebar_row_selected");
        assert_eq!(draft.selected_role, Some("sidebar_row_selected"));
        draft.clear_inspector_selection();
        assert_eq!(
            (draft.selected_role, draft.usage_count, draft.is_inspecting),
            (None, None, false)
        );
    }

    #[test]
    fn highlight_roles_follow_the_family_when_advanced_and_equal_colors_when_guided() {
        let mut draft = draft("x", ThemeAppearance::Light);
        draft.select_theme_role("canvas");
        // Guided: every role currently equal to the canvas value.
        let canvas = draft.colors().canvas.to_string();
        let roles = draft.selected_highlight_roles();
        assert!(roles.contains(&"canvas"));
        for role in &roles {
            assert_eq!(
                get_color_role(draft.colors(), role).map(|v| v.to_string()),
                Some(canvas.clone())
            );
        }
        draft.is_advanced = true;
        assert_eq!(
            draft.selected_highlight_roles(),
            vec!["canvas", "chrome", "toolbar"]
        );
        draft.selected_role = Some("error");
        assert_eq!(
            draft.selected_highlight_roles(),
            vec!["error", "error_foreground", "error_surface"]
        );
    }

    #[test]
    fn creating_installs_a_managed_theme_and_reports_created() {
        let _guard = library();
        let mut draft = draft("Aurora Night", ThemeAppearance::Dark);
        draft.update_color("canvas", "#101827");
        let mut context = None;
        let closed = draft.submit(None, &mut |theme, ctx| {
            context = Some((theme.id.to_string(), ctx));
            true
        });
        assert!(closed);
        assert_eq!(draft.error, None);
        let (id, ctx) = context.expect("saved");
        assert_eq!(id, "aurora-night");
        assert_eq!(
            ctx,
            ThemeSavedContext {
                created: true,
                merged_appearance: None
            }
        );
        let saved = get_custom_themes().remove(0);
        assert!(saved.managed);
        assert_eq!(saved.appearance, ThemeAppearance::Dark);
    }

    #[test]
    fn an_empty_name_blocks_the_save() {
        let _guard = library();
        let mut draft = draft("   ", ThemeAppearance::Light);
        assert!(!draft.submit(None, &mut accept));
        assert_eq!(draft.error.as_deref(), Some("Name your theme first."));
        assert!(get_custom_themes().is_empty());
    }

    #[test]
    fn a_failed_activation_rolls_the_install_back_so_a_retry_can_run() {
        let _guard = library();
        let mut draft = draft("Aurora", ThemeAppearance::Light);
        assert!(!draft.submit(None, &mut |_, _| false));
        assert_eq!(
            draft.error.as_deref(),
            Some("Theme saved, but it could not be made active. Try again.")
        );
        assert!(get_custom_themes().is_empty());
        assert!(draft.submit(None, &mut accept));
        assert_eq!(get_custom_themes().len(), 1);
    }

    #[test]
    fn a_taken_id_surfaces_the_library_error_and_retyping_clears_it() {
        let _guard = library();
        let mut grove_clash = draft("Grove", ThemeAppearance::Light);
        // `grove` is a built-in id, so it cannot be installed.
        assert!(!grove_clash.submit(None, &mut accept));
        assert!(grove_clash.error.is_some());
        grove_clash.set_name("Grove two", None);
        assert_eq!(grove_clash.error, None);
    }

    #[test]
    fn creating_with_an_existing_name_adds_the_other_palette_to_that_theme() {
        let _guard = library();
        let light = install("Dual", "light", "#f8fbff", true);
        let mut draft = draft("dual", ThemeAppearance::Light);
        // The light side is taken, so the draft flips to the free side.
        assert_eq!(draft.active_appearance, ThemeAppearance::Dark);
        let target = draft.merge_target(None).expect("merge target");
        assert_eq!(target.id, light.id);
        assert_eq!(draft.submit_label(None, Some(&target)), "Add dark palette");

        draft.update_color("canvas", "#0b0f1a");
        let mut context = None;
        assert!(draft.submit(None, &mut |_, ctx| {
            context = Some(ctx);
            true
        }));
        assert_eq!(
            context,
            Some(ThemeSavedContext {
                created: false,
                merged_appearance: Some(ThemeAppearance::Dark)
            })
        );
        let themes = get_custom_themes();
        assert_eq!(themes.len(), 1);
        assert_eq!(
            modes(&themes[0]),
            [ThemeAppearance::Light, ThemeAppearance::Dark]
        );
        // The guided flag survives only because the whole theme is guided.
        assert!(themes[0].managed);
    }

    #[test]
    fn both_sides_taken_blocks_the_create_with_an_explanation() {
        let _guard = library();
        let mut both = install("Both", "light", "#f8fbff", true);
        both = update_custom_theme(
            parse_theme_file(json!({
                "version": 1, "id": both.id.to_string(), "name": "Both", "appearance": "light",
                "colors": theme_colors_to_camel_map(&both.colors),
                "variants": { "dark": theme_colors_to_camel_map(&get_standard_theme_colors(ThemeAppearance::Dark)) },
            }))
            .expect("both"),
        )
        .expect("update");
        assert_eq!(modes(&both).len(), 2);
        let mut draft = draft("Both", ThemeAppearance::Light);
        let target = draft.merge_target(None);
        assert_eq!(
            draft.appearance_lock_reason(ThemeAppearance::Dark, None, target.as_ref()),
            Some("\u{201c}Both\u{201d} already has a dark palette.".to_string())
        );
        assert!(!draft.set_active_appearance(ThemeAppearance::Dark, None));
        assert!(!draft.submit(None, &mut accept));
        assert_eq!(
            draft.error.as_deref(),
            Some("\u{201c}Both\u{201d} already has light and dark palettes. Pick another name.")
        );
    }

    #[test]
    fn editing_saves_in_place_and_keeps_the_collection() {
        let _guard = library();
        let theme = install("Paper", "light", "#f8fbff", false);
        let mut draft = ThemeEditorDraft::seeded(Some(&theme), None, None, ThemeAppearance::Light);
        draft.set_name("Paper Two", Some(&theme));
        draft.update_color("canvas", "#fafafa");
        let mut context = None;
        assert!(draft.submit(Some(&theme), &mut |saved, ctx| {
            context = Some((saved.id.to_string(), saved.label.to_string(), ctx));
            true
        }));
        let (id, label, ctx) = context.expect("saved");
        assert_eq!((id.as_str(), label.as_str()), ("paper", "Paper Two"));
        assert_eq!(ctx, ThemeSavedContext::default());
        assert_eq!(get_custom_themes().len(), 1);
        // Advanced (unmanaged source): no managed flag is added.
        assert!(!get_custom_themes()[0].managed);
        assert_eq!(draft.submit_label(Some(&theme), None), "Save changes");
    }

    #[test]
    fn a_single_palette_theme_locks_the_missing_appearance_with_an_explanation() {
        let _guard = library();
        let theme = install("Paper", "light", "#f8fbff", false);
        let draft = ThemeEditorDraft::seeded(Some(&theme), None, None, ThemeAppearance::Light);
        assert_eq!(
            draft.appearance_lock_reason(ThemeAppearance::Dark, Some(&theme), None),
            Some(
                "\u{201c}Paper\u{201d} has no dark palette. Create a theme with the same name to add one."
                    .to_string()
            )
        );
        assert_eq!(
            draft.appearance_lock_reason(ThemeAppearance::Light, Some(&theme), None),
            None
        );
    }

    #[test]
    fn renaming_onto_another_theme_folds_the_edited_palette_in_and_retires_the_old_entry() {
        let _guard = library();
        let light = install("Dual", "light", "#f8fbff", true);
        let dark = install("Dusk", "dark", "#0b0f1a", true);
        let mut draft = ThemeEditorDraft::seeded(Some(&dark), None, None, ThemeAppearance::Dark);
        draft.set_name("Dual", Some(&dark));
        let target = draft.merge_target(Some(&dark)).expect("target");
        assert_eq!(target.id, light.id);
        assert_eq!(
            draft.submit_label(Some(&dark), Some(&target)),
            "Merge into \u{201c}Dual\u{201d}"
        );
        let mut context = None;
        assert!(draft.submit(Some(&dark), &mut |saved, ctx| {
            context = Some((saved.id.to_string(), ctx));
            true
        }));
        let (id, ctx) = context.expect("saved");
        assert_eq!(id, "dual");
        assert_eq!(ctx.merged_appearance, Some(ThemeAppearance::Dark));
        assert!(!ctx.created);
        let themes = get_custom_themes();
        assert_eq!(themes.len(), 1);
        assert_eq!(
            modes(&themes[0]),
            [ThemeAppearance::Light, ThemeAppearance::Dark]
        );
    }

    #[test]
    fn renaming_onto_a_theme_that_already_has_that_palette_is_refused() {
        let _guard = library();
        install("Dual", "light", "#f8fbff", true);
        let other = install("Other", "light", "#ffffff", true);
        let mut draft = ThemeEditorDraft::seeded(Some(&other), None, None, ThemeAppearance::Light);
        draft.set_name("Dual", Some(&other));
        assert!(!draft.submit(Some(&other), &mut accept));
        assert_eq!(
            draft.error.as_deref(),
            Some("\u{201c}Dual\u{201d} already has a light palette. Pick another name.")
        );
        assert_eq!(get_custom_themes().len(), 2);
    }

    #[test]
    fn a_failed_merge_activation_restores_the_pre_merge_library() {
        let _guard = library();
        let light = install("Dual", "light", "#f8fbff", true);
        let dark = install("Dusk", "dark", "#0b0f1a", true);
        let mut draft = ThemeEditorDraft::seeded(Some(&dark), None, None, ThemeAppearance::Dark);
        draft.set_name("Dual", Some(&dark));
        assert!(!draft.submit(Some(&dark), &mut |_, _| false));
        assert_eq!(
            draft.error.as_deref(),
            Some("Theme saved, but it could not be made active. Try again.")
        );
        let themes = get_custom_themes();
        let mut ids: Vec<String> = themes.iter().map(|t| t.id.to_string()).collect();
        ids.sort();
        assert_eq!(ids, ["dual", "dusk"]);
        let restored = themes.iter().find(|t| t.id == light.id).expect("dual");
        assert_eq!(modes(restored), [ThemeAppearance::Light]);
    }
}
