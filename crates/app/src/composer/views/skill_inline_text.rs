//! Port of upstream/apps/web/src/components/chat/SkillInlineText.tsx
//!
//! `$skill` tokens in running text become skill chips. [`split_skill_segments`]
//! is `SkillInlineText`'s walk over `matchInlineSkills`; `ChatMarkdown` applies
//! it to the text runs of its paragraphs and list items
//! (`renderSkillInlineMarkdownChildren`, which leaves `code` and links alone),
//! and [`SkillInlineText`] renders one string on its own, e.g. a review comment.
//!
//! Not ported: `data-markdown-copy` (copying a selection that crosses a chip
//! yields the chip's label, not `$skill`).

use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};
use turin_shared::inline_skills::{
    InlineSkill, format_provider_skill_display_name, match_inline_skills,
};

use crate::composer::views::context_chip::{ContextChip, ContextChipKind};
use crate::ui::IconName;

/// A piece of text split around its skill tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSegment {
    Text(String),
    Skill {
        /// `formatProviderSkillDisplayName(skill)`.
        label: String,
        /// `$name`, what the chip copies as.
        raw_text: String,
    },
}

pub fn split_skill_segments(text: &str, skills: &[InlineSkill]) -> Vec<SkillSegment> {
    let mut segments = Vec::new();
    let mut cursor = 0;
    for found in match_inline_skills(text, skills) {
        if found.start > cursor {
            segments.push(SkillSegment::Text(text[cursor..found.start].to_owned()));
        }
        segments.push(SkillSegment::Skill {
            label: format_provider_skill_display_name(found.skill),
            raw_text: found.raw_text,
        });
        cursor = found.end;
    }
    if cursor == 0 {
        return vec![SkillSegment::Text(text.to_owned())];
    }
    if cursor < text.len() {
        segments.push(SkillSegment::Text(text[cursor..].to_owned()));
    }
    segments
}

/// `SkillChipIcon`: the skill glyph (`composerInlineChip`'s package outline).
pub const SKILL_CHIP_ICON: IconName = IconName::Package;

#[derive(IntoElement)]
pub struct SkillInlineText {
    pub id: SharedString,
    pub text: String,
    pub skills: Vec<InlineSkill>,
}

impl RenderOnce for SkillInlineText {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let segments = split_skill_segments(&self.text, &self.skills);
        let id = self.id;
        let children: Vec<AnyElement> = segments
            .into_iter()
            .enumerate()
            .map(|(index, segment)| match segment {
                SkillSegment::Text(text) => div().child(text).into_any_element(),
                SkillSegment::Skill { label, .. } => {
                    ContextChip::new(SharedString::from(format!("{id}-skill-{index}")))
                        .kind(ContextChipKind::Skill)
                        .icon(SKILL_CHIP_ICON)
                        .label(label)
                        .into_any_element()
                }
            })
            .collect();
        div().flex().flex_wrap().items_center().children(children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, display_name: Option<&str>) -> InlineSkill {
        InlineSkill {
            name: name.to_owned(),
            display_name: display_name.map(str::to_owned),
        }
    }

    #[test]
    fn leaves_text_without_known_skills_whole() {
        assert_eq!(
            split_skill_segments("use $unknown here", &[skill("review", None)]),
            vec![SkillSegment::Text("use $unknown here".into())]
        );
        assert_eq!(
            split_skill_segments("", &[skill("review", None)]),
            vec![SkillSegment::Text(String::new())]
        );
    }

    #[test]
    fn turns_a_known_token_into_a_chip_between_its_text() {
        assert_eq!(
            split_skill_segments("run $review now", &[skill("review", Some("Code Review"))]),
            vec![
                SkillSegment::Text("run ".into()),
                SkillSegment::Skill {
                    label: "Code Review".into(),
                    raw_text: "$review".into()
                },
                SkillSegment::Text(" now".into()),
            ]
        );
    }

    #[test]
    fn a_token_at_either_end_has_no_empty_text_around_it() {
        assert_eq!(
            split_skill_segments("$review", &[skill("review", None)]),
            vec![SkillSegment::Skill {
                label: "Review".into(),
                raw_text: "$review".into()
            }]
        );
    }

    #[test]
    fn handles_several_tokens() {
        let skills = [skill("a-skill", None), skill("b-skill", None)];
        let segments = split_skill_segments("$a-skill and $b-skill", &skills);
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[1], SkillSegment::Text(text) if text == " and "));
    }
}
