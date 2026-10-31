//! Explicit, single-request service contract tests. Never call this from save or generation.
//!
//! The shipped samples contain no course data. HTTP success alone is not a test pass; the
//! response must satisfy the actual feature's structure and the known sample. Redirects,
//! speculative protocol fallbacks and automatic retries are deliberately disabled.

use crate::credentials::{CredentialVault, Secret};
use crate::preferences::{
    Authentication, ServiceConfiguration, ServiceProtocol, ServiceTestEvidence, TestOutcome,
    normalize_endpoint,
};
use base64::Engine as _;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SPEECH_SAMPLE: &[u8] = include_bytes!("../assets/tests/service-speech.wav");
const BLUE_CARD: &[u8] = include_bytes!("../assets/tests/service-blue-card.png");
const MAX_RESPONSE: u64 = 128 * 1024;

pub fn test_notice() -> &'static str {
    course2md::tr!("检查仅发送内置示例，服务商可能计费")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    Speech,
    Proofread,
    Summary,
    Vision,
}

impl TestKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Speech => course2md::tr!("语音识别"),
            Self::Proofread => course2md::tr!("AI 校对"),
            Self::Summary => course2md::tr!("生成摘要"),
            Self::Vision => course2md::tr!("截图辅助校对"),
        }
    }

    pub fn contract(self) -> &'static str {
        match self {
            Self::Speech => "speech-en-blue-notebook-seven-pages-v1",
            Self::Proofread => "proofread-en-two-segment-ids-v1",
            Self::Summary => "summary-en-tldr-points-outline-v1",
            Self::Vision => "vision-en-blue-card-segment-id-v1",
        }
    }

    fn accepts(self, protocol: ServiceProtocol) -> bool {
        match self {
            Self::Speech => matches!(
                protocol,
                ServiceProtocol::SpeechTranscriptions | ServiceProtocol::SpeechChat
            ),
            // Ollama 与 AiChat 同为 chat/completions 方言；Codex 由 test_codex_blocking 覆盖
            _ => matches!(
                protocol,
                ServiceProtocol::AiChat | ServiceProtocol::OllamaChat
            ),
        }
    }
}

/// A recorded, explicit configuration refusal can stop a repair from immediately
/// resending course data. It is not a prerequisite for saving a service or for
/// trying an untested configuration. Authentication applies to the connection;
/// model refusals remain scoped to the capabilities being repaired.
pub fn blocks_reprocessing(
    evidence: &ServiceTestEvidence,
    config: &ServiceConfiguration,
    required_contracts: &[&str],
) -> bool {
    if evidence.fingerprint != config.fingerprint(&evidence.contract) {
        return false;
    }
    // A rate limit or server failure says nothing about the configuration.
    if evidence
        .http_status
        .is_some_and(|status| status == 429 || status >= 500)
    {
        return false;
    }
    match evidence.outcome {
        TestOutcome::AuthenticationRefused => true,
        TestOutcome::ModelRefused => required_contracts.contains(&evidence.contract.as_str()),
        _ => false,
    }
}

/// Runs one capability check on the calling thread; call it from a blocking IO thread.
/// A request already sent cannot be retracted: only attach the result if the
/// configuration fingerprint still matches.
pub fn test_service_blocking(
    config: &ServiceConfiguration,
    kind: TestKind,
    vault: &dyn CredentialVault,
    cancelled: &AtomicBool,
) -> ServiceTestEvidence {
    if config.protocol == ServiceProtocol::CodexResponses {
        test_codex(config, kind, cancelled)
    } else {
        run_test(config, kind, vault, cancelled, &HttpTransport)
    }
}

