//! Port of upstream/apps/server/src/provider/Drivers/ClaudeSkillDispatch.test.ts.
use super::*;
use serde_json::json;

fn skills() -> HashSet<String> {
    ["2spec", "implement", "review", "re-release-version"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn dispatch(leading: Option<&str>, command: &str, name: &str) -> ClaudeSkillDispatch {
    ClaudeSkillDispatch {
        leading_text: leading.map(str::to_owned),
        command_text: command.to_owned(),
        skill_name: name.to_owned(),
    }
}

#[test]
fn leaves_a_prompt_without_a_known_skill_untouched() {
    assert_eq!(plan_claude_skill_dispatch("fix the build", &skills()), None);
    // Not a discovered skill, so it stays prose rather than becoming a command.
    assert_eq!(
        plan_claude_skill_dispatch("echo $HOME then $unknown", &skills()),
        None
    );
}

#[test]
fn moves_a_mid_prompt_mention_into_a_trailing_slash_command() {
    assert_eq!(
        plan_claude_skill_dispatch("ok, now $implement all the tickets", &skills()),
        Some(dispatch(
            Some("ok, now"),
            "/implement all the tickets",
            "implement"
        ))
    );
}

#[test]
fn keeps_a_mention_that_opens_the_prompt_as_a_single_command_block() {
    assert_eq!(
        plan_claude_skill_dispatch("$review\nfocus on auth", &skills()),
        Some(dispatch(None, "/review\nfocus on auth", "review"))
    );
}

#[test]
fn dispatches_a_known_skill_whose_name_begins_with_a_digit() {
    assert_eq!(
        plan_claude_skill_dispatch("use $2spec for this", &skills()),
        Some(dispatch(Some("use"), "/2spec for this", "2spec"))
    );
}

#[test]
fn dispatches_the_last_mention_and_rewrites_earlier_ones_inline() {
    assert_eq!(
        plan_claude_skill_dispatch("$review the diff, then $implement the fixes", &skills()),
        Some(dispatch(
            Some("/review the diff, then"),
            "/implement the fixes",
            "implement"
        ))
    );
}

#[test]
fn dispatches_currency_prefixed_mentions_and_preserves_their_source_boundaries() {
    for symbol in ["€", "£", "¥", "₹", "₩", "₿", "𑿝"] {
        assert_eq!(
            plan_claude_skill_dispatch(
                &format!("{symbol}review the diff, then {symbol}implement the fixes"),
                &skills()
            ),
            Some(dispatch(
                Some("/review the diff, then"),
                "/implement the fixes",
                "implement"
            ))
        );
        assert_eq!(
            plan_claude_skill_dispatch(&format!("{symbol}2spec for this"), &skills()),
            Some(dispatch(None, "/2spec for this", "2spec"))
        );
        assert_eq!(
            plan_claude_skill_dispatch(&format!("5{symbol}review {symbol}unknown"), &skills()),
            None
        );
    }
}

#[test]
fn ignores_a_dollar_token_glued_to_other_text() {
    assert_eq!(
        plan_claude_skill_dispatch("cost is 5$implement", &skills()),
        None
    );
}

#[test]
fn ignores_currency_amounts_and_compact_monetary_expressions() {
    let mut with_currency = skills();
    for name in ["20", "20k", "100M", "1e6"] {
        with_currency.insert(name.to_owned());
    }
    for symbol in ["$", "€", "£", "¥", "₹", "₩", "₿", "𑿝"] {
        assert_eq!(
            plan_claude_skill_dispatch(
                &format!("pay {symbol}20 {symbol}20k {symbol}100M {symbol}1e6 tomorrow"),
                &with_currency
            ),
            None
        );
    }
}

#[test]
fn a_dispatched_prompt_becomes_text_blocks_with_the_command_last() {
    assert_eq!(
        dispatch_content_blocks("ok, now $implement all the tickets", &skills()),
        Some(vec![
            json!({"type": "text", "text": "ok, now"}),
            json!({"type": "text", "text": "/implement all the tickets"}),
        ])
    );
    assert_eq!(
        dispatch_content_blocks("$review\nfocus on auth", &skills()),
        Some(vec![
            json!({"type": "text", "text": "/review\nfocus on auth"})
        ])
    );
    assert_eq!(dispatch_content_blocks("fix the build", &skills()), None);
}
