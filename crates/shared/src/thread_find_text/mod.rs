//! Port of upstream/packages/shared/src/threadFindText.ts
//!
//! The text half of thread find: user, assistant and plan Markdown become the segments of visible
//! text that find indexes, and they have to match what the renderer shows. Upstream walks the hast
//! tree the chat renderer is built from, flushing a segment at every block element, so inline
//! wrappers never split a phrase. Citation chips show their label, file links show the file chip
//! label (with parent folders when names collide), artifact template cards show their name and
//! kind, `$skill` tokens show the skill's display name, and context chips are not indexed.
//!
//! The Markdown pipeline is reproduced over markdown-rs ([`to_hast`]) and, for assistant
//! messages, html5ever and a reduction of `rehype-sanitize` ([`hast`]). See PORTING.md
//! Deviations for where this departs from `unified`.

mod hast;
mod to_hast;

use std::collections::HashMap;

use turin_contracts::orchestration_v2::{
    OrchestrationV2ConversationMessage, OrchestrationV2ConversationMessageRole,
};

use self::hast::{Element, Hast};
use crate::assistant_citations::{assistant_citation_label, parse_assistant_citation_href};
use crate::codex_artifact_templates::{
    CodexArtifactTemplate, CodexArtifactTemplateAttributes,
    codex_artifact_template_presentation_label, resolve_codex_artifact_template,
};
use crate::codex_markdown_directives::{
    DirectiveMatch, DirectiveReplacement, collect_directive_matches,
    render_codex_inline_directives_as_markdown, to_mdast_guarded,
};
use crate::composer_context_legacy::upgrade_legacy_context_message;
use crate::composer_context_references::parse_composer_context_href;
use crate::file_links::{
    build_file_link_parent_suffix_by_path, file_link_label, split_file_path_position,
};
use crate::inline_skills::{InlineSkill, format_provider_skill_display_name, match_inline_skills};
use crate::markdown_github_alerts::remark_github_alerts;
use crate::markdown_links::{
    extract_inline_code_spans, extract_markdown_link_hrefs, inline_code_file_path_candidate,
    is_markdown_file_link_label, resolve_markdown_file_link_target,
};
use crate::markdown_list_indentation::remark_normalize_list_item_indentation;
use crate::proposed_plan_text::{proposed_plan_title, strip_displayed_plan_markdown};
use turin_contracts::base_schemas::{is_js_whitespace, js_trim};

/// Inline wrappers (including Shiki token spans) must not split a search phrase.
pub const THREAD_FIND_BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "th",
    "thead",
    "tr",
    "ul",
];

/// `THREAD_FIND_BLOCK_TAGS.has(tag)`.
pub fn is_thread_find_block_tag(tag: &str) -> bool {
    THREAD_FIND_BLOCK_TAGS.contains(&tag)
}

/// `shouldPreserveAssistantLineBreaks`: `/^★ Insight(?:\s|─)/mu`.
///
/// The `m` flag makes `^` match at the start of the text and after `\n`, `\r`, U+2028 and
/// U+2029 (not only after `\n`, which is all `str::lines` splits on), and `\s` includes line
/// terminators, so a bare `★ Insight` line followed by a break matches too.
pub fn should_preserve_assistant_line_breaks(text: &str) -> bool {
    const PREFIX: &str = "★ Insight";
    let mut at_line_start = true;
    let mut rest = text;
    while !rest.is_empty() {
        if at_line_start
            && let Some(after) = rest.strip_prefix(PREFIX)
            && (after.starts_with(is_js_whitespace) || after.starts_with('─'))
        {
            return true;
        }
        let Some(ch) = rest.chars().next() else { break };
        at_line_start = matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}');
        rest = &rest[ch.len_utf8()..];
    }
    false
}

/// `Pick<OrchestrationV2ConversationMessage, "role" | "text" | "streaming" | "context">`.
#[derive(Debug, Clone, Copy)]
pub struct SearchableMessage<'a> {
    pub role: OrchestrationV2ConversationMessageRole,
    pub text: &'a str,
    pub streaming: bool,
    /// Whether the message carries structured context (`message.context` is set).
    pub has_context: bool,
}

impl<'a> From<&'a OrchestrationV2ConversationMessage> for SearchableMessage<'a> {
    fn from(message: &'a OrchestrationV2ConversationMessage) -> Self {
        Self {
            role: message.role,
            text: &message.text,
            streaming: message.streaming,
            has_context: message.context.is_some(),
        }
    }
}

