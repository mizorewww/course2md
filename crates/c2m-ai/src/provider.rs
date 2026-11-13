//! LLM 服务方言：每个 provider 的 endpoint、认证头、请求体与响应提取集中于此
//!（单一事实源）。登录方法见 crate::login。
//!
//! OpenAiCompatible 与 Ollama 共用 chat/completions 方言；Codex 走 ChatGPT 后端
//! Responses API（SSE 流），SSE 聚合为 chat/completions 形状的 JSON 后，
//! 下游校验与解析逻辑保持不变。

use anyhow::{Result, bail, ensure};
use serde_json::Value;

use crate::llm::{LlmProvider, LlmSettings};

/// Codex 登录态的 ChatGPT 后端（OAuth token 仅对此端点有效，非 api.openai.com）。
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// 该后端对 text.format=json_object 有前置检查：input 消息须含小写 "json"
///（大小写敏感；instructions 中出现不算数）。校对的首个文本块是纯 segments
/// JSON，不能污染，因此在末尾追加独立的约束块。
fn ensure_json_marker(content: &mut Value) {
    let Some(parts) = content.as_array_mut() else {
        return;
    };
    let mentioned = parts
        .iter()
        .any(|p| p["text"].as_str().is_some_and(|t| t.contains("json")));
    if !mentioned {
        parts.push(serde_json::json!({"type": "input_text", "text": "（输出 json）"}));
    }
}

/// 完整请求 URL。
pub(crate) fn endpoint(s: &LlmSettings) -> String {
    match s.provider {
        LlmProvider::Codex => CODEX_RESPONSES_URL.into(),
        _ => crate::llm::endpoint(&s.base_url),
    }
}

/// 认证头；空 vec = 不认证（本地服务）。Codex 在此解析并必要时刷新 access token。
pub(crate) fn auth_headers(s: &LlmSettings) -> Result<Vec<(String, String)>> {
    match s.provider {
        LlmProvider::Codex => crate::login::codex::auth_headers(),
        _ if s.api_key.is_empty() => Ok(Vec::new()),
        _ => Ok(vec![(
            "Authorization".into(),
            format!("Bearer {}", s.api_key),
        )]),
    }
}

/// Whether sending a request again costs no per-request money: a ChatGPT
/// plan (Codex) or a local Ollama server. An unconfirmed result from such a
/// service is resent automatically instead of waiting for the user's consent.
pub(crate) fn free_to_resend(s: &LlmSettings) -> bool {
    matches!(s.provider, LlmProvider::Codex | LlmProvider::Ollama)
}

/// 响应是否为 SSE 流（Codex Responses API 只提供流式）。
pub(crate) fn is_sse(s: &LlmSettings) -> bool {
    s.provider == LlmProvider::Codex
}

/// How a request goes out, beside what it asks ([`chat_body`], the ledger identity).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Delivery {
    /// The output budget for this request; None keeps the body's own.
    pub max_tokens: Option<u32>,
    /// Ask the model not to think first, where the service has a way to say so.
    pub no_reasoning: bool,
    /// How the answer is asked to arrive.
    pub stream: Stream,
}

/// How an answer is asked to arrive. Each step down is for a service that refused
/// the one above it by name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Stream {
    /// Streamed, with token usage at the end.
    #[default]
    WithUsage,
    /// Streamed, without `stream_options`.
    Bare,
    /// One answer at the end.
    Off,
}

impl Stream {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WithUsage => "usage",
            Self::Bare => "bare",
            Self::Off => "off",
        }
    }
}

