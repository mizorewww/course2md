//! Prompts are ledger identity: a task resumed in another interface language
//! must find its receipts instead of paying for the same requests again.

use course2md::dispatch::{ControlSource, TaskControl};
use course2md::i18n::{self, Lang};
use course2md::llm::{self, LlmProvider, LlmSettings};
use course2md::timeline::{Section, TranscriptEvent};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// The interface language is process-wide state, so tests that set it take turns.
fn language_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

#[test]
fn requests_do_not_vary_with_the_interface_language() {
    let _language = language_lock();
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

/// Proofreading stable ids that 2.0.1 (`3db4a88`) saved for [`former_sections`] with
/// [`former_settings`], by (vision, custom prompt, interface language). They were taken
/// by running that commit's `llm::polish_sections_report` in each language; Ollama gave
/// the same ids. Simplified Chinese was also what `--json` and `run-task` sent.
const FORMER_IDS: [(bool, bool, Lang, &str); 16] = [
    (
        false,
        false,
        Lang::ZhHans,
        "b1f173390116abbe3564d1a2936ebfe01db17de10acfc524083e86b62762765b",
    ),
    (
        false,
        false,
        Lang::En,
        "5df12144b6b314c0660e342208e89cb16bf3b33b9bf2fa332663e91432841f67",
    ),
    (
        false,
        false,
        Lang::ZhHant,
        "380e487640498cd5b6092e1f79fc78fa4d3bbdf9c00c81b8c4e4643974b9f967",
    ),
    (
        false,
        false,
        Lang::Ja,
        "d87960e1da77050c413b0c36c3e524a545bf0454c668144eacec857efde71733",
    ),
    (
        false,
        true,
        Lang::ZhHans,
        "51ca5a34a9b83138f7c92e59779a63ee74c130b0962354949f81f5aba1120f2e",
    ),
    (
        false,
        true,
        Lang::En,
        "ff058df9c87deb3d2025a7ccc2b82b0e18efbc18096e1219dc17f9b800d3de42",
    ),
    (
        false,
        true,
        Lang::ZhHant,
        "5c9049e5cc0ecf7fbfeeeb8d943896551c7a05b2812e15686ebbe8d06f28391e",
    ),
    (
        false,
        true,
        Lang::Ja,
        "5a2122301e243985ce8d2ac5754c9d30f0fc1950bef29c10ecca09f4b26b19d2",
    ),
    (
        true,
        false,
        Lang::ZhHans,
        "ef48ee65593fa318ec9181e363487fdfebd0f95efb56cd55a3398e374fd91b24",
    ),
    (
        true,
        false,
        Lang::En,
        "0bb353edf5f82889b096abbd16ad2c10eaf7c45520c7e87fc907071e68a8d6b8",
    ),
    (
        true,
        false,
        Lang::ZhHant,
        "c45c2f8be2d096f57a6939bdec8cc2564b6bb6077abc18449b3bc9d67a8f9897",
    ),
    (
        true,
        false,
        Lang::Ja,
        "5a37e7e7ab73c002eafed2dc5f8d89dd1ea76c3ddcd956a515f695486316f99c",
    ),
    (
        true,
        true,
        Lang::ZhHans,
        "94ce89d5f3c85cdfac220733ec8ffea9653a898525da139041a236397f3c1c6a",
    ),
    (
        true,
        true,
        Lang::En,
        "2ad3e368f675341f76ed916af46f437655dce2191c4bafd40d9eacbb9c64ec6d",
    ),
    (
        true,
        true,
        Lang::ZhHant,
        "5d2a4c25f2670a259b4fdfc4bcaa465806ff0dcae02b9918f61ad5feb5058170",
    ),
    (
        true,
        true,
        Lang::Ja,
        "f4e7368a2f83ef0648afa2503147ee63c7db737e7876387f3a2d92ad64314f39",
    ),
];

fn former_sections() -> Vec<Section> {
    let event = |start: f64, text: &str| TranscriptEvent {
        start,
        end: start + 1.,
        text: text.into(),
        raw: None,
        translation: None,
    };
    vec![Section {
        t: 0.,
        end: 2.,
        image: "slide.jpg".into(),
        speech: vec![event(0., "原文"), event(1., "second line")],
    }]
}

/// Nothing listens on the discard port: a request that is sent fails, and its
/// receipt records that it was not sent.
fn former_settings(provider: LlmProvider, vision: bool, prompt: bool) -> LlmSettings {
    LlmSettings {
        provider,
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "m".into(),
        api_key: "k".into(),
        vision,
        prompt: prompt.then(|| "UX-RULE-42".into()),
        concurrency: 1,
        ..Default::default()
    }
}

fn write_receipt(work: &Path, stable_id: &str, state: &str) {
    let content = r#"{"segments":[{"id":0,"text":"校对后"},{"id":1,"text":"Second line."}]}"#;
    let receipt = serde_json::json!({
        "schema": 1,
        "stable_id": stable_id,
        "request_id": format!("{stable_id}.1"),
        "purpose": "proofreading",
        "description": "",
        "service_version": "snapshot:former",
        "attempt": 1,
        "state": state,
        "http_status": (state == "completed").then_some(200),
        "response": (state == "completed")
            .then(|| serde_json::json!({"choices":[{"message":{"content":content}}]})),
        "message": null,
    });
    let dir = work.join("requests");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{stable_id}.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}

fn receipt_state(work: &Path, stable_id: &str) -> String {
    let path = work.join("requests").join(format!("{stable_id}.json"));
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    receipt["state"].as_str().unwrap().to_owned()
}

fn receipt_ids(work: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(work.join("requests"))
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name();
            name.to_string_lossy().trim_end_matches(".json").to_owned()
        })
        .collect();
    ids.sort();
    ids
}

