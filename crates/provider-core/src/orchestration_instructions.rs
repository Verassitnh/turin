//! Port of upstream/packages/provider-core/src/server/orchestrationInstructions.ts.
//!
//! The prompt texts are verbatim. They describe the `t3-code` MCP server,
//! which Turin does not ship (orchestrator MCP is cut), so callers pass
//! `has_t3_mcp = false` and these never reach a provider; they are kept so a
//! future in-process MCP server can reuse them unchanged.

use turin_contracts::provider_policy::ProviderInteractionMode;

pub const T3_CODE_ORCHESTRATION_INSTRUCTIONS: &str = r#"

## T3 Code orchestration

The `t3-code` MCP server provides app-owned orchestration. Treat these concepts distinctly:

- A delegated task/subagent is child work owned by the current thread. Use `orchestrator_capabilities` to discover the current provider/model IDs from the same live catalog as the composer, including configured custom models. Do not treat a native tool's model list as the full list of available subagent models. Prefer native subagent tools for same-provider work only when they support the chosen model. Use `delegate_task` with that provider instance and model when native tools cannot, including for same-provider work. Also use `delegate_task` for cross-provider or explicitly T3-owned child tasks. For a child task that needs a separate checkout, use `delegate_task` with `isolation:"worktree"` across any provider or model. It starts from the parent local commit and runs project setup; uncommitted edits are not copied. Retain each returned `taskId`, and use `task_status` or `task_cancel` to manage it. The returned `childThreadId` is backing storage for the subagent, not the target for starting another delegated review round.
- `t3_thread_launch` and `create_threads` create ordinary top-level T3 conversations. Use them only when the user explicitly asks for separate/new/top-level threads or conversations. Never use them merely because the user said "subagent" or requested parallel delegated work.
- For every T3 delegated review round, call `delegate_task` again. Include the original brief, prior findings, responses, and unresolved objections in each new task prompt. Track each round by its own `taskId`. Use a distinct `clientRequestId` per round, stable across retries of that round. Do not use `t3_thread_send` on `childThreadId` to continue a delegated review.
- `schedule_task` creates persistent recurring work in the app scheduler. Pass `schedule` as a structured object, never as JSON text: `{"type":"interval","everyMs":3600000}` for an interval, or `{"type":"fixed_time","timeOfDay":"09:00","weekdays":[1,2,3,4,5]}` for a wall-clock schedule, or `{"type":"webhook"}` to run on each request to the returned `webhookUrl` (the run sees the request only through `{{body.path}}`-style placeholders in the prompt). By default runs return to the current thread, which suits orchestrating: each trigger arrives here and you delegate or dedupe; set `bindToCurrentThread=false` only when the user wants a fresh thread for every run. After scheduling a timer, report the returned cadence and next run time; for a webhook, report its `webhookUrl`, or say T3 Connect remote access is needed if it is missing.
- When you need a secret from the user (a token, API key, or webhook signing secret), call `request_secret` so they enter it privately, then pass the returned `secretRef` to the tool that needs it, e.g. `signature.secretRef` on a webhook task for a sender that signs requests such as GitHub. A `secretRef` works once. Never ask for a secret in chat, never invent one, and never repeat one.
- To mention another thread to the user, link it as `[title](t3-thread://v1/<threadId>)` with its exact `threadId`, not URL-encoded. T3 Code opens the thread in the app and shows its current title.

### Choose the workspace before starting a new thread

For independent implementation or a PR stack in its own worktree, use `t3_thread_launch` with an explicit `workspaceStrategy`. It creates or selects the workspace, binds the new thread to it, and prepares it before the agent starts. Put the task in `message`, not `prompt`:

- New worktree: `{"title":"UI cleanup","workspaceStrategy":{"type":"worktree","baseRef":"feature/base","branch":"feature/ui-cleanup","startFromOrigin":false},"message":"Implement the cleanup and open a PR against feature/base."}`
- Existing worktree: `{"title":"Continue cleanup","workspaceStrategy":{"type":"existing_worktree","worktreePath":"/absolute/path/to/worktree","branch":"feature/ui-cleanup"},"message":"Continue the cleanup."}`
- Project's main checkout: `workspaceStrategy:{"type":"root"}`. Omitting workspaceStrategy also selects root; it does not inherit the caller's worktree.

