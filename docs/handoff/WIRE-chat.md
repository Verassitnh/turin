# WIRE-chat handoff

Step 1 live wiring for the chat column and composer on routed threads (`LiveChatTarget`).

## Wired

- **Subagent bar**: `live_wiring::subagent_bar_from_state` from projection + provider catalog; demo bar only when `live` is absent (`chat_view.rs`).
- **Overlay banners**: provider status + thread error from projection/runtime; session dismiss keys per thread.
- **Usage limit recovery**: composer `BannerItem` from shell + runtime; **Resume/Snooze** → `Commands::thread_command("thread.metadata.update", { limitRecovery })` via `LiveSource::banner_action`.
- **Page scroll**: `TimelinePageScrollSession` + `ChatComposer::bind_page_scroll`; composer PageUp/Down drives `MessagesTimeline::apply_page_scroll_delta`.
- **Composer context**: `context_record_count`, `build_context_fragment` (copy), `Layer::ContextDetails` popover; wire→draft import via shared `Rc<RefCell<ComposerDraftContextRecords>>` + `composer_context_draft_from_record`.
- **Context chip in prompt**: `PromptEditorEvent::OpenContextReference` opens the details layer from the composer.
- **Citation comments**: composer citation chips → `open_citation_comment` / `request_citation_comment`; live assistant markdown selection → cite toolbar → `insert_assistant_citation_from_selection`.
- **Pending approvals/questions**: top drawer from timeline snapshot + `TimelineSource::respond_*` (live `LiveSource`); question attachment pickers on drawer cards (`question_attachment_drafts` global store).
- **Composer message-mode pending answers**: `UserInputResponseCapability::Message` → `ComposerViewModel::pending_answer` + `LiveSource::submit_composer_pending_answer` / `respond_to_pending_input`.
- **Thread navigation seam**: `thread_navigation::request_open_thread` / `take_thread_open_request` (shell should consume like `diff::take_turn_request`); `LiveSource::open_thread` enqueues.
- **Thread details / lineage panel**: `git_host.details_card()` when open (not demo card on live threads).
- **Changed files tree**: Pierre file icons via `ui::pierre_file_icon::file_tree_file_icon` (stack with `task/WIRE-pr-ui` if duplicated).

## Shell integration (outside owned paths)

| Seam | Consumer action |
|------|-----------------|
| **`take_thread_open_request`** | On tick or after navigation, open the returned thread id in the sidebar (same pattern as diff open requests). |
| **Question attachment uploads on respond** | Drawer/composer paths stage files locally; full upload queue parity with web may still need command-bus hooks when responding. |

## Owned paths

`chat_view.rs`, `components/chat/**`, `composer/**` wiring touched for Step 1 (no `main.rs` / shell edits in this branch).

## Commits

Follow-on commits on `task/WIRE-chat` with `Model: composer-2.5 (cursor)` trailer.