/// Plans render `$skill` tokens literally (no skill chips), so they are indexed as written.
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

/// The find segments of a message, or `None` for roles that are not indexed.
pub fn searchable_message_segments(
    message: &SearchableMessage<'_>,
    cwd: Option<&str>,
    skills: &[InlineSkill],
) -> Option<Vec<String>> {
    match message.role {
        OrchestrationV2ConversationMessageRole::User => {
            let upgraded;
            let text = if message.has_context {
                message.text
            } else {
                upgraded = upgrade_legacy_context_message(message.text).text;
                &upgraded
            };
            Some(markdown_thread_find_text(text, true, cwd, skills, false))
        }
        OrchestrationV2ConversationMessageRole::Assistant => {
            let text = if !message.text.is_empty() {
                message.text
            } else if message.streaming {
                ""
            } else {
                "(empty response)"
            };
            Some(markdown_thread_find_text(
                text,
                false,
                cwd,
                skills,
                should_preserve_assistant_line_breaks(message.text),
            ))
        }
        OrchestrationV2ConversationMessageRole::System => None,
    }
}

/// What the placeholders in the prepared Markdown stand for.
#[derive(Default)]
struct DirectiveSubstitutions {
    templates: Vec<CodexArtifactTemplate>,
    literals: Vec<String>,
}

