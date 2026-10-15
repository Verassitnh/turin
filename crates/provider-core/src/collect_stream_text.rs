//! Port of upstream/packages/provider-core/src/server/collectStreamText.ts.

use futures::{Stream, StreamExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedUint8StreamText {
    pub text: String,
    pub truncated: bool,
    pub bytes: usize,
    pub invalid_utf8: bool,
}

/// Lossy UTF-8 decode plus whether the input was not valid UTF-8
/// (upstream `decodeUtf8` returns `{ text, invalidUtf8 }`).
pub fn decode_utf8(bytes: &[u8]) -> (String, bool) {
    (
        String::from_utf8_lossy(bytes).into_owned(),
        std::str::from_utf8(bytes).is_err(),
    )
}

/// Fold a byte stream into text, capped at `max_bytes`.
///
/// Keeps draining after truncation so the child process can exit normally,
/// as upstream's fold does (killing after the output cap can force an
/// expensive taskkill on Windows).
pub async fn collect_uint8_stream_text<S, B, E>(
    stream: S,
    max_bytes: Option<usize>,
    truncated_marker: Option<&str>,
) -> Result<CollectedUint8StreamText, E>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    let max_bytes = max_bytes.unwrap_or(usize::MAX);
    let mut chunks: Vec<u8> = Vec::new();
    let mut truncated = false;
    futures::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let chunk = chunk.as_ref();
        if truncated {
            continue;
        }
        let remaining = max_bytes.saturating_sub(chunks.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = chunk.len().min(remaining);
        chunks.extend_from_slice(&chunk[..take]);
        truncated = chunk.len() > remaining;
    }
    let bytes = chunks.len();
    let (mut text, invalid_utf8) = decode_utf8(&chunks);
    let marker = truncated_marker.unwrap_or("");
    if truncated && !marker.is_empty() {
        text.push_str(marker);
    }
    Ok(CollectedUint8StreamText {
        text,
        truncated,
        bytes,
        invalid_utf8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn chunks(items: &[&[u8]]) -> impl Stream<Item = Result<Vec<u8>, ()>> + use<> {
        stream::iter(
            items
                .iter()
                .map(|item| Ok(item.to_vec()))
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test]
    async fn collects_chunks_into_decoded_text() {
        let result = collect_uint8_stream_text(chunks(&[b"hello ", b"world"]), None, None)
            .await
            .unwrap();
        assert_eq!(
            result,
            CollectedUint8StreamText {
                text: "hello world".into(),
                bytes: 11,
                truncated: false,
                invalid_utf8: false
            }
        );
    }

    #[tokio::test]
    async fn truncates_by_bytes_and_appends_marker_once() {
        let result =
            collect_uint8_stream_text(chunks(&[b"abcdef", b"ghij"]), Some(5), Some("[truncated]"))
                .await
                .unwrap();
        assert_eq!(
            result,
            CollectedUint8StreamText {
                text: "abcde[truncated]".into(),
                bytes: 5,
                truncated: true,
                invalid_utf8: false
            }
        );
    }

    #[tokio::test]
    async fn reports_invalid_utf8_separately_from_literal_replacement_character() {
        let invalid = collect_uint8_stream_text(chunks(&[&[0x66, 0x80, 0x6f]]), None, None)
            .await
            .unwrap();
        let literal =
            collect_uint8_stream_text(chunks(&["before\u{FFFD}after".as_bytes()]), None, None)
                .await
                .unwrap();
        assert!(invalid.invalid_utf8);
        assert_eq!(invalid.text, "f\u{FFFD}o");
        assert!(!literal.invalid_utf8);
        assert_eq!(literal.text, "before\u{FFFD}after");
    }

    #[tokio::test]
    async fn drains_the_stream_after_truncation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let polled = AtomicUsize::new(0);
        let source = stream::iter(vec![
            Ok::<_, ()>(b"abcdef".to_vec()),
            Ok(b"ghij".to_vec()),
            Ok(b"klm".to_vec()),
        ])
        .inspect(|_| {
            polled.fetch_add(1, Ordering::SeqCst);
        });
        let result = collect_uint8_stream_text(source, Some(1), None)
            .await
            .unwrap();
        assert_eq!(result.bytes, 1);
        assert_eq!(polled.load(Ordering::SeqCst), 3);
    }
}