/// The body as sent: streamed, so a long answer arrives as it is written and only
/// a silence can time it out, with `delivery` applied. None of this changes what
/// is asked, so the ledger identifies the request by `body` (and by what a
/// service made it change, see `Chat::send`).
pub(crate) fn sent_body(s: &LlmSettings, body: &Value, delivery: Delivery) -> Value {
    let mut sent = body.clone();
    // Codex is already a stream, and the ChatGPT backend takes no output limit here.
    if s.provider == LlmProvider::Codex {
        return sent;
    }
    let Some(object) = sent.as_object_mut() else {
        return sent;
    };
    if delivery.stream != Stream::Off {
        object.insert("stream".into(), Value::Bool(true));
    }
    if delivery.stream == Stream::WithUsage {
        object.insert(
            "stream_options".into(),
            serde_json::json!({"include_usage": true}),
        );
    }
    if let Some(max_tokens) = delivery.max_tokens {
        object.insert("max_tokens".into(), max_tokens.into());
    }
    if controls_reasoning(s, delivery) {
        object.insert("reasoning_effort".into(), "none".into());
    }
    sent
}

/// Whether the sent body turns thinking off. Ollama, local or Ollama Cloud
/// (`ollama.com`, often set up as an OpenAI-compatible service), reads
/// `reasoning_effort: "none"` as think=false. Other services differ in what they
/// accept, so they keep their default.
pub(crate) fn controls_reasoning(s: &LlmSettings, delivery: Delivery) -> bool {
    delivery.no_reasoning
        && match s.provider {
            LlmProvider::Ollama => true,
            LlmProvider::OpenAiCompatible => is_ollama_cloud(&s.base_url),
            LlmProvider::Codex => false,
        }
}

fn is_ollama_cloud(base_url: &str) -> bool {
    url::Url::parse(base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "ollama.com" || host.ends_with(".ollama.com"))
}

/// A streamed answer, read to its end, as the chat/completions JSON the rest reads.
/// A service that answered with one JSON body despite `stream` is read as it is.
pub(crate) fn stream_to_chat_json(s: &LlmSettings, bytes: &[u8]) -> Result<Value> {
    if s.provider == LlmProvider::Codex {
        return sse_to_chat_json(bytes);
    }
    let text = String::from_utf8_lossy(bytes);
    if text.trim_start().starts_with('{') {
        return Ok(serde_json::from_str(text.trim())?);
    }
    chat_sse_to_chat_json(&text)
}

/// Whether the answer stopped at its output limit instead of finishing.
pub(crate) fn truncated(value: &Value) -> bool {
    matches!(
        value["choices"][0]["finish_reason"].as_str(),
        Some("length" | "max_tokens" | "max_output_tokens")
    )
}

/// chat/completions SSE (`data: {"choices":[{"delta":{"content":…}}]}` … `data: [DONE]`)
/// aggregated into one response. A stream must end with a finish reason or `[DONE]`;
/// one that stops before is a lost answer, not a short one. Thinking deltas
/// (`reasoning_content`, `reasoning`) are not part of the answer.
fn chat_sse_to_chat_json(text: &str) -> Result<Value> {
    let mut out = String::new();
    let mut finish: Option<String> = None;
    let mut done = false;
    let mut usage = Value::Null;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            done = true;
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(error) = event.get("error").filter(|e| !e.is_null()) {
            let message = error["message"]
                .as_str()
                .or_else(|| error.as_str())
                .unwrap_or("unknown error");
            bail!("AI 服务返回错误内容 / AI service returned an error: {message}");
        }
        let choice = &event["choices"][0];
        if let Some(delta) = choice["delta"]["content"].as_str() {
            out.push_str(delta);
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            finish = Some(reason.to_owned());
        }
        if event["usage"].is_object() {
            usage = event["usage"].clone();
        }
    }
    ensure!(
        done || finish.is_some(),
        crate::tr!("AI 服务响应中断（流未完成）/ AI response stream ended before completion")
    );
    let finish = finish.unwrap_or_else(|| "stop".into());
    Ok(serde_json::json!({
        "choices": [{"message": {"content": out}, "finish_reason": finish}],
        "usage": usage,
    }))
}

