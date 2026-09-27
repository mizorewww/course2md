//! LLM 服务方言：每个 provider 的 endpoint、认证头、请求体与响应提取集中于此
//!（单一事实源，对照 docs/ENGINEERING-AUDIT.md H6）。登录方法见 crate::login。
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
///（大小写敏感，实测 2026-09；instructions 中出现不算数）。润色的首个文本块
/// 是纯 segments JSON（校验逻辑要按 JSON 解析复核段落数），不能污染；
/// 因此在末尾追加独立的约束块。
fn ensure_json_marker(content: &mut Value) {
    let Some(parts) = content.as_array_mut() else { return };
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
        _ => Ok(vec![("Authorization".into(), format!("Bearer {}", s.api_key))]),
    }
}

/// 响应是否为 SSE 流（Codex Responses API 只提供流式）。
pub(crate) fn is_sse(s: &LlmSettings) -> bool {
    s.provider == LlmProvider::Codex
}

/// 构造请求体（结构化输出契约在两种方言中各自表达）。
pub(crate) fn chat_body(s: &LlmSettings, system: &str, user: impl Into<Value>, max_tokens: u32) -> Value {
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
        _ => crate::llm::chat_body(&s.model, system, user.into(), max_tokens),
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

/// 请求体中的用户输入文本（校对校验要复核段落数；两种方言各取所需）。
pub(crate) fn user_input_text(body: &Value) -> Option<&str> {
    if let Some(text) = body["messages"][1]["content"][0]["text"].as_str() {
        return Some(text);
    }
    body["input"][0]["content"].as_array()?.iter().find_map(|part| {
        if part["type"] == "input_text" {
            part["text"].as_str()
        } else {
            None
        }
    })
}

/// 端点是否声明了结构化输出约束（400/422 降级重试的前提）。
pub(crate) fn has_structured_format(s: &LlmSettings, body: &Value) -> bool {
    match s.provider {
        LlmProvider::Codex => body.get("text").is_some(),
        _ => body.get("response_format").is_some(),
    }
}

/// 去掉结构化输出约束（端点明确不支持时降级重试一次）。
pub(crate) fn strip_structured_format(s: &LlmSettings, body: &mut Value) {
    let key = match s.provider {
        LlmProvider::Codex => "text",
        _ => "response_format",
    };
    if let Some(obj) = body.as_object_mut() {
        obj.remove(key);
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
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else { continue };
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
        "AI 服务响应中断（流未完成）/ AI response stream ended before completion"
    );
    ensure!(
        !out.trim().is_empty(),
        "AI 服务响应缺少正文 / AI response is missing text"
    );
    Ok(serde_json::json!({"choices": [{"message": {"content": out}}], "usage": usage}))
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
        // 首个文本块保持纯输入（校验按 JSON 解析）；json 约束块追加在末尾
        assert_eq!(user_input_text(&body), Some("输入"));
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
    fn openai_body_and_input_text_roundtrip() {
        let s = settings(LlmProvider::OpenAiCompatible);
        let body = chat_body(
            &s,
            "系统指令",
            Value::Array(vec![serde_json::json!({"type": "text", "text": "输入"})]),
            100,
        );
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(user_input_text(&body), Some("输入"));
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

        let failed = br#"data: {"type":"response.failed","response":{"error":{"message":"rate limited"}}}
"#;
        assert!(sse_to_chat_json(failed).is_err());
        assert!(sse_to_chat_json(b"data: {\"type\":\"response.created\"}\n").is_err());
    }
}
