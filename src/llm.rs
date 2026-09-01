//! LLM 字幕润色（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。

use crate::timeline::{Section, TranscriptEvent};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::time::Duration;

pub const DEFAULT_PROMPT: &str = "你是视频逐字稿校对器。输入的每一项是一段已按自然停顿组织的连续讲解。\
修正明显的语音识别错误（错别字、同音字、专有名词拼写），删除不影响原意的冗余口头填充，\
并修复不自然的断句和标点，使文字自然、书面化。不得概括、扩写、翻译、增删实质内容或改变原意；\
保持原语言。输出与输入逐条对应的 JSON 对象数组，每项形如 {\"id\":序号,\"text\":\"校对后的文本\"}，不要输出任何其他内容。";

/// 每次请求合并的语音段数。
const BATCH: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSettings {
    pub enabled: bool,
    /// OpenAI 兼容 base URL，如 https://api.deepseek.com/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 覆盖默认校对提示词
    pub prompt: Option<String>,
    /// 关闭「可开启 LLM」的结束提示
    pub disable_hint: bool,
}

/// base_url -> 完整 chat/completions URL。
pub fn endpoint(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else {
        format!("{b}/chat/completions")
    }
}

/// 校验配置可直接使用。
pub fn validate(s: &LlmSettings) -> Result<()> {
    if s.base_url.trim().is_empty() {
        bail!("llm.base_url 未配置，请运行 course2md llm setup");
    }
    if s.model.trim().is_empty() {
        bail!("llm.model 未配置，请运行 course2md llm setup");
    }
    Ok(())
}

/// 用 LLM 批量润色字幕；失败批次保留原文（润色失败不阻断转换）。
pub fn polish(mut events: Vec<TranscriptEvent>, s: &LlmSettings) -> Vec<TranscriptEvent> {
    let batches = events.chunks_mut(BATCH).len();
    let pb = indicatif::ProgressBar::new(batches as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    let mut warned = false;
    for chunk in events.chunks_mut(BATCH) {
        pb.inc(1);
        // 带分段下标请求/校验：防止模型重排或漏项导致的静默错位
        let items: Vec<(usize, &str)> = chunk
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.text.as_str()))
            .collect();
        match chat(s, &items) {
            Ok(polished) => {
                let mut by_id: Vec<Option<String>> = vec![None; chunk.len()];
                let mut bad = false;
                for (id, text) in polished {
                    if id >= chunk.len() || by_id[id].is_some() {
                        bad = true;
                        break;
                    }
                    by_id[id] = Some(text);
                }
                if bad || by_id.iter().any(|v| v.is_none()) {
                    warn_once(&mut warned, "LLM 返回 id 集与输入不符，该批保留原文");
                    continue;
                }
                for (ev, new_text) in chunk.iter_mut().zip(by_id.into_iter().map(|v| v.unwrap())) {
                    if !new_text.is_empty() && new_text != ev.text {
                        ev.raw.get_or_insert_with(|| ev.text.clone());
                        ev.text = new_text;
                    }
                }
            }
            Err(e) => warn_once(&mut warned, &format!("LLM 润色失败（{e:#}），保留原文")),
        }
    }
    pb.finish_and_clear();
    events
}

/// 对已经按截图和自然停顿组织好的段落逐段校对。
///
/// Section 内的顺序是唯一的回填依据；原始细粒度 ASR 事件由调用方另存到
/// timeline.jsonl，避免段落校对破坏可追溯的时间线。
pub fn polish_sections(sections: &mut [Section], s: &LlmSettings) {
    let (lengths, events) = flatten_sections(sections);
    restore_sections(sections, &lengths, polish(events, s));
}

fn flatten_sections(sections: &[Section]) -> (Vec<usize>, Vec<TranscriptEvent>) {
    let lengths = sections.iter().map(|section| section.speech.len()).collect();
    let events = sections
        .iter()
        .flat_map(|section| section.speech.iter().cloned())
        .collect();
    (lengths, events)
}

