//! Port of upstream `ThreadFind.ts` (`findProjectedThreadItems`) and the
//! message text extraction of `shared/threadFindText.ts`.
//!
//! The matcher counts occurrences of a query in the text a message *renders*,
//! so its segments follow the Markdown renderer: block elements split
//! segments, inline wrappers do not, file links show their chip label,
//! assistant citations their label, and `$skill` tokens their display name.
//!
//! Turin differences (PORTING.md Deviations): the Markdown is parsed with
//! `markdown-rs` (GFM) instead of the remark/rehype plugin chain, so Codex
//! inline directives and artifact-template cards are not rendered into
//! segments; and no scan cache is kept (every search reads the projection).

use markdown::mdast::Node;
use markdown::{ParseOptions, to_mdast};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProjectedTurnItem, OrchestrationV2SearchThreadInput,
    OrchestrationV2SearchThreadResult, OrchestrationV2SearchThreadSkill,
    OrchestrationV2ThreadFindMatch, OrchestrationV2ThreadFindNavigation, OrchestrationV2TurnItem,
};
use turin_contracts::{NonNegativeInt, RunId, TrimmedNonEmptyString};
use turin_shared::assistant_citations::{assistant_citation_label, parse_assistant_citation_href};
use turin_shared::composer_context_legacy::upgrade_legacy_context_message;
use turin_shared::composer_context_references::parse_composer_context_href;
use turin_shared::file_links::{
    build_file_link_parent_suffix_by_path, file_link_label, split_file_path_position,
};
use turin_shared::inline_skills::{
    InlineSkill, format_provider_skill_display_name, match_inline_skills,
};
use turin_shared::markdown_links::{
    extract_inline_code_spans, extract_markdown_link_hrefs, inline_code_file_path_candidate,
    is_markdown_file_link_label, resolve_markdown_file_link_target,
};
use turin_shared::proposed_plan_text::{proposed_plan_title, strip_displayed_plan_markdown};
use turin_shared::thread_search::count_thread_search_occurrences;

/// `assistant` rendering keeps hard line breaks only for Claude insight blocks.
fn should_preserve_assistant_line_breaks(text: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix("★ Insight")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('─'))
    })
}

struct TextWalk<'a> {
    user_message: bool,
    line_breaks: bool,
    cwd: Option<&'a str>,
    skills: &'a [InlineSkill],
    parent_suffixes: std::collections::HashMap<String, String>,
    segments: Vec<String>,
    text: String,
}

