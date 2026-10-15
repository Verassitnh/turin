# Upstream coverage inventory

Census of every non-test TypeScript source file in upstream T3 Code (`pingdotgg/t3code@ec1313c8`, `./upstream`) under `apps/server/src`, `apps/web/src`, `apps/desktop/src` and `packages/*/src`, classified against PORTING.md and TASKS.md, with the port status found on the Turin task branches. Generated 2026-10-15 from `main` plus every local `task/*` branch.

- **KEEP**: Turin ports the file; the owning task is given.
- **CUT**: dropped, with the PORTING.md reason.
- **UNASSIGNED**: Turin needs it but no task owns it yet. Each one carries a proposed new task (N1–N11, below).
- **Status** (KEEP only): **ported** = some branch has a Rust module whose `//! Port of upstream/<path>` header names the file directly; **partial** = the file is only named indirectly ("Port of the X half of …", "(layout only)", "see …") or a module with the same snake_case name exists without a header; **not started** = neither.

Lines are `wc -l` of the upstream file. Counts are files / lines.

## Summary

2,274 files / 681,075 lines in scope: 1160 / 377,385 KEEP, 979 / 259,363 CUT, 135 / 44,327 UNASSIGNED.

### Overall progress

| Measure | Lines | Percent |
|---|---:|---:|
| Ported, of KEEP lines | 152,349 / 377,385 | 40.4% |
| Ported + partial, of KEEP lines | 207,356 / 377,385 | 54.9% |
| Ported, of KEEP + UNASSIGNED lines (everything Turin needs) | 152,349 / 421,712 | 36.1% |
| Ported + partial, of KEEP + UNASSIGNED lines | 207,356 / 421,712 | 49.2% |

Partial files are counted at full length in the "+ partial" rows, so the true figure lies between the two rows of each pair.

### By area

| Area | All | KEEP | CUT | UNASSIGNED | Ported | Partial | Not started | Ported % of KEEP lines |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Server orchestration | 71 / 45,417 | 66 / 42,649 | 5 / 2,768 | 0 / 0 | 24 / 14,508 | 2 / 12,320 | 40 / 15,821 | 34.0% |
| Providers | 251 / 105,643 | 167 / 74,432 | 76 / 29,528 | 8 / 1,683 | 72 / 40,414 | 9 / 4,146 | 86 / 29,872 | 54.3% |
| Git / source control | 110 / 48,307 | 67 / 32,269 | 42 / 15,861 | 1 / 177 | 56 / 26,659 | 2 / 159 | 9 / 5,451 | 82.6% |
| Persistence | 85 / 6,460 | 82 / 5,510 | 3 / 950 | 0 / 0 | 82 / 5,510 | 0 / 0 | 0 / 0 | 100.0% |
| Server: projects | 15 / 5,734 | 0 / 0 | 6 / 1,550 | 9 / 4,184 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Server: runtime and other | 212 / 74,691 | 8 / 2,209 | 186 / 64,291 | 18 / 8,191 | 1 / 403 | 3 / 490 | 4 / 1,316 | 18.2% |
| Contracts | 68 / 27,633 | 37 / 14,964 | 31 / 12,669 | 0 / 0 | 36 / 13,931 | 1 / 1,033 | 0 / 0 | 93.1% |
| Shared | 119 / 24,492 | 85 / 18,238 | 34 / 6,254 | 0 / 0 | 56 / 10,392 | 6 / 2,334 | 23 / 5,512 | 57.0% |
| Client runtime | 198 / 39,892 | 102 / 21,977 | 96 / 17,915 | 0 / 0 | 55 / 11,533 | 5 / 1,867 | 42 / 8,577 | 52.5% |
| Web UI: composer | 96 / 30,498 | 96 / 30,498 | 0 / 0 | 0 / 0 | 1 / 101 | 3 / 8,620 | 92 / 21,777 | 0.3% |
| Web UI: cut features | 347 / 64,999 | 0 / 0 | 347 / 64,999 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: diff | 26 / 4,686 | 26 / 4,686 | 0 / 0 | 0 / 0 | 0 / 0 | 2 / 550 | 24 / 4,136 | 0.0% |
| Web UI: pull requests | 84 / 23,508 | 84 / 23,508 | 0 / 0 | 0 / 0 | 1 / 10 | 1 / 595 | 82 / 22,903 | 0.0% |
| Web UI: settings, themes, keybindings | 108 / 33,986 | 108 / 33,986 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 108 / 33,986 | 0.0% |
| Web UI: shell + design system | 80 / 12,555 | 80 / 12,555 | 0 / 0 | 0 / 0 | 19 / 4,709 | 7 / 1,834 | 54 / 6,012 | 37.5% |
| Web UI: sidebar | 35 / 16,800 | 35 / 16,800 | 0 / 0 | 0 / 0 | 20 / 9,477 | 2 / 5,077 | 13 / 2,246 | 56.4% |
| Web UI: state glue | 9 / 892 | 9 / 892 | 0 / 0 | 0 / 0 | 0 / 0 | 2 / 349 | 7 / 543 | 0.0% |
| Web UI: thread view | 108 / 42,212 | 108 / 42,212 | 0 / 0 | 0 / 0 | 15 / 14,702 | 10 / 15,633 | 83 / 11,877 | 34.8% |
| Web UI: unassigned (N10) | 4 / 573 | 0 / 0 | 0 / 0 | 4 / 573 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N11) | 2 / 520 | 0 / 0 | 0 / 0 | 2 / 520 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N2) | 13 / 3,060 | 0 / 0 | 0 / 0 | 13 / 3,060 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N4) | 16 / 4,722 | 0 / 0 | 0 / 0 | 16 / 4,722 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N5) | 7 / 1,501 | 0 / 0 | 0 / 0 | 7 / 1,501 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N6) | 14 / 5,850 | 0 / 0 | 0 / 0 | 14 / 5,850 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N7b) | 28 / 9,458 | 0 / 0 | 0 / 0 | 28 / 9,458 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N8) | 8 / 2,357 | 0 / 0 | 0 / 0 | 8 / 2,357 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Web UI: unassigned (N9) | 1 / 56 | 0 / 0 | 0 / 0 | 1 / 56 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Desktop (Electron) | 152 / 41,451 | 0 / 0 | 146 / 39,456 | 6 / 1,995 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| Remote-access packages | 7 / 3,122 | 0 / 0 | 7 / 3,122 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | n/a |
| **Total** | 2274 / 681,075 | 1160 / 377,385 | 979 / 259,363 | 135 / 44,327 | 438 / 152,349 | 55 / 55,007 | 667 / 170,029 | 40.4% |

### By owning task (KEEP files)

| Task | Scope | Board state | KEEP | Ported | Partial | Not started | Ported % |
|---|---|---|---:|---:|---:|---:|---:|
| T01 | contracts | done, QC queued | 37 / 14,964 | 36 / 13,931 | 1 / 1,033 | 0 / 0 | 93.1% |
| T02 | persistence | done (merged) | 83 / 6,424 | 82 / 5,510 | 1 / 914 | 0 / 0 | 85.8% |
| T03 | design system + shell | done (merged) | 82 / 13,654 | 21 / 5,808 | 7 / 1,834 | 54 / 6,012 | 42.5% |
| T05a | shared (contract-free) | r4 active | 43 / 6,060 | 37 / 5,246 | 1 / 143 | 5 / 671 | 86.6% |
| T05b | shared (contract-dependent) | active | 33 / 9,008 | 12 / 2,990 | 4 / 1,277 | 17 / 4,741 | 33.2% |
| T06 | git / vcs / checkpointing / workspace | r2 active | 25 / 13,173 | 24 / 13,138 | 0 / 0 | 1 / 35 | 99.7% |
| T07 | source control + GitHub | r3 active | 42 / 19,096 | 32 / 13,521 | 2 / 159 | 8 / 5,416 | 70.8% |
| T08 | provider core | done, QC queued | 63 / 10,683 | 36 / 5,049 | 1 / 182 | 26 / 5,452 | 47.3% |
| T08b | provider core remainder | active | 7 / 2,294 | 3 / 145 | 1 / 816 | 3 / 1,333 | 6.3% |
| T09 | Codex provider | r3 active | 29 / 13,837 | 6 / 9,063 | 2 / 156 | 21 / 4,618 | 65.5% |
| T09a | codex app-server protocol | done, QC pending | 8 / 1,909 | 6 / 1,755 | 0 / 0 | 2 / 154 | 91.9% |
| T10 | Claude provider | r3 active | 18 / 12,873 | 8 / 11,163 | 0 / 0 | 10 / 1,710 | 86.7% |
| T11 | OpenCode adapters | active | 11 / 10,435 | 4 / 8,828 | 2 / 945 | 5 / 662 | 84.6% |
| T11a | OpenCode transport | done, in QC | 6 / 2,290 | 5 / 1,152 | 1 / 1,138 | 0 / 0 | 50.3% |
| T12 | Cursor provider (ACP) | r1 done, r2 next | 22 / 16,663 | 3 / 429 | 1 / 495 | 18 / 15,739 | 2.6% |
| T12a | ACP protocol | done, in QC | 9 / 4,454 | 6 / 3,845 | 1 / 414 | 2 / 195 | 86.3% |
| T13a | orchestrator read side | r2 active | 14 / 9,867 | 8 / 8,425 | 0 / 0 | 6 / 1,442 | 85.4% |
| T13b | orchestrator write side | r2 active | 24 / 7,633 | 15 / 5,714 | 3 / 490 | 6 / 1,429 | 74.9% |
| T13c | orchestrator core + provider run services | todo | 30 / 25,379 | 0 / 0 | 2 / 12,320 | 28 / 13,059 | 0.0% |
| T14 | client view-model reducers | r2 active | 103 / 19,842 | 50 / 8,836 | 7 / 2,216 | 46 / 8,790 | 44.5% |
| T15 | sidebar UI | done, QC queued | 36 / 17,063 | 20 / 9,477 | 2 / 5,077 | 14 / 2,509 | 55.5% |
| T16 | thread view UI | done, QC queued | 115 / 45,353 | 21 / 17,810 | 10 / 15,633 | 84 / 11,910 | 39.3% |
| T17 | composer UI | active | 97 / 30,532 | 1 / 101 | 3 / 8,620 | 93 / 21,811 | 0.3% |
| T18 | diff UI | active | 26 / 4,686 | 0 / 0 | 2 / 550 | 24 / 4,136 | 0.0% |
| T19 | GitHub PR views | todo | 84 / 23,508 | 1 / 10 | 1 / 595 | 82 / 22,903 | 0.0% |
| T20 | settings + keybindings UI | todo | 109 / 34,293 | 0 / 0 | 0 / 0 | 109 / 34,293 | 0.0% |
| T21 | hooks | done (merged) | 1 / 403 | 1 / 403 | 0 / 0 | 0 / 0 | 100.0% |
| T22 | app wiring | todo | 3 / 1,009 | 0 / 0 | 0 / 0 | 3 / 1,009 | 0.0% |

T13c, T19, T20 and T22 have no branch yet; their few ported or partial hits are modules other tasks already wrote. Board state is copied from TASKS.md.

### UNASSIGNED, grouped into proposed tasks

| Proposed task | Files | Lines |
|---|---:|---:|
| N1 App runtime: settings, keybindings, config and storage | 7 | 3,803 |
| N2 Projects and onboarding | 22 | 7,244 |
| N3 Text generation | 8 | 1,683 |
| N4 Usage page | 23 | 7,780 |
| N5 Editor / Open-in integration | 10 | 2,667 |
| N6 Git UI and review backend | 15 | 6,027 |
| N7 GitHub media proxy | 1 | 164 |
| N7b App shell, routing and command palette | 28 | 9,458 |
| N8 Provider maintenance and update UI | 8 | 2,357 |
| N9 Native macOS shell | 7 | 2,051 |
| N10 Prompt links | 4 | 573 |
| N11 Project content search | 2 | 520 |
| **Total** | 135 | 44,327 |

#### N1: App runtime: settings, keybindings, config and storage (7 files, 3,803 lines)

Server-side settings service (settings.json read/write/watch, secrets), keybindings file loading/validation, data-dir config, local environment descriptor, storage cleanup. Crate: new `turin-runtime` (or `shared`/`app` modules). Prerequisite for T20 Settings UI and T22 wiring.

- `apps/server/src/config.ts` (274)
- `apps/server/src/environment/ServerEnvironment.ts` (294)
- `apps/server/src/environment/ServerEnvironmentLabel.ts` (196)
- `apps/server/src/keybindings.ts` (761)
- `apps/server/src/serverSettings.ts` (1,244)
- `apps/server/src/settingsSecrets.ts` (150)
- `apps/server/src/storageCleanup.ts` (884)

#### N2: Projects and onboarding (22 files, 7,244 lines)

ProjectService/ProjectMutation, RepositoryIdentityResolver, ProjectFavicon/Enrichment, ManagedProjectFolders (scratch project, new project from name), AgentSession scanner/importer (discover projects from Claude/Codex transcripts), first-run / welcome wizard, add-project flows. Backend into `turin-orchestration` (PORTING maps `apps/server/src/project` there), UI into `crates/app`.

- `apps/server/src/project/AgentSessionImporter.ts` (409)
- `apps/server/src/project/AgentSessionJson.ts` (201)
- `apps/server/src/project/AgentSessionScanner.ts` (1,494)
- `apps/server/src/project/ManagedProjectFolders.ts` (527)
- `apps/server/src/project/ProjectEnrichmentService.ts` (311)
- `apps/server/src/project/ProjectFaviconResolver.ts` (377)
- `apps/server/src/project/ProjectMutation.ts` (53)
- `apps/server/src/project/ProjectService.ts` (594)
- `apps/server/src/project/RepositoryIdentityResolver.ts` (218)
- `apps/web/src/components/NoProjectsHero.tsx` (55)
- `apps/web/src/components/onboarding/FirstRunGate.tsx` (238)
- `apps/web/src/components/onboarding/WelcomeWizard.tsx` (1,837)
- `apps/web/src/hooks/useNewProject.ts` (151)
- `apps/web/src/hooks/useScratchProject.ts` (93)
- `apps/web/src/onboarding/firstRun.logic.ts` (187)
- `apps/web/src/onboarding/firstRun.ts` (16)
- `apps/web/src/onboarding/projectImport.logic.ts` (161)
- `apps/web/src/onboarding/providerReadiness.logic.ts` (127)
- `apps/web/src/onboarding/targetEnvironment.logic.ts` (77)
- `apps/web/src/onboarding/useProjectScans.ts` (31)
- `apps/web/src/routes/welcome.tsx` (62)
- `apps/web/src/state/agentSessions.ts` (25)

#### N3: Text generation (8 files, 1,683 lines)

