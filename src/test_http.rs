//! A loopback HTTP/1.1 server for unit tests: it answers each connection with
//! the next canned response, written as raw bytes so tests can send truncated
//! bodies, and hands back what each request sent.

use std::io::{Read, Write};
use std::time::Duration;

pub struct Request {
    pub head: String,
    pub body: Vec<u8>,
}

/// Serves `responses` in order, one per connection. Returns the base URL
/// (`http://127.0.0.1:port`) and a handle yielding the requests received.
pub fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<Request>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            requests.push(read_request(&mut stream));
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (format!("http://{address}"), server)
}

/// A complete JSON response.
pub fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn read_request(stream: &mut std::net::TcpStream) -> Request {
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    let body_start = loop {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            break data.len();
        }
        data.extend_from_slice(&buffer[..n]);
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..body_start]).into_owned();
    let length: usize = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < body_start + length {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..n]);
    }
    Request {
        head,
        body: data[body_start..].to_vec(),
    }
}