impl TextWalk<'_> {
    fn flush(&mut self) {
        if !self.text.trim().is_empty() {
            self.segments.push(std::mem::take(&mut self.text));
        } else {
            self.text.clear();
        }
    }

    fn label_text(node: &Node) -> String {
        match node {
            Node::Text(text) => text.value.clone(),
            Node::InlineCode(code) => code.value.clone(),
            other => other
                .children()
                .map(|children| children.iter().map(Self::label_text).collect())
                .unwrap_or_default(),
        }
    }

    fn push_text(&mut self, value: &str, in_pre: bool, inline_skills: bool) {
        let mut value = value.to_owned();
        if inline_skills && !self.skills.is_empty() {
            let mut cursor = 0;
            let mut rendered = String::new();
            for found in match_inline_skills(&value, self.skills) {
                rendered.push_str(&value[cursor..found.start]);
                rendered.push_str(&format_provider_skill_display_name(found.skill));
                cursor = found.end;
            }
            rendered.push_str(&value[cursor..]);
            value = rendered;
        }
        if in_pre {
            self.text.push_str(&value);
            return;
        }
        // `breaks` turns each soft newline into a `<br>`, which ends a segment.
        let mut lines = value.split('\n').peekable();
        while let Some(line) = lines.next() {
            self.text.push_str(&line.replace('\r', ""));
            if lines.peek().is_some() {
                if self.line_breaks {
                    self.flush();
                } else {
                    self.text.push(' ');
                }
            }
        }
    }

    /// Link and inline-code file chips: the chip's visible label.
    fn file_chip(&mut self, node: &Node, candidate: Option<String>) -> bool {
        let target = candidate.as_deref().and_then(|candidate| {
            resolve_markdown_file_link_target(Some(candidate), self.cwd, None)
        });
        let Some(target) = target else {
            return false;
        };
        if let (Node::Link(_), Some(candidate)) = (node, &candidate) {
            let label = Self::label_text(node);
            if !is_markdown_file_link_label(&label, candidate) {
                self.text.push_str(&label);
                self.text.push(' ');
            }
        }
        let position = split_file_path_position(&target, "");
        self.text
            .push_str(&file_link_label(&position, &self.parent_suffixes));
        true
    }

    fn visit(&mut self, node: &Node, in_pre: bool, inline_skills: bool) {
        let href = match node {
            Node::Link(link) => Some(link.url.as_str()),
            Node::Image(image) => Some(image.url.as_str()),
            _ => None,
        };
        if self.user_message
            && let Some(href) = href
            && parse_composer_context_href(href).is_some()
        {
            self.flush();
            return;
        }
        if let (Node::Link(_), Some(href)) = (node, href)
            && let Some(citation) = parse_assistant_citation_href(href)
        {
            // Matches the citation chip, which shows its label instead of the link text.
            self.text.push_str(&assistant_citation_label(&citation));
            return;
        }
        let candidate = match node {
            Node::Link(link) => Some(link.url.clone()),
            Node::InlineCode(code) if !in_pre => inline_code_file_path_candidate(&code.value),
            _ => None,
        };
        if candidate.is_some() && self.file_chip(node, candidate) {
            return;
        }
        let block = matches!(
            node,
            Node::Paragraph(_)
                | Node::Heading(_)
                | Node::Blockquote(_)
                | Node::List(_)
                | Node::ListItem(_)
                | Node::Code(_)
                | Node::ThematicBreak(_)
                | Node::Table(_)
                | Node::TableRow(_)
                | Node::TableCell(_)
                | Node::Break(_)
                | Node::FootnoteDefinition(_)
        );
        if block {
            self.flush();
        }
        match node {
            Node::Text(text) => self.push_text(&text.value, in_pre, inline_skills),
            Node::InlineCode(code) => self.push_text(&code.value, in_pre, false),
            Node::Code(code) => self.push_text(&code.value, true, false),
            // Raw HTML is text only for user messages (the renderer shows it literally).
            Node::Html(html) if self.user_message => self.push_text(&html.value, in_pre, false),
            Node::Definition(_) | Node::Image(_) | Node::Html(_) => {}
            other => {
                let render_skills = match other {
                    Node::Link(_) => false,
                    Node::Paragraph(_) | Node::ListItem(_) => true,
                    _ => inline_skills,
                };
                if let Some(children) = other.children() {
                    for child in children {
                        self.visit(
                            child,
                            in_pre || matches!(other, Node::Code(_)),
                            render_skills,
                        );
                    }
                }
            }
        }
        if block {
            self.flush();
        }
    }
}

/// `markdownThreadFindText`.
fn markdown_thread_find_text(
    markdown: &str,
    user_message: bool,
    cwd: Option<&str>,
    skills: &[InlineSkill],
    line_breaks: bool,
) -> Vec<String> {
    let Ok(tree) = to_mdast(markdown, &ParseOptions::gfm()) else {
        return vec![markdown.to_owned()];
    };
    let paths: Vec<String> = extract_markdown_link_hrefs(markdown)
        .into_iter()
        .chain(
            extract_inline_code_spans(markdown)
                .iter()
                .filter_map(|span| inline_code_file_path_candidate(span)),
        )
        .filter_map(|href| resolve_markdown_file_link_target(Some(&href), cwd, None))
        .map(|target| split_file_path_position(&target, "").path)
        .collect();
    let mut walk = TextWalk {
        user_message,
        // User messages always break on newlines; assistants only when asked.
        line_breaks: user_message || line_breaks,
        cwd,
        skills,
        parent_suffixes: build_file_link_parent_suffix_by_path(&paths),
        segments: Vec::new(),
        text: String::new(),
    };
    walk.visit(&tree, false, false);
    walk.flush();
    walk.segments
}

/// `searchablePlanSegments`: plans render `$skill` tokens literally.
pub fn searchable_plan_segments(markdown: &str, cwd: Option<&str>) -> Vec<String> {
    let mut segments =
        vec![proposed_plan_title(markdown).unwrap_or_else(|| "Proposed plan".to_owned())];
    segments.extend(markdown_thread_find_text(
        &strip_displayed_plan_markdown(markdown),
        false,
        cwd,
        &[],
        false,
    ));
    segments
}

