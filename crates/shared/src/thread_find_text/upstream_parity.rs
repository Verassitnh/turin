//! Differential cases for the directive scanner and thread find: each row is what upstream's
//! `searchableMessageSegments`, `renderCodexDirectivesForCopy` and
//! `renderCodexInlineDirectivesAsMarkdown` returned for the input (recorded by running
//! `upstream/packages/shared/src/threadFindText.ts` and `codexMarkdownDirectives.ts` under bun).
//!
//! They cover where the source scanner has to agree with micromark about whether a directive is a
//! directive at all: indented code and HTML blocks (no directives, and no placeholder token
//! may reach the find text), link reference definitions, GFM autolink literals, fenced code that
//! ends with its container, and the text unresolved directives are rebuilt to.

use pretty_assertions::assert_eq;

use super::*;
use crate::codex_markdown_directives::render_codex_directives_for_copy;

type Role = OrchestrationV2ConversationMessageRole;

/// `(role, input, find segments, copy output, inline output)`
type Case = (
    Role,
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
);

#[rustfmt::skip]
const CASES: &[Case] = &[
    (
        Role::Assistant,
        "    ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        &["::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n"],
        "    ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        "    ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ),
    (
        Role::Assistant,
        "Para\n\n\t::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        &["Para", "::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n"],
        "Para\n\n\t::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        "Para\n\n\t::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ),
    (
        Role::Assistant,
        "- a\n\n      ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        &["a", "::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n"],
        "- a\n\n      ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        "- a\n\n      ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ),
    (
        Role::Assistant,
        "<div>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</div>",
        &[" ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"} "],
        "<div>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</div>",
        "<div>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</div>",
    ),
    (
        Role::Assistant,
        "<details>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</details>",
        &[" ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"} "],
        "<details>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</details>",
        "<details>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</details>",
    ),
    (
        Role::Assistant,
        "<pre>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</pre>",
        &["::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n"],
        "<pre>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</pre>",
        "<pre>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</pre>",
    ),
    (
        Role::Assistant,
        "<p>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</p>",
        &[" ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"} "],
        "<p>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</p>",
        "<p>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</p>",
    ),
    (
        Role::Assistant,
        "Para\n    ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        &["Para ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}"],
        "Para\n    ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        "Para\n    ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ),
    (
        Role::Assistant,
        "Para\n   ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        &["Para", "Hello World", "Document template"],
        "Para\n   Hello World (Document template)",
        "Para\n   ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ),
    (
        Role::Assistant,
        "1. a\n\n   ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
        &["a", "Hello World", "Document template"],
        "1. a\n\n   Hello World (Document template)",
        "1. a\n\n   ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}",
    ),
    (
        Role::Assistant,
        "    ::artifact-template{skill_name=\"x\"}",
        &["::artifact-template{skill_name=\"x\"}\n"],
        "    ::artifact-template{skill_name=\"x\"}",
        "    ::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::Assistant,
        "Para\n\n\t::artifact-template{skill_name=\"x\"}",
        &["Para", "::artifact-template{skill_name=\"x\"}\n"],
        "Para\n\n\t::artifact-template{skill_name=\"x\"}",
        "Para\n\n\t::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::Assistant,
        "- a\n\n      ::artifact-template{skill_name=\"x\"}",
        &["a", "::artifact-template{skill_name=\"x\"}\n"],
        "- a\n\n      ::artifact-template{skill_name=\"x\"}",
        "- a\n\n      ::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::Assistant,
        "<div>\n::artifact-template{skill_name=\"x\"}\n</div>",
        &[" ::artifact-template{skill_name=\"x\"} "],
        "<div>\n::artifact-template{skill_name=\"x\"}\n</div>",
        "<div>\n::artifact-template{skill_name=\"x\"}\n</div>",
    ),
    (
        Role::Assistant,
        "<details>\n::artifact-template{skill_name=\"x\"}\n</details>",
        &[" ::artifact-template{skill_name=\"x\"} "],
        "<details>\n::artifact-template{skill_name=\"x\"}\n</details>",
        "<details>\n::artifact-template{skill_name=\"x\"}\n</details>",
    ),
    (
        Role::Assistant,
        "<pre>\n::artifact-template{skill_name=\"x\"}\n</pre>",
        &["::artifact-template{skill_name=\"x\"}\n"],
        "<pre>\n::artifact-template{skill_name=\"x\"}\n</pre>",
        "<pre>\n::artifact-template{skill_name=\"x\"}\n</pre>",
    ),
    (
        Role::Assistant,
        "<p>\n::artifact-template{skill_name=\"x\"}\n</p>",
        &[" ::artifact-template{skill_name=\"x\"} "],
        "<p>\n::artifact-template{skill_name=\"x\"}\n</p>",
        "<p>\n::artifact-template{skill_name=\"x\"}\n</p>",
    ),
    (
        Role::Assistant,
        "Para\n    ::artifact-template{skill_name=\"x\"}",
        &["Para ::artifact-template{skill_name=\"x\"}"],
        "Para\n    ::artifact-template{skill_name=\"x\"}",
        "Para\n    ::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::Assistant,
        "Para\n   ::artifact-template{skill_name=\"x\"}",
        &["Para", "::artifact-template{skill_name=\"x\"}"],
        "Para\n   ::artifact-template{skill_name=\"x\"}",
        "Para\n   ::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::Assistant,
        "1. a\n\n   ::artifact-template{skill_name=\"x\"}",
        &["a", "::artifact-template{skill_name=\"x\"}"],
        "1. a\n\n   ::artifact-template{skill_name=\"x\"}",
        "1. a\n\n   ::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::User,
        "<div>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</div>",
        &["<div> ::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"} </div>"],
        "<div>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</div>",
        "<div>\n::artifact-template{skill_name=\"artifact-template-hello-world\" skill_directory=\"/Users/test/.codex/skills/artifact-template-hello-world\" display_name=\"Hello World\" artifact_kind=\"document\"}\n</div>",
    ),
    (
        Role::User,
        "    ::artifact-template{skill_name=\"x\"}",
        &["::artifact-template{skill_name=\"x\"}\n"],
        "    ::artifact-template{skill_name=\"x\"}",
        "    ::artifact-template{skill_name=\"x\"}",
    ),
    (
        Role::Assistant,
        "    :codex-file-citation{path=\"a/b.ts\"}",
        &[":codex-file-citation{path=\"a/b.ts\"}\n"],
        "    :codex-file-citation{path=\"a/b.ts\"}",
        "    :codex-file-citation{path=\"a/b.ts\"}",
    ),
    (
        Role::Assistant,
        "1. a\n\n       :codex-file-citation{path=\"a/b.ts\"}",
        &["a", ":codex-file-citation{path=\"a/b.ts\"}\n"],
        "1. a\n\n       :codex-file-citation{path=\"a/b.ts\"}",
        "1. a\n\n       :codex-file-citation{path=\"a/b.ts\"}",
    ),
    (
        Role::Assistant,
        "<div>:codex-file-citation{path=\"a/b.ts\"}</div>",
        &[":codex-file-citation{path=\"a/b.ts\"}"],
        "<div>:codex-file-citation{path=\"a/b.ts\"}</div>",
        "<div>:codex-file-citation{path=\"a/b.ts\"}</div>",
    ),
    (
        Role::Assistant,
        "<details><summary>A</summary>body :codex-file-citation{path=\"a/b.ts\"}</details>",
        &["A", "body :codex-file-citation{path=\"a/b.ts\"}"],
        "<details><summary>A</summary>body :codex-file-citation{path=\"a/b.ts\"}</details>",
        "<details><summary>A</summary>body :codex-file-citation{path=\"a/b.ts\"}</details>",
    ),
    (
        Role::Assistant,
        "<span>\n:codex-file-citation{path=\"a/b.ts\"}\n</span>",
        &[" :codex-file-citation{path=\"a/b.ts\"} "],
        "<span>\n:codex-file-citation{path=\"a/b.ts\"}\n</span>",
        "<span>\n:codex-file-citation{path=\"a/b.ts\"}\n</span>",
    ),
    (
        Role::Assistant,
        "<p>:codex-followup[Go **now**]{prompt=\"p\"}</p>",
        &[":codex-followup[Go **now**]{prompt=\"p\"}"],
        "<p>:codex-followup[Go **now**]{prompt=\"p\"}</p>",
        "<p>:codex-followup[Go **now**]{prompt=\"p\"}</p>",
    ),
    (
        Role::Assistant,
        "<?php :codex-file-citation{path=\"a/b.ts\"} ?>",
        &[],
        "<?php :codex-file-citation{path=\"a/b.ts\"} ?>",
        "<?php :codex-file-citation{path=\"a/b.ts\"} ?>",
    ),
    (
        Role::Assistant,
        "<![CDATA[ :codex-file-citation{path=\"a/b.ts\"} ]]>",
        &[],
        "<![CDATA[ :codex-file-citation{path=\"a/b.ts\"} ]]>",
        "<![CDATA[ :codex-file-citation{path=\"a/b.ts\"} ]]>",
    ),
    (
        Role::Assistant,
        "[x]: http://x.com/a ':codex-file-citation{path=\"a/b.ts\"}'",
        &[],
        "[x]: http://x.com/a ':codex-file-citation{path=\"a/b.ts\"}'",
        "[x]: http://x.com/a ':codex-file-citation{path=\"a/b.ts\"}'",
    ),
    (
        Role::Assistant,
        "[x]: <http://x.com/a:codex-file-citation{path=\"a/b.ts\"}>",
        &[],
        "[x]: <http://x.com/a:codex-file-citation{path=\"a/b.ts\"}>",
        "[x]: <http://x.com/a:codex-file-citation{path=\"a/b.ts\"}>",
    ),
    (
        Role::Assistant,
        "https://example.com/:codex-file-citation{path=\"a/b.ts\"}",
        &["https://example.com/:codex-file-citation{path=\"a/b.ts\"}"],
        "https://example.com/[b.ts](<a/b.ts>)",
        "https://example.com/[b.ts](<a/b.ts>)",
    ),
    (
        Role::Assistant,
        "www.example.com/:codex-file-citation{path=\"a/b.ts\"}",
        &["www.example.com/:codex-file-citation{path=\"a/b.ts\"}"],
        "www.example.com/[b.ts](<a/b.ts>)",
        "www.example.com/[b.ts](<a/b.ts>)",
    ),
    (
        Role::Assistant,
        "http://x.com/:codex-file-citation{path=\"a/b.ts\"}",
        &["http://x.com/:codex-file-citation{path=\"a/b.ts\"}"],
        "http://x.com/[b.ts](<a/b.ts>)",
        "http://x.com/[b.ts](<a/b.ts>)",
    ),
    (
        Role::Assistant,
        "```\n:codex-file-citation{path=\"a/b.ts\"}\n```",
        &[":codex-file-citation{path=\"a/b.ts\"}\n"],
        "```\n:codex-file-citation{path=\"a/b.ts\"}\n```",
        "```\n:codex-file-citation{path=\"a/b.ts\"}\n```",
    ),
    (
        Role::Assistant,
        "~~~\n:codex-followup[Go **now**]{prompt=\"p\"}\n~~~",
        &[":codex-followup[Go **now**]{prompt=\"p\"}\n"],
        "~~~\n:codex-followup[Go **now**]{prompt=\"p\"}\n~~~",
        "~~~\n:codex-followup[Go **now**]{prompt=\"p\"}\n~~~",
    ),
    (
        Role::Assistant,
        "- ```\n:codex-file-citation{path=\"a/b.ts\"}\n",
        &["b.ts"],
        "- ```\n[b.ts](<a/b.ts>)\n",
        "- ```\n[b.ts](<a/b.ts>)\n",
    ),
    (
        Role::Assistant,
        "    :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        &[":codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n"],
        "    :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        "    :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
    ),
    (
        Role::Assistant,
        "1. a\n\n       :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        &["a", ":codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n"],
        "1. a\n\n       :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        "1. a\n\n       :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
    ),
    (
        Role::Assistant,
        "<div>:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}</div>",
        &[":codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}"],
        "<div>:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}</div>",
        "<div>:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}</div>",
    ),
    (
        Role::Assistant,
        "<details><summary>A</summary>body :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}</details>",
        &["A", "body :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}"],
        "<details><summary>A</summary>body :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}</details>",
        "<details><summary>A</summary>body :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}</details>",
    ),
    (
        Role::Assistant,
        "<span>\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n</span>",
        &[" :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} "],
        "<span>\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n</span>",
        "<span>\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n</span>",
    ),
    (
        Role::Assistant,
        "<p>:codex-followup[Go **now**]{prompt=\"p\"}</p>",
        &[":codex-followup[Go **now**]{prompt=\"p\"}"],
        "<p>:codex-followup[Go **now**]{prompt=\"p\"}</p>",
        "<p>:codex-followup[Go **now**]{prompt=\"p\"}</p>",
    ),
    (
        Role::Assistant,
        "<?php :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} ?>",
        &[],
        "<?php :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} ?>",
        "<?php :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} ?>",
    ),
    (
        Role::Assistant,
        "<![CDATA[ :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} ]]>",
        &[],
        "<![CDATA[ :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} ]]>",
        "<![CDATA[ :codex-file-citation{path=\"a/b.ts\" purpose=\"output\"} ]]>",
    ),
    (
        Role::Assistant,
        "[x]: http://x.com/a ':codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}'",
        &[],
        "[x]: http://x.com/a ':codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}'",
        "[x]: http://x.com/a ':codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}'",
    ),
    (
        Role::Assistant,
        "[x]: <http://x.com/a:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}>",
        &[],
        "[x]: <http://x.com/a:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}>",
        "[x]: <http://x.com/a:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}>",
    ),
    (
        Role::Assistant,
        "https://example.com/:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        &["https://example.com/:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}"],
        "https://example.com/[b.ts](<a/b.ts>)",
        "https://example.com/[b.ts](<a/b.ts>)",
    ),
    (
        Role::Assistant,
        "www.example.com/:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        &["www.example.com/:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}"],
        "www.example.com/[b.ts](<a/b.ts>)",
        "www.example.com/[b.ts](<a/b.ts>)",
    ),
    (
        Role::Assistant,
        "http://x.com/:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}",
        &["http://x.com/:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}"],
        "http://x.com/[b.ts](<a/b.ts>)",
        "http://x.com/[b.ts](<a/b.ts>)",
    ),
    (
        Role::Assistant,
        "```\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n```",
        &[":codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n"],
        "```\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n```",
        "```\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n```",
    ),
    (
        Role::Assistant,
        "~~~\n:codex-followup[Go **now**]{prompt=\"p\"}\n~~~",
        &[":codex-followup[Go **now**]{prompt=\"p\"}\n"],
        "~~~\n:codex-followup[Go **now**]{prompt=\"p\"}\n~~~",
        "~~~\n:codex-followup[Go **now**]{prompt=\"p\"}\n~~~",
    ),
    (
        Role::Assistant,
        "- ```\n:codex-file-citation{path=\"a/b.ts\" purpose=\"output\"}\n",
        &["b.ts"],
        "- ```\n[b.ts](<a/b.ts>)\n",
        "- ```\n[b.ts](<a/b.ts>)\n",
    ),
    (
        Role::Assistant,
        ":codex-followup{}",
        &[":codex-followup"],
        ":codex-followup{}",
        ":codex-followup{}",
    ),
    (
        Role::Assistant,
        ":codex-followup[]{prompt=\"p\"}",
        &[":codex-followup{prompt=\"p\"}"],
        ":codex-followup[]{prompt=\"p\"}",
        ":codex-followup[]{prompt=\"p\"}",
    ),
    (
        Role::Assistant,
        ":codex-file-citation{path}",
        &[":codex-file-citation{path=\"\"}"],
        ":codex-file-citation{path}",
        ":codex-file-citation{path}",
    ),
    (
        Role::Assistant,
        ":codex-followup{prompt='q'}",
        &[":codex-followup{prompt=\"q\"}"],
        ":codex-followup{prompt='q'}",
        ":codex-followup{prompt='q'}",
    ),
    (
        Role::Assistant,
        ":codex-followup{line_range_start=3}",
        &[":codex-followup{line_range_start=\"3\"}"],
        ":codex-followup{line_range_start=3}",
        ":codex-followup{line_range_start=3}",
    ),
    (
        Role::Assistant,
        ":codex-followup{\n.cls}",
        &[":codex-followup{class=\"cls\"}"],
        ":codex-followup{\n.cls}",
        ":codex-followup{\n.cls}",
    ),
    (
        Role::Assistant,
        ":codex-followup[a](http://x.com){prompt=\"p\"}",
        &[":codex-followup[a](http://x.com){prompt=\"p\"}"],
        ":codex-followup[a](http://x.com){prompt=\"p\"}",
        ":codex-followup[a](http://x.com){prompt=\"p\"}",
    ),
    (
        Role::Assistant,
        ":codex-followup[a]",
        &[":codex-followup[a]"],
        ":codex-followup[a]",
        ":codex-followup[a]",
    ),
    (
        Role::Assistant,
        ":codex-file-citation{path=\"a/b.ts\" path=\"c.ts\"}",
        &["c.ts"],
        "[c.ts](<c.ts>)",
        "[c.ts](<c.ts>)",
    ),
    (
        Role::Assistant,
        "[a :codex-file-citation{path=\"a/b.ts\"}](http://x.com)",
        &["a :codex-file-citation{path=\"a/b.ts\"}"],
        "[a :codex-file-citation{path=\"a/b.ts\"}](http://x.com)",
        "[a :codex-file-citation{path=\"a/b.ts\"}](http://x.com)",
    ),
    (
        Role::Assistant,
        ":codex-file-citation{path=\"../../../../x\"}",
        &["x"],
        "[x](<../../../../x>)",
        "[x](<../../../../x>)",
    ),
    (
        Role::Assistant,
        ":codex-file-citation[lbl]{path=\"../../../../x\"}",
        &["x"],
        "[x](<../../../../x>)",
        "[x](<../../../../x>)",
    ),
    (
        Role::Assistant,
        ":codex-followup[![i](x.png)]{prompt=\"p\"}",
        &[":codex-followup[]{prompt=\"p\"}"],
        ":codex-followup[![i](x.png)]{prompt=\"p\"}",
        ":codex-followup[![i](x.png)]{prompt=\"p\"}",
    ),
    (
        Role::Assistant,
        ":codex-followup[a :codex-file-citation{path=\"a/b.ts\"} b]{prompt=\"p\"}",
        &["a b.ts b"],
        "a :codex-file-citation{path=\"a/b.ts\"} b",
        "a :codex-file-citation{path=\"a/b.ts\"} b",
    ),
    (
        Role::Assistant,
        ":codex-followup[  Go  ]{prompt=\"p\"}",
        &["  Go  "],
        "  Go  ",
        "  Go  ",
    ),
    (
        Role::Assistant,
        "x :codex-followup[ Go ]{prompt=\"p\"} y",
        &["x  Go  y"],
        "x  Go  y",
        "x  Go  y",
    ),
    (
        Role::Assistant,
        ":codex-followup[*]{prompt=\"p\"}",
        &["*"],
        "*",
        "*",
    ),
    (
        Role::Assistant,
        ":codex-followup[# h]{prompt=\"p\"}",
        &["# h"],
        "# h",
        "# h",
    ),
    (
        Role::Assistant,
        ":codex-followup[a :codex-followup[b :codex-file-citation{path=\"a/b.ts\"}]{prompt=\"q\"}]{prompt=\"p\"}",
        &["a b b.ts"],
        "a :codex-followup[b :codex-file-citation{path=\"a/b.ts\"}]{prompt=\"q\"}",
        "a :codex-followup[b :codex-file-citation{path=\"a/b.ts\"}]{prompt=\"q\"}",
    ),
    (
        Role::Assistant,
        "[l :codex-followup[a :codex-file-citation{path=\"a/b.ts\"}]{prompt=\"p\"}](http://x.com)",
        &["l a :codex-file-citation{path=\"a/b.ts\"}"],
        "[l a :codex-file-citation{path=\"a/b.ts\"}](http://x.com)",
        "[l a :codex-file-citation{path=\"a/b.ts\"}](http://x.com)",
    ),
    (
        Role::Assistant,
        ":codex-followup[Go **now**]{prompt=\"p\"}",
        &["Go now"],
        "Go **now**",
        "Go **now**",
    ),
    (
        Role::Assistant,
        "x :codex-followup[Go **now**]{prompt=\"p\"}",
        &["x Go now"],
        "x Go **now**",
        "x Go **now**",
    ),
    (
        Role::User,
        ":codex-followup{}",
        &[":codex-followup"],
        ":codex-followup{}",
        ":codex-followup{}",
    ),
];

#[test]
fn matches_the_upstream_find_segments_and_renderers() {
    for (role, input, find, copy, inline) in CASES {
        let message = SearchableMessage {
            role: *role,
            text: input,
            streaming: false,
            has_context: false,
        };
        assert_eq!(
            searchable_message_segments(&message, None, &[]).expect("indexed role"),
            find.iter()
                .map(|segment| (*segment).to_owned())
                .collect::<Vec<_>>(),
            "find segments of {input:?}"
        );
        assert_eq!(
            &render_codex_directives_for_copy(input),
            copy,
            "copy of {input:?}"
        );
        assert_eq!(
            &render_codex_inline_directives_as_markdown(input),
            inline,
            "inline Markdown of {input:?}"
        );
    }
}
