# S2b handoff: chat media and citation UI

Branch `task/S2b-chat-media-ui`, worktree `turin-wt/S2b-chat-media-ui`, from `task/S2-chat-ui`. Commit trailer: `Model: grok-4.7 (cursor)`.

S2's relationships, PR row, lineage, tooltips, canvas, and cards were left untouched.

## Ported

| upstream | Rust | notes |
|---|---|---|
| `components/chat/AssistantCitationSource.tsx` | `components/chat/assistant_citation_source.rs` | scroll offset, pulse keyframes, comment-highlight registry. The row paints the pulse. Scroll uses `ListState::scroll_to` |
| `components/chat/AssistantCitationChip.tsx` | `components/chat/assistant_citation_chip.rs` | message chip and composer chip. Comment session tests cover source-unavailable |
| `components/chat/AssistantCitationCommentEditor.tsx` | `components/chat/assistant_citation_comment_editor.rs` | Enter saves, Command/Ctrl+Enter saves and sends, Shift+Enter inserts a line, Escape cancels |
| `components/chat/assistantCitationCommentDismissal.ts` | `components/chat/assistant_citation_comment_dismissal.rs` | outside-press commits, Escape discards, over-length stays open |
| `components/chat/useAssistantCitationTarget.ts` | `components/chat/assistant_citation_target.rs` | `AssistantCitationController`. Same-thread "Go to source" calls `MessagesTimeline::reveal_citation` |
| `components/chat/ExpandedImageDialog.tsx`, `ExpandedImagePreview.tsx`, `ZoomableImage.tsx` | `components/chat/expanded_image.rs` | fit, wheel zoom, click toggle, arrow pan. Protocol-relative URLs and the blob table match the preview tests |
| `components/chat/MermaidDiagram.tsx` | `components/chat/mermaid_diagram.rs` | source block plus `Unable to render diagram: No native Mermaid renderer is available.` The fence still toggles Show diagram / Show code |
| `components/chat/HighlightedTokens.tsx` | `components/chat/highlighted_tokens.rs` | `words_of`, `keyed_lines`, `with_embedded_scripts`. Spans are passed in; `embeddedScripts.ts` stays with U3c |
| `components/chat/HighlightedCodeLines.tsx` | `components/chat/highlighted_tokens.rs` and `markdown/code_highlight.rs` | text identity for every prefix. Tree-sitter runs off the UI thread and fills uncolored gaps with the foreground |

Mounts: citation links in markdown open the quote overlay; composer chips sit above the prompt and own the comment editor; user and composer thumbnails open the image dialog; `ChatView` paints `chat_media_layer` over the column.

## Gate

`scripts/check.sh -p turin` green. 2467 passed, 0 failed, 1 ignored.

## Screenshots

`scripts/shot.sh` found the Turin window, then `screencapture -l` failed with `could not create image from window`. A full-display capture of this session was black, so `docs/screenshots/S2b/` has no images. Demos are `TURIN_CHAT_MEDIA_DEMO=image|video|citation`.

## Remaining

- DOM text-selection half of `assistantTextSelection.ts` (`captureAssistantTextSelection`, source anchors, live ranges). The registry and scroll math are ported; CSS Highlight is not.
- `embeddedScripts.ts` (U3c). Nested-script tests pass explicit spans.
- No native Mermaid renderer. The fence shows the upstream error fallback, not an SVG.
- No video element. A video opens the unavailable frame.
- Composer chips are not ProseMirror atoms. They are scanned from the flat prompt.
- Screenshot pass, once a display can capture the window.
