//! Prompts are ledger identity: a task resumed in another interface language
//! must find its receipts instead of paying for the same requests again.

use course2md::dispatch::{ControlSource, TaskControl};
use course2md::i18n::{self, Lang};
use course2md::llm::{self, LlmSettings};
use course2md::timeline::{Section, TranscriptEvent};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Answers every request with one valid proofreading result and records bodies.
fn serve() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let body = read_body(&mut stream);
            seen.lock().unwrap().push(body);
            let content = r#"{\"segments\":[{\"id\":0,\"text\":\"校对后\"}]}"#;
            let reply = format!(r#"{{"choices":[{{"message":{{"content":"{content}"}}}}]}}"#);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (format!("http://{address}/v1"), bodies)
}

fn read_body(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map_or(0, |value| value.trim().parse().unwrap());
            if data.len() >= end + 4 + length {
                return String::from_utf8_lossy(&data[end + 4..end + 4 + length]).into_owned();
            }
        }
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "connection closed mid-request");
        data.extend_from_slice(&chunk[..read]);
    }
}

fn sections() -> Vec<Section> {
    vec![Section {
        t: 0.,
        end: 1.,
        image: "slide.jpg".into(),
        speech: vec![TranscriptEvent {
            start: 0.,
            end: 1.,
            text: "原文".into(),
            raw: None,
            translation: None,
        }],
    }]
}

fn polish(settings: &LlmSettings, frames: &Path, control: &TaskControl) {
    let report = llm::polish_sections_report(&mut sections(), frames, settings, control).unwrap();
    assert_eq!(report.succeeded, 1, "{:?}", report.note);
}

// One test: the interface language is process-wide state.
#[test]
fn requests_do_not_vary_with_the_interface_language() {
    let (base_url, bodies) = serve();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("slide.jpg"), b"not decoded").unwrap();
    let languages = [Lang::ZhHans, Lang::En, Lang::ZhHant, Lang::Ja];
    for vision in [false, true] {
        let settings = LlmSettings {
            base_url: base_url.clone(),
            model: "m".into(),
            vision,
            concurrency: 1,
            ..Default::default()
        };

        // Each language sends what the others send, connection test included.
        let requests = |lang: Lang| {
            i18n::set_lang(lang);
            bodies.lock().unwrap().clear();
            polish(&settings, dir.path(), &TaskControl::default());
            llm::test_connection(&settings).unwrap();
            bodies.lock().unwrap().clone()
        };
        let expected = requests(Lang::ZhHans);
        assert_eq!(expected.len(), 2);
        for lang in languages {
            assert_eq!(requests(lang), expected, "{lang:?}");
        }

        // A completed request is found again after the language changes.
        let work = tempfile::tempdir().unwrap();
        let control = TaskControl::open(work.path(), ControlSource::default()).unwrap();
        bodies.lock().unwrap().clear();
        for lang in languages {
            i18n::set_lang(lang);
            polish(&settings, dir.path(), &control);
        }
        assert_eq!(
            bodies.lock().unwrap().len(),
            1,
            "resent after a language change"
        );
    }
}
