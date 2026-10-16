use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

const MAX_ACP_TERMINAL_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ACP_TERMINAL_FRAME_BYTES: usize = MAX_ACP_TERMINAL_OUTPUT_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentTerminalState {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub output: String,
    pub exit_status: Option<Value>,
    pub pending_output_bytes: Option<Vec<u8>>,
}

fn is_base64_data_code(code: u8) -> bool {
    code.is_ascii_uppercase()
        || code.is_ascii_lowercase()
        || code.is_ascii_digit()
        || code == b'+'
        || code == b'/'
}

fn decode_base64_terminal_frame(encoded: &str) -> Option<Vec<u8>> {
    let max_encoded = MAX_ACP_TERMINAL_FRAME_BYTES.div_ceil(3).saturating_mul(4);
    if encoded.len() > max_encoded {
        return None;
    }
    let padding = if encoded.ends_with("==") {
        2
    } else if encoded.ends_with('=') {
        1
    } else {
        0
    };
    let data_length = encoded.len().saturating_sub(padding);
    if encoded.len() % 4 == 1
        || (padding > 0 && !encoded.len().is_multiple_of(4))
        || (padding == 1 && data_length % 4 != 3)
        || (padding == 2 && data_length % 4 != 2)
    {
        return None;
    }
    for byte in encoded.as_bytes().iter().take(data_length) {
        if !is_base64_data_code(*byte) {
            return None;
        }
    }
    let decoded = STANDARD.decode(encoded).ok()?;
    if decoded.len() > MAX_ACP_TERMINAL_FRAME_BYTES {
        None
    } else {
        Some(decoded)
    }
}

fn trailing_incomplete_utf8_start(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return bytes.len();
    }
    let mut lead_index = bytes.len().saturating_sub(1);
    while lead_index < bytes.len() && (bytes[lead_index] & 0xc0) == 0x80 {
        if lead_index == 0 {
            break;
        }
        lead_index -= 1;
    }
    let lead = bytes[lead_index];
    let expected_length = if (0xc2..=0xdf).contains(&lead) {
        2
    } else if (0xe0..=0xef).contains(&lead) {
        3
    } else if (0xf0..=0xf4).contains(&lead) {
        4
    } else {
        1
    };
    if bytes.len() - lead_index < expected_length {
        lead_index
    } else {
        bytes.len()
    }
}

fn decode_terminal_output_chunk(
    pending: Option<&[u8]>,
    encoded: &str,
) -> Option<(String, Option<Vec<u8>>)> {
    let frame = decode_base64_terminal_frame(encoded)?;
    let mut bytes = pending.unwrap_or(&[]).to_vec();
    bytes.extend_from_slice(&frame);
    let complete_end = trailing_incomplete_utf8_start(&bytes);
    let text = String::from_utf8_lossy(&bytes[..complete_end]).into_owned();
    if complete_end == bytes.len() {
        Some((text, None))
    } else {
        Some((text, Some(bytes[complete_end..].to_vec())))
    }
}

