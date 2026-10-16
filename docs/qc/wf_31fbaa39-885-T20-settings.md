# QC wf_31fbaa39-885 — T20 settings UI (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 9 · refuted: 2

## Lead report

Base checkout: /Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings (paths below are relative to it)

1. HIGH crates/app/src/settings/panels/providers.rs:1263 (generic env table at 1525-1545). Sensitive env values render as plaintext. The dedicated key row and every generic row use `self.input` with `variable.value`, ignoring `sensitive` and `value_redacted`. Save sets `value_redacted = Some(false)` (1543), so the typed secret stays visible. Footer copy at 1622 ("never returned to the app") is false. Fix: render sensitive rows with a masked input (secret_input-style, "Configured" placeholder when saved), keep secrets out of the displayed view model, and return `value_redacted: true` with an empty value after save, as upstream settingsSecrets.ts does. Correct the footer copy. Backend redaction layer does not exist yet, so the host side needs work too.

2. MEDIUM crates/app/src/settings/dialog.rs:263. CLIProxy management key ("hub-key") is typed into an unmasked input (`self.input`, rendered via `controls::text_field` at 312). Upstream uses SecretInput. Fix: `self.secret_input("hub-key", "", window, cx)`. The save read at 342 and the close-path clear at 281-286 still work.

3. MEDIUM crates/app/src/settings/panels/providers.rs:795 (data read at 766-771). Provider account email is shown in clear text after "Authenticated as". Upstream ProviderInstanceCard.tsx:203-217 uses RedactedSensitiveText (blur, reveal toggle). Fix: port RedactedSensitiveText, or reuse the source_control.rs:163-165 placeholder/reveal pattern on the email span. Not a credential, so PII-level severity. (Claimed line 801 is wrong; 795 is the render.)

4. MEDIUM crates/app/src/settings/controls.rs:77 (doc comment 56-57). Settings Switch uses THUMB=14, giving a 26x16 track and 12px thumb. Upstream default size at sm: is 16px (switch.tsx:21-24), so settings toggles should be 30x18 with a 14px thumb and 12px checked travel. The current values match size="sm", which only a few non-settings-row switches use. Fix: set `THUMB` to 16 and correct the doc comment to "thumb 14px, track 30x18".

5. LOW crates/app/src/settings/panels/source_control.rs:678 and 712. The fetch-interval reset and write use the General-panel helper `background_activity_override_settings` (panels_logic.rs:257-295), which pins all nine resolved values as overrides. Upstream SourceControlSettings.tsx:96-116 merges only current.overrides and drops undefined keys. Effect: later base-profile changes in Custom mode stop reaching the eight unrelated keys. Fix: add a source-control override helper that merges onto current.overrides and removes undefined keys, and use it at 678 and 712. Verifiers disagreed on severity (medium vs low); low is the better fit.

6. LOW crates/app/src/settings/provider_form.rs:745 (test fn; helpers at 307 and 314). Upstream ProviderInstanceCard.test.ts:180-237 has four pure-logic secret/environment cases that are not ported. Rule 5 requires porting them. The Rust test covers only set and clear with value_redacted None. No test calls read_provider_environment_variable or provider_environment_without_names, and none uses value_redacted Some(true). Fix: port the four cases as #[cfg(test)] assertions, or add a Deviations row explaining each omission. The sensitive-default concern was refuted: the only env driver (cursor) sets sensitive: true explicitly.

7. LOW crates/app/src/settings/dialog.rs:221-243 (usage_source_id_from_url). Hub id derivation differs from upstream `new URL(url).host`. Scheme-less input ("hub.example.ts.net:8318") and userinfo ("https://u:p@host") produce different persisted keys (Rust "cliproxy-u-p-host" vs upstream "cliproxy-host"). Unparseable input upstream slugs the raw text including path, not just the host. Fix: drop userinfo and mirror the URL().host fallback and raw-text slug behaviour. Add a Deviations row if the divergence is kept. Existing test (371-385) covers only the standard https case.

8. LOW crates/app/src/settings/panels/projects.rs:131-133. Blank project title is silently ignored. Upstream ProjectSettingsPanel.tsx:291-294 shows a warning toast "Project title cannot be empty". No toast or notice surface exists in crates/app/src/settings yet. Fix: show an equivalent warning when the trimmed title is empty, which needs a feedback mechanism; the rename is only skipped, not corrupted.

