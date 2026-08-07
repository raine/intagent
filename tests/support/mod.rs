use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub target: String,
    pub headers: String,
    pub body: Value,
}

pub struct FixtureServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl FixtureServer {
    pub async fn start(build: impl FnOnce(&str) -> Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let responses = build(&base_url);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut responses: VecDeque<Value> = responses.into();
            while let Some(response) = responses.pop_front() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut buffer = [0_u8; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "fixture request ended before its headers");
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(index) = find_bytes(&bytes, b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + content_length {
                    let mut buffer = [0_u8; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "fixture request ended before its body");
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let first_line = headers.lines().next().unwrap();
                let mut first_line = first_line.split_whitespace();
                let method = first_line.next().unwrap().to_owned();
                let target = first_line.next().unwrap().to_owned();
                let body = if content_length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
                };
                captured.lock().unwrap().push(RecordedRequest {
                    method,
                    target,
                    headers,
                    body,
                });
                let body = serde_json::to_vec(&response).unwrap();
                let response_headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response_headers.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        Self {
            base_url,
            requests,
            task,
        }
    }

    pub async fn finish(self) -> Vec<RecordedRequest> {
        self.task.await.unwrap();
        Arc::try_unwrap(self.requests)
            .unwrap()
            .into_inner()
            .unwrap()
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
