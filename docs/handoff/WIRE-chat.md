# WIRE-chat handoff

Step 1 live wiring for the chat column and composer on routed threads (`LiveChatTarget`).

## Wired

- **Subagent bar**: `live_wiring::subagent_bar_from_state` from projection + provider catalog; demo bar only when `live` is absent (`chat_view.rs`).
- **Overlay banners**: provider status + thread error from projection/runtime; session dismiss keys per thread.
- **Usage limit recovery**: composer `BannerItem` from shell + runtime; **Resume/Snooze** → `Commands::thread_command("thread.metadata.update", { limitRecovery })` via `LiveSource::banner_action`.
- **Page scroll**: `TimelinePageScrollSession` + `ChatComposer::bind_page_scroll`; composer PageUp/Down drives `MessagesTimeline::apply_page_scroll_delta`.
- **Composer context**: `context_record_count`, `build_context_fragment` (copy), `Layer::ContextDetails` popover; host `set_context_records` + draft attachments merge in `composer_context_records()`.
- **Citation comments**: composer citation chips → `open_citation_comment` / `request_citation_comment` (existing path).
- **Pending approvals/questions**: top drawer from timeline snapshot + `TimelineSource::respond_*` (live `LiveSource`).
- **Thread details / lineage panel**: `git_host.details_card()` when open (not demo card on live threads).
- **Changed files tree**: Pierre file icons via `ui::pierre_file_icon::file_tree_file_icon` (stack with `task/WIRE-pr-ui` if duplicated).

## Seams (no hidden noops in owned UI)

| Area | Blocker |
|------|---------|
| **Assistant selection toolbar (live)** | GPUI markdown has no `captureAssistantTextSelection` / DOM observer; `assistant_text_selection.rs` is logic-only. Live threads omit demo toolbar. |
| **Cite → composer** | Needs selection capture → `AssistantCitation` + `ChatComposer::request_citation_comment` bridge. |
| **`LiveSource::open_thread`** | No shell global like `diff::request_open_turn_diff`; sidebar `navigate_to_thread` lives outside owned paths. |
| **Context paste import** | `PromptEditor::set_import_context_fragment(None)` until wire→draft merge uses `Rc<RefCell<ComposerDraftContextRecords>>` + `composer_context_records` reverse helpers. |
| **Context chip click in prompt** | `PromptEditor` has no `OpenContextReference` event; chips cannot open details popover from editor yet. |
| **Question attachments (C2)** | `state/question_attachments.rs` keys exist; no pending-question attachment UI in `composer_pending_panels.rs` (upstream `QuestionAttachment*` panels not ported). |
| **Composer `pending_answer` send** | Live questions use top drawer, not composer footer answer mode (`respond_to_pending_input` still `not_wired` for composer-only path). |
| **Usage limit banner dismiss** | Intentionally no-op (notice, not overlay). |

## Owned paths

`chat_view.rs`, `components/chat/**`, `composer/views/**` (composer wiring only).

## Commits

Retain `4a64a024` (Pierre changed-files icon). Follow-on commits on `task/WIRE-chat` with `Model: composer-2.5 (cursor)` trailer.