fn restore_sections(sections: &mut [Section], lengths: &[usize], events: Vec<TranscriptEvent>) {
    debug_assert_eq!(sections.len(), lengths.len());
    let mut events = events.into_iter();
    for (section, &len) in sections.iter_mut().zip(lengths) {
        section.speech = events.by_ref().take(len).collect();
    }
    debug_assert!(events.next().is_none());
}

fn warn_once(warned: &mut bool, msg: &str) {
    if !*warned {
        tracing::warn!("{msg}（后续同类问题不再重复提示）");
        *warned = true;
    } else {
        tracing::debug!("{msg}");
    }
}

/// 发一批（id, 文本）给 LLM，返回润色后的 (id, 文本) 列表。
fn chat(s: &LlmSettings, items: &[(usize, &str)]) -> Result<Vec<(usize, String)>> {
    validate(s)?;
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    let body = serde_json::json!({
        "model": s.model,
        "temperature": 0.0,
        "max_tokens": 4096,
        "messages": [
            {"role": "system", "content": format!("{} 输出为 JSON 数组，每项形如 {{\"id\":序号,\"text\":润色后的文本}}，id 必须与输入一一对应。", effective_prompt(s))},
            {"role": "user", "content": serde_json::to_string(&payload)?},
        ],
    });
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(180))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body)
        .context("LLM 请求失败")?;
    let v: serde_json::Value = resp.into_json().context("LLM 响应解析失败")?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    parse_id_text_pairs(&content)
        .with_context(|| format!("LLM 响应不是 id/text JSON 数组: {:.200}", content))
}