/// Checks several capabilities in order and stops at the first that does not pass.
/// Codex sends the same text request for proofreading and summaries, so one
/// text check answers both.
pub fn test_services_blocking(
    config: &ServiceConfiguration,
    kinds: &[TestKind],
    vault: &dyn CredentialVault,
    cancelled: &AtomicBool,
) -> Vec<(TestKind, ServiceTestEvidence)> {
    let mut results: Vec<(TestKind, ServiceTestEvidence)> = Vec::new();
    for &kind in kinds {
        let shared = (config.protocol == ServiceProtocol::CodexResponses
            && kind != TestKind::Vision)
            .then(|| {
                results
                    .iter()
                    .find(|(tested, _)| *tested != TestKind::Vision)
                    .map(|(_, evidence)| evidence.clone())
            })
            .flatten();
        let evidence = match shared {
            Some(text_check) => ServiceTestEvidence {
                fingerprint: config.fingerprint(kind.contract()),
                contract: kind.contract().into(),
                message: passed_message(kind),
                ..text_check
            },
            None => test_service_blocking(config, kind, vault, cancelled),
        };
        let passed = evidence.outcome == TestOutcome::Passed;
        results.push((kind, evidence));
        if !passed || cancelled.load(Ordering::Acquire) {
            break;
        }
    }
    results
}

fn passed_message(kind: TestKind) -> String {
    course2md::tr!("{}测试通过", kind.label())
}

/// Unsent evidence for this check; `None` when cancellation already stops it.
fn new_evidence(
    config: &ServiceConfiguration,
    kind: TestKind,
    cancelled: &AtomicBool,
) -> Result<ServiceTestEvidence, ServiceTestEvidence> {
    let mut evidence = ServiceTestEvidence {
        fingerprint: config.fingerprint(kind.contract()),
        contract: kind.contract().into(),
        tested_at: crate::store::now(),
        outcome: TestOutcome::NotSent,
        message: String::new(),
        details: vec![
            course2md::tr!("用途：{}", kind.label()),
            course2md::tr!("请求模型：{}", config.model),
        ],
        http_status: None,
    };
    if cancelled.load(Ordering::Acquire) {
        evidence.message = cancelled_message().into();
        return Err(evidence);
    }
    Ok(evidence)
}

fn cancelled_message() -> &'static str {
    course2md::tr!("测试已取消，尚未发送请求")
}

/// Codex uses the CLI's Responses/SSE dialect (course2md::llm::test_connection); the
/// desktop chat/completions transport does not apply. The proofreading structure is
/// checked again during conversion, which keeps the original text on failure.
fn test_codex(
    config: &ServiceConfiguration,
    kind: TestKind,
    cancelled: &AtomicBool,
) -> ServiceTestEvidence {
    let mut evidence = match new_evidence(config, kind, cancelled) {
        Ok(evidence) => evidence,
        Err(cancelled) => return cancelled,
    };
    if kind == TestKind::Speech || config.model.trim().is_empty() {
        evidence.message = course2md::tr!("所选服务不支持这项测试，尚未发送请求").into();
        return evidence;
    }
    let settings = course2md::llm::LlmSettings {
        enabled: true,
        provider: course2md::llm::LlmProvider::Codex,
        model: config.model.clone(),
        vision: kind == TestKind::Vision,
        ..Default::default()
    };
    match course2md::llm::test_connection(&settings) {
        Ok(()) => {
            evidence.outcome = TestOutcome::Passed;
            evidence.message = passed_message(kind);
        }
        Err(error) => {
            let failure = codex_failure(&error);
            evidence.http_status = failure.http_status;
            evidence.message = match failure.outcome {
                TestOutcome::AuthenticationRefused => {
                    course2md::tr!("Codex 登录已失效或尚未连接，请重新连接账号").into()
                }
                TestOutcome::OutcomeUnknown => unknown_result_message().into(),
                _ => course2md::tr!("服务有回应但未通过检查，请确认模型支持此用途").into(),
            };
            evidence.outcome = failure.outcome;
            evidence
                .details
                .push(format!("{error:#}").chars().take(240).collect::<String>());
        }
    }
    evidence
}

struct CodexFailure {
    outcome: TestOutcome,
    http_status: Option<u16>,
}