/// `searchableMessageSegments`. `None` for roles that are not searched.
pub fn searchable_message_segments(
    user: bool,
    text: &str,
    streaming: bool,
    has_context: bool,
    cwd: Option<&str>,
    skills: &[InlineSkill],
) -> Vec<String> {
    if user {
        let text = if has_context {
            text.to_owned()
        } else {
            upgrade_legacy_context_message(text).text
        };
        return markdown_thread_find_text(&text, true, cwd, skills, true);
    }
    let shown = if text.is_empty() && !streaming {
        "(empty response)"
    } else {
        text
    };
    markdown_thread_find_text(
        shown,
        false,
        cwd,
        skills,
        should_preserve_assistant_line_breaks(text),
    )
}

/// One searchable row: its identity and match count.
#[derive(Debug, Clone)]
struct FindDocument {
    entry_id: String,
    run_id: Option<RunId>,
    count: usize,
}

fn searchable_rows(
    items: &[OrchestrationV2ProjectedTurnItem],
) -> Vec<&OrchestrationV2ProjectedTurnItem> {
    let folded_answers: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|row| match &row.item {
            OrchestrationV2TurnItem::UserInputRequest {
                question_answer: Some(answer),
                ..
            } => Some(format!("async-answer:{}", answer.request_id)),
            _ => None,
        })
        .collect();
    items
        .iter()
        .filter(|row| match &row.item {
            OrchestrationV2TurnItem::UserMessage { message_id, .. } => {
                !folded_answers.contains(message_id.as_str())
            }
            OrchestrationV2TurnItem::AssistantMessage { .. }
            | OrchestrationV2TurnItem::ProposedPlan { .. } => true,
            _ => false,
        })
        .collect()
}

fn document_for(
    row: &OrchestrationV2ProjectedTurnItem,
    cwd: Option<&str>,
    skills: &[InlineSkill],
    query: &str,
) -> FindDocument {
    let (entry_id, segments) = match &row.item {
        OrchestrationV2TurnItem::UserMessage {
            message_id,
            text,
            context,
            ..
        } => (
            message_id.to_string(),
            searchable_message_segments(true, text, false, context.is_some(), cwd, skills),
        ),
        OrchestrationV2TurnItem::AssistantMessage {
            message_id,
            text,
            streaming,
            ..
        } => (
            message_id.to_string(),
            searchable_message_segments(false, text, *streaming, false, cwd, skills),
        ),
        OrchestrationV2TurnItem::ProposedPlan { id, markdown, .. } => {
            (id.to_string(), searchable_plan_segments(markdown, cwd))
        }
        other => (
            turin_provider_core::turn_item::turn_item_common(other)
                .id
                .to_string(),
            Vec::new(),
        ),
    };
    let count = segments
        .iter()
        .map(|text| count_thread_search_occurrences(text, query))
        .sum();
    FindDocument {
        entry_id,
        run_id: turin_provider_core::turn_item::turn_item_common(&row.item)
            .run_id
            .cloned(),
        count,
    }
}

struct Selection {
    total_matches: usize,
    active_index: usize,
    document: Option<usize>,
    occurrence: usize,
}

/// `selectMatch`.
fn select_match(documents: &[FindDocument], input: &OrchestrationV2SearchThreadInput) -> Selection {
    let total_matches: usize = documents.iter().map(|doc| doc.count).sum();
    let index = input.index.map(|index| index as usize);
    let mut requested_index = index.unwrap_or(0);
    if index.is_none()
        && let Some(start) = &input.start
        && let Some(start_index) = documents
            .iter()
            .position(|doc| doc.entry_id == start.entry_id.as_str())
    {
        requested_index = documents[..start_index]
            .iter()
            .map(|doc| doc.count)
            .sum::<usize>()
            + (start.occurrence as usize).min(documents[start_index].count);
        if requested_index >= total_matches {
            requested_index = 0;
        }
    }
    let active_index = if index.is_none() && total_matches > 0 {
        let total = total_matches as i64;
        (requested_index as i64 + input.offset.unwrap_or(0)).rem_euclid(total) as usize
    } else {
        requested_index.min(total_matches.saturating_sub(1))
    };
    let mut occurrence = active_index;
    for (position, document) in documents.iter().enumerate() {
        if occurrence < document.count {
            return Selection {
                total_matches,
                active_index,
                document: Some(position),
                occurrence,
            };
        }
        occurrence -= document.count;
    }
    Selection {
        total_matches,
        active_index,
        document: None,
        occurrence: 0,
    }
}

