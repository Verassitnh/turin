//! Port of upstream/apps/web/src/components/settings/ThemeImportDialog.tsx
//!
//! The dialog's pipeline: the file-size guard, the JSON highlighter behind the
//! editor, batch import (VS Code families pair their light and dark variants),
//! the update-or-copy conflict step and the single-paste submit. Reading files
//! and painting the dialog is the view's job (`ThemeImportDialog` below).

use serde_json::{Map, Value};

use crate::theme::theme_palette_library::{
    THEME_FILE_VERSION, get_custom_themes, install_custom_theme, parse_theme_file,
    remove_custom_theme, theme_colors_to_camel_map, update_custom_theme,
};
use crate::theme::theme_palettes::{ThemeAppearance, ThemeDefinition};
use crate::theme::vscode_theme_import::{
    ThemeImportEntry, humanize_theme_name, is_vscode_theme_file, pair_vscode_themes,
    parse_vscode_theme_file, resolve_theme_label_collisions,
};

// ── Size guard ───────────────────────────────────────────────────────

/// A full theme export is a few KB, so anything past this is not a theme file.
/// The guard runs on the size before the bytes are ever read: a large file
/// would otherwise be pulled into memory, highlighted, and rendered, which
/// locks the UI for as long as that takes.
pub const MAX_THEME_FILE_BYTES: u64 = 256 * 1024;

/// Highlighting rebuilds the whole markup on every keystroke, so oversized
/// pastes fall back to plain text instead of freezing the editor.
pub const MAX_HIGHLIGHTED_JSON_LENGTH: usize = 20_000;

/// JS `Number.prototype.toFixed(1)`-style rounding for the sizes shown here.
fn format_byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", (bytes as f64 / 1024.0).round() as u64)
    } else {
        format!("{bytes} bytes")
    }
}

/// `describeOversizedThemeFile`: the error to show for a file too large to be
/// a theme, else `None`.
pub fn describe_oversized_theme_file(bytes: u64) -> Option<String> {
    if bytes <= MAX_THEME_FILE_BYTES {
        return None;
    }
    Some(format!(
        "That file is {}. Theme files are only a few KB, so this one was not read (limit {}).",
        format_byte_size(bytes),
        format_byte_size(MAX_THEME_FILE_BYTES)
    ))
}

// ── JSON highlighting ────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonTokenKind {
    /// Punctuation and whitespace between tokens.
    Plain,
    /// A string followed by `:`.
    Key,
    /// Any other string.
    String,
    Number,
    /// `true`, `false`, `null`.
    Keyword,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonSpan {
    pub text: String,
    pub kind: JsonTokenKind,
}

/// `highlightJson`: the tokenizer behind the editor's overlay. Mirrors the
/// upstream token regex
/// `"(?:\\.|[^"\\])*"|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null`
/// scanned left to right.
pub fn highlight_json(value: &str) -> Vec<JsonSpan> {
    let chars: Vec<char> = value.chars().collect();
    let mut spans: Vec<JsonSpan> = Vec::new();
    let mut plain = String::new();
    let mut index = 0usize;
    let flush = |plain: &mut String, spans: &mut Vec<JsonSpan>| {
        if !plain.is_empty() {
            spans.push(JsonSpan {
                text: std::mem::take(plain),
                kind: JsonTokenKind::Plain,
            });
        }
    };
    while index < chars.len() {
        if let Some((end, kind)) = scan_json_token(&chars, index) {
            flush(&mut plain, &mut spans);
            let kind = if kind == JsonTokenKind::String
                && chars[end..].iter().copied().find(|c| !c.is_whitespace()) == Some(':')
            {
                JsonTokenKind::Key
            } else {
                kind
            };
            spans.push(JsonSpan {
                text: chars[index..end].iter().collect(),
                kind,
            });
            index = end;
        } else {
            plain.push(chars[index]);
            index += 1;
        }
    }
    flush(&mut plain, &mut spans);
    spans
}

