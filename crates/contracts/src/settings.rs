//! Port of upstream/packages/contracts/src/settings.ts
//!
//! Persisted settings JSON keeps the upstream shape so a real `settings.json`
//! round-trips byte for byte: every `withDecodingDefault` field still encodes
//! (defaults are written out on save, exactly like Effect's encoder), and only
//! the combinators that omit keys on encode (`optionalKey`,
//! `ForwardCompatibleOptional`, `OmittedWhenNull`) skip serialization.
//!
//! Fields and helpers tied to cut modules are dropped (device hosts, auth
//! scope helpers, preview browser defaults, browser profiles, the Antigravity
//! provider settings); see the Deviations table in PORTING.md.
//!
//! Optionality follows PORTING.md: `optionalKey(X)` is `Option<X>` with the
//! file-local `optional_key` codec (explicit `null` is rejected, as upstream
//! does), `optionalKey(NullOr(X))` is `double_option`, and
//! `withDecodingDefault` is a non-`Option` field with `default = "..."` and no
//! `skip_serializing_if`. `Schema.Number` fields are written without a `.0`
//! like `JSON.stringify` does (`js_number`).

use crate::base_schemas::{
    ContractError, ProjectId, ThreadId, TrimmedNonEmptyString, TrimmedString, js_number,
    js_number_nullable, serialize_js_number,
};
use crate::environment::{EnvironmentMachineKind, ThreadEnvMode, WorktreeSubmodules};
use crate::keybindings::KeybindingShortcut;
use crate::model::{
    CustomModelSetting, DEFAULT_TEXT_GENERATION_MODEL, DEFAULT_TEXT_GENERATION_REASONING_EFFORT,
    ProviderOptionSelection, ProviderOptionSelectionValue,
};
use crate::model_selection::ModelSelection;
use crate::project::ProjectScript;
use crate::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
};
use crate::provider_policy::{DEFAULT_RUNTIME_MODE, RuntimeMode};
use crate::pull_request::PullRequestMergeMethod;
use crate::usage_limit_source_id::UsageLimitSourceId;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::time::Duration;

// ── Local serde helpers ────────────────────────────────────────

/// `Schema.optionalKey(X)` / `Schema.optional(X)` for a non-nullable `X`: the
/// key may be absent, but an explicit `null` is an invalid value, not "absent"
/// (plain serde `Option<T>` silently turns `null` into `None`, so a patch like
/// `{ "wordWrap": null }` would decode instead of being rejected as upstream
/// does). Use as `#[serde(default, skip_serializing_if = "Option::is_none",
/// with = "optional_key")]`. Nullable keys use `double_option` instead.
mod optional_key {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        T::deserialize(d).map(Some)
    }
}

// ── Client Settings (local-only) ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimestampFormat {
    #[serde(rename = "locale")]
    Locale,
    #[serde(rename = "12-hour")]
    TwelveHour,
    #[serde(rename = "24-hour")]
    TwentyFourHour,
}
const DEFAULT_TIMESTAMP_FORMAT: TimestampFormat = TimestampFormat::Locale;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLayout {
    Stacked,
    Split,
}
const DEFAULT_DIFF_LAYOUT: DiffLayout = DiffLayout::Stacked;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidebarProjectSortOrder {
    #[serde(rename = "updated_at")]
    UpdatedAt,
    #[serde(rename = "created_at")]
    CreatedAt,
    #[serde(rename = "manual")]
    Manual,
}
pub const DEFAULT_SIDEBAR_PROJECT_SORT_ORDER: SidebarProjectSortOrder =
    SidebarProjectSortOrder::UpdatedAt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidebarThreadSortOrder {
    #[serde(rename = "updated_at")]
    UpdatedAt,
    #[serde(rename = "created_at")]
    CreatedAt,
}
// Not exported: mobile was the last consumer of the value itself; the
// wire field keeps its decoding default below.
const DEFAULT_SIDEBAR_THREAD_SORT_ORDER: SidebarThreadSortOrder = SidebarThreadSortOrder::UpdatedAt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidebarProjectGroupingMode {
    #[serde(rename = "repository")]
    Repository,
    #[serde(rename = "repository_path")]
    RepositoryPath,
    #[serde(rename = "separate")]
    Separate,
}
const DEFAULT_SIDEBAR_PROJECT_GROUPING_MODE: SidebarProjectGroupingMode =
    SidebarProjectGroupingMode::Repository;

pub const MIN_SIDEBAR_THREAD_PREVIEW_COUNT: i64 = 1;
pub const MAX_SIDEBAR_THREAD_PREVIEW_COUNT: i64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SidebarThreadPreviewCount(i64);

impl SidebarThreadPreviewCount {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_SIDEBAR_THREAD_PREVIEW_COUNT..=MAX_SIDEBAR_THREAD_PREVIEW_COUNT).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 1 and 15",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for SidebarThreadPreviewCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for SidebarThreadPreviewCount {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for SidebarThreadPreviewCount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

const DEFAULT_SIDEBAR_THREAD_PREVIEW_COUNT: i64 = 6;

pub const MIN_SIDEBAR_AUTO_SETTLE_AFTER_DAYS: f64 = 1.0;
pub const MAX_SIDEBAR_AUTO_SETTLE_AFTER_DAYS: f64 = 90.0;

/// `SidebarAutoSettleAfterDays`: upstream `Schema.Number` (so fractional days
/// decode), constrained to `MIN..=MAX` like `Schema.isBetween`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SidebarAutoSettleAfterDays(f64);

