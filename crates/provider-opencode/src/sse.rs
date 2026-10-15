//! Manual SSE decoder.
//!
//! The 45s watchdog sits on the raw byte stream, before frames are parsed, so a
//! `: heartbeat` comment keeps a quiet server alive. Nothing here reconnects or
//! sends `Last-Event-ID`. The adapter subscribes again and backfills.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::EventStreamError;

/// Upstream writes a `: heartbeat` every 10 to 15 seconds. This long without a
/// single byte means the server is stuck, not idle.
pub const SILENT_STREAM_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub data: String,
    pub event: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Default)]
struct SseDecoder {
    buf: Vec<u8>,
    data: Vec<String>,
    event: Option<String>,
    id: Option<String>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(newline) = self.buf.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buf.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if let Some(frame) = self.dispatch() {
                    frames.push(frame);
                }
                continue;
            }
            if line[0] == b':' {
                continue;
            }
            let line = String::from_utf8_lossy(&line);
            let line: &str = &line;
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };
            match field {
                "data" => self.data.push(value.to_string()),
                "event" => self.event = Some(value.to_string()),
                "id" if !value.contains('\0') => {
                    self.id = Some(value.to_string());
                }
                _ => {}
            }
        }
        frames
    }

    fn dispatch(&mut self) -> Option<SseFrame> {
        let data = std::mem::take(&mut self.data);
        let event = self.event.take();
        let id = self.id.take();
        if data.is_empty() && event.is_none() && id.is_none() {
            return None;
        }
        Some(SseFrame {
            data: data.join("\n"),
            event,
            id,
        })
    }
}

/// Byte stream with a per-chunk deadline. EOF ends the stream. A missed
/// deadline yields one [`EventStreamError::Silent`] and then ends.
pub fn watch_bytes<S, E>(stream: S, timeout: Duration) -> WatchBytes<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    WatchBytes {
        stream,
        timeout,
        pending: None,
        failed: false,
    }
}

pub struct WatchBytes<S> {
    stream: S,
    timeout: Duration,
    pending: Option<Pin<Box<tokio::time::Sleep>>>,
    failed: bool,
}

impl<S, E> Stream for WatchBytes<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    type Item = Result<Bytes, EventStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.stream).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                this.pending = None;
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.failed = true;
                this.pending = None;
                Poll::Ready(Some(Err(EventStreamError::Transport(err.to_string()))))
            }
            Poll::Ready(None) => {
                this.pending = None;
                Poll::Ready(None)
            }
            Poll::Pending => {
                if this.pending.is_none() {
                    this.pending = Some(Box::pin(tokio::time::sleep(this.timeout)));
                }
                let sleep = this.pending.as_mut().unwrap().as_mut();
                if sleep.poll(cx).is_ready() {
                    this.failed = true;
                    this.pending = None;
                    return Poll::Ready(Some(Err(EventStreamError::Silent)));
                }
                Poll::Pending
            }
        }
    }
}

/// Decodes SSE from an already-watched byte stream. Dropping the result aborts
/// the decoder task; it does not open a new request.
pub fn decode_sse<S, E>(stream: S, timeout: Duration) -> EventStream<SseFrame>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut watched = watch_bytes(stream, timeout);
        let mut decoder = SseDecoder::default();
        loop {
            match watched.next_item().await {
                None => break,
                Some(Err(err)) => {
                    let _ = tx.send(Err(err));
                    break;
                }
                Some(Ok(bytes)) => {
                    for frame in decoder.push(&bytes) {
                        if tx.send(Ok(frame)).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
    EventStream { rx, task }
}

/// Helper so `decode_sse` can await the next watched chunk without a StreamExt
/// bound that would fight the manual `Stream` impl.
impl<S, E> WatchBytes<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    async fn next_item(&mut self) -> Option<Result<Bytes, EventStreamError>> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }
}

pub struct EventStream<T> {
    rx: mpsc::UnboundedReceiver<Result<T, EventStreamError>>,
    task: JoinHandle<()>,
}

impl<T> Stream for EventStream<T> {
    type Item = Result<T, EventStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().rx.poll_recv(cx)
    }
}

impl<T> Drop for EventStream<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::time::Duration;

    #[test]
    fn comments_do_not_become_events_and_data_joins_on_newline() {
        let mut decoder = SseDecoder::default();
        let frames = decoder.push(
            b": heartbeat\n\ndata: {\"type\":\"server.connected\"}\ndata: {\"more\":true}\n\n",
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].data,
            "{\"type\":\"server.connected\"}\n{\"more\":true}"
        );
    }

    #[tokio::test]
    async fn a_quiet_byte_stream_trips_the_watchdog_before_decode() {
        let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
        let mut watched = watch_bytes(stream, Duration::from_millis(80));
        let err = watched.next_item().await.unwrap().unwrap_err();
        assert!(matches!(err, EventStreamError::Silent));
        drop(tx);
    }

    #[tokio::test]
    async fn heartbeats_reset_the_watchdog() {
        let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let mut events = decode_sse(stream, Duration::from_millis(200));
        tx.send(Ok(Bytes::from_static(b": heartbeat\n\n")))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        tx.send(Ok(Bytes::from_static(
            b"data: {\"type\":\"server.connected\",\"properties\":{}}\n\n",
        )))
        .await
        .unwrap();
        let frame = tokio::time::timeout(Duration::from_millis(400), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(frame.data.contains("server.connected"));
    }
}
