//! Port of upstream/packages/shared/src/promptLinks.ts
//!
//! `DEFAULT_HOSTED_APP_URL` is inlined from `connectAuth.ts`, which is cut
//! (T3 Connect). The constant is the only piece this module needs.

use crate::js_string::{JS_DOT, JS_WS, utf16_len};
use turin_contracts::base_schemas::js_trim;
use url::Url;

/// From upstream `connectAuth.ts`.
pub const DEFAULT_HOSTED_APP_URL: &str = "https://app.t3.codes";

pub const PROMPT_LINK_MAX_PROMPT_LENGTH: usize = 8_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLinkRepository {
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLinkPullRequest {
    pub owner: String,
    pub name: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPrompt {
    pub text: String,
    pub hidden_characters: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewThreadLink {
    New {
        prompt: LinkedPrompt,
        repository: Option<PromptLinkRepository>,
        provider: Option<String>,
        model: Option<String>,
    },
    PullRequest {
        pull_request: PromptLinkPullRequest,
        prompt: Option<LinkedPrompt>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptLinkResult<T> {
    Success(T),
    Failure(String),
}

fn hidden_pattern() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"[\u{0000}-\u{0008}\u{000B}\u{000C}\u{000E}-\u{001F}\u{007F}-\u{009F}\u{00AD}\u{034F}\u{061C}\u{115F}\u{1160}\u{17B4}\u{17B5}\u{180B}-\u{180F}\u{200B}\u{200C}\u{200E}\u{200F}\u{202A}-\u{202E}\u{2060}-\u{206F}\u{3164}\u{FE00}-\u{FE0D}\u{FEFF}\u{FFA0}\u{FFF0}-\u{FFFB}\u{1BCA0}-\u{1BCA3}\u{1D173}-\u{1D17A}\u{E0000}-\u{E0FFF}]",
        )
        .expect("hidden characters")
    })
}

fn line_break() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"\r\n?|[\u{2028}\u{2029}]").expect("line break"))
}

fn is_emoji_part(code_point: Option<u32>) -> bool {
    let Some(code_point) = code_point else {
        return false;
    };
    (0x1f000..=0x1faff).contains(&code_point)
        || (0x2190..=0x2bff).contains(&code_point)
        || code_point == 0xfe0f
}

/// Removes characters a reader cannot see, and counts them.
pub fn sanitize_linked_prompt(raw: &str) -> LinkedPrompt {
    let mut hidden_characters = 0u32;
    let normalized = line_break().replace_all(raw, "\n");
    let visible: String = hidden_pattern()
        .replace_all(&normalized, |_caps: &regex::Captures| {
            hidden_characters += 1;
            ""
        })
        .into_owned();
    let chars: Vec<char> = visible.chars().collect();
    let mut text = String::new();
    for (index, character) in chars.iter().copied().enumerate() {
        if character != '\u{200D}' {
            text.push(character);
            continue;
        }
        let kept = is_emoji_part(chars.get(index.wrapping_sub(1)).map(|ch| u32::from(*ch)))
            && is_emoji_part(chars.get(index + 1).map(|ch| u32::from(*ch)));
        if kept {
            text.push(character);
        } else {
            hidden_characters += 1;
        }
    }
    LinkedPrompt {
        text: js_trim(&text).to_string(),
        hidden_characters,
    }
}

fn repository_of(owner: &str, name: &str) -> Option<PromptLinkRepository> {
    static OWNER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$").expect("owner")
    });
    static NAME: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^[A-Za-z0-9._-]{1,100}$").expect("name"));
    static DOTS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^\.+$").expect("dots"));
    let trimmed_name = regex::Regex::new(r"(?i)\.git$")
        .expect("git")
        .replace(name, "")
        .into_owned();
    if !OWNER.is_match(owner) || !NAME.is_match(&trimmed_name) || DOTS.is_match(&trimmed_name) {
        return None;
    }
    Some(PromptLinkRepository {
        owner: owner.to_string(),
        name: trimmed_name,
    })
}

pub fn parse_prompt_link_repository(value: &str) -> Option<PromptLinkRepository> {
    let trimmed = js_trim(value);
    static URL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^https?://(?:www\.)?github\.com/([^/?#]+)/([^/?#]+)/?$")
            .expect("repo url")
    });
    static SHORT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(r"^([^/{JS_WS}]+)/([^/{JS_WS}]+)$")).expect("repo short")
    });
    let captures = URL_RE
        .captures(trimmed)
        .or_else(|| SHORT.captures(trimmed))?;
    repository_of(captures.get(1)?.as_str(), captures.get(2)?.as_str())
}