impl SidebarAutoSettleAfterDays {
    pub fn new(value: f64) -> Result<Self, ContractError> {
        if (MIN_SIDEBAR_AUTO_SETTLE_AFTER_DAYS..=MAX_SIDEBAR_AUTO_SETTLE_AFTER_DAYS)
            .contains(&value)
        {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "a number between 1 and 90",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for SidebarAutoSettleAfterDays {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for SidebarAutoSettleAfterDays {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_js_number(self.0, s)
    }
}

impl<'de> Deserialize<'de> for SidebarAutoSettleAfterDays {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(f64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

const DEFAULT_SIDEBAR_AUTO_SETTLE_AFTER_DAYS: SidebarAutoSettleAfterDays =
    SidebarAutoSettleAfterDays(3.0);

pub const MIN_GLASS_OPACITY: i64 = 40;
pub const MAX_GLASS_OPACITY: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlassOpacity(i64);

impl GlassOpacity {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_GLASS_OPACITY..=MAX_GLASS_OPACITY).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 40 and 100",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for GlassOpacity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for GlassOpacity {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for GlassOpacity {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

const DEFAULT_GLASS_OPACITY: GlassOpacity = GlassOpacity(80);

pub const MIN_APPEARANCE_CONTRAST: i64 = 50;
pub const MAX_APPEARANCE_CONTRAST: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppearanceContrast(i64);

impl AppearanceContrast {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_APPEARANCE_CONTRAST..=MAX_APPEARANCE_CONTRAST).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 50 and 200",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for AppearanceContrast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for AppearanceContrast {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for AppearanceContrast {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

const DEFAULT_APPEARANCE_CONTRAST: AppearanceContrast = AppearanceContrast(100);

pub const MIN_PANEL_ANIMATION_DURATION_MS: i64 = 0;
pub const MAX_PANEL_ANIMATION_DURATION_MS: i64 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelAnimationDurationMs(i64);

impl PanelAnimationDurationMs {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_PANEL_ANIMATION_DURATION_MS..=MAX_PANEL_ANIMATION_DURATION_MS).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 0 and 400",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for PanelAnimationDurationMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for PanelAnimationDurationMs {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for PanelAnimationDurationMs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

const DEFAULT_PANEL_ANIMATION_DURATION_MS: PanelAnimationDurationMs = PanelAnimationDurationMs(0);

/**
 * Font size preferences, in CSS pixels. The ranges are deliberately narrow:
 * the interface size scales every rem-based dimension in the app, so the
 * bounds keep layouts intact rather than offering unusable extremes.
 */
pub const MIN_INTERFACE_FONT_SIZE: i64 = 12;
pub const MAX_INTERFACE_FONT_SIZE: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceFontSize(i64);

impl InterfaceFontSize {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_INTERFACE_FONT_SIZE..=MAX_INTERFACE_FONT_SIZE).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 12 and 20",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for InterfaceFontSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for InterfaceFontSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for InterfaceFontSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

pub const DEFAULT_INTERFACE_FONT_SIZE: InterfaceFontSize = InterfaceFontSize(16);

pub const MIN_PROMPT_FONT_SIZE: i64 = 12;
pub const MAX_PROMPT_FONT_SIZE: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PromptFontSize(i64);

impl PromptFontSize {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_PROMPT_FONT_SIZE..=MAX_PROMPT_FONT_SIZE).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 12 and 20",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for PromptFontSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for PromptFontSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for PromptFontSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

pub const DEFAULT_PROMPT_FONT_SIZE: PromptFontSize = PromptFontSize(14);

pub const MIN_CODE_FONT_SIZE: i64 = 10;
pub const MAX_CODE_FONT_SIZE: i64 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodeFontSize(i64);

impl CodeFontSize {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_CODE_FONT_SIZE..=MAX_CODE_FONT_SIZE).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 10 and 18",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for CodeFontSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for CodeFontSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for CodeFontSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

pub const DEFAULT_CODE_FONT_SIZE: CodeFontSize = CodeFontSize(13);

pub const MIN_TERMINAL_FONT_SIZE: i64 = 8;
pub const MAX_TERMINAL_FONT_SIZE: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalFontSize(i64);

impl TerminalFontSize {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (MIN_TERMINAL_FONT_SIZE..=MAX_TERMINAL_FONT_SIZE).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 8 and 20",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for TerminalFontSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for TerminalFontSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for TerminalFontSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

const DEFAULT_TERMINAL_FONT_SIZE: TerminalFontSize = TerminalFontSize(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnvironmentIdentificationMode {
    #[serde(rename = "artwork")]
    Artwork,
    #[serde(rename = "pill")]
    Pill,
    #[serde(rename = "none")]
    None,
}
pub const DEFAULT_ENVIRONMENT_IDENTIFICATION_MODE: EnvironmentIdentificationMode =
    EnvironmentIdentificationMode::Artwork;

impl EnvironmentIdentificationMode {
    /// Wire name of the mode, matching upstream's literal set.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Artwork => "artwork",
            Self::Pill => "pill",
            Self::None => "none",
        }
    }
}

/// `SnapShotKeyChord`: a shortcut with at least one modifier held, so a plain
/// key press never triggers a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapShotKeyChord(pub KeybindingShortcut);

impl SnapShotKeyChord {
    pub fn new(shortcut: KeybindingShortcut) -> Result<Self, ContractError> {
        let s = &shortcut;
        if s.meta_key || s.ctrl_key || s.shift_key || s.alt_key || s.mod_key {
            Ok(Self(shortcut))
        } else {
            Err(ContractError {
                expected: "a shortcut with at least one modifier (Snapshot shortcut requires a modifier.)",
                got: "a chord with no modifiers".to_owned(),
            })
        }
    }
}

impl fmt::Display for SnapShotKeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.key.fmt(f)
    }
}

impl Serialize for SnapShotKeyChord {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for SnapShotKeyChord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(KeybindingShortcut::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

pub const SNAP_SHOT_MODIFIERS: &[&str] = &["shift", "meta", "control", "alt"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapShotModifier {
    Shift,
    Meta,
    Control,
    Alt,
}

string_literal!(
    /// Constant `"both-shift-keys"` tag of the both-shift-keys capture shortcut.
    SnapShotBothShiftKeysKind,
    "both-shift-keys"
);
string_literal!(
    /// Constant `"modifier-pair"` tag of the modifier-pair capture shortcut.
    SnapShotModifierPairKind,
    "modifier-pair"
);

/// `SnapShotShortcut`: both shift keys, a configurable modifier pair, or an
/// explicit key chord with at least one modifier. Untagged like upstream's
/// `Schema.Union`: the two `kind`-tagged struct members come first, the chord
/// (which has no `kind`) last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SnapShotShortcut {
    BothShiftKeys {
        kind: SnapShotBothShiftKeysKind,
    },
    ModifierPair {
        kind: SnapShotModifierPairKind,
        modifier: SnapShotModifier,
    },
    Chord(SnapShotKeyChord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SnapShotSound {
    #[serde(rename = "soft-pop")]
    SoftPop,
    #[serde(rename = "camera-shutter")]
    CameraShutter,
}
const DEFAULT_SNAP_SHOT_SOUND: SnapShotSound = SnapShotSound::SoftPop;

/// `isModifierPairShortcut`: whether the shortcut is one of the two
/// `kind`-tagged pair shapes rather than an explicit chord.
pub fn is_modifier_pair_shortcut(shortcut: &SnapShotShortcut) -> bool {
    matches!(
        shortcut,
        SnapShotShortcut::BothShiftKeys { .. } | SnapShotShortcut::ModifierPair { .. }
    )
}

/// `snapShotShortcutModifierPair`: the pair a pair-shaped shortcut stands for.
/// The chord member never reaches this (see [`is_modifier_pair_shortcut`]).
pub fn snap_shot_shortcut_modifier_pair(shortcut: &SnapShotShortcut) -> SnapShotModifier {
    match shortcut {
        SnapShotShortcut::BothShiftKeys { .. } => SnapShotModifier::Shift,
        SnapShotShortcut::ModifierPair { modifier, .. } => *modifier,
        SnapShotShortcut::Chord(_) => {
            unreachable!("chord shortcuts are not modifier pairs")
        }
    }
}

/// `APPLE_MODIFIER_LABELS` / `OTHER_MODIFIER_LABELS`, folded into one lookup:
/// the label a platform shows for each modifier ("Command" on Apple keyboards,
/// "Super" elsewhere).
fn snap_shot_modifier_label(modifier: SnapShotModifier, apple: bool) -> &'static str {
    match (modifier, apple) {
        (SnapShotModifier::Shift, _) => "Shift",
        (SnapShotModifier::Meta, true) => "Command",
        (SnapShotModifier::Meta, false) => "Super",
        (SnapShotModifier::Control, true) => "Control",
        (SnapShotModifier::Control, false) => "Ctrl",
        (SnapShotModifier::Alt, true) => "Option",
        (SnapShotModifier::Alt, false) => "Alt",
    }
}

/// `snapShotModifierPairLabel`: "Shift + Shift"-style label for a pair.
pub fn snap_shot_modifier_pair_label(modifier: SnapShotModifier, apple: bool) -> String {
    let label = snap_shot_modifier_label(modifier, apple);
    format!("{label} + {label}")
}

const DEFAULT_SNAP_SHOT_SHORTCUT: SnapShotShortcut = SnapShotShortcut::BothShiftKeys {
    kind: SnapShotBothShiftKeysKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NotificationMode {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "notifications")]
    Notifications,
    #[serde(rename = "sound")]
    Sound,
    #[serde(rename = "notifications-and-sound")]
    NotificationsAndSound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuitConfirmationMode {
    #[serde(rename = "direct")]
    Direct,
    #[serde(rename = "hold")]
    Hold,
    #[serde(rename = "double-click")]
    DoubleClick,
}
const DEFAULT_QUIT_CONFIRMATION_MODE: QuitConfirmationMode = QuitConfirmationMode::Hold;

impl QuitConfirmationMode {
    /// Wire name of the mode, matching upstream's literal set.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Hold => "hold",
            Self::DoubleClick => "double-click",
        }
    }
}

/// Serde codec for `QuitConfirmationModeSetting` (`Schema.Union([
/// QuitConfirmationMode, LegacyConfirmQuit])`): the canonical mode string, or
/// the legacy boolean form older settings files carry. Booleans decode to their
/// equivalent mode and encode back as the canonical string value.
mod quit_confirmation_mode_setting {
    use super::QuitConfirmationMode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &QuitConfirmationMode, s: S) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<QuitConfirmationMode, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Mode(QuitConfirmationMode),
            // `LegacyConfirmQuit`: true held to quit, false quit directly.
            Legacy(bool),
        }
        match Wire::deserialize(d)? {
            Wire::Mode(mode) => Ok(mode),
            // Legacy booleans decode to their equivalent mode; the union
            // encoder always emits the canonical string.
            Wire::Legacy(true) => Ok(QuitConfirmationMode::Hold),
            Wire::Legacy(false) => Ok(QuitConfirmationMode::Direct),
        }
    }
}

/**
 * A user-chosen font family (a single name or a comma-separated list). Empty
 * means "use the app default"; clients compose their own fallback stacks.
 */
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontFamilyPreference(String);

impl FontFamilyPreference {
    const MAX_LENGTH: usize = 200;
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        if value.as_ref().encode_utf16().count() > Self::MAX_LENGTH {
            return Err(ContractError {
                expected: "a font family of at most 200 UTF-16 code units",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(value.as_ref().to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FontFamilyPreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for FontFamilyPreference {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FontFamilyPreference {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/**
 * The environment's theme, set with `t3 theme set <id>`. Each client applies
 * it once per value — live when connected, on its next connect otherwise — so
 * setting it switches every client, while a theme a user picks in Settings
 * afterwards sticks until the next set. Empty means "no environment theme",
 * which is also how it is cleared.
 *
 * Deliberately absent from ServerSettingsPatch: `t3 theme set` checks that an
 * id is syntactically valid and actually resolvable, and a generic RPC patch
 * would let a client write a theme no client can resolve, bypassing both.
 */
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefaultThemePreference(String);

impl DefaultThemePreference {
    const MAX_LENGTH: usize = 64;
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        if value.as_ref().encode_utf16().count() > Self::MAX_LENGTH {
            return Err(ContractError {
                expected: "a string of at most 64 UTF-16 code units",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(value.as_ref().to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DefaultThemePreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for DefaultThemePreference {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DefaultThemePreference {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/**
 * Defaults for the in-app preview browser, applied whenever a tab is opened
 * without an explicit viewport/zoom/appearance — by the user opening a browser
 * tab, or by an agent calling `preview_open` with no size. Recording quality is
 * client-local for the same reason: the Chromium guest being captured belongs
 * to the desktop app.
 */
pub const DEFAULT_BROWSER_AUTO_SHOW_FLOATING_PREVIEW: bool = true;
pub const BROWSER_RECORDING_FRAME_RATES: &[i64] = &[30, 60];

/// `Schema.Literals([30, 60])`: numeric literals, so they travel as JSON
/// numbers (`30`), not strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserRecordingFrameRate {
    Thirty,
    Sixty,
}

impl BrowserRecordingFrameRate {
    pub fn get(self) -> i64 {
        match self {
            Self::Thirty => 30,
            Self::Sixty => 60,
        }
    }
}

impl Serialize for BrowserRecordingFrameRate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.get())
    }
}

impl<'de> Deserialize<'de> for BrowserRecordingFrameRate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match i64::deserialize(d)? {
            30 => Ok(Self::Thirty),
            60 => Ok(Self::Sixty),
            other => Err(serde::de::Error::custom(format!(
                "expected 30 or 60, got {other}"
            ))),
        }
    }
}
pub const DEFAULT_BROWSER_RECORDING_FRAME_RATE: BrowserRecordingFrameRate =
    BrowserRecordingFrameRate::Thirty;

/**
 * Where a clicked link goes: the OS default browser, or a tab in the in-app
 * browser beside the thread. "system" is the default because that is what
 * every link did before the setting existed.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserLinkTarget {
    System,
    App,
}
pub const DEFAULT_BROWSER_LINK_TARGET: BrowserLinkTarget = BrowserLinkTarget::System;

/// One entry of `LoadBalancingWeights`: a model's share, 0..=100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoadBalancingWeight(i64);

impl LoadBalancingWeight {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (0..=100).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 0 and 100",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for LoadBalancingWeight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for LoadBalancingWeight {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for LoadBalancingWeight {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// `Schema.Record(TrimmedNonEmptyString, …)`: insertion order preserved.
pub type LoadBalancingWeights = IndexMap<TrimmedNonEmptyString, LoadBalancingWeight>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiffColorScheme {
    #[serde(rename = "red-green")]
    RedGreen,
    #[serde(rename = "blue-orange")]
    BlueOrange,
}

/// Maximum width of the chat timeline and composer on wide screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatWidth {
    Comfortable,
    Wide,
    Full,
}

/** Preferences shared by clients connected to the same environment. */
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFavorite {
    // The historical field name is retained; default instance ids also decode old provider-kind favorites.
    pub provider: ProviderInstanceId,
    pub model: TrimmedNonEmptyString,
}

pub type ModelFavorites = Vec<ModelFavorite>;

/// One provider's visibility and ordering preferences (`hiddenModels`,
/// `modelOrder`), shared by the client and server model pickers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelPreferencesEntry {
    #[serde(default)]
    pub hidden_models: Vec<String>,
    #[serde(default)]
    pub model_order: Vec<String>,
}

/// `Schema.Record(ProviderInstanceId, …)`: insertion order preserved.
pub type ProviderModelPreferences = IndexMap<ProviderInstanceId, ProviderModelPreferencesEntry>;

/// Upstream `ModelPreferences`: the two model preference fields shared by
/// client and server settings. In-memory type, never a wire shape on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreferences {
    pub favorites: ModelFavorites,
    pub provider_model_preferences: ProviderModelPreferences,
}

fn default_true() -> bool {
    true
}

fn default_notification_mode() -> NotificationMode {
    NotificationMode::Off
}

fn default_diff_color_scheme() -> DiffColorScheme {
    DiffColorScheme::RedGreen
}

fn default_chat_width() -> ChatWidth {
    ChatWidth::Comfortable
}

fn default_appearance_contrast() -> AppearanceContrast {
    DEFAULT_APPEARANCE_CONTRAST
}

fn default_panel_animation_duration_ms() -> PanelAnimationDurationMs {
    DEFAULT_PANEL_ANIMATION_DURATION_MS
}

fn default_browser_recording_frame_rate() -> BrowserRecordingFrameRate {
    DEFAULT_BROWSER_RECORDING_FRAME_RATE
}

fn default_browser_link_target() -> BrowserLinkTarget {
    DEFAULT_BROWSER_LINK_TARGET
}

fn default_confirm_quit() -> QuitConfirmationMode {
    DEFAULT_QUIT_CONFIRMATION_MODE
}

fn default_diff_layout() -> DiffLayout {
    DEFAULT_DIFF_LAYOUT
}

fn default_environment_identification_mode() -> EnvironmentIdentificationMode {
    DEFAULT_ENVIRONMENT_IDENTIFICATION_MODE
}

fn default_glass_opacity() -> GlassOpacity {
    DEFAULT_GLASS_OPACITY
}

fn default_font_size_interface() -> InterfaceFontSize {
    DEFAULT_INTERFACE_FONT_SIZE
}

fn default_font_size_prompt() -> PromptFontSize {
    DEFAULT_PROMPT_FONT_SIZE
}

fn default_font_size_code() -> CodeFontSize {
    DEFAULT_CODE_FONT_SIZE
}

fn default_font_size_terminal() -> TerminalFontSize {
    DEFAULT_TERMINAL_FONT_SIZE
}

fn default_send_shortcut() -> SendShortcut {
    SendShortcut::Enter
}

fn default_follow_up_behavior() -> FollowUpBehavior {
    FollowUpBehavior::Queue
}

fn default_sidebar_project_grouping_mode() -> SidebarProjectGroupingMode {
    DEFAULT_SIDEBAR_PROJECT_GROUPING_MODE
}

fn default_sidebar_project_sort_order() -> SidebarProjectSortOrder {
    DEFAULT_SIDEBAR_PROJECT_SORT_ORDER
}

fn default_sidebar_thread_sort_order() -> SidebarThreadSortOrder {
    DEFAULT_SIDEBAR_THREAD_SORT_ORDER
}

fn default_sidebar_thread_preview_count() -> SidebarThreadPreviewCount {
    SidebarThreadPreviewCount::new(DEFAULT_SIDEBAR_THREAD_PREVIEW_COUNT)
        .expect("default is within 1..=15")
}

fn default_timestamp_format() -> TimestampFormat {
    DEFAULT_TIMESTAMP_FORMAT
}

fn default_snap_shot_shortcut() -> SnapShotShortcut {
    DEFAULT_SNAP_SHOT_SHORTCUT
}

fn default_snap_shot_sound() -> SnapShotSound {
    DEFAULT_SNAP_SHOT_SOUND
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SendShortcut {
    #[serde(rename = "enter")]
    Enter,
    #[serde(rename = "mod-enter-multiline")]
    ModEnterMultiline,
    #[serde(rename = "mod-enter")]
    ModEnter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FollowUpBehavior {
    Queue,
    Steer,
}

impl SendShortcut {
    /// Wire name of the shortcut, matching upstream's literal set.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::ModEnterMultiline => "mod-enter-multiline",
            Self::ModEnter => "mod-enter",
        }
    }
}

impl FollowUpBehavior {
    /// Wire name of the behavior, matching upstream's literal set.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSettings {
    #[serde(default = "default_notification_mode")]
    pub notification_mode: NotificationMode,
    #[serde(default)]
    pub in_app_notifications_enabled: bool,
    #[serde(default = "default_diff_color_scheme")]
    pub diff_color_scheme: DiffColorScheme,
    #[serde(default = "default_chat_width")]
    pub chat_width: ChatWidth,
    #[serde(default)]
    pub load_balancing_enabled: bool,
    #[serde(default)]
    pub load_balancing_weights: LoadBalancingWeights,
    #[serde(default = "default_appearance_contrast")]
    pub appearance_contrast: AppearanceContrast,
    // Panel motion defaults to zero because width and height transitions cause
    // layout work on every frame, which is noticeable on lower-power clients.
    #[serde(default = "default_panel_animation_duration_ms")]
    pub panel_animation_duration_ms: PanelAnimationDurationMs,
    #[serde(default = "default_browser_recording_frame_rate")]
    pub browser_recording_frame_rate: BrowserRecordingFrameRate,
    #[serde(default)]
    pub browser_recording_show_key_presses: bool,
    #[serde(default)]
    pub browser_recording_show_mouse_presses: bool,
    /**
     * Where links clicked in a thread (chat markdown, terminal output) open.
     * Only the desktop app has an in-app browser, so other clients ignore "app".
     */
    #[serde(default = "default_browser_link_target")]
    pub browser_link_target: BrowserLinkTarget,
    /**
     * Whether an agent using a preview pops the floating mini player into
     * view. Only applies when the agent didn't ask either way — an explicit
     * `open`/`show` on `preview_open` still wins, since that is the agent
     * deliberately showing or hiding its work.
     */
    #[serde(default = "default_true")]
    pub browser_auto_show_floating_preview: bool,
    // Desktop-only. Boolean values from older settings files decode to their
    // equivalent mode and encode back as the canonical string value.
    #[serde(
        default = "default_confirm_quit",
        with = "quit_confirmation_mode_setting"
    )]
    pub confirm_quit: QuitConfirmationMode,
    #[serde(default)]
    pub confirm_thread_archive: bool,
    #[serde(default = "default_true")]
    pub confirm_thread_delete: bool,
    #[serde(default)]
    pub confirm_thread_unpin: bool,
    #[serde(default)]
    pub dismissed_provider_update_notification_keys: Vec<TrimmedNonEmptyString>,
    #[serde(default = "default_true")]
    pub diff_files_collapsed: bool,
    #[serde(default = "default_true")]
    pub diff_ignore_whitespace: bool,
    #[serde(default = "default_diff_layout")]
    pub diff_layout: DiffLayout,
    #[serde(default = "default_environment_identification_mode")]
    pub environment_identification_mode: EnvironmentIdentificationMode,
    #[serde(default = "default_glass_opacity")]
    pub glass_opacity: GlassOpacity,
    #[serde(default = "default_font_size_interface")]
    pub font_size_interface: InterfaceFontSize,
    #[serde(default = "default_font_size_prompt")]
    pub font_size_prompt: PromptFontSize,
    #[serde(default = "default_font_size_code")]
    pub font_size_code: CodeFontSize,
    #[serde(default = "default_font_size_terminal")]
    pub font_size_terminal: TerminalFontSize,
    #[serde(default)]
    pub font_family_code: FontFamilyPreference,
    #[serde(default)]
    pub font_family_composer: FontFamilyPreference,
    #[serde(default)]
    pub font_family_sans: FontFamilyPreference,
    #[serde(default)]
    pub font_family_terminal: FontFamilyPreference,
    // Grayscale `-webkit-font-smoothing: antialiased` (thinner strokes);
    // disabling restores the platform's heavier default. No effect off macOS.
    #[serde(default = "default_true")]
    pub font_smoothing: bool,
    // When the first-run welcome wizard finished (or was skipped), as an ISO
    // timestamp. `null` alone does not mean "show the wizard" — every install
    // that predates this field decodes to `null` — so the gate also requires an
    // empty workspace before it treats the client as a fresh install.
    #[serde(default)]
    pub onboarding_completed_at: Option<String>,
    #[serde(default)]
    pub persist_composer_context_strip: bool,
    // Legacy client values retained for migration to the connected environment.
    #[serde(default)]
    pub favorites: ModelFavorites,
    #[serde(default)]
    pub provider_model_preferences: ProviderModelPreferences,
    #[serde(default)]
    pub pull_request_merge_method_overrides:
        IndexMap<TrimmedNonEmptyString, PullRequestMergeMethod>,
    // Legacy plan mode. The composer's Build/Plan toggle was removed from the
    // default UI; this beta flag restores it (plus the /plan and /default slash
    // commands) for users who still rely on the old workflow.
    #[serde(default)]
    pub plan_mode_enabled: bool,
    // Legacy context window meter. The composer hides it by default; users who
    // still want the old usage indicator can restore it from Settings.
    #[serde(default)]
    pub context_window_meter_enabled: bool,
    // Desktop resting composer: scrolling an existing thread's conversation
    // settles the composer into its single-line layout. Losing focus never does.
    #[serde(default = "default_true")]
    pub composer_collapse_on_scroll: bool,
    // Rich text is the default; users can opt out for literal Markdown editing.
    #[serde(default = "default_true")]
    pub composer_rich_text_enabled: bool,
    #[serde(default = "default_send_shortcut")]
    pub send_shortcut: SendShortcut,
    #[serde(default = "default_follow_up_behavior")]
    pub follow_up_behavior: FollowUpBehavior,
    #[serde(default)]
    pub proactive_panels_enabled: bool,
    #[serde(default = "default_true")]
    pub show_skills_in_slash_menu: bool,
    // Legacy sidebar (the original per-project tree). Deliberately a fresh key
    // (was `sidebarV2Enabled` + `sidebarV2ConfiguredByUser`): decoding drops the
    // old keys, so everyone, including prior beta opt-outs, resets to the new
    // default sidebar.
    #[serde(default)]
    pub legacy_sidebar_enabled: bool,
    // Beta: working and monitoring threads fold into a Working shelf and return
    // to the top of the inbox once they need the user. The inbox then orders by
    // time, so manual placement there is ignored (and kept) while it is on.
    #[serde(default)]
    pub sidebar_working_shelf_enabled: bool,
    #[serde(default = "default_sidebar_project_grouping_mode")]
    pub sidebar_project_grouping_mode: SidebarProjectGroupingMode,
    #[serde(default)]
    pub sidebar_project_grouping_overrides:
        IndexMap<TrimmedNonEmptyString, SidebarProjectGroupingMode>,
    #[serde(default = "default_sidebar_project_sort_order")]
    pub sidebar_project_sort_order: SidebarProjectSortOrder,
    #[serde(default = "default_sidebar_thread_sort_order")]
    pub sidebar_thread_sort_order: SidebarThreadSortOrder,
    #[serde(default = "default_sidebar_thread_preview_count")]
    pub sidebar_thread_preview_count: SidebarThreadPreviewCount,
    #[serde(default = "default_timestamp_format")]
    pub timestamp_format: TimestampFormat,
    #[serde(default)]
    pub snap_shot_enabled: bool,
    #[serde(default = "default_true")]
    pub snap_shot_include_accessibility: bool,
    #[serde(default = "default_snap_shot_shortcut")]
    pub snap_shot_shortcut: SnapShotShortcut,
    #[serde(default = "default_true")]
    pub snap_shot_play_sound: bool,
    #[serde(default = "default_snap_shot_sound")]
    pub snap_shot_sound: SnapShotSound,
    #[serde(default = "default_true")]
    pub snap_shot_flash: bool,
    #[serde(default = "default_true")]
    pub snap_shot_animations: bool,
    #[serde(default = "default_true")]
    pub word_wrap: bool,
}

// Per-field decoding defaults, mirroring `Schema.withDecodingDefault`. A bare
// `#[serde(default)]` appears wherever upstream's default equals
// `Default::default()` of the field type.
impl Default for ClientSettings {
    /// Port of `DEFAULT_CLIENT_SETTINGS = Schema.decodeSync(ClientSettingsSchema)({})`.
    fn default() -> Self {
        serde_json::from_str("{}").expect("every ClientSettings field has a decoding default")
    }
}

/// Port of `DEFAULT_CLIENT_SETTINGS`.
pub fn default_client_settings() -> ClientSettings {
    ClientSettings::default()
}

// ── Server Settings (server-authoritative) ────────────────────

/// `UsageModelTokenPrice`: a finite, non-negative USD rate
/// (`Schema.Number.check(isFinite, isGreaterThanOrEqualTo(0))`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageModelTokenPrice(f64);

impl UsageModelTokenPrice {
    pub fn new(value: f64) -> Result<Self, ContractError> {
        if value.is_finite() && value >= 0.0 {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "a finite, non-negative number",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for UsageModelTokenPrice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for UsageModelTokenPrice {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_js_number(self.0, s)
    }
}

impl<'de> Deserialize<'de> for UsageModelTokenPrice {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(f64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/** USD per million tokens. Omitted cache rates use the input rate. */
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelPriceOverride {
    pub input_cost_per_million_tokens: UsageModelTokenPrice,
    pub output_cost_per_million_tokens: UsageModelTokenPrice,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub cache_read_cost_per_million_tokens: Option<UsageModelTokenPrice>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub cache_write_cost_per_million_tokens: Option<UsageModelTokenPrice>,
}

/// `makeBinaryPathSetting(fallback)`: a binary path setting that decodes empty
/// input to the provider's default executable. Encoding is the plain encoding,
/// and the field's decoding default is the fallback itself. serde `with`
/// modules cannot take a parameter, so each provider settings struct wraps this
/// in a two-line module (see `codex_binary_path`) and pairs it with
/// `#[serde(default = "...")]` returning the same fallback.
pub fn decode_binary_path_setting<'de, D>(d: D, fallback: &str) -> Result<TrimmedString, D::Error>
where
    D: Deserializer<'de>,
{
    let value = TrimmedString::deserialize(d)?;
    if value.as_str().is_empty() {
        Ok(TrimmedString::new(fallback))
    } else {
        Ok(value)
    }
}

mod codex_binary_path {
    use super::*;

    pub fn serialize<S: Serializer>(value: &TrimmedString, s: S) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TrimmedString, D::Error> {
        decode_binary_path_setting(d, "codex")
    }
}

mod claude_binary_path {
    use super::*;

    pub fn serialize<S: Serializer>(value: &TrimmedString, s: S) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TrimmedString, D::Error> {
        decode_binary_path_setting(d, "claude")
    }
}

/// Control a settings form renders for a provider settings field
/// (upstream `ProviderSettingsFormControl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderSettingsFormControl {
    Text,
    Password,
    Textarea,
    Switch,
    Select,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettingsFormOption {
    pub value: String,
    pub label: String,
}

/// Per-field metadata the settings form renders (upstream attaches it with
/// `Schema.annotateKey`; Turin stores the shape only — the annotation values
/// live with the settings UI).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettingsFormAnnotation {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub control: Option<ProviderSettingsFormControl>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub placeholder: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub hidden: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub clear_when_empty: Option<ClearWhenEmpty>,
    /** Choices for a `select` control. The first entry is the default. */
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub options: Option<Vec<ProviderSettingsFormOption>>,
}

/// Whether an empty input clears the stored field or is persisted as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClearWhenEmpty {
    Omit,
    Persist,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettingsFormSchemaAnnotation {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub order: Option<Vec<String>>,
}

fn default_codex_binary_path() -> TrimmedString {
    TrimmedString::new("codex")
}

fn default_claude_binary_path() -> TrimmedString {
    TrimmedString::new("claude")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodexSetupMode {
    #[serde(rename = "managed")]
    Managed,
    #[serde(rename = "existing")]
    Existing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub setup_mode: Option<CodexSetupMode>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_codex_binary_path", with = "codex_binary_path")]
    pub binary_path: TrimmedString,
    #[serde(default)]
    pub home_path: TrimmedString,
    #[serde(default)]
    pub shadow_home_path: TrimmedString,
    #[serde(default)]
    pub launch_args: TrimmedString,
    #[serde(default)]
    pub custom_models: Vec<CustomModelSetting>,
}

// Empty, or an integer from 100,000 to 1,000,000. Shared by the full
// Claude settings schema and its patch so an out-of-range value fails at
// the update that introduced it. `is_valid` implements it without a regex
// engine; kept as the upstream source of truth for the rule.
#[allow(dead_code)]
const CLAUDE_AUTO_COMPACT_WINDOW_PATTERN: &str = "^(?:|[1-9]\\d{5}|1000000)$";

/// `ClaudeSettings.autoCompactWindow`: empty (Claude's default) or a token
/// threshold matched by [`CLAUDE_AUTO_COMPACT_WINDOW_PATTERN`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCompactWindow(TrimmedString);

impl AutoCompactWindow {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedString::new(value);
        if Self::is_valid(value.as_str()) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an empty string or an integer from 100000 to 1000000",
                got: value.into_inner(),
            })
        }
    }
    /// `^(?:|[1-9]\d{5}|1000000)$` without a regex engine.
    fn is_valid(value: &str) -> bool {
        value.is_empty()
            || value == "1000000"
            || (value.len() == 6
                && (b'1'..=b'9').contains(&value.as_bytes()[0])
                && value.as_bytes()[1..].iter().all(u8::is_ascii_digit))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for AutoCompactWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for AutoCompactWindow {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for AutoCompactWindow {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Default for AutoCompactWindow {
    /// Upstream decoding default: empty, i.e. Claude's own threshold.
    fn default() -> Self {
        Self(TrimmedString::new(""))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_claude_binary_path", with = "claude_binary_path")]
    pub binary_path: TrimmedString,
    #[serde(default)]
    pub home_path: TrimmedString,
    #[serde(default)]
    pub custom_models: Vec<CustomModelSetting>,
    #[serde(default)]
    pub launch_args: String,
    #[serde(default)]
    pub auto_compact_window: AutoCompactWindow,
}

/// `secretSettingsFields(CodexSettings)`: neither built-in provider schema has
/// a `secretSetting` field.
pub const CODEX_SECRET_SETTINGS_FIELDS: &[&str] = &[];
/// `secretSettingsFields(ClaudeSettings)`.
pub const CLAUDE_SECRET_SETTINGS_FIELDS: &[&str] = &[];
/// `secretSettingsFields(UsageLimitSourceConfig)`: stored in the secret store
/// and redacted before reaching a client.
pub const USAGE_LIMIT_SOURCE_SECRET_SETTINGS_FIELDS: &[&str] = &["managementKey"];

/// Order the settings form renders `CodexSettings` fields in
/// (`makeProviderSettingsSchema` options).
pub const CODEX_SETTINGS_FORM_ORDER: &[&str] =
    &["binaryPath", "homePath", "shadowHomePath", "launchArgs"];
/// Order the settings form renders `ClaudeSettings` fields in.
pub const CLAUDE_SETTINGS_FORM_ORDER: &[&str] =
    &["binaryPath", "homePath", "autoCompactWindow", "launchArgs"];

/// The `Schema.annotateKey` metadata upstream attaches to one provider
/// settings field: its title, description and `providerSettingsForm`
/// annotation. The settings UI renders its form from these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSettingsFormField {
    pub field: &'static str,
    pub title: Option<&'static str>,
    pub description: Option<&'static str>,
    pub annotation: ProviderSettingsFormAnnotation,
}

fn hidden_form_field(field: &'static str) -> ProviderSettingsFormField {
    ProviderSettingsFormField {
        field,
        title: None,
        description: None,
        annotation: ProviderSettingsFormAnnotation {
            hidden: Some(true),
            ..Default::default()
        },
    }
}

fn form_field(
    field: &'static str,
    title: &'static str,
    description: &'static str,
    placeholder: Option<&'static str>,
    clear_when_empty: Option<ClearWhenEmpty>,
) -> ProviderSettingsFormField {
    ProviderSettingsFormField {
        field,
        title: Some(title),
        description: Some(description),
        annotation: ProviderSettingsFormAnnotation {
            placeholder: placeholder.map(str::to_owned),
            clear_when_empty,
            ..Default::default()
        },
    }
}

/// Annotations of `CodexSettings`, in declaration order.
pub fn codex_settings_form_fields() -> Vec<ProviderSettingsFormField> {
    vec![
        hidden_form_field("setupMode"),
        hidden_form_field("enabled"),
        form_field(
            "binaryPath",
            "Binary path",
            "Path to the Codex binary used by this instance.",
            Some("codex"),
            Some(ClearWhenEmpty::Omit),
        ),
        form_field(
            "homePath",
            "CODEX_HOME path",
            "Custom Codex home and config directory.",
            Some("~/.codex"),
            Some(ClearWhenEmpty::Omit),
        ),
        form_field(
            "shadowHomePath",
            "Shadow home path",
            "Account-specific Codex home. Keeps auth.json separate while sharing state from CODEX_HOME.",
            Some("~/.codex-t3/personal"),
            Some(ClearWhenEmpty::Omit),
        ),
        // No `providerSettingsForm` annotation, only a title and description.
        form_field(
            "launchArgs",
            "Launch arguments",
            "Additional CLI arguments passed to codex app-server on session start.",
            None,
            None,
        ),
        hidden_form_field("customModels"),
    ]
}

/// Annotations of `ClaudeSettings`, in declaration order.
pub fn claude_settings_form_fields() -> Vec<ProviderSettingsFormField> {
    vec![
        hidden_form_field("enabled"),
        form_field(
            "binaryPath",
            "Binary path",
            "Path to the Claude binary used by this instance.",
            Some("claude"),
            Some(ClearWhenEmpty::Omit),
        ),
        form_field(
            "homePath",
            "CLAUDE_CONFIG_DIR path",
            "Custom Claude home and config directory. Keeps .claude.json and .claude separate.",
            Some("~/.claude"),
            Some(ClearWhenEmpty::Omit),
        ),
        hidden_form_field("customModels"),
        form_field(
            "launchArgs",
            "Launch arguments",
            "Additional CLI arguments passed on session start.",
            Some("e.g. --chrome"),
            Some(ClearWhenEmpty::Omit),
        ),
        form_field(
            "autoCompactWindow",
            "Auto-compact after",
            "Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default.",
            Some("e.g. 300000"),
            Some(ClearWhenEmpty::Omit),
        ),
    ]
}

string_literal!(
    /// Constant `"cliproxy"` kind of a usage limit source.
    CliproxyKind,
    "cliproxy"
);

/**
 * A read-only quota source outside this environment's provider CLIs. The
 * only kind today is a CLIProxyAPI hub, whose management API reports the
 * windows of every pooled account. The key travels in settings for now, like
 * provider environment secrets; it is redacted before reaching a client.
 */
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceConfig {
    pub kind: CliproxyKind,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub label: Option<TrimmedNonEmptyString>,
    pub url: TrimmedNonEmptyString,
    // `secretSetting`: stored separately on the host.
    #[serde(default)]
    pub management_key: TrimmedString,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/** A GitHub host name, lowercased on decode so `GitHub.com` and `github.com` are one entry. */
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitHubHost(TrimmedNonEmptyString);

impl GitHubHost {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = TrimmedNonEmptyString::new(value)?;
        // Lowercasing a trimmed, non-empty host leaves it trimmed and non-empty.
        Ok(Self(
            TrimmedNonEmptyString::new(trimmed.as_str().to_lowercase())
                .expect("lowercasing a trimmed non-empty host keeps it non-empty"),
        ))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for GitHubHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for GitHubHost {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for GitHubHost {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/**
 * One source control host's saved settings. Each host package declares the fields it reads in
 * its client definition's settings schema and decodes this blob with it; the environment stores
 * it whole, so a host this build does not ship keeps its settings. A field annotated as a secret
 * holds a string, or a string per server host, whose value lives in the server's secret store:
 * settings and clients only see a redaction marker for a saved one.
 */
pub type SourceControlHostSettings = IndexMap<String, serde_json::Value>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilitySettings {
    #[serde(default)]
    pub otlp_traces_url: TrimmedString,
    #[serde(default)]
    pub otlp_metrics_url: TrimmedString,
    #[serde(default)]
    pub otlp_logs_url: TrimmedString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceControlWritingStyleMode {
    #[serde(rename = "repo_conventions")]
    RepoConventions,
    #[serde(rename = "conventional_commits")]
    ConventionalCommits,
    #[serde(rename = "custom")]
    Custom,
}

fn default_source_control_writing_style_mode() -> SourceControlWritingStyleMode {
    SourceControlWritingStyleMode::RepoConventions
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlWritingStyleSettings {
    #[serde(default = "default_source_control_writing_style_mode")]
    pub mode: SourceControlWritingStyleMode,
    #[serde(default)]
    pub custom_instructions: TrimmedString,
    #[serde(default = "default_true")]
    pub follow_change_request_templates: bool,
}

impl Default for SourceControlWritingStyleSettings {
    fn default() -> Self {
        Self {
            mode: SourceControlWritingStyleMode::RepoConventions,
            custom_instructions: TrimmedString::default(),
            follow_change_request_templates: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BranchNamingMode {
    Static,
    Semantic,
    Custom,
}

impl BranchNamingMode {
    /// Wire name of the mode, matching upstream's literal set.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Semantic => "semantic",
            Self::Custom => "custom",
        }
    }
}

/// Upstream `BranchNamingOptions` (plain interface, not a schema).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchNamingOptions {
    pub mode: BranchNamingMode,
    pub prefix: String,
    pub instructions: String,
}

pub const DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL: Duration = Duration::from_secs(30);
pub const DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Wire shape of `Schema.DurationFromMillis`: a number of milliseconds.
pub type DurationFromMillis = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundActivityProfile {
    Balanced,
    Performance,
    #[serde(rename = "battery-saver")]
    BatterySaver,
}
pub const DEFAULT_BACKGROUND_ACTIVITY_PROFILE: BackgroundActivityProfile =
    BackgroundActivityProfile::Balanced;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackgroundActivityProfileSelection {
    #[serde(rename = "balanced")]
    Balanced,
    #[serde(rename = "performance")]
    Performance,
    #[serde(rename = "battery-saver")]
    BatterySaver,
    #[serde(rename = "custom")]
    Custom,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivityOverrides {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub automatic_git_fetch_interval: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_health_refresh_interval: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub host_power_monitor_active_interval: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub host_power_monitor_idle_interval: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub idle_client_ttl: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub pause_when_host_locked: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub pause_when_host_low_power: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub pause_when_client_low_power: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub pause_when_on_battery: Option<bool>,
}

int_literal!(
    /// Constant `1` schema version of the background activity settings.
    BackgroundActivitySchemaVersion,
    1
);

fn default_background_activity_schema_version() -> BackgroundActivitySchemaVersion {
    BackgroundActivitySchemaVersion
}

fn default_background_activity_profile_selection() -> BackgroundActivityProfileSelection {
    BackgroundActivityProfileSelection::Balanced
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivitySettings {
    #[serde(default = "default_background_activity_schema_version")]
    pub schema_version: BackgroundActivitySchemaVersion,
    #[serde(default = "default_background_activity_profile_selection")]
    pub profile: BackgroundActivityProfileSelection,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub base_profile: Option<BackgroundActivityProfile>,
    #[serde(default)]
    pub overrides: BackgroundActivityOverrides,
}

impl Default for BackgroundActivitySettings {
    fn default() -> Self {
        Self {
            schema_version: BackgroundActivitySchemaVersion,
            profile: BackgroundActivityProfileSelection::Balanced,
            base_profile: None,
            overrides: BackgroundActivityOverrides::default(),
        }
    }
}

/**
 * How assistant text reaches clients while a turn runs.
 * - `turn`: hold the whole message until the turn finishes or pauses.
 * - `paragraph`: deliver each finished paragraph or closed code block.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseStreamingMode {
    Turn,
    Paragraph,
}

/**
 * Server settings a project may override. Every other server setting is
 * environment-wide: providers, keybindings, observability, device hosts,
 * background activity, theme. UI, search and the write planner derive
 * eligibility from this list, so adding a key here is the whole opt-in.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageRetentionDays(i64);

impl StorageRetentionDays {
    pub fn new(value: i64) -> Result<Self, ContractError> {
        if (1..=3650).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError {
                expected: "an integer between 1 and 3650",
                got: value.to_string(),
            })
        }
    }
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for StorageRetentionDays {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for StorageRetentionDays {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(self.0)
    }
}

impl<'de> Deserialize<'de> for StorageRetentionDays {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorktreeKeepWhen {
    #[serde(rename = "any-local-files")]
    AnyLocalFiles,
    #[serde(rename = "uncommitted-changes")]
    UncommittedChanges,
    #[serde(rename = "tracked-changes")]
    TrackedChanges,
}

fn default_worktree_keep_when() -> WorktreeKeepWhen {
    WorktreeKeepWhen::UncommittedChanges
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCleanupRules {
    #[serde(default = "default_worktree_keep_when")]
    pub worktree_keep_when: WorktreeKeepWhen,
    pub worktree_after_days: Option<StorageRetentionDays>,
    pub worktree_on_merge: bool,
    pub worktree_on_delete: bool,
    pub worktree_unchanged: bool,
}

/// The non-null member of `WorktreeCleanup`: cleanup disabled, or custom rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum WorktreeCleanup {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "custom")]
    Custom { rules: WorktreeCleanupRules },
}

pub const PROJECT_SCOPED_SERVER_SETTING_KEYS: &[&str] = &[
    "worktreeCleanup",
    "defaultModelSelection",
    "defaultRuntimeMode",
    "defaultThreadEnvMode",
    "newWorktreesStartFromOrigin",
    "worktreeSubmodules",
    "defaultAutoPull",
    "defaultProjectScripts",
    "enableAgentBrowserAccess",
    "enableAgentDeviceAccess",
    "textGenerationModelSelection",
    "sourceControlWriterModelSelection",
    "reviewGuideModelSelection",
    "reviewGuideInstructions",
    "sourceControlWritingStyle",
    "removeAgentCreditsOnMerge",
    "branchNamingMode",
    "branchNamePrefix",
    "branchNameInstructions",
    "pullRequestMergeMethod",
    "sidebarAutoSettleOnMerge",
    "sidebarAutoSettleAfterDays",
    "sidebarAutoSettleKeepPullRequests",
    "continueThreadsAfterServerUpdate",
    "responseStreamingMode",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProviderInstanceOverride {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enabled: Option<bool>,
}

/**
 * One project's overrides. An absent key inherits the environment value;
 * `null` is a real value where the environment type is nullable (no default
 * model, no dedicated writer model, never auto-settle).
 *
 * `providerInstances` sits outside `PROJECT_SCOPED_SERVER_SETTING_KEYS`:
 * every other key replaces its `ServerSettings` field whole, while this one
 * keeps the settings.json shape but overrides field by field, per
 * `ProviderInstanceId`. Only `enabled` is overridable today. An instance id
 * or field absent here inherits the machine value, and a present `enabled`
 * wins in either direction, so a project can turn on an instance the machine
 * disabled, or turn off one the machine enabled. See
 * `resolve_project_provider_instance_enabled` for the combined resolution.
 */
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettingsOverrides {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub worktree_cleanup: Option<Option<WorktreeCleanup>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_runtime_mode: Option<RuntimeMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub new_worktrees_start_from_origin: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::forward_compatible_optional"
    )]
    pub worktree_submodules: Option<WorktreeSubmodules>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_auto_pull: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_project_scripts: Option<Vec<ProjectScript>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enable_agent_browser_access: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enable_agent_device_access: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub text_generation_model_selection: Option<ModelSelection>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub source_control_writer_model_selection: Option<Option<ModelSelection>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub review_guide_model_selection: Option<Option<ModelSelection>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub review_guide_instructions: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub source_control_writing_style: Option<SourceControlWritingStyleSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub remove_agent_credits_on_merge: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub branch_naming_mode: Option<BranchNamingMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub branch_name_prefix: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub branch_name_instructions: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub pull_request_merge_method: Option<Option<PullRequestMergeMethod>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_auto_settle_on_merge: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub sidebar_auto_settle_after_days: Option<Option<SidebarAutoSettleAfterDays>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_auto_settle_keep_pull_requests: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub continue_threads_after_server_update: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub response_streaming_mode: Option<ResponseStreamingMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_instances: Option<IndexMap<ProviderInstanceId, ProjectProviderInstanceOverride>>,
}

/// `Schema.Record(ProjectId, ProjectSettingsOverrides)`: insertion order preserved.
pub type ProjectSettingsOverridesMap = IndexMap<ProjectId, ProjectSettingsOverrides>;

/**
 * Whether `null` is a stored override value for this key rather than "unset".
 * Clients writing a project override treat null for every other key as a
 * request to remove the override, so a picker's "Inherit" item and the row's
 * reset do the same thing.
 */
pub fn is_nullable_project_settings_override(key: &str) -> bool {
    NULLABLE_PROJECT_SETTINGS_OVERRIDES.contains(&key)
}

const NULLABLE_PROJECT_SETTINGS_OVERRIDES: &[&str] = &[
    "defaultModelSelection",
    "sourceControlWriterModelSelection",
    "reviewGuideModelSelection",
    "pullRequestMergeMethod",
    "sidebarAutoSettleAfterDays",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageCleanupReportEntryKind {
    #[serde(rename = "worktree")]
    Worktree,
    #[serde(rename = "browser-artifacts")]
    BrowserArtifacts,
    #[serde(rename = "logs")]
    Logs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageCleanupReportEntryOutcome {
    Removed,
    Kept,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupReportEntry {
    pub kind: StorageCleanupReportEntryKind,
    pub outcome: StorageCleanupReportEntryOutcome,
    pub reason: String,
    pub path: Option<String>,
    pub thread_id: Option<ThreadId>,
    pub thread_title: Option<String>,
    #[serde(default, with = "js_number_nullable")]
    pub bytes: Option<f64>,
    #[serde(default, with = "js_number_nullable")]
    pub files: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageCleanupReportTrigger {
    Automatic,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupCounts {
    #[serde(with = "js_number")]
    pub removed: f64,
    #[serde(with = "js_number")]
    pub kept: f64,
    #[serde(with = "js_number")]
    pub failed: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupReport {
    pub trigger: StorageCleanupReportTrigger,
    pub started_at: String,
    pub finished_at: String,
    pub entries: Vec<StorageCleanupReportEntry>,
    pub counts: StorageCleanupCounts,
    #[serde(with = "js_number")]
    pub omitted_count: f64,
    #[serde(default, with = "js_number")]
    pub bytes_freed: f64,
}

fn default_storage_cleanup() -> StorageCleanupSettings {
    StorageCleanupSettings::default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupSettings {
    #[serde(default = "default_worktree_keep_when")]
    pub worktree_keep_when: WorktreeKeepWhen,
    #[serde(default)]
    pub worktree_after_days: Option<StorageRetentionDays>,
    #[serde(default)]
    pub worktree_on_merge: bool,
    #[serde(default)]
    pub worktree_on_delete: bool,
    #[serde(default)]
    pub worktree_unchanged: bool,
    #[serde(default)]
    pub browser_artifacts_after_days: Option<StorageRetentionDays>,
    #[serde(default)]
    pub logs_after_days: Option<StorageRetentionDays>,
}

impl Default for StorageCleanupSettings {
    fn default() -> Self {
        Self {
            worktree_keep_when: WorktreeKeepWhen::UncommittedChanges,
            worktree_after_days: None,
            worktree_on_merge: false,
            worktree_on_delete: false,
            worktree_unchanged: false,
            browser_artifacts_after_days: None,
            logs_after_days: None,
        }
    }
}

fn default_response_streaming_mode() -> ResponseStreamingMode {
    ResponseStreamingMode::Paragraph
}

fn default_runtime_mode() -> RuntimeMode {
    DEFAULT_RUNTIME_MODE
}

fn default_sidebar_auto_settle_after_days() -> Option<SidebarAutoSettleAfterDays> {
    Some(DEFAULT_SIDEBAR_AUTO_SETTLE_AFTER_DAYS)
}

fn default_automatic_git_fetch_interval() -> DurationFromMillis {
    DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL.as_millis() as u64
}

fn default_provider_health_refresh_interval() -> DurationFromMillis {
    DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL.as_millis() as u64
}

fn default_background_activity_profile() -> BackgroundActivityProfile {
    DEFAULT_BACKGROUND_ACTIVITY_PROFILE
}

fn default_environment_icon() -> Option<EnvironmentMachineKind> {
    None
}

fn default_worktree_submodules() -> Option<WorktreeSubmodules> {
    None
}

fn default_branch_name_prefix() -> TrimmedString {
    TrimmedString::new("t3")
}

fn default_branch_naming_mode() -> BranchNamingMode {
    BranchNamingMode::Static
}

fn default_text_generation_model_selection() -> ModelSelection {
    ModelSelection {
        instance_id: ProviderInstanceId::new("codex").expect("codex is a valid instance id"),
        model: TrimmedNonEmptyString::new(DEFAULT_TEXT_GENERATION_MODEL)
            .expect("default model id is non-empty"),
        options: Some(vec![ProviderOptionSelection {
            id: TrimmedNonEmptyString::new("reasoningEffort").expect("non-empty"),
            value: ProviderOptionSelectionValue::Str(
                TrimmedNonEmptyString::new(DEFAULT_TEXT_GENERATION_REASONING_EFFORT)
                    .expect("non-empty"),
            ),
        }]),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    // Null means no client has migrated this preference yet; an empty value is
    // authoritative so another device cannot resurrect cleared preferences.
    #[serde(default)]
    pub favorites: Option<ModelFavorites>,
    #[serde(default)]
    pub provider_model_preferences: Option<ProviderModelPreferences>,
    #[serde(default)]
    pub worktree_cleanup: Option<WorktreeCleanup>,
    #[serde(default = "default_storage_cleanup")]
    pub storage_cleanup: StorageCleanupSettings,
    /**
     * Absolute directory new worktrees are created under, e.g. `D:\worktrees`
     * or `~/worktrees`. Empty uses `<T3 home>/worktrees`.
     */
    #[serde(default)]
    pub worktrees_directory: TrimmedString,
    /**
     * Custom locations used before the current one. Server-maintained so
     * worktrees left there stay eligible for cleanup and review diffs.
     */
    #[serde(default)]
    pub previous_worktrees_directories: Vec<TrimmedString>,
    #[serde(default = "default_response_streaming_mode")]
    pub response_streaming_mode: ResponseStreamingMode,
    #[serde(default = "default_true")]
    pub enable_provider_update_checks: bool,
    // Retain the update-era key. On by default so restarts resume the work they
    // interrupted; recovery stops after three automatic continuations in a row.
    #[serde(default = "default_true")]
    pub continue_threads_after_server_update: bool,
    /**
     * Whether agents may drive the in-app preview browser. Turning this off
     * withholds the MCP credential, so the `t3-code` server (and with it every
     * `preview_*` tool) is never attached to a provider session, and the prompt
     * text describing those tools is dropped along with them. The user's own
     * browser panel is unaffected — this gates agent access only.
     *
     * Server-authoritative rather than client-local: tool injection and prompt
     * construction both happen on the server, and the answer must not differ
     * between a desktop window and a phone attached to the same server.
     */
    #[serde(default = "default_true")]
    pub enable_agent_browser_access: bool,
    #[serde(default)]
    pub project_agent_browser_access_overrides: IndexMap<ProjectId, bool>,
    #[serde(default)]
    pub default_auto_pull: bool,
    #[serde(default)]
    pub default_project_scripts: Vec<ProjectScript>,
    #[serde(default)]
    pub project_script_overrides: IndexMap<ProjectId, Option<Vec<ProjectScript>>>,
    #[serde(default)]
    pub project_auto_pull_overrides: IndexMap<ProjectId, bool>,
    #[serde(default)]
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default = "default_runtime_mode")]
    pub default_runtime_mode: RuntimeMode,
    /**
     * Per-project overrides of the keys in `PROJECT_SCOPED_SERVER_SETTING_KEYS`,
     * plus `providerInstances`. The source of truth for project
     * settings; `projectAgentBrowserAccessOverrides`, `projectAutoPullOverrides`
     * and `projectScriptOverrides` are derived views kept for one release so
     * older clients keep reading them.
     */
    #[serde(default)]
    pub project_settings_overrides: ProjectSettingsOverridesMap,
    /**
     * Whether the legacy per-project fields have been folded into
     * `projectSettingsOverrides`. The fold runs once so a later reset in the
     * settings UI is not undone by the next server start.
     */
    #[serde(default)]
    pub project_settings_folded: bool,
    /**
     * Whether agents may drive simulators and emulators. Gates the `device_*`
     * MCP tools and the preconfigured `agent-device` CLI the same way
     * `enableAgentBrowserAccess` gates the browser: server-authoritative, applied
     * when the provider session is prepared. The user's own Device panel is
     * unaffected.
     */
    #[serde(default)]
    pub enable_agent_device_access: bool,
    /**
     * Whether this server may install and run T3's device helper processes.
     * Kept separate from agent access so enabling the user's Device panel does
     * not also grant providers control of simulators and emulators.
     */
    #[serde(default)]
    pub enable_device_support: bool,
    /** Whether the server-local Device panel setup flow has been completed. */
    #[serde(default)]
    pub device_onboarding_completed: bool,
    #[serde(default = "default_sidebar_auto_settle_after_days")]
    pub sidebar_auto_settle_after_days: Option<SidebarAutoSettleAfterDays>,
    #[serde(default)]
    pub snooze_limited_threads: bool,
    #[serde(default)]
    pub auto_resume_limited_threads: bool,
    #[serde(default)]
    pub sidebar_auto_settle_keep_pull_requests: bool,
    #[serde(default = "default_true")]
    pub sidebar_auto_settle_on_merge: bool,
    #[serde(default)]
    pub background_activity: BackgroundActivitySettings,
    // Legacy flat fields retained for old settings files and old clients. New
    // consumers should resolve `backgroundActivity` instead.
    #[serde(default = "default_automatic_git_fetch_interval")]
    pub automatic_git_fetch_interval: DurationFromMillis,
    #[serde(default = "default_provider_health_refresh_interval")]
    pub provider_health_refresh_interval: DurationFromMillis,
    #[serde(default = "default_background_activity_profile")]
    pub background_activity_profile: BackgroundActivityProfile,
    #[serde(default)]
    pub default_theme: DefaultThemePreference,
    /**
     * When the environment's theme was last set, so clients can tell a re-set
     * of the same value from one they already applied: `t3 theme set` must act
     * even when it names the theme it named before. Empty on environments
     * provisioned by builds that predate it, where clients fall back to
     * applying once per value.
     */
    #[serde(default)]
    pub default_theme_set_at: DefaultThemePreference,
    /**
     * The icon clients draw for this environment. Null means "use what the
     * server detected" (`environment.platform.machine`), falling back to a
     * generic server. Lives on the server, not the client, so every device
     * sees the same machine. A kind picked on a newer server decodes as null
     * here rather than failing the whole settings snapshot for an older client.
     */
    #[serde(
        default = "default_environment_icon",
        with = "crate::base_schemas::forward_compatible_nullable"
    )]
    pub environment_icon: Option<EnvironmentMachineKind>,
    /**
     * Null means inherit: the repository's t3.json, then "local". The old
     * default "local" was never persisted (defaults are stripped on write), so
     * it now decodes as inherit, which resolves the same way because the old
     * chain also let t3.json outrank the environment. Null stays off the wire
     * so older clients, which require a literal here, keep decoding.
     */
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::omitted_when_null"
    )]
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    #[serde(default = "default_true")]
    pub new_worktrees_start_from_origin: bool,
    /**
     * Null defers to the repository's t3.json, then to recursive. A value
     * picked on a newer server decodes as null here rather than failing the
     * whole settings snapshot for an older client.
     */
    #[serde(
        default = "default_worktree_submodules",
        with = "crate::base_schemas::forward_compatible_nullable"
    )]
    pub worktree_submodules: Option<WorktreeSubmodules>,
    #[serde(default)]
    pub add_project_base_directory: TrimmedString,
    #[serde(default = "default_text_generation_model_selection")]
    pub text_generation_model_selection: ModelSelection,
    #[serde(default = "default_branch_naming_mode")]
    pub branch_naming_mode: BranchNamingMode,
    #[serde(default = "default_branch_name_prefix")]
    pub branch_name_prefix: TrimmedString,
    #[serde(default)]
    pub branch_name_instructions: TrimmedString,
    #[serde(default)]
    pub remove_agent_credits_on_merge: bool,
    #[serde(default)]
    pub source_control_writing_style: SourceControlWritingStyleSettings,
    #[serde(default)]
    pub source_control_writer_model_selection: Option<ModelSelection>,
    /**
     * Beta: review guides on this server. Off hides the Guide tab in every client and refuses to
     * write guides, for people and agents alike.
     */
    #[serde(default)]
    pub review_guides_enabled: bool,
    /**
     * The model that reads a pull request to write its review guide. It investigates a checkout
     * over many turns, so it can want a stronger model than one-shot writing. `null` uses the
     * source control writer model.
     */
    #[serde(default)]
    pub review_guide_model_selection: Option<ModelSelection>,
    /**
     * The maintainer's taste for review guides: what to dwell on, skip, or order first. Shapes the
     * guide's emphasis only; the output format and coverage rules stay fixed.
     */
    #[serde(default)]
    pub review_guide_instructions: TrimmedString,
    /**
     * The merge method pull requests start with; `null` reuses the method
     * last chosen on this device. Server-side so a project can override it
     * like any other project setting.
     */
    #[serde(default)]
    pub pull_request_merge_method: Option<PullRequestMergeMethod>,

    // New driver-agnostic instance map. Keyed by `ProviderInstanceId`; values
    // are `ProviderInstanceConfig` envelopes. The driver-specific config blob
    // is `Schema.Unknown` at this layer so envelopes with unknown drivers
    // (forks, downgrades, in-flight PR branches) round-trip without loss.
    // See providerInstance.ts for the forward/backward compatibility invariant.
    #[serde(default)]
    pub provider_instances: ProviderInstanceConfigMap,
    #[serde(default)]
    pub observability: ObservabilitySettings,
    /** Keyed by source control host kind, such as `gitcafe`; see `SourceControlHostSettings`. */
    #[serde(default)]
    pub source_control_hosts: IndexMap<String, SourceControlHostSettings>,
    // Keyed by a user-chosen id so a source keeps its rows across edits. Entries
    // this build cannot decode round-trip untouched, as provider instances do.
    #[serde(default)]
    pub usage_limit_sources: IndexMap<UsageLimitSourceId, UsageLimitSourceConfig>,
    /** Allows this server to read the Cursor CLI's macOS Keychain login for account usage. */
    #[serde(default)]
    pub cursor_keychain_usage_enabled: bool,
    /** Exact model IDs, applied to past and future usage on this environment. */
    #[serde(default)]
    pub usage_price_overrides: IndexMap<TrimmedNonEmptyString, UsageModelPriceOverride>,
    /**
     * Exact model ID to the model its usage counts as, such as a preview slug to
     * its released name. The mapped model is priced and reported as its target.
     */
    #[serde(default)]
    pub usage_model_aliases: IndexMap<TrimmedNonEmptyString, TrimmedNonEmptyString>,
}

impl Default for ServerSettings {
    /// Port of `DEFAULT_SERVER_SETTINGS = Schema.decodeSync(ServerSettings)({})`.
    fn default() -> Self {
        serde_json::from_str("{}").expect("every ServerSettings field has a decoding default")
    }
}

/// Port of `DEFAULT_SERVER_SETTINGS`.
pub fn default_server_settings() -> ServerSettings {
    ServerSettings::default()
}

/**
 * Read the legacy `enabled` flag embedded in a provider instance config
 * blob. The envelope-level `ProviderInstanceConfig.enabled` is the single
 * flag going forward; this reader exists for old settings files that still
 * carry the flag in-config.
 */
pub fn provider_instance_config_enabled_flag(config: Option<&serde_json::Value>) -> Option<bool> {
    let object = config?.as_object()?;
    match object.get("enabled") {
        Some(serde_json::Value::Bool(enabled)) => Some(*enabled),
        _ => None,
    }
}

/**
 * Built-in drivers that stay off until the user turns them on. Matches the
 * `enabled` decoding default of each driver's settings schema.
 */
const DEFAULT_DISABLED_PROVIDER_DRIVERS: &[&str] =
    &["cursor", "grok", "muse", "pi", "opencode", "antigravity"];

/** Built-in drivers whose default instance runs before the user configures it. */
const DEFAULT_ENABLED_DEFAULT_INSTANCES: &[&str] = &["codex", "claudeAgent"];

/**
 * Whether the built-in default instance at `instanceId` is enabled while
 * settings have no `providerInstances` entry for it. Only Codex and Claude
 * start on; any other id without an entry has no running instance.
 */
pub fn is_unconfigured_default_instance_enabled(instance_id: &ProviderInstanceId) -> bool {
    DEFAULT_ENABLED_DEFAULT_INSTANCES.contains(&instance_id.as_str())
}

/**
 * Default enabled state for a driver when neither the envelope nor the config
 * blob carries a flag. Unknown (fork) drivers default to enabled.
 */
fn default_enabled_for_driver(driver: &ProviderDriverKind) -> bool {
    !DEFAULT_DISABLED_PROVIDER_DRIVERS.contains(&driver.as_str())
}

/**
 * Resolve whether a configured provider instance is enabled. An explicit
 * false on either the envelope or the in-config flag wins (most
 * restrictive), so a user's disable is never silently undone by the other
 * flag. Otherwise: envelope, then config, then the driver's default.
 */
pub fn resolve_provider_instance_enabled(instance: &ProviderInstanceConfig) -> bool {
    let config_enabled = provider_instance_config_enabled_flag(instance.config.as_ref());
    if instance.enabled == Some(false) || config_enabled == Some(false) {
        return false;
    }
    instance
        .enabled
        .or(config_enabled)
        .unwrap_or_else(|| default_enabled_for_driver(&instance.driver))
}

/**
 * Effective enablement for a provider instance within a project: the
 * project's `providerInstances[id].enabled` override when set, otherwise the
 * machine-wide `resolveProviderInstanceEnabled` value. `projectId` of `null`
 * (no project in scope) always resolves to the machine-wide value.
 */
pub fn resolve_project_provider_instance_enabled(
    provider_instances: &ProviderInstanceConfigMap,
    project_settings_overrides: &ProjectSettingsOverridesMap,
    project_id: Option<&ProjectId>,
    instance_id: &ProviderInstanceId,
) -> bool {
    let override_enabled = project_id.and_then(|project_id| {
        project_settings_overrides
            .get(project_id)
            .and_then(|overrides| overrides.provider_instances.as_ref())
            .and_then(|instances| instances.get(instance_id))
            .and_then(|instance| instance.enabled)
    });
    if let Some(enabled) = override_enabled {
        return enabled;
    }
    match provider_instances.get(instance_id) {
        Some(instance_config) => resolve_provider_instance_enabled(instance_config),
        None => is_unconfigured_default_instance_enabled(instance_id),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServerSettingsOperation {
    #[serde(rename = "normalize")]
    Normalize,
    #[serde(rename = "check-exists")]
    CheckExists,
    #[serde(rename = "create-provider-instance")]
    CreateProviderInstance,
    #[serde(rename = "read-file")]
    ReadFile,
    #[serde(rename = "read-provider-history")]
    ReadProviderHistory,
    #[serde(rename = "read-project-settings")]
    ReadProjectSettings,
    #[serde(rename = "read-secret")]
    ReadSecret,
    #[serde(rename = "remove-secret")]
    RemoveSecret,
    #[serde(rename = "remove-stale-secret")]
    RemoveStaleSecret,
    #[serde(rename = "write-secret")]
    WriteSecret,
    #[serde(rename = "write-file")]
    WriteFile,
    #[serde(rename = "prepare-directory")]
    PrepareDirectory,
}

impl ServerSettingsOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::CheckExists => "check-exists",
            Self::CreateProviderInstance => "create-provider-instance",
            Self::ReadFile => "read-file",
            Self::ReadProviderHistory => "read-provider-history",
            Self::ReadProjectSettings => "read-project-settings",
            Self::ReadSecret => "read-secret",
            Self::RemoveSecret => "remove-secret",
            Self::RemoveStaleSecret => "remove-stale-secret",
            Self::WriteSecret => "write-secret",
            Self::WriteFile => "write-file",
            Self::PrepareDirectory => "prepare-directory",
        }
    }
}

impl fmt::Display for ServerSettingsOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `ServerSettingsError` (upstream `Schema.TaggedError`). The Display mirrors
/// upstream's `get message()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerSettingsError {
    #[serde(rename = "ServerSettingsError")]
    ServerSettingsError {
        settings_path: String,
        operation: ServerSettingsOperation,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "optional_key"
        )]
        provider_instance_id: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "optional_key"
        )]
        environment_variable: Option<String>,
        // Validation failures (e.g. a create colliding with an existing
        // instance) originate without an upstream defect.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "optional_key"
        )]
        cause: Option<serde_json::Value>,
    },
}

impl fmt::Display for ServerSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::ServerSettingsError {
            settings_path,
            operation,
            provider_instance_id,
            environment_variable,
            ..
        } = self;
        let provider = match provider_instance_id {
            Some(id) => format!(" for provider {id}"),
            None => String::new(),
        };
        let variable = match environment_variable {
            Some(name) => format!(" and environment variable {name}"),
            None => String::new(),
        };
        write!(
            f,
            "Server settings {operation} failed{provider}{variable} at {settings_path}."
        )
    }
}

impl std::error::Error for ServerSettingsError {}

// ── Unified type ─────────────────────────────────────────────────────

/**
 * Port of upstream `UnifiedSettings = Omit<ServerSettings, keyof
 * ModelPreferences> & ClientSettings`. Upstream composes the type purely for
 * the settings UI; server and client settings are written to separate files,
 * so this is never a single wire shape. Turin composes instead of flattening
 * ~110 duplicated fields.
 */
#[derive(Debug, Clone, PartialEq)]
pub struct UnifiedSettings {
    pub server: ServerSettings,
    pub client: ClientSettings,
}

impl Default for UnifiedSettings {
    /// Port of `DEFAULT_UNIFIED_SETTINGS = { ...DEFAULT_SERVER_SETTINGS,
    /// ...DEFAULT_CLIENT_SETTINGS }`.
    fn default() -> Self {
        Self {
            server: ServerSettings::default(),
            client: ClientSettings::default(),
        }
    }
}

/// Port of `DEFAULT_UNIFIED_SETTINGS`.
pub fn default_unified_settings() -> UnifiedSettings {
    UnifiedSettings::default()
}

// ── Server Settings Patch (replace with a Schema.deepPartial if available) ──────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectionPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub instance_id: Option<ProviderInstanceId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub model: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_provider_option_selections"
    )]
    pub options: Option<Vec<ProviderOptionSelection>>,
}

/// Serde codec for the optional `options` field of [`ModelSelectionPatch`]:
/// upstream's `ProviderOptionSelections` (the canonical array, or the legacy
/// object shape from pre-migration data) inside an `optionalKey`.
mod optional_provider_option_selections {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<Vec<ProviderOptionSelection>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<ProviderOptionSelection>>, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        crate::model::provider_option_selections::deserialize(raw)
            .map(Some)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelFavorite {
    pub provider: ProviderInstanceId,
    pub model: TrimmedNonEmptyString,
    pub favorite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelHidden {
    pub provider: ProviderInstanceId,
    pub model: TrimmedNonEmptyString,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetProviderModelOrder {
    pub provider: ProviderInstanceId,
    pub model_order: Vec<TrimmedString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateModelPreferences {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub favorites: Option<ModelFavorites>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_model_preferences: Option<ProviderModelPreferences>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCleanupRulesPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_keep_when: Option<WorktreeKeepWhen>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub worktree_after_days: Option<Option<StorageRetentionDays>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_on_merge: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_on_delete: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_unchanged: Option<bool>,
}

/// Patch form of `WorktreeCleanup`: custom rules may arrive partially.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum WorktreeCleanupPatch {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "custom")]
    Custom { rules: WorktreeCleanupRulesPatch },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupSettingsPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_keep_when: Option<WorktreeKeepWhen>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub worktree_after_days: Option<Option<StorageRetentionDays>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_on_merge: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_on_delete: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktree_unchanged: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub browser_artifacts_after_days: Option<Option<StorageRetentionDays>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub logs_after_days: Option<Option<StorageRetentionDays>>,
}

/// Upstream's patch reuses `BackgroundActivityOverrides` (every key is already
/// optional).
pub type BackgroundActivityOverridesPatch = BackgroundActivityOverrides;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivitySettingsPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub schema_version: Option<BackgroundActivitySchemaVersion>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub profile: Option<BackgroundActivityProfileSelection>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub base_profile: Option<BackgroundActivityProfile>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub overrides: Option<BackgroundActivityOverridesPatch>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlWritingStyleSettingsPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub mode: Option<SourceControlWritingStyleMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub custom_instructions: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub follow_change_request_templates: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilitySettingsPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub otlp_traces_url: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub otlp_metrics_url: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub otlp_logs_url: Option<TrimmedString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettingsPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub set_model_favorites: Option<Vec<SetModelFavorite>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub set_models_hidden: Option<Vec<SetModelHidden>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub set_provider_model_order: Option<SetProviderModelOrder>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub favorites: Option<ModelFavorites>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_model_preferences: Option<ProviderModelPreferences>,
    // Applied under the settings service's write lock, only while still unset.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub migrate_model_preferences: Option<MigrateModelPreferences>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub worktree_cleanup: Option<Option<WorktreeCleanupPatch>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub storage_cleanup: Option<StorageCleanupSettingsPatch>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub worktrees_directory: Option<TrimmedString>,
    // Server settings
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub response_streaming_mode: Option<ResponseStreamingMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enable_provider_update_checks: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub continue_threads_after_server_update: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enable_agent_browser_access: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub project_agent_browser_access_overrides: Option<IndexMap<ProjectId, Option<bool>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_auto_pull: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_project_scripts: Option<Vec<ProjectScript>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub project_script_overrides: Option<IndexMap<ProjectId, Option<Vec<ProjectScript>>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub project_auto_pull_overrides: Option<IndexMap<ProjectId, Option<bool>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub default_runtime_mode: Option<RuntimeMode>,
    /**
     * Per-project entry replacement: each entry replaces that project's whole
     * override set and `null` removes it. Clearing one override means resending
     * the entry without that key. Per-key null cannot express "clear" for the
     * keys whose value type is itself nullable, and clients always hold the
     * current entry from the last settings snapshot.
     *
     * `providerInstances` is the one exception: an entry that omits the key
     * keeps the stored value instead of clearing it, so a client built before
     * that field existed cannot wipe it by editing another key. Clearing every
     * provider override for a project means sending `providerInstances: {}`.
     */
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub project_settings_overrides: Option<IndexMap<ProjectId, Option<ProjectSettingsOverrides>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enable_agent_device_access: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub enable_device_support: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub device_onboarding_completed: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub sidebar_auto_settle_after_days: Option<Option<SidebarAutoSettleAfterDays>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_auto_settle_keep_pull_requests: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_auto_settle_on_merge: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub auto_resume_limited_threads: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snooze_limited_threads: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub background_activity: Option<BackgroundActivitySettingsPatch>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub automatic_git_fetch_interval: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_health_refresh_interval: Option<DurationFromMillis>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub background_activity_profile: Option<BackgroundActivityProfile>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub environment_icon: Option<Option<EnvironmentMachineKind>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub new_worktrees_start_from_origin: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub worktree_submodules: Option<Option<WorktreeSubmodules>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub add_project_base_directory: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub text_generation_model_selection: Option<ModelSelectionPatch>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub branch_naming_mode: Option<BranchNamingMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub branch_name_prefix: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub branch_name_instructions: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub remove_agent_credits_on_merge: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub source_control_writing_style: Option<SourceControlWritingStyleSettingsPatch>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub source_control_writer_model_selection: Option<Option<ModelSelection>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub review_guides_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub review_guide_model_selection: Option<Option<ModelSelection>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub review_guide_instructions: Option<TrimmedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub pull_request_merge_method: Option<Option<PullRequestMergeMethod>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub observability: Option<ObservabilitySettingsPatch>,
    /**
     * Merges per host: a patched field replaces the saved one, so a client sends a per-server-host
     * map whole, with the redaction marker for each saved secret it keeps. An empty or omitted
     * secret is removed from the secret store.
     */
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub source_control_hosts: Option<IndexMap<String, SourceControlHostSettings>>,
    // Whole-map replacement for the new instance config. Patching individual
    // entries is intentionally out of scope: the map is small, and partial
    // patches risk leaving driver-specific config in a half-merged state.
    // The web UI sends a fully-formed map every time it edits this field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_instances: Option<ProviderInstanceConfigMap>,
    // Per-entry, unlike `providerInstances`: a client only ever adds or removes
    // one source, and sending the whole map races another edit that has not
    // echoed back yet. `null` removes; the server merges into its current map.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub usage_limit_sources: Option<IndexMap<UsageLimitSourceId, Option<UsageLimitSourceConfig>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub cursor_keychain_usage_enabled: Option<bool>,
    /** Each entry replaces one model's rates; `null` restores automatic pricing. */
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub usage_price_overrides:
        Option<IndexMap<TrimmedNonEmptyString, Option<UsageModelPriceOverride>>>,
    /** Each entry replaces one model's mapping; `null` removes it. */
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub usage_model_aliases: Option<IndexMap<TrimmedNonEmptyString, Option<TrimmedNonEmptyString>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSettingsPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub notification_mode: Option<NotificationMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub in_app_notifications_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub diff_color_scheme: Option<DiffColorScheme>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub chat_width: Option<ChatWidth>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub load_balancing_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub load_balancing_weights: Option<LoadBalancingWeights>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub appearance_contrast: Option<AppearanceContrast>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub panel_animation_duration_ms: Option<PanelAnimationDurationMs>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub browser_recording_frame_rate: Option<BrowserRecordingFrameRate>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub browser_recording_show_key_presses: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub browser_recording_show_mouse_presses: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub browser_link_target: Option<BrowserLinkTarget>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub browser_auto_show_floating_preview: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub confirm_quit: Option<QuitConfirmationMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub confirm_thread_archive: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub confirm_thread_delete: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub confirm_thread_unpin: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub diff_files_collapsed: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub diff_ignore_whitespace: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub diff_layout: Option<DiffLayout>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub environment_identification_mode: Option<EnvironmentIdentificationMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub glass_opacity: Option<GlassOpacity>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub onboarding_completed_at: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_size_interface: Option<InterfaceFontSize>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_size_prompt: Option<PromptFontSize>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_size_code: Option<CodeFontSize>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_size_terminal: Option<TerminalFontSize>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_family_code: Option<FontFamilyPreference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_family_composer: Option<FontFamilyPreference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_family_sans: Option<FontFamilyPreference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_family_terminal: Option<FontFamilyPreference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub font_smoothing: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub persist_composer_context_strip: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub favorites: Option<ModelFavorites>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub provider_model_preferences: Option<ProviderModelPreferences>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub pull_request_merge_method_overrides:
        Option<IndexMap<TrimmedNonEmptyString, PullRequestMergeMethod>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub plan_mode_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub context_window_meter_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub composer_collapse_on_scroll: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub composer_rich_text_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub send_shortcut: Option<SendShortcut>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub follow_up_behavior: Option<FollowUpBehavior>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub proactive_panels_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub show_skills_in_slash_menu: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub legacy_sidebar_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_working_shelf_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_project_grouping_mode: Option<SidebarProjectGroupingMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_project_grouping_overrides:
        Option<IndexMap<TrimmedNonEmptyString, SidebarProjectGroupingMode>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_project_sort_order: Option<SidebarProjectSortOrder>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_thread_sort_order: Option<SidebarThreadSortOrder>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub sidebar_thread_preview_count: Option<SidebarThreadPreviewCount>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub timestamp_format: Option<TimestampFormat>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_include_accessibility: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_shortcut: Option<SnapShotShortcut>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_play_sound: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_sound: Option<SnapShotSound>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_flash: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub snap_shot_animations: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_key"
    )]
    pub word_wrap: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CustomModelEntry, ModelCapabilities, ProviderOptionChoice, ProviderOptionDescriptor,
        SelectOptionType, SelectProviderOptionDescriptor,
    };
    use serde_json::json;

