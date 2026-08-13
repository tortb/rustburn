//! 集成测试公共工具：基于标准库 TcpListener 的本地 mock HTTP server。
//!
//! 用于在不依赖真实网络的前提下验证 OSV 查询与更新下载链路。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 简单的本地 HTTP mock server。
///
/// handler 接收 (method, path, body) 并返回 (status, content_type, response_body)。
/// 每个连接上循环处理多个请求，直到客户端关闭。
pub struct MockServer {
    /// base URL，例如 `http://127.0.0.1:PORT`
    pub addr: String,
    request_count: Arc<AtomicUsize>,
}

impl MockServer {
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&str, &str, &str) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        listener.set_nonblocking(false).expect("set blocking");
        let addr = format!("http://{}", listener.local_addr().expect("local addr"));
        let request_count = Arc::new(AtomicUsize::new(0));
        let count = request_count.clone();
        let handler = Arc::new(handler);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let count = count.clone();
                let handler = handler.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, count, handler.as_ref());
                });
            }
        });

        MockServer {
            addr,
            request_count,
        }
    }

    /// 当前已收到的请求数。
    pub fn requests(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

fn handle_connection<F>(
    mut stream: TcpStream,
    count: Arc<AtomicUsize>,
    handler: &F,
) -> std::io::Result<()>
where
    F: Fn(&str, &str, &str) -> (u16, &'static str, Vec<u8>),
{
    // 在一个连接上循环处理多个请求（ureq 会复用 keep-alive 连接）
    loop {
        let Some((method, path, body)) = read_request(&mut stream)? else {
            return Ok(());
        };
        count.fetch_add(1, Ordering::SeqCst);
        let (status, content_type, resp_body) = handler(&method, &path, &body);
        write_response(&mut stream, status, content_type, &resp_body)?;
    }
}

/// 读取一个 HTTP 请求：请求行 + 头 + 按 Content-Length 读取 body。
/// 连接关闭（读不到数据）时返回 Ok(None)。
fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<(String, String, String)>> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;

    let mut data = Vec::new();
    let mut buf = [0u8; 4096];

    // 读取直到头部结束（\r\n\r\n）
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(None); // 连接关闭
        }
        data.extend_from_slice(&buf[..n]);
        if data.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if data.len() > 1024 * 1024 {
            return Ok(None); // 过大的请求，防御性丢弃
        }
    }

    let header_end = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(data.len());

    let head = String::from_utf8_lossy(&data[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    // 读取 body 剩余部分
    let mut body = data[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    body.truncate(content_length);
    let body = String::from_utf8_lossy(&body).to_string();

    Ok(Some((method, path, body)))
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}