/// One status classification for both Codex and OpenAI-compatible tests.
/// A rate limit, server error or transport loss leaves the outcome unknown;
/// explicit credential, model or parameter rejections are configuration errors.
fn classify_status(status: Option<u16>, model_error: bool, transport: bool) -> TestOutcome {
    match status {
        Some(401 | 403) => TestOutcome::AuthenticationRefused,
        Some(404) if model_error => TestOutcome::ModelRefused,
        Some(400 | 422) if model_error => TestOutcome::ModelRefused,
        Some(429 | 500..=599) => TestOutcome::OutcomeUnknown,
        None if transport => TestOutcome::OutcomeUnknown,
        _ => TestOutcome::ContractMismatch,
    }
}

/// A missing or refused login is a configuration refusal and blocks a repair.
/// Transport failures, timeouts, rate limits and server errors leave the result
/// unknown; any other answer did not satisfy the check.
fn codex_failure(error: &anyhow::Error) -> CodexFailure {
    let login = error.chain().any(|cause| {
        cause
            .downcast_ref::<course2md::login::codex::CodexLoginRequired>()
            .is_some()
    });
    let http_status = error.chain().find_map(|cause| match cause.downcast_ref() {
        Some(course2md::llm::RequestError::Status(status)) => Some(*status),
        _ => None,
    });
    let transport = error.chain().any(|cause| {
        matches!(
            cause.downcast_ref(),
            Some(course2md::llm::RequestError::Transport(_))
        ) || cause.downcast_ref::<std::io::Error>().is_some()
    });
    let outcome = if login {
        TestOutcome::AuthenticationRefused
    } else {
        classify_status(http_status, false, transport)
    };
    CodexFailure {
        outcome,
        http_status,
    }
}

fn unknown_result_message() -> &'static str {
    course2md::tr!("未收到确定结果，测试可能已产生费用")
}

struct HttpRequest {
    endpoint: String,
    content_type: String,
    body: Vec<u8>,
    authorization: Option<Secret>,
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

enum TransportFailure {
    /// The request or response was not fully observed. No retry is performed.
    Unknown,
    ResponseTooLarge(u16),
}

trait Transport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportFailure>;
}

struct HttpTransport;

impl Transport for HttpTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportFailure> {
        let agent = crate::bounded_http::json_agent(
            Duration::from_secs(10),
            Duration::from_secs(30),
            Some(Duration::from_secs(15)),
            Duration::from_secs(45),
        );
        let call = agent
            .post(&request.endpoint)
            .set("Content-Type", &request.content_type);
        let call = crate::bounded_http::json_call(call, request.authorization.as_ref());
        // A nonempty POST body is non-retryable in ureq 2. Each test creates a fresh agent,
        // so a recycled connection cannot trigger a hidden resend either.
        let response = match call.send_bytes(&request.body) {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(_)) => return Err(TransportFailure::Unknown),
        };
        let status = response.status();
        let body =
            crate::bounded_http::read_bounded(response, MAX_RESPONSE).map_err(
                |error| match error {
                    crate::bounded_http::BoundedReadError::TooLarge => {
                        TransportFailure::ResponseTooLarge(status)
                    }
                    crate::bounded_http::BoundedReadError::Network => TransportFailure::Unknown,
                },
            )?;
        Ok(HttpResponse { status, body })
    }
}