fn scan_json_token(chars: &[char], start: usize) -> Option<(usize, JsonTokenKind)> {
    let c = chars[start];
    if c == '"' {
        let mut index = start + 1;
        while index < chars.len() {
            match chars[index] {
                '\\' if index + 1 < chars.len() => index += 2,
                '"' => return Some((index + 1, JsonTokenKind::String)),
                '\\' => return None,
                _ => index += 1,
            }
        }
        return None;
    }
    for keyword in ["true", "false", "null"] {
        let end = start + keyword.len();
        if end <= chars.len() && chars[start..end].iter().copied().eq(keyword.chars()) {
            return Some((end, JsonTokenKind::Keyword));
        }
    }
    let mut index = start;
    if chars.get(index) == Some(&'-') {
        index += 1;
    }
    match chars.get(index) {
        Some('0') => index += 1,
        Some(d) if d.is_ascii_digit() => {
            while chars.get(index).is_some_and(char::is_ascii_digit) {
                index += 1;
            }
        }
        _ => return None,
    }
    if chars.get(index) == Some(&'.') && chars.get(index + 1).is_some_and(char::is_ascii_digit) {
        index += 1;
        while chars.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
    }
    if matches!(chars.get(index), Some('e' | 'E')) {
        let mut exponent = index + 1;
        if matches!(chars.get(exponent), Some('+' | '-')) {
            exponent += 1;
        }
        if chars.get(exponent).is_some_and(char::is_ascii_digit) {
            while chars.get(exponent).is_some_and(char::is_ascii_digit) {
                exponent += 1;
            }
            index = exponent;
        }
    }
    Some((index, JsonTokenKind::Number))
}

// ── Import pipeline ──────────────────────────────────────────────────

/// What the import pipeline needs from a file; the native picker or a drop
/// target supplies the text.
#[derive(Clone, Debug)]
pub struct ImportableThemeFile {
    pub name: String,
    pub size: u64,
    pub text: Result<String, String>,
}

/// Parses one JSON value the way the dialog does: VS Code themes are converted
/// on the way in; anything else has to be one of our own files.
pub fn parse_importable_theme(value: Value) -> Result<ThemeDefinition, String> {
    if is_vscode_theme_file(&value) {
        parse_vscode_theme_file(value)
    } else {
        parse_theme_file(value)
    }
}

#[derive(Debug, Default)]
pub struct BatchImportOutcome {
    pub installed: Vec<ThemeDefinition>,
    /// Themes whose id is already installed; they wait for an update-or-copy
    /// decision instead of failing.
    pub conflicting: Vec<ThemeDefinition>,
    pub failures: Vec<String>,
}

impl BatchImportOutcome {
    /// The dialog's next step: the failure line, the conflict prompt, or close.
    pub fn error(&self) -> Option<String> {
        (!self.failures.is_empty()).then(|| self.failures.join(" \u{2014} "))
    }
}

/// `readThemeBatch`: several files at once import as a batch. VS Code families
/// pair their light and dark variants, everything installs without activating.
pub fn import_theme_batch(files: &[ImportableThemeFile]) -> BatchImportOutcome {
    let mut outcome = BatchImportOutcome::default();
    let mut parsed: Vec<ThemeImportEntry> = Vec::new();
    for file in files {
        if describe_oversized_theme_file(file.size).is_some() {
            outcome.failures.push(format!("{}: too large", file.name));
            continue;
        }
        let theme = file
            .text
            .clone()
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
            .and_then(parse_importable_theme);
        match theme {
            Ok(theme) => parsed.push(ThemeImportEntry {
                theme,
                source_name: Some(file.name.clone()),
            }),
            Err(cause) => {
                let message = if cause.is_empty() {
                    "not a theme file".to_string()
                } else {
                    cause
                };
                outcome.failures.push(format!("{}: {message}", file.name));
            }
        }
    }
    let resolved = resolve_theme_label_collisions(&parsed);
    for theme in pair_vscode_themes(&resolved, None) {
        if get_custom_themes()
            .iter()
            .any(|existing| existing.id == theme.id)
        {
            outcome.conflicting.push(theme);
            continue;
        }
        match install_custom_theme(theme.clone()) {
            Ok(installed) => outcome.installed.push(installed),
            Err(cause) => {
                let message = if cause.is_empty() {
                    "could not install".to_string()
                } else {
                    cause
                };
                outcome.failures.push(format!("{}: {message}", theme.label));
            }
        }
    }
    outcome
}

