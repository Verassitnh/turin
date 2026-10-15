//! Port of upstream/packages/shared/src/codexAuthHandoff.ts
//!
//! The URLs follow the WHATWG parser through the `url` crate, as upstream's `new URL` does.
//! `js_len` stands in for JS `string.length` in every length bound, and the Effect schemas'
//! `isMaxLength` checks are applied where upstream decodes.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use turin_contracts::base_schemas::{EnvironmentId, js_len};
use turin_contracts::provider_instance::ProviderInstanceId;

use crate::js_string::{decode_uri_component, encode_uri_component};
use crate::provider_auth_return_url::provider_auth_return_url;

const INVALID_SIGN_IN: &str = "Invalid ChatGPT sign-in request.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAuthHandoff {
    pub authorization_url: String,
    pub return_url: String,
    pub environment_id: EnvironmentId,
    pub instance_id: ProviderInstanceId,
    pub flow_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexAuthDelivery {
    environment_id: EnvironmentId,
    instance_id: ProviderInstanceId,
    flow_id: String,
    callback_url: String,
    return_hash: String,
}

/// Output of [`read_codex_auth_delivery`]: the delivery with its return URL sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthDeliveryInput {
    pub environment_id: EnvironmentId,
    pub instance_id: ProviderInstanceId,
    pub flow_id: String,
    pub callback_url: String,
    pub return_hash: String,
    pub return_url: String,
}

/// `Schema.NonEmptyString.check(Schema.isMaxLength(128))`
fn is_flow_id(value: &str) -> bool {
    !value.is_empty() && js_len(value) <= 128
}

fn max_len(value: &str, limit: usize) -> bool {
    js_len(value) <= limit
}

fn is_valid_handoff(input: &CodexAuthHandoff) -> bool {
    max_len(&input.authorization_url, 16_384)
        && max_len(&input.return_url, 4_096)
        && is_flow_id(&input.flow_id)
}

fn is_valid_delivery(input: &CodexAuthDelivery) -> bool {
    is_flow_id(&input.flow_id)
        && max_len(&input.callback_url, 16_384)
        && max_len(&input.return_hash, 128)
}

/// `Schema.encodeSync(Schema.fromJsonString(...))`: validates, then writes compact JSON.
fn encode_handoff(input: &CodexAuthHandoff) -> Result<String, anyhow::Error> {
    anyhow::ensure!(is_valid_handoff(input), INVALID_SIGN_IN);
    Ok(serde_json::to_string(input)?)
}

fn decode_handoff(raw: &str) -> Option<CodexAuthHandoff> {
    let input: CodexAuthHandoff = serde_json::from_str(raw).ok()?;
    is_valid_handoff(&input).then_some(input)
}

fn encode_delivery(input: &CodexAuthDelivery) -> Result<String, anyhow::Error> {
    anyhow::ensure!(is_valid_delivery(input), "Invalid Codex auth delivery.");
    Ok(serde_json::to_string(input)?)
}

fn decode_delivery(raw: &str) -> Option<CodexAuthDelivery> {
    let input: CodexAuthDelivery = serde_json::from_str(raw).ok()?;
    is_valid_delivery(&input).then_some(input)
}

/// Whether `url` carries credentials or a non-empty fragment, which JS `url.username`,
/// `url.password`, and `url.hash` report. An empty fragment (`#`) reads as no hash.
fn has_credentials_or_hash(url: &url::Url) -> bool {
    !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
}

/// `url.hash` as JS reports it: `#fragment`, or empty when there is none.
fn js_hash(url: &url::Url) -> String {
    match url.fragment() {
        Some(fragment) if !fragment.is_empty() => format!("#{fragment}"),
        _ => String::new(),
    }
}

static REDIRECT_URI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^http://127\.0\.0\.1:[1-9][0-9]{0,4}/auth/callback$").expect("static regex")
});
static STATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{16,128}$").expect("static regex"));
static CODE_CHALLENGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{43}$").expect("static regex"));
static CLIENT_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(dynamic_agent_client|oaiapp_[A-Za-z0-9_-]+)$").expect("static regex")
});
static OAIAPP_CLIENT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^oaiapp_[A-Za-z0-9_-]+$").expect("static regex"));

/// The authorization request a helper may open: OpenAI's authorize endpoint, one loopback redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthorizationRequest {
    pub authorization_url: String,
    pub redirect_uri: String,
    pub state: String,
}