/// Backslash-escapes every ASCII punctuation character, so the text parses as itself.
fn escape_markdown_punctuation(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() * 2);
    for ch in text.chars() {
        if ch.is_ascii_punctuation() {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// `source[start..end]` with the directives in it replaced. `gap` rewrites the text between them.
fn apply_directive_matches(
    source: &str,
    start: usize,
    end: usize,
    matches: &[DirectiveMatch],
    gap: &dyn Fn(&str) -> String,
    substitutions: &mut DirectiveSubstitutions,
) -> String {
    let mut ordered: Vec<&DirectiveMatch> = matches.iter().collect();
    ordered.sort_by_key(|directive| directive.start);
    let mut rendered = String::new();
    let mut cursor = start;
    for directive in ordered {
        if directive.start < cursor || directive.end > end {
            continue;
        }
        rendered.push_str(&gap(&source[cursor..directive.start]));
        rendered.push_str(&match &directive.replacement {
            DirectiveReplacement::Markdown(rendered) => rendered.clone(),
            // The label is inline Markdown of its own, with the directives in it rendered. Text
            // that would be a block on its own (a lone `*`) is escaped, and white space at the
            // ends is kept as character references, since a paragraph would trim it.
            DirectiveReplacement::LabelRange {
                start: label_start,
                end: label_end,
                nested,
            } => {
                let label = &source[*label_start..*label_end];
                let inline = matches!(
                    to_mdast_guarded(label, &to_hast::parse_options()),
                    Some(markdown::mdast::Node::Root(root))
                        if matches!(root.children.as_slice(), [markdown::mdast::Node::Paragraph(_)])
                );
                let label_gap: &dyn Fn(&str) -> String = if inline {
                    &|text| text.to_owned()
                } else {
                    &escape_markdown_punctuation
                };
                keep_edge_whitespace(&apply_directive_matches(
                    source,
                    *label_start,
                    *label_end,
                    nested,
                    label_gap,
                    substitutions,
                ))
            }
            // A one-line heading that is only a token: it ends where the directive did, and
            // `to_hast` turns it into the card.
            DirectiveReplacement::Template(template) => {
                substitutions.templates.push(template.clone());
                format!(
                    "# {}",
                    to_hast::template_token(substitutions.templates.len() - 1)
                )
            }
            // `restoreLeafDirective`: a paragraph of the directive's own text. The same heading
            // placeholder makes it interrupt the surrounding paragraph.
            DirectiveReplacement::Literal(text) => {
                substitutions.literals.push(text.clone());
                format!(
                    "# {}",
                    to_hast::literal_token(substitutions.literals.len() - 1)
                )
            }
            // `restoreTextDirective`: the node's own text, which parses as itself once escaped.
            DirectiveReplacement::InlineLiteral(text) => escape_markdown_punctuation(text),
        });
        cursor = directive.end;
    }
    rendered.push_str(&gap(&source[cursor..end]));
    rendered
}

/// Leading and trailing spaces and tabs as character references.
fn keep_edge_whitespace(text: &str) -> String {
    let reference = |ch: char| if ch == '\t' { "&#9;" } else { "&#32;" };
    let is_blank = |ch: char| ch == ' ' || ch == '\t';
    let body = text.trim_matches(is_blank);
    if body.is_empty() {
        return text.to_owned();
    }
    let leading = text.len() - text.trim_start_matches(is_blank).len();
    let trailing = text.len() - text.trim_end_matches(is_blank).len();
    let mut kept = String::with_capacity(text.len() + 4 * (leading + trailing));
    kept.extend(text[..leading].chars().map(reference));
    kept.push_str(body);
    kept.extend(text[text.len() - trailing..].chars().map(reference));
    kept
}

/// The Markdown with each Codex directive replaced by what the renderer shows, the artifact
/// template cards the replaced `::artifact-template` directives stand for, and the text of the
/// unresolved ones, which become paragraphs of their own.
fn prepare_directives(markdown: &str) -> (String, Vec<CodexArtifactTemplate>, Vec<String>) {
    if !markdown.contains(":codex-file-citation")
        && !markdown.contains(":codex-followup")
        && !markdown.contains("::artifact-template")
    {
        return (markdown.to_owned(), Vec::new(), Vec::new());
    }
    let matches = collect_directive_matches(markdown, true);
    let mut substitutions = DirectiveSubstitutions::default();
    let prepared = apply_directive_matches(
        markdown,
        0,
        markdown.len(),
        &matches,
        &|text| text.to_owned(),
        &mut substitutions,
    );
    (prepared, substitutions.templates, substitutions.literals)
}

/// The hast tree the renderer is built from, for user messages (no rehype plugins) or assistant
/// messages (`rehype-raw` and `rehype-sanitize`).
fn render_tree(markdown: &str, user_message: bool, line_breaks: bool) -> Vec<Hast> {
    let (prepared, templates, literals) = prepare_directives(markdown);
    let mut root = to_mdast_guarded(&prepared, &to_hast::parse_options()).unwrap_or_else(|| {
        markdown::mdast::Node::Root(markdown::mdast::Root {
            children: vec![markdown::mdast::Node::Text(markdown::mdast::Text {
                value: markdown.to_owned(),
                position: None,
            })],
            position: None,
        })
    });
    remark_github_alerts(&mut root);
    remark_normalize_list_item_indentation(&mut root, &prepared, &|source| {
        to_mdast_guarded(source, &to_hast::parse_options())
    });
    // `userProcessor` and `assistantBreaksProcessor` use `remarkBreaks`.
    if user_message || line_breaks {
        to_hast::newline_to_break(&mut root);
    }
    let mut tree = to_hast::to_hast(&root, &prepared, &templates, &literals);
    if !user_message {
        hast::preserve_bare_anchor_placeholders(&mut tree);
        tree = hast::sanitize(hast::parse_raw(&tree));
    }
    tree
}

struct Walker<'a> {
    user_message: bool,
    skills: &'a [InlineSkill],
    cwd: Option<&'a str>,
    parent_suffixes: HashMap<String, String>,
    segments: Vec<String>,
    text: String,
}

impl Walker<'_> {
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.text);
        if !js_trim(&text).is_empty() {
            self.segments.push(text);
        }
    }

    fn append_text(&mut self, value: &str, in_pre: bool, inline_skills: bool) {
        let mut value = value.to_owned();
        if inline_skills {
            let mut cursor = 0;
            let mut rendered = String::new();
            for matched in match_inline_skills(&value, self.skills) {
                rendered.push_str(&value[cursor..matched.start]);
                rendered.push_str(&format_provider_skill_display_name(matched.skill));
                cursor = matched.end;
            }
            rendered.push_str(&value[cursor..]);
            value = rendered;
        }
        if in_pre {
            self.text.push_str(&value);
        } else {
            self.text
                .push_str(&value.replace("\r\n", " ").replace('\n', " "));
        }
    }

    fn visit(&mut self, node: &Hast, in_pre: bool, inline_skills: bool) {
        match node {
            Hast::Text(value) => self.append_text(value, in_pre, inline_skills),
            Hast::Raw(value) => {
                if self.user_message {
                    self.append_text(value, in_pre, inline_skills);
                }
            }
            Hast::Element(element) => self.visit_element(element, in_pre, inline_skills),
        }
    }

    fn visit_element(&mut self, element: &Element, in_pre: bool, inline_skills: bool) {
        let href = element
            .props
            .get("href")
            .or_else(|| element.props.get("src"))
            .map(String::as_str);
        if self.user_message
            && let Some(href) = href
            && parse_composer_context_href(href).is_some()
        {
            self.flush();
            return;
        }
        let tag = element.tag.as_str();
        if tag == "a"
            && let Some(href) = href
            && let Some(citation) = parse_assistant_citation_href(href)
        {
            // Matches the citation chip, which shows its label instead of the link text.
            self.text.push_str(&assistant_citation_label(&citation));
            return;
        }
        let candidate: Option<String> = if tag == "a" {
            href.map(str::to_owned)
        } else if tag == "code" && !in_pre && element.props.contains_key("dataInlineCode") {
            let code_text: String = element
                .children
                .iter()
                .map(|child| match child {
                    Hast::Text(value) | Hast::Raw(value) => value.as_str(),
                    Hast::Element(_) => "",
                })
                .collect();
            inline_code_file_path_candidate(&code_text)
        } else {
            None
        };
        if let Some(candidate) = &candidate
            && let Some(target) = resolve_markdown_file_link_target(Some(candidate), self.cwd, None)
        {
            let label = label_text_of_element(element);
            if tag == "a" && !is_markdown_file_link_label(&label, candidate) {
                self.text.push_str(&label);
                self.text.push(' ');
            }
            self.text.push_str(&file_link_label(
                &split_file_path_position(&target, ""),
                &self.parent_suffixes,
            ));
            return;
        }
        if let Some(template) = artifact_template_from_properties(element) {
            // Matches the card's visible text; its action button is not indexed.
            self.flush();
            self.segments.push(template.display_name.clone());
            self.segments.push(
                codex_artifact_template_presentation_label(&template.artifact_kind)
                    .unwrap_or_default()
                    .to_owned(),
            );
            return;
        }
        let block = is_thread_find_block_tag(tag);
        if block {
            self.flush();
        }
        let render_skills = if tag == "code" || tag == "a" {
            false
        } else {
            inline_skills || tag == "p" || tag == "li"
        };
        for child in &element.children {
            self.visit(child, in_pre || tag == "pre", render_skills);
        }
        if block {
            self.flush();
        }
    }
}