/// `versionedCopy`: a copy of an already-installed theme under the source
/// file's name when that differs (Dracula Soft), else the next free "Name (1)".
pub fn versioned_copy(
    theme: &ThemeDefinition,
    preferred_name: Option<&str>,
) -> Result<ThemeDefinition, String> {
    let copy_of = |name: String| -> Result<ThemeDefinition, String> {
        let mut file = Map::new();
        file.insert("version".into(), THEME_FILE_VERSION.into());
        file.insert("name".into(), name.into());
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
        if theme.variants.light.is_some() || theme.variants.dark.is_some() {
            let mut variants = Map::new();
            if let Some(light) = &theme.variants.light {
                variants.insert(
                    "light".into(),
                    Value::Object(theme_colors_to_camel_map(light)),
                );
            }
            if let Some(dark) = &theme.variants.dark {
                variants.insert(
                    "dark".into(),
                    Value::Object(theme_colors_to_camel_map(dark)),
                );
            }
            file.insert("variants".into(), Value::Object(variants));
        }
        if theme.managed {
            file.insert("managed".into(), Value::Bool(true));
        }
        parse_theme_file(Value::Object(file))
    };
    let taken = |candidate: &ThemeDefinition| {
        get_custom_themes()
            .iter()
            .any(|existing| existing.id == candidate.id)
    };
    if let Some(preferred) = preferred_name
        && preferred.to_lowercase() != theme.label.to_lowercase()
    {
        let candidate = copy_of(preferred.chars().take(48).collect())?;
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }
    for copy in 1..100 {
        let suffix = format!(" ({copy})");
        let stem: String = theme
            .label
            .chars()
            .take(48usize.saturating_sub(suffix.chars().count()))
            .collect();
        let candidate = copy_of(format!("{stem}{suffix}"))?;
        if taken(&candidate) {
            continue;
        }
        return Ok(candidate);
    }
    Err(format!("Too many copies of \"{}\".", theme.label))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictResolution {
    Update,
    Copy,
}

#[derive(Debug, Default)]
pub struct ConflictOutcome {
    pub resolved: Vec<ThemeDefinition>,
    pub failures: Vec<String>,
}

/// `resolveConflicts`.
pub fn resolve_import_conflicts(
    conflicts: &[ThemeDefinition],
    file_name: Option<&str>,
    mode: ConflictResolution,
) -> ConflictOutcome {
    let mut outcome = ConflictOutcome::default();
    let preferred_name = (conflicts.len() == 1)
        .then_some(file_name)
        .flatten()
        .map(|name| {
            let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            humanize_theme_name(stem)
        });
    for theme in conflicts {
        let result = match mode {
            ConflictResolution::Update => {
                let existing = get_custom_themes()
                    .into_iter()
                    .find(|candidate| candidate.id == theme.id);
                let theme_to_update = match existing.and_then(|existing| existing.collection) {
                    Some(collection) => ThemeDefinition {
                        collection: Some(collection),
                        ..theme.clone()
                    },
                    None => theme.clone(),
                };
                update_custom_theme(theme_to_update)
            }
            ConflictResolution::Copy => {
                versioned_copy(theme, preferred_name.as_deref()).and_then(install_custom_theme)
            }
        };
        match result {
            Ok(resolved) => outcome.resolved.push(resolved),
            Err(cause) => {
                let message = if cause.is_empty() {
                    "failed".to_string()
                } else {
                    cause
                };
                outcome.failures.push(format!("{}: {message}", theme.label));
            }
        }
    }
    outcome
}

#[derive(Debug)]
pub enum SubmitOutcome {
    /// Installed and activated; close the dialog.
    Imported(ThemeDefinition),
    /// The id is already installed; ask update-or-copy.
    Conflict(ThemeDefinition),
    Error(String),
}

/// `handleSubmit`: pasted text bypasses the file guard, so the same limit
/// applies here.
pub fn submit_theme_json(
    json: &str,
    on_imported: &mut dyn FnMut(&ThemeDefinition) -> bool,
) -> SubmitOutcome {
    if let Some(oversized) = describe_oversized_theme_file(json.encode_utf16().count() as u64) {
        return SubmitOutcome::Error(oversized);
    }
    let theme = serde_json::from_str::<Value>(json)
        .map_err(|error| error.to_string())
        .and_then(parse_importable_theme);
    let theme = match theme {
        Ok(theme) => theme,
        Err(cause) => {
            return SubmitOutcome::Error(if cause.is_empty() {
                "That theme file is invalid.".to_string()
            } else {
                cause
            });
        }
    };
    if get_custom_themes()
        .iter()
        .any(|existing| existing.id == theme.id)
    {
        return SubmitOutcome::Conflict(theme);
    }
    let installed = match install_custom_theme(theme) {
        Ok(installed) => installed,
        Err(cause) => return SubmitOutcome::Error(cause),
    };
    if !on_imported(&installed) {
        // Roll the install back so a retry can run it again instead of failing
        // on the already-taken theme id. Storage failing wholesale is covered
        // by the error below.
        let _ = remove_custom_theme(&installed.id);
        return SubmitOutcome::Error(
            "Theme added, but it could not be selected. Try again.".into(),
        );
    }
    SubmitOutcome::Imported(installed)
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

    fn library() -> parking_lot::MutexGuard<'static, ()> {
        let guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        guard
    }

    // describe("theme import size guard")

    #[test]
    fn accepts_anything_a_theme_file_could_plausibly_be() {
        for bytes in [0, 4_096, MAX_THEME_FILE_BYTES] {
            assert_eq!(describe_oversized_theme_file(bytes), None);
        }
    }

    #[test]
    fn rejects_a_file_too_large_to_be_a_theme_and_names_its_size() {
        let message = describe_oversized_theme_file(100 * 1024 * 1024).expect("oversized");
        assert!(message.contains("100.0 MB"), "{message}");
        assert!(message.contains("256 KB"), "{message}");
    }

    #[test]
    fn reports_sizes_just_past_the_limit_in_kb() {
        assert!(
            describe_oversized_theme_file(MAX_THEME_FILE_BYTES + 1)
                .expect("oversized")
                .contains("256 KB")
        );
    }

    // highlightJson

    #[test]
    fn highlights_keys_strings_numbers_and_keywords() {
        let spans = highlight_json("{\n  \"a\": [1, -2.5e3, true, null, \"s\"]\n}");
        let kinds: Vec<(&str, JsonTokenKind)> = spans
            .iter()
            .filter(|span| span.kind != JsonTokenKind::Plain)
            .map(|span| (span.text.as_str(), span.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("\"a\"", JsonTokenKind::Key),
                ("1", JsonTokenKind::Number),
                ("-2.5e3", JsonTokenKind::Number),
                ("true", JsonTokenKind::Keyword),
                ("null", JsonTokenKind::Keyword),
                ("\"s\"", JsonTokenKind::String),
            ]
        );
        // The spans cover the text exactly.
        let rejoined: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(rejoined, "{\n  \"a\": [1, -2.5e3, true, null, \"s\"]\n}");
    }

    #[test]
    fn unterminated_strings_and_stray_text_stay_plain() {
        let spans = highlight_json("{ \"open");
        assert!(spans.iter().all(|span| span.kind == JsonTokenKind::Plain));
        let escaped = highlight_json(r#""a\"b": 1"#);
        assert_eq!(escaped[0].text, r#""a\"b""#);
        assert_eq!(escaped[0].kind, JsonTokenKind::Key);
    }

    // submit / import pipeline

    fn theme_json(name: &str) -> String {
        json!({
            "version": 1,
            "name": name,
            "appearance": "light",
            "colors": { "canvas": "#f8fbff", "accent": "#5b6cff" },
        })
        .to_string()
    }

    #[test]
    fn submitting_a_theme_installs_it_and_hands_it_to_the_caller() {
        let _guard = library();
        let mut handed = None;
        let outcome = submit_theme_json(&theme_json("Aurora"), &mut |theme| {
            handed = Some(theme.id.to_string());
            true
        });
        assert!(matches!(outcome, SubmitOutcome::Imported(_)));
        assert_eq!(handed.as_deref(), Some("aurora"));
        assert_eq!(get_custom_themes().len(), 1);
    }

    #[test]
    fn a_theme_that_cannot_be_selected_is_rolled_back_so_a_retry_can_run() {
        let _guard = library();
        let outcome = submit_theme_json(&theme_json("Aurora"), &mut |_| false);
        match outcome {
            SubmitOutcome::Error(message) => {
                assert_eq!(
                    message,
                    "Theme added, but it could not be selected. Try again."
                );
            }
            other => panic!("expected an error, got {other:?}"),
        }
        assert!(get_custom_themes().is_empty());
        assert!(matches!(
            submit_theme_json(&theme_json("Aurora"), &mut |_| true),
            SubmitOutcome::Imported(_)
        ));
    }

    #[test]
    fn an_installed_id_waits_for_an_update_or_copy_decision() {
        let _guard = library();
        submit_theme_json(&theme_json("Aurora"), &mut |_| true);
        match submit_theme_json(&theme_json("Aurora"), &mut |_| true) {
            SubmitOutcome::Conflict(theme) => assert_eq!(theme.id.as_str(), "aurora"),
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn invalid_and_oversized_pastes_are_errors() {
        let _guard = library();
        assert!(matches!(
            submit_theme_json("{ nope", &mut |_| true),
            SubmitOutcome::Error(_)
        ));
        assert!(matches!(
            submit_theme_json(&json!({ "version": 9 }).to_string(), &mut |_| true),
            SubmitOutcome::Error(message) if message.contains("unsupported version")
        ));
        let huge = " ".repeat(MAX_THEME_FILE_BYTES as usize + 1);
        match submit_theme_json(&huge, &mut |_| true) {
            SubmitOutcome::Error(message) => assert!(message.contains("256 KB")),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    fn file(name: &str, text: &str) -> ImportableThemeFile {
        ImportableThemeFile {
            name: name.to_string(),
            size: text.len() as u64,
            text: Ok(text.to_string()),
        }
    }

    #[test]
    fn a_batch_installs_valid_files_and_reports_the_rest() {
        let _guard = library();
        let outcome = import_theme_batch(&[
            file("aurora.json", &theme_json("Aurora")),
            file("broken.json", "{ nope"),
            ImportableThemeFile {
                name: "huge.json".to_string(),
                size: MAX_THEME_FILE_BYTES + 1,
                text: Ok(String::new()),
            },
            ImportableThemeFile {
                name: "unreadable.json".to_string(),
                size: 4,
                text: Err("permission denied".to_string()),
            },
        ]);
        assert_eq!(outcome.installed.len(), 1);
        assert!(outcome.conflicting.is_empty());
        let error = outcome.error().expect("failures");
        assert!(error.contains("broken.json: "), "{error}");
        assert!(error.contains("huge.json: too large"), "{error}");
        assert!(
            error.contains("unreadable.json: permission denied"),
            "{error}"
        );
        assert!(error.contains(" \u{2014} "), "{error}");
    }

    #[test]
    fn a_batch_sets_aside_ids_that_are_already_installed() {
        let _guard = library();
        submit_theme_json(&theme_json("Aurora"), &mut |_| true);
        let outcome = import_theme_batch(&[
            file("aurora.json", &theme_json("Aurora")),
            file("fresh.json", &theme_json("Fresh")),
        ]);
        assert_eq!(outcome.installed.len(), 1);
        assert_eq!(outcome.conflicting.len(), 1);
        assert_eq!(outcome.conflicting[0].id.as_str(), "aurora");
        assert_eq!(outcome.error(), None);
    }

    #[test]
    fn keep_both_installs_a_numbered_copy_and_update_replaces_in_place() {
        let _guard = library();
        submit_theme_json(&theme_json("Aurora"), &mut |_| true);
        let conflict =
            parse_theme_file(serde_json::from_str(&theme_json("Aurora")).unwrap()).expect("theme");

        let copy = resolve_import_conflicts(
            std::slice::from_ref(&conflict),
            None,
            ConflictResolution::Copy,
        );
        assert!(copy.failures.is_empty());
        assert_eq!(copy.resolved[0].label.as_str(), "Aurora (1)");
        let second = resolve_import_conflicts(
            std::slice::from_ref(&conflict),
            None,
            ConflictResolution::Copy,
        );
        assert_eq!(second.resolved[0].label.as_str(), "Aurora (2)");

        let mut changed = conflict.clone();
        changed.label = "Aurora".into();
        changed.colors.accent = "#ff0000".into();
        let updated = resolve_import_conflicts(&[changed], None, ConflictResolution::Update);
        assert!(updated.failures.is_empty());
        assert_eq!(get_custom_themes().len(), 3);
    }

    #[test]
    fn a_single_conflict_copy_prefers_the_source_file_name() {
        let _guard = library();
        submit_theme_json(&theme_json("Dracula"), &mut |_| true);
        let conflict =
            parse_theme_file(serde_json::from_str(&theme_json("Dracula")).unwrap()).expect("theme");
        let outcome = resolve_import_conflicts(
            &[conflict],
            Some("dracula-soft.json"),
            ConflictResolution::Copy,
        );
        assert_eq!(outcome.resolved[0].label.as_str(), "Dracula Soft");
    }

    #[test]
    fn versioned_copy_gives_up_after_ninety_nine_copies() {
        let _guard = library();
        submit_theme_json(&theme_json("Aurora"), &mut |_| true);
        let theme = get_custom_themes().remove(0);
        for _ in 1..100 {
            versioned_copy(&theme, None)
                .and_then(install_custom_theme)
                .expect("copy");
        }
        assert_eq!(
            versioned_copy(&theme, None).expect_err("exhausted"),
            "Too many copies of \"Aurora\"."
        );
    }
}
