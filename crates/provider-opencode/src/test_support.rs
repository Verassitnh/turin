//! In-process HTTP/SSE server for transport tests.
#![cfg(test)]

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    pub authorization: Option<String>,
    pub directory_header: Option<String>,
    #[allow(dead_code)]
    pub accept: Option<String>,
    pub body: String,
}

pub struct FakeResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// Write headers, then these chunks, then hold the socket open.
    pub stream: Option<Vec<Vec<u8>>>,
    pub hold: bool,
}

impl FakeResponse {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: Some("application/json".to_string()),
            body: body.as_bytes().to_vec(),
            stream: None,
            hold: false,
        }
    }

    pub fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: None,
            body: Vec::new(),
            stream: None,
            hold: false,
        }
    }

    pub fn sse(chunks: Vec<Vec<u8>>, hold: bool) -> Self {
        Self {
            status: 200,
            content_type: Some("text/event-stream".to_string()),
            body: Vec::new(),
            stream: Some(chunks),
            hold,
        }
    }
}

pub struct FakeServer {
    pub url: String,
    pub requests: mpsc::UnboundedReceiver<Recorded>,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

impl FakeServer {
    pub async fn spawn(handler: impl Fn(Recorded) -> FakeResponse + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown, mut watch_rx) = watch::channel(false);
        let (tx, rx) = mpsc::unbounded_channel();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = watch_rx.changed() => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let handler = Arc::clone(&handler);
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let _ = handle_conn(socket, handler, tx).await;
                        });
                    }
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            requests: rx,
            shutdown,
            task,
        }
    }
}

async fn handle_conn(
    mut socket: tokio::net::TcpStream,
    handler: Arc<dyn Fn(Recorded) -> FakeResponse + Send + Sync>,
    tx: mpsc::UnboundedSender<Recorded>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut authorization = None;
    let mut directory_header = None;
    let mut accept = None;
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("x-opencode-directory") {
            directory_header = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("accept") {
            accept = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
    }
    while buf.len() - header_end < content_length {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(
        &buf[header_end..header_end + content_length.min(buf.len() - header_end)],
    )
    .to_string();
    let recorded = Recorded {
        method,
        target,
        authorization,
        directory_header,
        accept,
        body,
    };
    let _ = tx.send(recorded.clone());
    let response = handler(recorded);
    let reason = if response.status == 204 {
        "No Content"
    } else if response.status == 401 {
        "Unauthorized"
    } else {
        "OK"
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", response.status);
    if let Some(content_type) = &response.content_type {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    if response.stream.is_none() {
        head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    socket.write_all(head.as_bytes()).await?;
    if let Some(chunks) = &response.stream {
        for chunk in chunks {
            socket.write_all(chunk).await?;
            socket.flush().await?;
        }
    } else {
        socket.write_all(&response.body).await?;
    }
    if response.hold {
        let _ = socket.read(&mut tmp).await;
    }
    Ok(())
}
