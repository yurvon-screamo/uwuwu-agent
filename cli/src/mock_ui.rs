//! In-process mock of the AFFiNE UI API for unit tests: serves sign-in and
//! GraphQL requests from a step queue (same TcpListener pattern as the MCP
//! MockGateway in tests/mcp_transport.rs). Connection: close per response —
//! reqwest opens a fresh connection for every step, the queue order is the
//! contract.

#![cfg(test)]

use std::io::{Read, Write};
use std::net::TcpListener;

pub(crate) struct UiStep {
    pub status: u16,
    pub body: String,
}

impl UiStep {
    pub fn ok(body: String) -> Self {
        Self { status: 200, body }
    }

    pub fn status(status: u16, body: String) -> Self {
        Self { status, body }
    }
}

pub(crate) struct MockUi {
    addr: std::net::SocketAddr,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockUi {
    pub fn start(steps: Vec<UiStep>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            for step in steps {
                let stream = loop {
                    if std::time::Instant::now() > deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                };
                serve(stream, &step, &log);
            }
        });
        MockUi { addr, requests }
    }

    pub fn config(&self) -> crate::affine_search_server::ServerSearchConfig {
        crate::affine_search_server::ServerSearchConfig {
            ui_url: format!("http://{}", self.addr),
            email: "bot@example.test".into(),
            password: "pw".into(),
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Тела всех запросов (для ассертов на docTitle/query-контент).
    pub fn request_bodies(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(mut stream: std::net::TcpStream, step: &UiStep, log: &std::sync::Mutex<Vec<String>>) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let head = String::from_utf8_lossy(&buf);
                if let Some(headers_end) = head.find("\r\n\r\n") {
                    let content_length = head[..headers_end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    log.lock()
        .unwrap()
        .push(String::from_utf8_lossy(&buf).to_string());
    let reason = if step.status == 200 { "OK" } else { "Status" };
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        step.status,
        reason,
        step.body.len(),
        step.body
    );
    stream.write_all(resp.as_bytes()).unwrap();
    stream.flush().unwrap();
}