    fn decode_server(value: serde_json::Value) -> ServerSettings {
        serde_json::from_value(value).unwrap()
    }

    fn decode_server_patch(value: serde_json::Value) -> ServerSettingsPatch {
        serde_json::from_value(value).unwrap()
    }

    fn decode_client(value: serde_json::Value) -> ClientSettings {
        serde_json::from_value(value).unwrap()
    }

    fn decode_client_patch(value: serde_json::Value) -> ClientSettingsPatch {
        serde_json::from_value(value).unwrap()
    }

    fn encode<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn provider_instance_id(id: &str) -> ProviderInstanceId {
        ProviderInstanceId::new(id).unwrap()
    }

    fn provider_driver(driver: &str) -> ProviderDriverKind {
        ProviderDriverKind::new(driver).unwrap()
    }

    fn project_id(id: &str) -> ProjectId {
        ProjectId::new(id).unwrap()
    }

    fn config(
        driver: &str,
        enabled: Option<bool>,
        config: serde_json::Value,
    ) -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: provider_driver(driver),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled,
            config: Some(config),
        }
    }

    // ── ServerSettings response streaming ────────────────────────

    #[test]
    fn response_streaming_defaults_to_paragraph() {
        assert_eq!(
            decode_server(json!({})).response_streaming_mode,
            ResponseStreamingMode::Paragraph
        );
    }

    #[test]
    fn response_streaming_round_trips_as_setting_and_project_override() {
        for mode in ["turn", "paragraph"] {
            let input = json!({
                "responseStreamingMode": mode,
                "projectSettingsOverrides": { "project": { "responseStreamingMode": mode } },
            });
            let encoded = encode(&decode_server(input.clone()));
            assert_eq!(encoded["responseStreamingMode"], json!(mode));
            assert_eq!(
                encoded["projectSettingsOverrides"]["project"]["responseStreamingMode"],
                json!(mode)
            );
            let patch = decode_server_patch(input.clone());
            assert_eq!(
                patch.response_streaming_mode,
                Some(match mode {
                    "turn" => ResponseStreamingMode::Turn,
                    _ => ResponseStreamingMode::Paragraph,
                })
            );
        }
    }

    #[test]
    fn response_streaming_rejects_unknown_modes() {
        for mode in ["token", "unsupported"] {
            for input in [
                json!({ "responseStreamingMode": mode }),
                json!({ "projectSettingsOverrides": { "project": { "responseStreamingMode": mode } } }),
            ] {
                assert!(serde_json::from_value::<ServerSettings>(input.clone()).is_err());
                assert!(serde_json::from_value::<ServerSettingsPatch>(input).is_err());
            }
        }
    }

    // ── storage cleanup settings ─────────────────────────────────

    #[test]
    fn cleanup_stays_disabled_for_existing_installations() {
        let settings = decode_server(json!({}));
        assert_eq!(settings.worktree_cleanup, None);
        assert_eq!(
            settings.storage_cleanup,
            StorageCleanupSettings {
                worktree_keep_when: WorktreeKeepWhen::UncommittedChanges,
                worktree_after_days: None,
                worktree_on_merge: false,
                worktree_on_delete: false,
                worktree_unchanged: false,
                browser_artifacts_after_days: None,
                logs_after_days: None,
            }
        );
    }

    #[test]
    fn cleanup_patch_accepts_partial_retention_updates() {
        assert_eq!(
            decode_server_patch(json!({ "storageCleanup": { "worktreeAfterDays": 8 } }))
                .storage_cleanup,
            Some(StorageCleanupSettingsPatch {
                worktree_after_days: Some(Some(StorageRetentionDays::new(8).unwrap())),
                ..Default::default()
            })
        );
        assert_eq!(
            decode_server_patch(json!({ "storageCleanup": { "worktreeAfterDays": null } }))
                .storage_cleanup,
            Some(StorageCleanupSettingsPatch {
                worktree_after_days: Some(None),
                ..Default::default()
            })
        );
    }

    #[test]
    fn cleanup_patch_partial_but_stored_rules_complete() {
        let patch = decode_server_patch(json!({
            "worktreeCleanup": { "mode": "custom", "rules": { "worktreeAfterDays": 8 } }
        }));
        assert_eq!(
            patch.worktree_cleanup,
            Some(Some(WorktreeCleanupPatch::Custom {
                rules: WorktreeCleanupRulesPatch {
                    worktree_after_days: Some(Some(StorageRetentionDays::new(8).unwrap())),
                    ..Default::default()
                }
            }))
        );
        // Stored project rules must be complete: the required booleans fail
        // the whole settings snapshot when missing.
        assert!(serde_json::from_value::<ServerSettings>(json!({
            "projectSettingsOverrides": {
                "project": { "worktreeCleanup": { "mode": "custom", "rules": { "worktreeAfterDays": 8 } } }
            }
        }))
        .is_err());
    }

    #[test]
    fn cleanup_decodes_older_custom_rules_with_default_local_file_policy() {
        let settings = decode_server(json!({
            "worktreeCleanup": {
                "mode": "custom",
                "rules": {
                    "worktreeAfterDays": 8,
                    "worktreeOnMerge": false,
                    "worktreeOnDelete": false,
                    "worktreeUnchanged": false
                }
            }
        }));
        assert_eq!(
            settings.worktree_cleanup,
            Some(WorktreeCleanup::Custom {
                rules: WorktreeCleanupRules {
                    worktree_keep_when: WorktreeKeepWhen::UncommittedChanges,
                    worktree_after_days: Some(StorageRetentionDays::new(8).unwrap()),
                    worktree_on_merge: false,
                    worktree_on_delete: false,
                    worktree_unchanged: false,
                }
            })
        );
        assert_eq!(
            decode_server_patch(
                json!({ "storageCleanup": { "worktreeKeepWhen": "tracked-changes" } })
            )
            .storage_cleanup,
            Some(StorageCleanupSettingsPatch {
                worktree_keep_when: Some(WorktreeKeepWhen::TrackedChanges),
                ..Default::default()
            })
        );
        assert!(
            serde_json::from_value::<ServerSettingsPatch>(
                json!({ "storageCleanup": { "worktreeKeepWhen": "unknown" } })
            )
            .is_err()
        );
    }

    #[test]
    fn cleanup_rejects_invalid_retention() {
        for days in [json!(0), json!(-1), json!(1.5), json!(3651)] {
            assert!(
                serde_json::from_value::<ServerSettingsPatch>(json!({
                    "storageCleanup": { "browserArtifactsAfterDays": days }
                }))
                .is_err(),
                "{days} should be rejected"
            );
        }
    }

    // ── ClientSettings rich text composer ────────────────────────

    #[test]
    fn rich_text_enabled_for_new_and_existing_settings() {
        assert!(decode_client(json!({})).composer_rich_text_enabled);
        assert!(decode_client(json!({ "sendShortcut": "mod-enter" })).composer_rich_text_enabled);
    }

    #[test]
    fn rich_text_preserves_explicit_opt_out() {
        let settings = decode_client(json!({ "composerRichTextEnabled": false }));
        assert!(!settings.composer_rich_text_enabled);
        assert_eq!(encode(&settings)["composerRichTextEnabled"], json!(false));
        assert_eq!(
            decode_client_patch(json!({ "composerRichTextEnabled": false }))
                .composer_rich_text_enabled,
            Some(false)
        );
    }

    // ── ServerSettings default permissions ──────────────────────

    #[test]
    fn default_permissions_stay_full_access() {
        assert_eq!(
            decode_server(json!({})).default_runtime_mode,
            RuntimeMode::FullAccess
        );
        assert_eq!(
            default_server_settings().default_runtime_mode,
            RuntimeMode::FullAccess
        );
    }

    #[test]
    fn default_permissions_round_trip_as_setting_and_project_override() {
        for mode in [
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::Auto,
            RuntimeMode::FullAccess,
        ] {
            let mode_json = match mode {
                RuntimeMode::ApprovalRequired => "approval-required",
                RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
                RuntimeMode::Auto => "auto",
                RuntimeMode::FullAccess => "full-access",
            };
            let input = json!({
                "defaultRuntimeMode": mode_json,
                "projectSettingsOverrides": { "project": { "defaultRuntimeMode": mode_json } },
            });
            let encoded = encode(&decode_server(input.clone()));
            assert_eq!(encoded["defaultRuntimeMode"], json!(mode_json));
            assert_eq!(
                encoded["projectSettingsOverrides"]["project"]["defaultRuntimeMode"],
                json!(mode_json)
            );
            assert_eq!(decode_server_patch(input).default_runtime_mode, Some(mode));
        }
    }

    #[test]
    fn default_permissions_reject_unsupported() {
        assert!(
            serde_json::from_value::<ServerSettings>(json!({
                "defaultRuntimeMode": "unsupported"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ServerSettingsPatch>(json!({
                "defaultRuntimeMode": "unsupported"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ServerSettingsPatch>(json!({
                "projectSettingsOverrides": { "project": { "defaultRuntimeMode": "unsupported" } }
            }))
            .is_err()
        );
    }

    // ── ServerSettings usage price overrides ─────────────────────

    #[test]
    fn usage_price_overrides_default_and_trim_model_ids() {
        assert!(decode_server(json!({})).usage_price_overrides.is_empty());
        let settings = decode_server(json!({
            "usagePriceOverrides": {
                "  vendor/example-model  ": {
                    "inputCostPerMillionTokens": 2,
                    "outputCostPerMillionTokens": 8
                }
            }
        }));
        let encoded = encode(&settings);
        assert_eq!(
            encoded["usagePriceOverrides"],
            json!({
                "vendor/example-model": {
                    "inputCostPerMillionTokens": 2,
                    "outputCostPerMillionTokens": 8
                }
            })
        );
    }

    #[test]
    fn usage_price_overrides_accept_zero_cache_rates_and_deletion() {
        let overrides = json!({
            "example-model": {
                "inputCostPerMillionTokens": 0,
                "outputCostPerMillionTokens": 0,
                "cacheReadCostPerMillionTokens": 0.5,
                "cacheWriteCostPerMillionTokens": 3
            },
            "removed-model": null
        });
        let patch = decode_server_patch(json!({ "usagePriceOverrides": overrides.clone() }));
        let decoded = patch.usage_price_overrides.as_ref().unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded.get(&TrimmedNonEmptyString::new("removed-model").unwrap()),
            Some(&None)
        );
        let encoded = encode(&patch);
        assert_eq!(encoded["usagePriceOverrides"], overrides);
    }

    #[test]
    fn usage_price_overrides_reject_invalid_rates() {
        // NaN and Infinity from upstream's test are not representable in JSON,
        // so only the encodable invalid rates apply here.
        for field in [
            "inputCostPerMillionTokens",
            "outputCostPerMillionTokens",
            "cacheReadCostPerMillionTokens",
            "cacheWriteCostPerMillionTokens",
        ] {
            for value in [json!(-1), json!("2")] {
                let usage_price_overrides = json!({
                    "example-model": {
                        "inputCostPerMillionTokens": 2,
                        "outputCostPerMillionTokens": 8,
                        field: value
                    }
                });
                assert!(
                    serde_json::from_value::<ServerSettings>(json!({
                        "usagePriceOverrides": usage_price_overrides
                    }))
                    .is_err()
                );
                assert!(
                    serde_json::from_value::<ServerSettingsPatch>(json!({
                        "usagePriceOverrides": usage_price_overrides
                    }))
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn usage_price_overrides_reject_empty_ids_and_incomplete_pricing() {
        for usage_price_overrides in [
            json!({ " ": { "inputCostPerMillionTokens": 2, "outputCostPerMillionTokens": 8 } }),
            json!({ "example-model": { "inputCostPerMillionTokens": 2 } }),
            json!({ "example-model": { "outputCostPerMillionTokens": 8 } }),
        ] {
            assert!(
                serde_json::from_value::<ServerSettingsPatch>(json!({
                    "usagePriceOverrides": usage_price_overrides
                }))
                .is_err()
            );
        }
    }

    // ── provider settings schemas ────────────────────────────────

    #[test]
    fn claude_custom_models_accept_legacy_bare_slugs_alongside_entries() {
        let decoded: ClaudeSettings = serde_json::from_value(json!({
            "customModels": [
                "bare-slug",
                {
                    "slug": "named",
                    "name": "Named",
                    "capabilities": {
                        "optionDescriptors": [
                            {
                                "id": "effort",
                                "label": "Reasoning",
                                "type": "select",
                                "options": [{ "id": "high", "label": "High", "isDefault": true }]
                            }
                        ]
                    }
                }
            ]
        }))
        .unwrap();
        assert_eq!(
            decoded.custom_models,
            vec![
                CustomModelSetting::Slug("bare-slug".to_owned()),
                CustomModelSetting::Entry(CustomModelEntry {
                    slug: TrimmedNonEmptyString::new("named").unwrap(),
                    name: Some(TrimmedNonEmptyString::new("Named").unwrap()),
                    capabilities: Some(ModelCapabilities {
                        option_descriptors: Some(vec![ProviderOptionDescriptor::Select(
                            SelectProviderOptionDescriptor {
                                id: TrimmedNonEmptyString::new("effort").unwrap(),
                                label: TrimmedNonEmptyString::new("Reasoning").unwrap(),
                                description: None,
                                type_: SelectOptionType,
                                options: vec![ProviderOptionChoice {
                                    id: TrimmedNonEmptyString::new("high").unwrap(),
                                    label: TrimmedNonEmptyString::new("High").unwrap(),
                                    description: None,
                                    is_default: Some(true),
                                }],
                                current_value: None,
                                prompt_injected_values: None,
                            }
                        )]),
                    }),
                }),
            ]
        );
    }

    #[test]
    fn claude_auto_compaction_defaults_to_claude_threshold() {
        let decoded: ClaudeSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(decoded.auto_compact_window.as_str(), "");
    }

    #[test]
    fn claude_auto_compaction_accepts_supported_thresholds() {
        for value in ["100000", "300000", "1000000"] {
            let decoded: ClaudeSettings =
                serde_json::from_value(json!({ "autoCompactWindow": value })).unwrap();
            assert_eq!(decoded.auto_compact_window.as_str(), value);
        }
    }

    #[test]
    fn claude_auto_compaction_rejects_unsupported_thresholds() {
        for value in ["99999", "1000001", "300k", "invalid"] {
            assert!(
                serde_json::from_value::<ClaudeSettings>(json!({ "autoCompactWindow": value }))
                    .is_err(),
                "{value} should be rejected"
            );
        }
    }

    #[test]
    fn codex_binary_path_defaults_to_the_fallback() {
        let decoded: CodexSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(decoded.binary_path.as_str(), "codex");
        assert!(decoded.enabled);
        // Empty and blank inputs decode to the provider's default executable.
        for input in ["", "   "] {
            let decoded: CodexSettings =
                serde_json::from_value(json!({ "binaryPath": input })).unwrap();
            assert_eq!(decoded.binary_path.as_str(), "codex");
        }
    }

    #[test]
    fn codex_setup_mode_round_trips() {
        let decoded: CodexSettings =
            serde_json::from_value(json!({ "setupMode": "managed" })).unwrap();
        assert_eq!(decoded.setup_mode, Some(CodexSetupMode::Managed));
        assert_eq!(encode(&decoded)["setupMode"], json!("managed"));
        let plain: CodexSettings = serde_json::from_value(json!({})).unwrap();
        assert!(encode(&plain).get("setupMode").is_none());
    }

    // ── ClientSettings notifications ────────────────────────────

    #[test]
    fn notifications_require_opt_in_when_unset() {
        let settings = decode_client(json!({}));
        assert_eq!(settings.notification_mode, NotificationMode::Off);
        assert!(!settings.in_app_notifications_enabled);
        let patch = decode_client_patch(json!({}));
        assert_eq!(patch.in_app_notifications_enabled, None);
        assert_eq!(patch.notification_mode, None);
    }

    #[test]
    fn notifications_round_trip_in_app_preference() {
        for enabled in [true, false] {
            assert_eq!(
                encode(&decode_client(
                    json!({ "inAppNotificationsEnabled": enabled })
                ))["inAppNotificationsEnabled"],
                json!(enabled)
            );
            assert_eq!(
                decode_client_patch(json!({ "inAppNotificationsEnabled": enabled }))
                    .in_app_notifications_enabled,
                Some(enabled)
            );
        }
    }

    #[test]
    fn notifications_reject_invalid_in_app_preference() {
        for value in [json!("true"), json!(1), json!(null)] {
            assert!(
                serde_json::from_value::<ClientSettings>(json!({
                    "inAppNotificationsEnabled": value
                }))
                .is_err()
            );
            assert!(
                serde_json::from_value::<ClientSettingsPatch>(json!({
                    "inAppNotificationsEnabled": value
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn notifications_round_trip_every_mode() {
        for (mode, expected) in [
            ("off", NotificationMode::Off),
            ("notifications", NotificationMode::Notifications),
            ("sound", NotificationMode::Sound),
            (
                "notifications-and-sound",
                NotificationMode::NotificationsAndSound,
            ),
        ] {
            assert_eq!(
                encode(&decode_client(json!({ "notificationMode": mode })))["notificationMode"],
                json!(mode)
            );
            assert_eq!(
                decode_client_patch(json!({ "notificationMode": mode })).notification_mode,
                Some(expected)
            );
        }
    }

    #[test]
    fn notifications_reject_unsupported_modes() {
        for mode in [json!("always"), json!(true), json!(null)] {
            assert!(
                serde_json::from_value::<ClientSettings>(json!({
                    "notificationMode": mode
                }))
                .is_err()
            );
            assert!(
                serde_json::from_value::<ClientSettingsPatch>(json!({
                    "notificationMode": mode
                }))
                .is_err()
            );
        }
    }

    // ── ClientSettings defaults round-up ─────────────────────────

    #[test]
    fn client_settings_decode_upstream_defaults() {
        let settings = decode_client(json!({}));
        assert_eq!(
            settings,
            ClientSettings {
                notification_mode: NotificationMode::Off,
                in_app_notifications_enabled: false,
                diff_color_scheme: DiffColorScheme::RedGreen,
                chat_width: ChatWidth::Comfortable,
                load_balancing_enabled: false,
                load_balancing_weights: LoadBalancingWeights::default(),
                appearance_contrast: AppearanceContrast::new(100).unwrap(),
                panel_animation_duration_ms: PanelAnimationDurationMs::new(0).unwrap(),
                browser_recording_frame_rate: BrowserRecordingFrameRate::Thirty,
                browser_recording_show_key_presses: false,
                browser_recording_show_mouse_presses: false,
                browser_link_target: BrowserLinkTarget::System,
                browser_auto_show_floating_preview: true,
                confirm_quit: QuitConfirmationMode::Hold,
                confirm_thread_archive: false,
                confirm_thread_delete: true,
                confirm_thread_unpin: false,
                dismissed_provider_update_notification_keys: Vec::new(),
                diff_files_collapsed: true,
                diff_ignore_whitespace: true,
                diff_layout: DiffLayout::Stacked,
                environment_identification_mode: EnvironmentIdentificationMode::Artwork,
                glass_opacity: GlassOpacity::new(80).unwrap(),
                font_size_interface: InterfaceFontSize::new(16).unwrap(),
                font_size_prompt: PromptFontSize::new(14).unwrap(),
                font_size_code: CodeFontSize::new(13).unwrap(),
                font_size_terminal: TerminalFontSize::new(12).unwrap(),
                font_family_code: FontFamilyPreference::default(),
                font_family_composer: FontFamilyPreference::default(),
                font_family_sans: FontFamilyPreference::default(),
                font_family_terminal: FontFamilyPreference::default(),
                font_smoothing: true,
                onboarding_completed_at: None,
                persist_composer_context_strip: false,
                favorites: Vec::new(),
                provider_model_preferences: ProviderModelPreferences::default(),
                pull_request_merge_method_overrides: IndexMap::new(),
                plan_mode_enabled: false,
                context_window_meter_enabled: false,
                composer_collapse_on_scroll: true,
                composer_rich_text_enabled: true,
                send_shortcut: SendShortcut::Enter,
                follow_up_behavior: FollowUpBehavior::Queue,
                proactive_panels_enabled: false,
                show_skills_in_slash_menu: true,
                legacy_sidebar_enabled: false,
                sidebar_working_shelf_enabled: false,
                sidebar_project_grouping_mode: SidebarProjectGroupingMode::Repository,
                sidebar_project_grouping_overrides: IndexMap::new(),
                sidebar_project_sort_order: SidebarProjectSortOrder::UpdatedAt,
                sidebar_thread_sort_order: SidebarThreadSortOrder::UpdatedAt,
                sidebar_thread_preview_count: SidebarThreadPreviewCount::new(6).unwrap(),
                timestamp_format: TimestampFormat::Locale,
                snap_shot_enabled: false,
                snap_shot_include_accessibility: true,
                snap_shot_shortcut: SnapShotShortcut::BothShiftKeys {
                    kind: SnapShotBothShiftKeysKind,
                },
                snap_shot_play_sound: true,
                snap_shot_sound: SnapShotSound::SoftPop,
                snap_shot_flash: true,
                snap_shot_animations: true,
                word_wrap: true,
            }
        );
    }

    #[test]
    fn client_settings_drops_retired_and_cut_keys() {
        // Retired upstream keys (sidebarV2*, compact*, composerCollapseOnBlur)
        // and cut Turin fields (browser defaults/profiles) decode away.
        let settings = decode_client(json!({
            "sidebarV2Enabled": false,
            "sidebarV2ConfiguredByUser": true,
            "compactSidebarEnabled": true,
            "sidebarCompactThreadRows": true,
            "composerCollapseOnBlur": false,
            "chatWordWrap": false,
            "diffWordWrap": false,
            "browserDefaultViewport": { "_tag": "fill" },
            "browserProfiles": [],
        }));
        assert!(!settings.legacy_sidebar_enabled);
        assert!(settings.word_wrap);
        assert!(settings.composer_collapse_on_scroll);
        assert_eq!(
            decode_client_patch(json!({
                "compactSidebarEnabled": true, "sidebarCompactThreadRows": true
            })),
            ClientSettingsPatch::default()
        );
    }

    #[test]
    fn diff_files_stay_collapsed_without_a_saved_preference() {
        assert!(decode_client(json!({})).diff_files_collapsed);
        for collapsed in [true, false] {
            assert_eq!(
                encode(&decode_client(json!({ "diffFilesCollapsed": collapsed })))["diffFilesCollapsed"],
                json!(collapsed)
            );
            assert_eq!(
                decode_client_patch(json!({ "diffFilesCollapsed": collapsed }))
                    .diff_files_collapsed,
                Some(collapsed)
            );
        }
    }

    #[test]
    fn diff_colors_default_and_round_trip() {
        assert_eq!(
            decode_client(json!({})).diff_color_scheme,
            DiffColorScheme::RedGreen
        );
        for (scheme, expected) in [
            ("red-green", DiffColorScheme::RedGreen),
            ("blue-orange", DiffColorScheme::BlueOrange),
        ] {
            assert_eq!(
                encode(&decode_client(json!({ "diffColorScheme": scheme })))["diffColorScheme"],
                json!(scheme)
            );
            assert_eq!(
                decode_client_patch(json!({ "diffColorScheme": scheme })).diff_color_scheme,
                Some(expected)
            );
        }
        assert!(
            serde_json::from_value::<ClientSettings>(json!({
                "diffColorScheme": "purple-yellow"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "diffColorScheme": "purple-yellow"
            }))
            .is_err()
        );
    }

    #[test]
    fn chat_width_default_and_round_trip() {
        assert_eq!(decode_client(json!({})).chat_width, ChatWidth::Comfortable);
        for (width, expected) in [
            ("comfortable", ChatWidth::Comfortable),
            ("wide", ChatWidth::Wide),
            ("full", ChatWidth::Full),
        ] {
            assert_eq!(
                encode(&decode_client(json!({ "chatWidth": width })))["chatWidth"],
                json!(width)
            );
            assert_eq!(
                decode_client_patch(json!({ "chatWidth": width })).chat_width,
                Some(expected)
            );
        }
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "chatWidth": "huge"
            }))
            .is_err()
        );
    }

    #[test]
    fn load_balancing_requires_opt_in() {
        assert!(!decode_client(json!({})).load_balancing_enabled);
        assert!(!decode_client(json!({ "loadBalancingWeights": {} })).load_balancing_enabled);
        for enabled in [true, false] {
            assert_eq!(
                decode_client_patch(json!({ "loadBalancingEnabled": enabled }))
                    .load_balancing_enabled,
                Some(enabled)
            );
        }
    }

    #[test]
    fn load_balancing_weights_are_range_checked() {
        assert!(
            serde_json::from_value::<ClientSettings>(json!({
                "loadBalancingWeights": { "gpt-5.6": 101 }
            }))
            .is_err()
        );
        let decoded = decode_client(json!({ "loadBalancingWeights": { "gpt-5.6": 60 } }));
        assert_eq!(
            decoded.load_balancing_weights,
            [(
                TrimmedNonEmptyString::new("gpt-5.6").unwrap(),
                LoadBalancingWeight::new(60).unwrap()
            )]
            .into_iter()
            .collect::<LoadBalancingWeights>()
        );
    }

    #[test]
    fn composer_context_strip_defaults_to_draft_only() {
        assert!(!decode_client(json!({})).persist_composer_context_strip);
        assert_eq!(
            decode_client_patch(json!({ "persistComposerContextStrip": true }))
                .persist_composer_context_strip,
            Some(true)
        );
    }

    #[test]
    fn word_wrap_defaults_on() {
        assert!(decode_client(json!({})).word_wrap);
    }

    #[test]
    fn proactive_panels_are_opt_in() {
        assert!(!decode_client(json!({})).proactive_panels_enabled);
        assert_eq!(
            decode_client_patch(json!({ "proactivePanelsEnabled": true })).proactive_panels_enabled,
            Some(true)
        );
    }

    #[test]
    fn context_window_meter_defaults_off() {
        assert!(!decode_client(json!({})).context_window_meter_enabled);
        assert!(
            decode_client(json!({ "contextWindowMeterEnabled": true }))
                .context_window_meter_enabled
        );
        assert_eq!(
            decode_client_patch(json!({ "contextWindowMeterEnabled": true }))
                .context_window_meter_enabled,
            Some(true)
        );
    }

    #[test]
    fn send_shortcut_defaults_to_enter() {
        assert_eq!(decode_client(json!({})).send_shortcut, SendShortcut::Enter);
        for shortcut in ["enter", "mod-enter-multiline", "mod-enter"] {
            assert_eq!(
                decode_client(json!({ "sendShortcut": shortcut }))
                    .send_shortcut
                    .as_str_name(),
                shortcut
            );
            assert_eq!(
                decode_client_patch(json!({ "sendShortcut": shortcut }))
                    .send_shortcut
                    .map(|s| s.as_str_name()),
                Some(shortcut)
            );
        }
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "sendShortcut": "invalid"
            }))
            .is_err()
        );
    }

    #[test]
    fn follow_up_behavior_defaults_to_queue() {
        assert_eq!(
            decode_client(json!({})).follow_up_behavior,
            FollowUpBehavior::Queue
        );
        for behavior in ["queue", "steer"] {
            assert_eq!(
                decode_client(json!({ "followUpBehavior": behavior }))
                    .follow_up_behavior
                    .as_str_name(),
                behavior
            );
            assert_eq!(
                decode_client_patch(json!({ "followUpBehavior": behavior }))
                    .follow_up_behavior
                    .map(|b| b.as_str_name()),
                Some(behavior)
            );
        }
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "followUpBehavior": "invalid"
            }))
            .is_err()
        );
    }

    #[test]
    fn composer_collapses_on_scroll_by_default() {
        assert!(decode_client(json!({})).composer_collapse_on_scroll);
        assert_eq!(
            decode_client_patch(json!({ "composerCollapseOnScroll": false }))
                .composer_collapse_on_scroll,
            Some(false)
        );
    }

    // ── ClientSettings window capture ───────────────────────────

    #[test]
    fn snapshot_defaults_to_capture_off_with_feedback_on() {
        let settings = decode_client(json!({}));
        assert!(!settings.snap_shot_enabled);
        assert!(settings.snap_shot_include_accessibility);
        assert_eq!(
            settings.snap_shot_shortcut,
            SnapShotShortcut::BothShiftKeys {
                kind: SnapShotBothShiftKeysKind
            }
        );
        assert!(settings.snap_shot_play_sound);
        assert_eq!(settings.snap_shot_sound, SnapShotSound::SoftPop);
        assert!(settings.snap_shot_flash);
        assert!(settings.snap_shot_animations);
    }

    #[test]
    fn snapshot_patch_accepts_capture_updates() {
        let patch = decode_client_patch(json!({
            "snapShotEnabled": true,
            "snapShotIncludeAccessibility": false,
            "snapShotShortcut": {
                "key": "w", "metaKey": false, "ctrlKey": false,
                "shiftKey": true, "altKey": true, "modKey": false
            },
            "snapShotPlaySound": false,
            "snapShotSound": "camera-shutter",
            "snapShotFlash": false,
            "snapShotAnimations": false
        }));
        assert!(patch.snap_shot_enabled.unwrap());
        assert!(!patch.snap_shot_include_accessibility.unwrap());
        assert!(matches!(
            patch.snap_shot_shortcut,
            Some(SnapShotShortcut::Chord(_))
        ));
        assert!(!patch.snap_shot_play_sound.unwrap());
        assert_eq!(patch.snap_shot_sound, Some(SnapShotSound::CameraShutter));
        assert!(!patch.snap_shot_flash.unwrap());
        assert!(!patch.snap_shot_animations.unwrap());
    }

    #[test]
    fn snapshot_rejects_unknown_sounds() {
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "snapShotSound": "doorbell"
            }))
            .is_err()
        );
    }

    #[test]
    fn snapshot_accepts_modifier_pairs() {
        let patch = decode_client_patch(json!({
            "snapShotShortcut": { "kind": "modifier-pair", "modifier": "meta" }
        }));
        assert_eq!(
            patch.snap_shot_shortcut,
            Some(SnapShotShortcut::ModifierPair {
                kind: SnapShotModifierPairKind,
                modifier: SnapShotModifier::Meta,
            })
        );
        assert_eq!(
            snap_shot_shortcut_modifier_pair(&patch.snap_shot_shortcut.unwrap()),
            SnapShotModifier::Meta
        );
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "snapShotShortcut": { "kind": "modifier-pair", "modifier": "hyper" }
            }))
            .is_err()
        );
    }

    #[test]
    fn snapshot_rejects_chords_without_a_modifier() {
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "snapShotShortcut": {
                    "key": "w", "metaKey": false, "ctrlKey": false,
                    "shiftKey": false, "altKey": false, "modKey": false
                }
            }))
            .is_err()
        );
    }

    #[test]
    fn snapshot_modifier_pair_helpers() {
        let both_shift = SnapShotShortcut::BothShiftKeys {
            kind: SnapShotBothShiftKeysKind,
        };
        assert!(is_modifier_pair_shortcut(&both_shift));
        assert_eq!(
            snap_shot_shortcut_modifier_pair(&both_shift),
            SnapShotModifier::Shift
        );
        // An explicit chord (the third union member) is not a pair shortcut.
        let chord = SnapShotShortcut::Chord(
            SnapShotKeyChord::new(KeybindingShortcut {
                key: crate::keybindings::KeybindingValue::new("w").unwrap(),
                meta_key: false,
                ctrl_key: false,
                shift_key: true,
                alt_key: false,
                mod_key: false,
            })
            .unwrap(),
        );
        assert!(!is_modifier_pair_shortcut(&chord));
        assert_eq!(
            snap_shot_modifier_pair_label(SnapShotModifier::Meta, true),
            "Command + Command"
        );
        assert_eq!(
            snap_shot_modifier_pair_label(SnapShotModifier::Meta, false),
            "Super + Super"
        );
        assert_eq!(
            snap_shot_modifier_pair_label(SnapShotModifier::Alt, true),
            "Option + Option"
        );
        assert_eq!(
            snap_shot_modifier_pair_label(SnapShotModifier::Control, false),
            "Ctrl + Ctrl"
        );
    }

    // ── ClientSettings quit confirmation ────────────────────────

    #[test]
    fn quit_confirmation_defaults_to_hold() {
        assert_eq!(
            decode_client(json!({})).confirm_quit,
            QuitConfirmationMode::Hold
        );
    }

    #[test]
    fn quit_confirmation_accepts_every_mode() {
        for (mode, expected) in [
            ("direct", QuitConfirmationMode::Direct),
            ("hold", QuitConfirmationMode::Hold),
            ("double-click", QuitConfirmationMode::DoubleClick),
        ] {
            assert_eq!(
                decode_client(json!({ "confirmQuit": mode })).confirm_quit,
                expected
            );
            assert_eq!(
                decode_client_patch(json!({ "confirmQuit": mode })).confirm_quit,
                Some(expected)
            );
        }
    }

    #[test]
    fn quit_confirmation_migrates_legacy_booleans() {
        for (legacy, mode) in [(json!(true), "hold"), (json!(false), "direct")] {
            let settings = decode_client(json!({ "confirmQuit": legacy }));
            assert_eq!(settings.confirm_quit.as_str_name(), mode);
            // Encodes back as the canonical string value.
            assert_eq!(encode(&settings)["confirmQuit"], json!(mode));
        }
    }

    #[test]
    fn quit_confirmation_rejects_booleans_at_the_patch_boundary() {
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "confirmQuit": true
            }))
            .is_err()
        );
    }

    // ── ClientSettings recording and appearance ranges ───────────

    #[test]
    fn browser_recording_frame_rate_defaults_to_30() {
        assert_eq!(
            decode_client(json!({})).browser_recording_frame_rate,
            BrowserRecordingFrameRate::Thirty
        );
        for (rate, ok) in [
            (30, true),
            (60, true),
            (24, false),
            (59, false),
            (120, false),
        ] {
            assert_eq!(
                serde_json::from_value::<ClientSettings>(json!({
                    "browserRecordingFrameRate": rate
                }))
                .is_ok(),
                ok
            );
            assert_eq!(
                serde_json::from_value::<ClientSettingsPatch>(json!({
                    "browserRecordingFrameRate": rate
                }))
                .is_ok(),
                ok
            );
        }
    }

    #[test]
    fn recording_input_overlays_default_off_and_accept_opt_ins() {
        let settings = decode_client(json!({}));
        assert!(!settings.browser_recording_show_key_presses);
        assert!(!settings.browser_recording_show_mouse_presses);
        let patch = decode_client_patch(json!({
            "browserRecordingShowKeyPresses": true,
            "browserRecordingShowMousePresses": false
        }));
        assert_eq!(patch.browser_recording_show_key_presses, Some(true));
        assert_eq!(patch.browser_recording_show_mouse_presses, Some(false));
    }

    #[test]
    fn glass_opacity_ranges() {
        assert_eq!(
            decode_client(json!({})).glass_opacity,
            GlassOpacity::new(80).unwrap()
        );
        for (value, ok) in [
            (39, false),
            (101, false),
            (40, true),
            (75, true),
            (100, true),
        ] {
            assert_eq!(
                serde_json::from_value::<ClientSettings>(json!({ "glassOpacity": value })).is_ok(),
                ok
            );
            assert_eq!(
                serde_json::from_value::<ClientSettingsPatch>(json!({ "glassOpacity": value }))
                    .is_ok(),
                ok
            );
        }
        // Upstream rejects 72.5 as a non-integer value.
        assert!(serde_json::from_value::<ClientSettings>(json!({ "glassOpacity": 72.5 })).is_err());
    }

    #[test]
    fn appearance_contrast_ranges() {
        assert_eq!(
            decode_client(json!({})).appearance_contrast,
            AppearanceContrast::new(100).unwrap()
        );
        for (value, ok) in [
            (49, false),
            (201, false),
            (50, true),
            (100, true),
            (150, true),
            (200, true),
        ] {
            assert_eq!(
                serde_json::from_value::<ClientSettings>(json!({ "appearanceContrast": value }))
                    .is_ok(),
                ok
            );
            assert_eq!(
                serde_json::from_value::<ClientSettingsPatch>(json!({
                    "appearanceContrast": value
                }))
                .is_ok(),
                ok
            );
        }
        assert!(
            serde_json::from_value::<ClientSettings>(json!({ "appearanceContrast": 92.5 }))
                .is_err()
        );
    }

    #[test]
    fn panel_animation_duration_ranges() {
        assert_eq!(
            decode_client(json!({})).panel_animation_duration_ms,
            PanelAnimationDurationMs::new(0).unwrap()
        );
        for (value, ok) in [
            (0, true),
            (400, true),
            (-1, false),
            (401, false),
            (150, true),
        ] {
            assert_eq!(
                serde_json::from_value::<ClientSettingsPatch>(json!({
                    "panelAnimationDurationMs": value
                }))
                .is_ok(),
                ok
            );
        }
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "panelAnimationDurationMs": 150.5
            }))
            .is_err()
        );
    }

    #[test]
    fn font_sizes_default_and_range_check() {
        let settings = decode_client(json!({}));
        assert_eq!(
            settings.font_size_interface,
            InterfaceFontSize::new(16).unwrap()
        );
        assert_eq!(settings.font_size_prompt, PromptFontSize::new(14).unwrap());
        assert_eq!(settings.font_size_code, CodeFontSize::new(13).unwrap());
        assert_eq!(
            settings.font_size_terminal,
            TerminalFontSize::new(12).unwrap()
        );
        for (value, ok) in [(11, false), (12, true), (20, true), (21, false)] {
            assert_eq!(
                serde_json::from_value::<ClientSettings>(json!({ "fontSizeInterface": value }))
                    .is_ok(),
                ok
            );
        }
        assert!(serde_json::from_value::<ClientSettings>(json!({ "fontSizeCode": 9 })).is_err());
        assert!(
            serde_json::from_value::<ClientSettings>(json!({ "fontSizeTerminal": 21 })).is_err()
        );
    }

    #[test]
    fn environment_identification_defaults_to_artwork() {
        assert_eq!(
            decode_client(json!({})).environment_identification_mode,
            EnvironmentIdentificationMode::Artwork
        );
        for mode in ["artwork", "pill", "none"] {
            assert_eq!(
                decode_client_patch(json!({ "environmentIdentificationMode": mode }))
                    .environment_identification_mode
                    .map(|m| m.as_str_name()),
                Some(mode)
            );
        }
        assert!(
            serde_json::from_value::<ClientSettings>(json!({
                "environmentIdentificationMode": "badge"
            }))
            .is_err()
        );
    }

    #[test]
    fn sidebar_thread_preview_count_ranges() {
        assert_eq!(
            decode_client(json!({})).sidebar_thread_preview_count,
            SidebarThreadPreviewCount::new(6).unwrap()
        );
        for (value, ok) in [(0, false), (1, true), (15, true), (16, false)] {
            assert_eq!(
                serde_json::from_value::<ClientSettings>(json!({
                    "sidebarThreadPreviewCount": value
                }))
                .is_ok(),
                ok
            );
        }
    }

    #[test]
    fn timestamp_format_round_trips() {
        assert_eq!(
            decode_client(json!({})).timestamp_format,
            TimestampFormat::Locale
        );
        for (format, expected) in [
            ("locale", TimestampFormat::Locale),
            ("12-hour", TimestampFormat::TwelveHour),
            ("24-hour", TimestampFormat::TwentyFourHour),
        ] {
            assert_eq!(
                encode(&decode_client(json!({ "timestampFormat": format })))["timestampFormat"],
                json!(format)
            );
            assert_eq!(
                decode_client_patch(json!({ "timestampFormat": format })).timestamp_format,
                Some(expected)
            );
        }
    }

    // ── ClientSettings pull request merge methods ────────────────

    #[test]
    fn client_pr_merge_method_overrides_default_empty() {
        assert!(
            decode_client(json!({}))
                .pull_request_merge_method_overrides
                .is_empty()
        );
        let patch = decode_client_patch(json!({
            "pullRequestMergeMethodOverrides": { "project": "squash" }
        }));
        assert_eq!(
            patch.pull_request_merge_method_overrides,
            Some(
                [(
                    TrimmedNonEmptyString::new("project").unwrap(),
                    PullRequestMergeMethod::Squash
                )]
                .into_iter()
                .collect()
            )
        );
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({
                "pullRequestMergeMethodOverrides": { "project": "fast-forward" }
            }))
            .is_err()
        );
    }

    // ── ServerSettings thread settlement ─────────────────────────

    #[test]
    fn thread_settlement_defaults_three_days_and_merge_on() {
        let settings = decode_server(json!({}));
        assert_eq!(
            settings.sidebar_auto_settle_after_days,
            Some(SidebarAutoSettleAfterDays::new(3.0).unwrap())
        );
        assert!(settings.sidebar_auto_settle_on_merge);
    }

    #[test]
    fn thread_settlement_allows_disabling_both_rules() {
        let settings = decode_server(json!({
            "sidebarAutoSettleAfterDays": null,
            "sidebarAutoSettleOnMerge": false
        }));
        assert_eq!(settings.sidebar_auto_settle_after_days, None);
        assert!(!settings.sidebar_auto_settle_on_merge);
        let patch = decode_server_patch(json!({
            "sidebarAutoSettleAfterDays": null,
            "sidebarAutoSettleOnMerge": false
        }));
        assert_eq!(patch.sidebar_auto_settle_after_days, Some(None));
        assert_eq!(patch.sidebar_auto_settle_on_merge, Some(false));
    }

    #[test]
    fn thread_settlement_rejects_out_of_range_thresholds() {
        for value in [json!(-1), json!(0), json!(91)] {
            assert!(
                serde_json::from_value::<ServerSettings>(json!({
                    "sidebarAutoSettleAfterDays": value
                }))
                .is_err()
            );
            assert!(
                serde_json::from_value::<ServerSettingsPatch>(json!({
                    "sidebarAutoSettleAfterDays": value
                }))
                .is_err()
            );
        }
    }

    // ── ServerSettings.providerInstances ────────────────────────

    #[test]
    fn provider_instances_default_to_empty_map() {
        assert!(default_server_settings().provider_instances.is_empty());
        assert!(decode_server(json!({})).provider_instances.is_empty());
    }

    #[test]
    fn provider_instances_decode_multi_instance_maps_with_fork_drivers() {
        let decoded = decode_server(json!({
            "providerInstances": {
                "codex_personal": {
                    "driver": "codex",
                    "displayName": "Codex (personal)",
                    "config": { "homePath": "~/.codex_personal" }
                },
                "codex_work": {
                    "driver": "codex",
                    "config": { "homePath": "~/.codex_work" }
                },
                "ollama_local": {
                    "driver": "ollama",
                    "displayName": "Ollama (local)",
                    "config": { "endpoint": "http://localhost:11434" }
                }
            }
        }));
        let instances = &decoded.provider_instances;
        assert_eq!(
            instances
                .get(&provider_instance_id("codex_personal"))
                .unwrap()
                .driver
                .as_str(),
            "codex"
        );
        assert_eq!(
            instances
                .get(&provider_instance_id("codex_work"))
                .unwrap()
                .config,
            Some(json!({ "homePath": "~/.codex_work" }))
        );
        // Critical: a config naming a driver this build does not know about
        // must round-trip without loss. The runtime handles "driver not
        // installed" — the schema must not.
        let ollama = instances
            .get(&provider_instance_id("ollama_local"))
            .unwrap();
        assert_eq!(ollama.driver.as_str(), "ollama");
        assert_eq!(
            ollama.config,
            Some(json!({ "endpoint": "http://localhost:11434" }))
        );
    }

    #[test]
    fn provider_instances_reject_invalid_slug_keys() {
        assert!(
            serde_json::from_value::<ServerSettings>(json!({
                "providerInstances": { "1bad": { "driver": "codex" } }
            }))
            .is_err()
        );
    }

    // ── provider enabled defaults ────────────────────────────────

    #[test]
    fn provider_enabled_keeps_muse_disabled_until_configured() {
        let muse = provider_driver("muse");
        assert!(!resolve_provider_instance_enabled(&config(
            "muse",
            None,
            json!({})
        )));
        assert!(resolve_provider_instance_enabled(&config(
            "muse",
            Some(true),
            json!({})
        )));
        assert!(!resolve_provider_instance_enabled(&config(
            "muse",
            Some(true),
            json!({ "enabled": false })
        )));
        assert_eq!(muse.as_str(), "muse");
    }

    #[test]
    fn provider_enabled_enables_only_stable_bindings_by_default() {
        let enabled_by_default =
            |driver: &str| resolve_provider_instance_enabled(&config(driver, None, json!({})));
        assert!(enabled_by_default("codex"));
        assert!(enabled_by_default("claudeAgent"));
        for driver in ["cursor", "grok", "muse", "pi", "opencode", "antigravity"] {
            assert!(!enabled_by_default(driver), "{driver} should default off");
        }
    }

    #[test]
    fn provider_enabled_resolves_with_explicit_false_winning() {
        // No flags anywhere: driver default applies.
        assert!(!resolve_provider_instance_enabled(&config(
            "grok",
            None,
            json!({})
        )));
        assert!(resolve_provider_instance_enabled(&config(
            "codex",
            None,
            json!({})
        )));
        // Unknown fork drivers stay enabled.
        assert!(resolve_provider_instance_enabled(&config(
            "ollama",
            None,
            json!({})
        )));
        // Envelope flag wins over the driver default.
        assert!(resolve_provider_instance_enabled(&config(
            "grok",
            Some(true),
            json!({})
        )));
        assert!(!resolve_provider_instance_enabled(&config(
            "codex",
            Some(false),
            json!({})
        )));
        // Legacy in-config flag fills in when the envelope is silent.
        assert!(resolve_provider_instance_enabled(&config(
            "grok",
            None,
            json!({ "enabled": true })
        )));
        // Conflicting flags: the explicit false wins, whichever side it is on.
        assert!(!resolve_provider_instance_enabled(&config(
            "grok",
            Some(true),
            json!({ "enabled": false })
        )));
        assert!(!resolve_provider_instance_enabled(&config(
            "codex",
            Some(false),
            json!({ "enabled": true })
        )));
    }

    #[test]
    fn provider_instance_config_enabled_flag_reads_legacy_blobs() {
        assert_eq!(provider_instance_config_enabled_flag(None), None);
        assert_eq!(
            provider_instance_config_enabled_flag(Some(&json!(null))),
            None
        );
        assert_eq!(
            provider_instance_config_enabled_flag(Some(&json!([true]))),
            None
        );
        assert_eq!(
            provider_instance_config_enabled_flag(Some(&json!({ "enabled": true }))),
            Some(true)
        );
        assert_eq!(
            provider_instance_config_enabled_flag(Some(&json!({ "enabled": "yes" }))),
            None
        );
    }

    // ── ServerSettings worktree defaults ─────────────────────────

    #[test]
    fn thread_env_mode_defaults_to_inherit_and_keeps_stored_values() {
        assert_eq!(decode_server(json!({})).default_thread_env_mode, None);
        assert_eq!(
            decode_server(json!({ "defaultThreadEnvMode": "worktree" })).default_thread_env_mode,
            Some(ThreadEnvMode::Worktree)
        );
        // A kind a newer server may add decodes as inherit, not failure.
        assert_eq!(
            decode_server(json!({ "defaultThreadEnvMode": "remote" })).default_thread_env_mode,
            None
        );
        assert_eq!(
            decode_server_patch(json!({ "defaultThreadEnvMode": null })).default_thread_env_mode,
            Some(None)
        );
    }

    #[test]
    fn inherited_thread_env_mode_stays_off_the_wire() {
        let encoded = encode(&decode_server(json!({})));
        assert!(encoded.get("defaultThreadEnvMode").is_none());
        let encoded = encode(&decode_server(
            json!({ "defaultThreadEnvMode": "worktree" }),
        ));
        assert_eq!(encoded["defaultThreadEnvMode"], json!("worktree"));
    }

    #[test]
    fn worktrees_start_from_origin_by_default() {
        assert!(decode_server(json!({})).new_worktrees_start_from_origin);
        assert_eq!(
            decode_server_patch(json!({ "newWorktreesStartFromOrigin": false }))
                .new_worktrees_start_from_origin,
            Some(false)
        );
    }

    #[test]
    fn worktree_submodules_default_to_inherit_and_tolerate_unknown_modes() {
        assert_eq!(decode_server(json!({})).worktree_submodules, None);
        assert_eq!(
            decode_server(json!({ "worktreeSubmodules": "top-level" })).worktree_submodules,
            Some(WorktreeSubmodules::TopLevel)
        );
        assert_eq!(
            decode_server(json!({ "worktreeSubmodules": "shallow" })).worktree_submodules,
            None
        );
        assert_eq!(
            decode_server_patch(json!({ "worktreeSubmodules": null })).worktree_submodules,
            Some(None)
        );
    }

    // ── ServerSettings.sourceControlWritingStyle ─────────────────

    #[test]
    fn writing_style_defaults_for_legacy_configs() {
        let settings = decode_server(json!({}));
        assert_eq!(
            settings.source_control_writing_style,
            SourceControlWritingStyleSettings {
                mode: SourceControlWritingStyleMode::RepoConventions,
                custom_instructions: TrimmedString::new(""),
                follow_change_request_templates: true,
            }
        );
        assert_eq!(settings.source_control_writer_model_selection, None);
    }

    #[test]
    fn writing_style_patch_trims_partial_updates() {
        let patch = decode_server_patch(json!({
            "sourceControlWritingStyle": {
                "mode": "custom",
                "customInstructions": "  Prefer concise wording.  "
            }
        }));
        assert_eq!(
            patch.source_control_writing_style,
            Some(SourceControlWritingStyleSettingsPatch {
                mode: Some(SourceControlWritingStyleMode::Custom),
                custom_instructions: Some(TrimmedString::new("Prefer concise wording.")),
                follow_change_request_templates: None,
            })
        );
    }

    // ── ServerSettingsPatch.providerInstances ────────────────────

    #[test]
    fn patch_provider_instances_is_a_whole_map_replacement() {
        assert_eq!(decode_server_patch(json!({})).provider_instances, None);
        let replacement = decode_server_patch(json!({
            "providerInstances": {
                "codex_personal": { "driver": "codex", "config": { "homePath": "~/.codex" } }
            }
        }));
        assert_eq!(
            replacement
                .provider_instances
                .as_ref()
                .unwrap()
                .get(&provider_instance_id("codex_personal"))
                .unwrap()
                .driver
                .as_str(),
            "codex"
        );
        // Fork-defined driver entries survive patch decoding.
        let fork = decode_server_patch(json!({
            "providerInstances": {
                "ollama_local": {
                    "driver": "ollama",
                    "config": { "endpoint": "http://localhost:11434" }
                }
            }
        }));
        assert_eq!(
            fork.provider_instances
                .as_ref()
                .unwrap()
                .get(&provider_instance_id("ollama_local"))
                .unwrap()
                .driver
                .as_str(),
            "ollama"
        );
    }

    // ── ServerSettingsPatch string normalization ─────────────────

    #[test]
    fn patch_trims_string_settings_while_decoding() {
        let patch = decode_server_patch(json!({
            "addProjectBaseDirectory": "  ~/Development  ",
            "textGenerationModelSelection": { "model": "  gpt-5.4-mini  " },
            "observability": { "otlpTracesUrl": "  http://localhost:4318/v1/traces  " },
            "providerInstances": {
                "codex_personal": {
                    "driver": "  codex  ",
                    "displayName": "  Codex Personal  ",
                    "config": { "homePath": "  ~/.codex-personal  " }
                }
            }
        }));
        assert_eq!(
            patch.add_project_base_directory.unwrap().as_str(),
            "~/Development"
        );
        assert_eq!(
            patch
                .text_generation_model_selection
                .unwrap()
                .model
                .unwrap()
                .as_str(),
            "gpt-5.4-mini"
        );
        assert_eq!(
            patch
                .observability
                .unwrap()
                .otlp_traces_url
                .unwrap()
                .as_str(),
            "http://localhost:4318/v1/traces"
        );
        let instances = patch.provider_instances.unwrap();
        let instance = instances
            .get(&provider_instance_id("codex_personal"))
            .unwrap();
        assert_eq!(instance.driver.as_str(), "codex");
        assert_eq!(
            instance.display_name.as_ref().unwrap().as_str(),
            "Codex Personal"
        );
        // The driver-specific config blob stays opaque: no trimming inside it.
        assert_eq!(
            instance.config,
            Some(json!({ "homePath": "  ~/.codex-personal  " }))
        );
    }

    #[test]
    fn encode_trims_server_settings_values_before_validation() {
        let settings = decode_server(json!({
            "addProjectBaseDirectory": "  ~/Development  "
        }));
        assert_eq!(
            encode(&settings)["addProjectBaseDirectory"],
            json!("~/Development")
        );
    }

    // ── ServerSettings environment icon ─────────────────────────

    #[test]
    fn environment_icon_defaults_to_null() {
        assert_eq!(decode_server(json!({})).environment_icon, None);
        assert_eq!(
            decode_server(json!({})).environment_icon,
            None::<EnvironmentMachineKind>
        );
    }

    #[test]
    fn environment_icon_keeps_known_kinds() {
        assert_eq!(
            decode_server(json!({ "environmentIcon": "mac-mini" })).environment_icon,
            Some(EnvironmentMachineKind::MacMini)
        );
        assert_eq!(
            decode_server(json!({ "environmentIcon": "linux" })).environment_icon,
            Some(EnvironmentMachineKind::Linux)
        );
    }

    #[test]
    fn environment_icon_decodes_newer_kinds_as_null() {
        assert_eq!(
            decode_server(json!({ "environmentIcon": "toaster" })).environment_icon,
            None
        );
    }

    #[test]
    fn environment_icon_round_trips_through_encode() {
        for kind in ["laptop", "linux"] {
            let settings = decode_server(json!({ "environmentIcon": kind }));
            assert_eq!(encode(&settings)["environmentIcon"], json!(kind));
        }
        // Null (the default) stays on the wire: the field is required upstream.
        assert_eq!(
            encode(&decode_server(json!({})))["environmentIcon"],
            json!(null)
        );
    }

    // ── branch naming settings ───────────────────────────────────

    #[test]
    fn branch_naming_defaults_to_the_t3_static_prefix() {
        let settings = decode_server(json!({}));
        assert_eq!(settings.branch_naming_mode, BranchNamingMode::Static);
        assert_eq!(settings.branch_name_prefix.as_str(), "t3");
        assert_eq!(settings.branch_name_instructions.as_str(), "");
    }

    #[test]
    fn branch_naming_round_trips_with_project_overrides() {
        for mode in ["static", "semantic", "custom"] {
            let input = json!({
                "branchNamingMode": mode,
                "branchNamePrefix": "team/",
                "branchNameInstructions": "Include the issue ID.",
                "projectSettingsOverrides": {
                    "project": {
                        "branchNamingMode": mode,
                        "branchNamePrefix": "team/",
                        "branchNameInstructions": "Include the issue ID."
                    }
                }
            });
            let encoded = encode(&decode_server(input.clone()));
            assert_eq!(encoded["branchNamingMode"], json!(mode));
            assert_eq!(encoded["branchNamePrefix"], json!("team/"));
            let patch = decode_server_patch(input);
            assert_eq!(
                patch.branch_naming_mode.map(|m| m.as_str_name()),
                Some(mode)
            );
            assert_eq!(
                patch.branch_name_prefix.map(|p| p.as_str().to_owned()),
                Some("team/".to_owned())
            );
        }
    }

    #[test]
    fn remove_agent_credits_on_merge_defaults_false() {
        assert!(!decode_server(json!({})).remove_agent_credits_on_merge);
        assert_eq!(
            decode_server_patch(json!({ "removeAgentCreditsOnMerge": true }))
                .remove_agent_credits_on_merge,
            Some(true)
        );
        let settings = decode_server(json!({
            "projectSettingsOverrides": { "project": { "removeAgentCreditsOnMerge": true } }
        }));
        assert_eq!(
            settings
                .project_settings_overrides
                .get(&project_id("project"))
                .unwrap()
                .remove_agent_credits_on_merge,
            Some(true)
        );
    }

    // ── project-scoped keys ──────────────────────────────────────

    #[test]
    fn nullable_project_settings_overrides_match_upstream() {
        for key in [
            "defaultModelSelection",
            "sourceControlWriterModelSelection",
            "reviewGuideModelSelection",
            "pullRequestMergeMethod",
            "sidebarAutoSettleAfterDays",
        ] {
            assert!(is_nullable_project_settings_override(key), "{key}");
        }
        for key in [
            "worktreeCleanup",
            "defaultRuntimeMode",
            "responseStreamingMode",
        ] {
            assert!(!is_nullable_project_settings_override(key), "{key}");
        }
        assert_eq!(PROJECT_SCOPED_SERVER_SETTING_KEYS.len(), 25);
        for key in [
            "worktreeCleanup",
            "defaultModelSelection",
            "responseStreamingMode",
        ] {
            assert!(PROJECT_SCOPED_SERVER_SETTING_KEYS.contains(&key));
        }
    }

    // ── resolveProjectProviderInstanceEnabled ────────────────────

    fn overrides_for(
        instances: Option<IndexMap<ProviderInstanceId, ProjectProviderInstanceOverride>>,
    ) -> ProjectSettingsOverridesMap {
        [(
            project_id("project-a"),
            ProjectSettingsOverrides {
                provider_instances: instances,
                ..Default::default()
            },
        )]
        .into_iter()
        .collect()
    }

    #[test]
    fn project_provider_enabled_inherits_machine_value_without_an_override() {
        let instances = [(
            provider_instance_id("codex_work"),
            config("codex", Some(false), json!({})),
        )]
        .into_iter()
        .collect::<ProviderInstanceConfigMap>();
        let overrides = overrides_for(None);
        assert!(!resolve_project_provider_instance_enabled(
            &instances,
            &overrides,
            Some(&project_id("project-a")),
            &provider_instance_id("codex_work"),
        ));
    }

    #[test]
    fn project_provider_enabled_can_enable_a_machine_disabled_instance() {
        let instances = [(
            provider_instance_id("codex_work"),
            config("codex", Some(false), json!({})),
        )]
        .into_iter()
        .collect::<ProviderInstanceConfigMap>();
        let overrides = overrides_for(Some(
            [(
                provider_instance_id("codex_work"),
                ProjectProviderInstanceOverride {
                    enabled: Some(true),
                },
            )]
            .into_iter()
            .collect(),
        ));
        assert!(resolve_project_provider_instance_enabled(
            &instances,
            &overrides,
            Some(&project_id("project-a")),
            &provider_instance_id("codex_work"),
        ));
    }

    #[test]
    fn project_provider_enabled_can_disable_a_machine_enabled_instance() {
        let instances = [(
            provider_instance_id("codex_work"),
            config("codex", Some(true), json!({})),
        )]
        .into_iter()
        .collect::<ProviderInstanceConfigMap>();
        let overrides = overrides_for(Some(
            [(
                provider_instance_id("codex_work"),
                ProjectProviderInstanceOverride {
                    enabled: Some(false),
                },
            )]
            .into_iter()
            .collect(),
        ));
        assert!(!resolve_project_provider_instance_enabled(
            &instances,
            &overrides,
            Some(&project_id("project-a")),
            &provider_instance_id("codex_work"),
        ));
    }

    #[test]
    fn project_provider_enabled_uses_machine_value_without_a_project() {
        let instances = [(
            provider_instance_id("codex_work"),
            config("codex", Some(true), json!({})),
        )]
        .into_iter()
        .collect::<ProviderInstanceConfigMap>();
        let overrides = overrides_for(Some(
            [(
                provider_instance_id("codex_work"),
                ProjectProviderInstanceOverride {
                    enabled: Some(false),
                },
            )]
            .into_iter()
            .collect(),
        ));
        assert!(resolve_project_provider_instance_enabled(
            &instances,
            &overrides,
            None,
            &provider_instance_id("codex_work"),
        ));
    }

    #[test]
    fn project_provider_enabled_falls_back_to_the_unconfigured_default() {
        let instances = ProviderInstanceConfigMap::new();
        let overrides = overrides_for(None);
        // `codex_work` has no settings entry, so with no project override
        // either, the project lookup and the machine-wide lookup agree on the
        // unconfigured-default fallback.
        let with_project = resolve_project_provider_instance_enabled(
            &instances,
            &overrides,
            Some(&project_id("project-a")),
            &provider_instance_id("codex_work"),
        );
        let without_project = resolve_project_provider_instance_enabled(
            &instances,
            &overrides,
            None,
            &provider_instance_id("codex_work"),
        );
        assert_eq!(with_project, without_project);
        assert!(!with_project);

        let overridden = overrides_for(Some(
            [(
                provider_instance_id("codex_work"),
                ProjectProviderInstanceOverride {
                    enabled: Some(true),
                },
            )]
            .into_iter()
            .collect(),
        ));
        assert!(resolve_project_provider_instance_enabled(
            &instances,
            &overridden,
            Some(&project_id("project-a")),
            &provider_instance_id("codex_work"),
        ));
    }

    #[test]
    fn unconfigured_default_instances_are_codex_and_claude() {
        assert!(is_unconfigured_default_instance_enabled(
            &provider_instance_id("codex")
        ));
        assert!(is_unconfigured_default_instance_enabled(
            &provider_instance_id("claudeAgent")
        ));
        assert!(!is_unconfigured_default_instance_enabled(
            &provider_instance_id("cursor")
        ));
        assert!(!is_unconfigured_default_instance_enabled(
            &provider_instance_id("codex_work")
        ));
    }

    // ── server model preferences ────────────────────────────────

    #[test]
    fn model_preferences_visibility_patch_round_trips() {
        let input = json!({
            "setModelsHidden": [{ "provider": "codex_work", "model": "sol", "hidden": false }]
        });
        let patch = decode_server_patch(input.clone());
        assert_eq!(
            patch.set_models_hidden,
            Some(vec![SetModelHidden {
                provider: provider_instance_id("codex_work"),
                model: TrimmedNonEmptyString::new("sol").unwrap(),
                hidden: false,
            }])
        );
        assert_eq!(encode(&patch), input);
        // The flag is required.
        assert!(
            serde_json::from_value::<ServerSettingsPatch>(json!({
                "setModelsHidden": [{ "provider": "codex_work", "model": "sol" }]
            }))
            .is_err()
        );
    }

    #[test]
    fn model_preferences_distinguish_unmigrated_from_cleared() {
        let settings = decode_server(json!({}));
        assert_eq!(settings.favorites, None);
        assert_eq!(settings.provider_model_preferences, None);
        let input = json!({ "favorites": [], "providerModelPreferences": {} });
        let encoded = encode(&decode_server(input.clone()));
        assert_eq!(encoded["favorites"], json!([]));
        assert_eq!(encoded["providerModelPreferences"], json!({}));
        assert_eq!(
            decode_server_patch(input.clone()),
            ServerSettingsPatch {
                favorites: Some(Vec::new()),
                provider_model_preferences: Some(ProviderModelPreferences::default()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn model_preferences_round_trip_by_provider_instance() {
        let input = json!({
            "favorites": [{ "provider": "codex_work", "model": "sol" }],
            "providerModelPreferences": {
                "codex_work": { "hiddenModels": ["astra"], "modelOrder": ["sol"] }
            }
        });
        let settings = decode_server(input.clone());
        assert_eq!(
            settings.favorites,
            Some(vec![ModelFavorite {
                provider: provider_instance_id("codex_work"),
                model: TrimmedNonEmptyString::new("sol").unwrap(),
            }])
        );
        assert_eq!(
            settings.provider_model_preferences,
            Some(
                [(
                    provider_instance_id("codex_work"),
                    ProviderModelPreferencesEntry {
                        hidden_models: vec!["astra".to_owned()],
                        model_order: vec!["sol".to_owned()],
                    }
                )]
                .into_iter()
                .collect()
            )
        );
        let encoded = encode(&settings);
        assert_eq!(
            encoded["favorites"],
            json!([{ "provider": "codex_work", "model": "sol" }])
        );
        assert_eq!(
            encoded["providerModelPreferences"],
            json!({ "codex_work": { "hiddenModels": ["astra"], "modelOrder": ["sol"] } })
        );
        assert_eq!(
            decode_server_patch(input.clone()).favorites,
            Some(vec![ModelFavorite {
                provider: provider_instance_id("codex_work"),
                model: TrimmedNonEmptyString::new("sol").unwrap(),
            }])
        );
        // Applied under the settings service's write lock, only while unset.
        let migrate = decode_server_patch(json!({ "migrateModelPreferences": input }));
        assert_eq!(
            migrate.migrate_model_preferences,
            Some(MigrateModelPreferences {
                favorites: Some(vec![ModelFavorite {
                    provider: provider_instance_id("codex_work"),
                    model: TrimmedNonEmptyString::new("sol").unwrap(),
                }]),
                provider_model_preferences: Some(
                    [(
                        provider_instance_id("codex_work"),
                        ProviderModelPreferencesEntry {
                            hidden_models: vec!["astra".to_owned()],
                            model_order: vec!["sol".to_owned()],
                        }
                    )]
                    .into_iter()
                    .collect()
                ),
            })
        );
    }

    // ── default server settings snapshot ─────────────────────────

    #[test]
    fn default_server_settings_decode_upstream_defaults() {
        let settings = decode_server(json!({}));
        assert_eq!(settings, default_server_settings());
        assert_eq!(
            settings.text_generation_model_selection,
            ModelSelection {
                instance_id: provider_instance_id("codex"),
                model: TrimmedNonEmptyString::new(DEFAULT_TEXT_GENERATION_MODEL).unwrap(),
                options: Some(vec![ProviderOptionSelection {
                    id: TrimmedNonEmptyString::new("reasoningEffort").unwrap(),
                    value: ProviderOptionSelectionValue::Str(
                        TrimmedNonEmptyString::new(DEFAULT_TEXT_GENERATION_REASONING_EFFORT)
                            .unwrap()
                    ),
                }]),
            }
        );
        assert!(!settings.default_auto_pull);
        assert!(settings.enable_provider_update_checks);
        assert!(settings.continue_threads_after_server_update);
        assert!(!settings.project_settings_folded);
        assert_eq!(settings.automatic_git_fetch_interval, 30_000);
        assert_eq!(settings.provider_health_refresh_interval, 300_000);
        assert_eq!(
            settings.background_activity_profile,
            BackgroundActivityProfile::Balanced
        );
        assert_eq!(
            settings.background_activity,
            BackgroundActivitySettings::default()
        );
        assert_eq!(settings.worktrees_directory.as_str(), "");
        assert_eq!(settings.previous_worktrees_directories, Vec::new());
        assert_eq!(settings.branch_name_prefix.as_str(), "t3");
        assert!(settings.provider_instances.is_empty());
        assert_eq!(
            settings.usage_price_overrides,
            IndexMap::<TrimmedNonEmptyString, UsageModelPriceOverride>::new()
        );
    }

    #[test]
    fn default_server_settings_encodes_upstream_shape() {
        let encoded = encode(&decode_server(json!({})));
        // Required keys with null/empty defaults are always written, like
        // upstream's encoder.
        for key in [
            "favorites",
            "providerModelPreferences",
            "worktreeCleanup",
            "defaultModelSelection",
            "sourceControlWriterModelSelection",
            "reviewGuideModelSelection",
            "pullRequestMergeMethod",
            "environmentIcon",
            "worktreeSubmodules",
            "onboardingCompletedAt",
        ] {
            if key == "onboardingCompletedAt" {
                continue;
            }
            assert_eq!(encoded[key], json!(null), "{key} should encode as null");
        }
        // OmittedWhenNull: an inherited thread env mode never crosses the wire.
        assert!(encoded.get("defaultThreadEnvMode").is_none());
        assert_eq!(encoded["responseStreamingMode"], json!("paragraph"));
        assert_eq!(encoded["defaultRuntimeMode"], json!("full-access"));
        assert_eq!(encoded["providerInstances"], json!({}));
        assert_eq!(
            encoded["textGenerationModelSelection"],
            json!({
                "instanceId": "codex",
                "model": DEFAULT_TEXT_GENERATION_MODEL,
                "options": [{ "id": "reasoningEffort", "value": DEFAULT_TEXT_GENERATION_REASONING_EFFORT }]
            })
        );
        assert_eq!(
            encoded["backgroundActivity"],
            json!({
                "schemaVersion": 1,
                "profile": "balanced",
                "overrides": {}
            })
        );
        assert_eq!(encoded["automaticGitFetchInterval"], json!(30000));
        assert_eq!(encoded["branchNamingMode"], json!("static"));
        assert_eq!(encoded["branchNamePrefix"], json!("t3"));
    }

    #[test]
    fn default_client_settings_encode_writes_every_field() {
        let encoded = encode(&decode_client(json!({})));
        assert_eq!(encoded["confirmQuit"], json!("hold"));
        assert_eq!(encoded["favorites"], json!([]));
        assert_eq!(encoded["onboardingCompletedAt"], json!(null));
        assert_eq!(encoded["notificationMode"], json!("off"));
        assert_eq!(
            encoded["snapShotShortcut"],
            json!({ "kind": "both-shift-keys" })
        );
        assert_eq!(encoded["sidebarProjectSortOrder"], json!("updated_at"));
    }

    // ── server settings error message ────────────────────────────

    #[test]
    fn server_settings_error_message_matches_upstream() {
        let error = ServerSettingsError::ServerSettingsError {
            settings_path: "/tmp/settings.json".to_owned(),
            operation: ServerSettingsOperation::WriteSecret,
            provider_instance_id: Some("codex".to_owned()),
            environment_variable: Some("OPENAI_API_KEY".to_owned()),
            cause: None,
        };
        assert_eq!(
            error.to_string(),
            "Server settings write-secret failed for provider codex and environment variable OPENAI_API_KEY at /tmp/settings.json."
        );
        let bare = ServerSettingsError::ServerSettingsError {
            settings_path: "/tmp/settings.json".to_owned(),
            operation: ServerSettingsOperation::Normalize,
            provider_instance_id: None,
            environment_variable: None,
            cause: None,
        };
        assert_eq!(
            bare.to_string(),
            "Server settings normalize failed at /tmp/settings.json."
        );
        // TaggedError wire shape.
        assert_eq!(
            encode(&bare),
            json!({
                "_tag": "ServerSettingsError",
                "settingsPath": "/tmp/settings.json",
                "operation": "normalize"
            })
        );
    }

    // ── unified settings ─────────────────────────────────────────

    #[test]
    fn unified_settings_composes_both_defaults() {
        let unified = default_unified_settings();
        assert_eq!(unified.server, default_server_settings());
        assert_eq!(unified.client, default_client_settings());
    }

    // ── real settings files round-trip ──────────────────────────

    // Shape captured from a real upstream server settings file.
    const REAL_SERVER_SETTINGS: &str = r#"{
        "defaultModelSelection": { "instanceId": "claudeAgent", "model": "claude-opus-5-5" },
        "projectSettingsFolded": true,
        "providerInstances": {
            "codex": {
                "driver": "codex",
                "enabled": true,
                "config": {
                    "binaryPath": "codex",
                    "homePath": "",
                    "shadowHomePath": "",
                    "launchArgs": "",
                    "customModels": [],
                    "setupMode": "managed"
                }
            },
            "claudeAgent_2": { "driver": "claudeAgent", "displayName": "Claude", "enabled": false },
            "claudeAgent": {
                "driver": "claudeAgent",
                "enabled": true,
                "config": {
                    "binaryPath": "claude",
                    "homePath": "",
                    "customModels": [],
                    "launchArgs": "",
                    "autoCompactWindow": ""
                }
            },
            "cursor": { "driver": "cursor", "displayName": "Cursor", "enabled": true },
            "opencode": {
                "driver": "opencode",
                "displayName": "OpenCode",
                "enabled": true,
                "config": { "binaryPath": "opencode" }
            }
        },
        "cursorKeychainUsageEnabled": true
    }"#;

    #[test]
    fn real_server_settings_file_round_trips() {
        let settings: ServerSettings = serde_json::from_str(REAL_SERVER_SETTINGS).unwrap();
        assert!(settings.project_settings_folded);
        assert!(settings.cursor_keychain_usage_enabled);
        assert_eq!(
            settings.default_model_selection,
            Some(ModelSelection {
                instance_id: provider_instance_id("claudeAgent"),
                model: TrimmedNonEmptyString::new("claude-opus-5-5").unwrap(),
                options: None,
            })
        );
        let encoded = encode(&settings);
        // Keys present in the file come back unchanged.
        assert_eq!(encoded["projectSettingsFolded"], json!(true));
        assert_eq!(encoded["cursorKeychainUsageEnabled"], json!(true));
        assert_eq!(
            encoded["defaultModelSelection"],
            json!({ "instanceId": "claudeAgent", "model": "claude-opus-5-5" })
        );
        assert_eq!(
            encoded["providerInstances"]["codex"],
            json!({
                "driver": "codex",
                "enabled": true,
                "config": {
                    "binaryPath": "codex",
                    "homePath": "",
                    "shadowHomePath": "",
                    "launchArgs": "",
                    "customModels": [],
                    "setupMode": "managed"
                }
            })
        );
        assert_eq!(
            encoded["providerInstances"]["claudeAgent_2"],
            json!({
                "driver": "claudeAgent", "displayName": "Claude", "enabled": false
            })
        );
        // The saved file's key order within providerInstances is preserved.
        let keys: Vec<&str> = encoded["providerInstances"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "codex",
                "claudeAgent_2",
                "claudeAgent",
                "cursor",
                "opencode"
            ]
        );
        // ...and decoding the re-encoded file still succeeds (round trip).
        let redecoded: ServerSettings = serde_json::from_value(encoded).unwrap();
        assert_eq!(redecoded, settings);
    }

    // Shape captured from a real upstream client settings file. The cut
    // preview/browser-profile keys decode away; every kept key round-trips.
    const REAL_CLIENT_SETTINGS: &str = r#"{
        "notificationMode": "notifications-and-sound",
        "inAppNotificationsEnabled": false,
        "diffColorScheme": "red-green",
        "chatWidth": "comfortable",
        "loadBalancingEnabled": false,
        "loadBalancingWeights": {},
        "appearanceContrast": 125,
        "panelAnimationDurationMs": 25,
        "browserDefaultViewport": { "_tag": "fill" },
        "browserDefaultZoomFactor": 1,
        "browserDefaultAppearance": "system",
        "browserRecordingFrameRate": 30,
        "browserRecordingShowKeyPresses": false,
        "browserRecordingShowMousePresses": false,
        "browserLinkTarget": "system",
        "browserAutoShowFloatingPreview": true,
        "browserProfiles": [],
        "browserDefaultProfileId": "default",
        "confirmQuit": "hold",
        "confirmThreadArchive": false,
        "confirmThreadDelete": true,
        "confirmThreadUnpin": false,
        "dismissedProviderUpdateNotificationKeys": [],
        "diffFilesCollapsed": true,
        "diffIgnoreWhitespace": true,
        "diffLayout": "stacked",
        "environmentIdentificationMode": "artwork",
        "glassOpacity": 60,
        "fontSizeInterface": 16,
        "fontSizePrompt": 14,
        "fontSizeCode": 13,
        "fontSizeTerminal": 12,
        "fontFamilyCode": "",
        "fontFamilyComposer": "",
        "fontFamilySans": "",
        "fontFamilyTerminal": "",
        "fontSmoothing": true,
        "onboardingCompletedAt": "2026-10-05T12:11:21.930Z",
        "persistComposerContextStrip": false,
        "favorites": [],
        "providerModelPreferences": {},
        "pullRequestMergeMethodOverrides": {},
        "planModeEnabled": false,
        "contextWindowMeterEnabled": false,
        "composerCollapseOnScroll": true,
        "composerRichTextEnabled": true,
        "sendShortcut": "enter",
        "followUpBehavior": "steer",
        "proactivePanelsEnabled": true,
        "showSkillsInSlashMenu": true,
        "legacySidebarEnabled": false,
        "sidebarWorkingShelfEnabled": true,
        "sidebarProjectGroupingMode": "repository",
        "sidebarProjectGroupingOverrides": {},
        "sidebarProjectSortOrder": "updated_at",
        "sidebarThreadSortOrder": "updated_at",
        "sidebarThreadPreviewCount": 6,
        "timestampFormat": "locale",
        "snapShotEnabled": false,
        "snapShotIncludeAccessibility": true,
        "snapShotShortcut": { "kind": "both-shift-keys" },
        "snapShotPlaySound": true,
        "snapShotSound": "soft-pop",
        "snapShotFlash": true,
        "snapShotAnimations": true,
        "wordWrap": true
    }"#;

    #[test]
    fn real_client_settings_file_decodes_and_round_trips_kept_keys() {
        let settings: ClientSettings = serde_json::from_str(REAL_CLIENT_SETTINGS).unwrap();
        assert_eq!(
            settings.notification_mode,
            NotificationMode::NotificationsAndSound
        );
        assert_eq!(
            settings.appearance_contrast,
            AppearanceContrast::new(125).unwrap()
        );
        assert_eq!(
            settings.panel_animation_duration_ms,
            PanelAnimationDurationMs::new(25).unwrap()
        );
        assert_eq!(settings.glass_opacity, GlassOpacity::new(60).unwrap());
        assert_eq!(settings.follow_up_behavior, FollowUpBehavior::Steer);
        assert!(settings.proactive_panels_enabled);
        assert!(settings.sidebar_working_shelf_enabled);
        assert_eq!(
            settings.onboarding_completed_at,
            Some("2026-10-05T12:11:21.930Z".to_owned())
        );
        assert_eq!(settings.confirm_quit, QuitConfirmationMode::Hold);
        let encoded = encode(&settings);
        for (key, value) in [
            ("notificationMode", json!("notifications-and-sound")),
            ("appearanceContrast", json!(125)),
            ("panelAnimationDurationMs", json!(25)),
            ("glassOpacity", json!(60)),
            ("followUpBehavior", json!("steer")),
            ("proactivePanelsEnabled", json!(true)),
            ("sidebarWorkingShelfEnabled", json!(true)),
            ("onboardingCompletedAt", json!("2026-10-05T12:11:21.930Z")),
            ("confirmQuit", json!("hold")),
        ] {
            assert_eq!(encoded[key], value, "{key} must round-trip");
        }
        // The re-encoded file still decodes.
        let redecoded: ClientSettings = serde_json::from_value(encoded).unwrap();
        assert_eq!(redecoded, settings);
    }

    // ── usage limit sources and model aliases maps ───────────────

    #[test]
    fn usage_limit_sources_decode_and_round_trip() {
        let settings = decode_server(json!({
            "usageLimitSources": {
                "hub": {
                    "kind": "cliproxy",
                    "label": "CLIProxy hub",
                    "url": "http://localhost:8787",
                    "managementKey": "secret",
                    "enabled": false
                }
            }
        }));
        let source = settings
            .usage_limit_sources
            .get(&UsageLimitSourceId::new("hub"))
            .unwrap();
        assert_eq!(source.url.as_str(), "http://localhost:8787");
        assert_eq!(source.management_key.as_str(), "secret");
        assert!(!source.enabled);
        assert_eq!(
            source.label,
            Some(TrimmedNonEmptyString::new("CLIProxy hub").unwrap())
        );
        // Default label omitted, default enabled true.
        let minimal = decode_server(json!({
            "usageLimitSources": { "bare": { "kind": "cliproxy", "url": "http://x" } }
        }));
        let bare = minimal
            .usage_limit_sources
            .get(&UsageLimitSourceId::new("bare"))
            .unwrap();
        assert_eq!(bare.label, None);
        assert!(bare.enabled);
        assert_eq!(
            encode(&minimal)["usageLimitSources"],
            json!({ "bare": { "kind": "cliproxy", "url": "http://x", "managementKey": "", "enabled": true } })
        );
    }

    #[test]
    fn usage_model_aliases_round_trip() {
        let settings = decode_server(json!({
            "usageModelAliases": { "gpt-6-preview": "gpt-6" }
        }));
        assert_eq!(
            settings.usage_model_aliases,
            [(
                TrimmedNonEmptyString::new("gpt-6-preview").unwrap(),
                TrimmedNonEmptyString::new("gpt-6").unwrap()
            )]
            .into_iter()
            .collect::<IndexMap<TrimmedNonEmptyString, TrimmedNonEmptyString>>()
        );
        assert_eq!(
            encode(&settings)["usageModelAliases"],
            json!({ "gpt-6-preview": "gpt-6" })
        );
        assert!(
            serde_json::from_value::<ServerSettings>(json!({
                "usageModelAliases": { " ": "x" }
            }))
            .is_err()
        );
    }

    #[test]
    fn observability_settings_decode_and_default() {
        let settings = decode_server(json!({
            "observability": { "otlpTracesUrl": "http://localhost:4318/v1/traces" }
        }));
        assert_eq!(
            settings.observability.otlp_traces_url.as_str(),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(settings.observability.otlp_metrics_url.as_str(), "");
        assert_eq!(
            decode_server(json!({})).observability,
            ObservabilitySettings::default()
        );
    }

    #[test]
    fn background_activity_settings_decode() {
        let settings = decode_server(json!({
            "backgroundActivity": {
                "profile": "custom",
                "baseProfile": "battery-saver",
                "overrides": { "automaticGitFetchInterval": 60000, "pauseWhenOnBattery": true }
            }
        }));
        let activity = &settings.background_activity;
        assert_eq!(activity.profile, BackgroundActivityProfileSelection::Custom);
        assert_eq!(
            activity.base_profile,
            Some(BackgroundActivityProfile::BatterySaver)
        );
        assert_eq!(
            activity.overrides.automatic_git_fetch_interval,
            Some(60_000)
        );
        assert_eq!(activity.overrides.pause_when_on_battery, Some(true));
        assert_eq!(
            encode(&settings)["backgroundActivity"],
            json!({
                "schemaVersion": 1,
                "profile": "custom",
                "baseProfile": "battery-saver",
                "overrides": { "automaticGitFetchInterval": 60000, "pauseWhenOnBattery": true }
            })
        );
        // Legacy flat fields still decode.
        let flat = decode_server(json!({
            "automaticGitFetchInterval": 45000,
            "providerHealthRefreshInterval": 600000,
            "backgroundActivityProfile": "performance"
        }));
        assert_eq!(flat.automatic_git_fetch_interval, 45_000);
        assert_eq!(flat.provider_health_refresh_interval, 600_000);
        assert_eq!(
            flat.background_activity_profile,
            BackgroundActivityProfile::Performance
        );
    }

    #[test]
    fn worktrees_directory_and_previous_directories_round_trip() {
        let settings = decode_server(json!({
            "worktreesDirectory": "  ~/worktrees  ",
            "previousWorktreesDirectories": ["~/old-worktrees"]
        }));
        assert_eq!(settings.worktrees_directory.as_str(), "~/worktrees");
        assert_eq!(
            encode(&settings)["worktreesDirectory"],
            json!("~/worktrees")
        );
    }

    #[test]
    fn server_settings_operation_round_trips_every_literal() {
        for operation in [
            "normalize",
            "check-exists",
            "create-provider-instance",
            "read-file",
            "read-provider-history",
            "read-project-settings",
            "read-secret",
            "remove-secret",
            "remove-stale-secret",
            "write-secret",
            "write-file",
            "prepare-directory",
        ] {
            let decoded: ServerSettingsOperation =
                serde_json::from_value(json!(operation)).unwrap();
            assert_eq!(decoded.as_str(), operation);
            assert_eq!(encode(&decoded), json!(operation));
        }
    }
    // ── audit: exact default snapshots and re-encode ─────────────

    /// Every `withDecodingDefault` of `ServerSettings`, spelled out from
    /// settings.ts, minus the cut `deviceHosts` and the omitted-when-null
    /// `defaultThreadEnvMode`. Decoding `{}` and re-encoding must write all of
    /// them with exactly these values.
    fn expected_default_server_json() -> serde_json::Value {
        {
            let raw = r####"{
            "favorites": null,
            "providerModelPreferences": null,
            "worktreeCleanup": null,
            "storageCleanup": {
                "worktreeKeepWhen": "uncommitted-changes",
                "worktreeAfterDays": null,
                "worktreeOnMerge": false,
                "worktreeOnDelete": false,
                "worktreeUnchanged": false,
                "browserArtifactsAfterDays": null,
                "logsAfterDays": null
            },
            "worktreesDirectory": "",
            "previousWorktreesDirectories": [],
            "responseStreamingMode": "paragraph",
            "enableProviderUpdateChecks": true,
            "continueThreadsAfterServerUpdate": true,
            "enableAgentBrowserAccess": true,
            "projectAgentBrowserAccessOverrides": {},
            "defaultAutoPull": false,
            "defaultProjectScripts": [],
            "projectScriptOverrides": {},
            "projectAutoPullOverrides": {},
            "defaultModelSelection": null,
            "defaultRuntimeMode": "full-access",
            "projectSettingsOverrides": {},
            "projectSettingsFolded": false,
            "enableAgentDeviceAccess": false,
            "enableDeviceSupport": false,
            "deviceOnboardingCompleted": false,
            "sidebarAutoSettleAfterDays": 3,
            "snoozeLimitedThreads": false,
            "autoResumeLimitedThreads": false,
            "sidebarAutoSettleKeepPullRequests": false,
            "sidebarAutoSettleOnMerge": true,
            "backgroundActivity": { "schemaVersion": 1, "profile": "balanced", "overrides": {} },
            "automaticGitFetchInterval": 30000,
            "providerHealthRefreshInterval": 300000,
            "backgroundActivityProfile": "balanced",
            "defaultTheme": "",
            "defaultThemeSetAt": "",
            "environmentIcon": null,
            "newWorktreesStartFromOrigin": true,
            "worktreeSubmodules": null,
            "addProjectBaseDirectory": "",
            "textGenerationModelSelection": {
                "instanceId": "codex",
                "model": "$MODEL",
                "options": [{ "id": "reasoningEffort", "value": "$EFFORT" }]
            },
            "branchNamingMode": "static",
            "branchNamePrefix": "t3",
            "branchNameInstructions": "",
            "removeAgentCreditsOnMerge": false,
            "sourceControlWritingStyle": {
                "mode": "repo_conventions",
                "customInstructions": "",
                "followChangeRequestTemplates": true
            },
            "sourceControlWriterModelSelection": null,
            "reviewGuidesEnabled": false,
            "reviewGuideModelSelection": null,
            "reviewGuideInstructions": "",
            "pullRequestMergeMethod": null,
            "providerInstances": {},
            "observability": { "otlpTracesUrl": "", "otlpMetricsUrl": "", "otlpLogsUrl": "" },
            "sourceControlHosts": {},
            "usageLimitSources": {},
            "cursorKeychainUsageEnabled": false,
            "usagePriceOverrides": {},
            "usageModelAliases": {}
        }"####;
            serde_json::from_str(
                &raw.replace("$MODEL", DEFAULT_TEXT_GENERATION_MODEL)
                    .replace("$EFFORT", DEFAULT_TEXT_GENERATION_REASONING_EFFORT),
            )
            .unwrap()
        }
    }

    #[test]
    fn default_server_settings_encode_every_decoding_default_exactly() {
        assert_eq!(
            encode(&decode_server(json!({}))),
            expected_default_server_json()
        );
        // Re-decoding the written defaults gives the same settings back.
        let again: ServerSettings = serde_json::from_value(expected_default_server_json()).unwrap();
        assert_eq!(again, default_server_settings());
    }

    #[test]
    fn default_client_settings_encode_every_decoding_default_exactly() {
        let expected = serde_json::from_str::<serde_json::Value>(
            r####"{
            "notificationMode": "off",
            "inAppNotificationsEnabled": false,
            "diffColorScheme": "red-green",
            "chatWidth": "comfortable",
            "loadBalancingEnabled": false,
            "loadBalancingWeights": {},
            "appearanceContrast": 100,
            "panelAnimationDurationMs": 0,
            "browserRecordingFrameRate": 30,
            "browserRecordingShowKeyPresses": false,
            "browserRecordingShowMousePresses": false,
            "browserLinkTarget": "system",
            "browserAutoShowFloatingPreview": true,
            "confirmQuit": "hold",
            "confirmThreadArchive": false,
            "confirmThreadDelete": true,
            "confirmThreadUnpin": false,
            "dismissedProviderUpdateNotificationKeys": [],
            "diffFilesCollapsed": true,
            "diffIgnoreWhitespace": true,
            "diffLayout": "stacked",
            "environmentIdentificationMode": "artwork",
            "glassOpacity": 80,
            "fontSizeInterface": 16,
            "fontSizePrompt": 14,
            "fontSizeCode": 13,
            "fontSizeTerminal": 12,
            "fontFamilyCode": "",
            "fontFamilyComposer": "",
            "fontFamilySans": "",
            "fontFamilyTerminal": "",
            "fontSmoothing": true,
            "onboardingCompletedAt": null,
            "persistComposerContextStrip": false,
            "favorites": [],
            "providerModelPreferences": {},
            "pullRequestMergeMethodOverrides": {},
            "planModeEnabled": false,
            "contextWindowMeterEnabled": false,
            "composerCollapseOnScroll": true,
            "composerRichTextEnabled": true,
            "sendShortcut": "enter",
            "followUpBehavior": "queue",
            "proactivePanelsEnabled": false,
            "showSkillsInSlashMenu": true,
            "legacySidebarEnabled": false,
            "sidebarWorkingShelfEnabled": false,
            "sidebarProjectGroupingMode": "repository",
            "sidebarProjectGroupingOverrides": {},
            "sidebarProjectSortOrder": "updated_at",
            "sidebarThreadSortOrder": "updated_at",
            "sidebarThreadPreviewCount": 6,
            "timestampFormat": "locale",
            "snapShotEnabled": false,
            "snapShotIncludeAccessibility": true,
            "snapShotShortcut": { "kind": "both-shift-keys" },
            "snapShotPlaySound": true,
            "snapShotSound": "soft-pop",
            "snapShotFlash": true,
            "snapShotAnimations": true,
            "wordWrap": true
        }"####,
        )
        .unwrap();
        assert_eq!(encode(&decode_client(json!({}))), expected);
        assert_eq!(decode_client(expected), default_client_settings());
    }

    #[test]
    fn provider_settings_defaults_encode_every_key() {
        let codex: CodexSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            encode(&codex),
            json!({
                "enabled": true,
                "binaryPath": "codex",
                "homePath": "",
                "shadowHomePath": "",
                "launchArgs": "",
                "customModels": []
            })
        );
        let claude: ClaudeSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            encode(&claude),
            json!({
                "enabled": true,
                "binaryPath": "claude",
                "homePath": "",
                "customModels": [],
                "launchArgs": "",
                "autoCompactWindow": ""
            })
        );
        // Whitespace is trimmed on decode, but launchArgs is a plain string.
        let claude: ClaudeSettings = serde_json::from_value(json!({
            "binaryPath": "  /opt/claude  ",
            "homePath": "  ~/.claude  ",
            "launchArgs": "  --chrome ",
            "autoCompactWindow": " 300000 "
        }))
        .unwrap();
        assert_eq!(claude.binary_path.as_str(), "/opt/claude");
        assert_eq!(claude.home_path.as_str(), "~/.claude");
        assert_eq!(claude.launch_args, "  --chrome ");
        assert_eq!(claude.auto_compact_window.as_str(), "300000");
    }

    #[test]
    fn provider_settings_form_metadata_matches_upstream() {
        let codex = codex_settings_form_fields();
        let names: Vec<_> = codex.iter().map(|field| field.field).collect();
        assert_eq!(
            names,
            [
                "setupMode",
                "enabled",
                "binaryPath",
                "homePath",
                "shadowHomePath",
                "launchArgs",
                "customModels"
            ]
        );
        assert_eq!(codex[0].annotation.hidden, Some(true));
        assert_eq!(codex[2].annotation.placeholder.as_deref(), Some("codex"));
        assert_eq!(
            codex[5].annotation,
            ProviderSettingsFormAnnotation::default()
        );
        let claude = claude_settings_form_fields();
        assert_eq!(claude.last().unwrap().field, "autoCompactWindow");
        assert_eq!(
            CLAUDE_SETTINGS_FORM_ORDER,
            ["binaryPath", "homePath", "autoCompactWindow", "launchArgs"]
        );
        assert_eq!(USAGE_LIMIT_SOURCE_SECRET_SETTINGS_FIELDS, ["managementKey"]);
    }

    // ── audit: absent vs null in optionalKey fields ──────────────

    #[test]
    fn optional_key_fields_reject_explicit_null() {
        // `Schema.optionalKey(X)` with a non-nullable X: absent is fine, null is
        // a type error (plain serde would turn it into "absent").
        for patch in [
            json!({ "wordWrap": null }),
            json!({ "confirmThreadUnpin": "yes" }),
            json!({ "glassOpacity": null }),
            json!({ "sidebarThreadPreviewCount": null }),
            json!({ "snapShotShortcut": null }),
        ] {
            assert!(
                serde_json::from_value::<ClientSettingsPatch>(patch.clone()).is_err(),
                "{patch} should be rejected"
            );
        }
        for patch in [
            json!({ "defaultRuntimeMode": null }),
            json!({ "enableAgentBrowserAccess": null }),
            json!({ "defaultProjectScripts": null }),
            json!({ "textGenerationModelSelection": null }),
            json!({ "textGenerationModelSelection": { "model": null } }),
            json!({ "storageCleanup": { "worktreeOnMerge": null } }),
            json!({ "backgroundActivity": { "baseProfile": null } }),
            json!({ "providerInstances": null }),
            json!({ "projectSettingsOverrides": { "p": { "defaultRuntimeMode": null } } }),
            json!({ "projectSettingsOverrides": { "p": { "providerInstances": null } } }),
        ] {
            assert!(
                serde_json::from_value::<ServerSettingsPatch>(patch.clone()).is_err(),
                "{patch} should be rejected"
            );
        }
        assert!(
            serde_json::from_value::<ServerSettings>(json!({
                "projectSettingsOverrides": { "p": { "branchNamePrefix": null } }
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<UsageModelPriceOverride>(json!({
                "inputCostPerMillionTokens": 1,
                "outputCostPerMillionTokens": 1,
                "cacheReadCostPerMillionTokens": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<UsageLimitSourceConfig>(json!({
                "kind": "cliproxy", "url": "http://x", "label": null
            }))
            .is_err()
        );
        // The same keys are fine when absent.
        assert_eq!(
            decode_client_patch(json!({})),
            ClientSettingsPatch::default()
        );
        assert_eq!(
            decode_server_patch(json!({})),
            ServerSettingsPatch::default()
        );
    }

    #[test]
    fn nullable_patch_fields_keep_absent_null_and_value_apart() {
        // optionalKey(NullOr(X)): absent = unchanged, null = clear.
        let absent = decode_server_patch(json!({}));
        assert_eq!(absent.default_model_selection, None);
        let cleared = decode_server_patch(json!({ "defaultModelSelection": null }));
        assert_eq!(cleared.default_model_selection, Some(None));
        assert_eq!(encode(&cleared), json!({ "defaultModelSelection": null }));
        let set = decode_server_patch(json!({
            "defaultModelSelection": { "instanceId": "codex", "model": "m" }
        }));
        assert!(matches!(set.default_model_selection, Some(Some(_))));

        for key in [
            "sourceControlWriterModelSelection",
            "reviewGuideModelSelection",
            "pullRequestMergeMethod",
            "sidebarAutoSettleAfterDays",
            "environmentIcon",
            "defaultThreadEnvMode",
            "worktreeSubmodules",
            "worktreeCleanup",
        ] {
            let patch = decode_server_patch(json!({ key: null }));
            assert_eq!(
                encode(&patch),
                json!({ key: null }),
                "{key} null must round-trip"
            );
            assert_eq!(encode(&decode_server_patch(json!({}))), json!({}), "{key}");
        }
        let client = decode_client_patch(json!({ "onboardingCompletedAt": null }));
        assert_eq!(client.onboarding_completed_at, Some(None));
        assert_eq!(encode(&client), json!({ "onboardingCompletedAt": null }));
        let client =
            decode_client_patch(json!({ "onboardingCompletedAt": "2026-01-01T00:00:00.000Z" }));
        assert_eq!(
            client.onboarding_completed_at,
            Some(Some("2026-01-01T00:00:00.000Z".to_owned()))
        );
        assert_eq!(decode_client_patch(json!({})).onboarding_completed_at, None);

        // Record entries: null removes, a value replaces.
        let patch = decode_server_patch(json!({
            "projectSettingsOverrides": { "gone": null, "kept": { "defaultAutoPull": true } },
            "projectScriptOverrides": { "gone": null },
            "projectAutoPullOverrides": { "gone": null, "on": true },
            "projectAgentBrowserAccessOverrides": { "gone": null },
            "usageLimitSources": { "gone": null },
            "usageModelAliases": { "gone": null, "preview": "released" }
        }));
        let overrides = patch.project_settings_overrides.as_ref().unwrap();
        assert_eq!(overrides.get(&project_id("gone")), Some(&None));
        assert!(matches!(overrides.get(&project_id("kept")), Some(Some(_))));
        let encoded = encode(&patch);
        assert_eq!(encoded["projectSettingsOverrides"]["gone"], json!(null));
        assert_eq!(encoded["projectScriptOverrides"]["gone"], json!(null));
        assert_eq!(
            encoded["projectAutoPullOverrides"],
            json!({ "gone": null, "on": true })
        );
        assert_eq!(encoded["usageLimitSources"]["gone"], json!(null));
        assert_eq!(
            encoded["usageModelAliases"],
            json!({ "gone": null, "preview": "released" })
        );
    }

    #[test]
    fn project_overrides_distinguish_inherit_from_explicit_null() {
        let input = json!({
            "defaultModelSelection": null,
            "sourceControlWriterModelSelection": null,
            "reviewGuideModelSelection": null,
            "pullRequestMergeMethod": null,
            "sidebarAutoSettleAfterDays": null,
            "worktreeCleanup": null
        });
        let overrides: ProjectSettingsOverrides = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(overrides.default_model_selection, Some(None));
        assert_eq!(overrides.worktree_cleanup, Some(None));
        assert_eq!(overrides.sidebar_auto_settle_after_days, Some(None));
        assert_eq!(encode(&overrides), input);
        // Inheriting writes nothing.
        let inherit: ProjectSettingsOverrides = serde_json::from_value(json!({})).unwrap();
        assert_eq!(inherit, ProjectSettingsOverrides::default());
        assert_eq!(encode(&inherit), json!({}));
        // Non-null values and the forward-compatible submodule mode.
        let overrides: ProjectSettingsOverrides = serde_json::from_value(json!({
            "sidebarAutoSettleAfterDays": 7,
            "worktreeSubmodules": "shallow-from-the-future",
            "pullRequestMergeMethod": "squash"
        }))
        .unwrap();
        assert_eq!(
            overrides.sidebar_auto_settle_after_days,
            Some(Some(SidebarAutoSettleAfterDays::new(7.0).unwrap()))
        );
        assert_eq!(overrides.worktree_submodules, None);
        assert_eq!(
            encode(&overrides),
            json!({ "sidebarAutoSettleAfterDays": 7, "pullRequestMergeMethod": "squash" })
        );
    }

    // ── audit: decoding defaults inside nested structs ───────────

    #[test]
    fn nested_decoding_defaults_fill_missing_keys() {
        // storageCleanup keeps its own defaults, including the keep-when policy.
        let settings = decode_server(json!({ "storageCleanup": {} }));
        assert_eq!(settings.storage_cleanup, StorageCleanupSettings::default());
        let settings = decode_server(json!({ "storageCleanup": { "logsAfterDays": 30 } }));
        assert_eq!(
            settings.storage_cleanup.logs_after_days,
            Some(StorageRetentionDays::new(30).unwrap())
        );
        // sourceControlWritingStyle, observability, backgroundActivity: `{}`.
        let settings = decode_server(json!({
            "sourceControlWritingStyle": {},
            "observability": {},
            "backgroundActivity": {}
        }));
        assert_eq!(
            settings.source_control_writing_style,
            SourceControlWritingStyleSettings::default()
        );
        assert_eq!(settings.observability, ObservabilitySettings::default());
        assert_eq!(
            settings.background_activity,
            BackgroundActivitySettings::default()
        );
        // Per-provider model preferences default both lists.
        let preferences: ProviderModelPreferences =
            serde_json::from_value(json!({ "codex": {} })).unwrap();
        assert_eq!(
            preferences.get(&provider_instance_id("codex")),
            Some(&ProviderModelPreferencesEntry::default())
        );
        // A usage limit source defaults its key and enabled flag.
        let source: UsageLimitSourceConfig =
            serde_json::from_value(json!({ "kind": "cliproxy", "url": " http://hub " })).unwrap();
        assert_eq!(source.management_key.as_str(), "");
        assert!(source.enabled);
        assert_eq!(source.url.as_str(), "http://hub");
        assert_eq!(source.label, None);
        assert_eq!(
            encode(&source),
            json!({ "kind": "cliproxy", "url": "http://hub", "managementKey": "", "enabled": true })
        );
        assert!(
            serde_json::from_value::<UsageLimitSourceConfig>(
                json!({ "kind": "other", "url": "http://hub" })
            )
            .is_err()
        );
        // Report entries default bytes/files to null and the report bytesFreed to 0.
        let entry: StorageCleanupReportEntry = serde_json::from_value(json!({
            "kind": "browser-artifacts",
            "outcome": "kept",
            "reason": "recent",
            "path": null,
            "threadId": null,
            "threadTitle": null
        }))
        .unwrap();
        assert_eq!(entry.bytes, None);
        assert_eq!(entry.files, None);
        assert_eq!(encode(&entry)["bytes"], json!(null));
        let report: StorageCleanupReport = serde_json::from_value(json!({
            "trigger": "manual",
            "startedAt": "a",
            "finishedAt": "b",
            "entries": [],
            "counts": { "removed": 1, "kept": 2, "failed": 0 },
            "omittedCount": 0
        }))
        .unwrap();
        assert_eq!(report.bytes_freed, 0.0);
        let encoded = encode(&report);
        assert_eq!(encoded["bytesFreed"], json!(0));
        assert_eq!(
            encoded["counts"],
            json!({ "removed": 1, "kept": 2, "failed": 0 })
        );
    }

    #[test]
    fn schema_numbers_encode_like_json_stringify() {
        let price: UsageModelPriceOverride = serde_json::from_value(json!({
            "inputCostPerMillionTokens": 3.0,
            "outputCostPerMillionTokens": 0.25,
            "cacheReadCostPerMillionTokens": 0,
        }))
        .unwrap();
        assert_eq!(
            encode(&price),
            json!({
                "inputCostPerMillionTokens": 3,
                "outputCostPerMillionTokens": 0.25,
                "cacheReadCostPerMillionTokens": 0
            })
        );
        let text = serde_json::to_string(&price).unwrap();
        assert!(text.contains("\"inputCostPerMillionTokens\":3,"), "{text}");
        let settings = decode_server(json!({ "sidebarAutoSettleAfterDays": 2.5 }));
        assert_eq!(encode(&settings)["sidebarAutoSettleAfterDays"], json!(2.5));
        let text = serde_json::to_string(&default_server_settings()).unwrap();
        assert!(text.contains("\"sidebarAutoSettleAfterDays\":3,"), "{text}");
    }

    #[test]
    fn browser_recording_frame_rate_is_a_numeric_literal() {
        let settings = decode_client(json!({ "browserRecordingFrameRate": 60 }));
        assert_eq!(
            settings.browser_recording_frame_rate,
            BrowserRecordingFrameRate::Sixty
        );
        assert_eq!(encode(&settings)["browserRecordingFrameRate"], json!(60));
        // The strings "30"/"60" are not the numeric literals.
        assert!(
            serde_json::from_value::<ClientSettings>(json!({ "browserRecordingFrameRate": "30" }))
                .is_err()
        );
        assert_eq!(BROWSER_RECORDING_FRAME_RATES, [30, 60]);
        assert_eq!(DEFAULT_BROWSER_RECORDING_FRAME_RATE.get(), 30);
    }

    #[test]
    fn length_limits_count_utf16_code_units() {
        // 100 astral characters = 200 UTF-16 code units: accepted. 101 = 202: rejected.
        let ok = "😀".repeat(100);
        let too_long = "😀".repeat(101);
        assert!(FontFamilyPreference::new(&ok).is_ok());
        assert!(FontFamilyPreference::new(&too_long).is_err());
        assert!(FontFamilyPreference::new("a".repeat(200)).is_ok());
        assert!(FontFamilyPreference::new("a".repeat(201)).is_err());
        assert!(
            serde_json::from_value::<ClientSettingsPatch>(json!({ "fontFamilyCode": too_long }))
                .is_err()
        );
        assert!(DefaultThemePreference::new("😀".repeat(32)).is_ok());
        assert!(DefaultThemePreference::new("😀".repeat(33)).is_err());
        assert!(
            serde_json::from_value::<ServerSettings>(
                json!({ "defaultThemeSetAt": "a".repeat(65) })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ServerSettings>(json!({ "defaultTheme": "a".repeat(64) }))
                .is_ok()
        );
    }

    #[test]
    fn snapshot_shortcut_union_members_decode_in_order() {
        let both: SnapShotShortcut =
            serde_json::from_value(json!({ "kind": "both-shift-keys" })).unwrap();
        assert!(is_modifier_pair_shortcut(&both));
        let pair: SnapShotShortcut =
            serde_json::from_value(json!({ "kind": "modifier-pair", "modifier": "alt" })).unwrap();
        assert_eq!(
            snap_shot_shortcut_modifier_pair(&pair),
            SnapShotModifier::Alt
        );
        assert_eq!(
            encode(&pair),
            json!({ "kind": "modifier-pair", "modifier": "alt" })
        );
        let chord = json!({
            "key": "k", "metaKey": false, "ctrlKey": false,
            "shiftKey": false, "altKey": false, "modKey": true
        });
        let decoded: SnapShotShortcut = serde_json::from_value(chord.clone()).unwrap();
        assert!(!is_modifier_pair_shortcut(&decoded));
        assert_eq!(encode(&decoded), chord);
        assert_eq!(
            snap_shot_modifier_pair_label(SnapShotModifier::Meta, true),
            "Command + Command"
        );
        assert_eq!(
            snap_shot_modifier_pair_label(SnapShotModifier::Control, false),
            "Ctrl + Ctrl"
        );
    }

    #[test]
    fn full_server_settings_round_trip_preserves_every_value() {
        let input = serde_json::from_str::<serde_json::Value>(r####"{
            "favorites": [{ "provider": "codex", "model": "m" }],
            "providerModelPreferences": { "codex": { "hiddenModels": ["a"], "modelOrder": ["b"] } },
            "worktreeCleanup": { "mode": "custom", "rules": {
                "worktreeKeepWhen": "tracked-changes", "worktreeAfterDays": 14,
                "worktreeOnMerge": true, "worktreeOnDelete": false, "worktreeUnchanged": true } },
            "storageCleanup": {
                "worktreeKeepWhen": "any-local-files", "worktreeAfterDays": 8,
                "worktreeOnMerge": true, "worktreeOnDelete": true, "worktreeUnchanged": true,
                "browserArtifactsAfterDays": 3650, "logsAfterDays": 1 },
            "worktreesDirectory": "~/wt",
            "previousWorktreesDirectories": ["/old"],
            "responseStreamingMode": "turn",
            "enableProviderUpdateChecks": false,
            "continueThreadsAfterServerUpdate": false,
            "enableAgentBrowserAccess": false,
            "projectAgentBrowserAccessOverrides": { "p": true },
            "defaultAutoPull": true,
            "projectScriptOverrides": { "p": null },
            "projectAutoPullOverrides": { "p": false },
            "defaultModelSelection": { "instanceId": "claudeAgent", "model": "m" },
            "defaultRuntimeMode": "approval-required",
            "projectSettingsOverrides": { "p": { "defaultModelSelection": null, "branchNamePrefix": "x" } },
            "projectSettingsFolded": true,
            "enableAgentDeviceAccess": true,
            "enableDeviceSupport": true,
            "deviceOnboardingCompleted": true,
            "sidebarAutoSettleAfterDays": null,
            "snoozeLimitedThreads": true,
            "autoResumeLimitedThreads": true,
            "sidebarAutoSettleKeepPullRequests": true,
            "sidebarAutoSettleOnMerge": false,
            "backgroundActivity": {
                "schemaVersion": 1, "profile": "custom", "baseProfile": "battery-saver",
                "overrides": { "idleClientTtl": 1000, "pauseWhenOnBattery": true } },
            "automaticGitFetchInterval": 60000,
            "providerHealthRefreshInterval": 120000,
            "backgroundActivityProfile": "performance",
            "defaultTheme": "dracula",
            "defaultThemeSetAt": "2026-01-01T00:00:00.000Z",
            "environmentIcon": "linux",
            "defaultThreadEnvMode": "worktree",
            "newWorktreesStartFromOrigin": false,
            "worktreeSubmodules": "top-level",
            "addProjectBaseDirectory": "~/dev",
            "branchNamingMode": "custom",
            "branchNamePrefix": "me/",
            "branchNameInstructions": "Include the issue.",
            "removeAgentCreditsOnMerge": true,
            "sourceControlWritingStyle": {
                "mode": "custom", "customInstructions": "Be brief.", "followChangeRequestTemplates": false },
            "sourceControlWriterModelSelection": { "instanceId": "codex", "model": "w" },
            "reviewGuidesEnabled": true,
            "reviewGuideModelSelection": { "instanceId": "codex", "model": "g" },
            "reviewGuideInstructions": "Dwell on tests.",
            "pullRequestMergeMethod": "rebase",
            "providerInstances": { "codex_work": { "driver": "codex", "enabled": false } },
            "observability": { "otlpTracesUrl": "http://t", "otlpMetricsUrl": "", "otlpLogsUrl": "" },
            "sourceControlHosts": { "github": { "token": "redacted", "n": 1 } },
            "usageLimitSources": { "hub": { "kind": "cliproxy", "url": "http://hub",
                "managementKey": "k", "enabled": false, "label": "Hub" } },
            "cursorKeychainUsageEnabled": true,
            "usagePriceOverrides": { "m": { "inputCostPerMillionTokens": 1, "outputCostPerMillionTokens": 2.5 } },
            "usageModelAliases": { "preview": "released" }
        }"####).unwrap();
        let settings = decode_server(input.clone());
        let encoded = encode(&settings);
        for (key, value) in input.as_object().unwrap() {
            if key == "textGenerationModelSelection" {
                continue;
            }
            assert_eq!(&encoded[key], value, "{key} must round-trip");
        }
        assert_eq!(decode_server(encoded), settings);
    }
}