fn run_test(
    config: &ServiceConfiguration,
    kind: TestKind,
    vault: &dyn CredentialVault,
    cancelled: &AtomicBool,
    transport: &dyn Transport,
) -> ServiceTestEvidence {
    let mut evidence = match new_evidence(config, kind, cancelled) {
        Ok(evidence) => evidence,
        Err(cancelled) => return cancelled,
    };
    if !kind.accepts(config.protocol) {
        evidence.message = course2md::tr!("所选服务类型不支持这项测试，尚未发送请求").into();
        return evidence;
    }
    if normalize_endpoint(&config.endpoint, config.protocol)
        .ok()
        .as_deref()
        != Some(config.endpoint.as_str())
        || config.model.trim().is_empty()
    {
        evidence.message = course2md::tr!("请先填写有效的服务地址和模型 ID，尚未发送请求").into();
        return evidence;
    }
    let authorization = if config.authentication == Authentication::ApiKey {
        let Some(reference) = &config.credential else {
            evidence.message = course2md::tr!("此认证方式需要 API Key，尚未发送请求").into();
            return evidence;
        };
        match vault.resolve(reference) {
            Ok(secret) => Some(secret),
            Err(error) => {
                evidence.message = course2md::tr!("{}；尚未发送请求", error);
                return evidence;
            }
        }
    } else {
        None
    };
    let (content_type, body) = request_body(config, kind);
    let request = HttpRequest {
        endpoint: config.endpoint.clone(),
        content_type,
        body,
        authorization,
    };
    if cancelled.load(Ordering::Acquire) {
        evidence.message = cancelled_message().into();
        return evidence;
    }
    let response = match transport.send(&request) {
        Ok(response) => response,
        Err(TransportFailure::Unknown) => {
            evidence.outcome = TestOutcome::OutcomeUnknown;
            evidence.message = unknown_result_message().into();
            return evidence;
        }
        Err(TransportFailure::ResponseTooLarge(status)) => {
            evidence.outcome = TestOutcome::ContractMismatch;
            evidence.http_status = Some(status);
            evidence.message = course2md::tr!("服务返回的内容异常，请核对服务地址").into();
            evidence.details.push(course2md::tr!(
                "HTTP {}；响应超过 128 KiB，未读取其余内容",
                status
            ));
            return evidence;
        }
    };
    evidence.http_status = Some(response.status);
    evidence.details.push(format!("HTTP {}", response.status));
    let json = serde_json::from_slice::<Value>(&response.body).ok();
    if response.status == 401 || response.status == 403 {
        evidence.outcome = TestOutcome::AuthenticationRefused;
        evidence.message = course2md::tr!("服务拒绝凭据，请检查 API Key 或此模型的使用权限").into();
        return evidence;
    }
    if !(200..300).contains(&response.status)
        || json
            .as_ref()
            .is_some_and(|value| value.get("error").is_some())
    {
        let code = json
            .as_ref()
            .and_then(|value| {
                value
                    .pointer("/error/code")
                    .or_else(|| value.pointer("/error/type"))
            })
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let model_error = [
            "model_not_found",
            "invalid_model",
            "model_not_available",
            "unsupported_model",
            "model_not_supported",
        ]
        .contains(&code.as_str());
        evidence.outcome = classify_status(Some(response.status), model_error, false);
        evidence.message = if response.status >= 500 {
            course2md::tr!("服务返回错误，未收到确定结果，测试可能已产生费用")
        } else if response.status == 429 {
            course2md::tr!("服务暂时无法接受测试请求，请查看服务的额度或速率限制")
        } else if model_error {
            course2md::tr!("服务不接受所选模型，请检查模型 ID 和使用权限")
        } else if (300..400).contains(&response.status) {
            course2md::tr!("服务地址已变更或不正确，请核对")
        } else {
            course2md::tr!("已收到回应，但服务未接受这项测试请求")
        }
        .into();
        return evidence;
    }
    let Some(json) = json else {
        evidence.outcome = TestOutcome::ContractMismatch;
        evidence.message = course2md::tr!("服务返回的内容格式不对，请确认服务类型和地址").into();
        return evidence;
    };
    let content = if config.protocol == ServiceProtocol::SpeechTranscriptions {
        json.get("text").and_then(Value::as_str)
    } else {
        json.pointer("/choices/0/message/content")
            .and_then(Value::as_str)
    };
    let Some(content) = content.filter(|content| !content.trim().is_empty()) else {
        evidence.outcome = TestOutcome::ContractMismatch;
        evidence.message = course2md::tr!("服务有回应但内容为空，请确认模型支持此用途").into();
        return evidence;
    };
    match verify_sample(kind, content) {
        Ok(()) => {
            evidence.outcome = TestOutcome::Passed;
            evidence.message = passed_message(kind);
        }
        Err((outcome, detail)) => {
            evidence.outcome = outcome;
            evidence.message = if evidence.outcome == TestOutcome::ContractMismatch {
                course2md::tr!("返回格式不符合要求").into()
            } else {
                course2md::tr!("样例未通过，请查看详情").into()
            };
            evidence.details.push(detail);
            let excerpt = if let Some(secret) = &request.authorization {
                content.replace(secret.expose(), course2md::tr!("[已隐藏凭据]"))
            } else {
                content.to_owned()
            };
            evidence.details.push(course2md::tr!(
                "样例回应：{}",
                excerpt.chars().take(240).collect::<String>()
            ));
        }
    }
    evidence
}