fn parse_prompt_link_pull_request(value: &str) -> Option<PromptLinkPullRequest> {
    let trimmed = js_trim(value);
    static URL_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"(?i)^https?://(?:www\.)?github\.com/([^/?#]+)/([^/?#]+)/pull/([0-9]+)(?:[/?#]{JS_DOT}*)?$"
        ))
        .expect("pr url")
    });
    static SHORT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"^([^/{JS_WS}#]+)/([^/{JS_WS}#]+)(?:/|#)([0-9]+)$"
        ))
        .expect("pr short")
    });
    let captures = URL_RE
        .captures(trimmed)
        .or_else(|| SHORT.captures(trimmed))?;
    let repository = repository_of(captures.get(1)?.as_str(), captures.get(2)?.as_str())?;
    let number = parse_safe_positive(captures.get(3)?.as_str())?;
    Some(PromptLinkPullRequest {
        owner: repository.owner,
        name: repository.name,
        number,
    })
}

fn parse_safe_positive(raw: &str) -> Option<u64> {
    let number: u64 = raw.parse().ok()?;
    if number == 0 || number > 9_007_199_254_740_991 {
        return None;
    }
    Some(number)
}

pub fn prompt_link_pull_request_url(pull_request: &PromptLinkPullRequest) -> String {
    format!(
        "https://github.com/{}/{}/pull/{}",
        pull_request.owner, pull_request.name, pull_request.number
    )
}

fn query_pairs(search: &str) -> Vec<(String, String)> {
    let search = search.strip_prefix('?').unwrap_or(search);
    url::form_urlencoded::parse(search.as_bytes())
        .into_owned()
        .collect()
}

/// Upstream `URLSearchParams.has(key)` — true when the query names `key`, even with an empty value.
pub fn search_has_query_key(search: &str, key: &str) -> bool {
    query_pairs(search).iter().any(|(name, _)| name == key)
}

fn pair<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn read_prompt(pairs: &[(String, String)]) -> PromptLinkResult<Option<LinkedPrompt>> {
    let Some(raw) = pair(pairs, "prompt") else {
        return PromptLinkResult::Success(None);
    };
    if utf16_len(raw) > PROMPT_LINK_MAX_PROMPT_LENGTH {
        return PromptLinkResult::Failure(format!(
            "This prompt is longer than {} characters, so T3 Code did not open it.",
            "8,000"
        ));
    }
    let prompt = sanitize_linked_prompt(raw);
    if prompt.text.is_empty() && prompt.hidden_characters > 0 {
        return PromptLinkResult::Failure(
            "This prompt had only hidden characters, so it may be malicious. T3 Code did not open it."
                .to_string(),
        );
    }
    if prompt.text.is_empty() {
        PromptLinkResult::Success(None)
    } else {
        PromptLinkResult::Success(Some(prompt))
    }
}

fn optional_param(pairs: &[(String, String)], key: &str) -> Option<String> {
    let trimmed = js_trim(pair(pairs, key)?);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn parse_new_thread_link(search: &str) -> PromptLinkResult<NewThreadLink> {
    let pairs = query_pairs(search);
    let prompt = match read_prompt(&pairs) {
        PromptLinkResult::Failure(error) => return PromptLinkResult::Failure(error),
        PromptLinkResult::Success(prompt) => prompt,
    };
    if pair(&pairs, "pr").is_some() {
        let Some(pull_request) = pair(&pairs, "pr").and_then(parse_prompt_link_pull_request) else {
            return PromptLinkResult::Failure(
                "This link names a pull request T3 Code cannot read.".to_string(),
            );
        };
        return PromptLinkResult::Success(NewThreadLink::PullRequest {
            pull_request,
            prompt,
        });
    }
    let Some(prompt) = prompt else {
        return PromptLinkResult::Failure("This link has no prompt.".to_string());
    };
    let repository = if let Some(repo) = pair(&pairs, "repo") {
        match parse_prompt_link_repository(repo) {
            Some(repository) => Some(repository),
            None => {
                return PromptLinkResult::Failure(
                    "This link names a repository T3 Code cannot read.".to_string(),
                );
            }
        }
    } else {
        None
    };
    let provider = optional_param(&pairs, "provider").map(|value| value.to_ascii_lowercase());
    PromptLinkResult::Success(NewThreadLink::New {
        prompt,
        repository,
        provider,
        model: optional_param(&pairs, "model"),
    })
}

pub fn parse_thread_link_prompt(search: &str) -> PromptLinkResult<Option<LinkedPrompt>> {
    read_prompt(&query_pairs(search))
}

pub fn without_prompt_link_params(search: &str) -> String {
    let rest = query_pairs(search).into_iter().filter(|(key, _)| {
        !matches!(
            key.as_str(),
            "prompt" | "repo" | "pr" | "provider" | "model"
        )
    });
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(rest)
        .finish();
    if encoded.is_empty() {
        String::new()
    } else {
        format!("?{encoded}")
    }
}

pub struct NewThreadLinkInput<'a> {
    pub prompt: &'a str,
    pub repository: Option<&'a PromptLinkRepository>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub origin: Option<&'a str>,
}

