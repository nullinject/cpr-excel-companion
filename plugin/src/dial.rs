//! 控制通道直连：trustedProcess 插件是宿主同机的原生进程，
//! 宿主受管 HTTP 禁止回环/私网地址，控制平面与观察转发改为进程内直连桥接。
use std::time::Duration;

pub struct DirectReply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 极小 HTTP/1.1 客户端，仅覆盖桥接控制平面的 POST 用法。
/// URL 限 http；响应按 Connection: close 读到 EOF。
pub async fn post(
    url: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<DirectReply, String> {
    let (host, port, path) = parse(url).ok_or("control url must be http://host:port/path")?;
    let connect = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::net::TcpStream::connect((host.as_str(), port)).await
    })
    .await
    .map_err(|_| "control connect timed out".to_owned())?
    .map_err(|e| format!("control connect failed: {e}"))?;
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nhost: {host}:{port}\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = connect;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("control write failed: {e}"))?;
    stream
        .write_all(&body)
        .await
        .map_err(|e| format!("control write failed: {e}"))?;
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut raw))
        .await
        .map_err(|_| "control read timed out".to_owned())?
        .map_err(|e| format!("control read failed: {e}"))?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("invalid control response")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("invalid control status line")?;
    Ok(DirectReply {
        status,
        body: raw[split + 4..].to_vec(),
    })
}

fn parse(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, 80),
    };
    Some((host.to_owned(), port, path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_control_urls() {
        assert_eq!(
            parse("http://127.0.0.1:8089/_control"),
            Some(("127.0.0.1".into(), 8089, "/_control".into()))
        );
        assert_eq!(
            parse("http://bridge.example:8089"),
            Some(("bridge.example".into(), 8089, "/".into()))
        );
        assert_eq!(parse("https://example.invalid/x"), None);
    }
}