fn label_text(node: &Hast) -> String {
    match node {
        Hast::Text(value) | Hast::Raw(value) => value.clone(),
        Hast::Element(element) => label_text_of_element(element),
    }
}

fn label_text_of_element(element: &Element) -> String {
    element.children.iter().map(label_text).collect()
}

/// `artifactTemplateFromHastProperties`.
fn artifact_template_from_properties(element: &Element) -> Option<CodexArtifactTemplate> {
    if element
        .props
        .get("dataCodexArtifactTemplate")
        .map(String::as_str)
        != Some("true")
    {
        return None;
    }
    let property = |name: &str| element.props.get(name).map(String::as_str);
    resolve_codex_artifact_template(Some(&CodexArtifactTemplateAttributes {
        artifact_kind: property("dataArtifactKind"),
        display_name: property("dataDisplayName"),
        gallery_kind: property("dataGalleryKind").map(Some),
        skill_directory: property("dataSkillDirectory"),
        skill_name: property("dataSkillName"),
    }))
}

/// Uses the renderer's Markdown transforms, without mounting folded/virtualized rows.
pub fn markdown_thread_find_text(
    markdown: &str,
    user_message: bool,
    cwd: Option<&str>,
    skills: &[InlineSkill],
    line_breaks: bool,
) -> Vec<String> {
    let tree = render_tree(markdown, user_message, line_breaks);
    let code_span_candidates: Vec<String> = extract_inline_code_spans(markdown)
        .iter()
        .filter_map(|span| inline_code_file_path_candidate(span))
        .collect();
    let hrefs = extract_markdown_link_hrefs(&render_codex_inline_directives_as_markdown(markdown))
        .into_iter()
        .chain(code_span_candidates);
    let paths: Vec<String> = hrefs
        .filter_map(|href| resolve_markdown_file_link_target(Some(&href), cwd, None))
        .map(|target| split_file_path_position(&target, "").path)
        .collect();
    let mut walker = Walker {
        user_message,
        skills,
        cwd,
        parent_suffixes: build_file_link_parent_suffix_by_path(&paths),
        segments: Vec::new(),
        text: String::new(),
    };
    for node in &tree {
        walker.visit(node, false, false);
    }
    walker.flush();
    walker.segments
}

#[cfg(test)]
mod diff_tmp;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod upstream_parity;