fn request_body(config: &ServiceConfiguration, kind: TestKind) -> (String, Vec<u8>) {
    if config.protocol == ServiceProtocol::SpeechTranscriptions {
        let boundary = format!("course2md-test-{}", uuid::Uuid::new_v4());
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"course2md-speech-test.wav\"\r\nContent-Type: audio/wav\r\n\r\n", config.model).into_bytes();
        body.extend_from_slice(SPEECH_SAMPLE);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        return (format!("multipart/form-data; boundary={boundary}"), body);
    }
    let (system, user) = match kind {
        TestKind::Speech => (
            "Transcribe the audio verbatim in its original language. Return only the transcript.",
            json!([
                {"type":"text", "text":"Transcribe this short audio."},
                {"type":"input_audio", "input_audio":{"data":base64::engine::general_purpose::STANDARD.encode(SPEECH_SAMPLE), "format":"wav"}}
            ]),
        ),
        TestKind::Proofread => (
            "Correct grammar and punctuation while preserving meaning and language. Return only JSON: {\"segments\":[{\"id\":0,\"text\":\"corrected text\"}]}. Preserve each input ID exactly once.",
            json!(
                "[{\"id\":0,\"text\":\"The student have three book.\"},{\"id\":1,\"text\":\"Water freezes at zero degrees Celsius.\"}]"
            ),
        ),
        TestKind::Summary => (
            "Summarize the transcript in its original language. Return only JSON with tldr (string), key_points (array of strings), and outline (array of {t: numeric seconds, title: string, detail: string}). Preserve the concrete facts.",
            json!(
                "[0s] Plants use sunlight to make food through photosynthesis. [8s] The process uses water and carbon dioxide and releases oxygen."
            ),
        ),
        TestKind::Vision => (
            "Correct the transcript using the attached image. Preserve the input ID. Return only JSON: {\"segments\":[{\"id\":0,\"text\":\"corrected text\"}]}.",
            json!([
                {"type":"text", "text":"[{\"id\":0,\"text\":\"The card shown is red.\"}]"},
                {"type":"image_url", "image_url":{"url":format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(BLUE_CARD))}}
            ]),
        ),
    };
    let mut body = json!({
        "model":config.model, "temperature":0, "max_tokens":512,
        "messages":[{"role":"system","content":system},{"role":"user","content":user}]
    });
    if kind != TestKind::Speech {
        body["response_format"] = json!({"type":"json_object"});
    }
    (
        "application/json".into(),
        serde_json::to_vec(&body).expect("sample is serializable"),
    )
}

fn words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_word(text: &str, word: &str) -> bool {
    text.split_whitespace().any(|actual| actual == word)
}

