//! Joins a configured server URL with an API path the way `versionProbe.ts` does:
//! a path prefix and query are kept (`/base/?x=1` + `/api/info` → `/base/api/info?x=1`).

use crate::error::OpenCodeRuntimeError;

pub fn parse_http_base(
    server_url: &str,
    operation: &str,
) -> Result<reqwest::Url, OpenCodeRuntimeError> {
    let url = reqwest::Url::parse(server_url.trim()).map_err(|_| {
        OpenCodeRuntimeError::new(
            operation,
            "The OpenCode server URL is not a valid http:// or https:// URL.",
        )
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(OpenCodeRuntimeError::new(
            operation,
            "The OpenCode server URL is not a valid http:// or https:// URL.",
        ));
    }
    Ok(url)
}

pub fn join_server_path(base: &reqwest::Url, path: &str) -> reqwest::Url {
    let mut url = base.clone();
    let prefix = url.path().trim_end_matches('/');
    url.set_path(&format!("{prefix}{path}"));
    url
}

/// Bracket-encoded query objects, matching `@opencode/client` `appendQuery`.
/// `{location: {directory}}` becomes `location[directory]=<directory>`.
pub fn append_query(pairs: &mut Vec<(String, String)>, key: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => pairs.push((key.to_string(), "null".to_string())),
        serde_json::Value::Array(items) => {
            for item in items {
                append_query(pairs, key, item);
            }
        }
        serde_json::Value::Object(map) => {
            for (child, item) in map {
                append_query(pairs, &format!("{key}[{child}]"), item);
            }
        }
        serde_json::Value::String(text) => pairs.push((key.to_string(), text.clone())),
        other => pairs.push((key.to_string(), other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_path_prefix_and_query() {
        let base = reqwest::Url::parse("http://proxy:8080/opencode/?route=oc").unwrap();
        let joined = join_server_path(&base, "/api/info");
        assert_eq!(
            joined.as_str(),
            "http://proxy:8080/opencode/api/info?route=oc"
        );
    }

    #[test]
    fn encodes_location_as_bracketed_keys() {
        let mut pairs = Vec::new();
        append_query(
            &mut pairs,
            "location",
            &serde_json::json!({ "directory": "/work" }),
        );
        assert_eq!(
            pairs,
            vec![("location[directory]".to_string(), "/work".to_string())]
        );
    }
}