fn truncate_terminal_output(output: &str) -> String {
    let bytes = output.as_bytes();
    if bytes.len() <= MAX_ACP_TERMINAL_OUTPUT_BYTES {
        return output.to_owned();
    }
    let mut start = bytes.len() - MAX_ACP_TERMINAL_OUTPUT_BYTES;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Applies an ACP v2 agent-owned terminal snapshot or chunk to display state.
pub fn apply_acp_agent_terminal_update(
    previous: Option<&AcpAgentTerminalState>,
    update: &Value,
) -> AcpAgentTerminalState {
    let current = previous.cloned().unwrap_or(AcpAgentTerminalState {
        command: None,
        cwd: None,
        output: String::new(),
        exit_status: None,
        pending_output_bytes: None,
    });
    let session_update = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("");
    if session_update == "terminal_output_chunk" {
        let data = update.get("data").and_then(Value::as_str).unwrap_or("");
        let Some((text, pending)) =
            decode_terminal_output_chunk(current.pending_output_bytes.as_deref(), data)
        else {
            return current;
        };
        return AcpAgentTerminalState {
            command: current.command,
            cwd: current.cwd,
            output: truncate_terminal_output(&format!("{}{}", current.output, text)),
            exit_status: current.exit_status,
            pending_output_bytes: pending,
        };
    }
    let mut next = current;
    if update.get("command").is_some_and(Value::is_null) {
        next.command = None;
    } else if let Some(command) = update.get("command").and_then(Value::as_str) {
        next.command = Some(command.to_owned());
    }
    if update.get("cwd").is_some_and(Value::is_null) {
        next.cwd = None;
    } else if let Some(cwd) = update.get("cwd").and_then(Value::as_str) {
        next.cwd = Some(cwd.to_owned());
    }
    if let Some(output) = update.get("output") {
        let decoded = if output.is_null() {
            Some(("".to_owned(), None))
        } else if let Some(data) = output.get("data").and_then(Value::as_str) {
            decode_terminal_output_chunk(None, data)
        } else {
            None
        };
        if let Some((text, pending)) = decoded {
            next.output = truncate_terminal_output(&text);
            next.pending_output_bytes = pending;
        }
    }
    if update.get("exitStatus").is_some_and(Value::is_null) {
        next.exit_status = None;
    } else if let Some(exit_status) = update.get("exitStatus")
        && !exit_status.is_null()
    {
        let pending_text = next
            .pending_output_bytes
            .as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        next.output = truncate_terminal_output(&format!("{}{}", next.output, pending_text));
        next.pending_output_bytes = None;
        next.exit_status = Some(exit_status.clone());
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;

    #[test]
    fn applies_terminal_snapshots_and_output_chunks() {
        let snapshot = apply_acp_agent_terminal_update(
            None,
            &json!({
                "sessionUpdate": "terminal_update",
                "terminalId": "terminal-1",
                "command": "pnpm test",
                "cwd": "/workspace",
                "output": { "data": STANDARD.encode("first line\n") },
            }),
        );
        let chunked = apply_acp_agent_terminal_update(
            Some(&snapshot),
            &json!({
                "sessionUpdate": "terminal_output_chunk",
                "terminalId": "terminal-1",
                "data": STANDARD.encode("second line\n"),
            }),
        );
        let completed = apply_acp_agent_terminal_update(
            Some(&chunked),
            &json!({
                "sessionUpdate": "terminal_update",
                "terminalId": "terminal-1",
                "exitStatus": { "exitCode": 0 },
            }),
        );
        assert_eq!(
            completed,
            AcpAgentTerminalState {
                command: Some("pnpm test".to_owned()),
                cwd: Some("/workspace".to_owned()),
                output: "first line\nsecond line\n".to_owned(),
                exit_status: Some(json!({ "exitCode": 0 })),
                pending_output_bytes: None,
            }
        );
    }

    #[test]
    fn preserves_utf8_split_across_terminal_chunks() {
        let bytes = "A🙂B".as_bytes();
        let first = apply_acp_agent_terminal_update(
            None,
            &json!({
                "sessionUpdate": "terminal_output_chunk",
                "terminalId": "terminal-utf8",
                "data": STANDARD.encode(&bytes[..3]),
            }),
        );
        let second = apply_acp_agent_terminal_update(
            Some(&first),
            &json!({
                "sessionUpdate": "terminal_output_chunk",
                "terminalId": "terminal-utf8",
                "data": STANDARD.encode(&bytes[3..]),
            }),
        );
        assert_eq!(first.output, "A");
        assert!(first.pending_output_bytes.is_some());
        assert_eq!(second.output, "A🙂B");
        assert!(second.pending_output_bytes.is_none());
    }

    #[test]
    fn drops_malformed_and_oversized_terminal_frames() {
        let seeded = apply_acp_agent_terminal_update(
            None,
            &json!({
                "sessionUpdate": "terminal_output_chunk",
                "terminalId": "terminal-invalid",
                "data": STANDARD.encode("kept"),
            }),
        );
        assert_eq!(
            apply_acp_agent_terminal_update(
                Some(&seeded),
                &json!({
                    "sessionUpdate": "terminal_output_chunk",
                    "terminalId": "terminal-invalid",
                    "data": "not base64!",
                }),
            ),
            seeded
        );
        let oversized = STANDARD.encode(vec![0u8; MAX_ACP_TERMINAL_FRAME_BYTES + 1]);
        assert_eq!(
            apply_acp_agent_terminal_update(
                Some(&seeded),
                &json!({
                    "sessionUpdate": "terminal_output_chunk",
                    "terminalId": "terminal-oversized",
                    "data": oversized,
                }),
            ),
            seeded
        );
    }
}