/// Upstream `new URL("/new", origin)`: the path is replaced, so a trailing
/// slash or a path on the origin is dropped. An unparseable origin is an error,
/// as it is upstream (which throws), not a fallback to the hosted app.
pub fn build_new_thread_link(input: NewThreadLinkInput<'_>) -> Result<String, url::ParseError> {
    let origin = input.origin.unwrap_or(DEFAULT_HOSTED_APP_URL);
    let mut url = Url::parse(origin)?.join("/new")?;
    {
        let mut query = url.query_pairs_mut();
        query.clear();
        query.append_pair("prompt", input.prompt);
        if let Some(repository) = input.repository {
            query.append_pair("repo", &format!("{}/{}", repository.owner, repository.name));
        }
        if let Some(provider) = input.provider {
            query.append_pair("provider", provider);
            if let Some(model) = input.model {
                query.append_pair("model", model);
            }
        }
    }
    Ok(url.to_string())
}

/// Upstream `URL.search`: a bare `?` is the empty string, not `?`.
fn js_search(url: &Url) -> String {
    url.query()
        .filter(|query| !query.is_empty())
        .map(|query| format!("?{query}"))
        .unwrap_or_default()
}

fn app_scheme() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"^t3code(?:-[a-z]+)?:$").expect("scheme"))
}

fn thread_path() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"^/([^/]+)/([^/]+)/?$").expect("thread path"))
}

pub fn prompt_link_app_path(value: &str, hosted_app_url: Option<&str>) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let scheme = format!("{}:", url.scheme());
    if app_scheme().is_match(&scheme) {
        let host = url.host_str().unwrap_or("");
        if host == "new" && (url.path().is_empty() || url.path() == "/") {
            return Some(format!("/new{}", js_search(&url)));
        }
        if host == "threads" {
            let captures = thread_path().captures(url.path())?;
            return Some(format!(
                "/{}/{}{}",
                captures.get(1)?.as_str(),
                captures.get(2)?.as_str(),
                js_search(&url)
            ));
        }
        return None;
    }
    let hosted = Url::parse(hosted_app_url.unwrap_or(DEFAULT_HOSTED_APP_URL)).ok()?;
    if url.origin() != hosted.origin() {
        return None;
    }
    if url.path() == "/new" {
        return Some(format!("/new{}", js_search(&url)));
    }
    if url.query_pairs().any(|(key, _)| key == "prompt") {
        let captures = thread_path().captures(url.path())?;
        return Some(format!(
            "/{}/{}{}",
            captures.get(1)?.as_str(),
            captures.get(2)?.as_str(),
            js_search(&url)
        ));
    }
    None
}

