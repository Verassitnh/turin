//! Port of upstream/packages/shared/src/claudeCompaction.ts

/// Copy for Claude's resume compaction dialog, shared by the server adapter
/// (which asks the question) and the client (which recognizes the question and
/// its "never" answer). Both sides must agree on these strings.
pub const CLAUDE_RESUME_COMPACTION_NEVER_ANSWER: &str = "Don't ask again";

fn group_thousands(value: i64) -> String {
    // `value.toLocaleString("en-US")`
    let mut digits = value.unsigned_abs().to_string();
    let mut grouped = String::new();
    while digits.len() > 3 {
        let split = digits.len() - 3;
        grouped = format!(",{}", &digits[split..]) + &grouped;
        digits.truncate(split);
    }
    format!("{}{grouped}{sign}", digits, sign = if value < 0 { "-" } else { "" })
}

pub fn format_claude_resume_compaction_question(
    age_minutes: i64,
    estimated_tokens: i64,
) -> String {
    let age_label = if age_minutes >= 60 {
        format!("{}h {}m", age_minutes / 60, age_minutes % 60)
    } else {
        format!("{age_minutes}m")
    };
    format!(
        "This session is {age_label} old and uses {} tokens. Compact it before continuing?",
        group_thousands(estimated_tokens)
    )
}

fn question_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"^This session is (?:\d+h \d+m|\d+m) old and uses \d{1,3}(?:,\d{3})* tokens\. Compact it before continuing\?$",
        )
        .expect("static regex is valid")
    })
}

pub fn is_claude_resume_compaction_question(question: &str) -> bool {
    question_pattern().is_match(question)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_its_own_formatted_question() {
        let inputs = [
            (145i64, 275_123i64),
            (70, 100_000),
            (59, 1_234_567),
            (0, 0),
        ];
        for (age_minutes, estimated_tokens) in inputs {
            let question = format_claude_resume_compaction_question(age_minutes, estimated_tokens);
            assert!(is_claude_resume_compaction_question(&question), "{question}");
        }
    }

    #[test]
    fn formats_ages_above_and_below_one_hour() {
        assert_eq!(
            format_claude_resume_compaction_question(145, 275_123),
            "This session is 2h 25m old and uses 275,123 tokens. Compact it before continuing?"
        );
        assert_eq!(
            format_claude_resume_compaction_question(45, 1_000),
            "This session is 45m old and uses 1,000 tokens. Compact it before continuing?"
        );
    }

    #[test]
    fn does_not_match_unrelated_questions() {
        assert!(!is_claude_resume_compaction_question(
            "The build cache is large. Compact it before continuing?"
        ));
    }
}