/// A task begun by 2.0.1 in any interface language finds that version's receipts,
/// whatever language it resumes in, and new tasks keep 2.0.1's Simplified Chinese ids.
#[test]
fn receipts_saved_by_2_0_1_are_found_in_every_language() {
    let _language = language_lock();
    let frames = tempfile::tempdir().unwrap();
    std::fs::write(frames.path().join("slide.jpg"), b"not decoded").unwrap();
    let languages = [Lang::ZhHans, Lang::En, Lang::ZhHant, Lang::Ja];
    for provider in [LlmProvider::OpenAiCompatible, LlmProvider::Ollama] {
        for &(vision, prompt, started_in, former_id) in &FORMER_IDS {
            let settings = former_settings(provider, vision, prompt);
            let case = format!("{provider:?} vision={vision} prompt={prompt} {started_in:?}");
            let polish = |control: &TaskControl| {
                llm::polish_sections_report(
                    &mut former_sections(),
                    frames.path(),
                    &settings,
                    control,
                )
                .unwrap()
            };

            // Completed under 2.0.1: replayed in every language, nothing sent.
            let work = tempfile::tempdir().unwrap();
            write_receipt(work.path(), former_id, "completed");
            for lang in languages {
                i18n::set_lang(lang);
                let control = TaskControl::open(work.path(), ControlSource::default()).unwrap();
                let report = polish(&control);
                assert_eq!(
                    report.succeeded, 2,
                    "{case}, resumed in {lang:?}: {:?}",
                    report.note
                );
            }
            assert_eq!(receipt_ids(work.path()), [former_id], "{case}");

            // In flight when 2.0.1 stopped: still unresolved, never sent again unasked.
            // (Ollama is free, so it sends again, as 2.0.1 did.)
            if provider == LlmProvider::OpenAiCompatible {
                let work = tempfile::tempdir().unwrap();
                write_receipt(work.path(), former_id, "sending");
                i18n::set_lang(Lang::En);
                let control = TaskControl::open(work.path(), ControlSource::default()).unwrap();
                assert_eq!(polish(&control).succeeded, 0, "{case}");
                assert_eq!(receipt_ids(work.path()), [former_id], "{case}");
                assert_eq!(receipt_state(work.path(), former_id), "uncertain", "{case}");
            }
        }
    }

    // A new task's request has 2.0.1's Simplified Chinese id, in whichever language: the
    // replays above show the language does not change it. Each send fails and is retried,
    // so each case runs in one language only.
    let zh_hans = FORMER_IDS
        .iter()
        .filter(|(_, _, lang, _)| *lang == Lang::ZhHans);
    for (&(vision, prompt, _, former_id), lang) in zh_hans.zip(languages) {
        i18n::set_lang(lang);
        let settings = former_settings(LlmProvider::OpenAiCompatible, vision, prompt);
        let work = tempfile::tempdir().unwrap();
        let control = TaskControl::open(work.path(), ControlSource::default()).unwrap();
        let report =
            llm::polish_sections_report(&mut former_sections(), frames.path(), &settings, &control)
                .unwrap();
        assert_eq!(report.succeeded, 0);
        let case = format!("vision={vision} prompt={prompt} {lang:?}");
        assert_eq!(receipt_ids(work.path()), [former_id], "{case}");
        assert_eq!(receipt_state(work.path(), former_id), "not_sent", "{case}");
    }
}