/// 构造请求体（结构化输出契约在两种方言中各自表达）。
pub(crate) fn chat_body(
    s: &LlmSettings,
    system: &str,
    user: impl Into<Value>,
    max_tokens: u32,
) -> Value {
    match s.provider {
        LlmProvider::Codex => {
            let mut content = codex_content(user.into());
            ensure_json_marker(&mut content);
            serde_json::json!({
                "model": s.model,
                "instructions": system,
                "input": [{"type": "message", "role": "user", "content": content}],
                "store": false,
                "stream": true,
                "text": {"format": {"type": "json_object"}},
            })
        }
        // Temperature is fixed at 0: proofreading and summaries require deterministic output.
        _ => serde_json::json!({
            "model": s.model,
            "temperature": 0.0,
            "max_tokens": max_tokens,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user.into()},
            ]
        }),
    }
}

/// test_connection 的最小请求体。
pub(crate) fn test_body(s: &LlmSettings, user: Value) -> Value {
    match s.provider {
        LlmProvider::Codex => serde_json::json!({
            "model": s.model,
            "input": [{"type": "message", "role": "user", "content": codex_content(user)}],
            "store": false,
            "stream": true,
        }),
        _ => serde_json::json!({
            "model": s.model,
            // 推理模型的 reasoning 会先吃掉 token 预算：8 个 token 会导致 content
            // 为空（finish_reason=length）的假阴性；512 够推理余量也不贵
            "max_tokens": 512,
            "messages": [{"role": "user", "content": user}],
        }),
    }
}

/// OpenAI 多模态内容块 → Responses API 输入块。
fn codex_content(user: Value) -> Value {
    let parts = match user {
        Value::String(text) => {
            vec![serde_json::json!({"type": "input_text", "text": text})]
        }
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part["type"].as_str() {
                Some("image_url") => serde_json::json!({
                    "type": "input_image",
                    "image_url": part["image_url"]["url"].as_str().unwrap_or_default(),
                }),
                _ => serde_json::json!({
                    "type": "input_text",
                    "text": part["text"].as_str().unwrap_or_default(),
                }),
            })
            .collect(),
        other => vec![serde_json::json!({"type": "input_text", "text": other.to_string()})],
    };
    Value::Array(parts)
}

fn structured_format_key(s: &LlmSettings) -> &'static str {
    match s.provider {
        LlmProvider::Codex => "text",
        _ => "response_format",
    }
}

/// 端点是否声明了结构化输出约束（400/422 降级重试的前提）。
pub(crate) fn has_structured_format(s: &LlmSettings, body: &Value) -> bool {
    body.get(structured_format_key(s)).is_some()
}

/// 去掉结构化输出约束（端点明确不支持时降级重试一次）。
pub(crate) fn strip_structured_format(s: &LlmSettings, body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove(structured_format_key(s));
    }
}