pub fn prompt_link_deep_link(app_path: &str, scheme: Option<&str>) -> String {
    let scheme = scheme.unwrap_or("t3code");
    let url = Url::parse(&format!("https://app.invalid{app_path}"))
        .or_else(|_| Url::parse("https://app.invalid/").and_then(|base| base.join(app_path)))
        .expect("app path");
    let host = if url.path() == "/new" {
        "new".to_string()
    } else {
        format!("threads{}", url.path())
    };
    format!("{scheme}://{host}{}", js_search(&url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_hidden_characters_and_keeps_emoji() {
        let tagged = format!("rate me{}\u{E0042}", char::from_u32(0xE0041).unwrap());
        let sanitized = sanitize_linked_prompt(&format!("a\u{200B}b\u{202E}c\u{FEFF}{tagged}"));
        assert_eq!(sanitized.text, "abcrate me");
        assert_eq!(sanitized.hidden_characters, 5);
        assert_eq!(
            sanitize_linked_prompt("a\u{2028}b\u{2029}c"),
            LinkedPrompt {
                text: "a\nb\nc".into(),
                hidden_characters: 0,
            }
        );
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let heart = "\u{2764}\u{FE0F}";
        assert_eq!(
            sanitize_linked_prompt(&format!("{family} {heart}")),
            LinkedPrompt {
                text: format!("{family} {heart}"),
                hidden_characters: 0,
            }
        );
        assert_eq!(sanitize_linked_prompt("a\u{200D}b").hidden_characters, 1);
        assert_eq!(
            sanitize_linked_prompt("\u{1F468}\u{200D}").hidden_characters,
            1
        );
        assert_eq!(
            sanitize_linked_prompt("\u{20000}\u{200D}\u{20001}").hidden_characters,
            1
        );
    }

    #[test]
    fn reads_a_shared_prompt() {
        match parse_new_thread_link(
            "?prompt=Rate+me&repo=https://github.com/pingdotgg/t3code.git&provider=Codex&model=gpt-5",
        ) {
            PromptLinkResult::Success(NewThreadLink::New {
                prompt,
                repository,
                provider,
                model,
            }) => {
                assert_eq!(prompt.text, "Rate me");
                assert_eq!(prompt.hidden_characters, 0);
                let repository = repository.expect("repository");
                assert_eq!(repository.owner, "pingdotgg");
                assert_eq!(repository.name, "t3code");
                assert_eq!(provider.as_deref(), Some("codex"));
                assert_eq!(model.as_deref(), Some("gpt-5"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn reads_pull_request_links() {
        for pr in [
            "pingdotgg/t3code/42",
            "pingdotgg/t3code%2342",
            "https://github.com/pingdotgg/t3code/pull/42/files",
        ] {
            match parse_new_thread_link(&format!("?pr={pr}")) {
                PromptLinkResult::Success(NewThreadLink::PullRequest {
                    pull_request,
                    prompt,
                }) => {
                    assert_eq!(pull_request.owner, "pingdotgg");
                    assert_eq!(pull_request.name, "t3code");
                    assert_eq!(pull_request.number, 42);
                    assert!(prompt.is_none());
                }
                other => panic!("unexpected {other:?} for {pr}"),
            }
        }
    }

    #[test]
    fn refuses_links_it_cannot_open_safely() {
        assert!(matches!(
            parse_new_thread_link("?repo=a/b"),
            PromptLinkResult::Failure(_)
        ));
        assert!(matches!(
            parse_new_thread_link("?prompt=%E2%80%8B"),
            PromptLinkResult::Failure(_)
        ));
        assert!(matches!(
            parse_thread_link_prompt("?prompt=%E2%80%8B"),
            PromptLinkResult::Failure(_)
        ));
        assert!(matches!(
            parse_new_thread_link("?prompt=x&repo=../etc"),
            PromptLinkResult::Failure(_)
        ));
        assert!(matches!(
            parse_new_thread_link(&format!("?prompt={}", "x".repeat(8_001))),
            PromptLinkResult::Failure(_)
        ));
    }

    #[test]
    fn detects_prompt_query_key_like_url_search_params_has() {
        assert!(search_has_query_key("?prompt=", "prompt"));
        assert!(search_has_query_key("?prompt=Fix+it", "prompt"));
        assert!(!search_has_query_key("?panel=diff", "prompt"));
        assert!(!search_has_query_key("?notprompt=1", "prompt"));
    }

    #[test]
    fn reads_the_prompt_and_leaves_other_parameters() {
        assert_eq!(
            parse_thread_link_prompt("?prompt=Fix+it"),
            PromptLinkResult::Success(Some(LinkedPrompt {
                text: "Fix it".into(),
                hidden_characters: 0,
            }))
        );
        assert_eq!(
            parse_thread_link_prompt("?panel=diff"),
            PromptLinkResult::Success(None)
        );
        assert_eq!(
            without_prompt_link_params("?prompt=x&panel=diff"),
            "?panel=diff"
        );
    }

    #[test]
    fn maps_app_scheme_and_deep_links() {
        assert_eq!(
            prompt_link_app_path("t3code://new?prompt=hi", None).as_deref(),
            Some("/new?prompt=hi")
        );
        assert_eq!(
            prompt_link_app_path("t3code-dev://threads/env-1/thread-1", None).as_deref(),
            Some("/env-1/thread-1")
        );
        assert_eq!(
            prompt_link_app_path("https://app.t3.codes/env-1/thread-1?prompt=go", None).as_deref(),
            Some("/env-1/thread-1?prompt=go")
        );
        assert_eq!(
            prompt_link_app_path("https://app.t3.codes/settings/general", None),
            None
        );
        assert_eq!(
            prompt_link_app_path("https://evil.test/new?prompt=hi", None),
            None
        );
        assert_eq!(prompt_link_app_path("t3code://app/index.html", None), None);
        assert_eq!(
            prompt_link_deep_link("/new?prompt=hi", None),
            "t3code://new?prompt=hi"
        );
        assert_eq!(
            prompt_link_deep_link("/env-1/thread-1?prompt=hi", None),
            "t3code://threads/env-1/thread-1?prompt=hi"
        );
    }

    #[test]
    fn new_thread_link_resolves_the_new_path_against_the_origin() {
        let link = |origin| {
            build_new_thread_link(NewThreadLinkInput {
                prompt: "hi",
                repository: None,
                provider: None,
                model: None,
                origin,
            })
            .unwrap()
        };
        // `new URL("/new", origin)` replaces the path, so the origin's own path
        // and any trailing slash drop out.
        assert_eq!(
            link(Some("https://app.t3.codes/")),
            "https://app.t3.codes/new?prompt=hi"
        );
        assert_eq!(
            link(Some("http://localhost:5173/app/x")),
            "http://localhost:5173/new?prompt=hi"
        );
        assert_eq!(link(None), "https://app.t3.codes/new?prompt=hi");
    }

    #[test]
    fn new_thread_link_rejects_an_unparseable_origin() {
        let result = build_new_thread_link(NewThreadLinkInput {
            prompt: "hi",
            repository: None,
            provider: None,
            model: None,
            origin: Some("not a url"),
        });
        assert!(result.is_err());
    }

    #[test]
    fn a_bare_question_mark_is_an_empty_search() {
        // `URL.search` is "" when the query is empty, so no stray `?` survives.
        assert_eq!(
            prompt_link_app_path("https://app.t3.codes/new?", None).as_deref(),
            Some("/new")
        );
        assert_eq!(
            prompt_link_app_path("t3code://new?", None).as_deref(),
            Some("/new")
        );
        assert_eq!(
            prompt_link_app_path("t3code://threads/env-1/thread-1?", None).as_deref(),
            Some("/env-1/thread-1")
        );
        assert_eq!(prompt_link_deep_link("/new?", None), "t3code://new");
        assert_eq!(
            prompt_link_deep_link("/env-1/thread-1?", None),
            "t3code://threads/env-1/thread-1"
        );
    }

    #[test]
    fn builds_a_link_that_parses_back_to_the_same_prompt() {
        let repository = PromptLinkRepository {
            owner: "pingdotgg".to_owned(),
            name: "t3code".to_owned(),
        };
        let link = build_new_thread_link(NewThreadLinkInput {
            prompt: "Swear count & rating?",
            repository: Some(&repository),
            provider: Some("claudeAgent"),
            model: Some("opus"),
            origin: None,
        })
        .unwrap();
        let parsed_url = Url::parse(&link).unwrap();
        let search = format!("?{}", parsed_url.query().unwrap_or_default());
        let path = prompt_link_app_path(&link, None).unwrap();
        assert_eq!(path, format!("{}{search}", parsed_url.path()));
        match parse_new_thread_link(&search) {
            PromptLinkResult::Success(NewThreadLink::New { prompt, .. }) => {
                assert_eq!(prompt.text, "Swear count & rating?");
            }
            other => panic!("expected a new-thread link, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod qc_regressions {
    use super::*;

    #[test]
    fn repository_short_form_rejects_u_feff_like_js_whitespace() {
        // Node 22: /^([^/\s]+)\/([^/\s]+)$/.test("a﻿/b") is false.
        assert_eq!(parse_prompt_link_repository("a\u{FEFF}/b"), None);
    }
}
