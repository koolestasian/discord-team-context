//! A tiny HTTP server on 127.0.0.1 for adapter tests. It answers one scripted response
//! per connection and records every request. No real provider is ever contacted.

#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub(crate) method: String,
    pub(crate) path: String,
    /// Header names are lowercased.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl Captured {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

/// What the server does with a request.
#[derive(Debug, Clone)]
pub(crate) enum Behaviour {
    /// Reply with this status and body.
    Respond { status: u16, body: String },
    /// Reply with a redirect to `location`.
    Redirect { location: String },
    /// Read the request, then never answer.
    Hang,
}

impl Behaviour {
    pub(crate) fn ok(body: impl Into<String>) -> Self {
        Self::Respond {
            status: 200,
            body: body.into(),
        }
    }

    pub(crate) fn status(status: u16, body: impl Into<String>) -> Self {
        Self::Respond {
            status,
            body: body.into(),
        }
    }
}

pub(crate) struct MockServer {
    address: String,
    captured: Arc<Mutex<Vec<Captured>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MockServer {
    pub(crate) fn start(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let captured = Arc::clone(&captured);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    handle(stream, &behaviour, &captured, &stop);
                }
            })
        };
        Self {
            address,
            captured,
            stop,
            thread: Some(thread),
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(crate) fn requests(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock `accept`.
        let _ = TcpStream::connect(&self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(
    stream: TcpStream,
    behaviour: &Behaviour,
    captured: &Mutex<Vec<Captured>>,
    stop: &AtomicBool,
) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    captured.lock().unwrap().push(Captured {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let mut stream = stream;
    match behaviour {
        Behaviour::Respond { status, body } => {
            let head = format!(
                "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
        Behaviour::Redirect { location } => {
            let head = format!(
                "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
        Behaviour::Hang => {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