/// Validates the authorization URL. The helper only opens OpenAI's authorize endpoint and receives
/// a loopback callback.
pub fn codex_authorization_request(
    value: &str,
) -> Result<CodexAuthorizationRequest, anyhow::Error> {
    let url = url::Url::parse(value)?;
    anyhow::ensure!(
        js_len(value) <= 16_384
            && url.origin().ascii_serialization() == "https://auth.openai.com"
            && url.path() == "/api/accounts/authorize"
            && !has_credentials_or_hash(&url),
        INVALID_SIGN_IN
    );
    let single = |key: &str| -> Result<String, anyhow::Error> {
        let values: Vec<String> = url
            .query_pairs()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
            .collect();
        match values.as_slice() {
            [only] if !only.is_empty() => Ok(only.clone()),
            _ => Err(anyhow::anyhow!(INVALID_SIGN_IN)),
        }
    };
    let redirect_uri = single("redirect_uri")?;
    let redirect = url::Url::parse(&redirect_uri)?;
    let state = single("state")?;
    let code_challenge = single("code_challenge")?;
    let client_id = single("client_id")?;
    anyhow::ensure!(
        REDIRECT_URI.is_match(&redirect_uri)
            && redirect.port().is_some()
            && STATE.is_match(&state)
            && single("response_type")? == "code"
            && single("code_challenge_method")? == "S256"
            && CODE_CHALLENGE.is_match(&code_challenge)
            && CLIENT_ID.is_match(&client_id),
        INVALID_SIGN_IN
    );
    Ok(CodexAuthorizationRequest {
        authorization_url: url.to_string(),
        redirect_uri,
        state,
    })
}

