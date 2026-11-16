//! A loopback HTTP/1.1 server for unit tests: it answers connections with
//! canned responses or a caller-provided stream handler, written as raw bytes
//! so tests can send truncated bodies, stall mid-response, or drop sockets, and
//! hands back what each request sent.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub struct Request {
    pub head: String,
    pub body: Vec<u8>,
}

/// Serves `responses` in order, one per connection. Returns the base URL
/// (`http://127.0.0.1:port`) and a handle yielding the requests received.
pub fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    (format!("http://{address}"), serve_on(listener, responses))
}

/// [`serve`] on a listener the test bound earlier.
pub fn serve_on(
    listener: TcpListener,
    responses: Vec<String>,
) -> std::thread::JoinHandle<Vec<Request>> {
    std::thread::spawn(move || {
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
    })
}

/// Serves an unbounded sequence of connections on `127.0.0.1:0`, reading each
/// complete HTTP/1.1 request on its own thread and passing `(request, &mut stream)`
/// to `handler`. Returning without writing drops the socket after receiving the request.
pub fn serve_stream(handler: impl Fn(Request, &mut TcpStream) + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handler = std::sync::Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let handler = handler.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let req = read_request(&mut stream);
                if !req.head.is_empty() {
                    handler(req, &mut stream);
                }
            });
        }
    });
    format!("http://{address}")
}

/// Accepts a single connection on `127.0.0.1:0`, reads the complete HTTP/1.1
/// request, and runs `handler` on the open stream.
pub fn serve_once(handler: impl FnOnce(Request, &mut TcpStream) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let req = read_request(&mut stream);
        handler(req, &mut stream);
    });
    format!("http://{address}/")
}

/// A complete JSON 200 OK response.
pub fn json_response(body: &str) -> String {
    status_response(200, "OK", "application/json", body)
}

/// A complete HTTP/1.1 response with explicit status and content type.
pub fn status_response(status: u16, reason: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn read_request(stream: &mut TcpStream) -> Request {
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    let body_start = loop {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break data.len(),
            Ok(n) => {
                data.extend_from_slice(&buffer[..n]);
                if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            }
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
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => data.extend_from_slice(&buffer[..n]),
        }
    }
    let body_end = (body_start + length).min(data.len());
    Request {
        head,
        body: data[body_start..body_end].to_vec(),
    }
}
