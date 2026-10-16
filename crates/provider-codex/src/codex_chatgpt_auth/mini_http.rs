//! A small HTTP/1.1 reader and writer for the loopback callback and the tests'
//! mock token server. Both speak to `reqwest` on `127.0.0.1` only.

use std::collections::HashMap;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub struct MiniRequest {
    pub method: String,
    pub target: String,
    /// Read by the mock token server. The loopback callback only needs the target.
    #[allow(dead_code)]
    pub headers: HashMap<String, String>,
    #[allow(dead_code)]
    pub body: Vec<u8>,
}

/// The largest body read. The loopback callback is a GET and the mock token requests are
/// small forms, so anything bigger is refused rather than buffered.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// How long a peer may take to send its request before it is dropped.
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `read_request` with a deadline, for sockets that face the network.
pub async fn read_request_within(
    socket: &mut TcpStream,
    timeout: std::time::Duration,
) -> std::io::Result<MiniRequest> {
    tokio::time::timeout(timeout, read_request(socket))
        .await
        .unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request not received in time",
            ))
        })
}

pub async fn read_request(socket: &mut TcpStream) -> std::io::Result<MiniRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "closed before headers",
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = find_subsequence(&buf, b"\r\n\r\n") {
            break end;
        }
        if buf.len() > 65_536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        ));
    }
    while body.len() < length {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(length);
    Ok(MiniRequest {
        method,
        target,
        headers,
        body,
    })
}

pub async fn write_response(
    socket: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    let _ = socket.shutdown().await;
    Ok(())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn a_stalled_peer_is_dropped_at_the_deadline() {
        let (_client, mut server) = pair().await;
        let error = read_request_within(&mut server, std::time::Duration::from_millis(50))
            .await
            .err()
            .expect("timeout");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let (mut client, mut server) = pair().await;
        client
            .write_all(
                format!(
                    "POST /x HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
                    MAX_BODY_BYTES + 1
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let error = read_request(&mut server).await.err().expect("refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_small_request_reads_with_its_body() {
        let (mut client, mut server) = pair().await;
        client
            .write_all(b"POST /x HTTP/1.1\r\ncontent-length: 2\r\n\r\nhi")
            .await
            .unwrap();
        let request = read_request_within(&mut server, READ_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(
            (request.method.as_str(), request.target.as_str()),
            ("POST", "/x")
        );
        assert_eq!(request.body, b"hi");
    }
}