/// 从模型输出提取 [{"id":n,"text":"..."}]（容忍代码围栏与前后杂文）。
pub fn parse_id_text_pairs(content: &str) -> Option<Vec<(usize, String)>> {
    let start = content.find('[')?;
    let end = content.rfind(']')?;
    if end <= start {
        return None;
    }
    let v: Vec<serde_json::Value> = serde_json::from_str(&content[start..=end]).ok()?;
    let mut out = vec![];
    for item in v {
        let id = item.get("id")?.as_u64()? as usize;
        let text = item.get("text")?.as_str()?.to_string();
        out.push((id, text));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 从模型输出中提取 JSON 字符串数组（容忍 ```json 围栏与前后杂文）。
pub fn parse_text_array(content: &str) -> Option<Vec<String>> {
    let start = content.find('[')?;
    let end = content.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&content[start..=end]).ok()
}

/// 空白提示词视为未设置，回落到内置提示词。
fn effective_prompt(s: &LlmSettings) -> &str {
    s.prompt
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(DEFAULT_PROMPT)
}

/// 用最小请求验证端点与凭据可用。
pub fn test_connection(s: &LlmSettings) -> Result<()> {
    validate(s)?;
    let body = serde_json::json!({
        "model": s.model,
        "max_tokens": 8,
        "messages": [{"role": "user", "content": "只回复两个字符：ok"}],
    });
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(60))
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body)
        .context("连接失败")?;
    let v: serde_json::Value = resp.into_json().context("响应解析失败")?;
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    println!("端点返回：{}", text.trim());
    Ok(())
}

/// `llm setup`：交互式补齐缺失项并写盘。
pub fn setup_interactive(
    mut cfg: crate::settings::ConfigFile,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    disable_hint: bool,
) -> Result<crate::settings::ConfigFile> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut line = String::new();

    // 回车保留当前值；hint 为展示用的掩码（如 api_key 已配置时）。
    let mut ask = |prompt: &str, current: &str, hint: &str| -> String {
        line.clear();
        let _ = write!(out, "{prompt}");
        if !hint.is_empty() {
            let _ = write!(out, "[回车保留 {hint}] ");
        }
        let _ = write!(out, ": ");
        let _ = out.flush();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            return current.to_string();
        }
        let t = line.trim();
        if t.is_empty() {
            current.to_string()
        } else {
            t.to_string()
        }
    };

    cfg.llm.base_url = base_url.unwrap_or_else(|| {
        ask(
            "Base URL（OpenAI 兼容，如 https://api.deepseek.com/v1）",
            &cfg.llm.base_url,
            &cfg.llm.base_url,
        )
    });
    cfg.llm.api_key = api_key.unwrap_or_else(|| {
        ask(
            "API Key",
            &cfg.llm.api_key,
            if cfg.llm.api_key.is_empty() {
                ""
            } else {
                "已配置的 Key"
            },
        )
    });
    cfg.llm.model =
        model.unwrap_or_else(|| ask("模型名（如 deepseek-chat）", &cfg.llm.model, &cfg.llm.model));
    // 容错：没写 scheme 时补 https://
    if !cfg.llm.base_url.is_empty() && !cfg.llm.base_url.contains("://") {
        cfg.llm.base_url = format!("https://{}", cfg.llm.base_url.trim());
    }
    cfg.llm.disable_hint = disable_hint;
    cfg.llm.enabled = true;
    Ok(cfg)
}

pub fn print_status(cfg: &crate::settings::ConfigFile) {
    let s = &cfg.llm;
    println!("配置文件：{}", crate::settings::config_path().display());
    println!(
        "  LLM 润色：{}",
        if s.enabled { "已开启" } else { "已关闭" }
    );
    println!(
        "  base_url：{}",
        if s.base_url.is_empty() {
            "-"
        } else {
            &s.base_url
        }
    );
    let key_disp = if s.api_key.is_empty() {
        "-".to_string()
    } else {
        format!("{}...（已隐藏）", &s.api_key[..s.api_key.len().min(6)])
    };
    println!("  api_key ：{key_disp}");
    println!(
        "  model   ：{}",
        if s.model.is_empty() { "-" } else { &s.model }
    );
    println!(
        "  结束提示：{}",
        if s.disable_hint {
            "已关闭"
        } else {
            "开启"
        }
    );
    if !s.enabled && !s.disable_hint {
        println!("（运行 course2md llm setup 可开启）");
    }
}

pub fn write_hint_note(path: &std::path::Path) {
    let msg = if crate::i18n::is_zh() {
        format!(
            "\n提示：可用 LLM 自动润色字幕（修正语气词与明显识别错误），运行 `course2md llm setup` 一键开启。\n配置文件：{}（加 --no-llm-hint 或在配置中设 disable_hint 可关闭本提示）\n",
            path.display()
        )
    } else {
        format!(
            "\nTip: enable LLM transcript polishing to fix filler words and obvious ASR errors — run `course2md llm setup`.\nConfig: {} (suppress this tip with --no-llm-hint or disable_hint in the config)\n",
            path.display()
        )
    };
    let _ = std::io::stderr().write_all(msg.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_join() {
        assert_eq!(
            endpoint("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.x.com/v1/"),
            "https://api.x.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.x.com/v1/chat/completions"),
            "https://api.x.com/v1/chat/completions"
        );
    }

    #[test]
    fn parse_id_pairs_tolerates_fences() {
        let got = parse_id_text_pairs(
            "```json\n[{\"id\":0,\"text\":\"a\"},{\"id\":1,\"text\":\"b\"}]\n```",
        )
        .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        assert!(parse_id_text_pairs("没有数组").is_none());
        assert!(parse_id_text_pairs("[]").is_none());
    }

    #[test]
    fn section_helpers_preserve_section_boundaries() {
        let mut sections = vec![
            Section {
                t: 0.0,
                image: "a.jpg".into(),
                speech: vec![TranscriptEvent {
                    start: 0.0,
                    end: 1.0,
                    text: "first".into(),
                    raw: None,
                }],
            },
            Section {
                t: 10.0,
                image: "b.jpg".into(),
                speech: vec![
                    TranscriptEvent {
                        start: 10.0,
                        end: 11.0,
                        text: "second".into(),
                        raw: None,
                    },
                    TranscriptEvent {
                        start: 11.0,
                        end: 12.0,
                        text: "third".into(),
                        raw: None,
                    },
                ],
            },
        ];

        let (lengths, mut events) = flatten_sections(&sections);
        events[0].text = "polished first".into();
        restore_sections(&mut sections, &lengths, events);
        assert_eq!(sections[0].speech.len(), 1);
        assert_eq!(sections[1].speech.len(), 2);
        assert_eq!(sections[0].speech[0].text, "polished first");
    }
}
