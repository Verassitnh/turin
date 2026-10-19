//! Regression guard for the GPUI re-entrancy class of crash behind the pull request row click
//! (`app_sidebar_layout.rs`): a `cx.subscribe*` / `cx.observe*` callback runs with its own
//! entity already leased (`this`), so updating that entity again through a captured
//! `cx.entity().downgrade()` / `cx.weak_entity()` handle panics with
//! "cannot update <Entity> while it is already being updated". The panic happens inside the
//! platform mouse handler, which cannot unwind, so it aborts the app.
//!
//! No GPUI test harness is compiled into this workspace, so the guard is a source scan: every
//! `fn` in `crates/app/src` whose subscription/observation body updates a handle to its own
//! entity fails [`tests::subscriptions_never_update_their_own_entity`].

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// `cx.<method>(` calls whose callback leases the subscribing entity.
    const LEASING_CALLS: &[&str] = &[
        "cx.subscribe(",
        "cx.subscribe_in(",
        "cx.observe(",
        "cx.observe_in(",
    ];
    /// How many lines after the call the callback body is searched.
    const BODY_LINES: usize = 30;

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    fn self_handle_binding(line: &str) -> Option<String> {
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix("let ")?;
        let (name, tail) = rest.split_once(" = ")?;
        let name = name.trim_start_matches("mut ").trim();
        let is_self_handle =
            tail.starts_with("cx.entity().downgrade()") || tail.starts_with("cx.weak_entity()");
        (is_self_handle && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .then(|| name.to_owned())
    }

    fn is_fn_header(line: &str) -> bool {
        let trimmed = line.trim_start();
        trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub(crate) fn ")
            || trimmed.starts_with("pub(super) fn ")
            || trimmed.starts_with("async fn ")
    }

    /// `<name>.update(` or `<name>\n.update(` inside `body`.
    fn updates_handle(body: &str, name: &str) -> bool {
        let mut rest = body;
        while let Some(index) = rest.find(name) {
            let before_ok = index == 0
                || !rest[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let after = rest[index + name.len()..].trim_start();
            if before_ok && after.starts_with(".update(") {
                return true;
            }
            rest = &rest[index + name.len()..];
        }
        false
    }

    /// Every (file, line) where a leasing callback updates its own entity's handle.
    pub(super) fn offenders(root: &Path) -> Vec<String> {
        let mut files = Vec::new();
        rust_sources(root, &mut files);
        files.sort();
        let mut found = Vec::new();
        for file in files {
            // This file carries the offending shape as test fixtures.
            if file.ends_with(file!().rsplit('/').next().unwrap_or(file!())) {
                continue;
            }
            let source = std::fs::read_to_string(&file).unwrap_or_default();
            let lines: Vec<&str> = source.lines().collect();
            let mut handles: Vec<String> = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                if is_fn_header(line) {
                    handles.clear();
                }
                if let Some(name) = self_handle_binding(line) {
                    handles.push(name);
                }
                if !LEASING_CALLS.iter().any(|call| line.contains(call)) {
                    continue;
                }
                let end = (index + BODY_LINES).min(lines.len());
                let body = lines[index..end].join("\n");
                for name in &handles {
                    if updates_handle(&body, name) {
                        found.push(format!(
                            "{}:{}: `{name}.update(` inside its own subscription",
                            file.display(),
                            index + 1
                        ));
                    }
                }
            }
        }
        found
    }

    #[test]
    fn pull_request_crash_class_subscriptions_never_update_their_own_entity() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let found = offenders(&root);
        assert!(
            found.is_empty(),
            "callbacks that re-enter their own entity (this panics in GPUI):\n{}",
            found.join("\n")
        );
    }

    /// The exact shape that crashed the pull request row click: a layout subscribing to the
    /// route host and re-entering itself through a downgraded handle to mount the panel.
    #[test]
    fn pull_request_open_panel_reentrant_subscription_is_detected() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("layout.rs"),
            r#"
    fn open_pull_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = cx.entity().downgrade();
        let subscription = cx.subscribe_in(&host, window, move |_, _, event, _, cx| {
            if matches!(event, PullRequestsRouteEvent::OpenPanel) {
                layout
                    .update(cx, |this, cx| this.set_right_panel_content(None, cx))
                    .ok();
            }
        });
    }
"#,
        )
        .expect("write fixture");
        let found = offenders(dir.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].ends_with("layout.rs:4: `layout.update(` inside its own subscription"));
    }

    /// The fix: the callback uses the `this` it is given, so nothing is flagged.
    #[test]
    fn pull_request_open_panel_fixed_subscription_passes() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("layout.rs"),
            r#"
    fn open_pull_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.subscribe_in(&host, window, |this, _, event, _, cx| {
            this.on_pull_requests_route_event(event, cx);
        });
    }

    fn other(&mut self, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        window.defer(cx, move |_, cx| {
            weak.update(cx, |this, cx| cx.notify()).ok();
        });
    }
"#,
        )
        .expect("write fixture");
        assert!(offenders(dir.path()).is_empty());
    }
}
