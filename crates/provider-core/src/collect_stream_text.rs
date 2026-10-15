//! Port of upstream/packages/provider-core/src/server/collectStreamText.ts.

use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedUint8StreamText {
    pub text: String,
    pub truncated: bool,
    pub bytes: usize,
    pub invalid_utf8: bool,
}

pub fn decode_utf8(bytes: &[u8]) -> (String, bool) {
    (
        String::from_utf8_lossy(bytes).into_owned(),
        std::str::from_utf8(bytes).is_err(),
    )
}

/// Collect a stream in byte-sized chunks. Drains the producer after the cap
/// so its child process can exit normally, as the upstream fold does.
pub async fn collect_uint8_stream_text<E, Next, Fut>(
    mut next: Next,
    max_bytes: Option<usize>,
    truncated_marker: Option<&str>,
) -> Result<CollectedUint8StreamText, E>
where
    Next: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<Vec<u8>>, E>>,
{
    let max_bytes = max_bytes.unwrap_or(usize::MAX);
    let mut collected = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = next().await? {
        if truncated {
            continue;
        }
        let remaining = max_bytes.saturating_sub(collected.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = chunk.len().min(remaining);
        collected.extend_from_slice(&chunk[..take]);
        truncated = chunk.len() > remaining;
    }
    let bytes = collected.len();
    let (mut text, invalid_utf8) = decode_utf8(&collected);
    if truncated {
        text.push_str(truncated_marker.unwrap_or(""));
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
    use std::collections::VecDeque;
    use std::sync::Mutex;

    async fn collect(
        chunks: Vec<Vec<u8>>,
        cap: Option<usize>,
        marker: Option<&str>,
    ) -> CollectedUint8StreamText {
        let queue = Mutex::new(VecDeque::from(chunks));
        collect_uint8_stream_text(
            || async { Ok::<_, ()>(queue.lock().unwrap().pop_front()) },
            cap,
            marker,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn upstream_cases() {
        assert_eq!(
            collect(vec![b"hello ".to_vec(), b"world".to_vec()], None, None).await,
            CollectedUint8StreamText {
                text: "hello world".into(),
                bytes: 11,
                truncated: false,
                invalid_utf8: false
            }
        );
        assert_eq!(
            collect(
                vec![b"abcdef".to_vec(), b"ghij".to_vec()],
                Some(5),
                Some("[truncated]")
            )
            .await,
            CollectedUint8StreamText {
                text: "abcde[truncated]".into(),
                bytes: 5,
                truncated: true,
                invalid_utf8: false
            }
        );
        assert!(
            collect(vec![vec![0x66, 0x80, 0x6f]], None, None)
                .await
                .invalid_utf8
        );
        assert!(
            !collect(vec!["before\u{FFFD}after".as_bytes().to_vec()], None, None)
                .await
                .invalid_utf8
        );
    }

    #[tokio::test]
    async fn drains_after_truncation() {
        let queue = Mutex::new(VecDeque::from(vec![b"abcdef".to_vec(), b"ghij".to_vec()]));
        let result = collect_uint8_stream_text(
            || async { Ok::<_, ()>(queue.lock().unwrap().pop_front()) },
            Some(1),
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.bytes, 1);
        assert!(queue.lock().unwrap().is_empty());
    }
}