/// Validation is shared by the desktop listener and the environment receiving the code.
pub fn codex_callback_url(
    value: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<url::Url, anyhow::Error> {
    let callback = url::Url::parse(value)?;
    let expected = url::Url::parse(redirect_uri)?;
    let params = |key: &str| -> Vec<String> {
        callback
            .query_pairs()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
            .collect()
    };
    let states = params("state");
    let codes = params("code");
    let errors = params("error");
    let clients = params("client_id");
    let code_or_error = (codes.len() == 1 && !codes[0].is_empty() && errors.is_empty())
        || (errors.len() == 1 && !errors[0].is_empty() && codes.is_empty());
    anyhow::ensure!(
        js_len(value) <= 16_384
            && callback.origin() == expected.origin()
            && callback.path() == expected.path()
            && !has_credentials_or_hash(&callback)
            && states.len() == 1
            && states[0] == state
            && clients.len() <= 1
            && (clients.is_empty() || OAIAPP_CLIENT_ID.is_match(&clients[0]))
            && code_or_error,
        "This redirect URL does not belong to the current sign-in."
    );
    Ok(callback)
}

/// The `t3code://auth/codex` (or `t3code-dev://`) link that carries a handoff to the desktop app.
pub fn codex_auth_handoff_url(
    input: &CodexAuthHandoff,
    development: bool,
) -> Result<String, anyhow::Error> {
    let scheme = if development { "t3code-dev" } else { "t3code" };
    let mut url = url::Url::parse(&format!("{scheme}://auth/codex"))?;
    let request = encode_handoff(input)?;
    url.query_pairs_mut().append_pair("request", &request);
    Ok(url.to_string())
}

/// Reads a handoff link. Anything that does not name a valid handoff reads as None.
pub fn read_codex_auth_handoff(value: &str, development: bool) -> Option<CodexAuthHandoff> {
    let url = url::Url::parse(value).ok()?;
    let expected_scheme = if development { "t3code-dev" } else { "t3code" };
    if js_len(value) > 32_768
        || url.scheme() != expected_scheme
        || url.host_str() != Some("auth")
        || url.path() != "/codex"
        || has_credentials_or_hash(&url)
    {
        return None;
    }
    let requests: Vec<String> = url
        .query_pairs()
        .filter(|(name, _)| name == "request")
        .map(|(_, value)| value.into_owned())
        .collect();
    let [request] = requests.as_slice() else {
        return None;
    };
    let input = decode_handoff(request)?;
    codex_authorization_request(&input.authorization_url).ok()?;
    provider_auth_return_url(Some(&input.return_url))?;
    Some(input)
}

/// Codes travel in a fragment, never in hosted web requests or a token store on the helper.
pub fn codex_auth_delivery_url(
    input: &CodexAuthHandoff,
    callback_url: &str,
) -> Result<String, anyhow::Error> {
    let request = codex_authorization_request(&input.authorization_url)?;
    codex_callback_url(callback_url, &request.redirect_uri, &request.state)?;
    let destination = provider_auth_return_url(Some(&input.return_url))
        .ok_or_else(|| anyhow::anyhow!("Invalid T3 Code return address."))?;
    let mut url = url::Url::parse(&destination)?;
    let delivery = CodexAuthDelivery {
        environment_id: input.environment_id.clone(),
        instance_id: input.instance_id.clone(),
        flow_id: input.flow_id.clone(),
        callback_url: callback_url.to_owned(),
        return_hash: js_hash(&url),
    };
    let encoded = encode_delivery(&delivery)?;
    url.set_fragment(Some(&format!(
        "codex-auth={}",
        encode_uri_component(&encoded)
    )));
    Ok(url.to_string())
}

/// Reads a delivery link from the fragment and rebuilds the return URL with its original hash.
pub fn read_codex_auth_delivery(value: &str) -> Option<CodexAuthDeliveryInput> {
    const PREFIX: &str = "codex-auth=";
    let url = url::Url::parse(value).ok()?;
    let fragment = url.fragment()?;
    if !fragment.starts_with(PREFIX) || js_len(&format!("#{fragment}")) > 32_768 {
        return None;
    }
    let raw = decode_uri_component(&fragment[PREFIX.len()..]).ok()?;
    let input = decode_delivery(&raw)?;
    let destination = provider_auth_return_url(Some(value))?;
    let mut return_url = url::Url::parse(&destination).ok()?;
    if input.return_hash.is_empty() {
        return_url.set_fragment(None);
    } else {
        return_url.set_fragment(Some(input.return_hash.trim_start_matches('#')));
    }
    let sanitized = provider_auth_return_url(Some(return_url.as_str()))?;
    Some(CodexAuthDeliveryInput {
        environment_id: input.environment_id,
        instance_id: input.instance_id,
        flow_id: input.flow_id,
        callback_url: input.callback_url,
        return_hash: input.return_hash,
        return_url: sanitized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::base_schemas::EnvironmentId as EnvId;

    const CALLBACK: &str = "http://127.0.0.1:54213/auth/callback?state=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&code=one-time-code&client_id=oaiapp_test";

    fn authorization_url() -> String {
        let mut url = url::Url::parse("https://auth.openai.com/api/accounts/authorize").unwrap();
        url.query_pairs_mut()
            .append_pair("client_id", "dynamic_agent_client")
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", "http://127.0.0.1:54213/auth/callback")
            .append_pair("state", &"a".repeat(43))
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &"b".repeat(43));
        url.to_string()
    }

    fn input() -> CodexAuthHandoff {
        CodexAuthHandoff {
            authorization_url: authorization_url(),
            return_url: "https://app.t3.codes/welcome#agents:remote-environment".to_owned(),
            environment_id: EnvId::new("remote-environment").unwrap(),
            instance_id: ProviderInstanceId::new("work-codex").unwrap(),
            flow_id: "flow-one".to_owned(),
        }
    }

    #[test]
    fn keeps_the_hosted_return_route_account_and_environment_with_the_code_in_a_fragment() {
        let input = input();
        assert_eq!(
            read_codex_auth_handoff(&codex_auth_handoff_url(&input, false).unwrap(), false),
            Some(input.clone())
        );
        let delivery = codex_auth_delivery_url(&input, CALLBACK).unwrap();
        assert_eq!(url::Url::parse(&delivery).unwrap().query(), None);
        assert_eq!(
            read_codex_auth_delivery(&delivery),
            Some(CodexAuthDeliveryInput {
                environment_id: input.environment_id.clone(),
                instance_id: input.instance_id.clone(),
                flow_id: input.flow_id.clone(),
                callback_url: CALLBACK.to_owned(),
                return_hash: "#agents:remote-environment".to_owned(),
                return_url: input.return_url.clone(),
            })
        );
    }

    #[test]
    fn rejects_other_handlers_schemes_arbitrary_return_sites_and_non_openai_authorization() {
        let link = codex_auth_handoff_url(&input(), false).unwrap();
        assert_eq!(read_codex_auth_handoff(&link, true), None);
        assert_eq!(
            read_codex_auth_handoff(&link.replace("auth/codex", "auth/other"), false),
            None
        );
        let attacker_return = CodexAuthHandoff {
            return_url: "https://attacker.example/welcome".to_owned(),
            ..input()
        };
        assert_eq!(
            read_codex_auth_handoff(
                &codex_auth_handoff_url(&attacker_return, false).unwrap(),
                false
            ),
            None
        );
        let attacker_authorization = CodexAuthHandoff {
            authorization_url: input()
                .authorization_url
                .replace("auth.openai.com", "attacker.example"),
            ..input()
        };
        assert_eq!(
            read_codex_auth_handoff(
                &codex_auth_handoff_url(&attacker_authorization, false).unwrap(),
                false
            ),
            None
        );
    }

    #[test]
    fn rejects_duplicated_authorization_parameters_and_non_loopback_callback_addresses() {
        let input = input();
        assert!(
            codex_authorization_request(&format!(
                "{}&redirect_uri=http://localhost:1/auth/callback",
                input.authorization_url
            ))
            .is_err()
        );
        let mut url = url::Url::parse(&input.authorization_url).unwrap();
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "redirect_uri")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .append_pair("redirect_uri", "http://localhost:54213/auth/callback");
        assert!(codex_authorization_request(url.as_str()).is_err());
        let mut url = url::Url::parse(&input.authorization_url).unwrap();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .append_pair("redirect_uri", "https://attacker.example/auth/callback");
        assert!(codex_authorization_request(url.as_str()).is_err());
        assert!(codex_auth_delivery_url(&input, &format!("{CALLBACK}&state=another")).is_err());
    }
}