For stacked work, set `baseRef` to the intended parent branch and `startFromOrigin:false` to use its local commits. Use `startFromOrigin:true` when you intend to fetch and start from origin. Uncommitted edits are not copied. Use `t3_worktree_list` to discover existing checkout paths. Project, model selection, and modes inherit unless supplied; a launched thread may not run with broader modes than yours.

`t3_thread_launch` is the single-thread launch tool. Use `create_threads` only for a batch of threads intentionally sharing the caller's checkout: it always inherits the caller's project, branch, and worktree and has no workspace override. Asking an agent to run `git worktree add` or `cd` in its prompt does not update T3's thread binding. Select the workspace in the launch call instead. `t3_worktree_handoff` moves the calling thread, not another thread, and cannot move a thread already attached to a worktree.

`t3_thread_launch` has no idempotency key. Retain its returned threadId and inspect it with `t3_thread_read` / `t3_thread_wait`; preparation can still be running after acceptance. If a launch fails or its response is lost, inspect `t3_thread_list` before retrying, since a thread may already exist.

Tool names may include a harness-normalized MCP prefix, such as `mcp__t3_code__delegate_task`; the semantics are the same. Some harnesses attach optional MCP servers lazily: if an initial tool-catalog scan does not show T3 tools, do not conclude that cross-provider delegation is unavailable. Make one bounded direct attempt using the known T3 tool name on the next tool step. In Codex code mode, for example, call `tools.mcp__t3_code__orchestrator_capabilities({})` before reporting that the capability is absent. Keep polling/wait loops bounded, do not duplicate active work, and use stable `clientRequestId` values when retrying tools that accept them.

ACP fallback: some ACP agents accept the injected MCP server but fail to expose its tools. When the T3 tools are absent and `T3_ACP_MCP_NODE` is present, call the same tools through the terminal: `ELECTRON_RUN_AS_NODE=1 "$T3_ACP_MCP_NODE" ${T3_ACP_MCP_ENTRYPOINT:+"$T3_ACP_MCP_ENTRYPOINT"} acp-mcp-call orchestrator_capabilities '{}'` (`T3_ACP_MCP_ENTRYPOINT` is unset when T3 runs as a standalone executable). Delegate with `acp-mcp-call delegate_task '{"task":"...","target":{"providerInstanceId":"...","model":"..."},"mode":"async","clientRequestId":"..."}'`. This is the supported T3 transport fallback, not an ordinary shell-based substitute for delegation.

### Showing visuals

When a chart, table, diagram, image collage, or mockup would say more than prose, build a self-contained HTML page, check it with `html_preview`, then publish it with `html_render` before your final reply. The reader sees the page above that reply, so don't announce or restate it; add only what it doesn't say.
"#;

pub const T3_CODE_BROWSER_TOOL_INSTRUCTIONS: &str = r#"

## T3 Code collaborative browser

You are running inside T3 Code. The `t3-code` MCP server is the product-native collaborative browser shared with the user. When it exposes `preview_*` tools, prefer those tools for browser navigation, inspection, interaction, screenshots, and recordings.

For browser work, first call `preview_status`. If no automation-capable preview is attached, call `preview_open` before concluding that the browser is unavailable. Then use `preview_navigate`, `preview_snapshot`, and the focused interaction tools. Prefer snapshot-provided locators over coordinates.

`preview_status` lists every browser tab in this thread, including tabs the user opened. When the user asks about "this page" or a page they have open, read their tab: pass its `tabId` to `preview_snapshot` or `preview_wait_for`, or omit `tabId` when you have no tab of your own. You may act on the user's tab, including `preview_evaluate`, only while its owner is `unclaimed`; while it is `human`, the user is driving, so read it with `preview_snapshot` or open your own tab. To use a browser profile (a set of saved logins), pass `profileId` from `preview_status` profiles to `preview_open`.