Provider-agnostic text generation (thread titles, commit messages, PR bodies, branch names): provider-core textGeneration*, threadTitleContext, apps/server textGeneration/*, plus per-provider generators for Codex, Claude, OpenCode and Cursor (Cursor must be rebuilt on ACP instead of the SDK). Seams already exist in T13b (ThreadTitleRegenerationService) and T06 (GitWorkflowService commit messages).

- `apps/server/src/textGeneration/ClaudeTextGeneration.ts` (345)
- `apps/server/src/textGeneration/CodexTextGeneration.ts` (416)
- `apps/server/src/textGeneration/TextGeneration.ts` (98)
- `apps/server/src/textGeneration/TextGenerationPresets.ts` (27)
- `apps/server/src/textGeneration/ThreadTitleLinks.ts` (45)
- `packages/provider-cursor/src/server/textGeneration.ts` (216)
- `packages/provider-opencode/src/server/textGeneration.ts` (328)
- `packages/provider-opencode/src/server/v2/textGeneration.ts` (208)

#### N4: Usage page (23 files, 7,780 lines)

UsageService (transcript scanning, aggregation, pricing, scan cache), usage-limit sources (incl. CLIProxyAPI), the Usage route and components/usage/*. Contracts `usage`/`usageLimitSourceId` are already kept.

- `apps/server/src/usage/UsageLimitSources.ts` (186)
- `apps/server/src/usage/UsageService.ts` (936)
- `apps/server/src/usage/cliproxyApi.ts` (379)
- `apps/server/src/usage/usageAggregation.ts` (373)
- `apps/server/src/usage/usagePricing.ts` (357)
- `apps/server/src/usage/usageScanCache.ts` (424)
- `apps/server/src/usage/usageTranscriptReader.ts` (403)
- `apps/web/src/components/usage/UsageLimits.tsx` (392)
- `apps/web/src/components/usage/UsageLimitsPooled.tsx` (653)
- `apps/web/src/components/usage/UsageModelDialog.tsx` (166)
- `apps/web/src/components/usage/UsagePage.tsx` (1,508)
- `apps/web/src/components/usage/UsagePriceOverrides.tsx` (628)
- `apps/web/src/components/usage/UsageProviderChart.tsx` (498)
- `apps/web/src/components/usage/UsageShareBar.tsx` (73)
- `apps/web/src/components/usage/usageBreakdown.ts` (86)
- `apps/web/src/components/usage/usagePagePreferences.ts` (35)
- `apps/web/src/components/usage/usagePriceForm.ts` (62)
- `apps/web/src/components/usage/usagePriceTable.ts` (135)
- `apps/web/src/components/usage/usagePriceTargets.ts` (81)
- `apps/web/src/components/usage/usageProviders.ts` (68)
- `apps/web/src/components/usage/usageShortcuts.ts` (39)
- `apps/web/src/routes/usage.tsx` (15)
- `apps/web/src/state/usage.ts` (283)

#### N5: Editor / Open-in integration (10 files, 2,667 lines)

ExternalLauncher (open in editor / file manager / browser), native app icon resolution, editor preferences and labels, OpenInPicker + shortcut, file context menu, JetBrains icons. Contracts `editor` already kept.

- `apps/server/src/assets/NativeAppIconResolver.ts` (274)
- `apps/server/src/assets/icns.ts` (30)
- `apps/server/src/process/externalLauncher.ts` (862)
- `apps/web/src/components/JetBrainsIcons.tsx` (610)
- `apps/web/src/components/chat/OpenInPicker.logic.ts` (22)
- `apps/web/src/components/chat/OpenInPicker.tsx` (470)
- `apps/web/src/components/chat/OpenInPickerShortcut.ts` (43)
- `apps/web/src/editorLabels.ts` (19)
- `apps/web/src/editorPreferences.ts` (131)
- `apps/web/src/fileContextMenu.ts` (206)

#### N6: Git UI and review backend (15 files, 6,027 lines)

Branch toolbar, branch picker, GitActionsControl (commit/push/PR flows UI over T06 GitWorkflowService), source-control action state, branch pagination, base-ref choices, workspace refresh; plus ReviewService (diff preview/file contents feeding T18). PORTING lists Git status/branches/commit/push as a kept feature with no UI owner.

- `apps/server/src/review/ReviewService.ts` (177)
- `apps/web/src/components/BranchPicker.tsx` (307)
- `apps/web/src/components/BranchToolbar.logic.ts` (329)
- `apps/web/src/components/BranchToolbar.tsx` (865)
- `apps/web/src/components/BranchToolbarBranchSelector.tsx` (827)
- `apps/web/src/components/BranchToolbarEnvModeSelector.tsx` (177)
- `apps/web/src/components/BranchToolbarEnvironmentSelector.tsx` (144)
- `apps/web/src/components/GitActionsControl.logic.ts` (488)
- `apps/web/src/components/GitActionsControl.tsx` (2,159)
- `apps/web/src/hooks/useWorkspaceMutationRefresh.ts` (30)
- `apps/web/src/lib/baseRefChoices.ts` (61)
- `apps/web/src/state/git.ts` (5)
- `apps/web/src/state/paginatedBranches.ts` (36)
- `apps/web/src/state/sourceControlActions.ts` (413)
- `apps/web/src/state/vcs.ts` (9)

#### N7: GitHub media proxy (1 files, 164 lines)

GitHubMediaFetch: authenticated fetch of private GitHub user-attachment images for PR markdown. Natural follow-up for T07 (backend) consumed by T19.

- `apps/server/src/assets/GitHubMediaFetch.ts` (164)

#### N7b: App shell, routing and command palette (28 files, 9,458 lines)

Route tree equivalents (__root, _chat, thread/draft/new routes), ThreadRouteView, right-panel store, UI-state persistence (uiStateStore, closed views + reopen), confirm dialogs, thread notifications, command palette (CommandPalette* ~4.6k lines), remaining app hooks (new thread, active thread/project). Could be folded into T22 if its scope is widened beyond the runtime bridge.

- `apps/web/src/closedViewStore.ts` (143)
- `apps/web/src/commandPaletteBus.ts` (39)
- `apps/web/src/components/CommandPalette.logic.ts` (623)
- `apps/web/src/components/CommandPalette.tsx` (3,667)
- `apps/web/src/components/CommandPaletteContent.tsx` (100)
- `apps/web/src/components/CommandPaletteResults.tsx` (247)
- `apps/web/src/components/ConfirmDialogHost.tsx` (96)
- `apps/web/src/components/RenderErrorBoundary.tsx` (41)
- `apps/web/src/components/ReopenClosedViewShortcut.tsx` (220)
- `apps/web/src/components/ThreadNotificationCoordinator.tsx` (255)
- `apps/web/src/components/ThreadRouteView.tsx` (198)
- `apps/web/src/confirmDialog.ts` (136)
- `apps/web/src/hooks/useActiveProjectTarget.ts` (41)
- `apps/web/src/hooks/useActiveThreadRef.ts` (20)
- `apps/web/src/hooks/useHandleNewThread.ts` (498)
- `apps/web/src/hooks/useNavigateBack.ts` (40)
- `apps/web/src/reopenClosedView.ts` (150)
- `apps/web/src/rightPanelStore.ts` (1,156)
- `apps/web/src/routes/__root.tsx` (628)
- `apps/web/src/routes/_chat.$environmentId.$threadId.tsx` (7)
- `apps/web/src/routes/_chat.draft.$draftId.tsx` (7)
- `apps/web/src/routes/_chat.index.tsx` (179)
- `apps/web/src/routes/_chat.new.tsx` (30)
- `apps/web/src/routes/_chat.tsx` (246)
- `apps/web/src/routes/projects.$projectKey.tsx` (17)
- `apps/web/src/threadNotifications.ts` (100)
- `apps/web/src/threadRoutes.ts` (106)
- `apps/web/src/uiStateStore.ts` (468)

#### N8: Provider maintenance and update UI (8 files, 2,357 lines)

Provider CLI update notifications, sidebar provider update pill, update actions, dismissal store. The backend (maintenanceResolver, ProviderLatestVersions) is T08b.

- `apps/web/src/components/ProviderUpdateEnvironmentRows.tsx` (409)
- `apps/web/src/components/ProviderUpdateLaunchNotification.environments.ts` (98)
- `apps/web/src/components/ProviderUpdateLaunchNotification.logic.ts` (864)
- `apps/web/src/components/ProviderUpdateLaunchNotification.tsx` (198)
- `apps/web/src/components/ProviderUpdatePrimaryNotification.tsx` (357)
- `apps/web/src/components/ProviderUpdatesAction.tsx` (128)
- `apps/web/src/components/sidebar/SidebarProviderUpdatePill.tsx` (210)
- `apps/web/src/providerUpdateDismissal.ts` (93)

#### N9: Native macOS shell (7 files, 2,051 lines)

GPUI equivalents of the Electron pieces Turin still needs: application menu, window lifecycle/state, quit-hold overlay, context-menu images, persisted client settings, dock notification badge.

- `apps/desktop/src/ipc/methods/notificationBadge.ts` (71)
- `apps/desktop/src/settings/DesktopClientSettings.ts` (234)
- `apps/desktop/src/window/ContextMenuImage.ts` (104)
- `apps/desktop/src/window/DesktopApplicationMenu.ts` (291)
- `apps/desktop/src/window/DesktopWindow.ts` (1,074)
- `apps/desktop/src/window/QuitHold.ts` (221)
- `apps/web/src/components/QuitHoldOverlay.tsx` (56)

#### N10: Prompt links (4 files, 573 lines)

Deep-link prompt landing (new thread from a link, hidden-character warning, copy prompt link). `client-runtime/promptLinks.ts` is already in T14; the UI surfaces are unowned. Keep or cut is a PM call.

- `apps/web/src/components/promptLinks/NewThreadLinkView.tsx` (379)
- `apps/web/src/components/promptLinks/ThreadLinkPrompt.tsx` (50)
- `apps/web/src/components/promptLinks/copyPromptLink.ts` (88)
- `apps/web/src/components/promptLinks/linkedPrompt.ts` (56)

#### N11: Project content search (2 files, 520 lines)

ProjectContentSearchDialog + HighlightedSearchLine over T06 WorkspaceSearchIndex.

- `apps/web/src/components/search/HighlightedSearchLine.tsx` (183)
- `apps/web/src/components/search/ProjectContentSearchDialog.tsx` (337)

### Notes for the PM

- **Cut files that a branch ported anyway.** A Turin `Port of` header names these even though PORTING.md cuts the feature (sometimes only one helper was copied); decide whether to keep or delete the code:
  - `apps/server/src/auth/ServerSecretStore.ts` → `T05b-shared-contracts:provider-core/src/credential_store.rs` (cut reason: T3 Connect / relay / remote access / auth)
  - `apps/server/src/persistence/reconcileV2PreviewMigration.ts` → `T23-t3-import:persistence/src/t3_import/mod.rs` (cut reason: Intentionally not called (PORTING Deviations))
  - `apps/server/src/project/ProjectSetupScriptRunner.ts` → `main:hooks/src/lib.rs` (cut reason: Thread launch into new worktree (replaced by hooks))
  - `apps/web/src/worktreeCleanup.ts` → `T15-sidebar:app/src/components/sidebar/thread_status_indicators.rs` (cut reason: Thread launch into new worktree (replaced by hooks))
  - `packages/contracts/src/background.ts` → `T05b-shared-contracts:provider-core/src/background_scope.rs` (cut reason: PORTING contracts cut list)
  - `packages/shared/src/AgentScope.ts` → `T05a-shared-pure:shared/src/agent_scope.rs` (cut reason: Linux systemd resource scoping (resource telemetry))
  - `packages/shared/src/connectAuth.ts` → `T05a-shared-pure:shared/src/prompt_links.rs` (cut reason: T3 Connect / relay / remote access / auth)
  - `packages/shared/src/hostClassification.ts` → `T05a-shared-pure:shared/src/host_classification.rs` (cut reason: T3 Connect / relay / remote access / auth)
  - `packages/shared/src/preview.ts` → `T05a-shared-pure:shared/src/provider_auth_return_url.rs` (cut reason: Browser / preview / devices)
  - `packages/shared/src/video.ts` → `T05a-shared-pure:shared/src/video.rs` (cut reason: Browser / preview / devices)
- **Duplicate ports.** These upstream files are named in a `Port of` header in more than one crate (sometimes only a helper or a view-side subset); full duplicates should be collapsed to one copy before merge (owning task in brackets):
  - `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts` [T09] → `codex-app-server`, `provider-codex`
  - `apps/server/src/processRunner.ts` [T21] → `git`, `hooks`
  - `packages/contracts/src/assistantCitations.ts` [T01] → `app`, `contracts`
  - `packages/contracts/src/chatAttachment.ts` [T01] → `app`, `contracts`
  - `packages/contracts/src/orchestrationV2.ts` [T01] → `app`, `contracts`
  - `packages/contracts/src/providerRuntime.ts` [T01] → `app`, `contracts`
  - `packages/contracts/src/server.ts` [T01] → `app`, `provider-core`
  - `packages/provider-core/src/server/collectStreamText.ts` [T08] → `provider-core`, `source-control`
  - `packages/provider-core/src/server/processGroup.ts` [T08] → `provider-core`, `provider-opencode`
  - `packages/shared/src/KeyedLock.ts` [T05a] → `orchestration`, `shared`
  - `packages/shared/src/changeRequestUrl.ts` [T05b] → `app`, `git`, `orchestration`, `source-control`
  - `packages/shared/src/codexArtifactTemplates.ts` [T09] → `app`, `shared`
  - `packages/shared/src/codexFileCitations.ts` [T09] → `app`, `shared`
  - `packages/shared/src/fileLinks.ts` [T05a] → `app`, `shared`
  - `packages/shared/src/filePreview.ts` [T05a] → `app`, `shared`
  - `packages/shared/src/git.ts` [T05b] → `git`, `source-control`
  - `packages/shared/src/gitPatchPath.ts` [T05a] → `git`, `shared`
  - `packages/shared/src/githubMarkdownLinks.ts` [T05a] → `shared`, `source-control`
  - `packages/shared/src/githubMedia.ts` [T05a] → `shared`, `source-control`
  - `packages/shared/src/markdownLinks.ts` [T05a] → `app`, `shared`
  - `packages/shared/src/orchestrationV2PendingBackgroundWork.ts` [T05b] → `app`, `orchestration`
  - `packages/shared/src/orchestrationV2ThreadError.ts` [T05b] → `app`, `orchestration`
  - `packages/shared/src/orchestrationV2Timeline.ts` [T05b] → `app`, `orchestration`
  - `packages/shared/src/path.ts` [T05a] → `app`, `shared`
  - `packages/shared/src/semver.ts` [T05a] → `provider-opencode`, `shared`
  - `packages/shared/src/threadPullRequests.ts` [T05b] → `app`, `orchestration`, `persistence`, `source-control`
  - `packages/source-control-core/src/client/definition.ts` [T07] → `app`, `source-control`
  - `packages/source-control-github/src/client/definition.ts` [T07] → `app`, `source-control`
- **Assignments made here without an explicit brief line** (directory or crate ownership used instead): every `orchestration-v2` file not in the T13a/T13b briefs → T13c (TASKS.md scope); `Drivers/Claude*`, `claude*` → T10 and `Drivers/Codex*`, `codex*` → T09; remaining provider-agnostic `apps/server/src/provider/*` → T08; provider-core `textGeneration*` → T08 (crate owner; `text_generation.rs` exists); `client-runtime/work-log/*` → T16 (ported in T16-logic); `apps/web/src/components/chat/*` → T16 unless composer-specific (T17); `components/settings/*` → T20; `components/pullRequest/*` → T19; `components/ui/*` → T03; `apps/server/src/{os-jank,serverRuntimeStartup,serverRuntimeState}.ts` → T22.
- **Persistence migrations** are ported as extracted SQL (`crates/persistence/sql/NNN_*.sql`); the `Migrations/OrchestrationV2/*` sub-migrations live inside `055_OrchestrationV2.sql` (PORTING Deviations). Auth, scheduled-task and webhook migrations are KEEP because the migration ledger must match upstream for T3 database import.
- **Status is mechanical.** "Ported" means a header exists on some branch, not that QC passed or that the port is complete. Branches still in flight (T05b, T06, T07, T08b, T09–T14, T17, T18) will move numbers quickly; rerun this census after merges.

Excluded from the census (generated code, test harnesses and fixtures that are not test-named):

- `apps/desktop/src/preview/AnnotationStyles.generated.ts`
- `apps/desktop/src/updates/updatesTestHarness.ts`
- `apps/server/src/preview/testing/DesktopPipeLifecycle.fixture.ts`
- `apps/server/src/provider/ClaudeModelCatalog.testFixtures.ts`
- `apps/server/src/provider/testUtils/providerRegistryMock.ts`
- `apps/server/src/testUtils/gitConfig.setup.ts`
- `apps/server/src/testUtils/weightedShardSequencer.ts`
- `apps/server/src/textGeneration/OpenCode2TextGeneration.fixture.ts`
- `apps/server/src/usage/usageTestFormats.ts`
- `apps/server/src/vcs/testing/VcsDriverContractHarness.ts`
- `apps/web/src/routeTree.gen.ts`
- `apps/web/src/test-fixtures.ts`
- `packages/client-runtime/src/state/orchestrationV2TestFixtures.ts`
- `packages/effect-acp/src/_generated/meta.gen.ts`
- `packages/effect-acp/src/_generated/schema-v1.gen.ts`
- `packages/effect-acp/src/_generated/schema.gen.ts`
- `packages/effect-codex-app-server/src/_generated/meta.gen.ts`
- `packages/effect-codex-app-server/src/_generated/namespaces.gen.ts`
- `packages/effect-codex-app-server/src/_generated/schema.gen.ts`
- `packages/provider-acp-registry/src/testing.ts`
- `packages/provider-cursor/src/testing.ts`
- `packages/provider-grok/src/testing.ts`
- `packages/provider-muse/src/testing.ts`
- `packages/provider-opencode/src/server/probeResponses.fixture.ts`
- `packages/provider-opencode/src/testing.ts`
- `packages/provider-testing/src/TestProviderHost.ts`
- `packages/provider-testing/src/fakeCli.ts`
- `packages/provider-testing/src/replayGate.ts`
- `packages/provider-testing/src/replayTranscript.ts`
- `packages/provider-testing/src/replayWorkspace.ts`
- `packages/shared/src/testing/longTempDir.ts`
- `packages/shared/src/testing/symlinks.ts`
- `packages/source-control-testing/src/TestSourceControlHost.ts`

## Full inventory

Grouped by upstream directory. Task/Reason is the owning task for KEEP, the cut reason for CUT and the proposed task for UNASSIGNED. Turin module is the first module found (branch:crate path).

### `apps/server/src/`

37 files, 12,888 lines: KEEP 12 / 2,326, CUT 20 / 7,249, UNASSIGNED 5 / 3,313, ported 2 / 462.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| attachmentPaths.ts | 27 | KEEP | T13b | partial | `T13b-orch-pipeline:orchestration/src/pipeline/attachment_store.rs` |
| attachmentStore.ts | 324 | KEEP | T13b | partial | `T13b-orch-pipeline:orchestration/src/pipeline/attachment_store.rs` |
| bin.ts | 27 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| binCli.ts | 100 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| bootstrap.ts | 241 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| claude-history-worker.ts | 7 | KEEP | T10 | not started |  |
| claudeHistoryWorker.ts | 37 | KEEP | T10 | not started |  |
| claudeModelOptions.ts | 59 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_model_options.rs` |
| cliAuthFormat.ts | 192 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| codexModelOptions.ts | 14 | KEEP | T09 | not started |  |
| compileCache.ts | 9 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| config.ts | 274 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| entrypoint.ts | 38 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| environmentTheme.ts | 307 | KEEP | T20 | not started |  |
| http.ts | 703 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| httpCors.ts | 14 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| httpResponseErrorGuard.ts | 39 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| imageMime.ts | 139 | KEEP | T13b | partial | `T13b-orch-pipeline:orchestration/src/pipeline/attachment_store.rs` |
| keybindings.ts | 761 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| os-jank.ts | 118 | KEEP | T22 | not started |  |
| processRunner.ts | 403 | KEEP | T21 | ported | `T06-git:git/src/process.rs` |
| rpcInitialItems.ts | 16 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| server.ts | 1,116 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| serverActivation.ts | 32 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| serverLifecycleEvents.ts | 56 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| serverLogger.ts | 49 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| serverOwnership.ts | 147 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| serverRuntimeStartup.ts | 700 | KEEP | T22 | not started |  |
| serverRuntimeState.ts | 191 | KEEP | T22 | not started |  |
| serverSettings.ts | 1,244 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| serviceLauncher.ts | 875 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| settingsSecrets.ts | 150 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| startupAccess.ts | 148 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| storageCleanup.ts | 884 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| worktreesDirectory.ts | 47 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| ws.ts | 3,284 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| zipArchive.ts | 116 | CUT | Only used by preview browser import and Antigravity install |  |  |

### `apps/server/src/assets/`

6 files, 1,837 lines: KEEP 0 / 0, CUT 3 / 1,369, UNASSIGNED 3 / 468, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AssetAccess.ts | 990 | CUT | HTTP asset/attachment transport (UI reads files in-process) |  |  |
| AttachmentUpload.ts | 243 | CUT | HTTP asset/attachment transport (UI reads files in-process) |  |  |
| GitHubMediaFetch.ts | 164 | **UNASSIGNED** | N7 GitHub media proxy |  |  |
| MediaFile.ts | 136 | CUT | HTTP asset/attachment transport (UI reads files in-process) |  |  |
| NativeAppIconResolver.ts | 274 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| icns.ts | 30 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |

### `apps/server/src/auth/`

14 files, 5,569 lines: KEEP 0 / 0, CUT 14 / 5,569, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| EnvironmentAuth.ts | 1,392 | CUT | T3 Connect / relay / remote access / auth |  |  |
| EnvironmentAuthPolicy.ts | 60 | CUT | T3 Connect / relay / remote access / auth |  |  |
| McpOAuth.ts | 599 | CUT | T3 Connect / relay / remote access / auth |  |  |
| PairingGrantStore.ts | 641 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ReusableDevAuth.ts | 32 | CUT | T3 Connect / relay / remote access / auth |  |  |
| RpcAuthorization.ts | 290 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ServerSecretStore.ts | 317 | CUT | T3 Connect / relay / remote access / auth |  |  |
| SessionStore.ts | 1,102 | CUT | T3 Connect / relay / remote access / auth |  |  |
| dpop.ts | 126 | CUT | T3 Connect / relay / remote access / auth |  |  |
| http.ts | 530 | CUT | T3 Connect / relay / remote access / auth |  |  |
| mcpOAuthHtml.ts | 46 | CUT | T3 Connect / relay / remote access / auth |  |  |
| mcpOAuthHttp.ts | 159 | CUT | T3 Connect / relay / remote access / auth |  |  |
| replayMarkers.ts | 77 | CUT | T3 Connect / relay / remote access / auth |  |  |
| utils.ts | 198 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/server/src/background/`

2 files, 451 lines: KEEP 0 / 0, CUT 2 / 451, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| BackgroundPolicy.ts | 350 | CUT | Background activity leases (contracts `background` cut) |  |  |
| HostPowerMonitor.ts | 101 | CUT | Background activity leases (contracts `background` cut) |  |  |

### `apps/server/src/checkpointing/`

5 files, 687 lines: KEEP 5 / 687, CUT 0 / 0, UNASSIGNED 0 / 0, ported 5 / 687.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| CheckpointDiffQuery.ts | 310 | KEEP | T06 | ported | `T06-git:git/src/checkpointing/checkpoint_diff_query.rs` |
| CheckpointStore.ts | 202 | KEEP | T06 | ported | `T06-git:git/src/checkpointing/checkpoint_store.rs` |
| Diffs.ts | 53 | KEEP | T06 | ported | `T06-git:git/src/checkpointing/diffs.rs` |
| Errors.ts | 94 | KEEP | T06 | ported | `T06-git:git/src/checkpointing/errors.rs` |
| Utils.ts | 28 | KEEP | T06 | ported | `T06-git:git/src/checkpointing/utils.rs` |

### `apps/server/src/cli/`

23 files, 6,165 lines: KEEP 0 / 0, CUT 23 / 6,165, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| acpMcpBridge.ts | 34 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| app.ts | 259 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| auth.ts | 249 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| authScopes.ts | 11 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| browser.ts | 191 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| claudeHistory.ts | 27 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| config.ts | 570 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| connect.ts | 733 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| invocation.ts | 209 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| pair.ts | 543 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| project.ts | 586 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| server.ts | 70 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| service.ts | 299 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| serviceLauncher.ts | 27 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| servicePreflight.ts | 17 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| sshHelper.ts | 143 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| theme.ts | 584 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| trace.ts | 217 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| triage.ts | 282 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| triagePrompt.ts | 232 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| uninstall.ts | 218 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| update.ts | 592 | CUT | Server CLI (Turin is a single GPUI app) |  |  |
| updateProgress.ts | 72 | CUT | Server CLI (Turin is a single GPUI app) |  |  |

### `apps/server/src/cloud/`

21 files, 6,673 lines: KEEP 0 / 0, CUT 21 / 6,673, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| CliState.ts | 103 | CUT | T3 Connect / relay / remote access / auth |  |  |
| CliTokenManager.ts | 591 | CUT | T3 Connect / relay / remote access / auth |  |  |
| CloudLink.ts | 1,738 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ManagedEndpointRuntime.ts | 701 | CUT | T3 Connect / relay / remote access / auth |  |  |
| bootService.ts | 1,005 | CUT | T3 Connect / relay / remote access / auth |  |  |
| cliAuthHtml.ts | 155 | CUT | T3 Connect / relay / remote access / auth |  |  |
| config.ts | 106 | CUT | T3 Connect / relay / remote access / auth |  |  |
| environmentKeys.ts | 104 | CUT | T3 Connect / relay / remote access / auth |  |  |
| http.ts | 161 | CUT | T3 Connect / relay / remote access / auth |  |  |
| linkChecks.ts | 151 | CUT | T3 Connect / relay / remote access / auth |  |  |
| managedTunnelStartup.ts | 80 | CUT | T3 Connect / relay / remote access / auth |  |  |
| pinnedRuntime.ts | 380 | CUT | T3 Connect / relay / remote access / auth |  |  |
| publicConfig.ts | 218 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayResponse.ts | 83 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayTracing.ts | 18 | CUT | T3 Connect / relay / remote access / auth |  |  |
| selfUpdate.ts | 346 | CUT | T3 Connect / relay / remote access / auth |  |  |
| serviceLauncherClient.ts | 253 | CUT | T3 Connect / relay / remote access / auth |  |  |
| servicePreflight.ts | 59 | CUT | T3 Connect / relay / remote access / auth |  |  |
| serviceProtocol.ts | 268 | CUT | T3 Connect / relay / remote access / auth |  |  |
| traceRelayRequest.ts | 77 | CUT | T3 Connect / relay / remote access / auth |  |  |
| updateHandoff.ts | 76 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/server/src/desktopUpdate/`

1 files, 233 lines: KEEP 0 / 0, CUT 1 / 233, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopAppUpdate.ts | 233 | CUT | Auto-updates (app / server) |  |  |

### `apps/server/src/device/`

12 files, 4,250 lines: KEEP 0 / 0, CUT 12 / 4,250, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AgentDeviceShim.ts | 60 | CUT | Browser / preview / devices |  |  |
| AgentDeviceTarget.ts | 49 | CUT | Browser / preview / devices |  |  |
| DeviceActions.ts | 568 | CUT | Browser / preview / devices |  |  |
| DeviceHost.ts | 141 | CUT | Browser / preview / devices |  |  |
| DeviceHubProxy.ts | 197 | CUT | Browser / preview / devices |  |  |
| DeviceService.ts | 1,186 | CUT | Browser / preview / devices |  |  |
| DeviceToolchain.ts | 268 | CUT | Browser / preview / devices |  |  |
| LocalDeviceHost.ts | 856 | CUT | Browser / preview / devices |  |  |
| SshDeviceHost.ts | 479 | CUT | Browser / preview / devices |  |  |
| deviceToolMaintenance.ts | 151 | CUT | Browser / preview / devices |  |  |
| localSshDeviceHost.ts | 71 | CUT | Browser / preview / devices |  |  |
| sshDeviceScript.ts | 224 | CUT | Browser / preview / devices |  |  |

### `apps/server/src/diagnostics/`

3 files, 746 lines: KEEP 0 / 0, CUT 3 / 746, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ProcessDiagnostics.ts | 167 | CUT | Telemetry / observability / resource telemetry |  |  |
| ProcessResourceMonitor.ts | 88 | CUT | Telemetry / observability / resource telemetry |  |  |
| TraceDiagnostics.ts | 491 | CUT | Telemetry / observability / resource telemetry |  |  |

### `apps/server/src/environment/`

5 files, 912 lines: KEEP 0 / 0, CUT 3 / 422, UNASSIGNED 2 / 490, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DirectEndpoints.ts | 161 | CUT | T3 Connect / relay / remote access / auth |  |  |
| RemoteOpenTargets.ts | 87 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ServerEnvironment.ts | 294 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| ServerEnvironmentLabel.ts | 196 | **UNASSIGNED** | N1 App runtime: settings, keybindings, config and storage |  |  |
| ServerEnvironmentMachine.ts | 174 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/server/src/git/`

6 files, 3,855 lines: KEEP 6 / 3,855, CUT 0 / 0, UNASSIGNED 0 / 0, ported 5 / 3,820.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| GitManager.ts | 3,208 | KEEP | T06 | ported | `T06-git:git/src/git_manager.rs` |
| GitWorkflowService.ts | 427 | KEEP | T06 | ported | `T06-git:git/src/git_workflow_service.rs` |
| detachStackFrame.ts | 20 | KEEP | T06 | ported | `T06-git:git/src/detach_stack_frame.rs` |
| linkCreatedPullRequest.ts | 94 | KEEP | T06 | ported | `T06-git:git/src/link_created_pull_request.rs` |
| refreshPushedPullRequests.ts | 35 | KEEP | T06 | not started |  |
| remoteRefs.ts | 71 | KEEP | T06 | ported | `T06-git:git/src/remote_refs.rs` |

### `apps/server/src/htmlRender/`

3 files, 1,329 lines: KEEP 0 / 0, CUT 3 / 1,329, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| HtmlRender.ts | 486 | CUT | Browser / preview / devices |  |  |
| headlessChrome.ts | 599 | CUT | Browser / preview / devices |  |  |
| publicProxy.ts | 244 | CUT | Browser / preview / devices |  |  |

### `apps/server/src/mcp/`

10 files, 6,329 lines: KEEP 0 / 0, CUT 10 / 6,329, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AcpMcpStdioBridge.ts | 365 | CUT | MCP apps / orchestrator MCP server |  |  |
| McpHttpServer.ts | 881 | CUT | MCP apps / orchestrator MCP server |  |  |
| McpInvocationContext.ts | 133 | CUT | MCP apps / orchestrator MCP server |  |  |
| McpSessionRegistry.ts | 266 | CUT | MCP apps / orchestrator MCP server |  |  |
| McpToolAccess.ts | 324 | CUT | MCP apps / orchestrator MCP server |  |  |
| OrchestratorMcpService.ts | 2,709 | CUT | MCP apps / orchestrator MCP server |  |  |
| PreviewAutomationBroker.ts | 711 | CUT | MCP apps / orchestrator MCP server |  |  |
| ThreadMetadataMcpService.ts | 223 | CUT | MCP apps / orchestrator MCP server |  |  |
| WorktreeMcpService.ts | 509 | CUT | MCP apps / orchestrator MCP server |  |  |
| threadAccess.ts | 208 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/attachment/`

3 files, 209 lines: KEEP 0 / 0, CUT 3 / 209, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 127 | CUT | MCP apps / orchestrator MCP server |  |  |
| input.ts | 10 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 72 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/device/`

2 files, 385 lines: KEEP 0 / 0, CUT 2 / 385, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 275 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 110 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/environment/`

2 files, 142 lines: KEEP 0 / 0, CUT 2 / 142, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 75 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 67 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/html/`

2 files, 177 lines: KEEP 0 / 0, CUT 2 / 177, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 64 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 113 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/orchestrator/`

2 files, 451 lines: KEEP 0 / 0, CUT 2 / 451, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 158 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 293 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/preview/`

2 files, 630 lines: KEEP 0 / 0, CUT 2 / 630, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 272 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 358 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/previewControls/`

2 files, 94 lines: KEEP 0 / 0, CUT 2 / 94, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 47 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 47 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/project/`

2 files, 445 lines: KEEP 0 / 0, CUT 2 / 445, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 283 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 162 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/pullRequests/`

2 files, 953 lines: KEEP 0 / 0, CUT 2 / 953, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 531 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 422 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/thread/`

2 files, 619 lines: KEEP 0 / 0, CUT 2 / 619, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 327 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 292 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcp/toolkits/worktree/`

2 files, 144 lines: KEEP 0 / 0, CUT 2 / 144, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| handlers.ts | 55 | CUT | MCP apps / orchestrator MCP server |  |  |
| tools.ts | 89 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/mcpApps/`

3 files, 450 lines: KEEP 0 / 0, CUT 3 / 450, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| McpAppModelContext.ts | 105 | CUT | MCP apps / orchestrator MCP server |  |  |
| McpAppRequests.ts | 237 | CUT | MCP apps / orchestrator MCP server |  |  |
| McpAppSnapshot.ts | 108 | CUT | MCP apps / orchestrator MCP server |  |  |

### `apps/server/src/observability/`

9 files, 959 lines: KEEP 0 / 0, CUT 9 / 959, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| Attributes.ts | 49 | CUT | Telemetry / observability / resource telemetry |  |  |
| BrowserTraceCollector.ts | 24 | CUT | Telemetry / observability / resource telemetry |  |  |
| DefectReporter.ts | 24 | CUT | Telemetry / observability / resource telemetry |  |  |
| EventLoopMonitor.ts | 188 | CUT | Telemetry / observability / resource telemetry |  |  |
| HeapSnapshot.ts | 52 | CUT | Telemetry / observability / resource telemetry |  |  |
| Metrics.ts | 161 | CUT | Telemetry / observability / resource telemetry |  |  |
| Observability.ts | 106 | CUT | Telemetry / observability / resource telemetry |  |  |
| OtlpEndpointCheck.ts | 98 | CUT | Telemetry / observability / resource telemetry |  |  |
| RpcInstrumentation.ts | 257 | CUT | Telemetry / observability / resource telemetry |  |  |

### `apps/server/src/orchestration-v2/`

70 files, 44,584 lines: KEEP 66 / 42,649, CUT 4 / 1,935, UNASSIGNED 0 / 0, ported 24 / 14,508.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AttachmentClaims.ts | 149 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/attachment_claims.rs` |
| CheckpointCaptureService.ts | 285 | KEEP | T13c | not started |  |
| CheckpointRestoreSafety.ts | 88 | KEEP | T13c | not started |  |
| CheckpointRollbackService.ts | 373 | KEEP | T13c | not started |  |
| CheckpointService.ts | 587 | KEEP | T13c | partial | `T06-git:git/src/checkpointing/utils.rs` |
| CommandPolicy.ts | 491 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/command_policy.rs` |
| CommandReceiptStore.ts | 212 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/command_receipt_store.rs` |
| ContextHandoffDelivery.ts | 161 | KEEP | T13c | not started |  |
| ContextHandoffService.ts | 498 | KEEP | T13c | not started |  |
| DispatchModeLimit.ts | 52 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/dispatch_mode_limit.rs` |
| EffectOutbox.ts | 739 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/effect_outbox.rs` |
| EffectWorker.ts | 917 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/effect_worker.rs` |
| EventSink.ts | 961 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/event_sink.rs` |
| EventStore.ts | 154 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/event_store.rs` |
| LiveStreamBudget.ts | 342 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| NotificationMailbox.ts | 39 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/notification_mailbox.rs` |
| Orchestrator.ts | 11,733 | KEEP | T13c | partial | `T13b-orch-pipeline:orchestration/src/pipeline/orchestrator_port.rs` |
| ProjectCommands.ts | 241 | KEEP | T13b | ported | `T13a-orch-projections:orchestration/src/projection/project_commands.rs` |
| ProjectStore.ts | 278 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/project_store.rs` |
| ProjectionMaintenance.ts | 376 | KEEP | T13a | not started |  |
| ProjectionStore.ts | 6,840 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/store/mod.rs` |
| ProviderAdapterRegistry.ts | 369 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_adapter_registry.rs` |
| ProviderContinuationService.ts | 319 | KEEP | T13c | not started |  |
| ProviderEventIngestor.ts | 834 | KEEP | T13c | not started |  |
| ProviderRuntimeRecoveryService.ts | 787 | KEEP | T13c | not started |  |
| ProviderSessionManager.ts | 2,594 | KEEP | T13c | not started |  |
| ProviderSessionTransitionPolicy.ts | 96 | KEEP | T13c | not started |  |
| ProviderSwitchService.ts | 219 | KEEP | T13c | not started |  |
| ProviderTurnControlService.ts | 409 | KEEP | T13c | not started |  |
| ProviderTurnStartService.ts | 1,291 | KEEP | T13c | not started |  |
| PullRequestSyncReactor.ts | 494 | KEEP | T13c | not started |  |
| PullRequestWatchReactor.ts | 652 | KEEP | T13c | not started |  |
| QueuedRunOrder.ts | 32 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/queued_run_order.rs` |
| ResourceCleanupService.ts | 85 | KEEP | T13b | not started |  |
| RestartBackgroundNote.ts | 314 | KEEP | T13c | not started |  |
| RestartContinuation.ts | 339 | KEEP | T13c | not started |  |
| RunExecutionService.ts | 1,543 | KEEP | T13c | not started |  |
| RunFinalizationService.ts | 121 | KEEP | T13c | not started |  |
| RuntimePolicy.ts | 167 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/runtime_policy.rs` |
| RuntimeRequestService.ts | 138 | KEEP | T13c | not started |  |
| ShellStream.ts | 348 | KEEP | T13a | not started |  |
| ThreadCommandExecutor.ts | 12 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/thread_command_executor.rs` |
| ThreadDeletion.ts | 234 | KEEP | T13b | not started |  |
| ThreadFind.ts | 449 | KEEP | T13a | not started |  |
| ThreadForkService.ts | 139 | KEEP | T13b | not started |  |
| ThreadLaunchService.ts | 1,096 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| ThreadLifecycleService.ts | 142 | KEEP | T13b | not started |  |
| ThreadLiveEventCoalescer.ts | 255 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/thread_live_event_coalescer.rs` |
| ThreadManagementService.ts | 1,220 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/thread_management_service.rs` |
| ThreadMessageIntake.ts | 250 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/thread_message_intake.rs` |
| ThreadPullRequestService.ts | 416 | KEEP | T13c | not started |  |
| ThreadSearch.ts | 176 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/thread_search.rs` |
| ThreadSettlementService.ts | 674 | KEEP | T13b | not started |  |
| ThreadStream.ts | 124 | KEEP | T13a | not started |  |
| ThreadTitleRegenerationService.ts | 155 | KEEP | T13b | not started |  |
| TurnItemPositionStore.ts | 115 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/turn_item_position_store.rs` |
| UsageLimitRecoveryWorker.ts | 115 | KEEP | T13c | not started |  |
| UserFacingErrors.ts | 110 | KEEP | T13b | ported | `T13b-orch-pipeline:orchestration/src/pipeline/user_facing_errors.rs` |
| WireProjection.ts | 261 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| assistantStreaming.ts | 132 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/assistant_streaming.rs` |
| builtInProviderAdapterDrivers.ts | 47 | KEEP | T08 | not started |  |
| http.ts | 236 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| pullRequestWatch.ts | 220 | KEEP | T13c | not started |  |
| runtimeLayer.ts | 365 | KEEP | T13c | not started |  |
| threadHistoryPaging.ts | 597 | KEEP | T13a | ported | `T13a-orch-projections:orchestration/src/projection/thread_history_paging.rs` |
| toolOutputImageBytes.ts | 135 | KEEP | T13a | not started |  |
| unknownField.ts | 10 | KEEP | T13a | not started |  |
| workflowAgentAnswers.ts | 97 | KEEP | T13c | not started |  |
| workflowFileRead.ts | 112 | KEEP | T13c | not started |  |
| workflowScriptQuery.ts | 23 | KEEP | T13c | not started |  |

### `apps/server/src/orchestration-v2/Adapters/`

5 files, 17,675 lines: KEEP 4 / 17,428, CUT 1 / 247, UNASSIGNED 0 / 0, ported 3 / 17,272.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AntigravityAdapterV2.ts | 247 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| ClaudeAdapterV2.ts | 9,614 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_adapter_v2.rs` |
| CodexAdapterV2.ts | 7,466 | KEEP | T09 | ported | `T09-codex-adapter:provider-codex/src/adapter.rs` |
| ProviderTextDeltaCoalescer.ts | 156 | KEEP | T13c | not started |  |
| claudeWorkflowProgress.ts | 192 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_workflow_progress.rs` |

### `apps/server/src/orchestration-v2/legacy/`

1 files, 833 lines: KEEP 0 / 0, CUT 1 / 833, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| LegacyV1ThreadImporter.ts | 833 | CUT | Legacy v1 importer |  |  |

### `apps/server/src/persistence/`

12 files, 3,212 lines: KEEP 9 / 2,262, CUT 3 / 950, UNASSIGNED 0 / 0, ported 9 / 2,262.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AuthPairingLinks.ts | 365 | CUT | T3 Connect / relay / remote access / auth |  |  |
| AuthSessions.ts | 529 | CUT | T3 Connect / relay / remote access / auth |  |  |
| Errors.ts | 134 | KEEP | T02 | ported | `main:persistence/src/errors.rs` |
| Migrations.ts | 273 | KEEP | T02 | ported | `main:persistence/src/migrations.rs` |
| OrchestrationCommandReceipts.ts | 186 | KEEP | T02 | ported | `main:persistence/src/orchestration_command_receipts.rs` |
| OrchestrationEventStore.ts | 793 | KEEP | T02 | ported | `main:persistence/src/orchestration_event_store.rs` |
| ProviderSessionRuntime.ts | 480 | KEEP | T02 | ported | `main:persistence/src/provider_session_runtime.rs` |
| PullRequestFilesViewed.ts | 184 | KEEP | T02 | ported | `main:persistence/src/pull_request_files_viewed.rs` |
| PullRequestReviewGuides.ts | 84 | KEEP | T02 | ported | `main:persistence/src/pull_request_review_guides.rs` |
| Sqlite.ts | 75 | KEEP | T02 | ported | `main:persistence/src/db.rs` |
| initializeV2Database.ts | 53 | KEEP | T02 | ported | `main:persistence/src/initialize_v2_database.rs` |
| reconcileV2PreviewMigration.ts | 56 | CUT | Intentionally not called (PORTING Deviations) |  |  |

### `apps/server/src/persistence/Migrations/`

62 files, 2,480 lines: KEEP 62 / 2,480, CUT 0 / 0, UNASSIGNED 0 / 0, ported 62 / 2,480.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| 001_OrchestrationEvents.ts | 44 | KEEP | T02 | ported | `main:persistence/sql/001_OrchestrationEvents.sql` |
| 002_OrchestrationCommandReceipts.ts | 28 | KEEP | T02 | ported | `main:persistence/sql/002_OrchestrationCommandReceipts.sql` |
| 003_CheckpointDiffBlobs.ts | 22 | KEEP | T02 | ported | `main:persistence/sql/003_CheckpointDiffBlobs.sql` |
| 004_ProviderSessionRuntime.ts | 29 | KEEP | T02 | ported | `main:persistence/sql/004_ProviderSessionRuntime.sql` |
| 005_Projections.ts | 153 | KEEP | T02 | ported | `main:persistence/sql/005_Projections.sql` |
| 006_ProjectionThreadSessionRuntimeModeColumns.ts | 19 | KEEP | T02 | ported | `main:persistence/sql/006_ProjectionThreadSessionRuntimeModeColumns.sql` |
| 007_ProjectionThreadMessageAttachments.ts | 11 | KEEP | T02 | ported | `main:persistence/sql/007_ProjectionThreadMessageAttachments.sql` |
| 008_ProjectionThreadActivitySequence.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/008_ProjectionThreadActivitySequence.sql` |
| 009_ProviderSessionRuntimeMode.ts | 4 | KEEP | T02 | ported | `main:persistence/sql/009_ProviderSessionRuntimeMode.sql` |
| 010_ProjectionThreadsRuntimeMode.ts | 17 | KEEP | T02 | ported | `main:persistence/sql/010_ProjectionThreadsRuntimeMode.sql` |
| 011_OrchestrationThreadCreatedRuntimeMode.ts | 13 | KEEP | T02 | ported | `main:persistence/sql/011_OrchestrationThreadCreatedRuntimeMode.sql` |
| 012_ProjectionThreadsInteractionMode.ts | 11 | KEEP | T02 | ported | `main:persistence/sql/012_ProjectionThreadsInteractionMode.sql` |
| 013_ProjectionThreadProposedPlans.ts | 22 | KEEP | T02 | ported | `main:persistence/sql/013_ProjectionThreadProposedPlans.sql` |
| 014_ProjectionThreadProposedPlanImplementation.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/014_ProjectionThreadProposedPlanImplementation.sql` |
| 015_ProjectionTurnsSourceProposedPlan.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/015_ProjectionTurnsSourceProposedPlan.sql` |
| 016_CanonicalizeModelSelections.ts | 235 | KEEP | T02 | ported | `main:persistence/sql/016_CanonicalizeModelSelections.sql` |
| 017_ProjectionThreadsArchivedAt.ts | 18 | KEEP | T02 | ported | `main:persistence/sql/017_ProjectionThreadsArchivedAt.sql` |
| 018_ProjectionThreadsArchivedAtIndex.ts | 11 | KEEP | T02 | ported | `main:persistence/sql/018_ProjectionThreadsArchivedAtIndex.sql` |
| 019_ProjectionSnapshotLookupIndexes.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/019_ProjectionSnapshotLookupIndexes.sql` |
| 020_AuthAccessManagement.ts | 42 | KEEP | T02 | ported | `main:persistence/sql/020_AuthAccessManagement.sql` |
| 021_AuthSessionClientMetadata.ts | 62 | KEEP | T02 | ported | `main:persistence/sql/021_AuthSessionClientMetadata.sql` |
| 022_AuthSessionLastConnectedAt.ts | 17 | KEEP | T02 | ported | `main:persistence/sql/022_AuthSessionLastConnectedAt.sql` |
| 023_ProjectionThreadShellSummary.ts | 26 | KEEP | T02 | ported | `main:persistence/sql/023_ProjectionThreadShellSummary.sql` |
| 024_BackfillProjectionThreadShellSummary.ts | 277 | KEEP | T02 | ported | `main:persistence/sql/024_BackfillProjectionThreadShellSummary.sql` |
| 025_CleanupInvalidProjectionPendingApprovals.ts | 27 | KEEP | T02 | ported | `main:persistence/sql/025_CleanupInvalidProjectionPendingApprovals.sql` |
| 026_CanonicalizeModelSelectionOptions.ts | 138 | KEEP | T02 | ported | `main:persistence/sql/026_CanonicalizeModelSelectionOptions.sql` |
| 027_ProviderSessionRuntimeInstanceId.ts | 38 | KEEP | T02 | ported | `main:persistence/sql/027_ProviderSessionRuntimeInstanceId.sql` |
| 028_ProjectionThreadSessionInstanceId.ts | 21 | KEEP | T02 | ported | `main:persistence/sql/028_ProjectionThreadSessionInstanceId.sql` |
| 029_ProjectionThreadDetailOrderingIndexes.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/029_ProjectionThreadDetailOrderingIndexes.sql` |
| 030_ProjectionThreadShellArchiveIndexes.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/030_ProjectionThreadShellArchiveIndexes.sql` |
| 031_AuthAuthorizationScopes.ts | 57 | KEEP | T02 | ported | `main:persistence/sql/031_AuthAuthorizationScopes.sql` |
| 032_AuthPairingProofKeyThumbprint.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/032_AuthPairingProofKeyThumbprint.sql` |
| 033_ProjectionThreadsSettled.ts | 23 | KEEP | T02 | ported | `main:persistence/sql/033_ProjectionThreadsSettled.sql` |
| 034_ProjectionThreadsSnoozed.ts | 23 | KEEP | T02 | ported | `main:persistence/sql/034_ProjectionThreadsSnoozed.sql` |
| 035_ProjectionThreadTitleRegeneration.ts | 23 | KEEP | T02 | ported | `main:persistence/sql/035_ProjectionThreadTitleRegeneration.sql` |
| 036_ProjectionThreadsPinned.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/036_ProjectionThreadsPinned.sql` |
| 037_ProjectionTurnsKeysetIndex.ts | 17 | KEEP | T02 | ported | `main:persistence/sql/037_ProjectionTurnsKeysetIndex.sql` |
| 038_ProjectionThreadsPinOrderKey.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/038_ProjectionThreadsPinOrderKey.sql` |
| 039_ProjectionProjectsDefaultThreadEnvMode.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/039_ProjectionProjectsDefaultThreadEnvMode.sql` |
| 040_ProjectionProjectFaviconPath.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/040_ProjectionProjectFaviconPath.sql` |
| 041_AuthSessionClientConnection.ts | 26 | KEEP | T02 | ported | `main:persistence/sql/041_AuthSessionClientConnection.sql` |
| 042_ProjectionThreadLinkedPullRequest.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/042_ProjectionThreadLinkedPullRequest.sql` |
| 043_ProjectionThreadsUnsettledAt.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/043_ProjectionThreadsUnsettledAt.sql` |
| 044_ClearAutomaticProjectModelDefaults.ts | 51 | KEEP | T02 | ported | `main:persistence/sql/044_ClearAutomaticProjectModelDefaults.sql` |
| 045_ProjectionProjectsAutoPull.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/045_ProjectionProjectsAutoPull.sql` |
| 046_RepairAutomaticSettlementTimestamps.ts | 70 | KEEP | T02 | ported | `main:persistence/sql/046_RepairAutomaticSettlementTimestamps.sql` |
| 047_ProjectionProjectIcon.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/047_ProjectionProjectIcon.sql` |
| 048_ProjectionThreadBranchPullRequest.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/048_ProjectionThreadBranchPullRequest.sql` |
| 049_ProjectionThreadsActiveOrderKey.ts | 15 | KEEP | T02 | ported | `main:persistence/sql/049_ProjectionThreadsActiveOrderKey.sql` |
| 050_ProjectionThreadPullRequests.ts | 92 | KEEP | T02 | ported | `main:persistence/src/legacy_pull_request_key.rs` |
| 051_ProjectionThreadMessageContext.ts | 16 | KEEP | T02 | ported | `main:persistence/sql/051_ProjectionThreadMessageContext.sql` |
| 052_ProjectionThreadTitleState.ts | 7 | KEEP | T02 | ported | `main:persistence/sql/052_ProjectionThreadTitleState.sql` |
| 053_PullRequestFilesViewed.ts | 25 | KEEP | T02 | ported | `main:persistence/sql/053_PullRequestFilesViewed.sql` |
| 054_ProjectionThreadsAutoSettleDisabledAt.ts | 15 | KEEP | T02 | ported | `main:persistence/sql/054_ProjectionThreadsAutoSettleDisabledAt.sql` |
| 055_OrchestrationV2.ts | 332 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| 056_RemoveRedundantProjectionIndexes.ts | 13 | KEEP | T02 | ported | `main:persistence/sql/056_RemoveRedundantProjectionIndexes.sql` |
| 057_ScheduledTaskWebhooks.ts | 34 | KEEP | T02 | ported | `main:persistence/sql/057_ScheduledTaskWebhooks.sql` |
| 058_WebhookRelayDeliveries.ts | 22 | KEEP | T02 | ported | `main:persistence/sql/058_WebhookRelayDeliveries.sql` |
| 059_McpAppModelContext.ts | 21 | KEEP | T02 | ported | `main:persistence/sql/059_McpAppModelContext.sql` |
| 060_ThreadSnapshotWindowIndexes.ts | 23 | KEEP | T02 | ported | `main:persistence/sql/060_ThreadSnapshotWindowIndexes.sql` |
| 061_PullRequestReviewGuides.ts | 19 | KEEP | T02 | ported | `main:persistence/sql/061_PullRequestReviewGuides.sql` |
| 062_ThreadFailureLookupIndex.ts | 15 | KEEP | T02 | ported | `main:persistence/sql/062_ThreadFailureLookupIndex.sql` |

### `apps/server/src/persistence/Migrations/OrchestrationV2/`

11 files, 768 lines: KEEP 11 / 768, CUT 0 / 0, UNASSIGNED 0 / 0, ported 11 / 768.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ApplicationEventSequenceIndexes.ts | 19 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| ApplicationEventSource.ts | 282 | KEEP | T02 | ported | `main:persistence/src/application_event_source.rs` |
| EffectCancellation.ts | 79 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| Foundation.ts | 180 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| LegacyV1ImportState.ts | 28 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| ProviderSessionBindings.ts | 28 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| RecoveryIndexes.ts | 31 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| ScheduledTasks.ts | 43 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| ShellIndexes.ts | 25 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| Subagents.ts | 29 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |
| ThreadLaunchWorkflows.ts | 24 | KEEP | T02 | ported | `main:persistence/sql/055_OrchestrationV2.sql` |

### `apps/server/src/preview/`

10 files, 6,112 lines: KEEP 0 / 0, CUT 10 / 6,112, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopBrowserChannel.ts | 234 | CUT | Browser / preview / devices |  |  |
| Manager.ts | 517 | CUT | Browser / preview / devices |  |  |
| PortScanner.ts | 809 | CUT | Browser / preview / devices |  |  |
| PreviewBrowser.ts | 463 | CUT | Browser / preview / devices |  |  |
| PreviewBrowserHost.ts | 158 | CUT | Browser / preview / devices |  |  |
| ServerBrowser.ts | 2,568 | CUT | Browser / preview / devices |  |  |
| ServerBrowserContexts.ts | 248 | CUT | Browser / preview / devices |  |  |
| ServerBrowserPage.ts | 669 | CUT | Browser / preview / devices |  |  |
| ServerBrowserStream.ts | 288 | CUT | Browser / preview / devices |  |  |
| SessionControl.ts | 158 | CUT | Browser / preview / devices |  |  |

### `apps/server/src/process/`

2 files, 1,238 lines: KEEP 0 / 0, CUT 1 / 376, UNASSIGNED 1 / 862, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| agentScope.ts | 376 | CUT | Linux systemd resource scoping (resource telemetry) |  |  |
| externalLauncher.ts | 862 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |

### `apps/server/src/project/`

15 files, 5,734 lines: KEEP 0 / 0, CUT 6 / 1,550, UNASSIGNED 9 / 4,184, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AgentSessionImporter.ts | 409 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| AgentSessionJson.ts | 201 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| AgentSessionScanner.ts | 1,494 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| ManagedProjectFolders.ts | 527 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| ProjectCloneTracker.ts | 480 | CUT | Project clone |  |  |
| ProjectEnrichmentService.ts | 311 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| ProjectFaviconResolver.ts | 377 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| ProjectMutation.ts | 53 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| ProjectService.ts | 594 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| ProjectSetupScriptRunner.ts | 485 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| RepositoryIdentityResolver.ts | 218 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| T3ProjectFileLoader.ts | 109 | CUT | t3 project file (contracts `t3ProjectFile` cut) |  |  |
| WorktreeSetupTracker.ts | 366 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| gitCloneProgress.ts | 44 | CUT | Project clone |  |  |
| http.ts | 66 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |

### `apps/server/src/provider/`

55 files, 14,067 lines: KEEP 48 / 11,092, CUT 7 / 2,975, UNASSIGNED 0 / 0, ported 12 / 3,018.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AntigravityAuth.ts | 561 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravityInstallation.ts | 846 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravityProvider.ts | 443 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| ClaudeModelCatalog.ts | 296 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_model_catalog.rs` |
| ClaudeModelManifest.ts | 82 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_model_catalog.rs` |
| ClaudeProvider.ts | 749 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_capabilities_probe.rs` |
| ClaudeTurnTokenUsage.ts | 145 | KEEP | T10 | ported | `T10-claude-adapter:provider-claude/src/claude_turn_token_usage.rs` |
| CodexAuthCallback.ts | 60 | KEEP | T09 | not started |  |
| CodexAuthCallbackPage.ts | 42 | KEEP | T09 | not started |  |
| CodexChatGptAuth.ts | 990 | KEEP | T09 | not started |  |
| CodexChatGptHandoff.ts | 63 | KEEP | T09 | not started |  |
| CodexChatGptModels.ts | 47 | KEEP | T09 | not started |  |
| CodexChatGptSessionLock.ts | 45 | KEEP | T09 | not started |  |
| CodexDeveloperInstructions.ts | 225 | KEEP | T09 | not started |  |
| CodexInstallation.ts | 735 | KEEP | T09 | not started |  |
| CodexManagedErrors.ts | 87 | KEEP | T09 | not started |  |
| CodexManagedHome.ts | 20 | KEEP | T09 | not started |  |
| CodexManagedRuntime.ts | 114 | KEEP | T09 | partial | `T09-codex-adapter:provider-codex/src/runtime.rs` |
| CodexMcpElicitation.ts | 222 | KEEP | T09 | not started |  |
| CodexProvider.ts | 795 | KEEP | T09 | ported | `T09-codex-adapter:provider-codex/src/codex_provider.rs` |
| CodexThreadRevert.ts | 42 | KEEP | T09 | partial | `T09-codex-adapter:codex-app-server/src/extensions.rs` |
| CodexToolPresentation.ts | 236 | KEEP | T09 | not started |  |
| CodexTurnTokenUsage.ts | 182 | KEEP | T09 | ported | `T09-codex-adapter:provider-codex/src/codex_turn_token_usage.rs` |
| Errors.ts | 40 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_errors.rs` |
| EventNdjsonLogger.ts | 834 | KEEP | T08b | not started |  |
| ModelManifest.ts | 334 | KEEP | T08 | not started |  |
| ProjectSettingsOverrideSweep.ts | 61 | KEEP | T08 | not started |  |
| ProviderAuthService.ts | 304 | KEEP | T08 | not started |  |
| ProviderCredentialStore.ts | 24 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/credential_store.rs` |
| ProviderHostLive.ts | 71 | KEEP | T08 | not started |  |
| ProviderInstanceRegistry.ts | 508 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_instance_registry.rs` |
| ProviderInstanceRegistryHydration.ts | 182 | KEEP | T08 | partial | `T08-provider-core:provider-core/src/provider_instance_registry_hydration.rs` |
| ProviderInstanceRegistryMutator.ts | 52 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_instance_registry.rs` |
| ProviderOrchestrationAdapterInfrastructure.ts | 32 | KEEP | T08 | not started |  |
| ProviderRegistry.ts | 1,147 | KEEP | T08 | not started |  |
| ProviderUsageLimitsIngestion.ts | 7 | KEEP | T08 | not started |  |
| antigravityAuthSupport.ts | 608 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| antigravityCallback.ts | 115 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| antigravityRelease.ts | 94 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| antigravityUsageLimits.ts | 308 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| builtInDrivers.ts | 98 | KEEP | T08 | not started |  |
| builtInProviderCatalog.ts | 17 | KEEP | T08 | not started |  |
| claudeResetCredits.ts | 280 | KEEP | T10 | not started |  |
| claudeUsageLimits.ts | 199 | KEEP | T10 | not started |  |
| codexLaunchArgs.ts | 39 | KEEP | T09 | not started |  |
| codexModelCatalog.ts | 65 | KEEP | T09 | not started |  |
| codexUsageLimits.ts | 250 | KEEP | T09 | not started |  |
| providerCompatibility.ts | 130 | KEEP | T08 | not started |  |
| providerInstallation.ts | 163 | KEEP | T08 | not started |  |
| providerMaintenanceCommandCoordinator.ts | 59 | KEEP | T08 | not started |  |
| providerMaintenanceRunner.ts | 603 | KEEP | T08 | not started |  |
| providerStatusCache.ts | 160 | KEEP | T08 | not started |  |
| resetCreditCoordinator.ts | 111 | KEEP | T08 | not started |  |
| unavailableProviderSnapshot.ts | 79 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/unavailable_provider_snapshot.rs` |
| userInputAttachments.ts | 66 | KEEP | T08 | ported | `T13b-orch-pipeline:orchestration/src/pipeline/user_input_attachments.rs` |

### `apps/server/src/provider/Drivers/`

14 files, 3,974 lines: KEEP 10 / 2,565, CUT 4 / 1,409, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AntigravityDriver.ts | 536 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravitySkills.ts | 236 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravityUsage.ts | 140 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| ClaudeDriver.ts | 378 | KEEP | T10 | not started |  |
| ClaudeExecutable.ts | 97 | KEEP | T10 | not started |  |
| ClaudeHome.ts | 110 | KEEP | T10 | not started |  |
| ClaudeSkillDispatch.ts | 80 | KEEP | T10 | not started |  |
| ClaudeSkills.ts | 401 | KEEP | T10 | not started |  |
| CodexDriver.ts | 389 | KEEP | T09 | not started |  |
| CodexHomeLayout.ts | 422 | KEEP | T09 | not started |  |
| CodexManagedProvider.ts | 306 | KEEP | T09 | not started |  |
| antigravityUsageReader.ts | 497 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| claudeUsage.ts | 121 | KEEP | T10 | not started |  |
| codexUsage.ts | 261 | KEEP | T09 | not started |  |

### `apps/server/src/provider/acp/`

4 files, 963 lines: KEEP 0 / 0, CUT 4 / 963, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AntigravityAcpSupport.ts | 391 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravityClientFiles.ts | 124 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravityProtocol.ts | 387 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AntigravitySessionFiles.ts | 61 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `apps/server/src/pullRequest/`

10 files, 5,246 lines: KEEP 9 / 5,222, CUT 1 / 24, UNASSIGNED 0 / 0, ported 8 / 1,613.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| PullRequestProviderRegistry.ts | 46 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/provider_registry.rs` |
| PullRequestReadCache.ts | 243 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/read_cache.rs` |
| PullRequestReviewGuideService.ts | 548 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/review_guide_service.rs` |
| PullRequestService.ts | 3,609 | KEEP | T07 | not started |  |
| http.ts | 24 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| linkedThreads.ts | 74 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/linked_threads.rs` |
| pullRequestReviewGuideInput.ts | 47 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/review_guide_input.rs` |
| pullRequestReviewGuideNormalize.ts | 184 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/review_guide_normalize.rs` |
| pullRequestSyncKey.ts | 45 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/sync_key.rs` |
| pullRequestViewedFiles.ts | 426 | KEEP | T07 | ported | `T07-source-control:source-control/src/pull_request/viewed_files.rs` |

### `apps/server/src/relay/`

3 files, 923 lines: KEEP 0 / 0, CUT 3 / 923, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AgentAwarenessRelay.ts | 852 | CUT | T3 Connect / relay / remote access / auth |  |  |
| HeldHooksWaker.ts | 50 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayEnvironmentClient.ts | 21 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/server/src/resourceTelemetry/`

8 files, 3,637 lines: KEEP 0 / 0, CUT 8 / 3,637, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopTelemetryReceiver.ts | 718 | CUT | Telemetry / observability / resource telemetry |  |  |
| HostResources.ts | 93 | CUT | Telemetry / observability / resource telemetry |  |  |
| Model.ts | 611 | CUT | Telemetry / observability / resource telemetry |  |  |
| NativeTelemetryClient.ts | 1,122 | CUT | Telemetry / observability / resource telemetry |  |  |
| ResourceAttribution.ts | 71 | CUT | Telemetry / observability / resource telemetry |  |  |
| ResourceMonitorBinary.ts | 227 | CUT | Telemetry / observability / resource telemetry |  |  |
| ResourceTelemetry.ts | 503 | CUT | Telemetry / observability / resource telemetry |  |  |
| ResourceTelemetryHistory.ts | 292 | CUT | Telemetry / observability / resource telemetry |  |  |

### `apps/server/src/review/`

1 files, 177 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 1 / 177, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ReviewService.ts | 177 | **UNASSIGNED** | N6 Git UI and review backend |  |  |

### `apps/server/src/scheduledTasks/`

6 files, 2,222 lines: KEEP 0 / 0, CUT 6 / 2,222, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| RelayDeliveryProof.ts | 91 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| Schedule.ts | 116 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| ScheduledTaskService.ts | 1,681 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| webhookRoute.ts | 139 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| webhookTemplate.ts | 158 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| webhookVerification.ts | 37 | CUT | Scheduled tasks / secret requests / webhooks |  |  |

### `apps/server/src/scheduling/`

1 files, 64 lines: KEEP 0 / 0, CUT 1 / 64, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| Scheduler.ts | 64 | CUT | Scheduled tasks / secret requests / webhooks |  |  |

### `apps/server/src/secrets/`

1 files, 281 lines: KEEP 0 / 0, CUT 1 / 281, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| SecretRequests.ts | 281 | CUT | Scheduled tasks / secret requests / webhooks |  |  |

### `apps/server/src/sourceControl/`

6 files, 1,121 lines: KEEP 6 / 1,121, CUT 0 / 0, UNASSIGNED 0 / 0, ported 1 / 72.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ServerSourceControlHost.ts | 42 | KEEP | T07 | not started |  |
| SourceControlDiscovery.ts | 143 | KEEP | T07 | not started |  |
| SourceControlProviderRegistry.ts | 316 | KEEP | T07 | not started |  |
| SourceControlRepositoryService.ts | 488 | KEEP | T07 | not started |  |
| builtInDrivers.ts | 60 | KEEP | T07 | partial | `T07-source-control:source-control/src/source_control/built_in_drivers.rs (name match, no header)` |
| sourceControlHostSecrets.ts | 72 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control/host_secrets.rs` |

### `apps/server/src/telemetry/`

2 files, 607 lines: KEEP 0 / 0, CUT 2 / 607, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AnalyticsService.ts | 290 | CUT | Telemetry / observability / resource telemetry |  |  |
| Identify.ts | 317 | CUT | Telemetry / observability / resource telemetry |  |  |

### `apps/server/src/terminal/`

3 files, 3,589 lines: KEEP 0 / 0, CUT 3 / 3,589, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| Manager.ts | 3,217 | CUT | Terminal / PTY |  |  |
| NodePtyAdapter.ts | 304 | CUT | Terminal / PTY |  |  |
| OutputProtocol.ts | 68 | CUT | Terminal / PTY |  |  |

### `apps/server/src/textGeneration/`

6 files, 1,244 lines: KEEP 0 / 0, CUT 1 / 313, UNASSIGNED 5 / 931, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AntigravityTextGeneration.ts | 313 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| ClaudeTextGeneration.ts | 345 | **UNASSIGNED** | N3 Text generation |  |  |
| CodexTextGeneration.ts | 416 | **UNASSIGNED** | N3 Text generation |  |  |
| TextGeneration.ts | 98 | **UNASSIGNED** | N3 Text generation |  |  |
| TextGenerationPresets.ts | 27 | **UNASSIGNED** | N3 Text generation |  |  |
| ThreadTitleLinks.ts | 45 | **UNASSIGNED** | N3 Text generation |  |  |

### `apps/server/src/usage/`

7 files, 3,058 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 7 / 3,058, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| UsageLimitSources.ts | 186 | **UNASSIGNED** | N4 Usage page |  |  |
| UsageService.ts | 936 | **UNASSIGNED** | N4 Usage page |  |  |
| cliproxyApi.ts | 379 | **UNASSIGNED** | N4 Usage page |  |  |
| usageAggregation.ts | 373 | **UNASSIGNED** | N4 Usage page |  |  |
| usagePricing.ts | 357 | **UNASSIGNED** | N4 Usage page |  |  |
| usageScanCache.ts | 424 | **UNASSIGNED** | N4 Usage page |  |  |
| usageTranscriptReader.ts | 403 | **UNASSIGNED** | N4 Usage page |  |  |

### `apps/server/src/utils/`

1 files, 37 lines: KEEP 0 / 0, CUT 1 / 37, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| subscribeBeforeSnapshot.ts | 37 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |

### `apps/server/src/vcs/`

8 files, 6,999 lines: KEEP 8 / 6,999, CUT 0 / 0, UNASSIGNED 0 / 0, ported 8 / 6,999.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| GitVcsDriver.ts | 1,335 | KEEP | T06 | ported | `T06-git:git/src/git_vcs_driver.rs` |
| GitVcsDriverCore.ts | 4,161 | KEEP | T06 | ported | `T06-git:git/src/git_vcs_driver_core.rs` |
| VcsDriver.ts | 92 | KEEP | T06 | ported | `T06-git:git/src/vcs_driver.rs` |
| VcsDriverRegistry.ts | 217 | KEEP | T06 | ported | `T06-git:git/src/vcs_driver_registry.rs` |
| VcsProcess.ts | 233 | KEEP | T06 | ported | `T06-git:git/src/vcs_process.rs` |
| VcsProjectConfig.ts | 157 | KEEP | T06 | ported | `T06-git:git/src/vcs_project_config.rs` |
| VcsProvisioningService.ts | 55 | KEEP | T06 | ported | `T06-git:git/src/vcs_provisioning_service.rs` |
| VcsStatusBroadcaster.ts | 749 | KEEP | T06 | ported | `T06-git:git/src/vcs_status_broadcaster.rs` |

### `apps/server/src/workspace/`

6 files, 1,632 lines: KEEP 6 / 1,632, CUT 0 / 0, UNASSIGNED 0 / 0, ported 6 / 1,632.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| WorkspaceEntries.ts | 367 | KEEP | T06 | ported | `T06-git:git/src/workspace/workspace_entries.rs` |
| WorkspaceFileSystem.ts | 401 | KEEP | T06 | ported | `T06-git:git/src/workspace/workspace_file_system.rs` |
| WorkspacePaths.ts | 230 | KEEP | T06 | ported | `T06-git:git/src/workspace/workspace_paths.rs` |
| WorkspaceSearchIndex.ts | 589 | KEEP | T06 | ported | `T06-git:git/src/workspace/workspace_search_index.rs` |
| fileHeaderMimeType.ts | 22 | KEEP | T06 | ported | `T06-git:git/src/workspace/file_header_mime_type.rs` |
| workspaceLease.ts | 23 | KEEP | T06 | ported | `T06-git:git/src/workspace/workspace_lease.rs` |

### `apps/web/src/`

96 files, 24,548 lines: KEEP 56 / 17,369, CUT 28 / 4,432, UNASSIGNED 12 / 2,747, ported 11 / 3,890.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AppRoot.tsx | 23 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| appearanceContrast.ts | 10 | KEEP | T03 | ported | `main:app/src/theme/appearance_contrast.rs` |
| appearanceFonts.ts | 395 | KEEP | T03 | ported | `main:app/src/theme/appearance_fonts.rs` |
| bootstrap.ts | 5 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| branding.logic.ts | 38 | KEEP | T03 | partial | `main:app/src/branding.rs (name match, no header)` |
| branding.ts | 27 | KEEP | T03 | partial | `main:app/src/branding.rs` |
| browserFaviconLogic.ts | 201 | CUT | Browser / preview / devices |  |  |
| browserFaviconStore.ts | 342 | CUT | Browser / preview / devices |  |  |
| browserHistoryStore.ts | 398 | CUT | Browser / preview / devices |  |  |
| clientPersistenceStorage.ts | 25 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| closedViewStore.ts | 143 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| commandPaletteBus.ts | 39 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| composer-code-block.ts | 254 | KEEP | T17 | not started |  |
| composer-code-languages.ts | 97 | KEEP | T17 | not started |  |
| composer-editor-mentions.ts | 180 | KEEP | T17 | not started |  |
| composer-list-continuation.ts | 147 | KEEP | T17 | not started |  |
| composer-logic.ts | 356 | KEEP | T17 | not started |  |
| composer-rich-text-doc.ts | 1,573 | KEEP | T17 | not started |  |
| composer-rich-text.ts | 131 | KEEP | T17 | not started |  |
| composer-undo-grouping.ts | 64 | KEEP | T17 | not started |  |
| composerDraftStore.ts | 4,613 | KEEP | T17 | not started |  |
| composerHandleContext.ts | 10 | KEEP | T17 | not started |  |
| composerPlaceholder.ts | 2 | KEEP | T17 | not started |  |
| confirmDialog.ts | 136 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| contextMenuFallback.ts | 509 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| desktopAppActivation.ts | 119 | CUT | Electron shell / desktop-only infra |  |  |
| diffFileActions.ts | 100 | KEEP | T18 | not started |  |
| diffPanelStore.ts | 140 | KEEP | T18 | not started |  |
| editorLabels.ts | 19 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| editorPreferences.ts | 131 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| env.ts | 6 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| fileContextMenu.ts | 206 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| filePathDisplay.ts | 55 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/file_path_display.rs` |
| hostedPairing.ts | 96 | CUT | T3 Connect / relay / remote access / auth |  |  |
| keybindings.ts | 458 | KEEP | T20 | not started |  |
| legalLinks.ts | 1 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| localApi.ts | 98 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| localEnvironment.ts | 9 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| logicalProject.ts | 15 | KEEP | T15 | ported | re-export barrel (no module needed) |
| main.tsx | 87 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| markdown-clipboard.ts | 411 | KEEP | T16 | not started |  |
| markdown-incremental.ts | 107 | KEEP | T16 | not started |  |
| markdown-links.ts | 98 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/markdown/markdown_links.rs (name match, no header)` |
| modelOrdering.ts | 86 | KEEP | T17 | not started |  |
| modelPickerVisibility.ts | 13 | KEEP | T17 | not started |  |
| modelSelection.ts | 396 | KEEP | T17 | not started |  |
| openVsxThemes.ts | 780 | KEEP | T20 | not started |  |
| pairingUrl.ts | 5 | CUT | T3 Connect / relay / remote access / auth |  |  |
| panelAnimations.ts | 79 | KEEP | T03 | not started |  |
| pendingUserInput.ts | 223 | KEEP | T16 | not started |  |
| pierre-icons.ts | 197 | KEEP | T18 | not started |  |
| pierre-tree-theme.ts | 27 | KEEP | T18 | not started |  |
| portDiscoveryState.ts | 87 | CUT | Browser / preview / devices |  |  |
| previewMiniPlayerStore.ts | 162 | CUT | Browser / preview / devices |  |  |
| previewStateStore.ts | 524 | CUT | Browser / preview / devices |  |  |
| projectIconColors.ts | 125 | KEEP | T15 | ported | `T15-sidebar:app/src/project_icon_colors.rs` |
| projectIconOptions.ts | 81 | KEEP | T15 | not started |  |
| projectIdentity.ts | 43 | KEEP | T15 | ported | `T15-sidebar:app/src/project_identity.rs` |
| projectScripts.ts | 112 | CUT | Project scripts run in terminal; t3.json is import-only (hooks replace) |  |  |
| promptStashStore.ts | 302 | KEEP | T17 | not started |  |
| proposedPlan.ts | 106 | KEEP | T16 | not started |  |
| providerAuthDelivery.ts | 19 | CUT | T3 Connect / relay / remote access / auth |  |  |
| providerInstances.ts | 351 | KEEP | T20 | not started |  |
| providerModels.ts | 107 | KEEP | T17 | not started |  |
| providerSkillSearch.ts | 108 | KEEP | T17 | not started |  |
| providerUpdateDismissal.ts | 93 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| pullRequestReference.ts | 38 | KEEP | T19 | not started |  |
| questionAttachments.ts | 63 | KEEP | T16 | not started |  |
| remoteOpen.ts | 201 | CUT | T3 Connect / relay / remote access / auth |  |  |
| reopenClosedView.ts | 150 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| reviewCommentContext.ts | 477 | KEEP | T18 | not started |  |
| rightPanelLayout.ts | 3 | KEEP | T03 | not started |  |
| rightPanelStore.ts | 1,156 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| router.ts | 23 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| session-logic.ts | 1,097 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/session_logic.rs` |
| settleAndAdvance.ts | 29 | KEEP | T15 | not started |  |
| shortcutModifierState.ts | 123 | KEEP | T20 | not started |  |
| sidebarPendingFileDropStore.ts | 74 | KEEP | T15 | not started |  |
| sidebarProjectGrouping.ts | 225 | KEEP | T15 | not started |  |
| sourceControlPresentation.ts | 67 | KEEP | T19 | not started |  |
| terminal-links.ts | 161 | CUT | Terminal / PTY |  |  |
| terminalUiStateStore.ts | 781 | CUT | Terminal / PTY |  |  |
| themePalette.ts | 1,662 | KEEP | T03 | ported | `main:app/src/theme/theme_palette.rs` |
| threadNotifications.ts | 100 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| threadRoutes.ts | 106 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| threadSelectionStore.ts | 131 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/thread_selection.rs` |
| threadSync.ts | 27 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| timestampFormat.ts | 355 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/timestamp_format.rs` |
| types.ts | 126 | KEEP | T16 | partial | `T16-logic:app/src/components/chat/timeline/types.rs` |
| uiStateStore.ts | 468 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| versionSkew.ts | 207 | CUT | T3 Connect / relay / remote access / auth |  |  |
| vscodeThemeImport.ts | 538 | KEEP | T20 | not started |  |
| workspaceBasenameLookup.ts | 54 | KEEP | T16 | not started |  |
| workspaceTitlebar.ts | 2 | KEEP | T03 | ported | `main:app/src/components/workspace_titlebar.rs` |
| worktreeCleanup.ts | 45 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| wslPaths.ts | 159 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/assets/`

2 files, 183 lines: KEEP 1 / 85, CUT 1 / 98, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| assetUrls.ts | 98 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| projectFaviconCache.ts | 85 | KEEP | T15 | not started |  |

### `apps/web/src/browser/`

30 files, 5,360 lines: KEEP 0 / 0, CUT 30 / 5,360, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| BrowserDeviceToolbar.tsx | 328 | CUT | Browser / preview / devices |  |  |
| BrowserProfileReporter.tsx | 40 | CUT | Browser / preview / devices |  |  |
| BrowserSurfaceSlot.tsx | 93 | CUT | Browser / preview / devices |  |  |
| BrowserViewportResizeHandles.tsx | 169 | CUT | Browser / preview / devices |  |  |
| ElectronBrowserHost.tsx | 182 | CUT | Browser / preview / devices |  |  |
| HostedBrowserWebview.tsx | 404 | CUT | Browser / preview / devices |  |  |
| ScreenRotationIcon.tsx | 26 | CUT | Browser / preview / devices |  |  |
| ServerBrowserSurface.tsx | 948 | CUT | Browser / preview / devices |  |  |
| annotationTheme.ts | 28 | CUT | Browser / preview / devices |  |  |
| browserDefaults.ts | 144 | CUT | Browser / preview / devices |  |  |
| browserDeviceToolbarState.ts | 11 | CUT | Browser / preview / devices |  |  |
| browserLinkTarget.ts | 68 | CUT | Browser / preview / devices |  |  |
| browserPointerStore.ts | 25 | CUT | Browser / preview / devices |  |  |
| browserRecording.ts | 853 | CUT | Browser / preview / devices |  |  |
| browserRecordingScope.ts | 14 | CUT | Browser / preview / devices |  |  |
| browserSurfaceStore.ts | 231 | CUT | Browser / preview / devices |  |  |
| browserTargetResolver.ts | 115 | CUT | Browser / preview / devices |  |  |
| browserViewportActions.ts | 90 | CUT | Browser / preview / devices |  |  |
| browserViewportLayout.ts | 316 | CUT | Browser / preview / devices |  |  |
| desktopTabLifetime.ts | 89 | CUT | Browser / preview / devices |  |  |
| hostedBrowserWebviewStyle.ts | 75 | CUT | Browser / preview / devices |  |  |
| openFileInPreview.ts | 168 | CUT | Browser / preview / devices |  |  |
| previewRuntime.ts | 79 | CUT | Browser / preview / devices |  |  |
| previewRuntimeTabId.ts | 23 | CUT | Browser / preview / devices |  |  |
| previewWebviewConfigState.ts | 104 | CUT | Browser / preview / devices |  |  |
| recordingCompositor.ts | 190 | CUT | Browser / preview / devices |  |  |
| serverPictureInPicture.ts | 177 | CUT | Browser / preview / devices |  |  |
| useBrowserViewportResize.ts | 260 | CUT | Browser / preview / devices |  |  |
| useOpenLink.ts | 72 | CUT | Browser / preview / devices |  |  |
| webviewCrashRecovery.ts | 38 | CUT | Browser / preview / devices |  |  |

### `apps/web/src/cloud/`

12 files, 1,532 lines: KEEP 0 / 0, CUT 12 / 1,532, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| connectCliAuth.ts | 68 | CUT | T3 Connect / relay / remote access / auth |  |  |
| connectOnboarding.ts | 18 | CUT | T3 Connect / relay / remote access / auth |  |  |
| dpop.ts | 185 | CUT | T3 Connect / relay / remote access / auth |  |  |
| linkEnvironment.ts | 349 | CUT | T3 Connect / relay / remote access / auth |  |  |
| linkEnvironmentAtoms.ts | 49 | CUT | T3 Connect / relay / remote access / auth |  |  |
| managedAuth.tsx | 108 | CUT | T3 Connect / relay / remote access / auth |  |  |
| managedRelayLayer.ts | 82 | CUT | T3 Connect / relay / remote access / auth |  |  |
| managedRelayState.ts | 116 | CUT | T3 Connect / relay / remote access / auth |  |  |
| primaryCloudLinkState.ts | 104 | CUT | T3 Connect / relay / remote access / auth |  |  |
| publicConfig.ts | 83 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayClientInstallDialog.ts | 126 | CUT | T3 Connect / relay / remote access / auth |  |  |
| useCloudLinkController.ts | 244 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/components/`

97 files, 58,168 lines: KEEP 54 / 39,805, CUT 17 / 4,845, UNASSIGNED 26 / 13,518, ported 16 / 12,638.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AnimatedHeight.tsx | 101 | KEEP | T03 | not started |  |
| AnimatedWidth.tsx | 39 | KEEP | T03 | not started |  |
| AppSidebarLayout.tsx | 361 | KEEP | T03 | ported | `main:app/src/components/app_sidebar_layout.rs` |
| BranchPicker.tsx | 307 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| BranchToolbar.logic.ts | 329 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| BranchToolbar.tsx | 865 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| BranchToolbarBranchSelector.tsx | 827 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| BranchToolbarEnvModeSelector.tsx | 177 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| BranchToolbarEnvironmentSelector.tsx | 144 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| ChatMarkdown.tsx | 3,500 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/markdown/chat_markdown.rs` |
| ChatView.logic.ts | 1,347 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat_view.rs (name match, no header)` |
| ChatView.tsx | 12,271 | KEEP | T16 | partial | `T16-logic:app/src/components/app_sidebar_layout.rs` |
| CommandBlock.tsx | 45 | KEEP | T16 | not started |  |
| CommandPalette.logic.ts | 623 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| CommandPalette.tsx | 3,667 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| CommandPaletteContent.tsx | 100 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| CommandPaletteResults.tsx | 247 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| ComposerContextLabel.tsx | 37 | KEEP | T17 | not started |  |
| ComposerPromptEditor.tsx | 17 | KEEP | T17 | not started |  |
| ComposerPromptEditorTiptap.tsx | 1,985 | KEEP | T17 | not started |  |
| ConfirmDialogHost.tsx | 96 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| ConnectionStatusDot.tsx | 77 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ContextChip.tsx | 133 | KEEP | T17 | not started |  |
| CustomSnoozeDialog.tsx | 226 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/custom_snooze_dialog.rs` |
| DiffFilePathCopyButton.tsx | 45 | KEEP | T18 | not started |  |
| DiffPanel.tsx | 1,254 | KEEP | T18 | not started |  |
| DiffPanelShell.tsx | 116 | KEEP | T18 | not started |  |
| DiffWorkerPoolProvider.tsx | 167 | KEEP | T18 | not started |  |
| EnvironmentMachineIcon.tsx | 78 | CUT | T3 Connect / relay / remote access / auth |  |  |
| GitActionsControl.logic.ts | 488 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| GitActionsControl.tsx | 2,159 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| Icons.tsx | 762 | KEEP | T03 | not started |  |
| JetBrainsIcons.tsx | 610 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| KeybindingsConfigWarning.tsx | 54 | KEEP | T20 | not started |  |
| KeybindingsUpdateToast.logic.ts | 45 | KEEP | T20 | not started |  |
| LegacySidebar.tsx | 3,900 | KEEP | T15 | partial | `T15-sidebar:app/src/components/sidebar.rs` |
| LegacyThreadMigrationToast.tsx | 45 | CUT | Legacy v1 importer |  |  |
| MorphIcon.tsx | 6 | KEEP | T03 | not started |  |
| NightlyMobileBeta.tsx | 150 | CUT | Mobile |  |  |
| NoActiveThreadState.tsx | 35 | KEEP | T16 | not started |  |
| NoProjectsHero.tsx | 55 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| PermissionUpdateNotice.tsx | 78 | CUT | Electron shell / desktop-only infra |  |  |
| PreviousWorktreeItemContent.tsx | 19 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| ProjectCloneToastCoordinator.tsx | 240 | CUT | Project clone |  |  |
| ProjectEnvironmentBadge.tsx | 54 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ProjectFavicon.tsx | 196 | KEEP | T15 | ported | `T15-sidebar:app/src/components/project_favicon.rs` |
| ProjectMonogram.tsx | 52 | KEEP | T15 | ported | `T15-sidebar:app/src/components/project_monogram.rs` |
| ProjectScriptsControl.tsx | 482 | CUT | Project scripts run in terminal; t3.json is import-only (hooks replace) |  |  |
| ProviderUpdateEnvironmentRows.tsx | 409 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| ProviderUpdateLaunchNotification.environments.ts | 98 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| ProviderUpdateLaunchNotification.logic.ts | 864 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| ProviderUpdateLaunchNotification.tsx | 198 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| ProviderUpdatePrimaryNotification.tsx | 357 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| ProviderUpdatesAction.tsx | 128 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| PullRequestContextDetails.tsx | 25 | KEEP | T19 | not started |  |
| PullRequestThreadDialog.tsx | 334 | KEEP | T19 | not started |  |
| QuitHoldOverlay.tsx | 56 | **UNASSIGNED** | N9 Native macOS shell |  |  |
| RenderErrorBoundary.tsx | 41 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| ReopenClosedViewShortcut.tsx | 220 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| RightPanelSheet.tsx | 31 | KEEP | T03 | not started |  |
| RightPanelTabs.tsx | 1,524 | KEEP | T03 | partial | `main:app/src/components/right_panel_tabs.rs` |
| ServerUpdateAction.tsx | 407 | CUT | Auto-updates (app / server) |  |  |
| Sidebar.drag.ts | 266 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_drag.rs` |
| Sidebar.logic.ts | 1,458 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_logic.rs` |
| Sidebar.motion.ts | 266 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_motion.rs` |
| Sidebar.pointer.ts | 157 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_pointer.rs` |
| Sidebar.snooze.ts | 57 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_snooze.rs` |
| Sidebar.tsx | 5,622 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar.rs` |
| SidebarStageBackdrop.tsx | 391 | CUT | Dev/nightly stage backdrop (single prod build; PORTING Deviations) |  |  |
| SlowRpcRequestToastCoordinator.tsx | 76 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| StartTruncatedPath.tsx | 25 | KEEP | T03 | not started |  |
| T3Wordmark.tsx | 12 | KEEP | T03 | partial | `T16-logic:app/src/assets.rs` |
| ThreadCommandSubtitle.tsx | 117 | KEEP | T16 | not started |  |
| ThreadContextChip.tsx | 39 | KEEP | T17 | not started |  |
| ThreadHoverCard.tsx | 34 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/thread_hover_card.rs` |
| ThreadNotificationCoordinator.tsx | 255 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| ThreadRouteView.tsx | 198 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| ThreadSearchMatch.tsx | 66 | KEEP | T15 | ported | `T15-sidebar:app/src/components/thread_search_match.rs` |
| ThreadStatusIndicators.tsx | 1,177 | KEEP | T15 | partial | `T15-sidebar:app/src/components/sidebar/thread_status_indicators.rs` |
| ThreadTerminalDrawer.tsx | 1,902 | CUT | Terminal / PTY |  |  |
| WorkspaceBreadcrumb.tsx | 77 | KEEP | T03 | ported | `main:app/src/components/workspace_breadcrumb.rs` |
| WorkspacePageContainer.tsx | 29 | KEEP | T03 | not started |  |
| WorkspacePageHeader.tsx | 31 | KEEP | T03 | not started |  |
| WorktreeBaseBranchPicker.tsx | 126 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| composerCodeBlockHighlight.ts | 231 | KEEP | T17 | not started |  |
| composerContextPresentation.tsx | 461 | KEEP | T17 | not started |  |
| composerFooterLayout.ts | 241 | KEEP | T17 | not started |  |
| composerInlineChip.ts | 14 | KEEP | T17 | not started |  |
| composerInlineTokenPaste.ts | 68 | KEEP | T17 | not started |  |
| composerSelection.ts | 15 | KEEP | T17 | not started |  |
| contextChipParts.tsx | 304 | KEEP | T17 | not started |  |
| contextPresentationRegistry.ts | 130 | KEEP | T17 | not started |  |
| desktopUpdate.logic.ts | 130 | CUT | Auto-updates (app / server) |  |  |
| desktopUpdate.toast.tsx | 64 | CUT | Auto-updates (app / server) |  |  |
| projectScriptEditor.tsx | 526 | CUT | Project scripts run in terminal; t3.json is import-only (hooks replace) |  |  |
| threadActionMenu.logic.ts | 265 | KEEP | T15 | ported | `T15-sidebar:app/src/components/thread_action_menu_logic.rs` |
| threadSidebarWidth.ts | 35 | KEEP | T15 | ported | `T15-sidebar:app/src/components/thread_sidebar_width.rs` |

### `apps/web/src/components/auth/`

3 files, 668 lines: KEEP 0 / 0, CUT 3 / 668, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AuthSurfaceShell.tsx | 40 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ConnectAgentSurface.tsx | 333 | CUT | T3 Connect / relay / remote access / auth |  |  |
| PairingRouteSurface.tsx | 295 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/components/chat/`

145 files, 39,053 lines: KEEP 131 / 36,381, CUT 11 / 2,137, UNASSIGNED 3 / 535, ported 11 / 9,779.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AgentElapsed.tsx | 76 | KEEP | T16 | not started |  |
| AssistantCitationChip.tsx | 278 | KEEP | T16 | not started |  |
| AssistantCitationCommentEditor.tsx | 102 | KEEP | T16 | not started |  |
| AssistantCitationSource.tsx | 377 | KEEP | T16 | not started |  |
| AssistantSelectionToolbar.tsx | 163 | KEEP | T16 | not started |  |
| ChangedFilesTree.tsx | 260 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/changed_files_tree.rs` |
| ChatCanvas.tsx | 172 | KEEP | T16 | not started |  |
| ChatCanvasContext.ts | 18 | KEEP | T16 | not started |  |
| ChatComposer.tsx | 7,852 | KEEP | T17 | partial | `T17-composer:app/src/components/chat/composer_surface.rs` |
| ChatGptSharingControl.tsx | 17 | KEEP | T16 | not started |  |
| ChatHeader.tsx | 441 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/chat_header.rs` |
| CompactComposerControlsMenu.tsx | 95 | KEEP | T17 | not started |  |
| ComposerActivityStatus.tsx | 23 | KEEP | T17 | not started |  |
| ComposerBanner.tsx | 369 | KEEP | T17 | partial | `T17-composer:app/src/components/chat/composer_pending_panels.rs` |
| ComposerBannerStack.tsx | 377 | KEEP | T17 | not started |  |
| ComposerCodeBlockLanguagePicker.tsx | 121 | KEEP | T17 | not started |  |
| ComposerCodeBlockNodeView.tsx | 67 | KEEP | T17 | not started |  |
| ComposerCommandMenu.tsx | 274 | KEEP | T17 | not started |  |
| ComposerControl.tsx | 127 | KEEP | T17 | not started |  |
| ComposerFeedback.tsx | 49 | KEEP | T17 | not started |  |
| ComposerImageThumbnail.tsx | 29 | KEEP | T17 | not started |  |
| ComposerPendingApprovalActions.tsx | 116 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/composer_pending_panels.rs` |
| ComposerPendingApprovalPanel.tsx | 68 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/composer_pending_panels.rs` |
| ComposerPendingTerminalContexts.tsx | 32 | CUT | Terminal / PTY |  |  |
| ComposerPendingUserInputPanel.tsx | 325 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/composer_pending_panels.rs` |
| ComposerPlanFollowUpBanner.tsx | 20 | KEEP | T16 | not started |  |
| ComposerPrimaryActions.tsx | 399 | KEEP | T17 | partial | `T17-composer:app/src/components/chat/composer_surface.rs` |
| ComposerPromptLengthValidation.tsx | 13 | KEEP | T17 | not started |  |
| ComposerServerUpdateStatus.tsx | 65 | CUT | Auto-updates (app / server) |  |  |
| ComposerStashBadge.tsx | 66 | KEEP | T17 | not started |  |
| ComposerStashMenu.tsx | 227 | KEEP | T17 | not started |  |
| ComposerSurface.tsx | 101 | KEEP | T17 | ported | `T17-composer:app/src/components/chat/composer_surface.rs` |
| ComposerTasksBadge.tsx | 235 | KEEP | T17 | not started |  |
| ComposerUsageLimits.tsx | 137 | KEEP | T17 | not started |  |
| ContextWindowMeter.logic.ts | 149 | KEEP | T16 | not started |  |
| ContextWindowMeter.tsx | 180 | KEEP | T16 | not started |  |
| DiffStatLabel.tsx | 53 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/changed_files_tree.rs` |
| DraftHeroHeadline.tsx | 430 | KEEP | T17 | not started |  |
| ExpandedImageDialog.tsx | 294 | KEEP | T16 | not started |  |
| ExpandedImagePreview.tsx | 199 | KEEP | T16 | not started |  |
| FileTagChip.tsx | 24 | KEEP | T17 | not started |  |
| HighlightedCodeLines.tsx | 48 | KEEP | T16 | not started |  |
| HighlightedTokens.tsx | 204 | KEEP | T16 | not started |  |
| HtmlRenderDialog.tsx | 83 | CUT | Browser / preview / devices |  |  |
| HtmlRenderFrame.tsx | 155 | CUT | Browser / preview / devices |  |  |
| MarkdownThreadLink.tsx | 53 | KEEP | T16 | not started |  |
| McpAppFrame.tsx | 550 | CUT | MCP apps / orchestrator MCP server |  |  |
| MermaidDiagram.tsx | 217 | KEEP | T16 | not started |  |
| MessageCopyButton.tsx | 67 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/message_copy_button.rs` |
| MessagesTimeline.logic.ts | 2,389 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/messages_timeline_logic.rs` |
| MessagesTimeline.tsx | 5,947 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/messages_timeline.rs` |
| ModelListRow.tsx | 153 | KEEP | T17 | not started |  |
| ModelPickerContent.tsx | 1,109 | KEEP | T17 | not started |  |
| ModelPickerSidebar.tsx | 254 | KEEP | T17 | not started |  |
| OpenInPicker.logic.ts | 22 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| OpenInPicker.tsx | 470 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| OpenInPickerShortcut.ts | 43 | **UNASSIGNED** | N5 Editor / Open-in integration |  |  |
| PanelLayoutControls.tsx | 161 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/panel_layout_controls.rs` |
| PierreEntryIcon.tsx | 96 | KEEP | T16 | not started |  |
| ProposedPlanCard.tsx | 287 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/timeline_message_rows.rs` |
| ProviderInstanceIcon.tsx | 142 | KEEP | T16 | not started |  |
| ProviderModelPicker.tsx | 326 | KEEP | T17 | not started |  |
| ProviderPackageIcon.tsx | 75 | KEEP | T16 | not started |  |
| ProviderStatusBanner.tsx | 152 | KEEP | T16 | not started |  |
| ProviderSubagentBar.tsx | 93 | KEEP | T16 | not started |  |
| QueuedRunsControl.tsx | 496 | KEEP | T17 | not started |  |
| SecretRequestCard.tsx | 179 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| ShellCommandBlock.tsx | 76 | KEEP | T16 | not started |  |
| SkillInlineText.tsx | 75 | KEEP | T17 | not started |  |
| SnapShotAttachmentDetails.tsx | 168 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SubagentTooltipContent.tsx | 191 | KEEP | T16 | not started |  |
| TerminalContextInlineChip.tsx | 66 | CUT | Terminal / PTY |  |  |
| ThreadAutomationsPanel.tsx | 240 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| ThreadContextDragGhost.tsx | 26 | KEEP | T17 | not started |  |
| ThreadDetailsCard.tsx | 203 | KEEP | T16 | not started |  |
| ThreadDetailsControl.tsx | 92 | KEEP | T16 | not started |  |
| ThreadDetailsPanel.tsx | 189 | KEEP | T16 | not started |  |
| ThreadDetailsPrRow.tsx | 493 | KEEP | T16 | not started |  |
| ThreadDetailsPrRows.tsx | 130 | KEEP | T16 | not started |  |
| ThreadDetailsSection.tsx | 45 | KEEP | T16 | not started |  |
| ThreadErrorBanner.tsx | 88 | KEEP | T16 | not started |  |
| ThreadFindBar.tsx | 129 | KEEP | T16 | not started |  |
| ThreadFindProvider.tsx | 73 | KEEP | T16 | not started |  |
| ThreadLineageWorkflowRow.tsx | 266 | KEEP | T16 | not started |  |
| ThreadRelationshipIcon.tsx | 89 | KEEP | T16 | not started |  |
| ThreadRelationshipsControl.tsx | 702 | KEEP | T16 | not started |  |
| ThreadStatusLine.tsx | 30 | KEEP | T16 | not started |  |
| TimelineSystemDivider.tsx | 67 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/timeline_work_rows.rs` |
| TraitsPicker.tsx | 698 | KEEP | T17 | not started |  |
| TraitsSpeed.tsx | 70 | KEEP | T17 | not started |  |
| UsageLimitRecoveryBanner.tsx | 98 | KEEP | T16 | not started |  |
| V2ItemInspector.tsx | 447 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/timeline_work_rows.rs` |
| V2LifecycleRow.tsx | 643 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/timeline_work_rows.rs` |
| WorkLog.tsx | 176 | KEEP | T16 | ported | `T16-thread-view:app/src/components/chat/work_log.rs` |
| WorkspaceEntryIcon.tsx | 41 | KEEP | T16 | not started |  |
| WorktreeSetupCard.tsx | 436 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |
| ZoomableImage.tsx | 247 | KEEP | T16 | not started |  |
| agentSpawnSummary.ts | 64 | KEEP | T16 | not started |  |
| assistantCitationCommentDismissal.ts | 21 | KEEP | T16 | not started |  |
| chatCanvasLayout.ts | 200 | KEEP | T16 | not started |  |
| composerAttachmentFiles.ts | 177 | KEEP | T17 | not started |  |
| composerContextUndo.ts | 65 | KEEP | T17 | not started |  |
| composerEventScope.ts | 52 | KEEP | T17 | not started |  |
| composerMentionDrag.ts | 98 | KEEP | T17 | not started |  |
| composerMenuHighlight.ts | 20 | KEEP | T17 | not started |  |
| composerPromptHistory.ts | 215 | KEEP | T17 | not started |  |
| composerProviderState.tsx | 232 | KEEP | T17 | not started |  |
| composerScrollGesture.ts | 79 | KEEP | T17 | not started |  |
| composerSlashCommandSearch.ts | 114 | KEEP | T17 | not started |  |
| composerSubmission.ts | 50 | KEEP | T17 | not started |  |
| composerTypingGuard.ts | 51 | KEEP | T17 | not started |  |
| draftHeroTransition.ts | 95 | KEEP | T17 | not started |  |
| externalLinkContextMenu.ts | 131 | KEEP | T16 | not started |  |
| folderDrop.ts | 24 | KEEP | T17 | not started |  |
| markdownFindContext.ts | 20 | KEEP | T16 | not started |  |
| markdownImageGallery.ts | 51 | KEEP | T16 | not started |  |
| modelPickerKeys.ts | 47 | KEEP | T17 | not started |  |
| modelPickerSearch.ts | 87 | KEEP | T17 | not started |  |
| pageScrollController.ts | 307 | KEEP | T16 | not started |  |
| pendingDraftWork.ts | 32 | KEEP | T17 | not started |  |
| providerIconUtils.ts | 62 | KEEP | T16 | not started |  |
| queuedMessageEdit.ts | 54 | KEEP | T17 | not started |  |
| restingComposerControlsMeasurement.ts | 110 | KEEP | T17 | not started |  |
| runtimeModeConfig.ts | 30 | KEEP | T17 | not started |  |
| threadContextDrag.ts | 86 | KEEP | T17 | not started |  |
| threadDetailsCardLayout.ts | 55 | KEEP | T16 | not started |  |
| threadDetailsPanelStyles.ts | 45 | KEEP | T16 | not started |  |
| threadFind.ts | 28 | KEEP | T16 | not started |  |
| threadFindActionBus.ts | 10 | KEEP | T16 | not started |  |
| threadFindHighlights.ts | 269 | KEEP | T16 | not started |  |
| threadFindPosition.ts | 24 | KEEP | T16 | not started |  |
| timelineMinimapItems.ts | 66 | KEEP | T16 | not started |  |
| timelineMinimapWheel.ts | 29 | KEEP | T16 | not started |  |
| timelineScrollAnchoring.ts | 182 | KEEP | T16 | not started |  |
| timelineScrollTarget.ts | 31 | KEEP | T16 | not started |  |
| useAssistantCitationTarget.ts | 191 | KEEP | T16 | not started |  |
| useAutoBalanceUpdateBanner.tsx | 163 | CUT | Auto-updates (app / server) |  |  |
| useComposerFocusState.ts | 20 | KEEP | T17 | not started |  |
| useComposerMenuState.ts | 12 | KEEP | T17 | not started |  |
| useComposerMultilinePrompt.ts | 45 | KEEP | T17 | not started |  |
| useComposerTriggerState.ts | 42 | KEEP | T17 | not started |  |
| useComposerTypingGuard.ts | 51 | KEEP | T17 | not started |  |
| useThreadFind.ts | 386 | KEEP | T16 | not started |  |
| useThreadFindNavigation.ts | 261 | KEEP | T16 | not started |  |
| workspaceFileDrop.ts | 84 | KEEP | T17 | not started |  |

### `apps/web/src/components/clerk/`

12 files, 873 lines: KEEP 0 / 0, CUT 12 / 873, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| BrowserManagedAuthShell.tsx | 25 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ClerkUserProfilePage.tsx | 87 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ElectronManagedAuthShell.tsx | 26 | CUT | T3 Connect / relay / remote access / auth |  |  |
| MobileClientsUserProfilePage.logic.ts | 45 | CUT | T3 Connect / relay / remote access / auth |  |  |
| MobileClientsUserProfilePage.tsx | 157 | CUT | T3 Connect / relay / remote access / auth |  |  |
| RemoveT3ConnectEnvironmentDialog.tsx | 78 | CUT | T3 Connect / relay / remote access / auth |  |  |
| T3ConnectAccountPages.tsx | 66 | CUT | T3 Connect / relay / remote access / auth |  |  |
| T3ConnectSidebarSignIn.tsx | 68 | CUT | T3 Connect / relay / remote access / auth |  |  |
| T3ConnectUserProfilePage.tsx | 254 | CUT | T3 Connect / relay / remote access / auth |  |  |
| authRedirect.ts | 21 | CUT | T3 Connect / relay / remote access / auth |  |  |
| clerkAppearance.ts | 34 | CUT | T3 Connect / relay / remote access / auth |  |  |
| useT3ConnectAuthPrompt.tsx | 12 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/components/cloud/`

5 files, 1,241 lines: KEEP 0 / 0, CUT 5 / 1,241, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| CloudEnvironmentConnectList.tsx | 511 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ConnectCliAuthSurface.tsx | 113 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ConnectOnboardingDialog.tsx | 429 | CUT | T3 Connect / relay / remote access / auth |  |  |
| RelayClientInstallDialog.tsx | 123 | CUT | T3 Connect / relay / remote access / auth |  |  |
| cloudEnvironmentConnectionPresentation.ts | 65 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/components/desktop/`

6 files, 929 lines: KEEP 0 / 0, CUT 6 / 929, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopAppActivationCoordinator.tsx | 100 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopWebLinkCoordinator.tsx | 154 | CUT | Electron shell / desktop-only infra |  |  |
| RunningThreadKeepAlive.tsx | 14 | CUT | Electron shell / desktop-only infra |  |  |
| SnapShotCoordinator.tsx | 414 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotShortcutKeys.tsx | 26 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SshPasswordPromptDialog.tsx | 221 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/components/device/`

21 files, 3,893 lines: KEEP 0 / 0, CUT 21 / 3,893, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DeviceAndroidFoldControls.tsx | 155 | CUT | Browser / preview / devices |  |  |
| DeviceControlsRail.tsx | 309 | CUT | Browser / preview / devices |  |  |
| DeviceDuoControls.tsx | 136 | CUT | Browser / preview / devices |  |  |
| DeviceDuoGlyph.tsx | 49 | CUT | Browser / preview / devices |  |  |
| DeviceDuoViewport.tsx | 205 | CUT | Browser / preview / devices |  |  |
| DeviceHostAvailability.tsx | 27 | CUT | Browser / preview / devices |  |  |
| DeviceHostUpdates.tsx | 67 | CUT | Browser / preview / devices |  |  |
| DeviceLoadingView.tsx | 50 | CUT | Browser / preview / devices |  |  |
| DevicePanel.tsx | 325 | CUT | Browser / preview / devices |  |  |
| DevicePhoneViewport.tsx | 159 | CUT | Browser / preview / devices |  |  |
| DeviceSetup.tsx | 318 | CUT | Browser / preview / devices |  |  |
| DeviceStreamView.tsx | 580 | CUT | Browser / preview / devices |  |  |
| DeviceToolVersions.tsx | 90 | CUT | Browser / preview / devices |  |  |
| DeviceToolsPanel.tsx | 660 | CUT | Browser / preview / devices |  |  |
| DeviceWorkspace.tsx | 131 | CUT | Browser / preview / devices |  |  |
| deviceFold.ts | 72 | CUT | Browser / preview / devices |  |  |
| deviceFrameLayout.ts | 8 | CUT | Browser / preview / devices |  |  |
| deviceHubApi.ts | 248 | CUT | Browser / preview / devices |  |  |
| deviceModels.ts | 37 | CUT | Browser / preview / devices |  |  |
| phoneTrackpad.ts | 137 | CUT | Browser / preview / devices |  |  |
| useDeviceControls.ts | 130 | CUT | Browser / preview / devices |  |  |

### `apps/web/src/components/diffs/`

10 files, 1,432 lines: KEEP 10 / 1,432, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AnnotatableCodeView.tsx | 306 | KEEP | T18 | not started |  |
| DiffCommentAnnotation.tsx | 137 | KEEP | T18 | not started |  |
| DiffFileLoadingBoundary.tsx | 27 | KEEP | T18 | not started |  |
| DiffFileStatus.tsx | 39 | KEEP | T18 | not started |  |
| DiffFileTree.tsx | 236 | KEEP | T18 | not started |  |
| StyledDiffCodeView.tsx | 261 | KEEP | T18 | not started |  |
| commentSubmitShortcut.ts | 16 | KEEP | T18 | not started |  |
| diffFileTree.logic.ts | 132 | KEEP | T18 | not started |  |
| useCodeViewFileReveal.ts | 33 | KEEP | T18 | not started |  |
| useReviewFilePatches.ts | 245 | KEEP | T18 | not started |  |

### `apps/web/src/components/files/`

25 files, 4,668 lines: KEEP 0 / 0, CUT 25 / 4,668, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AttachmentFilePreview.tsx | 366 | CUT | File tree / file preview side panel |  |  |
| AudioPreview.tsx | 19 | CUT | File tree / file preview side panel |  |  |
| BrowserDocumentFrame.tsx | 303 | CUT | File tree / file preview side panel |  |  |
| DelimitedTablePreview.tsx | 63 | CUT | File tree / file preview side panel |  |  |
| FileBreadcrumbs.tsx | 295 | CUT | File tree / file preview side panel |  |  |
| FileBrowserPanel.tsx | 574 | CUT | File tree / file preview side panel |  |  |
| FileMarkdownPreview.tsx | 34 | CUT | File tree / file preview side panel |  |  |
| FilePreviewPanel.tsx | 1,508 | CUT | File tree / file preview side panel |  |  |
| ProjectFilePicker.logic.ts | 73 | CUT | File tree / file preview side panel |  |  |
| ProjectFilePicker.tsx | 163 | CUT | File tree / file preview side panel |  |  |
| ReadOnlySourcePreview.tsx | 51 | CUT | File tree / file preview side panel |  |  |
| fileCommentAnnotations.ts | 54 | CUT | File tree / file preview side panel |  |  |
| fileContentRevision.ts | 12 | CUT | File tree / file preview side panel |  |  |
| fileEditorDismissal.ts | 53 | CUT | File tree / file preview side panel |  |  |
| fileLineReveal.ts | 24 | CUT | File tree / file preview side panel |  |  |
| filePath.ts | 60 | CUT | File tree / file preview side panel |  |  |
| filePreviewMode.ts | 76 | CUT | File tree / file preview side panel |  |  |
| fileSaveCoordinator.ts | 85 | CUT | File tree / file preview side panel |  |  |
| fileSurfaceChrome.tsx | 163 | CUT | File tree / file preview side panel |  |  |
| fileTreeDragMention.ts | 95 | CUT | File tree / file preview side panel |  |  |
| fileTreeExpansion.ts | 55 | CUT | File tree / file preview side panel |  |  |
| fileTreePathReconciliation.ts | 32 | CUT | File tree / file preview side panel |  |  |
| projectFilesQueryState.ts | 289 | CUT | File tree / file preview side panel |  |  |
| useDirectoryEntries.ts | 147 | CUT | File tree / file preview side panel |  |  |
| useFileSaveCoordinator.ts | 74 | CUT | File tree / file preview side panel |  |  |

### `apps/web/src/components/media/`

4 files, 580 lines: KEEP 0 / 0, CUT 4 / 580, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| MediaActions.tsx | 237 | CUT | Media viewers (PORTING Deviations: media viewers cut) |  |  |
| MediaVideoPlayer.tsx | 216 | CUT | Media viewers (PORTING Deviations: media viewers cut) |  |  |
| OpenMediaLink.tsx | 45 | CUT | Media viewers (PORTING Deviations: media viewers cut) |  |  |
| mediaContent.ts | 82 | CUT | Media viewers (PORTING Deviations: media viewers cut) |  |  |

### `apps/web/src/components/onboarding/`

2 files, 2,075 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 2 / 2,075, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| FirstRunGate.tsx | 238 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| WelcomeWizard.tsx | 1,837 | **UNASSIGNED** | N2 Projects and onboarding |  |  |

### `apps/web/src/components/permissions/`

2 files, 102 lines: KEEP 0 / 0, CUT 2 / 102, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| PermissionChecklist.tsx | 57 | CUT | Electron shell / desktop-only infra |  |  |
| usePermissionStatus.ts | 45 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/web/src/components/preview/`

33 files, 4,267 lines: KEEP 1 / 162, CUT 32 / 4,105, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AgentBrowserCursor.tsx | 99 | CUT | Browser / preview / devices |  |  |
| BrowserMockup.tsx | 24 | CUT | Browser / preview / devices |  |  |
| PreviewChromeRow.tsx | 320 | CUT | Browser / preview / devices |  |  |
| PreviewEmptyState.tsx | 96 | CUT | Browser / preview / devices |  |  |
| PreviewFaviconIcon.tsx | 59 | CUT | Browser / preview / devices |  |  |
| PreviewLocalServerCard.tsx | 28 | CUT | Browser / preview / devices |  |  |
| PreviewMoreMenu.tsx | 207 | CUT | Browser / preview / devices |  |  |
| PreviewPanel.tsx | 63 | CUT | Browser / preview / devices |  |  |
| PreviewPanelShell.tsx | 162 | KEEP | T03 | partial | `main:app/src/components/right_panel_tabs.rs` |
| PreviewRecentUrlCard.tsx | 53 | CUT | Browser / preview / devices |  |  |
| PreviewUnreachable.tsx | 144 | CUT | Browser / preview / devices |  |  |
| PreviewView.tsx | 1,154 | CUT | Browser / preview / devices |  |  |
| RightPanelResizeHandle.tsx | 34 | CUT | Browser / preview / devices |  |  |
| ThreadPreviewMiniPlayer.tsx | 564 | CUT | Browser / preview / devices |  |  |
| ZoomIndicator.tsx | 54 | CUT | Browser / preview / devices |  |  |
| addBrowserSurface.ts | 27 | CUT | Browser / preview / devices |  |  |
| agentBrowserCursorLogic.ts | 6 | CUT | Browser / preview / devices |  |  |
| closePreviewSession.ts | 44 | CUT | Browser / preview / devices |  |  |
| errorCodeMessages.ts | 12 | CUT | Browser / preview / devices |  |  |
| fileExplorerLabel.ts | 23 | CUT | Browser / preview / devices |  |  |
| openDiscoveredPort.ts | 33 | CUT | Browser / preview / devices |  |  |
| openPreviewSession.ts | 71 | CUT | Browser / preview / devices |  |  |
| openTerminalLinkInPreview.ts | 97 | CUT | Browser / preview / devices |  |  |
| previewActionBus.ts | 31 | CUT | Browser / preview / devices |  |  |
| previewBridge.ts | 9 | CUT | Browser / preview / devices |  |  |
| previewConstants.ts | 21 | CUT | Browser / preview / devices |  |  |
| previewEmptyStateLogic.ts | 11 | CUT | Browser / preview / devices |  |  |
| previewMiniPlayerLayout.ts | 386 | CUT | Browser / preview / devices |  |  |
| previewViewportRollback.ts | 18 | CUT | Browser / preview / devices |  |  |
| showPreviewPopup.ts | 24 | CUT | Browser / preview / devices |  |  |
| useDiscoveredLocalServers.ts | 105 | CUT | Browser / preview / devices |  |  |
| usePreviewBridge.ts | 196 | CUT | Browser / preview / devices |  |  |
| usePreviewSession.ts | 92 | CUT | Browser / preview / devices |  |  |

### `apps/web/src/components/promptLinks/`

5 files, 752 lines: KEEP 0 / 0, CUT 1 / 179, UNASSIGNED 4 / 573, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| HostedPromptLinkLanding.tsx | 179 | CUT | T3 Connect / relay / remote access / auth |  |  |
| NewThreadLinkView.tsx | 379 | **UNASSIGNED** | N10 Prompt links |  |  |
| ThreadLinkPrompt.tsx | 50 | **UNASSIGNED** | N10 Prompt links |  |  |
| copyPromptLink.ts | 88 | **UNASSIGNED** | N10 Prompt links |  |  |
| linkedPrompt.ts | 56 | **UNASSIGNED** | N10 Prompt links |  |  |

### `apps/web/src/components/pullRequest/`

58 files, 17,599 lines: KEEP 58 / 17,599, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| LinkPullRequestDialog.tsx | 255 | KEEP | T19 | not started |  |
| PullRequestActivityUnavailableState.tsx | 31 | KEEP | T19 | not started |  |
| PullRequestCandidatePicker.tsx | 164 | KEEP | T19 | not started |  |
| PullRequestChecksPopover.tsx | 219 | KEEP | T19 | not started |  |
| PullRequestCodeTab.tsx | 1,639 | KEEP | T19 | not started |  |
| PullRequestCommentBody.tsx | 60 | KEEP | T19 | not started |  |
| PullRequestCommentForm.tsx | 146 | KEEP | T19 | not started |  |
| PullRequestComposer.tsx | 204 | KEEP | T19 | not started |  |
| PullRequestCopyableCode.tsx | 66 | KEEP | T19 | not started |  |
| PullRequestDetailLayout.tsx | 130 | KEEP | T19 | not started |  |
| PullRequestDetailPanel.tsx | 3,050 | KEEP | T19 | not started |  |
| PullRequestEditButton.tsx | 26 | KEEP | T19 | not started |  |
| PullRequestGhosts.tsx | 444 | KEEP | T19 | not started |  |
| PullRequestGuideTab.tsx | 183 | KEEP | T19 | not started |  |
| PullRequestLabelPicker.tsx | 129 | KEEP | T19 | not started |  |
| PullRequestLinkPreview.tsx | 153 | KEEP | T19 | not started |  |
| PullRequestListEmptyState.tsx | 185 | KEEP | T19 | not started |  |
| PullRequestListFilters.tsx | 601 | KEEP | T19 | not started |  |
| PullRequestListRow.tsx | 165 | KEEP | T19 | not started |  |
| PullRequestMarkdown.tsx | 150 | KEEP | T19 | not started |  |
| PullRequestMarkdownEditor.tsx | 152 | KEEP | T19 | not started |  |
| PullRequestReactions.tsx | 169 | KEEP | T19 | not started |  |
| PullRequestReviewAnnotation.tsx | 382 | KEEP | T19 | not started |  |
| PullRequestReviewForm.tsx | 182 | KEEP | T19 | not started |  |
| PullRequestReviewerPicker.tsx | 134 | KEEP | T19 | not started |  |
| PullRequestRow.tsx | 283 | KEEP | T19 | not started |  |
| PullRequestSpeedActions.tsx | 130 | KEEP | T19 | not started |  |
| PullRequestStackHeader.tsx | 28 | KEEP | T19 | not started |  |
| PullRequestStackLayerContent.tsx | 28 | KEEP | T19 | not started |  |
| PullRequestStackLayers.tsx | 39 | KEEP | T19 | not started |  |
| PullRequestStackMenu.tsx | 266 | KEEP | T19 | not started |  |
| PullRequestStackPopover.tsx | 107 | KEEP | T19 | not started |  |
| PullRequestSummaryTab.tsx | 963 | KEEP | T19 | not started |  |
| PullRequestThreadLinks.tsx | 245 | KEEP | T19 | not started |  |
| PullRequestTimelineTab.tsx | 630 | KEEP | T19 | not started |  |
| PullRequestsUnavailableState.tsx | 60 | KEEP | T19 | not started |  |
| ThreadPullRequestsPanel.tsx | 458 | KEEP | T19 | not started |  |
| pullRequestDetail.logic.ts | 1,449 | KEEP | T19 | not started |  |
| pullRequestDiff.logic.ts | 64 | KEEP | T19 | not started |  |
| pullRequestEditing.logic.ts | 61 | KEEP | T19 | not started |  |
| pullRequestFileOrder.logic.ts | 197 | KEEP | T19 | not started |  |
| pullRequestFilesViewed.logic.ts | 100 | KEEP | T19 | not started |  |
| pullRequestIcons.tsx | 54 | KEEP | T19 | not started |  |
| pullRequestLinkContextMenu.ts | 65 | KEEP | T19 | not started |  |
| pullRequestList.logic.ts | 1,306 | KEEP | T19 | not started |  |
| pullRequestListLines.ts | 49 | KEEP | T19 | not started |  |
| pullRequestListPreferences.ts | 123 | KEEP | T19 | not started |  |
| pullRequestMarkdown.logic.ts | 53 | KEEP | T19 | not started |  |
| pullRequestPresentation.tsx | 595 | KEEP | T19 | partial | `T15-sidebar:app/src/components/sidebar/thread_status_indicators.rs` |
| pullRequestProjectAssignment.logic.ts | 137 | KEEP | T19 | not started |  |
| pullRequestProjectFilter.logic.ts | 53 | KEEP | T19 | not started |  |
| pullRequestReactions.logic.ts | 108 | KEEP | T19 | not started |  |
| pullRequestReviewStore.ts | 95 | KEEP | T19 | not started |  |
| pullRequestStackSnapshot.ts | 77 | KEEP | T19 | not started |  |
| pullRequestSummaryScroll.logic.ts | 20 | KEEP | T19 | not started |  |
| useInlineActionConfirm.ts | 55 | KEEP | T19 | not started |  |
| usePullRequestActions.ts | 471 | KEEP | T19 | not started |  |
| usePullRequestFilesViewed.ts | 211 | KEEP | T19 | not started |  |

### `apps/web/src/components/pullRequest/guide/`

11 files, 1,493 lines: KEEP 11 / 1,493, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| GuideExcerpt.tsx | 362 | KEEP | T19 | not started |  |
| GuideFileDiff.tsx | 50 | KEEP | T19 | not started |  |
| GuideNotes.tsx | 58 | KEEP | T19 | not started |  |
| GuideProse.tsx | 40 | KEEP | T19 | not started |  |
| GuideRail.tsx | 108 | KEEP | T19 | not started |  |
| GuideStop.tsx | 310 | KEEP | T19 | not started |  |
| GuideTour.tsx | 174 | KEEP | T19 | not started |  |
| PullRequestGuideRunCoordinator.tsx | 140 | KEEP | T19 | not started |  |
| guideRunStore.ts | 86 | KEEP | T19 | not started |  |
| useGuideDiffFiles.ts | 87 | KEEP | T19 | not started |  |
| useReviewGuideWriter.ts | 78 | KEEP | T19 | not started |  |

### `apps/web/src/components/search/`

2 files, 520 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 2 / 520, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| HighlightedSearchLine.tsx | 183 | **UNASSIGNED** | N11 Project content search |  |  |
| ProjectContentSearchDialog.tsx | 337 | **UNASSIGNED** | N11 Project content search |  |  |

### `apps/web/src/components/settings/`

121 files, 45,455 lines: KEEP 86 / 29,546, CUT 35 / 15,909, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AcpRegistryIcon.tsx | 170 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AcpRegistrySearchStep.tsx | 296 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AcpSessionManagementSection.tsx | 588 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AddCodexAccountDialog.tsx | 151 | KEEP | T20 | not started |  |
| AddProviderInstanceDialog.logic.ts | 134 | KEEP | T20 | not started |  |
| AddProviderInstanceDialog.tsx | 714 | KEEP | T20 | not started |  |
| AddProviderInstanceWizardSteps.tsx | 49 | KEEP | T20 | not started |  |
| AddUsageLimitSourceDialog.tsx | 156 | KEEP | T20 | not started |  |
| BranchNamingSettings.tsx | 161 | KEEP | T20 | not started |  |
| BrowserImportWizard.tsx | 607 | CUT | Browser / preview / devices |  |  |
| CaptureShortcutConfig.tsx | 346 | CUT | SnapShot screen capture (desktop feature) |  |  |
| ChatGptAccountPicker.tsx | 74 | KEEP | T20 | not started |  |
| ChatGptConnectionButton.tsx | 12 | KEEP | T20 | not started |  |
| ChatGptUsageButton.tsx | 19 | KEEP | T20 | not started |  |
| ChatGptWelcomeCoordinator.tsx | 83 | KEEP | T20 | not started |  |
| CliCommandSettingsRow.tsx | 78 | KEEP | T20 | not started |  |
| CodexSetupSection.logic.ts | 8 | KEEP | T20 | not started |  |
| CodexSetupSection.tsx | 1,148 | KEEP | T20 | not started |  |
| ConnectionsSettings.logic.ts | 109 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ConnectionsSettings.tsx | 4,186 | CUT | T3 Connect / relay / remote access / auth |  |  |
| CustomModelEditor.tsx | 389 | KEEP | T20 | not started |  |
| DeviceHostEditor.tsx | 224 | CUT | Browser / preview / devices |  |  |
| DeviceHostsSettings.tsx | 340 | CUT | Browser / preview / devices |  |  |
| DiagnosticsSettings.tsx | 1,357 | CUT | Telemetry / observability / resource telemetry |  |  |
| EnvironmentIconPicker.tsx | 108 | CUT | T3 Connect / relay / remote access / auth |  |  |
| EnvironmentRoutesList.tsx | 222 | CUT | T3 Connect / relay / remote access / auth |  |  |
| EnvironmentRow.tsx | 100 | CUT | T3 Connect / relay / remote access / auth |  |  |
| ExpandableText.tsx | 49 | KEEP | T20 | not started |  |
| FoldedSettingsSection.tsx | 98 | KEEP | T20 | not started |  |
| FontFamilyPicker.tsx | 244 | KEEP | T20 | not started |  |
| GitHubAccountSettings.logic.ts | 53 | KEEP | T20 | not started |  |
| GitHubAccountSettings.tsx | 230 | KEEP | T20 | not started |  |
| GitHubRoutingSettings.tsx | 127 | KEEP | T20 | not started |  |
| GitHubTokenSettings.tsx | 141 | KEEP | T20 | not started |  |
| IntegrationsSettings.tsx | 1,552 | CUT | Browser / preview / devices |  |  |
| KeybindingsSettings.logic.ts | 441 | KEEP | T20 | not started |  |
| KeybindingsSettings.tsx | 1,541 | KEEP | T20 | not started |  |
| LoadBalancingSettings.tsx | 134 | CUT | T3 Connect / relay / remote access / auth |  |  |
| LocalEnvironmentSetting.tsx | 103 | CUT | T3 Connect / relay / remote access / auth |  |  |
| NotificationSettings.tsx | 83 | KEEP | T20 | not started |  |
| OpenSourceLicenses.tsx | 294 | CUT | Open-source licenses page (web build artifact) |  |  |
| PanelAnimationsPreview.tsx | 52 | KEEP | T20 | not started |  |
| ProcessSignalActions.tsx | 48 | CUT | Telemetry / observability / resource telemetry |  |  |
| ProjectActionsList.tsx | 75 | KEEP | T20 | not started |  |
| ProjectActionsSettings.tsx | 235 | KEEP | T20 | not started |  |
| ProjectDefaultsSettings.tsx | 537 | KEEP | T20 | not started |  |
| ProjectFaviconPickerDialog.tsx | 143 | KEEP | T20 | not started |  |
| ProjectIconPickerDialog.tsx | 254 | KEEP | T20 | not started |  |
| ProjectSettingsPanel.logic.ts | 7 | KEEP | T20 | not started |  |
| ProjectSettingsPanel.tsx | 603 | KEEP | T20 | not started |  |
| ProjectsSettings.tsx | 36 | KEEP | T20 | not started |  |
| ProviderAccentColorPicker.tsx | 217 | KEEP | T20 | not started |  |
| ProviderAuthCallbackCoordinator.tsx | 50 | KEEP | T20 | not started |  |
| ProviderAuthTerminal.tsx | 80 | CUT | Terminal / PTY |  |  |
| ProviderAuthenticationSection.tsx | 475 | KEEP | T20 | not started |  |
| ProviderInstanceCard.tsx | 1,349 | KEEP | T20 | not started |  |
| ProviderModelsSection.tsx | 653 | KEEP | T20 | not started |  |
| ProviderSettingsForm.tsx | 587 | KEEP | T20 | not started |  |
| ProviderSettingsPanel.logic.ts | 171 | KEEP | T20 | not started |  |
| ProviderSettingsPanel.tsx | 1,339 | KEEP | T20 | not started |  |
| ProviderSetupSection.tsx | 328 | KEEP | T20 | not started |  |
| ProviderWizardAuthenticationStep.tsx | 101 | KEEP | T20 | not started |  |
| RedactedSensitiveText.tsx | 61 | KEEP | T20 | not started |  |
| ResourceTelemetryDiagnostics.logic.ts | 60 | CUT | Telemetry / observability / resource telemetry |  |  |
| ResourceTelemetryDiagnostics.tsx | 1,294 | CUT | Telemetry / observability / resource telemetry |  |  |
| ScheduledTasksSettings.tsx | 1,387 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| ScopedSwitch.tsx | 19 | KEEP | T20 | not started |  |
| SecretInput.tsx | 39 | KEEP | T20 | not started |  |
| SessionPermissions.tsx | 38 | CUT | T3 Connect / relay / remote access / auth |  |  |
| SettingInheritance.tsx | 318 | KEEP | T20 | not started |  |
| SettingsBreadcrumb.tsx | 39 | KEEP | T20 | not started |  |
| SettingsFontPreviews.tsx | 273 | KEEP | T20 | not started |  |
| SettingsGroup.tsx | 30 | KEEP | T20 | not started |  |
| SettingsPanels.logic.ts | 328 | KEEP | T20 | not started |  |
| SettingsPanels.tsx | 3,662 | KEEP | T20 | not started |  |
| SettingsScopeContext.tsx | 137 | KEEP | T20 | not started |  |
| SettingsScopeNotice.tsx | 90 | KEEP | T20 | not started |  |
| SettingsScopeSentence.tsx | 225 | KEEP | T20 | not started |  |
| SettingsSidebarNav.tsx | 381 | KEEP | T20 | not started |  |
| SnapShotSettings.logic.ts | 142 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotSettings.tsx | 600 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotSetupDialog.logic.ts | 98 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotSetupDialog.tsx | 476 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SourceControlHostSettings.tsx | 84 | KEEP | T20 | not started |  |
| SourceControlSettings.tsx | 647 | KEEP | T20 | not started |  |
| SourceControlWritingSettings.tsx | 607 | KEEP | T20 | not started |  |
| StorageSettings.tsx | 647 | KEEP | T20 | not started |  |
| TelemetryExportSettings.tsx | 243 | CUT | Telemetry / observability / resource telemetry |  |  |
| ThemeColorPicker.tsx | 369 | KEEP | T20 | not started |  |
| ThemeEditorHost.tsx | 131 | KEEP | T20 | not started |  |
| ThemeEditorPanel.tsx | 1,313 | KEEP | T20 | not started |  |
| ThemeImportDialog.tsx | 561 | KEEP | T20 | not started |  |
| ThemePreviewCircles.tsx | 230 | KEEP | T20 | not started |  |
| ThemeSearchSection.tsx | 457 | KEEP | T20 | not started |  |
| ThemeSettings.tsx | 1,045 | KEEP | T20 | not started |  |
| ThemeWireframe.tsx | 136 | KEEP | T20 | not started |  |
| UsageProviderSettings.tsx | 174 | KEEP | T20 | not started |  |
| browserImportWizard.logic.ts | 174 | CUT | Browser / preview / devices |  |  |
| customModelEditor.logic.ts | 274 | KEEP | T20 | not started |  |
| deviceHostConnectionChecks.ts | 69 | CUT | Browser / preview / devices |  |  |
| deviceHostsSettings.logic.ts | 35 | CUT | Browser / preview / devices |  |  |
| itemRows.ts | 5 | KEEP | T20 | not started |  |
| pairingUrls.ts | 20 | CUT | T3 Connect / relay / remote access / auth |  |  |
| providerDriverMeta.ts | 38 | KEEP | T20 | not started |  |
| providerStatus.ts | 172 | KEEP | T20 | not started |  |
| scheduledTasksSettings.logic.ts | 206 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| scopedSettings.ts | 504 | KEEP | T20 | not started |  |
| settingsLayout.tsx | 594 | KEEP | T20 | not started |  |
| settingsScope.ts | 156 | KEEP | T20 | not started |  |
| settingsScopeAxis.ts | 77 | KEEP | T20 | not started |  |
| settingsScopeNavigation.ts | 32 | KEEP | T20 | not started |  |
| settingsSearch.ts | 1,202 | KEEP | T20 | not started |  |
| themeEditorStore.ts | 68 | KEEP | T20 | not started |  |
| themeInspector.ts | 501 | KEEP | T20 | not started |  |
| useAvailableSettingsSearchItems.ts | 76 | KEEP | T20 | not started |  |
| useHostConnectionChecks.ts | 51 | CUT | Browser / preview / devices |  |  |
| useProjectScriptSettings.ts | 206 | KEEP | T20 | not started |  |
| useScopedModelAvailability.ts | 57 | KEEP | T20 | not started |  |
| useScopedSettings.ts | 162 | KEEP | T20 | not started |  |
| useSettingsProjectGroups.ts | 24 | KEEP | T20 | not started |  |
| useSnapShotShortcutRecorder.tsx | 152 | CUT | SnapShot screen capture (desktop feature) |  |  |

### `apps/web/src/components/sidebar/`

12 files, 2,412 lines: KEEP 4 / 519, CUT 7 / 1,683, UNASSIGNED 1 / 210, ported 2 / 456.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopUpdateStatusIcon.tsx | 130 | CUT | Auto-updates (app / server) |  |  |
| SidebarChrome.tsx | 252 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_chrome.rs` |
| SidebarProviderUpdatePill.tsx | 210 | **UNASSIGNED** | N8 Provider maintenance and update UI |  |  |
| SidebarThreadHeader.tsx | 204 | KEEP | T15 | ported | `T15-sidebar:app/src/components/sidebar/sidebar_thread_header.rs` |
| SidebarThreadUndoNotice.tsx | 27 | KEEP | T15 | not started |  |
| SidebarUpdatePill.tsx | 573 | CUT | Auto-updates (app / server) |  |  |
| SidebarUpdateReleaseNotes.tsx | 120 | CUT | Auto-updates (app / server) |  |  |
| UpdateEverythingDialog.tsx | 126 | CUT | Auto-updates (app / server) |  |  |
| mainAppLocation.ts | 36 | KEEP | T15 | not started |  |
| updateEverything.logic.ts | 382 | CUT | Auto-updates (app / server) |  |  |
| updateEverythingRun.tsx | 142 | CUT | Auto-updates (app / server) |  |  |
| useUpdateEverything.ts | 210 | CUT | Auto-updates (app / server) |  |  |

### `apps/web/src/components/ui/`

51 files, 6,608 lines: KEEP 50 / 6,527, CUT 1 / 81, UNASSIGNED 0 / 0, ported 11 / 1,977.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| alert-dialog.tsx | 147 | KEEP | T03 | not started |  |
| alert.tsx | 150 | KEEP | T03 | not started |  |
| anchoredCopyToast.ts | 33 | KEEP | T03 | not started |  |
| autocomplete.tsx | 257 | KEEP | T03 | not started |  |
| badge.tsx | 64 | KEEP | T03 | ported | `main:app/src/ui/badge.rs` |
| button.tsx | 137 | KEEP | T03 | ported | `main:app/src/ui/button.rs` |
| calendar.tsx | 112 | KEEP | T03 | not started |  |
| checkbox.tsx | 62 | KEEP | T03 | not started |  |
| collapsible-section-header.tsx | 52 | KEEP | T03 | ported | `T15-sidebar:app/src/ui/collapsible_section_header.rs` |
| collapsible.tsx | 47 | KEEP | T03 | not started |  |
| color-picker.tsx | 225 | KEEP | T03 | not started |  |
| combobox.tsx | 285 | KEEP | T03 | not started |  |
| command.tsx | 261 | KEEP | T03 | not started |  |
| dialog-styles.ts | 22 | KEEP | T03 | not started |  |
| dialog.tsx | 197 | KEEP | T03 | not started |  |
| discovery-list.tsx | 37 | KEEP | T03 | not started |  |
| draft-input.tsx | 21 | KEEP | T03 | not started |  |
| empty.tsx | 124 | KEEP | T03 | not started |  |
| group.tsx | 73 | KEEP | T03 | not started |  |
| input-group.tsx | 123 | KEEP | T03 | not started |  |
| input.tsx | 84 | KEEP | T03 | not started |  |
| kbd.tsx | 28 | KEEP | T03 | ported | `main:app/src/ui/kbd.rs` |
| label.tsx | 24 | KEEP | T03 | not started |  |
| menu.tsx | 358 | KEEP | T03 | ported | `main:app/src/ui/menu.rs` |
| middle-truncate.tsx | 81 | KEEP | T03 | not started |  |
| number-field.tsx | 97 | KEEP | T03 | not started |  |
| panel-tab-close-button.tsx | 41 | KEEP | T03 | not started |  |
| popover.tsx | 149 | KEEP | T03 | ported | `main:app/src/ui/popover.rs` |
| preview-card.tsx | 44 | KEEP | T03 | not started |  |
| qr-code.tsx | 81 | CUT | QR pairing (T3 Connect) |  |  |
| radio-group.tsx | 37 | KEEP | T03 | not started |  |
| refresh-icon.tsx | 39 | KEEP | T03 | not started |  |
| scroll-area.tsx | 108 | KEEP | T03 | ported | `main:app/src/ui/scroll_area.rs` |
| select.tsx | 222 | KEEP | T03 | not started |  |
| separator.tsx | 19 | KEEP | T03 | ported | `main:app/src/ui/separator.rs` |
| sheet.tsx | 168 | KEEP | T03 | not started |  |
| sidebar.tsx | 850 | KEEP | T03 | ported | `T15-sidebar:app/src/ui/sidebar.rs` |
| sidebarState.ts | 9 | KEEP | T03 | partial | `T15-sidebar:app/src/ui/sidebar.rs` |
| skeleton.tsx | 28 | KEEP | T03 | not started |  |
| spinner.tsx | 40 | KEEP | T03 | not started |  |
| standalone-page.tsx | 67 | KEEP | T03 | not started |  |
| switch.tsx | 48 | KEEP | T03 | not started |  |
| table.tsx | 67 | KEEP | T03 | not started |  |
| textarea.tsx | 46 | KEEP | T03 | not started |  |
| toast.logic.ts | 131 | KEEP | T03 | not started |  |
| toast.tsx | 800 | KEEP | T03 | not started |  |
| toastHelpers.ts | 66 | KEEP | T03 | not started |  |
| toggle-group.tsx | 78 | KEEP | T03 | not started |  |
| toggle.tsx | 56 | KEEP | T03 | ported | `main:app/src/ui/toggle.rs` |
| tooltip.tsx | 156 | KEEP | T03 | ported | `main:app/src/ui/tooltip.rs` |
| wizard.tsx | 157 | KEEP | T03 | not started |  |

### `apps/web/src/components/usage/`

14 files, 4,424 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 14 / 4,424, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| UsageLimits.tsx | 392 | **UNASSIGNED** | N4 Usage page |  |  |
| UsageLimitsPooled.tsx | 653 | **UNASSIGNED** | N4 Usage page |  |  |
| UsageModelDialog.tsx | 166 | **UNASSIGNED** | N4 Usage page |  |  |
| UsagePage.tsx | 1,508 | **UNASSIGNED** | N4 Usage page |  |  |
| UsagePriceOverrides.tsx | 628 | **UNASSIGNED** | N4 Usage page |  |  |
| UsageProviderChart.tsx | 498 | **UNASSIGNED** | N4 Usage page |  |  |
| UsageShareBar.tsx | 73 | **UNASSIGNED** | N4 Usage page |  |  |
| usageBreakdown.ts | 86 | **UNASSIGNED** | N4 Usage page |  |  |
| usagePagePreferences.ts | 35 | **UNASSIGNED** | N4 Usage page |  |  |
| usagePriceForm.ts | 62 | **UNASSIGNED** | N4 Usage page |  |  |
| usagePriceTable.ts | 135 | **UNASSIGNED** | N4 Usage page |  |  |
| usagePriceTargets.ts | 81 | **UNASSIGNED** | N4 Usage page |  |  |
| usageProviders.ts | 68 | **UNASSIGNED** | N4 Usage page |  |  |
| usageShortcuts.ts | 39 | **UNASSIGNED** | N4 Usage page |  |  |

### `apps/web/src/connection/`

8 files, 2,061 lines: KEEP 0 / 0, CUT 8 / 2,061, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| catalog.ts | 5 | CUT | T3 Connect / relay / remote access / auth |  |  |
| clientMetadata.ts | 97 | CUT | T3 Connect / relay / remote access / auth |  |  |
| desktopLocal.ts | 125 | CUT | T3 Connect / relay / remote access / auth |  |  |
| onboarding.ts | 37 | CUT | T3 Connect / relay / remote access / auth |  |  |
| platform.ts | 791 | CUT | T3 Connect / relay / remote access / auth |  |  |
| runtime.ts | 56 | CUT | T3 Connect / relay / remote access / auth |  |  |
| storage.ts | 922 | CUT | T3 Connect / relay / remote access / auth |  |  |
| useDesktopLocalBootstraps.ts | 28 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/environments/primary/`

8 files, 1,090 lines: KEEP 0 / 0, CUT 8 / 1,090, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| auth.ts | 488 | CUT | T3 Connect / relay / remote access / auth |  |  |
| context.ts | 101 | CUT | T3 Connect / relay / remote access / auth |  |  |
| desktopAuth.ts | 21 | CUT | T3 Connect / relay / remote access / auth |  |  |
| httpClient.ts | 25 | CUT | T3 Connect / relay / remote access / auth |  |  |
| httpLayer.ts | 57 | CUT | T3 Connect / relay / remote access / auth |  |  |
| index.ts | 39 | CUT | T3 Connect / relay / remote access / auth |  |  |
| sessionState.ts | 42 | CUT | T3 Connect / relay / remote access / auth |  |  |
| target.ts | 317 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/web/src/hooks/`

41 files, 6,120 lines: KEEP 25 / 4,646, CUT 9 / 601, UNASSIGNED 7 / 873, ported 2 / 225.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| FileMetadataThreadProvider.tsx | 64 | KEEP | T16 | not started |  |
| showThreadUndoNotice.ts | 113 | KEEP | T15 | not started |  |
| threadUndo.ts | 38 | KEEP | T15 | not started |  |
| useActiveProjectTarget.ts | 41 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| useActiveThreadRef.ts | 20 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| useCommitOnBlur.ts | 44 | KEEP | T03 | not started |  |
| useCopyToClipboard.ts | 262 | KEEP | T16 | not started |  |
| useCustomThemes.ts | 9 | KEEP | T20 | not started |  |
| useDefaultTheme.ts | 115 | KEEP | T20 | not started |  |
| useDelayedStatus.ts | 18 | KEEP | T14 | not started |  |
| useEnvironmentDisconnectDelay.ts | 25 | CUT | T3 Connect / relay / remote access / auth |  |  |
| useEnvironmentTheme.ts | 125 | KEEP | T20 | not started |  |
| useFileMetadata.ts | 34 | KEEP | T16 | not started |  |
| useHandleNewThread.ts | 498 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| useHtmlRenderTheme.ts | 56 | CUT | Browser / preview / devices |  |  |
| useLiveRefresh.ts | 194 | KEEP | T19 | not started |  |
| useLoadBalancedEnvironment.ts | 54 | CUT | T3 Connect / relay / remote access / auth |  |  |
| useLocalStorage.ts | 189 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| useMediaQuery.ts | 87 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| useNavigateBack.ts | 40 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| useNewProject.ts | 151 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| useNowMinute.ts | 66 | KEEP | T15 | not started |  |
| useOpenPanelPullRequestUrl.ts | 76 | KEEP | T19 | not started |  |
| usePreviewPanelInlineSize.ts | 98 | KEEP | T03 | ported | `main:app/src/hooks/use_preview_panel_inline_size.rs` |
| usePullRequestChecksRefresh.ts | 20 | KEEP | T19 | not started |  |
| usePullRequestLinking.ts | 117 | KEEP | T19 | not started |  |
| useRemoveClonedProject.ts | 64 | CUT | Project clone |  |  |
| useResizableWidth.ts | 127 | KEEP | T03 | ported | `main:app/src/hooks/use_resizable_width.rs` |
| useResizeDrag.ts | 128 | KEEP | T03 | not started |  |
| useScratchProject.ts | 93 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| useSettings.ts | 816 | KEEP | T20 | not started |  |
| useSidebarProjectGroups.ts | 49 | KEEP | T15 | not started |  |
| useSupportsMultiplePullRequests.ts | 10 | KEEP | T19 | not started |  |
| useT3ProjectFileScripts.ts | 65 | CUT | Project scripts run in terminal; t3.json is import-only (hooks replace) |  |  |
| useTerminalFocus.ts | 16 | CUT | Terminal / PTY |  |  |
| useTheme.ts | 679 | KEEP | T20 | not started |  |
| useThreadActionMenu.ts | 377 | KEEP | T15 | not started |  |
| useThreadActions.ts | 1,046 | KEEP | T15 | not started |  |
| useThreadVisitedMigration.ts | 45 | CUT | Legacy v1 importer |  |  |
| useTurnDiffSummaries.ts | 21 | KEEP | T18 | not started |  |
| useWorkspaceMutationRefresh.ts | 30 | **UNASSIGNED** | N6 Git UI and review backend |  |  |

### `apps/web/src/lib/`

61 files, 6,720 lines: KEEP 33 / 4,973, CUT 27 / 1,686, UNASSIGNED 1 / 61, ported 3 / 34.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| archivedThreadsState.ts | 50 | KEEP | T20 | not started |  |
| assistantCitationNavigation.ts | 47 | KEEP | T16 | not started |  |
| assistantTextSelection.ts | 290 | KEEP | T16 | not started |  |
| attachmentUploadQueue.ts | 675 | KEEP | T17 | not started |  |
| attachmentUploadState.ts | 61 | KEEP | T17 | not started |  |
| backgroundActivityReporter.ts | 255 | CUT | Telemetry / observability / resource telemetry |  |  |
| baseRefChoices.ts | 61 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| bootError.ts | 27 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| chatThreadActions.ts | 102 | KEEP | T16 | not started |  |
| checkpointDiffState.ts | 18 | KEEP | T18 | not started |  |
| chunkReloadGuard.ts | 39 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| color.ts | 62 | KEEP | T03 | partial | `main:app/src/theme/color.rs (name match, no header)` |
| composerContextRecords.ts | 531 | KEEP | T17 | not started |  |
| composerContextReferences.ts | 173 | KEEP | T17 | not started |  |
| composerDraftUploads.ts | 40 | KEEP | T17 | not started |  |
| composerPathSearchState.ts | 22 | KEEP | T17 | not started |  |
| contextWindow.ts | 179 | KEEP | T16 | not started |  |
| desktopPasteAsText.ts | 15 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| desktopSnapShot.ts | 44 | CUT | SnapShot screen capture (desktop feature) |  |  |
| diffCollapse.ts | 13 | KEEP | T18 | not started |  |
| diffFileContents.ts | 124 | KEEP | T18 | not started |  |
| diffRendering.ts | 378 | KEEP | T18 | partial | `T18-diff:app/src/components/chat/markdown/code_highlight.rs` |
| discardComposerDraft.ts | 79 | KEEP | T17 | not started |  |
| editableFocus.ts | 21 | KEEP | T17 | not started |  |
| elementContext.ts | 101 | CUT | Browser / preview / devices |  |  |
| embeddedScripts.ts | 544 | KEEP | T16 | not started |  |
| favicon.ts | 1 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| formatBytes.ts | 11 | KEEP | T03 | not started |  |
| imageCompression.ts | 558 | KEEP | T17 | not started |  |
| incrementalHighlighting.ts | 74 | KEEP | T16 | not started |  |
| lruCache.ts | 68 | KEEP | T03 | not started |  |
| observeResize.ts | 107 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| openPullRequestLink.ts | 283 | KEEP | T19 | not started |  |
| orchestrationV2Timeline.ts | 22 | KEEP | T16 | partial | `T16-thread-view:app/src/components/chat/timeline/orchestration_v2_timeline.rs (name match, no header)` |
| previewAnnotation.ts | 30 | CUT | Browser / preview / devices |  |  |
| previewFocus.ts | 15 | CUT | Browser / preview / devices |  |  |
| projectPaths.ts | 17 | KEEP | T16 | ported | re-export barrel (no module needed) |
| projectScriptKeybindings.ts | 66 | CUT | Project scripts run in terminal; t3.json is import-only (hooks replace) |  |  |
| resourceTelemetryState.ts | 77 | CUT | Telemetry / observability / resource telemetry |  |  |
| revealThreadTerminal.ts | 21 | CUT | Terminal / PTY |  |  |
| runtime.ts | 76 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| selectionActions.ts | 189 | KEEP | T16 | not started |  |
| snapShotAnimation.ts | 179 | CUT | SnapShot screen capture (desktop feature) |  |  |
| snapShotSetupResume.ts | 36 | CUT | SnapShot screen capture (desktop feature) |  |  |
| snapShotShortcut.ts | 121 | CUT | SnapShot screen capture (desktop feature) |  |  |
| snapShotSound.ts | 11 | CUT | SnapShot screen capture (desktop feature) |  |  |
| snapShotSource.ts | 40 | CUT | SnapShot screen capture (desktop feature) |  |  |
| sourceControlActions.ts | 10 | KEEP | T19 | ported | re-export barrel (no module needed) |
| storage.ts | 74 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| syntaxHighlighting.ts | 39 | KEEP | T16 | not started |  |
| t3ProjectFileDefaults.ts | 38 | CUT | Project scripts run in terminal; t3.json is import-only (hooks replace) |  |  |
| terminalCloseConfirm.ts | 42 | CUT | Terminal / PTY |  |  |
| terminalCloseShortcut.ts | 36 | CUT | Terminal / PTY |  |  |
| terminalContext.ts | 102 | CUT | Terminal / PTY |  |  |
| terminalFocus.ts | 14 | CUT | Terminal / PTY |  |  |
| threadSort.ts | 7 | KEEP | T15 | ported | re-export barrel (no module needed) |
| turnDiffTree.ts | 172 | KEEP | T18 | partial | `T18-diff:app/src/components/chat/changed_files_tree.rs` |
| utils.ts | 50 | KEEP | T03 | not started |  |
| videoFirstFrame.ts | 28 | CUT | Browser / preview / devices |  |  |
| visibleAnimation.ts | 64 | KEEP | T03 | not started |  |
| windowControlsOverlay.ts | 91 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |

### `apps/web/src/observability/`

2 files, 153 lines: KEEP 0 / 0, CUT 2 / 153, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| clientTracer.ts | 29 | CUT | Telemetry / observability / resource telemetry |  |  |
| clientTracing.ts | 124 | CUT | Telemetry / observability / resource telemetry |  |  |

### `apps/web/src/onboarding/`

6 files, 599 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 6 / 599, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| firstRun.logic.ts | 187 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| firstRun.ts | 16 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| projectImport.logic.ts | 161 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| providerReadiness.logic.ts | 127 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| targetEnvironment.logic.ts | 77 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| useProjectScans.ts | 31 | **UNASSIGNED** | N2 Projects and onboarding |  |  |

### `apps/web/src/routes/`

28 files, 4,419 lines: KEEP 10 / 3,097, CUT 9 / 131, UNASSIGNED 9 / 1,191, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| __root.tsx | 628 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| _chat.$environmentId.$threadId.tsx | 7 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| _chat.draft.$draftId.tsx | 7 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| _chat.index.tsx | 179 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| _chat.new.tsx | 30 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| _chat.pull-requests.tsx | 2,800 | KEEP | T19 | not started |  |
| _chat.tsx | 246 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| connect-agent.tsx | 7 | CUT | T3 Connect / relay / remote access / auth |  |  |
| connect.tsx | 13 | CUT | T3 Connect / relay / remote access / auth |  |  |
| pair.tsx | 58 | CUT | T3 Connect / relay / remote access / auth |  |  |
| projects.$projectKey.tsx | 17 | **UNASSIGNED** | N7b App shell, routing and command palette |  |  |
| settings.appearance.tsx | 11 | KEEP | T20 | not started |  |
| settings.archived.tsx | 7 | KEEP | T20 | not started |  |
| settings.connections.tsx | 7 | CUT | T3 Connect / relay / remote access / auth |  |  |
| settings.diagnostics.tsx | 7 | CUT | Telemetry / observability / resource telemetry |  |  |
| settings.general.tsx | 11 | KEEP | T20 | not started |  |
| settings.integrations.tsx | 7 | CUT | Browser / preview / devices |  |  |
| settings.keybindings.tsx | 7 | KEEP | T20 | not started |  |
| settings.open-source-licenses.tsx | 7 | CUT | Open-source licenses page (web build artifact) |  |  |
| settings.projects.tsx | 6 | KEEP | T20 | not started |  |
| settings.providers.tsx | 50 | KEEP | T20 | not started |  |
| settings.scheduled-tasks.tsx | 14 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| settings.snap-shot.tsx | 11 | CUT | SnapShot screen capture (desktop feature) |  |  |
| settings.source-control.tsx | 7 | KEEP | T20 | not started |  |
| settings.storage.tsx | 4 | KEEP | T20 | not started |  |
| settings.tsx | 194 | KEEP | T20 | not started |  |
| usage.tsx | 15 | **UNASSIGNED** | N4 Usage page |  |  |
| welcome.tsx | 62 | **UNASSIGNED** | N2 Projects and onboarding |  |  |

### `apps/web/src/rpc/`

3 files, 168 lines: KEEP 0 / 0, CUT 3 / 168, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| atomRegistry.ts | 9 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| requestLatencyState.ts | 158 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| transportError.ts | 1 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |

### `apps/web/src/state/`

44 files, 3,721 lines: KEEP 15 / 1,413, CUT 23 / 1,537, UNASSIGNED 6 / 771, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| agentSessions.ts | 25 | **UNASSIGNED** | N2 Projects and onboarding |  |  |
| assets.ts | 36 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| attachments.ts | 5 | KEEP | T17 | not started |  |
| auth.ts | 5 | CUT | T3 Connect / relay / remote access / auth |  |  |
| desktopNetworkAccess.ts | 98 | CUT | T3 Connect / relay / remote access / auth |  |  |
| desktopSshHosts.ts | 75 | CUT | T3 Connect / relay / remote access / auth |  |  |
| desktopUpdate.ts | 86 | CUT | Auto-updates (app / server) |  |  |
| desktopWslState.ts | 60 | CUT | T3 Connect / relay / remote access / auth |  |  |
| device.ts | 81 | CUT | Browser / preview / devices |  |  |
| entities.ts | 320 | KEEP | T14 | partial | `T14-client-state:app/src/state/entities.rs (name match, no header)` |
| environments.ts | 111 | CUT | T3 Connect / relay / remote access / auth |  |  |
| filesystem.ts | 25 | KEEP | T17 | not started |  |
| git.ts | 5 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| mcpApps.ts | 5 | CUT | Browser / preview / devices |  |  |
| npmRelease.ts | 45 | CUT | Auto-updates (app / server) |  |  |
| orchestration.ts | 5 | KEEP | T14 | not started |  |
| paginatedBranches.ts | 36 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| presentation.ts | 39 | KEEP | T14 | not started |  |
| preview.ts | 5 | CUT | Browser / preview / devices |  |  |
| previewStream.ts | 51 | CUT | Browser / preview / devices |  |  |
| primaryEnvironment.ts | 12 | CUT | T3 Connect / relay / remote access / auth |  |  |
| projectClones.ts | 52 | CUT | Project clone |  |  |
| projects.ts | 29 | KEEP | T14 | partial | `T14-client-state:app/src/state/projects.rs (name match, no header)` |
| pullRequests.ts | 404 | KEEP | T19 | not started |  |
| queries.ts | 402 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| query.ts | 48 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| relay.ts | 6 | CUT | T3 Connect / relay / remote access / auth |  |  |
| review.ts | 5 | KEEP | T18 | not started |  |
| server.ts | 128 | KEEP | T14 | not started |  |
| session.ts | 103 | KEEP | T14 | not started |  |
| shell.ts | 114 | KEEP | T14 | not started |  |
| sourceControl.ts | 5 | KEEP | T19 | not started |  |
| sourceControlActions.ts | 413 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| terminal.ts | 5 | CUT | Terminal / PTY |  |  |
| terminalSessions.ts | 200 | CUT | Terminal / PTY |  |  |
| threads.ts | 136 | KEEP | T14 | not started |  |
| usage.ts | 283 | **UNASSIGNED** | N4 Usage page |  |  |
| use-atom-command.ts | 23 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| use-atom-query-runner.ts | 32 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| use-orchestration-command.ts | 43 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| usePullRequestStack.ts | 33 | KEEP | T19 | not started |  |
| v2ItemSupport.ts | 62 | KEEP | T16 | not started |  |
| vcs.ts | 9 | **UNASSIGNED** | N6 Git UI and review backend |  |  |
| waitForAtomValue.ts | 56 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |

### `apps/web/src/terminal/ghostty/`

5 files, 4,257 lines: KEEP 0 / 0, CUT 5 / 4,257, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| core.ts | 1,290 | CUT | Terminal / PTY |  |  |
| keyCodes.ts | 267 | CUT | Terminal / PTY |  |  |
| renderer.ts | 272 | CUT | Terminal / PTY |  |  |
| runtime.ts | 233 | CUT | Terminal / PTY |  |  |
| surface.ts | 2,195 | CUT | Terminal / PTY |  |  |

### `apps/web/src/vendor/`

1 files, 90 lines: KEEP 1 / 90, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| mdast-find-and-replace.ts | 90 | KEEP | T16 | not started |  |

### `apps/desktop/src/`

9 files, 965 lines: KEEP 0 / 0, CUT 9 / 965, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| boot.ts | 4 | CUT | Electron shell / desktop-only infra |  |  |
| compileCache.ts | 28 | CUT | Electron shell / desktop-only infra |  |  |
| legacyLocalStorageMerge.ts | 113 | CUT | Electron shell / desktop-only infra |  |  |
| linuxSecretStorage.ts | 79 | CUT | Electron shell / desktop-only infra |  |  |
| mac-permission-preload.ts | 17 | CUT | Browser / preview / devices |  |  |
| main.ts | 256 | CUT | Electron shell / desktop-only infra |  |  |
| preload.ts | 442 | CUT | Electron shell / desktop-only infra |  |  |
| preview-pick-preload.ts | 10 | CUT | Browser / preview / devices |  |  |
| preview-pip-preload.ts | 16 | CUT | Browser / preview / devices |  |  |

### `apps/desktop/src/app/`

25 files, 5,304 lines: KEEP 0 / 0, CUT 25 / 5,304, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| CodexAuthCallback.ts | 5 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopApp.ts | 371 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopAppActivation.ts | 407 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopAppActivationBroker.ts | 146 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopAppIdentity.ts | 128 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopAssets.ts | 143 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopClerk.ts | 241 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopCliCommand.ts | 280 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopCliShim.ts | 187 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopConfig.ts | 67 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopConnectionCatalogStore.ts | 518 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopEarlyElectronStartup.ts | 170 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopEnvironment.ts | 286 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopLegacyLocalStorage.ts | 117 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopLifecycle.ts | 267 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopLinuxUrlHandler.ts | 277 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopObservability.ts | 739 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopPreReadyFileSystem.ts | 65 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopPreReadyPlatform.ts | 148 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopShutdown.ts | 35 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopState.ts | 19 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopStatePaths.ts | 32 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopUserData.ts | 98 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopWebLinks.ts | 98 | CUT | Electron shell / desktop-only infra |  |  |
| chromiumLocalStorage.ts | 460 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/backend/`

7 files, 3,612 lines: KEEP 0 / 0, CUT 7 / 3,612, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopBackendConfiguration.ts | 996 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopBackendManager.ts | 1,221 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopBackendPool.ts | 485 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopLocalEnvironmentAuth.ts | 134 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopNetworkInterfaces.ts | 53 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopServerExposure.ts | 577 | CUT | Electron shell / desktop-only infra |  |  |
| tailscaleEndpointProvider.ts | 146 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/electron/`

13 files, 2,363 lines: KEEP 0 / 0, CUT 13 / 2,363, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ElectronApp.ts | 211 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronDialog.ts | 197 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronMenu.ts | 282 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronPowerMonitor.ts | 90 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronProtocol.ts | 307 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronSafeStorage.ts | 100 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronShell.ts | 91 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronTheme.ts | 55 | CUT | Electron shell / desktop-only infra |  |  |
| ElectronUpdater.ts | 172 | CUT | Auto-updates (app / server) |  |  |
| ElectronWindow.ts | 387 | CUT | Electron shell / desktop-only infra |  |  |
| WindowsForeground.ts | 224 | CUT | Electron shell / desktop-only infra |  |  |
| WindowsForegroundFocusThread.ts | 148 | CUT | Electron shell / desktop-only infra |  |  |
| WindowsForegroundFocusWorker.ts | 99 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/ipc/`

3 files, 570 lines: KEEP 0 / 0, CUT 3 / 570, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopIpc.ts | 278 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopIpcHandlers.ts | 167 | CUT | Electron shell / desktop-only infra |  |  |
| channels.ts | 125 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/ipc/methods/`

16 files, 1,960 lines: KEEP 0 / 0, CUT 15 / 1,889, UNASSIGNED 1 / 71, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| appActivation.ts | 27 | CUT | Electron shell / desktop-only infra |  |  |
| cliCommand.ts | 34 | CUT | Electron shell / desktop-only infra |  |  |
| clientSettings.ts | 31 | CUT | Electron shell / desktop-only infra |  |  |
| connectionCatalog.ts | 37 | CUT | Electron shell / desktop-only infra |  |  |
| legacyLocalStorage.ts | 30 | CUT | Electron shell / desktop-only infra |  |  |
| localEnvironment.ts | 30 | CUT | Electron shell / desktop-only infra |  |  |
| notificationBadge.ts | 71 | **UNASSIGNED** | N9 Native macOS shell |  |  |
| preview.ts | 469 | CUT | Electron shell / desktop-only infra |  |  |
| providerAuth.ts | 49 | CUT | Electron shell / desktop-only infra |  |  |
| serverExposure.ts | 69 | CUT | Electron shell / desktop-only infra |  |  |
| snapShot.ts | 239 | CUT | Electron shell / desktop-only infra |  |  |
| sshEnvironment.ts | 233 | CUT | Electron shell / desktop-only infra |  |  |
| updates.ts | 62 | CUT | Electron shell / desktop-only infra |  |  |
| webLinks.ts | 16 | CUT | Electron shell / desktop-only infra |  |  |
| window.ts | 441 | CUT | Electron shell / desktop-only infra |  |  |
| wsl.ts | 122 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/permissions/`

4 files, 394 lines: KEEP 0 / 0, CUT 4 / 394, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| MacPermission.ts | 15 | CUT | Browser / preview / devices |  |  |
| MacPermissionHelper.ts | 220 | CUT | Browser / preview / devices |  |  |
| MacPermissions.ts | 45 | CUT | Browser / preview / devices |  |  |
| MacSettingsWindow.ts | 114 | CUT | Browser / preview / devices |  |  |

### `apps/desktop/src/preview/`

17 files, 8,066 lines: KEEP 0 / 0, CUT 17 / 8,066, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AnnotationKeyboard.ts | 17 | CUT | Browser / preview / devices |  |  |
| BrowserSession.ts | 380 | CUT | Browser / preview / devices |  |  |
| CdpRelay.ts | 224 | CUT | Browser / preview / devices |  |  |
| DesktopBrowserHost.ts | 245 | CUT | Browser / preview / devices |  |  |
| FaviconCapture.ts | 685 | CUT | Browser / preview / devices |  |  |
| GuestProtocol.ts | 17 | CUT | Browser / preview / devices |  |  |
| Manager.ts | 3,828 | CUT | Browser / preview / devices |  |  |
| PasskeyAttestation.ts | 59 | CUT | Browser / preview / devices |  |  |
| PasskeyBackend.ts | 26 | CUT | Browser / preview / devices |  |  |
| PasskeyBackendMac.ts | 96 | CUT | Browser / preview / devices |  |  |
| PasskeyBridge.ts | 286 | CUT | Browser / preview / devices |  |  |
| Passkeys.ts | 242 | CUT | Browser / preview / devices |  |  |
| PickPreload.ts | 1,518 | CUT | Browser / preview / devices |  |  |
| PickedElementPayload.ts | 146 | CUT | Browser / preview / devices |  |  |
| RecordingCursor.ts | 197 | CUT | Browser / preview / devices |  |  |
| RecordingInput.ts | 58 | CUT | Browser / preview / devices |  |  |
| WebviewPreferences.ts | 42 | CUT | Browser / preview / devices |  |  |

### `apps/desktop/src/preview/BrowserImport/`

9 files, 2,572 lines: KEEP 0 / 0, CUT 9 / 2,572, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| BrowserImport.ts | 337 | CUT | Browser / preview / devices |  |  |
| ChromiumCookies.ts | 370 | CUT | Browser / preview / devices |  |  |
| ChromiumKeys.ts | 336 | CUT | Browser / preview / devices |  |  |
| CookieDatabase.ts | 100 | CUT | Browser / preview / devices |  |  |
| FirefoxCookies.ts | 168 | CUT | Browser / preview / devices |  |  |
| LinuxBrowserSecret.ts | 40 | CUT | Browser / preview / devices |  |  |
| SafariCookies.ts | 273 | CUT | Browser / preview / devices |  |  |
| SafariPermission.ts | 31 | CUT | Browser / preview / devices |  |  |
| Sources.ts | 917 | CUT | Browser / preview / devices |  |  |

### `apps/desktop/src/settings/`

3 files, 1,154 lines: KEEP 0 / 0, CUT 2 / 920, UNASSIGNED 1 / 234, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopAppSettings.ts | 630 | CUT | Electron shell / desktop-only infra |  |  |
| DesktopClientSettings.ts | 234 | **UNASSIGNED** | N9 Native macOS shell |  |  |
| DesktopSavedEnvironments.ts | 290 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/shell/`

1 files, 545 lines: KEEP 0 / 0, CUT 1 / 545, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopShellEnvironment.ts | 545 | CUT | Electron shell / desktop-only infra |  |  |

### `apps/desktop/src/snapShot/`

27 files, 6,725 lines: KEEP 0 / 0, CUT 27 / 6,725, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ActiveWindow.ts | 143 | CUT | SnapShot screen capture (desktop feature) |  |  |
| CaptureShortcutConfig.ts | 304 | CUT | SnapShot screen capture (desktop feature) |  |  |
| DesktopSnapShot.ts | 1,706 | CUT | SnapShot screen capture (desktop feature) |  |  |
| GlobalShiftShortcutProcess.ts | 61 | CUT | SnapShot screen capture (desktop feature) |  |  |
| GlobalShiftShortcutWorker.ts | 39 | CUT | SnapShot screen capture (desktop feature) |  |  |
| GnomeCaptureSetup.ts | 246 | CUT | SnapShot screen capture (desktop feature) |  |  |
| HyprlandSnapShot.ts | 222 | CUT | SnapShot screen capture (desktop feature) |  |  |
| KdeSnapShot.ts | 285 | CUT | SnapShot screen capture (desktop feature) |  |  |
| LinuxSnapShot.ts | 422 | CUT | SnapShot screen capture (desktop feature) |  |  |
| MacModifierPairShortcutProcess.ts | 93 | CUT | SnapShot screen capture (desktop feature) |  |  |
| MacSnapShot.ts | 70 | CUT | SnapShot screen capture (desktop feature) |  |  |
| NativeCaptureFeedback.ts | 113 | CUT | SnapShot screen capture (desktop feature) |  |  |
| NiriCaptureShortcut.ts | 66 | CUT | SnapShot screen capture (desktop feature) |  |  |
| NiriSnapShot.ts | 263 | CUT | SnapShot screen capture (desktop feature) |  |  |
| PortalCaptureShortcut.ts | 429 | CUT | SnapShot screen capture (desktop feature) |  |  |
| RegionSnapShot.ts | 204 | CUT | SnapShot screen capture (desktop feature) |  |  |
| RegionSnapShotWorker.ts | 29 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotAccessibility.ts | 249 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotAccessibilityProcess.ts | 176 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotAccessibilityWorker.ts | 29 | CUT | SnapShot screen capture (desktop feature) |  |  |
| SnapShotTransition.ts | 450 | CUT | SnapShot screen capture (desktop feature) |  |  |
| WindowsCaptureFeedback.ts | 16 | CUT | SnapShot screen capture (desktop feature) |  |  |
| captureConfigEdit.ts | 195 | CUT | SnapShot screen capture (desktop feature) |  |  |
| captureConfigKdl.ts | 146 | CUT | SnapShot screen capture (desktop feature) |  |  |
| gnomeCaptureBundle.ts | 4 | CUT | SnapShot screen capture (desktop feature) |  |  |
| linuxCaptureSession.ts | 150 | CUT | SnapShot screen capture (desktop feature) |  |  |
| snapShot.ts | 615 | CUT | SnapShot screen capture (desktop feature) |  |  |

### `apps/desktop/src/ssh/`

2 files, 644 lines: KEEP 0 / 0, CUT 2 / 644, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopSshEnvironment.ts | 174 | CUT | T3 Connect / relay / remote access / auth |  |  |
| DesktopSshPasswordPrompts.ts | 470 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `apps/desktop/src/telemetry/`

2 files, 713 lines: KEEP 0 / 0, CUT 2 / 713, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopRendererHistory.ts | 283 | CUT | Telemetry / observability / resource telemetry |  |  |
| DesktopTelemetryPublisher.ts | 430 | CUT | Telemetry / observability / resource telemetry |  |  |

### `apps/desktop/src/updates/`

6 files, 2,185 lines: KEEP 0 / 0, CUT 6 / 2,185, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopRemoteUpdates.ts | 461 | CUT | Auto-updates (app / server) |  |  |
| DesktopUpdates.ts | 1,180 | CUT | Auto-updates (app / server) |  |  |
| releaseNotes.ts | 165 | CUT | Auto-updates (app / server) |  |  |
| remoteUpdateFlow.ts | 128 | CUT | Auto-updates (app / server) |  |  |
| updateChannels.ts | 18 | CUT | Auto-updates (app / server) |  |  |
| updateMachine.ts | 233 | CUT | Auto-updates (app / server) |  |  |

### `apps/desktop/src/window/`

4 files, 1,690 lines: KEEP 0 / 0, CUT 0 / 0, UNASSIGNED 4 / 1,690, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ContextMenuImage.ts | 104 | **UNASSIGNED** | N9 Native macOS shell |  |  |
| DesktopApplicationMenu.ts | 291 | **UNASSIGNED** | N9 Native macOS shell |  |  |
| DesktopWindow.ts | 1,074 | **UNASSIGNED** | N9 Native macOS shell |  |  |
| QuitHold.ts | 221 | **UNASSIGNED** | N9 Native macOS shell |  |  |

### `apps/desktop/src/wsl/`

4 files, 1,989 lines: KEEP 0 / 0, CUT 4 / 1,989, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| DesktopWslBackend.ts | 289 | CUT | T3 Connect / relay / remote access / auth |  |  |
| DesktopWslEnvironment.ts | 1,320 | CUT | T3 Connect / relay / remote access / auth |  |  |
| DesktopWslServerTree.ts | 254 | CUT | T3 Connect / relay / remote access / auth |  |  |
| wslPathParsing.ts | 126 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/client-runtime/src/`

26 files, 2,467 lines: KEEP 21 / 2,209, CUT 5 / 258, UNASSIGNED 0 / 0, ported 18 / 1,868.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| composerThreadItems.ts | 51 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/composer_thread_items.rs` |
| delayedStatus.ts | 86 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/delayed_status.rs` |
| diffCount.ts | 6 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/diff_count.rs` |
| filePreview.ts | 44 | KEEP | T14 | not started |  |
| handoff.ts | 91 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/handoff.rs` |
| imageAttachmentSize.ts | 34 | KEEP | T17 | not started |  |
| load-balancing.ts | 40 | CUT | Load balancing across environments (remote) |  |  |
| markdownImages.ts | 71 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/markdown_images.rs` |
| mediaActions.ts | 8 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/media_actions.rs` |
| mediaReference.ts | 92 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/media_reference.rs` |
| mediaSource.ts | 102 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/media_source.rs` |
| projectFaviconCache.ts | 263 | KEEP | T15 | not started |  |
| promptLinks.ts | 105 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/prompt_links.rs` |
| providerSkills.ts | 130 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/provider_skills.rs` |
| providerUpdateRequiredModels.ts | 45 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/provider_update_required_models.rs` |
| pullRequestMarkdown.ts | 146 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/pull_request_markdown.rs` |
| reviewGuideTour.ts | 224 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/review_guide_tour.rs` |
| scheduledTaskWebhook.ts | 13 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| secretRequest.ts | 101 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| sourceControlClients.ts | 56 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/source_control_clients.rs` |
| t3ToolSummary.ts | 445 | KEEP | T14 | ported | `T16-logic:app/src/components/chat/timeline/t3_tool_summary.rs` |
| textPaste.ts | 76 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/text_paste.rs` |
| threadPullRequestCompatibility.ts | 75 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/thread_pull_request_compatibility.rs` |
| userMessage.ts | 59 | KEEP | T14 | ported | `T14-client-state:app/src/state/helpers/user_message.rs` |
| webhookAddress.ts | 40 | CUT | Scheduled tasks / secret requests / webhooks |  |  |
| worktreeSetup.ts | 64 | CUT | Thread launch into new worktree (replaced by hooks) |  |  |

### `packages/client-runtime/src/authorization/`

4 files, 853 lines: KEEP 0 / 0, CUT 4 / 853, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| index.ts | 6 | CUT | T3 Connect / relay / remote access / auth |  |  |
| remote.ts | 250 | CUT | T3 Connect / relay / remote access / auth |  |  |
| service.ts | 559 | CUT | T3 Connect / relay / remote access / auth |  |  |
| tokenStore.ts | 38 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/client-runtime/src/connection/`

19 files, 4,933 lines: KEEP 0 / 0, CUT 19 / 4,933, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| catalog.ts | 160 | CUT | T3 Connect / relay / remote access / auth |  |  |
| compatibility.ts | 42 | CUT | T3 Connect / relay / remote access / auth |  |  |
| connectivity.ts | 19 | CUT | T3 Connect / relay / remote access / auth |  |  |
| credentialStore.ts | 23 | CUT | T3 Connect / relay / remote access / auth |  |  |
| driver.ts | 205 | CUT | T3 Connect / relay / remote access / auth |  |  |
| errors.ts | 206 | CUT | T3 Connect / relay / remote access / auth |  |  |
| githubRoutingPermissions.ts | 169 | CUT | T3 Connect / relay / remote access / auth |  |  |
| index.ts | 22 | CUT | T3 Connect / relay / remote access / auth |  |  |
| layer.ts | 106 | CUT | T3 Connect / relay / remote access / auth |  |  |
| model.ts | 178 | CUT | T3 Connect / relay / remote access / auth |  |  |
| onboarding.ts | 335 | CUT | T3 Connect / relay / remote access / auth |  |  |
| outdatedHostUpdate.ts | 210 | CUT | T3 Connect / relay / remote access / auth |  |  |
| presentation.ts | 152 | CUT | T3 Connect / relay / remote access / auth |  |  |
| profileStore.ts | 20 | CUT | T3 Connect / relay / remote access / auth |  |  |
| registry.ts | 1,212 | CUT | T3 Connect / relay / remote access / auth |  |  |
| resolver.ts | 323 | CUT | T3 Connect / relay / remote access / auth |  |  |
| routes.ts | 432 | CUT | T3 Connect / relay / remote access / auth |  |  |
| supervisor.ts | 1,077 | CUT | T3 Connect / relay / remote access / auth |  |  |
| wakeups.ts | 42 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/client-runtime/src/device/`

19 files, 4,275 lines: KEEP 0 / 0, CUT 19 / 4,275, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| androidFoldScene.ts | 420 | CUT | Browser / preview / devices |  |  |
| deviceFraming.ts | 82 | CUT | Browser / preview / devices |  |  |
| deviceMotion.ts | 279 | CUT | Browser / preview / devices |  |  |
| deviceViewSnap.ts | 26 | CUT | Browser / preview / devices |  |  |
| duoControl.ts | 151 | CUT | Browser / preview / devices |  |  |
| duoScene.ts | 244 | CUT | Browser / preview / devices |  |  |
| duoSnap.ts | 86 | CUT | Browser / preview / devices |  |  |
| duoViewer.ts | 602 | CUT | Browser / preview / devices |  |  |
| frame.ts | 24 | CUT | Browser / preview / devices |  |  |
| hubAccess.ts | 18 | CUT | Browser / preview / devices |  |  |
| model.ts | 88 | CUT | Browser / preview / devices |  |  |
| modelScene.ts | 118 | CUT | Browser / preview / devices |  |  |
| phoneInteraction.ts | 83 | CUT | Browser / preview / devices |  |  |
| phoneScene.ts | 302 | CUT | Browser / preview / devices |  |  |
| phoneViewer.ts | 444 | CUT | Browser / preview / devices |  |  |
| renderScheduler.ts | 23 | CUT | Browser / preview / devices |  |  |
| screenshot.ts | 30 | CUT | Browser / preview / devices |  |  |
| shapeProfile.ts | 124 | CUT | Browser / preview / devices |  |  |
| stream.ts | 1,131 | CUT | Browser / preview / devices |  |  |

### `packages/client-runtime/src/environment/`

5 files, 140 lines: KEEP 0 / 0, CUT 5 / 140, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| descriptor.ts | 17 | CUT | T3 Connect / relay / remote access / auth |  |  |
| endpoint.ts | 9 | CUT | T3 Connect / relay / remote access / auth |  |  |
| index.ts | 4 | CUT | T3 Connect / relay / remote access / auth |  |  |
| knownEnvironment.ts | 41 | CUT | T3 Connect / relay / remote access / auth |  |  |
| scoped.ts | 69 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/client-runtime/src/errors/`

6 files, 227 lines: KEEP 4 / 177, CUT 2 / 50, UNASSIGNED 0 / 0, ported 1 / 4.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| errorTrace.ts | 50 | KEEP | T14 | not started |  |
| index.ts | 4 | KEEP | T14 | ported | re-export barrel (no module needed) |
| network.ts | 4 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |
| orchestration.ts | 16 | KEEP | T14 | not started |  |
| safeLog.ts | 107 | KEEP | T14 | not started |  |
| transport.ts | 46 | CUT | Server process / RPC / HTTP transport (no client-server split) |  |  |

### `packages/client-runtime/src/mcpApps/`

1 files, 576 lines: KEEP 0 / 0, CUT 1 / 576, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| host.ts | 576 | CUT | MCP apps / orchestrator MCP server |  |  |

### `packages/client-runtime/src/operations/`

4 files, 1,485 lines: KEEP 4 / 1,485, CUT 0 / 0, UNASSIGNED 0 / 0, ported 4 / 1,485.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| commands.ts | 1,106 | KEEP | T14 | ported | `T14-client-state:app/src/state/operations/commands.rs` |
| index.ts | 3 | KEEP | T14 | ported | re-export barrel (no module needed) |
| projects.ts | 343 | KEEP | T14 | ported | `T14-client-state:app/src/state/operations/projects.rs` |
| threadTitle.ts | 33 | KEEP | T14 | ported | `T14-client-state:app/src/state/operations/thread_title.rs` |

### `packages/client-runtime/src/platform/`

6 files, 533 lines: KEEP 0 / 0, CUT 6 / 533, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| capabilities.ts | 78 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| index.ts | 7 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| orchestrationCache.ts | 41 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| persistence.ts | 154 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| source.ts | 15 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |
| storageDocument.ts | 238 | CUT | Web-only infra (router, React/atom glue, DOM, localStorage) |  |  |

### `packages/client-runtime/src/preview/`

1 files, 482 lines: KEEP 0 / 0, CUT 1 / 482, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| serverBrowserStream.ts | 482 | CUT | Browser / preview / devices |  |  |

### `packages/client-runtime/src/relay/`

5 files, 1,864 lines: KEEP 0 / 0, CUT 5 / 1,864, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| discovery.ts | 350 | CUT | T3 Connect / relay / remote access / auth |  |  |
| errorPresentation.ts | 110 | CUT | T3 Connect / relay / remote access / auth |  |  |
| index.ts | 4 | CUT | T3 Connect / relay / remote access / auth |  |  |
| managedRelay.ts | 955 | CUT | T3 Connect / relay / remote access / auth |  |  |
| managedRelayState.ts | 445 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/client-runtime/src/rpc/`

5 files, 1,049 lines: KEEP 0 / 0, CUT 5 / 1,049, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 452 | CUT | T3 Connect / relay / remote access / auth |  |  |
| http.ts | 176 | CUT | T3 Connect / relay / remote access / auth |  |  |
| index.ts | 4 | CUT | T3 Connect / relay / remote access / auth |  |  |
| protocol.ts | 12 | CUT | T3 Connect / relay / remote access / auth |  |  |
| session.ts | 405 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/client-runtime/src/state/`

88 files, 17,706 lines: KEEP 67 / 15,376, CUT 21 / 2,330, UNASSIGNED 0 / 0, ported 27 / 5,479.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| archivedThreads.ts | 69 | KEEP | T14 | ported | `T14-client-state:app/src/state/archived_threads.rs` |
| assets.ts | 280 | KEEP | T14 | partial | `T14-client-state:app/src/assets.rs (name match, no header)` |
| attachments.ts | 236 | KEEP | T14 | not started |  |
| auth.ts | 90 | CUT | T3 Connect / relay / remote access / auth |  |  |
| boundedThreadSnapshotHttp.ts | 175 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| cachePersistence.ts | 19 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| checkpointDiff.ts | 25 | KEEP | T14 | not started |  |
| commandPermissions.ts | 95 | CUT | T3 Connect / relay / remote access / auth |  |  |
| composerDispatch.ts | 32 | KEEP | T14 | ported | `T14-client-state:app/src/state/composer_dispatch.rs` |
| composerPathSearch.ts | 19 | KEEP | T14 | not started |  |
| connections.ts | 211 | CUT | T3 Connect / relay / remote access / auth |  |  |
| device.ts | 99 | CUT | Browser / preview / devices |  |  |
| deviceHubAccess.ts | 60 | CUT | Browser / preview / devices |  |  |
| entities.ts | 125 | KEEP | T14 | ported | `T14-client-state:app/src/state/entities.rs` |
| environmentHttpAuth.ts | 241 | CUT | T3 Connect / relay / remote access / auth |  |  |
| environmentStreams.ts | 35 | CUT | T3 Connect / relay / remote access / auth |  |  |
| fileMetadata.ts | 338 | KEEP | T14 | not started |  |
| filesystem.ts | 123 | KEEP | T14 | not started |  |
| git.ts | 23 | KEEP | T14 | not started |  |
| gitActions.ts | 354 | KEEP | T14 | not started |  |
| itemSupport.ts | 138 | KEEP | T14 | ported | `T14-client-state:app/src/state/item_support.rs` |
| mcpApps.ts | 33 | CUT | MCP apps / orchestrator MCP server |  |  |
| models.ts | 345 | KEEP | T14 | ported | `T14-client-state:app/src/state/models.rs` |
| orchestration.ts | 85 | KEEP | T14 | not started |  |
| orchestrationV2Projection.ts | 332 | KEEP | T14 | ported | `T14-client-state:app/src/state/orchestration_v2_projection.rs` |
| outdatedServerUpdate.ts | 78 | CUT | Auto-updates (app / server) |  |  |
| presentation.ts | 222 | KEEP | T14 | not started |  |
| preview.ts | 95 | CUT | Browser / preview / devices |  |  |
| projectCommands.ts | 228 | KEEP | T14 | not started |  |
| projectEntities.ts | 138 | KEEP | T14 | not started |  |
| projectGrouping.ts | 400 | KEEP | T14 | ported | `T14-client-state:app/src/state/project_grouping.rs` |
| projects.ts | 208 | KEEP | T14 | ported | `T14-client-state:app/src/state/projects.rs` |
| providerInstanceDisplay.ts | 103 | KEEP | T14 | not started |  |
| pullRequestDiffHttp.ts | 99 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| pullRequestRouting.ts | 510 | KEEP | T14 | not started |  |
| pullRequestStatsPersistence.ts | 43 | KEEP | T14 | not started |  |
| pullRequests.ts | 621 | KEEP | T14 | not started |  |
| queryPersistence.ts | 49 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| relayDiscovery.ts | 41 | CUT | T3 Connect / relay / remote access / auth |  |  |
| review.ts | 75 | KEEP | T14 | not started |  |
| runtime.ts | 870 | KEEP | T14 | partial | `T14-client-state:app/src/state/command_result.rs` |
| server.ts | 1,366 | KEEP | T14 | not started |  |
| serverConfigProjection.ts | 99 | KEEP | T14 | not started |  |
| session.ts | 189 | KEEP | T14 | not started |  |
| sharedSettings.ts | 178 | KEEP | T14 | not started |  |
| shell.ts | 574 | KEEP | T14 | partial | `T14-client-state:app/src/state/shell_state.rs` |
| shellCommands.ts | 16 | KEEP | T14 | not started |  |
| shellPullRequests.ts | 97 | KEEP | T14 | ported | `T14-client-state:app/src/state/shell_reducer.rs` |
| shellReducer.ts | 200 | KEEP | T14 | ported | `T14-client-state:app/src/state/shell_reducer.rs` |
| shellSnapshotHttp.ts | 132 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| snapshots.ts | 20 | KEEP | T14 | not started |  |
| sourceControl.ts | 86 | KEEP | T14 | not started |  |
| subagentDisplay.ts | 132 | KEEP | T14 | ported | `T14-client-state:app/src/state/subagent_display.rs` |
| subagentRuntime.ts | 320 | KEEP | T14 | ported | `T14-client-state:app/src/state/subagent_runtime.rs` |
| terminal.ts | 106 | CUT | Terminal / PTY |  |  |
| terminalOutput.ts | 320 | CUT | Terminal / PTY |  |  |
| terminalSession.ts | 213 | CUT | Terminal / PTY |  |  |
| threadCheckpoints.ts | 52 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_checkpoints.rs` |
| threadCommands.ts | 489 | KEEP | T14 | not started |  |
| threadDetail.ts | 199 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_detail.rs` |
| threadExecution.ts | 453 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_execution.rs` |
| threadFeedback.ts | 119 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_feedback.rs` |
| threadHistoryController.ts | 94 | KEEP | T14 | partial | `T14-client-state:app/src/state/thread_history_merge.rs` |
| threadHistoryHttp.ts | 44 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| threadHistoryMerge.ts | 154 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_history_merge.rs` |
| threadInbox.ts | 148 | KEEP | T14 | ported | `T15-sidebar:app/src/components/sidebar/runtime/thread_inbox.rs` |
| threadLifecycle.ts | 102 | KEEP | T14 | not started |  |
| threadRelationships.ts | 257 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_relationships.rs` |
| threadRequests.ts | 173 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_requests.rs` |
| threadRetention.ts | 3 | KEEP | T14 | not started |  |
| threadSearch.ts | 85 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_search.rs` |
| threadSettled.ts | 360 | KEEP | T14 | ported | `T15-sidebar:app/src/components/sidebar/runtime/thread_settled.rs` |
| threadShell.ts | 376 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_shell.rs` |
| threadSnapshotHttp.ts | 95 | CUT | Server process / RPC / HTTP transport (no client-server split); in-process reads come from T13a |  |  |
| threadSort.ts | 400 | KEEP | T14 | ported | `T15-sidebar:app/src/components/sidebar/runtime/thread_sort.rs` |
| threadState.ts | 25 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_state.rs` |
| threadSubagents.ts | 90 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_subagents.rs` |
| threadWorkflows.ts | 190 | KEEP | T14 | ported | `T14-client-state:app/src/state/thread_workflows.rs` |
| threads.ts | 979 | KEEP | T14 | not started |  |
| turnItemPresentation.ts | 49 | KEEP | T14 | partial | `T16-logic:app/src/components/chat/timeline/turn_item_presentation.rs` |
| usage.ts | 135 | KEEP | T14 | not started |  |
| usageAccess.ts | 43 | KEEP | T14 | not started |  |
| usageProgress.ts | 114 | KEEP | T14 | not started |  |
| vcs.ts | 363 | KEEP | T14 | not started |  |
| vcsAction.ts | 600 | KEEP | T14 | not started |  |
| vcsCommandScheduler.ts | 13 | KEEP | T14 | not started |  |
| vcsRef.ts | 9 | KEEP | T14 | not started |  |
| vcsRefInvalidation.ts | 83 | KEEP | T14 | not started |  |

### `packages/client-runtime/src/voice-input/`

3 files, 572 lines: KEEP 0 / 0, CUT 3 / 572, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| controller.ts | 515 | CUT | Voice input (T14 brief cut) |  |  |
| index.ts | 20 | CUT | Voice input (T14 brief cut) |  |  |
| transcription.ts | 37 | CUT | Voice input (T14 brief cut) |  |  |

### `packages/client-runtime/src/work-log/`

6 files, 2,730 lines: KEEP 6 / 2,730, CUT 0 / 0, UNASSIGNED 0 / 0, ported 5 / 2,697.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| commandLabel.ts | 1,461 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/command_label.rs` |
| itemDetail.ts | 272 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/item_detail.rs` |
| presentation.ts | 777 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/work_log_presentation.rs` |
| scrollAnchor.ts | 33 | KEEP | T16 | not started |  |
| toolPresentation.ts | 143 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/tool_presentation.rs` |
| userInput.ts | 44 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/user_input.rs` |

### `packages/contracts/src/`

68 files, 27,633 lines: KEEP 37 / 14,964, CUT 31 / 12,669, UNASSIGNED 0 / 0, ported 36 / 13,931.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| acpRegistry.ts | 347 | CUT | PORTING contracts cut list |  |  |
| agentSessions.ts | 111 | KEEP | T01 | ported | `T01-contracts:contracts/src/agent_sessions.rs` |
| applicationEvent.ts | 124 | CUT | PORTING contracts cut list |  |  |
| assets.ts | 331 | CUT | PORTING contracts cut list |  |  |
| assistantCitations.ts | 31 | KEEP | T01 | ported | `T01-contracts:contracts/src/assistant_citations.rs` |
| auth.ts | 652 | CUT | PORTING contracts cut list |  |  |
| background.ts | 110 | CUT | PORTING contracts cut list |  |  |
| baseSchemas.ts | 407 | KEEP | T01 | ported | `T01-contracts:contracts/src/base_schemas.rs` |
| browserImport.ts | 165 | CUT | PORTING contracts cut list |  |  |
| browserProfile.ts | 99 | CUT | PORTING contracts cut list |  |  |
| chatAttachment.ts | 260 | KEEP | T01 | ported | `T01-contracts:contracts/src/chat_attachment.rs` |
| checkpointDiff.ts | 65 | KEEP | T01 | ported | `T01-contracts:contracts/src/checkpoint_diff.rs` |
| clientRpcPermissions.ts | 67 | CUT | PORTING contracts cut list |  |  |
| composerContext.ts | 307 | KEEP | T01 | ported | `T01-contracts:contracts/src/composer_context.rs` |
| composerContextClipboard.ts | 27 | KEEP | T01 | ported | `T01-contracts:contracts/src/composer_context_clipboard.rs` |
| desktopAppActivation.ts | 53 | CUT | PORTING contracts cut list |  |  |
| desktopBootstrap.ts | 47 | CUT | PORTING contracts cut list |  |  |
| desktopBrowser.ts | 46 | CUT | PORTING contracts cut list |  |  |
| device.ts | 583 | CUT | PORTING contracts cut list |  |  |
| editor.ts | 337 | KEEP | T01 | ported | `T01-contracts:contracts/src/editor.rs` |
| environment.ts | 293 | KEEP | T01 | ported | `main:contracts/src/environment.rs` |
| environmentHttp.ts | 773 | CUT | PORTING contracts cut list |  |  |
| filesystem.ts | 92 | KEEP | T01 | ported | `T01-contracts:contracts/src/filesystem.rs` |
| git.ts | 507 | KEEP | T01 | ported | `T01-contracts:contracts/src/git.rs` |
| index.ts | 67 | KEEP | T01 | ported | re-export barrel (no module needed) |
| ipc.ts | 1,402 | CUT | PORTING contracts cut list |  |  |
| keybindings.ts | 227 | KEEP | T01 | ported | `T01-contracts:contracts/src/keybindings.rs` |
| mcpApps.ts | 114 | CUT | PORTING contracts cut list |  |  |
| model.ts | 238 | KEEP | T01 | ported | `T01-contracts:contracts/src/model.rs` |
| modelSelection.ts | 63 | KEEP | T01 | ported | `T01-contracts:contracts/src/model_selection.rs` |
| orchestrationDispatch.ts | 16 | KEEP | T01 | ported | `T01-contracts:contracts/src/orchestration_dispatch.rs` |
| orchestrationProject.ts | 28 | KEEP | T01 | ported | `T01-contracts:contracts/src/orchestration_project.rs` |
| orchestrationV2.ts | 3,858 | KEEP | T01 | ported | `T01-contracts:contracts/src/orchestration_v2.rs` |
| orchestratorMcp.ts | 684 | CUT | PORTING contracts cut list |  |  |
| preview.ts | 460 | CUT | PORTING contracts cut list |  |  |
| previewAutomation.ts | 1,191 | CUT | PORTING contracts cut list |  |  |
| project.ts | 564 | KEEP | T01 | ported | `T01-contracts:contracts/src/project.rs` |
| projectClone.ts | 122 | CUT | PORTING contracts cut list |  |  |
| provider.ts | 164 | KEEP | T01 | ported | `T01-contracts:contracts/src/provider.rs` |
| providerInstance.ts | 168 | KEEP | T01 | ported | `T01-contracts:contracts/src/provider_instance.rs` |
| providerPolicy.ts | 84 | KEEP | T01 | ported | `T01-contracts:contracts/src/provider_policy.rs` |
| providerRuntime.ts | 1,206 | KEEP | T01 | ported | `T01-contracts:contracts/src/provider_runtime.rs` |
| providerSetup.ts | 255 | KEEP | T01 | ported | `T01-contracts:contracts/src/provider_setup.rs` |
| providerUsageLimits.ts | 178 | KEEP | T01 | ported | `T01-contracts:contracts/src/provider_usage_limits.rs` |
| pullRequest.ts | 1,482 | KEEP | T01 | ported | `T01-contracts:contracts/src/pull_request.rs` |
| pullRequestReviewGuide.ts | 115 | KEEP | T01 | ported | `T01-contracts:contracts/src/pull_request_review_guide.rs` |
| relay.ts | 1,273 | CUT | PORTING contracts cut list |  |  |
| relayClient.ts | 63 | CUT | PORTING contracts cut list |  |  |
| remoteAccess.ts | 68 | CUT | PORTING contracts cut list |  |  |
| resourceTelemetry.ts | 522 | CUT | PORTING contracts cut list |  |  |
| review.ts | 70 | KEEP | T01 | ported | `T01-contracts:contracts/src/review.rs` |
| rpc.ts | 2,072 | CUT | PORTING contracts cut list |  |  |
| scheduledTask.ts | 339 | CUT | PORTING contracts cut list |  |  |
| secretRequest.ts | 52 | CUT | PORTING contracts cut list |  |  |
| server.ts | 1,033 | KEEP | T01 | partial | `T05b-shared-contracts:provider-core/src/server_provider.rs` |
| settings.ts | 1,761 | KEEP | T01 | ported | `T01-contracts:contracts/src/settings.rs` |
| sourceControl.ts | 207 | KEEP | T01 | ported | `T01-contracts:contracts/src/source_control.rs` |
| t3ProjectFile.ts | 152 | CUT | PORTING contracts cut list |  |  |
| terminal.ts | 384 | CUT | PORTING contracts cut list |  |  |
| threadMetadataMcp.ts | 122 | CUT | PORTING contracts cut list |  |  |
| threadPullRequest.ts | 136 | KEEP | T01 | ported | `T01-contracts:contracts/src/thread_pull_request.rs` |
| threadSearch.ts | 42 | KEEP | T01 | ported | `T01-contracts:contracts/src/thread_search.rs` |
| threadTitle.ts | 10 | KEEP | T01 | ported | `T01-contracts:contracts/src/thread_title.rs` |
| usage.ts | 259 | KEEP | T01 | ported | `T01-contracts:contracts/src/usage.rs` |
| usageLimitSourceId.ts | 9 | KEEP | T01 | ported | `T01-contracts:contracts/src/usage_limit_source_id.rs` |
| vcs.ts | 287 | KEEP | T01 | ported | `T01-contracts:contracts/src/vcs.rs` |
| worktreeMcp.ts | 128 | CUT | PORTING contracts cut list |  |  |
| worktreeSetup.ts | 124 | CUT | PORTING contracts cut list |  |  |

### `packages/effect-acp/src/`

7 files, 4,259 lines: KEEP 7 / 4,259, CUT 0 / 0, UNASSIGNED 0 / 0, ported 6 / 3,845.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| agent.ts | 586 | KEEP | T12a | ported | `T12a-acp:acp/src/agent.rs` |
| client.ts | 1,433 | KEEP | T12a | ported | `T12a-acp:acp/src/client.rs` |
| compat.ts | 451 | KEEP | T12a | ported | `T12a-acp:acp/src/compat.rs` |
| errors.ts | 380 | KEEP | T12a | ported | `T12a-acp:acp/src/errors.rs` |
| protocol.ts | 980 | KEEP | T12a | ported | `T12a-acp:acp/src/protocol.rs` |
| rpc.ts | 414 | KEEP | T12a | partial | `T12a-acp:acp/src/meta.rs` |
| schema.ts | 15 | KEEP | T12a | ported | re-export barrel (no module needed) |

### `packages/effect-acp/src/_internal/`

2 files, 195 lines: KEEP 2 / 195, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| shared.ts | 132 | KEEP | T12a | not started |  |
| stdio.ts | 63 | KEEP | T12a | not started |  |

### `packages/effect-codex-app-server/src/`

6 files, 1,755 lines: KEEP 6 / 1,755, CUT 0 / 0, UNASSIGNED 0 / 0, ported 6 / 1,755.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 275 | KEEP | T09a | ported | `T09a-codex-app-server:codex-app-server/src/client.rs` |
| errors.ts | 428 | KEEP | T09a | ported | `T09a-codex-app-server:codex-app-server/src/errors.rs` |
| protocol.ts | 485 | KEEP | T09a | ported | `T09a-codex-app-server:codex-app-server/src/protocol.rs` |
| replay.ts | 563 | KEEP | T09a | ported | `T09a-codex-app-server:codex-app-server/src/replay.rs` |
| rpc.ts | 1 | KEEP | T09a | ported | `T09a-codex-app-server:codex-app-server/src/rpc.rs` |
| schema.ts | 3 | KEEP | T09a | ported | `T09a-codex-app-server:codex-app-server/src/schema.rs` |

### `packages/effect-codex-app-server/src/_internal/`

2 files, 154 lines: KEEP 2 / 154, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| shared.ts | 91 | KEEP | T09a | not started |  |
| stdio.ts | 63 | KEEP | T09a | not started |  |

### `packages/provider-acp-registry/src/`

3 files, 120 lines: KEEP 0 / 0, CUT 3 / 120, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 28 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| server.ts | 16 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| settings.ts | 76 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-acp-registry/src/server/`

9 files, 5,025 lines: KEEP 0 / 0, CUT 9 / 5,025, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AcpRegistryRuntimeCoordinator.ts | 362 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| AcpRegistrySupport.ts | 1,963 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| adapter.ts | 327 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| auth.ts | 370 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| authenticationState.ts | 82 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| copilotAcp.ts | 175 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| devinAcp.ts | 57 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| driver.ts | 907 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| probe.ts | 782 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-acp/src/server/`

10 files, 14,424 lines: KEEP 8 / 13,735, CUT 2 / 689, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AcpSessionRuntime.ts | 3,064 | KEEP | T12 | not started |  |
| adapter.ts | 8,088 | KEEP | T12 | not started |  |
| clientPolicy.ts | 297 | KEEP | T12 | not started |  |
| clientTerminals.ts | 448 | CUT | Terminal / PTY |  |  |
| coreRuntimeEvents.ts | 255 | KEEP | T12 | not started |  |
| mcpOverAcpBridge.ts | 241 | CUT | MCP apps / orchestrator MCP server |  |  |
| nativeLogging.ts | 188 | KEEP | T12 | not started |  |
| runtimeModel.ts | 1,644 | KEEP | T12 | not started |  |
| sessionConfig.ts | 163 | KEEP | T12 | not started |  |
| stderr.ts | 36 | KEEP | T12 | not started |  |

### `packages/provider-core/src/`

1 files, 104 lines: KEEP 1 / 104, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 104 | KEEP | T08 | not started |  |

### `packages/provider-core/src/server/`

44 files, 7,375 lines: KEEP 44 / 7,375, CUT 0 / 0, UNASSIGNED 0 / 0, ported 32 / 4,056.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| IdAllocator.ts | 433 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/id_allocator.rs` |
| McpProviderSessions.ts | 40 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/mcp_provider_sessions.rs` |
| ModelCatalog.ts | 171 | KEEP | T08 | not started |  |
| ProviderAdapter.ts | 679 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_adapter.rs` |
| ProviderContinuationRequests.ts | 82 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_continuation_requests.rs` |
| ProviderEventLoggers.ts | 30 | KEEP | T08b | ported | `T08-provider-core:provider-core/src/provider_event_loggers.rs` |
| ProviderHost.ts | 76 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/provider_host.rs` |
| ProviderLatestVersions.ts | 75 | KEEP | T08b | ported | `T08-provider-core:provider-core/src/provider_latest_versions.rs` |
| adapterDriver.ts | 48 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/adapter_driver.rs` |
| attachmentPrompt.ts | 218 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/attachment_prompt.rs` |
| auth.ts | 57 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/auth.rs` |
| collectStreamText.ts | 83 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/collect_stream_text.rs` |
| driver.ts | 232 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/driver.rs` |
| errors.ts | 34 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/errors.rs` |
| failure.ts | 259 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/failure.rs` |
| handoffBudget.ts | 282 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/handoff_budget.rs` |
| instanceEnvironment.ts | 25 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/instance_environment.rs` |
| instanceIdentity.ts | 28 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/instance_identity.rs` |
| maintenance.ts | 30 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/maintenance.rs` |
| maintenanceResolver.ts | 816 | KEEP | T08b | partial | `T08-provider-core:provider-core/src/maintenance_resolver.rs` |
| managedProvider.ts | 312 | KEEP | T08 | not started |  |
| mcpSession.ts | 42 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/mcp_session.rs` |
| mcpToolPresentation.ts | 70 | KEEP | T08b | not started |  |
| nativeProtocolLogging.ts | 40 | KEEP | T08b | ported | `T08-provider-core:provider-core/src/native_protocol_logging.rs` |
| notification.ts | 235 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/notification.rs` |
| orchestrationInstructions.ts | 117 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/orchestration_instructions.rs` |
| pathExpansion.ts | 24 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/path_expansion.rs` |
| processGroup.ts | 16 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/process_group.rs` |
| providerAuthFlow.ts | 525 | KEEP | T08 | not started |  |
| randomUuid.ts | 13 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/id_allocator.rs` |
| runtimeInstructions.ts | 28 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/runtime_instructions.rs` |
| selectionTransition.ts | 37 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/selection_transition.rs` |
| snapshot.ts | 26 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/snapshot.rs` |
| snapshotProbe.ts | 297 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/snapshot_probe.rs` |
| snapshotSettings.ts | 49 | KEEP | T08 | not started |  |
| subagentProjection.ts | 429 | KEEP | T08b | not started |  |
| textGeneration.ts | 150 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/text_generation.rs` |
| textGenerationOperations.ts | 199 | KEEP | T08 | not started |  |
| textGenerationPolicy.ts | 19 | KEEP | T08 | not started |  |
| textGenerationPrompts.ts | 495 | KEEP | T08 | not started |  |
| textGenerationUtils.ts | 129 | KEEP | T08 | not started |  |
| threadTitleContext.ts | 105 | KEEP | T08 | not started |  |
| usage.ts | 186 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/usage.rs` |
| usageLimits.ts | 134 | KEEP | T08 | ported | `T08-provider-core:provider-core/src/usage_limits.rs` |

### `packages/provider-cursor/src/`

3 files, 77 lines: KEEP 3 / 77, CUT 0 / 0, UNASSIGNED 0 / 0, ported 2 / 43.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 34 | KEEP | T12 | not started |  |
| server.ts | 8 | KEEP | T12 | ported | re-export barrel (no module needed) |
| settings.ts | 35 | KEEP | T12 | ported | `T12-cursor-adapter:provider-cursor/src/settings.rs` |

### `packages/provider-cursor/src/server/`

19 files, 6,624 lines: KEEP 11 / 2,851, CUT 7 / 3,557, UNASSIGNED 1 / 216, ported 1 / 386.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| CursorAccountReader.ts | 46 | KEEP | T12 | not started |  |
| CursorAgentSdk.ts | 613 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| CursorKeychain.ts | 118 | KEEP | T12 | not started |  |
| CursorSdk.ts | 84 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| CursorSdkCatalog.ts | 95 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| CursorUsageAccounts.ts | 341 | KEEP | T12 | not started |  |
| accountCache.ts | 271 | KEEP | T12 | not started |  |
| accountUsage.ts | 386 | KEEP | T12 | ported | `T12-cursor-adapter:provider-cursor/src/account_usage.rs` |
| adapter.ts | 2,646 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| auth.ts | 495 | KEEP | T12 | partial | `T12-cursor-adapter:provider-cursor/src/auth.rs` |
| credentialStore.ts | 91 | KEEP | T12 | not started |  |
| driver.ts | 307 | KEEP | T12 | not started |  |
| sdk.ts | 17 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| sdkModel.ts | 50 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| skills.ts | 292 | KEEP | T12 | not started |  |
| status.ts | 350 | KEEP | T12 | not started |  |
| textGeneration.ts | 216 | **UNASSIGNED** | N3 Text generation |  |  |
| transportFailure.ts | 52 | CUT | Cursor in-process SDK runtime replaced by ACP (PORTING Deviations) |  |  |
| usageLimits.ts | 154 | KEEP | T12 | not started |  |

### `packages/provider-grok/src/`

3 files, 74 lines: KEEP 0 / 0, CUT 3 / 74, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 28 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| server.ts | 7 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| settings.ts | 39 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-grok/src/server/`

10 files, 4,106 lines: KEEP 0 / 0, CUT 10 / 4,106, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| acpSupport.ts | 273 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| adapter.ts | 435 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| driver.ts | 233 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| skills.ts | 152 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| status.ts | 588 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| textGeneration.ts | 136 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| usage.ts | 241 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| usageLimits.ts | 180 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| xaiAcpExtension.ts | 1,712 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| xaiBackgroundTasks.ts | 156 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-muse/src/`

3 files, 99 lines: KEEP 0 / 0, CUT 3 / 99, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 55 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| server.ts | 8 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| settings.ts | 36 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-muse/src/server/`

9 files, 3,936 lines: KEEP 0 / 0, CUT 9 / 3,936, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| adapter.ts | 2,334 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| driver.ts | 184 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| itemPresentation.ts | 147 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| maintenance.ts | 137 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| modelCatalog.ts | 139 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| protocol.ts | 232 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| sdk.ts | 254 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| status.ts | 154 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| textGeneration.ts | 355 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-opencode/src/`

3 files, 98 lines: KEEP 3 / 98, CUT 0 / 0, UNASSIGNED 0 / 0, ported 1 / 8.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 24 | KEEP | T11 | not started |  |
| server.ts | 8 | KEEP | T11 | ported | re-export barrel (no module needed) |
| settings.ts | 66 | KEEP | T11 | not started |  |

### `packages/provider-opencode/src/server/`

12 files, 8,034 lines: KEEP 11 / 7,706, CUT 0 / 0, UNASSIGNED 1 / 328, ported 5 / 5,051.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| OpenCodeRuntime.ts | 1,138 | KEEP | T11a | partial | `T11a-opencode-runtime:provider-opencode/src/error.rs` |
| OpenCodeServerLedger.ts | 340 | KEEP | T11a | ported | `T11a-opencode-runtime:provider-opencode/src/ledger.rs` |
| OpenCodeServerOwner.ts | 188 | KEEP | T11a | ported | `T11a-opencode-runtime:provider-opencode/src/server_owner.rs` |
| adapter.ts | 3,778 | KEEP | T11 | ported | `T11-opencode-adapter:provider-opencode/src/adapter/mod.rs` |
| driver.ts | 546 | KEEP | T11 | ported | `T11-opencode-adapter:provider-opencode/src/adapter/driver.rs` |
| openCode2Catalog.ts | 74 | KEEP | T11 | not started |  |
| status.ts | 775 | KEEP | T11 | partial | `T11-opencode-adapter:provider-opencode/src/adapter/logic.rs` |
| textGeneration.ts | 328 | **UNASSIGNED** | N3 Text generation |  |  |
| toolItems.ts | 170 | KEEP | T11 | partial | `T11-opencode-adapter:provider-opencode/src/adapter/logic.rs` |
| usage.ts | 372 | KEEP | T11 | not started |  |
| usageLimits.ts | 126 | KEEP | T11 | not started |  |
| versionProbe.ts | 199 | KEEP | T11a | ported | `T11a-opencode-runtime:provider-opencode/src/version_probe.rs` |

### `packages/provider-opencode/src/server/v2/`

4 files, 5,129 lines: KEEP 3 / 4,921, CUT 0 / 0, UNASSIGNED 1 / 208, ported 3 / 4,921.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| OpenCode2Client.ts | 227 | KEEP | T11a | ported | `T11a-opencode-runtime:provider-opencode/src/v2_client.rs` |
| OpenCode2Server.ts | 198 | KEEP | T11a | ported | `T11a-opencode-runtime:provider-opencode/src/v2_server.rs` |
| adapter.ts | 4,496 | KEEP | T11 | ported | `T11-opencode-adapter:provider-opencode/src/adapter/mod.rs` |
| textGeneration.ts | 208 | **UNASSIGNED** | N3 Text generation |  |  |

### `packages/provider-pi/src/`

3 files, 82 lines: KEEP 0 / 0, CUT 3 / 82, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| client.ts | 27 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| server.ts | 8 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| settings.ts | 47 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/provider-pi/src/server/`

10 files, 5,933 lines: KEEP 0 / 0, CUT 10 / 5,933, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| adapter.ts | 3,477 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| commands.ts | 101 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| driver.ts | 216 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| mcpExtensionSource.ts | 466 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| mcpInjection.ts | 321 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| rpc.ts | 493 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| status.ts | 485 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| textGeneration.ts | 126 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| thinkingCapabilities.ts | 97 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |
| usage.ts | 151 | CUT | Non-kept provider (Antigravity / Grok / Pi / Muse / ACP registry) |  |  |

### `packages/shared/src/`

119 files, 24,492 lines: KEEP 85 / 18,238, CUT 34 / 6,254, UNASSIGNED 0 / 0, ported 56 / 10,392.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AgentScope.ts | 135 | CUT | Linux systemd resource scoping (resource telemetry) |  |  |
| Array.ts | 6 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/array.rs` |
| DrainableWorker.ts | 94 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/drainable_worker.rs` |
| HostProcess.ts | 110 | KEEP | T05a | ported | `T07-source-control:source-control/src/shared/host_process.rs` |
| KeyedCoalescingWorker.ts | 125 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/keyed_coalescing_worker.rs` |
| KeyedLock.ts | 62 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/keyed_lock.rs` |
| Net.ts | 205 | KEEP | T05a | not started |  |
| PtyAdapter.ts | 66 | CUT | Terminal / PTY |  |  |
| String.ts | 8 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/string.rs` |
| Struct.ts | 23 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/struct.rs` |
| advertisedEndpoint.ts | 78 | CUT | T3 Connect / relay / remote access / auth |  |  |
| agentAwareness.ts | 139 | KEEP | T05b | not started |  |
| assistantCitations.ts | 192 | KEEP | T05b | ported | `T16-logic:app/src/components/chat/timeline/assistant_citations.rs` |
| atomicWrite.ts | 38 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/atomic_write.rs` |
| authScopeOptions.ts | 106 | CUT | T3 Connect / relay / remote access / auth |  |  |
| backgroundActivitySettings.ts | 270 | KEEP | T05b | not started |  |
| changeRequestUrl.ts | 242 | KEEP | T05b | ported | `T07-source-control:source-control/src/shared/change_request_url.rs` |
| chatList.ts | 42 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/chat_list.rs` |
| claudeCompaction.ts | 26 | KEEP | T10 | ported | `T05a-shared-pure:shared/src/claude_compaction.rs` |
| cliArgs.ts | 132 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/cli_args.rs` |
| cliRelease.ts | 124 | CUT | Electron shell / desktop-only infra |  |  |
| codexArtifactTemplates.ts | 136 | KEEP | T09 | ported | `T05a-shared-pure:shared/src/codex_artifact_templates.rs` |
| codexAuthCallback.ts | 100 | KEEP | T09 | not started |  |
| codexAuthHandoff.ts | 149 | KEEP | T05b | not started |  |
| codexFileCitations.ts | 56 | KEEP | T09 | ported | `T05a-shared-pure:shared/src/codex_file_citations.rs` |
| codexMarkdownDirectives.ts | 428 | KEEP | T09 | ported | `T16-logic:app/src/components/chat/timeline/codex_markdown_directives.rs` |
| composerContextClipboard.ts | 60 | KEEP | T05b | not started |  |
| composerContextLegacy.ts | 454 | KEEP | T05b | not started |  |
| composerContextLegacySend.ts | 161 | KEEP | T05b | not started |  |
| composerContextReferences.ts | 337 | KEEP | T05b | partial | `T13b-orch-pipeline:orchestration/src/pipeline/attachment_claims.rs` |
| composerInlineTokens.ts | 153 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/composer_inline_tokens.rs` |
| composerPullRequestMatches.ts | 87 | KEEP | T05b | ported | `T07-source-control:source-control/src/shared/composer_pull_request_matches.rs` |
| composerTrigger.ts | 150 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/composer_trigger.rs` |
| connectAuth.ts | 106 | CUT | T3 Connect / relay / remote access / auth |  |  |
| dateTime.ts | 38 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/date_time.rs` |
| delimitedPreview.ts | 56 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/delimited_preview.rs` |
| desktopAppControl.ts | 41 | CUT | Electron shell / desktop-only infra |  |  |
| desktopBootstrapToken.ts | 45 | CUT | Electron shell / desktop-only infra |  |  |
| devHome.ts | 103 | KEEP | T05a | not started |  |
| devProxy.ts | 31 | CUT | Electron shell / desktop-only infra |  |  |
| dpop.ts | 199 | CUT | T3 Connect / relay / remote access / auth |  |  |
| dpopCommon.ts | 20 | CUT | T3 Connect / relay / remote access / auth |  |  |
| editor.ts | 153 | KEEP | T05b | not started |  |
| favicon.ts | 101 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/favicon.rs` |
| fileLinks.ts | 183 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/file_links.rs` |
| filePreview.ts | 219 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/file_preview.rs` |
| git.ts | 428 | KEEP | T05b | ported | `T06-git:git/src/shared_git.rs` |
| gitPatchPath.ts | 140 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/git_patch_path.rs` |
| githubMarkdownLinks.ts | 91 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/github_markdown_links.rs` |
| githubMedia.ts | 70 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/github_media.rs` |
| hostClassification.ts | 190 | CUT | T3 Connect / relay / remote access / auth |  |  |
| htmlRender.ts | 492 | CUT | Browser / preview / devices |  |  |
| httpObservability.ts | 8 | CUT | Telemetry / observability / resource telemetry |  |  |
| httpReadiness.ts | 164 | KEEP | T05a | not started |  |
| image.ts | 48 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/image.rs` |
| imageDimensions.ts | 154 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/image_dimensions.rs` |
| inlineSkills.ts | 35 | KEEP | T05b | not started |  |
| keybindings.ts | 469 | KEEP | T05b | not started |  |
| legacyCliLauncher.ts | 42 | CUT | Electron shell / desktop-only infra |  |  |
| logging.ts | 265 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/logging.rs` |
| markdownGithubAlerts.ts | 72 | KEEP | T05a | ported | `T16-thread-view:app/src/components/chat/markdown/markdown_pipeline.rs` |
| markdownLinks.ts | 422 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/markdown_links.rs` |
| markdownListIndentation.ts | 143 | KEEP | T05a | partial | `T16-thread-view:app/src/components/chat/markdown/markdown_pipeline.rs` |
| markdownPipeline.ts | 293 | KEEP | T05a | ported | `T16-thread-view:app/src/components/chat/markdown/markdown_pipeline.rs` |
| mcpApp.ts | 286 | CUT | MCP apps / orchestrator MCP server |  |  |
| mcpResponsePayloads.ts | 65 | CUT | MCP apps / orchestrator MCP server |  |  |
| model.ts | 529 | KEEP | T05b | partial | `T14-client-state:app/src/state/support/model.rs` |
| nodeRuntime.ts | 111 | KEEP | T05a | not started |  |
| nodeSqliteClient.ts | 914 | KEEP | T02 | partial | `main:persistence/src/db.rs` |
| oauthScope.ts | 75 | CUT | T3 Connect / relay / remote access / auth |  |  |
| observability.ts | 822 | CUT | Telemetry / observability / resource telemetry |  |  |
| orchestrationTiming.ts | 88 | KEEP | T05b | ported | `T16-logic:app/src/components/chat/timeline/orchestration_timing.rs` |
| orchestrationV2BoundedSnapshot.ts | 58 | KEEP | T05b | not started |  |
| orchestrationV2PendingBackgroundWork.ts | 303 | KEEP | T05b | ported | `T13a-orch-projections:orchestration/src/projection/shared/pending_background_work.rs` |
| orchestrationV2ThreadError.ts | 124 | KEEP | T05b | ported | `T13a-orch-projections:orchestration/src/projection/shared/thread_error.rs` |
| orchestrationV2Timeline.ts | 112 | KEEP | T05b | ported | `T13a-orch-projections:orchestration/src/projection/shared/timeline.rs` |
| otelEnvironment.ts | 427 | CUT | Telemetry / observability / resource telemetry |  |  |
| path.ts | 123 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/path.rs` |
| preview.ts | 166 | CUT | Browser / preview / devices |  |  |
| previewViewport.ts | 175 | CUT | Browser / preview / devices |  |  |
| projectFavicon.ts | 46 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/project_favicon.rs` |
| projectScripts.ts | 88 | KEEP | T05b | partial | `T05b-shared-contracts:hooks/src/config.rs` |
| projectSettings.ts | 300 | KEEP | T05b | not started |  |
| promptLinks.ts | 249 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/prompt_links.rs` |
| proposedPlanText.ts | 20 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/proposed_plan_text.rs` |
| providerAuthReturnUrl.ts | 34 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/provider_auth_return_url.rs` |
| qrCode.ts | 985 | CUT | QR pairing (T3 Connect) |  |  |
| relayAuth.ts | 89 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayClient.ts | 651 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayJwt.ts | 103 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relaySigning.ts | 17 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayTracing.ts | 193 | CUT | T3 Connect / relay / remote access / auth |  |  |
| relayUrl.ts | 22 | CUT | T3 Connect / relay / remote access / auth |  |  |
| remote.ts | 245 | CUT | T3 Connect / relay / remote access / auth |  |  |
| schemaJson.ts | 276 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/schema_json.rs` |
| schemaYaml.ts | 88 | KEEP | T05a | not started |  |
| searchRanking.ts | 192 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/search_ranking.rs` |
| semver.ts | 234 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/semver.rs` |
| serverSettings.ts | 699 | KEEP | T05b | not started |  |
| shell.ts | 858 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/shell.rs` |
| sourceControl.ts | 323 | KEEP | T05b | partial | `T07-source-control:source-control/src/shared/source_control.rs` |
| symlink.ts | 49 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/symlink.rs` |
| t3McpToolPresentation.ts | 411 | KEEP | T16 | ported | `T16-logic:app/src/components/chat/timeline/t3_mcp_tool_presentation.rs` |
| t3ProjectFile.ts | 45 | CUT | t3 project file (contracts `t3ProjectFile` cut) |  |  |
| terminalLabels.ts | 49 | CUT | Terminal / PTY |  |  |
| themePalettes.ts | 954 | KEEP | T03 | ported | `main:app/src/theme/theme_palettes.rs` |
| themePreview.ts | 145 | KEEP | T03 | ported | `main:app/src/theme/theme_preview.rs` |
| thirdPartyLicenses.ts | 127 | CUT | Open-source licenses page (web build artifact) |  |  |
| threadFindText.ts | 233 | KEEP | T05b | not started |  |
| threadLinks.ts | 73 | KEEP | T05b | not started |  |
| threadPullRequests.ts | 370 | KEEP | T05b | ported | `T07-source-control:source-control/src/shared/thread_pull_requests.rs` |
| threadReference.ts | 40 | KEEP | T05b | not started |  |
| threadSearch.ts | 70 | KEEP | T05a | ported | `T05a-shared-pure:shared/src/thread_search.rs` |
| toolActivity.ts | 481 | KEEP | T05b | ported | `T16-logic:app/src/components/chat/timeline/tool_activity.rs` |
| toolOutput.ts | 300 | KEEP | T05b | ported | `T16-logic:app/src/components/chat/timeline/tool_output.rs` |
| usageFormat.ts | 263 | KEEP | T05b | ported | `T16-logic:app/src/components/chat/timeline/usage_format.rs` |
| usageLimits.ts | 803 | KEEP | T05b | not started |  |
| usageMerge.ts | 645 | KEEP | T05b | not started |  |
| video.ts | 29 | CUT | Browser / preview / devices |  |  |

### `packages/source-control-azure-devops/src/client/`

1 files, 73 lines: KEEP 0 / 0, CUT 1 / 73, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 73 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-azure-devops/src/server/`

8 files, 3,307 lines: KEEP 0 / 0, CUT 8 / 3,307, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| AzureDevOpsCli.ts | 554 | CUT | Non-GitHub source-control host |  |  |
| AzureDevOpsPullRequestCli.ts | 779 | CUT | Non-GitHub source-control host |  |  |
| AzureDevOpsPullRequestProvider.ts | 643 | CUT | Non-GitHub source-control host |  |  |
| AzureDevOpsSourceControlProvider.ts | 238 | CUT | Non-GitHub source-control host |  |  |
| azureDevOpsDiff.ts | 273 | CUT | Non-GitHub source-control host |  |  |
| azureDevOpsPullRequestJson.ts | 574 | CUT | Non-GitHub source-control host |  |  |
| azureDevOpsPullRequests.ts | 226 | CUT | Non-GitHub source-control host |  |  |
| driver.ts | 20 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-bitbucket/src/client/`

1 files, 101 lines: KEEP 0 / 0, CUT 1 / 101, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 101 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-bitbucket/src/server/`

7 files, 3,478 lines: KEEP 0 / 0, CUT 7 / 3,478, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| BitbucketApi.ts | 1,185 | CUT | Non-GitHub source-control host |  |  |
| BitbucketPullRequestApi.ts | 943 | CUT | Non-GitHub source-control host |  |  |
| BitbucketPullRequestProvider.ts | 386 | CUT | Non-GitHub source-control host |  |  |
| BitbucketSourceControlProvider.ts | 199 | CUT | Non-GitHub source-control host |  |  |
| bitbucketPullRequestJson.ts | 635 | CUT | Non-GitHub source-control host |  |  |
| bitbucketPullRequests.ts | 109 | CUT | Non-GitHub source-control host |  |  |
| driver.ts | 21 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-core/src/client/`

1 files, 324 lines: KEEP 1 / 324, CUT 0 / 0, UNASSIGNED 0 / 0, ported 1 / 324.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 324 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/definition.rs` |

### `packages/source-control-core/src/server/`

9 files, 1,840 lines: KEEP 9 / 1,840, CUT 0 / 0, UNASSIGNED 0 / 0, ported 8 / 1,711.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| PullRequestProvider.ts | 722 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/pull_request_provider.rs` |
| SourceControlHost.ts | 126 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/host.rs` |
| SourceControlProvider.ts | 197 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/source_control_provider.rs` |
| SourceControlRateLimit.ts | 181 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/rate_limit.rs` |
| diffFileRevisions.ts | 129 | KEEP | T07 | not started |  |
| discovery.ts | 340 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/discovery.rs` |
| driver.ts | 36 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/driver.rs` |
| mergeMessage.ts | 54 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/merge_message.rs` |
| pullRequestChecks.ts | 55 | KEEP | T07 | ported | `T07-source-control:source-control/src/source_control_core/pull_request_checks.rs` |

### `packages/source-control-forgejo/src/client/`

1 files, 106 lines: KEEP 0 / 0, CUT 1 / 106, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 106 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-forgejo/src/server/`

6 files, 2,121 lines: KEEP 0 / 0, CUT 6 / 2,121, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| ForgejoCli.ts | 723 | CUT | Non-GitHub source-control host |  |  |
| ForgejoPullRequestProvider.ts | 617 | CUT | Non-GitHub source-control host |  |  |
| ForgejoSourceControlProvider.ts | 450 | CUT | Non-GitHub source-control host |  |  |
| driver.ts | 21 | CUT | Non-GitHub source-control host |  |  |
| forgejoPullRequestJson.ts | 262 | CUT | Non-GitHub source-control host |  |  |
| forgejoPullRequests.ts | 48 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-gitcafe/src/client/`

1 files, 90 lines: KEEP 0 / 0, CUT 1 / 90, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 90 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-gitcafe/src/server/`

7 files, 2,438 lines: KEEP 0 / 0, CUT 7 / 2,438, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| GitCafeApi.ts | 176 | CUT | Non-GitHub source-control host |  |  |
| GitCafeCredentials.ts | 206 | CUT | Non-GitHub source-control host |  |  |
| GitCafePullRequestProvider.ts | 869 | CUT | Non-GitHub source-control host |  |  |
| GitCafeSourceControlProvider.ts | 539 | CUT | Non-GitHub source-control host |  |  |
| driver.ts | 21 | CUT | Non-GitHub source-control host |  |  |
| gitCafeHosts.ts | 49 | CUT | Non-GitHub source-control host |  |  |
| gitCafePullRequestJson.ts | 578 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-github/src/client/`

1 files, 99 lines: KEEP 1 / 99, CUT 0 / 0, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 99 | KEEP | T07 | partial | `T07-source-control:source-control/src/github/definition.rs` |

### `packages/source-control-github/src/server/`

16 files, 10,490 lines: KEEP 16 / 10,490, CUT 0 / 0, UNASSIGNED 0 / 0, ported 14 / 9,801.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| GitHubApi.ts | 587 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/api.rs` |
| GitHubCredentials.ts | 271 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/credentials.rs` |
| GitHubPullRequestApi.ts | 2,857 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/pull_request_api/mod.rs` |
| GitHubPullRequestProvider.ts | 668 | KEEP | T07 | not started |  |
| GitHubQuota.ts | 123 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/quota.rs` |
| GitHubSourceControlProvider.ts | 1,099 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/source_control_provider.rs` |
| driver.ts | 21 | KEEP | T07 | not started |  |
| gitHubAuthStatus.ts | 103 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/auth_status.rs` |
| gitHubChangeRequestTemplate.ts | 183 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/change_request_template.rs` |
| gitHubConditionalChecks.ts | 195 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/conditional_checks.rs` |
| gitHubPullRequestJson.ts | 3,504 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/pull_request_json/mod.rs` |
| gitHubPullRequests.ts | 145 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/pull_requests.rs` |
| gitHubRepositoryResolution.ts | 192 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/repository_resolution.rs` |
| githubGraphQl.ts | 100 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/graphql.rs` |
| githubStackActions.ts | 287 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/stack_actions.rs` |
| githubStackRebase.ts | 155 | KEEP | T07 | ported | `T07-source-control:source-control/src/github/stack_rebase.rs` |

### `packages/source-control-gitlab/src/client/`

1 files, 57 lines: KEEP 0 / 0, CUT 1 / 57, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| definition.ts | 57 | CUT | Non-GitHub source-control host |  |  |

### `packages/source-control-gitlab/src/server/`

8 files, 4,066 lines: KEEP 0 / 0, CUT 8 / 4,066, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| GitLabCli.ts | 660 | CUT | Non-GitHub source-control host |  |  |
| GitLabPullRequestCli.ts | 1,495 | CUT | Non-GitHub source-control host |  |  |
| GitLabPullRequestProvider.ts | 335 | CUT | Non-GitHub source-control host |  |  |
| GitLabSourceControlProvider.ts | 309 | CUT | Non-GitHub source-control host |  |  |
| driver.ts | 20 | CUT | Non-GitHub source-control host |  |  |
| gitLabAuthStatus.ts | 48 | CUT | Non-GitHub source-control host |  |  |
| gitLabMergeRequestJson.ts | 1,038 | CUT | Non-GitHub source-control host |  |  |
| gitLabMergeRequests.ts | 161 | CUT | Non-GitHub source-control host |  |  |

### `packages/ssh/src/`

5 files, 2,711 lines: KEEP 0 / 0, CUT 5 / 2,711, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| auth.ts | 215 | CUT | T3 Connect / relay / remote access / auth |  |  |
| command.ts | 368 | CUT | T3 Connect / relay / remote access / auth |  |  |
| config.ts | 269 | CUT | T3 Connect / relay / remote access / auth |  |  |
| errors.ts | 47 | CUT | T3 Connect / relay / remote access / auth |  |  |
| tunnel.ts | 1,812 | CUT | T3 Connect / relay / remote access / auth |  |  |

### `packages/tailscale/src/`

2 files, 411 lines: KEEP 0 / 0, CUT 2 / 411, UNASSIGNED 0 / 0, ported 0 / 0.

| File | Lines | Class | Task / Reason | Status | Turin module |
|---|---:|---|---|---|---|
| index.ts | 1 | CUT | T3 Connect / relay / remote access / auth |  |  |
| tailscale.ts | 410 | CUT | T3 Connect / relay / remote access / auth |  |  |