9. LOW crates/app/src/settings/layout.rs:346. Row description line-height is `rems(1.25)` (20px). Upstream settingsLayout.tsx:483 uses `leading-normal` (1.5 x 12px = 18px). The suggested `rems(1.5)` is wrong (that is 24px). Fix: `.line_height(relative(1.5))` (18px, as in dialog.rs:123 pattern). Leave the status line at layout.rs:354 unchanged.

VERDICT: FAIL (1 high: item 1; items 2-4 are medium and must also be fixed before PASS is reasonable).

## Confirmed findings (raw)

### [medium] CLIProxy management key is typed into an unmasked input
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/dialog.rs:263` · dimension: logic-fidelity · upheld 2/2
- Evidence: dialog.rs:263 `let key = self.input("hub-key", "", "", window, cx, |_, _, _, _| {});` is rendered by `field("Management key", &key)` (dialog.rs:300-311) through `controls::text_field`, which is not masked. The masked helper `SettingsView::secret_input` (view.rs:327-343, `.masked(true)`) exists and is used for the GitHub token (source_control.rs:861). Upstream AddUsageLimitSourceDialog.tsx:121-126 uses `<SecretInput id="usage-source-key" ...>` for the same field.
- Fix: Create the field with `self.secret_input("hub-key", "", window, cx)` and keep reading it from `this.inputs` on save. The close path already clears it.

### [high] Sensitive environment values render as plaintext, and nothing redacts them after save
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/panels/providers.rs:1263` · dimension: logic-fidelity · upheld 2/2
- Evidence: environment_field_row (providers.rs:1247-1270) builds the dedicated key input with `self.input(...)` and value `variable.value` unless `value_redacted == Some(true)`. The generic env table at providers.rs:1525 uses `self.input(...)` for every row, even when `variable.sensitive` is true; `sensitive` is only read for the toggle at 1548. On save, providers.rs:1543 sets `value_redacted = Some(false)`, so the typed secret stays in the view model and is shown on every render. No redaction layer exists in the crates: grep for valueRedacted/value_redacted/SECRET_REDACTED only hits provider_form.rs, panels/providers.rs, contracts/provider_instance.rs and provider-core/instance_environment.rs. Upstream renders SecretInput for sensitive values (ProviderInstanceCard.tsx:283-302 and 432-440, `configured={...valueRedacted === true}`) and the server strips values to valueRedacted:true (upstream/apps/server/src/settingsSecrets.ts:106-115).
- Fix: Render sensitive env rows with a masked input (`secret_input`-style, placeholder "Configured" when saved). Store secret values outside the displayed view model and return `value_redacted: true` with an empty value after save, as settingsSecrets.ts does.

### [medium] Provider account email is shown in clear text
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/panels/providers.rs:766` · dimension: logic-fidelity · upheld 2/2
- Evidence: providers.rs:766-771 reads `p.auth.email` and line 801 renders `parts.child("Authenticated as").child(email)` with no masking. Upstream ProviderInstanceCard.tsx:203-215 wraps the email in `RedactedSensitiveText` (aria-label "Toggle account email visibility"). ProviderInstanceCard.test.ts:91-130 asserts `expect(markup).not.toContain("developer@example.com")` and `blur-xs`.
- Fix: Port RedactedSensitiveText: show the email blurred or masked with a reveal toggle, as upstream does.

### [medium] Git fetch interval write and reset pin all background overrides
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/panels/source_control.rs:678` · dimension: logic-fidelity · upheld 2/2
- Evidence: source_control.rs:678-686 (reset) and 712-720 (write) call `background_activity_override_settings`, the General-panel helper. panels_logic.rs:257-295 materializes all nine resolved intervals and flags as overrides before applying the patch. Upstream SourceControlSettings.tsx uses its own local helper (lines 96-116), which spreads only `current.overrides` and then deletes undefined keys. The write at 410-412 sets only `automaticGitFetchInterval` and the reset at 388-390 only clears it. The Rust version therefore pins eight unrelated overrides to their current resolved values, so later profile changes stop applying to them.
- Fix: Add a source-control override helper that merges onto `current.overrides` and removes undefined keys, mirroring SourceControlSettings.tsx:96-116, and use it at source_control.rs:678 and 712.