Do not switch to global browser skills, Chrome, Node REPL browser automation, standalone Playwright, or agent-browser merely because the preview is initially closed or a first call fails. Inspect a failed preview call and retry with corrected arguments when the error is actionable. Use another browser system when:
- the T3 preview tools are absent, or `preview_open` returns an explicit unsupported/unavailable error;
- the user asks for another browser, or invokes a skill or documented repository workflow that names one; follow that workflow and report any prerequisite it is missing;
- preview calls on an open tab have failed twice on the same step (timeouts, `chrome-error://` pages, a different client answering). Quote the raw error and switch without asking the user which browser to use.
"#;

const T3_CODE_ACP_DEFAULT_MODE_INSTRUCTIONS: &str = r#"## T3 Code interaction mode: Default

Prefer making reasonable assumptions and carrying out the user's request. Ask a concise question only when a missing user decision would materially change the result. Treat this mode as active until T3 Code supplies a different interaction-mode instruction."#;

const T3_CODE_ACP_PLAN_MODE_INSTRUCTIONS: &str = r#"## T3 Code interaction mode: Plan

Investigate with read-only actions and do not edit files or otherwise execute the implementation. Resolve discoverable facts before asking questions. When the requirements are decision complete, return a concrete implementation plan and do not start implementing it. Treat this mode as active until T3 Code supplies a different interaction-mode instruction."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct T3AcpInstructionState {
    pub interaction_mode: ProviderInteractionMode,
    pub has_t3_mcp: bool,
}

/// ACP has no system/developer prompt field, so send T3-owned context in the
/// first user prompt and whenever the available tools or interaction mode change.
pub fn t3_acp_prompt_with_instructions(
    prompt: &str,
    state: T3AcpInstructionState,
    previous_state: Option<T3AcpInstructionState>,
) -> String {
    // Native slash commands must remain at the start of the prompt.
    if crate::js::js_trim_start(prompt).starts_with('/') {
        return prompt.to_owned();
    }
    if previous_state == Some(state) {
        return prompt.to_owned();
    }
    let mut instructions = vec![if state.interaction_mode == ProviderInteractionMode::Plan {
        T3_CODE_ACP_PLAN_MODE_INSTRUCTIONS.to_owned()
    } else {
        T3_CODE_ACP_DEFAULT_MODE_INSTRUCTIONS.to_owned()
    }];
    if state.has_t3_mcp {
        instructions.push(js_trim(T3_CODE_BROWSER_TOOL_INSTRUCTIONS).to_owned());
        instructions.push(js_trim(T3_CODE_ORCHESTRATION_INSTRUCTIONS).to_owned());
    }
    format!(
        "<t3_code_instructions>\n{}\n</t3_code_instructions>\n\n<user_request>\n{prompt}\n</user_request>",
        instructions.join("\n\n")
    )
}

use turin_contracts::js_trim;

/// Providers without a system/developer-instruction channel receive this
/// context in the first prompt. Keep the wrapper explicit so it cannot be
/// mistaken for text authored by the user.
fn prepend_t3_orchestration_instructions(prompt: &str) -> String {
    format!(
        "<t3_code_orchestration_instructions>{}</t3_code_orchestration_instructions>\n\n<user_request>\n{prompt}\n</user_request>",
        js_trim(T3_CODE_ORCHESTRATION_INSTRUCTIONS)
    )
}

pub fn t3_orchestration_prompt_for_first_run(
    prompt: &str,
    run_ordinal: u64,
    has_t3_mcp: bool,
) -> String {
    if run_ordinal == 1 && has_t3_mcp {
        prepend_t3_orchestration_instructions(prompt)
    } else {
        prompt.to_owned()
    }
}

