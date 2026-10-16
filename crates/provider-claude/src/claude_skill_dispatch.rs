//! Port of upstream/apps/server/src/provider/Drivers/ClaudeSkillDispatch.ts:
//! turns `$skill` mentions in a composer prompt into the slash invocation
//! Claude Code actually runs.
//!
//! The composer inserts `$name` for every provider. Claude Code does not parse
//! that, and treats it as prose. Its only user-side invocation is a text block
//! whose first character is `/`: the harness expands `/name args` into the
//! SKILL.md body, and every character after the name arrives as `ARGUMENTS`.
//! Verified against the CLI in stream-json mode, which the Agent SDK uses:
//!
//! - The check runs on the LAST text block of the message. Earlier text blocks
//!   are preserved verbatim.
//! - Leading whitespace, or a `/name` that starts a later line, is literal text.
//! - Only one skill expands per message; a second `/x` becomes argument text
//!   (anthropics/claude-code#87113). Earlier mentions are therefore rewritten to
//!   `/name` inline, so the model can start them through its Skill tool.
//!
//! So one mention anywhere in the prompt becomes a guaranteed invocation, and
//! the user's text on either side is kept in order.
use fancy_regex::Regex;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::OnceLock};

/// Same token shape the composer and timeline chips recognise, so a rendered
/// chip and a dispatched skill are always the same set. `fancy-regex` is needed
/// for the lookaheads and the `\p{Sc}` currency class.
fn mention_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(^|\s)\p{Sc}(?![0-9][0-9_]*(?:[kKmMbBtT]|[eE][0-9]+)?(?:\s|$))(?=[a-zA-Z0-9:_-]*[a-zA-Z])([a-zA-Z0-9][a-zA-Z0-9:_-]*)(?=\s|$)",
        )
        .expect("mention pattern")
    })
}

/// `ClaudeSkillDispatch`: the prompt split around the last known mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSkillDispatch {
    /// Text before the dispatched mention, or `None` when it opens the prompt.
    pub leading_text: Option<String>,
    /// `/name` plus the trailing text, ready to be the message's last text block.
    pub command_text: String,
    pub skill_name: String,
}

struct Mention {
    name: String,
    start: usize,
    end: usize,
}

/// `planClaudeSkillDispatch`: splits `prompt` around the last `$skill` mention
/// that names a known skill. `None` means nothing to dispatch, and the prompt
/// goes out unchanged. A mention that names no discovered skill, such as a
/// `$HOME` in prose, stays literal.
pub fn plan_claude_skill_dispatch(
    prompt: &str,
    skill_names: &HashSet<String>,
) -> Option<ClaudeSkillDispatch> {
    let mentions: Vec<Mention> = mention_pattern()
        .captures_iter(prompt)
        .filter_map(|captures| captures.ok())
        .filter_map(|captures| {
            let whole = captures.get(0)?;
            let name = captures.get(2)?.as_str();
            if !skill_names.contains(name) {
                return None;
            }
            let lead = captures.get(1).map_or(0, |m| m.as_str().len());
            Some(Mention {
                name: name.to_owned(),
                start: whole.start() + lead,
                end: whole.end(),
            })
        })
        .collect();
    let last = mentions.last()?;

    let leading = &prompt[..last.start];
    let trailing = &prompt[last.end..];
    // Earlier mentions become inline `/name`, rewritten right to left so the
    // offsets of the ones still to rewrite stay valid.
    let mut leading_with_inline_slashes = leading.to_owned();
    for mention in mentions[..mentions.len() - 1].iter().rev() {
        leading_with_inline_slashes
            .replace_range(mention.start..mention.end, &format!("/{}", mention.name));
    }
    let leading_trimmed = leading_with_inline_slashes.trim_end();

    Some(ClaudeSkillDispatch {
        leading_text: (!leading_trimmed.is_empty()).then(|| leading_trimmed.to_owned()),
        command_text: format!("/{}{trailing}", last.name).trim_end().to_owned(),
        skill_name: last.name.clone(),
    })
}

/// The content blocks for a dispatched prompt: the leading text (if any), then
/// the `/name` block that must be last. `None` when nothing dispatches, and the
/// caller sends the prompt as plain text.
pub fn dispatch_content_blocks(prompt: &str, skill_names: &HashSet<String>) -> Option<Vec<Value>> {
    let dispatch = plan_claude_skill_dispatch(prompt, skill_names)?;
    let mut blocks = Vec::with_capacity(2);
    if let Some(leading) = dispatch.leading_text {
        blocks.push(json!({"type": "text", "text": leading}));
    }
    blocks.push(json!({"type": "text", "text": dispatch.command_text}));
    Some(blocks)
}

#[cfg(test)]
mod tests;