### [low] Dropped upstream secret and environment tests, no Deviations row
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/provider_form.rs:745` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream ProviderInstanceCard.test.ts:180-237 has four secret-handling cases: replacing a redacted secret without keeping the marker, the secure default `sensitive ?? true` (ProviderInstanceCard.tsx:251,260), writing dedicated secrets as sensitive env vars, and separating dedicated secrets from the generic table via readProviderEnvironmentVariable/providerEnvironmentWithoutNames. The Rust test `environment_fields_set_replace_and_clear` (provider_form.rs:745-762) covers only set and clear with value_redacted None. No test calls `read_provider_environment_variable` (provider_form.rs:307) or `provider_environment_without_names` (provider_form.rs:314), and no test uses `value_redacted: Some(true)`. PORTING.md has no Deviations row for these cases. The sensitive default is also not ported: provider_form.rs:344,353 use `sensitive: field.sensitive` on a bool field.
- Fix: Port the four cases as #[cfg(test)] assertions, or add Deviations rows explaining why each was dropped.

### [low] Hub id derivation differs from URL().host for scheme-less or userinfo input
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/dialog.rs:221` · dimension: logic-fidelity · upheld 2/2
- Evidence: usage_source_id_from_url (dialog.rs:221-243) strips only the scheme and cuts at /?#, so userinfo stays in the host. Upstream AddUsageLimitSourceDialog.tsx:21-37 uses `new URL(url).host`, which drops userinfo and treats `hub.example.ts.net:8318` as scheme `hub.example.ts.net` with empty host (id `cliproxy-hub`). Rust gives `cliproxy-hub.example.ts.net-8318`, and `https://u:p@host` gives `cliproxy-u-p-host` where upstream gives `cliproxy-host`.
- Fix: Mirror URL().host: drop userinfo, and fall back to the empty-host id when the input has no `//` authority, as the URL parser does.

### [low] Empty project title is ignored without feedback
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T20-settings/crates/app/src/settings/panels/projects.rs:131` · dimension: logic-fidelity · upheld 2/2
- Evidence: projects.rs:131-133 `if title.is_empty() { return; }` drops the rename silently. Upstream ProjectSettingsPanel.tsx:291-294 shows a warning toast "Project title cannot be empty" before returning.
- Fix: Show the same warning (toast or inline message) when the trimmed title is empty.

### [medium] Settings Switch is 26x16 with a 12px thumb; upstream default Switch at sm: is 30x18 with a 14px thumb
- `crates/app/src/settings/controls.rs:77` · dimension: look-and-feel · upheld 2/2
- Evidence: controls.rs:77 `const THUMB: f32 = 14.;` gives track `.w(px(THUMB * 2. - 2.))` = 26px, `.h(px(THUMB + 2.))` = 16px, thumb `.size(px(THUMB - 2.))` = 12px, checked travel `THUMB - 4.` = 10px. Doc comment at controls.rs:56-57 claims 'thumb 14px, track 26x16', which does not match the code. Upstream upstream/apps/web/src/components/ui/switch.tsx:21-24: default size at sm: is `[--thumb-size:--spacing(4)]` = 16px, so track `h-[calc(var(--thumb-size)+2px)]` = 18px and `w-[calc(var(--thumb-size)*2-2px)]` = 30px. Thumb (switch.tsx:38) is `size-[calc(var(--thumb-size)-2px)]` = 14px, with checked `translate-x-[calc(var(--thumb-size)-4px)]` = 12px. Settings rows use the default size (e.g. upstream SettingsPanels.tsx:1125, 1425, 1711, 1737 render `<Switch>` with no size prop). The Rust switch is therefore about 13% narrower and 2px shorter than upstream, with a visibly smaller thumb in every settings toggle (screenshot dark-general.png shows the toggles at about 26px wide).
- Fix: Set `THUMB` to 16. (Track becomes 30x18, thumb 14, checked offset 12, mixed offset (16-4)/2 = 6, matching switch.tsx.) Correct the doc comment at controls.rs:56-57 to 'thumb 14px, track 30x18'.

### [low] Row description line-height is 1.25rem (20px); upstream leading-normal gives 18px
- `crates/app/src/settings/layout.rs:346` · dimension: look-and-feel · upheld 2/2
- Evidence: layout.rs:344-346: `.max_w(rems(36.)) .text_size(TEXT_XS.size) .line_height(rems(1.25))`. Upstream upstream/apps/web/src/components/settings/settingsLayout.tsx:483: `<p className="max-w-xl text-xs leading-normal text-muted-foreground/80">`. Tailwind v4 `leading-normal` is 1.5, so at text-xs (0.75rem) the line box is 18px. In screenshot dark-general.png the two-line description 'Resume usage-limit stops ... / continuation.' has a baseline step of about 36 display px, which is about 20 logical px at the 2x capture scale, not 18. Multi-line descriptions therefore sit about 11% looser than upstream and wrap at different heights.
- Fix: Use `.line_height(rems(1.5))` for the row description (`leading-normal`). Leave the status line and the unavailable-group message alone unless they are checked against their own upstream classes.
