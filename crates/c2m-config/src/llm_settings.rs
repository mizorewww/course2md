//! The AI service as configured: which one, its credentials, what it is asked to do.

use serde::{Deserialize, Serialize};

/// LLM 服务方言/登录方式。
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum LlmProvider {
    /// OpenAI 兼容 /chat/completions 端点
    // Shown as 2.0.1 showed it; files keep serde's `open-ai-compatible`.
    #[default]
    #[strum(serialize = "openai-compatible")]
    OpenAiCompatible,
    /// 本地 Ollama 服务（OpenAI 兼容方言，无需密钥）
    Ollama,
    /// OpenAI Codex 订阅登录（ChatGPT 后端 Responses API）
    Codex,
}

impl LlmProvider {
    pub fn as_str(&self) -> &'static str {
        self.into()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSettings {
    pub enabled: bool,
    /// 服务方言/登录方式（默认 OpenAI 兼容）
    pub provider: LlmProvider,
    /// OpenAI 兼容 base URL，如 https://api.deepseek.com/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 自定义校对指令（输出格式约束由系统自动追加，prompt 无法覆盖）
    pub prompt: Option<String>,
    /// 关闭「可开启 LLM」的结束提示
    pub disable_hint: bool,
    /// 视觉校对：每个请求附对应幻灯片截图，辅助纠正技术词汇（模型须支持图片输入）
    pub vision: bool,
    /// 独立生成视频摘要并写入笔记（不依赖校对 enabled）
    pub summarize: bool,
    /// Bilingual notes: the target language code (see [`crate::language::TARGETS`]).
    /// Skipped when the transcript is already in that language.
    pub translate: Option<String>,
    /// 校对并发数（chunk 间相互独立；自建网关/代理可调高）
    pub concurrency: usize,
    /// Send a failed AI request again on its own (with backoff), including one
    /// whose answer was lost and may already be charged. Off, such a request
    /// waits for the person's decision. Written only when changed, so files and
    /// saved task plans read by earlier versions stay as they were.
    #[serde(skip_serializing_if = "is_default_auto_retry")]
    pub auto_retry: bool,
    /// Sends per AI request, the first included, when `auto_retry` is on.
    #[serde(skip_serializing_if = "is_default_retry_attempts")]
    pub retry_attempts: u32,
}

/// AI requests are retried automatically unless the person turns it off.
pub const DEFAULT_AUTO_RETRY: bool = true;
/// One request and up to three retries.
pub const DEFAULT_RETRY_ATTEMPTS: u32 = 4;

fn is_default_auto_retry(value: &bool) -> bool {
    *value == DEFAULT_AUTO_RETRY
}

fn is_default_retry_attempts(value: &u32) -> bool {
    *value == DEFAULT_RETRY_ATTEMPTS
}

/// Credentials never reach logs or error text: the key shows only as set or not.
impl std::fmt::Debug for LlmSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmSettings")
            .field("enabled", &self.enabled)
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("api_key", &crate::config::Redacted(&self.api_key))
            .field("model", &self.model)
            .field("prompt", &self.prompt)
            .field("disable_hint", &self.disable_hint)
            .field("vision", &self.vision)
            .field("summarize", &self.summarize)
            .field("translate", &self.translate)
            .field("concurrency", &self.concurrency)
            .field("auto_retry", &self.auto_retry)
            .field("retry_attempts", &self.retry_attempts)
            .finish()
    }
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: LlmProvider::default(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            prompt: None,
            disable_hint: false,
            vision: false,
            summarize: false,
            translate: None,
            concurrency: DEFAULT_CONCURRENCY,
            auto_retry: DEFAULT_AUTO_RETRY,
            retry_attempts: DEFAULT_RETRY_ATTEMPTS,
        }
    }
}

impl LlmSettings {
    /// Whether any requested step sends text to the AI service.
    pub fn uses_service(&self) -> bool {
        self.enabled || self.summarize || self.translation_target().is_some()
    }

    /// The supported bilingual target, if one is set.
    pub fn translation_target(&self) -> Option<&'static str> {
        self.translate.as_deref().and_then(crate::language::target)
    }
}

/// LLM 校对默认并发数（chunk 间相互独立；可经 [llm] concurrency 调整）。
const DEFAULT_CONCURRENCY: usize = 8;

#[cfg(test)]
mod tests {
    use super::LlmProvider;

    #[test]
    fn providers_show_their_names_and_keep_their_file_form() {
        assert_eq!(
            LlmProvider::OpenAiCompatible.to_string(),
            "openai-compatible"
        );
        assert_eq!(LlmProvider::Ollama.as_str(), "ollama");
        assert_eq!(
            serde_json::to_string(&LlmProvider::OpenAiCompatible).unwrap(),
            r#""open-ai-compatible""#
        );
    }

    #[test]
    fn retry_defaults_are_not_written_and_changes_are() {
        let defaults = super::LlmSettings::default();
        let json = serde_json::to_value(&defaults).unwrap();
        assert!(json.get("auto_retry").is_none(), "{json}");
        assert!(json.get("retry_attempts").is_none(), "{json}");
        let changed = super::LlmSettings {
            auto_retry: false,
            retry_attempts: 2,
            ..Default::default()
        };
        let json = serde_json::to_value(&changed).unwrap();
        let back: super::LlmSettings = serde_json::from_value(json).unwrap();
        assert!(!back.auto_retry);
        assert_eq!(back.retry_attempts, 2);
    }
}