fn filled(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

fn verify_sample(kind: TestKind, content: &str) -> Result<(), (TestOutcome, String)> {
    let mismatch = |message: &str| Err((TestOutcome::SampleMismatch, message.to_owned()));
    let contract = |message: &str| (TestOutcome::ContractMismatch, message.to_owned());
    match kind {
        TestKind::Speech => {
            let text = words(content).replace("7", "seven");
            if !["blue", "notebook", "seven", "pages"]
                .iter()
                .all(|word| has_word(&text, word))
            {
                return mismatch(course2md::tr!(
                    "内置音频：The blue notebook contains seven pages.；回应未保留其中的关键信息"
                ));
            }
        }
        TestKind::Proofread | TestKind::Vision => {
            let pairs = course2md::llm::parse_segments(content)
                .ok_or_else(|| contract(course2md::tr!("需要 segments 列表，每项有 id 与 text")))?;
            let mut ids = pairs.iter().map(|(id, _)| *id).collect::<Vec<_>>();
            ids.sort_unstable();
            let expected: &[usize] = if kind == TestKind::Proofread {
                &[0, 1]
            } else {
                &[0]
            };
            if ids != expected || pairs.iter().any(|(_, text)| text.trim().is_empty()) {
                return Err(contract(course2md::tr!(
                    "返回段落 ID 必须与内置输入一一对应，且每段包含有效文字"
                )));
            }
            let segment = |id: usize| {
                pairs
                    .iter()
                    .find(|(found, _)| *found == id)
                    .map(|(_, text)| words(text))
                    .unwrap_or_default()
            };
            let first = segment(0);
            if kind == TestKind::Vision {
                if !has_word(&first, "blue") || has_word(&first, "red") {
                    return mismatch(course2md::tr!(
                        "内置图片为蓝色卡片，校对后应将原文的 red 改为 blue"
                    ));
                }
            } else {
                let second = segment(1);
                if !(first.contains("has three books") || first.contains("has 3 books"))
                    || !second.contains("water")
                    || !second.contains("celsius")
                {
                    return mismatch(course2md::tr!(
                        "第一段应纠正 have / book 的语法；第二段应保留水在零摄氏度结冰的原意"
                    ));
                }
            }
        }
        TestKind::Summary => {
            // The same reading production gives a summary: code fences, surrounding
            // prose and trailing commas do not make a working service fail its test.
            let value = course2md::llm::extract_json_object(content).ok_or_else(|| {
                contract(if content.contains('{') {
                    course2md::tr!("摘要 JSON 无法解析")
                } else {
                    course2md::tr!("需要摘要 JSON 对象")
                })
            })?;
            let list = |name| {
                value
                    .get(name)
                    .and_then(Value::as_array)
                    .filter(|items| !items.is_empty())
            };
            let structured = filled(value.get("tldr"))
                && list("key_points")
                    .is_some_and(|points| points.iter().all(|point| filled(Some(point))))
                && list("outline").is_some_and(|items| {
                    items.iter().all(|item| {
                        item.get("t")
                            .is_some_and(|time| time.is_number() || time.is_string())
                            && filled(item.get("title"))
                            && filled(item.get("detail"))
                    })
                });
            if !structured {
                return Err(contract(course2md::tr!(
                    "需要有效的 tldr、key_points 和带时间的 outline"
                )));
            }
            let text = words(content);
            if !["photosynthesis", "sunlight", "oxygen"]
                .iter()
                .all(|word| text.contains(word))
            {
                return mismatch(course2md::tr!(
                    "短文讲述光合作用使用阳光并释放氧气，摘要未保留这些关键信息"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemoryCredentialVault;
    use std::sync::atomic::AtomicUsize;

    struct FakeTransport {
        status: u16,
        body: Vec<u8>,
        fail: bool,
        calls: AtomicUsize,
    }
    impl Transport for FakeTransport {
        fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, TransportFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(TransportFailure::Unknown)
            } else {
                Ok(HttpResponse {
                    status: self.status,
                    body: self.body.clone(),
                })
            }
        }
    }

    fn config() -> ServiceConfiguration {
        ServiceConfiguration {
            name: "Test fixture".into(),
            protocol: ServiceProtocol::AiChat,
            endpoint: "https://example.test/v1/chat/completions".into(),
            model: "fixture-model".into(),
            authentication: Authentication::None,
            credential: None,
            credential_source: None,
        }
    }

    fn transport(status: u16, body: Value) -> FakeTransport {
        FakeTransport {
            status,
            body: serde_json::to_vec(&body).unwrap(),
            fail: false,
            calls: AtomicUsize::new(0),
        }
    }

    fn chat(content: &str) -> Value {
        json!({"choices":[{"message":{"content":content}}]})
    }

    fn run(fake: &FakeTransport) -> ServiceTestEvidence {
        run_test(
            &config(),
            TestKind::Proofread,
            &MemoryCredentialVault::new(),
            &AtomicBool::new(false),
            fake,
        )
    }

    #[test]
    fn a_summary_passes_with_whatever_production_tolerates() {
        let body = r#"{"tldr":"Photosynthesis uses sunlight and releases oxygen.",
            "key_points":["Plants use sunlight","They release oxygen"],
            "outline":[{"t":0,"title":"Photosynthesis","detail":"Sunlight in, oxygen out",},],}"#;
        for content in [
            body.to_owned(),
            format!("Here it is:\n```json\n{body}\n```\nDone."),
        ] {
            assert!(course2md::llm::extract_json_object(&content).is_some());
            assert_eq!(verify_sample(TestKind::Summary, &content), Ok(()));
        }
        assert!(verify_sample(TestKind::Summary, "no object here").is_err());
        assert!(verify_sample(TestKind::Summary, "{not json}").is_err());
    }

    #[test]
    fn http_200_html_error_json_and_missing_segment_ids_are_not_success() {
        let html = FakeTransport {
            status: 200,
            body: b"<html>login</html>".to_vec(),
            fail: false,
            calls: AtomicUsize::new(0),
        };
        assert_eq!(run(&html).outcome, TestOutcome::ContractMismatch);
        assert_ne!(
            run(&transport(
                200,
                json!({"error":{"message":"not available"}})
            ))
            .outcome,
            TestOutcome::Passed
        );
        assert_eq!(
            run(&transport(
                200,
                chat("{\"segments\":[{\"id\":9,\"text\":\"Something\"}]}")
            ))
            .outcome,
            TestOutcome::ContractMismatch
        );
    }

    #[test]
    fn structurally_valid_but_uncorrected_sample_is_distinct_from_bad_contract() {
        let evidence = run(&transport(
            200,
            chat(
                "{\"segments\":[{\"id\":0,\"text\":\"The student have three book.\"},{\"id\":1,\"text\":\"Water freezes at zero degrees Celsius.\"}]}",
            ),
        ));
        assert_eq!(evidence.outcome, TestOutcome::SampleMismatch);
        let evidence = run(&transport(
            200,
            chat(
                "{\"segments\":[{\"id\":0,\"text\":\"The student has three books.\"},{\"id\":1,\"text\":\"Water freezes at zero degrees Celsius.\"}]}",
            ),
        ));
        assert_eq!(evidence.outcome, TestOutcome::Passed);
    }

    #[test]
    fn unknown_response_is_never_automatically_resent() {
        let fake = FakeTransport {
            status: 0,
            body: vec![],
            fail: true,
            calls: AtomicUsize::new(0),
        };
        assert_eq!(run(&fake).outcome, TestOutcome::OutcomeUnknown);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancel_before_dispatch_and_missing_credentials_send_nothing() {
        let fake = transport(200, chat("ignored"));
        let vault = MemoryCredentialVault::new();
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            run_test(&config(), TestKind::Proofread, &vault, &cancelled, &fake).outcome,
            TestOutcome::NotSent
        );
        let mut config = config();
        config.authentication = Authentication::ApiKey;
        assert_eq!(
            run_test(
                &config,
                TestKind::Proofread,
                &vault,
                &AtomicBool::new(false),
                &fake
            )
            .outcome,
            TestOutcome::NotSent
        );
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn auth_and_model_refusal_are_specific_and_do_not_expose_raw_error() {
        let evidence = run(&transport(
            401,
            json!({"error":{"message":"secret-bearing backend error"}}),
        ));
        assert_eq!(evidence.outcome, TestOutcome::AuthenticationRefused);
        assert!(!format!("{evidence:?}").contains("secret-bearing"));
        assert_eq!(
            run(&transport(404, json!({"error":{"code":"model_not_found"}}))).outcome,
            TestOutcome::ModelRefused
        );
    }

    #[test]
    fn repair_blocks_only_matching_explicit_configuration_refusals() {
        let original = config();
        let required = [TestKind::Proofread.contract()];
        let refusal = run(&transport(
            401,
            json!({"error": {"code": "invalid_api_key"}}),
        ));
        assert!(blocks_reprocessing(&refusal, &original, &required));
        let mut renamed = original.clone();
        renamed.name = "Renamed service".into();
        assert!(blocks_reprocessing(&refusal, &renamed, &required));
        for changed in [
            ServiceConfiguration {
                endpoint: "https://other.test/v1/chat/completions".into(),
                ..original.clone()
            },
            ServiceConfiguration {
                model: "another-model".into(),
                ..original.clone()
            },
            ServiceConfiguration {
                authentication: Authentication::ApiKey,
                credential: Some("credential-new".into()),
                ..original.clone()
            },
        ] {
            assert!(!blocks_reprocessing(&refusal, &changed, &required));
        }
        let model_refusal = run(&transport(
            404,
            json!({"error": {"code": "model_not_found"}}),
        ));
        assert!(blocks_reprocessing(&model_refusal, &original, &required));
        assert!(!blocks_reprocessing(
            &model_refusal,
            &original,
            &[TestKind::Summary.contract()]
        ));
        for outcome in [
            TestOutcome::Passed,
            TestOutcome::NotSent,
            TestOutcome::OutcomeUnknown,
            TestOutcome::ContractMismatch,
            TestOutcome::SampleMismatch,
        ] {
            let mut evidence = refusal.clone();
            evidence.outcome = outcome;
            assert!(!blocks_reprocessing(&evidence, &original, &required));
        }
    }

    #[test]
    fn retryable_http_status_is_not_a_configuration_refusal() {
        for status in [429, 500, 503] {
            let evidence = run(&transport(
                status,
                json!({"error": {"code": "model_not_found"}}),
            ));
            assert!(!matches!(
                evidence.outcome,
                TestOutcome::AuthenticationRefused | TestOutcome::ModelRefused
            ));
            assert!(!blocks_reprocessing(
                &evidence,
                &config(),
                &[TestKind::Proofread.contract()]
            ));
            let mut older = evidence;
            older.outcome = TestOutcome::ModelRefused;
            assert!(!blocks_reprocessing(
                &older,
                &config(),
                &[TestKind::Proofread.contract()]
            ));
        }
        let forbidden = run(&transport(403, json!({"error": {"code": "forbidden"}})));
        assert!(blocks_reprocessing(
            &forbidden,
            &config(),
            &[TestKind::Proofread.contract()]
        ));
    }

    #[test]
    fn codex_failures_separate_login_refusal_from_unknown_and_mismatched_results() {
        let outcome = |error: anyhow::Error| codex_failure(&error).outcome;
        let expired = || anyhow::Error::new(course2md::login::codex::CodexLoginRequired);
        assert_eq!(outcome(expired()), TestOutcome::AuthenticationRefused);
        assert_eq!(
            outcome(expired().context("连接失败。请检查服务地址、密钥和模型。")),
            TestOutcome::AuthenticationRefused
        );
        let interrupted = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        assert_eq!(
            outcome(anyhow::Error::new(interrupted).context("服务响应接收中断")),
            TestOutcome::OutcomeUnknown
        );
        assert_eq!(
            outcome(anyhow::anyhow!("服务返回空响应")),
            TestOutcome::ContractMismatch
        );
    }

    #[test]
    fn speech_and_vision_verify_sample_content_not_just_nonempty_text() {
        assert!(verify_sample(TestKind::Speech, "The blue notebook contains 7 pages.").is_ok());
        assert!(verify_sample(TestKind::Speech, "error unavailable").is_err());
        assert!(
            verify_sample(
                TestKind::Vision,
                "{\"segments\":[{\"id\":0,\"text\":\"The card shown is blue.\"}]}"
            )
            .is_ok()
        );
        assert!(
            verify_sample(
                TestKind::Vision,
                "{\"segments\":[{\"id\":0,\"text\":\"The card shown is red.\"}]}"
            )
            .is_err()
        );
    }
}
