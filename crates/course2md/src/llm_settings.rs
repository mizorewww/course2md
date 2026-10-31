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
    clap::ValueEnum,
    Serialize,
    Deserialize,
    strum::Display,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum LlmProvider {
    /// OpenAI 兼容 /chat/completions 端点
    #[default]
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