/// Codex SSE 流聚合为 chat/completions 形状的 JSON，下游校验/提取逻辑不变。
/// 流必须以 response.completed 收尾，否则视为中断（对照 codex-api/src/sse/responses.rs）。
/// response.usage（input/output_tokens）归一为 chat 方言的 prompt/completion_tokens，
/// 供 token 实时反馈统一提取。
pub(crate) fn sse_to_chat_json(bytes: &[u8]) -> Result<Value> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut completed = false;
    let mut usage = serde_json::Value::Null;
    let mut failed: Option<String> = None;
    let mut cut_short = false;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match event["type"].as_str() {
            Some("response.output_text.delta") => {
                out.push_str(event["delta"].as_str().unwrap_or(""));
            }
            Some("response.completed") => {
                completed = true;
                let u = &event["response"]["usage"];
                if let (Some(input), Some(output)) =
                    (u["input_tokens"].as_u64(), u["output_tokens"].as_u64())
                {
                    usage = serde_json::json!({
                        "prompt_tokens": input,
                        "completion_tokens": output,
                    });
                }
            }
            // Stopped at the output limit: a short answer, not a failed one; the
            // caller asks again for what it lacks.
            Some("response.incomplete")
                if event["response"]["incomplete_details"]["reason"].as_str()
                    == Some("max_output_tokens") =>
            {
                completed = true;
                cut_short = true;
            }
            Some("response.failed") | Some("response.incomplete") | Some("error") => {
                failed = Some(
                    event["response"]["error"]["message"]
                        .as_str()
                        .or_else(|| event["response"]["incomplete_details"]["reason"].as_str())
                        .or_else(|| event["message"].as_str())
                        .unwrap_or("unknown error")
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    if let Some(message) = failed {
        bail!("AI 服务返回错误内容 / AI service returned an error: {message}");
    }
    ensure!(
        completed,
        crate::tr!("AI 服务响应中断（流未完成）/ AI response stream ended before completion")
    );
    ensure!(
        cut_short || !out.trim().is_empty(),
        crate::tr!("AI 服务响应缺少正文 / AI response is missing text")
    );
    let finish = if cut_short { "length" } else { "stop" };
    Ok(serde_json::json!({
        "choices": [{"message": {"content": out}, "finish_reason": finish}],
        "usage": usage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(provider: LlmProvider) -> LlmSettings {
        LlmSettings {
            provider,
            base_url: "https://api.example.com/v1".into(),
            model: "test-model".into(),
            ..Default::default()
        }
    }

    #[test]
    fn codex_body_uses_responses_dialect() {
        let s = settings(LlmProvider::Codex);
        let body = chat_body(
            &s,
            "系统指令",
            Value::Array(vec![
                serde_json::json!({"type": "text", "text": "输入"}),
                serde_json::json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,x"}}),
            ]),
            100,
        );
        assert_eq!(body["instructions"], "系统指令");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(body["text"]["format"]["type"], "json_object");
        // 首个文本块保持纯输入；json 约束块追加在末尾
        assert_eq!(body["input"][0]["content"][0]["text"], "输入");
        let last = body["input"][0]["content"].as_array().unwrap().len() - 1;
        assert!(
            body["input"][0]["content"][last]["text"]
                .as_str()
                .unwrap()
                .contains("json")
        );
        assert!(has_structured_format(&s, &body));
        let mut relaxed = body.clone();
        strip_structured_format(&s, &mut relaxed);
        assert!(!has_structured_format(&s, &relaxed));
    }

    #[test]
    fn openai_body_requests_structured_output() {
        let s = settings(LlmProvider::OpenAiCompatible);
        let body = chat_body(
            &s,
            "系统指令",
            Value::Array(vec![serde_json::json!({"type": "text", "text": "输入"})]),
            100,
        );
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["messages"][1]["content"][0]["text"], "输入");
        assert!(has_structured_format(&s, &body));
    }

    #[test]
    fn sse_aggregation_collects_deltas_and_surfaces_failures() {
        let ok = br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"{\"segments\":"}

data: {"type":"response.output_text.delta","delta":"[]}"}

data: {"type":"response.completed","response":{"usage":{"input_tokens":120,"output_tokens":36}}}

data: [DONE]

"#;
        let value = sse_to_chat_json(ok).unwrap();
        assert_eq!(
            value["choices"][0]["message"]["content"].as_str().unwrap(),
            "{\"segments\":[]}"
        );
        // usage 归一为 chat/completions 方言，供 token 实时反馈统一提取
        assert_eq!(value["usage"]["prompt_tokens"].as_u64(), Some(120));
        assert_eq!(value["usage"]["completion_tokens"].as_u64(), Some(36));

        let failed =
            br#"data: {"type":"response.failed","response":{"error":{"message":"rate limited"}}}
"#;
        assert!(sse_to_chat_json(failed).is_err());
        assert!(sse_to_chat_json(b"data: {\"type\":\"response.created\"}\n").is_err());

        let cut = br#"data: {"type":"response.output_text.delta","delta":"{\"segments\":[{\"id\":0"}
data: {"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}
"#;
        assert!(truncated(&sse_to_chat_json(cut).unwrap()));
    }

    /// One `data:` line per event, from JSON values.
    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect()
    }

    fn delta(content: &str) -> Value {
        serde_json::json!({"choices": [{"delta": {"content": content}}]})
    }

    #[test]
    fn chat_streams_are_assembled_and_their_ends_checked() {
        let s = settings(LlmProvider::OpenAiCompatible);
        let thinking = serde_json::json!({"choices": [{"delta": {"reasoning_content": "hmm"}}]});
        let finished = serde_json::json!({"choices": [{"delta": {"content": "[]}"}, "finish_reason": "stop"}]});
        let usage = serde_json::json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 4}});
        let ok = format!(
            "{}: keep-alive\n\n{}data: [DONE]\n\n",
            sse(&[thinking, delta("{\"segments\":")]),
            sse(&[finished, usage])
        );
        let value = stream_to_chat_json(&s, ok.as_bytes()).unwrap();
        assert_eq!(
            value["choices"][0]["message"]["content"], "{\"segments\":[]}",
            "thinking is not part of the answer"
        );
        assert_eq!(value["usage"]["completion_tokens"], 4);
        assert!(!truncated(&value));

        // Stopped at the output limit: kept, and marked.
        let limit = serde_json::json!({"choices": [{"delta": {}, "finish_reason": "length"}]});
        let cut = sse(&[delta("{\"segm"), limit]);
        let value = stream_to_chat_json(&s, cut.as_bytes()).unwrap();
        assert!(truncated(&value));
        assert_eq!(value["choices"][0]["message"]["content"], "{\"segm");

        // A stream that stops without an end is lost, not short.
        let lost = sse(&[delta("{")]);
        assert!(stream_to_chat_json(&s, lost.as_bytes()).is_err());
        // An error in the stream is the service's answer.
        let error = sse(&[serde_json::json!({"error": {"message": "overloaded"}})]);
        assert!(stream_to_chat_json(&s, error.as_bytes()).is_err());
        // A service that ignored `stream` is read as one body.
        let plain = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"length"}]}"#;
        assert!(truncated(
            &stream_to_chat_json(&s, plain.as_bytes()).unwrap()
        ));
    }

    #[test]
    fn sent_bodies_stream_and_keep_what_is_asked() {
        let body = |s: &LlmSettings| chat_body(s, "系统", Value::String("输入".into()), 16384);
        let delivery = Delivery {
            max_tokens: Some(4096),
            no_reasoning: true,
            ..Default::default()
        };
        let s = settings(LlmProvider::OpenAiCompatible);
        let sent = sent_body(&s, &body(&s), delivery);
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["stream_options"]["include_usage"], true);
        assert_eq!(sent["max_tokens"], 4096);
        assert!(sent.get("reasoning_effort").is_none());
        assert_eq!(sent["messages"], body(&s)["messages"]);

        let s = settings(LlmProvider::Ollama);
        let sent = sent_body(&s, &body(&s), delivery);
        assert_eq!(sent["reasoning_effort"], "none");
        let sent = sent_body(&s, &body(&s), Delivery::default());
        assert!(sent.get("reasoning_effort").is_none());
        assert_eq!(sent["max_tokens"], 16384);

        let s = settings(LlmProvider::Codex);
        assert_eq!(sent_body(&s, &body(&s), delivery), body(&s));

        // Ollama Cloud set up as an OpenAI-compatible service is Ollama too.
        let mut s = settings(LlmProvider::OpenAiCompatible);
        s.base_url = "https://ollama.com/v1".into();
        assert_eq!(
            sent_body(&s, &body(&s), delivery)["reasoning_effort"],
            "none"
        );
        s.base_url = "https://notollama.com/v1".into();
        assert!(!controls_reasoning(&s, delivery));

        // A service that refused streaming options, then streaming.
        let s = settings(LlmProvider::OpenAiCompatible);
        let bare = Delivery {
            stream: Stream::Bare,
            ..delivery
        };
        let sent = sent_body(&s, &body(&s), bare);
        assert_eq!(sent["stream"], true);
        assert!(sent.get("stream_options").is_none());
        let off = Delivery {
            stream: Stream::Off,
            ..delivery
        };
        let sent = sent_body(&s, &body(&s), off);
        assert!(sent.get("stream").is_none() && sent.get("stream_options").is_none());
    }
}