pub fn t3_orchestration_system_prompt(has_t3_mcp: bool) -> Option<&'static str> {
    has_t3_mcp.then_some(T3_CODE_ORCHESTRATION_INSTRUCTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_delegated_subagents_from_ordinary_top_level_threads() {
        for needle in [
            "Use `delegate_task`",
            "ordinary top-level T3 conversations",
            "Never use them merely",
            "cross-provider",
            "call `delegate_task` again",
            "Do not use `t3_thread_send` on `childThreadId`",
        ] {
            assert!(
                T3_CODE_ORCHESTRATION_INSTRUCTIONS.contains(needle),
                "{needle}"
            );
        }
    }

    #[test]
    fn documents_structured_schedules_instead_of_json_strings() {
        for needle in [
            "structured object, never as JSON text",
            "\"everyMs\":3600000",
            "bindToCurrentThread=false",
        ] {
            assert!(
                T3_CODE_ORCHESTRATION_INSTRUCTIONS.contains(needle),
                "{needle}"
            );
        }
    }

    #[test]
    fn keeps_template_escapes_decoded() {
        // `\${...}` in the TS template literal is a literal `${`.
        assert!(T3_CODE_ORCHESTRATION_INSTRUCTIONS.contains("${T3_ACP_MCP_ENTRYPOINT:+"));
        assert!(!T3_CODE_ORCHESTRATION_INSTRUCTIONS.contains("\\`"));
    }

    #[test]
    fn injects_prompt_fallback_only_for_an_mcp_enabled_first_run() {
        let prompt = "Inspect the repository.";
        let injected = t3_orchestration_prompt_for_first_run(prompt, 1, true);
        assert!(injected.contains("<t3_code_orchestration_instructions>"));
        assert!(injected.contains(&format!("<user_request>\n{prompt}\n</user_request>")));
        assert_eq!(
            t3_orchestration_prompt_for_first_run(prompt, 2, true),
            prompt
        );
        assert_eq!(
            t3_orchestration_prompt_for_first_run(prompt, 1, false),
            prompt
        );
    }

    #[test]
    fn only_exposes_the_system_prompt_when_the_mcp_server_is_attached() {
        assert_eq!(t3_orchestration_system_prompt(false), None);
        assert_eq!(
            t3_orchestration_system_prompt(true),
            Some(T3_CODE_ORCHESTRATION_INSTRUCTIONS)
        );
    }

    #[test]
    fn gives_acp_sessions_provider_neutral_mode_browser_and_orchestration_guidance() {
        let state = T3AcpInstructionState {
            interaction_mode: ProviderInteractionMode::Default,
            has_t3_mcp: true,
        };
        let injected = t3_acp_prompt_with_instructions("Inspect the repository.", state, None);
        for needle in [
            "T3 Code interaction mode: Default",
            "T3 Code collaborative browser",
            "T3 Code orchestration",
            "<user_request>\nInspect the repository.\n</user_request>",
        ] {
            assert!(injected.contains(needle), "{needle}");
        }
    }

    #[test]
    fn reinjects_acp_guidance_only_when_mode_or_tool_availability_changes() {
        let prompt = "Continue.";
        let default_state = T3AcpInstructionState {
            interaction_mode: ProviderInteractionMode::Default,
            has_t3_mcp: true,
        };
        assert_eq!(
            t3_acp_prompt_with_instructions(prompt, default_state, Some(default_state)),
            prompt
        );
        assert!(
            t3_acp_prompt_with_instructions(
                prompt,
                T3AcpInstructionState {
                    interaction_mode: ProviderInteractionMode::Plan,
                    ..default_state
                },
                Some(default_state)
            )
            .contains("T3 Code interaction mode: Plan")
        );
        let without_mcp = t3_acp_prompt_with_instructions(
            prompt,
            T3AcpInstructionState {
                interaction_mode: ProviderInteractionMode::Default,
                has_t3_mcp: false,
            },
            None,
        );
        assert!(without_mcp.contains("T3 Code interaction mode: Default"));
        assert!(!without_mcp.contains("T3 Code collaborative browser"));
        assert!(!without_mcp.contains("T3 Code orchestration"));
    }

    #[test]
    fn slash_commands_stay_at_the_start() {
        let state = T3AcpInstructionState {
            interaction_mode: ProviderInteractionMode::Default,
            has_t3_mcp: true,
        };
        assert_eq!(
            t3_acp_prompt_with_instructions("  /compact", state, None),
            "  /compact"
        );
    }
}