fn non_negative(value: usize) -> NonNegativeInt {
    value as NonNegativeInt
}

fn entry_id(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("an entry id is non-empty")
}

/// `resultForSelection`.
fn result_for_selection(
    selected: &Selection,
    documents: &[FindDocument],
    snapshot_sequence: u64,
) -> OrchestrationV2SearchThreadResult {
    let mut start_index = 0;
    let mut entries = Vec::new();
    for document in documents {
        if document.count > 0 {
            entries.push((document, start_index));
        }
        start_index += document.count;
    }
    let selected_entry = selected.document.and_then(|position| {
        entries
            .iter()
            .position(|(document, _)| document.entry_id == documents[position].entry_id)
    });
    let pick = |(document, start): &(&FindDocument, usize)| OrchestrationV2ThreadFindNavigation {
        entry_id: entry_id(&document.entry_id),
        run_id: document.run_id.clone(),
        start_index: non_negative(*start),
        count: non_negative(document.count),
    };
    let navigation: Vec<_> = if entries.len() <= 17 {
        entries.iter().map(pick).collect()
    } else {
        // A window of 17 around the selection, wrapping.
        let center = selected_entry.map_or(-8, |index| index as i64 - 8);
        (0..17)
            .map(|offset| {
                let index = (center + offset).rem_euclid(entries.len() as i64) as usize;
                pick(&entries[index])
            })
            .collect()
    };
    OrchestrationV2SearchThreadResult {
        complete: None,
        snapshot_sequence,
        total_matches: non_negative(selected.total_matches),
        active_index: non_negative(selected.active_index),
        navigation: Some(navigation),
        r#match: selected
            .document
            .map(|position| OrchestrationV2ThreadFindMatch {
                entry_id: entry_id(&documents[position].entry_id),
                run_id: documents[position].run_id.clone(),
                occurrence: non_negative(selected.occurrence),
            }),
    }
}

fn inline_skills(input: &OrchestrationV2SearchThreadInput) -> Vec<InlineSkill> {
    input
        .skills
        .iter()
        .flatten()
        .map(|skill: &OrchestrationV2SearchThreadSkill| InlineSkill {
            name: skill.name.clone(),
            display_name: skill.display_name.clone(),
        })
        .collect()
}

/// The frames of `searchThreadStream`: for an initial search (no index or
/// offset) an early frame with the first match at or after the reading anchor,
/// then the final frame with the totals. Everything else is the final frame.
pub fn find_projected_thread_items(
    items: &[OrchestrationV2ProjectedTurnItem],
    input: &OrchestrationV2SearchThreadInput,
    snapshot_sequence: u64,
    cwd: Option<&str>,
) -> Vec<OrchestrationV2SearchThreadResult> {
    let skills = inline_skills(input);
    let query = input.query.as_str();
    let rows = searchable_rows(items);
    let documents: Vec<FindDocument> = rows
        .iter()
        .map(|row| document_for(row, cwd, &skills, query))
        .collect();
    let final_frame = result_for_selection(
        &select_match(&documents, input),
        &documents,
        snapshot_sequence,
    );
    let mut frames = Vec::new();
    if input.index.is_none() && input.offset.unwrap_or(0) == 0 && !documents.is_empty() {
        let anchor = input
            .start
            .as_ref()
            .and_then(|start| {
                documents
                    .iter()
                    .position(|doc| doc.entry_id == start.entry_id.as_str())
            })
            .unwrap_or(0);
        for step in 0..=documents.len() {
            let position = (anchor + step) % documents.len();
            let document = &documents[position];
            let occurrence = match &input.start {
                Some(start) if step == 0 && document.entry_id == start.entry_id.as_str() => {
                    (start.occurrence as usize).min(document.count)
                }
                _ => 0,
            };
            if occurrence >= document.count {
                continue;
            }
            frames.push(OrchestrationV2SearchThreadResult {
                complete: Some(false),
                snapshot_sequence,
                total_matches: non_negative(1),
                active_index: non_negative(0),
                navigation: None,
                r#match: Some(OrchestrationV2ThreadFindMatch {
                    entry_id: entry_id(&document.entry_id),
                    run_id: document.run_id.clone(),
                    occurrence: non_negative(occurrence),
                }),
            });
            break;
        }
    }
    frames.push(final_frame);
    frames
}

#[cfg(test)]
mod tests;
