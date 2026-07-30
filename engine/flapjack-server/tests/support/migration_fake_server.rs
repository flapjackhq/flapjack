use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

pub(crate) struct RecordedRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    headers: BTreeMap<String, String>,
    pub(crate) body: Value,
}

impl RecordedRequest {
    pub(crate) fn header(&self, name: &str) -> Option<String> {
        self.headers.get(&name.to_ascii_lowercase()).cloned()
    }
}

pub(crate) struct StubResponse {
    status: u16,
    body: String,
    response_delay: Duration,
}

impl StubResponse {
    pub(crate) fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            response_delay: Duration::ZERO,
        }
    }

    pub(crate) fn text(status: u16, body: String) -> Self {
        Self {
            status,
            body,
            response_delay: Duration::ZERO,
        }
    }

    pub(crate) fn delayed_by(mut self, response_delay: Duration) -> Self {
        self.response_delay = response_delay;
        self
    }
}

pub(crate) struct FakeMigrationServer {
    bind_addr: String,
    requests: Receiver<RecordedRequest>,
}

impl FakeMigrationServer {
    pub(crate) fn start(responses: Vec<StubResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake migration server");
        let bind_addr = listener.local_addr().unwrap().to_string();
        let (sender, requests) = mpsc::channel();
        thread::spawn(move || serve_stub(listener, responses, sender));
        Self {
            bind_addr,
            requests,
        }
    }

    pub(crate) fn endpoint(&self) -> String {
        format!("http://{}", self.bind_addr)
    }

    pub(crate) fn take_requests(&self, count: usize) -> Vec<RecordedRequest> {
        (0..count)
            .map(|_| {
                self.requests
                    .recv_timeout(Duration::from_secs(5))
                    .expect("expected migration request")
            })
            .collect()
    }
}

fn serve_stub(
    listener: TcpListener,
    responses: Vec<StubResponse>,
    sender: Sender<RecordedRequest>,
) {
    for response in responses {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        handle_stub_connection(stream, response, &sender);
    }
}

fn handle_stub_connection(
    mut stream: TcpStream,
    response: StubResponse,
    sender: &Sender<RecordedRequest>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap().to_string();
    let path = request_parts.next().unwrap().to_string();
    let mut headers = BTreeMap::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body_bytes = vec![0; content_length];
    reader.read_exact(&mut body_bytes).unwrap();
    let body = if body_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap()
    };
    sender
        .send(RecordedRequest {
            method,
            path,
            headers,
            body,
        })
        .unwrap();
    thread::sleep(response.response_delay);

    let reason = if response.status < 400 { "OK" } else { "ERR" };
    let wire_response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        reason,
        response.body.len(),
        response.body
    );
    stream.write_all(wire_response.as_bytes()).unwrap();
}
