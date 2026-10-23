//! Independently committed preferences, incomplete service drafts and immutable service versions.
//!
//! This is the desktop's configuration authority. Credentials never enter these files.
//! Calling `apply_defaults` or submitting a task cannot publish a service editor draft.

use crate::credentials::{CredentialRef, CredentialVault, Secret};
use anyhow::{Context, Result, anyhow, bail, ensure};
use course2md::config::{AsrProvider, TranscriptSource};
use course2md::settings::{AsrApiMode, ConfigFile, Defaults, DesktopSettings};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub type ServiceId = String;
pub type ServiceVersionId = String;
pub type ServiceDraftId = String;

const SCHEMA: u32 = 1;

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

fn valid_id(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

fn relative_path_inside(path: &Path, root: &Path) -> Option<PathBuf> {
    fn normalized(path: &Path) -> PathBuf {
        let mut value = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    value.pop();
                }
                component => value.push(component.as_os_str()),
            }
        }
        value
    }
    if !path.is_absolute() || !root.is_absolute() {
        return None;
    }
    let path = normalized(path);
    let root = normalized(root);
    path.strip_prefix(&root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            path.canonicalize()
                .ok()?
                .strip_prefix(root.canonicalize().ok()?)
                .ok()
                .map(Path::to_path_buf)
        })
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceGroup {
    Generation,
    Application,
    Services,
}

impl PreferenceGroup {
    pub(crate) fn filename(self) -> &'static str {
        match self {
            Self::Generation => "generation.json",
            Self::Application => "application.json",
            Self::Services => "services.json",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsIssueKind {
    Recovered,
    NeedsReset,
    SaveFailed,
}

#[derive(Debug, Clone)]
pub struct SettingsIssue {
    pub group: PreferenceGroup,
    pub kind: SettingsIssueKind,
    pub message: String,
    pub detail: Option<String>,
}

pub fn save_failure_message(group: PreferenceGroup, error: &anyhow::Error) -> String {
    use std::io::ErrorKind;
    let reason = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .map(|cause| match cause.kind() {
            ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => {
                course2md::tr!("设置目录暂时无法写入")
            }
            ErrorKind::StorageFull => course2md::tr!("设置所在磁盘空间不足"),
            ErrorKind::NotFound => course2md::tr!("设置位置暂时不可访问"),
            _ => course2md::tr!("设置文件暂时无法写入"),
        })
        .unwrap_or(course2md::tr!("设置变更暂时无法保存"));
    let name = match group {
        PreferenceGroup::Application => course2md::tr!("应用偏好"),
        PreferenceGroup::Generation => course2md::tr!("生成笔记偏好"),
        PreferenceGroup::Services => course2md::tr!("服务配置"),
    };
    course2md::tr!("{}未保存：{}", name, reason)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GenerationPreferences {
    /// Ordinary processing parameters only. Storage is owned by the library registry.
    pub options: Defaults,
    pub ai_proofread: bool,
    pub ai_summary: bool,
    /// Bilingual notes: target language code, or None for no translation.
    pub translate: Option<String>,
    pub vision: bool,
    pub prompt: Option<String>,
    /// Durable editor text is not an active processing rule until explicitly applied.
    pub prompt_draft: Option<String>,
    pub local_model_draft: Option<String>,
    /// Returning from an online service restores the last local engine choice.
    /// None means that local engine selection is automatic.
    pub last_local_provider: Option<AsrProvider>,
    pub subtitle_languages_draft: Option<String>,
    pub ai_concurrency: usize,
    pub preferred_subtitle_languages: Vec<String>,
}

impl Default for GenerationPreferences {
    fn default() -> Self {
        Self {
            options: Defaults {
                formats: Some(Vec::new()),
                resume: Some(true),
                keep_video: Some(false),
                ..Defaults::default()
            },
            ai_proofread: false,
            ai_summary: false,
            translate: None,
            vision: false,
            prompt: None,
            prompt_draft: None,
            local_model_draft: None,
            last_local_provider: None,
            subtitle_languages_draft: None,
            ai_concurrency: 2,
            preferred_subtitle_languages: Vec::new(),
        }
    }
}

impl GenerationPreferences {
    pub fn select_provider(&mut self, provider: Option<AsrProvider>) {
        if provider == Some(AsrProvider::Api) {
            if self.options.provider != Some(AsrProvider::Api) {
                self.last_local_provider = self.options.provider;
            }
        } else {
            self.last_local_provider = provider;
        }
        self.options.provider = provider;
    }

    pub fn select_recognition_location(&mut self, online: bool) {
        let provider = if online {
            Some(AsrProvider::Api)
        } else if self.options.provider == Some(AsrProvider::Api) {
            self.last_local_provider
                .filter(|provider| *provider != AsrProvider::Api)
        } else {
            self.options.provider
        };
        self.select_provider(provider);
    }

    pub fn effective_vision(&self) -> bool {
        self.ai_proofread && self.vision
    }

    pub fn apply_to(&self, config: &mut ConfigFile) {
        let out = config.defaults.out.clone();
        config.defaults = self.options.clone();
        config.defaults.out = out;
        config.defaults.resume = Some(true);
        config.llm.enabled = self.ai_proofread;
        config.llm.summarize = self.ai_summary;
        config.llm.translate = self.translate.clone();
        config.llm.vision = self.effective_vision();
        config.llm.prompt = self.prompt.clone();
        config.llm.concurrency = self.ai_concurrency;
        config.llm.disable_hint = true;
        clear_service_fields(config);
    }
}

/// 界面文字大小档位：应用、下拉选项、持久化校验共用同一白名单
pub const FONT_SCALES: [f32; 4] = [1.0, 1.25, 1.5, 2.0];

/// The interface language, including the default that follows the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InterfaceLanguage {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-Hans")]
    ZhHans,
    #[serde(rename = "zh-Hant")]
    ZhHant,
    #[serde(rename = "ja")]
    Ja,
}

impl InterfaceLanguage {
    pub const ALL: [Self; 5] = [Self::System, Self::En, Self::ZhHans, Self::ZhHant, Self::Ja];

    pub fn resolve(self) -> course2md::i18n::Lang {
        match self {
            Self::System => course2md::i18n::Lang::detect(),
            Self::En => course2md::i18n::Lang::En,
            Self::ZhHans => course2md::i18n::Lang::ZhHans,
            Self::ZhHant => course2md::i18n::Lang::ZhHant,
            Self::Ja => course2md::i18n::Lang::Ja,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationPreferences {
    pub desktop: DesktopSettings,
    pub font_scale: f32,
    pub appearance: crate::palettes::ThemePreferences,
    /// Interface language; `System` follows the operating system at launch.
    pub language: InterfaceLanguage,
}

impl Default for ApplicationPreferences {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            appearance: Default::default(),
            language: InterfaceLanguage::default(),
            desktop: DesktopSettings {
                system_titlebar: true,
                setup_completed: false,
                ..DesktopSettings::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePurpose {
    Speech,
    Ai,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceProtocol {
    SpeechTranscriptions,
    SpeechChat,
    AiChat,
    /// 本地 Ollama 服务（OpenAI 兼容方言，无需密钥）
    OllamaChat,
    /// OpenAI Codex 订阅登录（ChatGPT 后端 Responses API；凭据由 CLI 令牌文件持有）
    CodexResponses,
}

impl ServiceProtocol {
    pub fn purpose(self) -> ServicePurpose {
        match self {
            Self::SpeechTranscriptions | Self::SpeechChat => ServicePurpose::Speech,
            Self::AiChat | Self::OllamaChat | Self::CodexResponses => ServicePurpose::Ai,
        }
    }

    /// Choice order: account sign-in, then this computer, then API services.
    pub const ALL: [Self; 5] = [
        Self::SpeechTranscriptions,
        Self::SpeechChat,
        Self::CodexResponses,
        Self::OllamaChat,
        Self::AiChat,
    ];

    /// The services of one purpose, in choice order.
    pub fn for_purpose(purpose: ServicePurpose) -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(move |protocol| protocol.purpose() == purpose)
    }

    pub fn endpoint_suffix(self) -> &'static str {
        match self {
            Self::SpeechTranscriptions => "/audio/transcriptions",
            Self::SpeechChat | Self::AiChat | Self::OllamaChat => "/chat/completions",
            Self::CodexResponses => "/responses",
        }
    }

    /// 无需 API Key 的服务类型：Ollama 本地服务不鉴权，Codex 由 CLI 令牌文件鉴权。
    pub fn keyless(self) -> bool {
        matches!(self, Self::OllamaChat | Self::CodexResponses)
    }

    /// The service type's choice label, shared by the editor and setup.
    pub fn choice_label(self) -> &'static str {
        match self {
            Self::SpeechTranscriptions => course2md::tr!("专用转写（如 Whisper）"),
            Self::SpeechChat => course2md::tr!("多模态对话（如 GPT-4o 音频）"),
            Self::AiChat => course2md::tr!("OpenAI 兼容服务"),
            Self::OllamaChat => course2md::tr!("Ollama 本地服务"),
            Self::CodexResponses => course2md::tr!("OpenAI Codex 订阅"),
        }
    }

    /// How the service is reached, heading its group in the type menu.
    pub fn choice_group(self) -> &'static str {
        match self {
            Self::CodexResponses => course2md::tr!("用账号登录"),
            Self::OllamaChat => course2md::tr!("本机"),
            Self::SpeechTranscriptions | Self::SpeechChat | Self::AiChat => "API",
        }
    }

    /// What choosing this type means for the person setting it up.
    pub fn choice_detail(self) -> &'static str {
        match self {
            Self::SpeechTranscriptions => course2md::tr!("上传音频，返回转写文字"),
            Self::SpeechChat => course2md::tr!("支持音频输入的对话模型"),
            Self::AiChat => course2md::tr!("填写接口地址和 API Key，适用于大多数云端服务"),
            Self::OllamaChat => course2md::tr!("使用本机运行的模型，不需要 API Key"),
            Self::CodexResponses => course2md::tr!("使用 ChatGPT 订阅，不需要 API Key"),
        }
    }

    /// 该协议对应的 CLI LLM 方言（仅 AI 用途参与映射）。
    pub fn llm_provider(self) -> course2md::llm::LlmProvider {
        match self {
            Self::OllamaChat => course2md::llm::LlmProvider::Ollama,
            Self::CodexResponses => course2md::llm::LlmProvider::Codex,
            _ => course2md::llm::LlmProvider::OpenAiCompatible,
        }
    }
}

fn url_credentials() -> &'static str {
    course2md::tr!("地址里不能带密钥或 ? # 等附加内容，请把密钥填到 API Key")
}

/// Codex 订阅登录的固定服务地址（CLI 方言端点固定，见 course2md::provider）。
pub const CODEX_ADDRESS: &str = course2md::provider::CODEX_BASE_URL;
/// Ollama 本地服务的默认地址（与 CLI 登录一致）。
pub const OLLAMA_DEFAULT_ADDRESS: &str = "http://localhost:11434";

/// 地址在其协议下归一化后的展示主机名——空服务名称的自动取值来源。
pub fn service_host(address: &str, protocol: ServiceProtocol) -> Option<String> {
    let endpoint = normalize_endpoint(address, protocol).ok()?;
    url::Url::parse(&endpoint)
        .ok()?
        .host_str()
        .map(str::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    ApiKey,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceDraft {
    pub id: ServiceDraftId,
    pub service_id: ServiceId,
    pub revision: u64,
    pub name: String,
    pub protocol: ServiceProtocol,
    pub address: String,
    pub model: String,
    pub authentication: Authentication,
    pub credential: Option<CredentialRef>,
    /// The variable name is informational. The captured value lives in the vault.
    pub credential_source: Option<String>,
    pub based_on: Option<ServiceVersionId>,
}

impl ServiceDraft {
    pub fn new(purpose: ServicePurpose) -> Self {
        Self {
            id: new_id("service-draft"),
            service_id: new_id("service"),
            revision: 0,
            name: String::new(),
            protocol: match purpose {
                ServicePurpose::Speech => ServiceProtocol::SpeechTranscriptions,
                ServicePurpose::Ai => ServiceProtocol::AiChat,
            },
            address: String::new(),
            model: String::new(),
            authentication: Authentication::ApiKey,
            credential: None,
            credential_source: None,
            based_on: None,
        }
    }

    pub fn from_version(version: &ServiceVersion) -> Self {
        Self {
            id: new_id("service-draft"),
            service_id: version.service_id.clone(),
            revision: 0,
            name: version.config.name.clone(),
            protocol: version.config.protocol,
            address: version.config.endpoint.clone(),
            model: version.config.model.clone(),
            authentication: version.config.authentication,
            credential: version.config.credential.clone(),
            credential_source: version.config.credential_source.clone(),
            based_on: Some(version.id.clone()),
        }
    }

    pub fn validate(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if let Err(error) = normalize_endpoint(&self.address, self.protocol) {
            errors.push(FieldError {
                field: "address",
                message: error.to_string(),
            });
        }
        if self.model.trim().is_empty() {
            errors.push(FieldError {
                field: "model",
                message: course2md::tr!("请输入服务提供的模型 ID").into(),
            });
        } else if self.model.chars().any(char::is_control) {
            errors.push(FieldError {
                field: "model",
                message: course2md::tr!("模型 ID 不能包含换行或控制字符").into(),
            });
        }
        if !self.protocol.keyless()
            && self.authentication == Authentication::ApiKey
            && self.credential.is_none()
        {
            errors.push(FieldError {
                field: "api_key",
                message: course2md::tr!("此认证方式需要 API Key").into(),
            });
        }
        errors
    }

    /// Static validation only: saving and constructing a test never send a network request.
    pub fn configuration(&self) -> Result<ServiceConfiguration> {
        let errors = self.validate();
        if !errors.is_empty() {
            bail!(
                "{}",
                errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join(course2md::tr!("；"))
            );
        }
        let endpoint = normalize_endpoint(&self.address, self.protocol)?;
        let host = url::Url::parse(&endpoint)?
            .host_str()
            .unwrap_or_default()
            .to_owned();
        // 无密钥协议（Ollama 本地、Codex 登录态）不持有任何凭据
        let authentication = if self.protocol.keyless() {
            Authentication::None
        } else {
            self.authentication
        };
        Ok(ServiceConfiguration {
            name: if self.name.trim().is_empty() {
                host
            } else {
                self.name.trim().to_owned()
            },
            protocol: self.protocol,
            endpoint,
            model: self.model.trim().to_owned(),
            authentication,
            credential: if authentication == Authentication::ApiKey {
                self.credential.clone()
            } else {
                None
            },
            credential_source: if authentication == Authentication::ApiKey {
                self.credential_source.clone()
            } else {
                None
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfiguration {
    pub name: String,
    pub protocol: ServiceProtocol,
    /// A normalized, complete request URL, never an address containing credentials.
    pub endpoint: String,
    pub model: String,
    pub authentication: Authentication,
    pub credential: Option<CredentialRef>,
    pub credential_source: Option<String>,
}

impl ServiceConfiguration {
    pub fn host(&self) -> String {
        url::Url::parse(&self.endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Structured identity rather than an unstable or collision-prone hash. It contains only
    /// public configuration and opaque credential IDs, never secret values. Display names do
    /// not invalidate evidence; the actual content contract does.
    pub fn fingerprint(&self, contract: &str) -> String {
        serde_json::to_string(&(
            self.protocol,
            &self.endpoint,
            &self.model,
            self.authentication,
            &self.credential,
            contract,
        ))
        .expect("serializing service identity cannot fail")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceVersion {
    pub id: ServiceVersionId,
    pub service_id: ServiceId,
    pub number: u64,
    pub saved_at: u64,
    #[serde(default)]
    pub published_from: Option<ServiceDraftId>,
    pub config: ServiceConfiguration,
}

impl ServiceVersion {
    /// Field-level differences against another version of the same service, in
    /// display order (name/model/endpoint/protocol/auth). Shared by the service
    /// picker's "已修改：…" label.
    pub fn changes_since(&self, old: &ServiceVersion) -> Vec<&'static str> {
        let mut changed = Vec::new();
        if old.config.name != self.config.name {
            changed.push(course2md::tr!("名称"));
        }
        if old.config.model != self.config.model {
            changed.push(course2md::tr!("模型"));
        }
        if old.config.endpoint != self.config.endpoint {
            changed.push(course2md::tr!("地址"));
        }
        if old.config.protocol != self.config.protocol {
            changed.push(course2md::tr!("服务类型"));
        }
        if old.config.authentication != self.config.authentication
            || old.config.credential != self.config.credential
        {
            changed.push(course2md::tr!("认证"));
        }
        changed
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceRefs {
    pub asr: Option<ServiceVersionId>,
    pub llm: Option<ServiceVersionId>,
}

impl ServiceRefs {
    pub fn required_for(&self, config: &ConfigFile) -> Self {
        Self {
            asr: (config.defaults.provider == Some(AsrProvider::Api)
                && config.defaults.transcript_source != Some(TranscriptSource::Subtitle))
            .then(|| self.asr.clone())
            .flatten(),
            llm: config
                .llm
                .uses_service()
                .then(|| self.llm.clone())
                .flatten(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingScope {
    /// A save in settings or an explicit "also use by default" choice.
    Defaults,
    /// The caller binds the returned ID to exactly one generation draft.
    CurrentTask,
    /// Saved for later choice; defaults and drafts are unchanged.
    Saved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    Passed,
    AuthenticationRefused,
    ModelRefused,
    ContractMismatch,
    SampleMismatch,
    OutcomeUnknown,
    NotSent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceTestEvidence {
    pub fingerprint: String,
    pub contract: String,
    pub tested_at: u64,
    pub outcome: TestOutcome,
    pub message: String,
    /// Safe, bounded, structured details, never raw response headers or secret-bearing bodies.
    pub details: Vec<String>,
    /// The HTTP status the service answered with, when it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct ServicesState {
    drafts: BTreeMap<ServiceDraftId, ServiceDraft>,
    versions: BTreeMap<ServiceVersionId, ServiceVersion>,
    defaults: ServiceRefs,
    /// Deleted service ids. Independent marker files keep dispatch blocked if
    /// an older services.json backup is restored.
    stopped: BTreeMap<ServiceId, u64>,
    tests: BTreeMap<String, ServiceTestEvidence>,
    discarded_drafts: BTreeSet<ServiceDraftId>,
}

/// Runtime-only configuration. Deliberately neither Debug nor Serialize. Consume immediately
/// into the process stdin payload; do not pass it back to preference or task persistence.
pub struct ResolvedConfig(ConfigFile);

impl ResolvedConfig {
    pub fn into_config(mut self) -> ConfigFile {
        std::mem::take(&mut self.0)
    }
}

impl Drop for ResolvedConfig {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.asr_api.api_key.zeroize();
        self.0.llm.api_key.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    schema: u32,
    revision: u64,
    value: T,
}

trait Validate {
    fn validate_value(&self) -> Result<()>;
}

impl Validate for GenerationPreferences {
    fn validate_value(&self) -> Result<()> {
        if self.options.out.is_some() {
            bail!(course2md::tr!("保存位置由存储设置管理"));
        }
        if self.ai_concurrency == 0 || self.ai_concurrency > 64 {
            bail!(course2md::tr!("AI 并发请求数需要在 1 到 64 之间"));
        }
        for (name, value) in [
            (course2md::tr!("画面相似度"), self.options.similarity),
            (course2md::tr!("画面采样间隔"), self.options.sample_interval),
            (course2md::tr!("画面间隔"), self.options.cooldown),
            (course2md::tr!("稳定画面时长"), self.options.stable_secs),
        ] {
            if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                bail!(course2md::tr!("{}需要是有效的非负数", name));
            }
        }
        if self.options.similarity.is_some_and(|value| value > 1.0) {
            bail!(course2md::tr!("画面相似度需要在 0 到 1 之间"));
        }
        if self
            .options
            .max_speech
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            bail!(course2md::tr!("语音分段长度需要大于 0"));
        }
        Ok(())
    }
}

impl Validate for ApplicationPreferences {
    fn validate_value(&self) -> Result<()> {
        if !self.appearance.valid() {
            bail!(course2md::tr!("请为浅色和深色外观分别选择对应的主题"));
        }
        if !crate::preferences::FONT_SCALES.contains(&self.font_scale) {
            bail!(course2md::tr!("请选择 100%、125%、150% 或 200% 的文字大小"));
        }
        Ok(())
    }
}

impl Validate for ServicesState {
    fn validate_value(&self) -> Result<()> {
        for (id, draft) in &self.drafts {
            if id != &draft.id
                || !valid_id(id, "service-draft-")
                || !valid_id(&draft.service_id, "service-")
            {
                bail!(course2md::tr!("服务设置记录不完整"));
            }
        }
        for (id, version) in &self.versions {
            if id != &version.id
                || !valid_id(id, "service-version-")
                || !valid_id(&version.service_id, "service-")
            {
                bail!(course2md::tr!("服务版本记录不完整"));
            }
            let config = &version.config;
            if normalize_endpoint(&config.endpoint, config.protocol)? != config.endpoint
                || config.model.trim().is_empty()
                || (config.authentication == Authentication::ApiKey && config.credential.is_none())
            {
                bail!(course2md::tr!("有效服务版本记录不完整"));
            }
        }
        for (reference, purpose) in [
            (&self.defaults.asr, ServicePurpose::Speech),
            (&self.defaults.llm, ServicePurpose::Ai),
        ] {
            if let Some(reference) = reference {
                let version = self
                    .versions
                    .get(reference)
                    .ok_or_else(|| anyhow!(course2md::tr!("默认服务版本不存在")))?;
                if version.config.protocol.purpose() != purpose {
                    bail!(course2md::tr!("默认服务用途不匹配"));
                }
            }
        }
        Ok(())
    }
}

pub(crate) struct PreferenceWriteResult {
    group: PreferenceGroup,
    revision: u64,
    result: Result<(), String>,
}

pub(crate) enum PreferenceSignal {
    Write {
        path: PathBuf,
        bytes: Vec<u8>,
        group: PreferenceGroup,
        revision: u64,
    },
    Recovery {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    Remove {
        path: PathBuf,
    },
    Flush(std::sync::mpsc::Sender<()>),
}

/// Where preference files are written. The store hands every write to its
/// writer and learns the outcome only through `try_result`, so the UI never
/// waits on the disk. Production uses [`ThreadWriter`]; tests inject
/// [`InlineWriter`], which performs the same work as soon as it is sent.
pub(crate) trait Writer: Send {
    fn send(&self, signal: PreferenceSignal) -> Result<()>;
    fn try_result(&self) -> Option<PreferenceWriteResult>;
    fn set_wake(&self, wake: smol::channel::Sender<()>);
    /// Waits until every signal sent so far has been performed.
    fn flush(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.send(PreferenceSignal::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

/// Performs signals and reports their results; shared by every writer.
struct WriterSink {
    root: PathBuf,
    results: std::sync::mpsc::Sender<PreferenceWriteResult>,
    wake: std::sync::Mutex<Option<smol::channel::Sender<()>>>,
}

impl WriterSink {
    fn new(root: PathBuf) -> (Self, std::sync::mpsc::Receiver<PreferenceWriteResult>) {
        let (results, receiver) = std::sync::mpsc::channel();
        (
            Self {
                root,
                results,
                wake: Default::default(),
            },
            receiver,
        )
    }

    fn perform(&self, signal: PreferenceSignal) {
        match signal {
            PreferenceSignal::Flush(ack) => {
                let _ = ack.send(());
            }
            PreferenceSignal::Recovery { path, bytes } => {
                let _ = atomic_write_with_retry(&path, &bytes);
            }
            PreferenceSignal::Remove { path } => {
                let _ = std::fs::remove_file(path);
            }
            PreferenceSignal::Write {
                path,
                bytes,
                group,
                revision,
            } => {
                let result = write_preference_file(&self.root, &path, &bytes)
                    .map_err(|error| format!("{error:#}"));
                let _ = self.results.send(PreferenceWriteResult {
                    group,
                    revision,
                    result,
                });
            }
        }
        if let Some(wake) = course2md::runtime::lock_recover(&self.wake).as_ref() {
            let _ = wake.try_send(());
        }
    }
}

fn writer_stopped() -> anyhow::Error {
    anyhow!(course2md::tr!("设置保存线程已停止"))
}

/// Writes on one background thread, in the order signals were sent.
pub(crate) struct ThreadWriter {
    signals: std::sync::mpsc::Sender<PreferenceSignal>,
    results: std::sync::mpsc::Receiver<PreferenceWriteResult>,
    sink: Arc<WriterSink>,
}

impl ThreadWriter {
    pub(crate) fn new(root: PathBuf) -> Self {
        let (sink, results) = WriterSink::new(root);
        let sink = Arc::new(sink);
        let (signals, receiver) = std::sync::mpsc::channel();
        let thread_sink = Arc::clone(&sink);
        std::thread::Builder::new()
            .name("course2md-preferences-writer".into())
            .spawn(move || {
                while let Ok(signal) = receiver.recv() {
                    thread_sink.perform(signal);
                }
            })
            .expect("preferences writer thread");
        Self {
            signals,
            results,
            sink,
        }
    }
}

impl Writer for ThreadWriter {
    fn send(&self, signal: PreferenceSignal) -> Result<()> {
        self.signals.send(signal).map_err(|_| writer_stopped())
    }
    fn try_result(&self) -> Option<PreferenceWriteResult> {
        self.results.try_recv().ok()
    }
    fn set_wake(&self, wake: smol::channel::Sender<()>) {
        *course2md::runtime::lock_recover(&self.sink.wake) = Some(wake);
    }
}

/// Performs each signal before `send` returns. Results are still collected
/// through `try_result`, exactly as from the background writer.
#[cfg(test)]
pub(crate) struct InlineWriter {
    sink: WriterSink,
    results: std::sync::Mutex<std::sync::mpsc::Receiver<PreferenceWriteResult>>,
}

#[cfg(test)]
impl InlineWriter {
    pub(crate) fn new(root: PathBuf) -> Self {
        let (sink, results) = WriterSink::new(root);
        Self {
            sink,
            results: std::sync::Mutex::new(results),
        }
    }
}

#[cfg(test)]
impl Writer for InlineWriter {
    fn send(&self, signal: PreferenceSignal) -> Result<()> {
        self.sink.perform(signal);
        Ok(())
    }
    fn try_result(&self) -> Option<PreferenceWriteResult> {
        course2md::runtime::lock_recover(&self.results)
            .try_recv()
            .ok()
    }
    fn set_wake(&self, wake: smol::channel::Sender<()>) {
        *course2md::runtime::lock_recover(&self.sink.wake) = Some(wake);
    }
}

fn write_preference_file(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(previous) = std::fs::read(path) {
        if serde_json::from_slice::<serde_json::Value>(&previous).is_ok() {
            atomic_write_with_retry(&path.with_extension("json.bak"), &previous)
                .context(course2md::tr!("无法保存此组设置的恢复副本"))?;
        } else {
            // Never replace the last good backup with a corrupt primary file.
            preserve_original(root, path)?;
        }
    }
    atomic_write_with_retry(path, bytes).context(course2md::tr!("无法保存此组设置"))?;
    Ok(())
}

pub struct Store {
    root: PathBuf,
    vault: Arc<dyn CredentialVault>,
    generation: GenerationPreferences,
    application: ApplicationPreferences,
    services: ServicesState,
    revisions: BTreeMap<PreferenceGroup, u64>,
    blocked: BTreeSet<PreferenceGroup>,
    issues: Vec<SettingsIssue>,
    generation_intent: Option<GenerationPreferences>,
    /// The generation value still on disk while a newer one is unconfirmed.
    durable_generation: Option<GenerationPreferences>,
    application_intent: Option<ApplicationPreferences>,
    recovery_drafts: BTreeSet<ServiceDraftId>,
    writer: Box<dyn Writer>,
    pending_writes: BTreeMap<PreferenceGroup, u64>,
    services_intent: Option<ServicesState>,
    pending_service_recovery: BTreeSet<String>,
}

impl Store {
    /// Loading a broken group never makes reading notes or unrelated settings unavailable.
    pub fn open(root: impl Into<PathBuf>, vault: Arc<dyn CredentialVault>) -> Self {
        let root = root.into();
        let writer = ThreadWriter::new(root.clone());
        Self::open_with(root, vault, writer)
    }

    /// `open` with an explicit writer; tests inject [`InlineWriter`].
    pub(crate) fn open_with(
        root: impl Into<PathBuf>,
        vault: Arc<dyn CredentialVault>,
        writer: impl Writer + 'static,
    ) -> Self {
        let mut store = Self {
            root: root.into(),
            vault,
            generation: GenerationPreferences::default(),
            application: ApplicationPreferences::default(),
            services: ServicesState::default(),
            revisions: BTreeMap::new(),
            blocked: BTreeSet::new(),
            issues: Vec::new(),
            generation_intent: None,
            durable_generation: None,
            application_intent: None,
            recovery_drafts: BTreeSet::new(),
            writer: Box::new(writer),
            pending_writes: BTreeMap::new(),
            services_intent: None,
            pending_service_recovery: BTreeSet::new(),
        };
        store.generation = store.load_group(PreferenceGroup::Generation);
        store.application = store.load_group(PreferenceGroup::Application);
        store.services = store.load_group(PreferenceGroup::Services);
        store.load_recovery_intents();
        store.forget_retired_service_records();
        store
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn set_wake_sender(&self, sender: smol::channel::Sender<()>) {
        self.writer.set_wake(sender);
    }
    pub fn flush(&self) {
        self.writer.flush();
    }
    pub fn is_pending(&self, group: PreferenceGroup) -> bool {
        self.pending_writes.contains_key(&group)
    }
    pub fn has_pending_services(&self) -> bool {
        self.services_intent.is_some()
    }
    pub fn retry_services(&mut self) -> Result<()> {
        let next = self
            .services_intent
            .clone()
            .ok_or_else(|| anyhow!(course2md::tr!("没有待保存的服务设置")))?;
        self.persist(PreferenceGroup::Services, &next)
    }
    /// Drain writer completions. The UI calls this from its event poll.
    pub fn process_writes(&mut self) -> Vec<(PreferenceGroup, Result<(), String>)> {
        let mut completed = Vec::new();
        while let Some(result) = self.writer.try_result() {
            let latest = self.pending_writes.get(&result.group) == Some(&result.revision);
            if latest {
                self.pending_writes.remove(&result.group);
            }
            if result.result.is_ok() {
                if latest {
                    self.clear_intent(result.group);
                    self.issues.retain(|issue| {
                        issue.group != result.group || issue.kind != SettingsIssueKind::SaveFailed
                    });
                }
            } else if let Err(error) = &result.result {
                self.issues.push(SettingsIssue {
                    group: result.group,
                    kind: SettingsIssueKind::SaveFailed,
                    message: save_failure_message(result.group, &anyhow!(error.clone())),
                    detail: Some(error.clone()),
                });
            }
            completed.push((result.group, result.result));
        }
        completed
    }
    fn clear_intent(&mut self, group: PreferenceGroup) {
        let file = match group {
            PreferenceGroup::Generation => {
                self.generation_intent = None;
                self.durable_generation = None;
                "generation-intent.json"
            }
            PreferenceGroup::Application => {
                self.application_intent = None;
                "application-intent.json"
            }
            PreferenceGroup::Services => {
                self.services_intent = None;
                for id in std::mem::take(&mut self.pending_service_recovery) {
                    self.recovery_drafts.remove(&id);
                    let _ = self.writer.send(PreferenceSignal::Remove {
                        path: self.recovery_root().join(format!("{id}.json")),
                    });
                }
                return;
            }
        };
        let _ = self.writer.send(PreferenceSignal::Remove {
            path: self.recovery_root().join(file),
        });
    }
    pub fn vault(&self) -> Arc<dyn CredentialVault> {
        Arc::clone(&self.vault)
    }
    pub fn available_environment_credentials(purpose: ServicePurpose) -> Vec<&'static str> {
        let names: &[&str] = match purpose {
            ServicePurpose::Speech => &["COURSE2MD_ASR_API_KEY", "OPENROUTER_API_KEY"],
            ServicePurpose::Ai => &["OPENAI_API_KEY", "OPENROUTER_API_KEY"],
        };
        names
            .iter()
            .copied()
            .filter(|name| {
                std::env::var(name)
                    .ok()
                    .map(Secret::new)
                    .is_some_and(|secret| !secret.is_empty())
            })
            .collect()
    }
    /// The user selects a named environment value explicitly. Capture it into a new vault
    /// entry now; queued tasks never consult the later process environment.
    pub fn capture_environment_credential(
        &mut self,
        mut draft: ServiceDraft,
        name: &str,
    ) -> Result<ServiceDraft> {
        if !Self::available_environment_credentials(draft.protocol.purpose()).contains(&name) {
            bail!(course2md::tr!(
                "这个环境变量当前没有可用的凭据，请重新选择或填写 API Key"
            ));
        }
        let value = Secret::new(
            std::env::var(name).map_err(|_| anyhow!(course2md::tr!("无法读取所选环境变量")))?,
        );
        let reference = self.vault.insert(value)?;
        draft.credential = Some(reference.clone());
        draft.credential_source = Some(course2md::tr!("环境变量 {}", name));
        draft.authentication = Authentication::ApiKey;
        match self.save_service_draft(draft, None) {
            Ok(draft) => Ok(draft),
            Err(error) => {
                let _ = self.vault.remove(&reference);
                Err(error)
            }
        }
    }
    pub fn generation(&self) -> &GenerationPreferences {
        &self.generation
    }
    pub fn generation_intent(&self) -> Option<&GenerationPreferences> {
        self.generation_intent.as_ref()
    }
    pub fn application_intent(&self) -> Option<&ApplicationPreferences> {
        self.application_intent.as_ref()
    }
    /// An unsuccessful preference commit may still be preserved as a non-active recovery
    /// record. This allows an ordinary exit without either losing the edit or activating it.
    pub fn unsaved_intents_are_preserved(&self) -> bool {
        let generation = self.generation_intent.as_ref().is_none_or(|intent| {
            read_envelope::<GenerationPreferences>(
                &self.recovery_root().join("generation-intent.json"),
            )
            .ok()
            .flatten()
            .is_some_and(|saved| saved.value == *intent)
        });
        let application = self.application_intent.as_ref().is_none_or(|intent| {
            read_envelope::<ApplicationPreferences>(
                &self.recovery_root().join("application-intent.json"),
            )
            .ok()
            .flatten()
            .is_some_and(|saved| saved.value == *intent)
        });
        generation && application
    }
    #[cfg(test)]
    fn is_recovery_draft(&self, id: &str) -> bool {
        self.recovery_drafts.contains(id)
    }
    pub fn revision(&self, group: PreferenceGroup) -> u64 {
        self.revisions.get(&group).copied().unwrap_or(0)
    }
    pub fn application(&self) -> &ApplicationPreferences {
        &self.application
    }
    pub fn issues(&self) -> &[SettingsIssue] {
        &self.issues
    }
    pub fn is_blocked(&self, group: PreferenceGroup) -> bool {
        self.blocked.contains(&group)
    }
    pub fn default_refs(&self) -> ServiceRefs {
        self.services.defaults.clone()
    }
    #[cfg(test)]
    fn service_drafts(&self) -> impl Iterator<Item = &ServiceDraft> {
        self.services.drafts.values()
    }
    pub fn draft(&self, id: &str) -> Option<&ServiceDraft> {
        self.services.drafts.get(id)
    }
    pub fn versions(&self) -> impl Iterator<Item = &ServiceVersion> {
        self.services.versions.values()
    }
    /// 每个 service_id 只保留最新 version（列表页与选择器共用）
    pub fn latest_versions(&self) -> BTreeMap<String, ServiceVersion> {
        let mut latest = BTreeMap::<String, ServiceVersion>::new();
        for version in self.versions() {
            if latest
                .get(&version.service_id)
                .is_none_or(|old| old.number < version.number)
            {
                latest.insert(version.service_id.clone(), version.clone());
            }
        }
        latest
    }
    pub fn version(&self, id: &str) -> Option<&ServiceVersion> {
        self.services.versions.get(id)
    }
    pub fn is_service_retired(&self, service_id: &str) -> bool {
        self.services.stopped.contains_key(service_id) || self.retired_marker(service_id).exists()
    }
    /// Display the currently loaded state without probing the filesystem.
    /// Actual publication, submission and execution still use dispatch checks.
    pub fn service_retired_in_snapshot(&self, service_id: &str) -> bool {
        self.services.stopped.contains_key(service_id)
    }
    fn retired_marker(&self, service_id: &str) -> PathBuf {
        self.root.join("stopped-services").join(service_id)
    }
    /// The marker's presence is the whole record; nothing reads its body.
    fn write_retire_marker(&self, service_id: &str) -> Result<()> {
        course2md::checkpoint::atomic_write(&self.retired_marker(service_id), b"deleted\n")
    }
    fn forget_retired_service_records(&mut self) {
        if self.is_blocked(PreferenceGroup::Services) {
            return;
        }
        let retired: BTreeSet<_> = self
            .services
            .stopped
            .keys()
            .cloned()
            .chain(
                std::fs::read_dir(self.root.join("stopped-services"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .map(str::to_owned)
                            .filter(|name| !name.starts_with('.'))
                    }),
            )
            .collect();
        if retired.is_empty() {
            return;
        }
        let leftover: Vec<_> = self
            .services
            .versions
            .values()
            .filter(|version| retired.contains(&version.service_id))
            .cloned()
            .collect();
        let leftover_drafts = self
            .services
            .drafts
            .values()
            .any(|draft| retired.contains(&draft.service_id));
        if leftover.is_empty() && !leftover_drafts {
            return;
        }
        for service_id in &retired {
            // Legacy tombstones lived only in services.json; ensure the
            // independent marker exists so a restored backup cannot revive
            // dispatch for this service.
            if !self.retired_marker(service_id).exists() {
                let _ = self.write_retire_marker(service_id);
            }
        }
        let mut next = self.services.clone();
        next.versions
            .retain(|_, version| !retired.contains(&version.service_id));
        next.drafts
            .retain(|_, draft| !retired.contains(&draft.service_id));
        if next
            .defaults
            .asr
            .as_ref()
            .is_some_and(|id| !next.versions.contains_key(id))
        {
            next.defaults.asr = None;
        }
        if next
            .defaults
            .llm
            .as_ref()
            .is_some_and(|id| !next.versions.contains_key(id))
        {
            next.defaults.llm = None;
        }
        if self.persist(PreferenceGroup::Services, &next).is_ok() {
            self.services = next;
        }
    }
    pub fn test_evidence(
        &self,
        config: &ServiceConfiguration,
        contract: &str,
    ) -> Option<&ServiceTestEvidence> {
        self.services.tests.get(&config.fingerprint(contract))
    }

    pub fn apply_defaults(&self, config: &mut ConfigFile) {
        self.generation.apply_to(config);
        config.desktop = self.application.desktop.clone();
    }

    pub fn defaults_config(&self) -> ConfigFile {
        let mut config = ConfigFile::default();
        self.apply_defaults(&mut config);
        config
    }

    /// A library move changes only paths physically inside that library. Keep a pending
    /// ordinary edit intact, and let the existing recovery journal retain a failed commit.
    pub fn relocate_generation_paths(&mut self, old: &Path, new: &Path) -> Result<()> {
        let mut next = self
            .generation_intent
            .as_ref()
            .unwrap_or(&self.generation)
            .clone();
        let Some(path) = &mut next.options.model_dir else {
            return Ok(());
        };
        let Some(relative) = relative_path_inside(path, old) else {
            return Ok(());
        };
        *path = new.join(relative);
        self.save_generation(next)
    }

    /// Backup cleanup must retain the old path while any effective or pending preference
    /// still names a model directory there. Merely copying the library does not release it.
    pub fn references_storage_path(&self, root: &Path) -> bool {
        std::iter::once(&self.generation)
            .chain(self.generation_intent.iter())
            .chain(self.durable_generation.iter())
            .filter_map(|value| value.options.model_dir.as_ref())
            .any(|path| relative_path_inside(path, root).is_some())
    }

    pub fn save_generation(&mut self, mut value: GenerationPreferences) -> Result<()> {
        value.options.out = None;
        value.options.resume = Some(true);
        let previous = self.generation.clone();
        self.save_with_intent(
            PreferenceGroup::Generation,
            "generation-intent.json",
            value,
            |store| &mut store.generation,
            |store| &mut store.generation_intent,
        )?;
        // The new value is active at once, but until the writer confirms it the
        // file still holds the previous one, which may still name a library.
        self.durable_generation.get_or_insert(previous);
        Ok(())
    }

    pub fn save_application(&mut self, mut value: ApplicationPreferences) -> Result<()> {
        value.desktop.system_titlebar = true;
        self.save_with_intent(
            PreferenceGroup::Application,
            "application-intent.json",
            value,
            |store| &mut store.application,
            |store| &mut store.application_intent,
        )
    }

    /// Persist one preference group. The value stays a recovery intent until the
    /// writer confirms the group file, so an interrupted save can be retried after
    /// relaunch. The closures split the borrow between the committed slot and its
    /// intent slot.
    fn save_with_intent<T>(
        &mut self,
        group: PreferenceGroup,
        file: &str,
        value: T,
        slot: impl for<'a> FnOnce(&'a mut Self) -> &'a mut T,
        intent_slot: impl for<'a> FnOnce(&'a mut Self) -> &'a mut Option<T>,
    ) -> Result<()>
    where
        T: Serialize + DeserializeOwned + Validate + Clone + 'static,
    {
        let revision = self.revisions.get(&group).copied().unwrap_or(0) + 1;
        let bytes = serde_json::to_vec_pretty(&Envelope {
            schema: SCHEMA,
            revision,
            value: &value,
        })?;
        self.writer.send(PreferenceSignal::Recovery {
            path: self.recovery_root().join(file),
            bytes,
        })?;
        if let Err(error) = self.persist(group, &value) {
            *intent_slot(self) = Some(value);
            return Err(error);
        }
        *slot(self) = value.clone();
        *intent_slot(self) = Some(value);
        Ok(())
    }

    /// Queues a recovery copy of `draft`; returns whether the writer accepted it.
    fn queue_service_recovery(&mut self, draft: &ServiceDraft) -> bool {
        let Ok(bytes) = serde_json::to_vec_pretty(draft) else {
            return false;
        };
        let path = self.recovery_root().join(format!("{}.json", draft.id));
        let queued = self
            .writer
            .send(PreferenceSignal::Recovery { path, bytes })
            .is_ok();
        self.pending_service_recovery.insert(draft.id.clone());
        self.recovery_drafts.insert(draft.id.clone());
        queued
    }

    fn check_draft_edit(&self, draft: &ServiceDraft) -> Result<()> {
        // Incomplete drafts are durable, but URL-embedded credentials must never be written
        // to a normal settings file, including when the URL itself is not yet parseable.
        if draft.address.contains('@') || draft.address.contains('?') || draft.address.contains('#')
        {
            bail!(url_credentials());
        }
        if let Some(current) = self.services.drafts.get(&draft.id) {
            if current.revision != draft.revision {
                bail!(course2md::tr!("服务编辑已发生变化，请重新打开服务后再试"));
            }
        } else if draft.revision != 0 {
            bail!(course2md::tr!("当前服务编辑已结束，请重新打开服务后再试"));
        }
        Ok(())
    }

    /// Incomplete text is a valid draft. Replacing a key allocates a new vault identity before
    /// the secret-free draft is committed. A stale editor cannot overwrite a newer revision.
    pub fn save_service_draft(
        &mut self,
        mut draft: ServiceDraft,
        new_key: Option<Secret>,
    ) -> Result<ServiceDraft> {
        self.check_draft_edit(&draft)?;
        let created_credential = match new_key {
            Some(secret) => {
                let reference = self.vault.insert(secret)?;
                draft.credential = Some(reference.clone());
                draft.credential_source = None;
                Some(reference)
            }
            None => None,
        };
        draft.revision += 1;
        let mut next = self.services.clone();
        next.drafts.insert(draft.id.clone(), draft.clone());
        let recovered = self.queue_service_recovery(&draft);
        if let Err(error) = self.persist(PreferenceGroup::Services, &next) {
            // The queued recovery copy keeps the edit while the group cannot be saved.
            if recovered {
                self.services.drafts.insert(draft.id.clone(), draft.clone());
                return Ok(draft);
            }
            if let Some(reference) = created_credential {
                let _ = self.vault.remove(&reference);
            }
            return Err(error);
        }
        self.services = next;
        Ok(draft)
    }

    pub fn discard_service_draft(&mut self, id: &str) -> Result<()> {
        if !self.services.drafts.contains_key(id) && !self.recovery_drafts.contains(id) {
            return Ok(());
        }
        let mut next = self.services.clone();
        next.drafts.remove(id);
        next.discarded_drafts.insert(id.to_owned());
        self.persist(PreferenceGroup::Services, &next)?;
        self.services = next;
        // The recovery copy is removed once the writer confirms the group.
        self.pending_service_recovery.insert(id.to_owned());
        Ok(())
    }

    /// Static checks of a publication that need no vault.
    fn publication(&self, draft_id: &str) -> Result<(ServiceDraft, ServiceConfiguration)> {
        let draft =
            self.services.drafts.get(draft_id).cloned().ok_or_else(|| {
                anyhow!(course2md::tr!("当前服务编辑已结束，请重新打开服务后再试"))
            })?;
        if self.is_service_retired(&draft.service_id) {
            bail!(course2md::tr!(
                "此服务已删除。请添加新的服务，原任务不会自动恢复外发"
            ));
        }
        let config = draft.configuration()?;
        if self.services.versions.values().any(|version| {
            version.service_id == draft.service_id
                && version.config.protocol.purpose() != config.protocol.purpose()
        }) {
            bail!(course2md::tr!(
                "修改接口不能改变此服务的用途，请添加另一用途的服务"
            ));
        }
        Ok((draft, config))
    }

    pub fn publish_service(
        &mut self,
        draft_id: &str,
        scope: BindingScope,
    ) -> Result<ServiceVersion> {
        let (draft, config) = self.publication(draft_id)?;
        if let Some(reference) = &config.credential {
            // Local credential availability is part of static validation, not a paid test.
            self.vault.resolve(reference)?;
        }
        let version = ServiceVersion {
            id: new_id("service-version"),
            service_id: draft.service_id.clone(),
            number: self
                .services
                .versions
                .values()
                .filter(|version| version.service_id == draft.service_id)
                .map(|version| version.number)
                .max()
                .unwrap_or(0)
                + 1,
            saved_at: now_seconds(),
            published_from: Some(draft.id.clone()),
            config,
        };
        let mut next = self.services.clone();
        next.versions.insert(version.id.clone(), version.clone());
        next.drafts.remove(draft_id);
        if scope == BindingScope::Defaults {
            match version.config.protocol.purpose() {
                ServicePurpose::Speech => next.defaults.asr = Some(version.id.clone()),
                ServicePurpose::Ai => next.defaults.llm = Some(version.id.clone()),
            }
        }
        self.queue_service_recovery(&draft);
        self.persist(PreferenceGroup::Services, &next)?;
        self.services = next;
        Ok(version)
    }

    pub fn set_default_service(
        &mut self,
        purpose: ServicePurpose,
        reference: Option<&str>,
    ) -> Result<()> {
        if let Some(reference) = reference {
            let version = self.check_dispatch(reference)?;
            if version.config.protocol.purpose() != purpose {
                bail!(course2md::tr!("此服务不支持所选用途"));
            }
        }
        let mut next = self.services.clone();
        match purpose {
            ServicePurpose::Speech => next.defaults.asr = reference.map(str::to_owned),
            ServicePurpose::Ai => next.defaults.llm = reference.map(str::to_owned),
        }
        self.persist(PreferenceGroup::Services, &next)?;
        self.services = next;
        Ok(())
    }

    /// Delete the service configuration. Already dispatched requests are still
    /// received and saved. A marker keeps later restores from sending new ones.
    pub fn delete_service(&mut self, service_id: &str) -> Result<()> {
        let versions: Vec<_> = self
            .services
            .versions
            .values()
            .filter(|version| version.service_id == service_id)
            .cloned()
            .collect();
        if versions.is_empty() {
            bail!(course2md::tr!("找不到此服务"));
        }
        let credentials: Vec<_> = versions
            .iter()
            .filter_map(|version| version.config.credential.clone())
            .collect();
        self.write_retire_marker(service_id)?;
        let mut next = self.services.clone();
        next.stopped
            .entry(service_id.to_owned())
            .or_insert_with(now_seconds);
        next.versions
            .retain(|_, version| version.service_id != service_id);
        next.drafts
            .retain(|_, draft| draft.service_id != service_id);
        if next
            .defaults
            .asr
            .as_ref()
            .is_some_and(|id| !next.versions.contains_key(id))
        {
            next.defaults.asr = None;
        }
        if next
            .defaults
            .llm
            .as_ref()
            .is_some_and(|id| !next.versions.contains_key(id))
        {
            next.defaults.llm = None;
        }
        self.persist(PreferenceGroup::Services, &next)?;
        self.services = next;
        for reference in credentials {
            let _ = self.vault.remove(&reference);
        }
        Ok(())
    }

    pub fn check_dispatch(&self, reference: &str) -> Result<&ServiceVersion> {
        if self.is_blocked(PreferenceGroup::Services) {
            bail!(course2md::tr!("服务记录暂时不可用，已保留原文件"));
        }
        let version = self.version(reference).ok_or_else(|| {
            anyhow!(course2md::tr!(
                "找不到任务使用的服务版本，请选择服务后建立新尝试"
            ))
        })?;
        // Single stat of the marker: the match keeps the two error classes
        // ("retired" vs "cannot confirm"), and the in-memory stopped map is
        // checked above, so is_service_retired's second stat is not repeated.
        if self.services.stopped.contains_key(&version.service_id) {
            bail!(course2md::tr!("此服务已删除，尚未发送新的请求"));
        }
        match std::fs::metadata(self.retired_marker(&version.service_id)) {
            Ok(_) => bail!(course2md::tr!("此服务已删除，尚未发送新的请求")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!(course2md::tr!(
                "暂时无法确认此服务是否已删除，尚未发送新的请求"
            )),
        }
        Ok(version)
    }

    /// Return only non-secret configuration; unused services do not participate in validation.
    pub fn config_for_refs(
        &self,
        base: &ConfigFile,
        references: &ServiceRefs,
    ) -> Result<ConfigFile> {
        self.config_for_refs_with_dispatch_check(base, references, true)
    }

    /// Non-secret configuration for rendering a plan from the loaded settings.
    /// This does not authorize dispatch or read external stop markers.
    pub fn config_for_preview(
        &self,
        base: &ConfigFile,
        references: &ServiceRefs,
    ) -> Result<ConfigFile> {
        self.config_for_refs_with_dispatch_check(base, references, false)
    }

    fn config_for_refs_with_dispatch_check(
        &self,
        base: &ConfigFile,
        references: &ServiceRefs,
        verify_dispatch: bool,
    ) -> Result<ConfigFile> {
        let check = |id: &str| -> Result<&ServiceVersion> {
            if verify_dispatch {
                return self.check_dispatch(id);
            }
            ensure!(
                !self.is_blocked(PreferenceGroup::Services),
                course2md::tr!("服务记录暂时不可用，已保留原文件")
            );
            let version = self.version(id).context(course2md::tr!(
                "找不到任务使用的服务版本，请选择服务后建立新尝试"
            ))?;
            ensure!(
                !self.services.stopped.contains_key(&version.service_id),
                course2md::tr!("此服务已删除，尚未发送新的请求")
            );
            Ok(version)
        };
        let mut config = base.clone();
        clear_service_fields(&mut config);
        let required = references.required_for(base);
        if base.defaults.provider == Some(AsrProvider::Api)
            && base.defaults.transcript_source != Some(TranscriptSource::Subtitle)
        {
            let id = required
                .asr
                .as_deref()
                .ok_or_else(|| anyhow!(course2md::tr!("请为这次笔记设置语音识别服务")))?;
            let version = check(id)?;
            if version.config.protocol.purpose() != ServicePurpose::Speech {
                bail!(course2md::tr!("所选服务不支持语音识别"));
            }
            config.asr_api.base_url = version.config.endpoint.clone();
            config.asr_api.model = version.config.model.clone();
            config.asr_api.mode = match version.config.protocol {
                ServiceProtocol::SpeechTranscriptions => AsrApiMode::Transcriptions,
                _ => AsrApiMode::Chat,
            };
        }
        if base.llm.uses_service() {
            let id = required
                .llm
                .as_deref()
                .ok_or_else(|| anyhow!(course2md::tr!("请为这次笔记设置 AI 服务")))?;
            let version = check(id)?;
            if version.config.protocol.purpose() != ServicePurpose::Ai {
                bail!(course2md::tr!("所选服务不支持 AI 校对或摘要"));
            }
            config.llm.provider = version.config.protocol.llm_provider();
            // Codex 方言端点固定（provider::endpoint 忽略 base_url）；按 CLI 约定留空
            config.llm.base_url = if version.config.protocol == ServiceProtocol::CodexResponses {
                String::new()
            } else {
                version.config.endpoint.clone()
            };
            config.llm.model = version.config.model.clone();
        }
        Ok(config)
    }

    /// Credential references a request build needs. Resolving them can block on the system
    /// keychain (its authorization dialog queue may even be held by another application), so
    /// UI callers must resolve these off the main thread and pass the results to
    /// `resolve_for_execution_with_secrets`.
    pub fn credential_references_for(
        &self,
        base: &ConfigFile,
        references: &ServiceRefs,
    ) -> Result<Vec<CredentialRef>> {
        let required = references.required_for(base);
        let mut needed = Vec::new();
        for id in [required.asr, required.llm].into_iter().flatten() {
            let version = self.check_dispatch(&id)?;
            if let Some(reference) = &version.config.credential {
                needed.push(reference.clone());
            }
        }
        Ok(needed)
    }

    pub fn resolve_for_execution_with_secrets(
        &self,
        base: &ConfigFile,
        references: &ServiceRefs,
        secrets: &HashMap<String, String>,
    ) -> Result<ResolvedConfig> {
        let mut config = ResolvedConfig(self.config_for_refs(base, references)?);
        let required = references.required_for(base);
        for (id, is_asr) in [(required.asr, true), (required.llm, false)] {
            if let Some(id) = id {
                // config_for_refs above already ran check_dispatch for exactly
                // these required ids; same synchronous call chain, so reuse the
                // validated lookup instead of statting the marker a second time.
                let version = self.version(&id).context(course2md::tr!(
                    "找不到任务使用的服务版本，请选择服务后建立新尝试"
                ))?;
                if let Some(reference) = &version.config.credential {
                    let secret = secrets.get(reference).cloned().context(course2md::tr!(
                        "服务凭据尚未就绪，请重新打开服务设置保存一次"
                    ))?;
                    if is_asr {
                        config.0.asr_api.api_key = secret;
                    } else {
                        config.0.llm.api_key = secret;
                    }
                }
            }
        }
        Ok(config)
    }

    pub fn record_test(&mut self, evidence: ServiceTestEvidence) -> Result<()> {
        let mut next = self.services.clone();
        next.tests.insert(evidence.fingerprint.clone(), evidence);
        self.persist(PreferenceGroup::Services, &next)?;
        self.services = next;
        Ok(())
    }

    /// Only the explicitly affected group is reset. Originals and the last backup are copied
    /// before publication, and failure leaves the old files and effective state untouched.
    pub fn reset_group(&mut self, group: PreferenceGroup) -> Result<()> {
        let path = self.root.join(group.filename());
        preserve_original(&self.root, &path)?;
        preserve_original(&self.root, &path.with_extension("json.bak"))?;
        self.blocked.remove(&group);
        let result = match group {
            PreferenceGroup::Generation => self.save_generation(GenerationPreferences::default()),
            PreferenceGroup::Application => {
                self.save_application(ApplicationPreferences::default())
            }
            PreferenceGroup::Services => {
                let value = ServicesState::default();
                self.persist(group, &value).map(|_| {
                    self.services = value;
                })
            }
        };
        if result.is_err() {
            self.blocked.insert(group);
        }
        result
    }

    fn load_group<T: Default + DeserializeOwned + Serialize + Validate>(
        &mut self,
        group: PreferenceGroup,
    ) -> T {
        let path = self.root.join(group.filename());
        match read_envelope::<T>(&path) {
            Ok(Some(envelope)) => {
                self.revisions.insert(group, envelope.revision);
                envelope.value
            }
            Ok(None) => T::default(),
            Err(_) => match read_envelope::<T>(&path.with_extension("json.bak")) {
                Ok(Some(envelope)) => {
                    let restored = preserve_original(&self.root, &path).and_then(|_| {
                        let bytes = serde_json::to_vec_pretty(&envelope)?;
                        course2md::checkpoint::atomic_write(&path, &bytes)
                    });
                    self.revisions.insert(group, envelope.revision);
                    self.issues.push(SettingsIssue {
                        group,
                        kind: if restored.is_ok() {
                            SettingsIssueKind::Recovered
                        } else {
                            SettingsIssueKind::SaveFailed
                        },
                        message: if restored.is_ok() {
                            course2md::tr!("已恢复最近一次可用的设置，原文件已保留").into()
                        } else {
                            course2md::tr!(
                                "正在使用最近一次可用的设置，但恢复文件尚未保存；原文件已保留"
                            )
                            .into()
                        },
                        detail: restored.err().map(|error| {
                            course2md::tr!(
                                "设置目录：{}\n{}",
                                self.root.display(),
                                format!("{error:#}")
                            )
                        }),
                    });
                    envelope.value
                }
                _ => {
                    self.blocked.insert(group);
                    self.issues.push(SettingsIssue {
                        group,
                        kind: SettingsIssueKind::NeedsReset,
                        message: course2md::tr!(
                            "此组设置暂时无法读取，原文件已保留。可以保留原文件并重置此组"
                        )
                        .into(),
                        detail: Some(course2md::tr!("设置目录：{}", self.root.display())),
                    });
                    T::default()
                }
            },
        }
    }

    fn recovery_root(&self) -> PathBuf {
        self.root.with_extension("recovery")
    }

    /// Restore one recovery intent saved by `save_with_intent`, when its
    /// revision is not older than the committed value.
    fn load_recovery_intent<T>(
        &mut self,
        group: PreferenceGroup,
        file: &str,
        message: &'static str,
        intent_slot: impl for<'a> FnOnce(&'a mut Self) -> &'a mut Option<T>,
    ) where
        T: Serialize + DeserializeOwned + Validate,
    {
        if let Ok(Some(intent)) = read_envelope::<T>(&self.recovery_root().join(file))
            && self.revisions.get(&group).copied().unwrap_or(0) <= intent.revision
        {
            *intent_slot(self) = Some(intent.value);
            self.issues.push(SettingsIssue {
                group,
                kind: SettingsIssueKind::SaveFailed,
                message: message.into(),
                detail: None,
            });
        }
    }

    fn load_recovery_intents(&mut self) {
        self.load_recovery_intent::<GenerationPreferences>(
            PreferenceGroup::Generation,
            "generation-intent.json",
            course2md::tr!("已找回上次未生效的设置变更，可以重试保存；当前仍使用原来的有效选项"),
            |store| &mut store.generation_intent,
        );
        self.load_recovery_intent::<ApplicationPreferences>(
            PreferenceGroup::Application,
            "application-intent.json",
            course2md::tr!("已找回上次未生效的应用设置，可以重试保存；当前仍使用原来的有效选项"),
            |store| &mut store.application_intent,
        );
        let Ok(entries) = std::fs::read_dir(self.recovery_root()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("service-draft-") || !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(draft) = serde_json::from_slice::<ServiceDraft>(&bytes) else {
                continue;
            };
            if !valid_id(&draft.id, "service-draft-") || !valid_id(&draft.service_id, "service-") {
                continue;
            }
            if self
                .services
                .versions
                .values()
                .any(|version| version.published_from.as_deref() == Some(&draft.id))
                || self.services.discarded_drafts.contains(&draft.id)
            {
                continue;
            }
            if self
                .services
                .drafts
                .get(&draft.id)
                .is_none_or(|current| current.revision < draft.revision)
            {
                self.recovery_drafts.insert(draft.id.clone());
                self.services.drafts.insert(draft.id.clone(), draft);
            }
        }
    }

    fn persist<T: Serialize + DeserializeOwned + Validate + 'static>(
        &mut self,
        group: PreferenceGroup,
        value: &T,
    ) -> Result<()> {
        let result = (|| {
            if self.blocked.contains(&group) {
                bail!(course2md::tr!("此组设置需要先保留原文件并重置"));
            }
            value.validate_value()?;
            let path = self.root.join(group.filename());
            let revision = self.revisions.get(&group).copied().unwrap_or(0) + 1;
            let bytes = serde_json::to_vec_pretty(&Envelope {
                schema: SCHEMA,
                revision,
                value,
            })?;
            if let Some(services) = (value as &dyn std::any::Any).downcast_ref::<ServicesState>() {
                self.services_intent = Some(services.clone());
            }
            self.writer.send(PreferenceSignal::Write {
                path,
                bytes,
                group,
                revision,
            })?;
            self.revisions.insert(group, revision);
            self.pending_writes.insert(group, revision);
            Ok(())
        })();
        self.issues
            .retain(|issue| issue.group != group || issue.kind == SettingsIssueKind::Recovered);
        if let Err(error) = &result {
            self.issues.push(SettingsIssue {
                group,
                kind: SettingsIssueKind::SaveFailed,
                message: save_failure_message(group, error),
                detail: Some(course2md::tr!(
                    "设置目录：{}\n{}",
                    self.root.display(),
                    format!("{error:#}")
                )),
            });
        }
        result
    }
}

fn read_envelope<T: DeserializeOwned + Validate>(path: &Path) -> Result<Option<Envelope<T>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let envelope: Envelope<T> =
        serde_json::from_slice(&bytes).context(course2md::tr!("设置记录无法解析"))?;
    if envelope.schema != SCHEMA {
        bail!(course2md::tr!("此组设置使用尚不支持的版本"));
    }
    envelope.value.validate_value()?;
    Ok(Some(envelope))
}

/// Retries a transiently failing local write. It sleeps between attempts, so
/// only the preferences writer thread may call it; the UI thread writes once.
fn atomic_write_with_retry(path: &Path, bytes: &[u8]) -> Result<()> {
    // Only idempotent local persistence is retried. Network tests never use this helper.
    let mut last_error = None;
    for _ in 0..3 {
        match course2md::checkpoint::atomic_write(path, bytes) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    Err(last_error.expect("write was attempted"))
}

fn preserve_original(root: &Path, source: &Path) -> Result<()> {
    let bytes = match std::fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings");
    // Also runs on the UI thread (open, reset), so it writes once without sleeping.
    course2md::checkpoint::atomic_write(
        &root
            .with_extension("recovery")
            .join(format!("{}-{name}", new_id("original"))),
        &bytes,
    )
}

pub fn strip_secrets(config: &mut ConfigFile) {
    use zeroize::Zeroize;
    config.asr_api.api_key.zeroize();
    config.llm.api_key.zeroize();
}

fn clear_service_fields(config: &mut ConfigFile) {
    strip_secrets(config);
    config.asr_api.base_url.clear();
    config.asr_api.model.clear();
    // 服务绑定未生效时不残留其他服务的方言选择
    config.llm.provider = course2md::llm::LlmProvider::default();
    config.llm.base_url.clear();
    config.llm.model.clear();
}

/// Accept the chosen protocol's full endpoint or a base URL. A known conflicting full
/// endpoint is an error, never silently changed to another protocol. URL credentials,
/// query-string tokens and fragments are rejected before they can enter normal files/logs.
pub fn normalize_endpoint(address: &str, protocol: ServiceProtocol) -> Result<String> {
    let mut url = url::Url::parse(address.trim())
        .map_err(|_| anyhow!(course2md::tr!("请输入包含 http:// 或 https:// 的服务地址")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!(course2md::tr!("请输入包含 http:// 或 https:// 的服务地址"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(url_credentials());
    }
    let path = url.path().trim_end_matches('/');
    let desired = protocol.endpoint_suffix();
    if course2md::config::conflicting_endpoint_suffix(
        path,
        desired,
        &[
            "/audio/transcriptions",
            "/chat/completions",
            "/responses",
            "/completions",
        ],
    ) {
        bail!(course2md::tr!(
            "这个地址和所选服务类型不匹配，请更换其中之一"
        ));
    }
    // Ollama 接受主机地址或 /v1 基础地址，统一补全到 OpenAI 兼容端点
    let base = if protocol == ServiceProtocol::OllamaChat
        && !path.ends_with(desired)
        && !path.ends_with("/v1")
    {
        format!("{path}/v1")
    } else {
        path.to_owned()
    };
    url.set_path(&course2md::config::append_endpoint_suffix(&base, desired));
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_for_execution(
        store: &Store,
        base: &ConfigFile,
        references: &ServiceRefs,
    ) -> Result<ResolvedConfig> {
        let mut secrets = HashMap::new();
        for reference in store.credential_references_for(base, references)? {
            secrets.insert(
                reference.clone(),
                store.vault.resolve(&reference)?.expose().to_owned(),
            );
        }
        store.resolve_for_execution_with_secrets(base, references, &secrets)
    }
    use crate::credentials::MemoryCredentialVault;

    /// Waits for the writer and applies its results, as the UI's poll does.
    fn settle(store: &mut Store) -> Vec<(PreferenceGroup, Result<(), String>)> {
        store.flush();
        store.process_writes()
    }
    fn failed(results: &[(PreferenceGroup, Result<(), String>)], group: PreferenceGroup) -> bool {
        results
            .iter()
            .any(|(g, result)| *g == group && result.is_err())
    }

    /// The production store with a writer that finishes each write before
    /// returning; results still arrive through `process_writes`.
    fn open(root: impl Into<PathBuf>, vault: Arc<dyn CredentialVault>) -> Store {
        let root = root.into();
        Store::open_with(root.clone(), vault, InlineWriter::new(root))
    }

    fn isolated() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path(), Arc::new(MemoryCredentialVault::new()));
        (directory, store)
    }

    fn complete_draft(store: &mut Store, model: &str) -> ServiceDraft {
        let mut draft = ServiceDraft::new(ServicePurpose::Ai);
        draft.address = "https://example.test/v1".into();
        draft.model = model.into();
        store
            .save_service_draft(draft, Some(Secret::new("test-only-secret")))
            .unwrap()
    }

    #[test]
    fn local_engine_choice_survives_online_service_and_restart() {
        for provider in [
            None,
            Some(AsrProvider::Cpu),
            Some(AsrProvider::Gpu),
            Some(AsrProvider::Coreml),
        ] {
            let (directory, mut store) = isolated();
            let mut preferences = store.generation().clone();
            // Existing preference files have only the active provider. The
            // first switch must capture it even without an earlier memory value.
            preferences.options.provider = provider;
            preferences.options.asr_model = Some("qwen3-1.7b".into());
            preferences.select_recognition_location(true);
            store.save_generation(preferences).unwrap();

            let reopened = open(directory.path(), store.vault());
            let mut restored = reopened.generation().clone();
            assert_eq!(restored.options.provider, Some(AsrProvider::Api));
            restored.select_recognition_location(false);
            assert_eq!(restored.options.provider, provider);
            assert_eq!(restored.options.asr_model.as_deref(), Some("qwen3-1.7b"));
        }
    }

    #[test]
    fn explicit_test_refusal_still_allows_save_only_without_changing_defaults() {
        let (_directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "fixture-model");
        let config = draft.configuration().unwrap();
        let contract = crate::service_test::TestKind::Proofread.contract();
        let evidence = ServiceTestEvidence {
            fingerprint: config.fingerprint(contract),
            contract: contract.into(),
            tested_at: 1,
            outcome: TestOutcome::AuthenticationRefused,
            message: "服务拒绝凭据".into(),
            details: vec!["HTTP 401".into()],
            http_status: Some(401),
        };
        store.record_test(evidence).unwrap();
        let version = store
            .publish_service(&draft.id, BindingScope::CurrentTask)
            .unwrap();
        assert_eq!(version.config, config);
        assert!(store.default_refs().llm.is_none());
        assert!(store.default_refs().asr.is_none());
        assert_eq!(
            store
                .test_evidence(&version.config, contract)
                .unwrap()
                .outcome,
            TestOutcome::AuthenticationRefused
        );
    }

    #[test]
    fn first_launch_remains_unfinished_until_setup_is_explicitly_completed() {
        let (directory, mut store) = isolated();
        assert!(!store.application().desktop.setup_completed);
        let mut preferences = store.application().clone();
        preferences.font_scale = 1.25;
        preferences.desktop.reduce_motion = true;
        store.save_application(preferences).unwrap();
        let mut reopened = open(directory.path(), store.vault());
        assert!(!reopened.application().desktop.setup_completed);
        assert_eq!(reopened.application().font_scale, 1.25);
        let mut preferences = reopened.application().clone();
        preferences.desktop.setup_completed = true;
        reopened.save_application(preferences).unwrap();
        let reopened = open(directory.path(), store.vault());
        assert!(reopened.application().desktop.setup_completed);
        assert_eq!(reopened.application().font_scale, 1.25);
        assert!(reopened.application().desktop.reduce_motion);
    }

    #[test]
    fn incomplete_service_draft_does_not_block_other_groups_or_replace_effective_service() {
        let (_directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "old-model");
        let old = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        let mut draft = ServiceDraft::from_version(&old);
        draft.address.clear();
        store.save_service_draft(draft, None).unwrap();
        let mut generation = store.generation().clone();
        generation.ai_summary = true;
        store.save_generation(generation).unwrap();
        assert!(store.generation().ai_summary);
        assert_eq!(store.default_refs().llm.as_deref(), Some(old.id.as_str()));
        assert_eq!(store.service_drafts().count(), 1);
    }

    #[test]
    fn task_only_publication_and_key_rotation_do_not_change_default_or_old_snapshot() {
        let (_directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "old-model");
        let old = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        let mut edit = ServiceDraft::from_version(&old);
        edit.model = "new-model".into();
        let edit = store
            .save_service_draft(edit, Some(Secret::new("replacement-test-key")))
            .unwrap();
        let new = store
            .publish_service(&edit.id, BindingScope::CurrentTask)
            .unwrap();
        assert_eq!(store.default_refs().llm, Some(old.id.clone()));
        assert_ne!(old.config.credential, new.config.credential);
        let mut config = ConfigFile::default();
        config.llm.enabled = true;
        let old_refs = ServiceRefs {
            llm: Some(old.id),
            asr: None,
        };
        let runtime = resolve_for_execution(&store, &config, &old_refs)
            .unwrap()
            .into_config();
        assert_eq!(runtime.llm.model, "old-model");
        assert_eq!(runtime.llm.api_key, "test-only-secret");
        let saved = store.config_for_refs(&config, &old_refs).unwrap();
        assert!(saved.llm.api_key.is_empty());
    }

    #[test]
    fn cancelling_a_service_edit_preserves_active_and_submitted_configuration() {
        let (directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "active-model");
        let active = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        let submitted_refs = store.default_refs();
        let mut edit = ServiceDraft::from_version(&active);
        edit.model = "cancelled-model".into();
        // An explicit test may stage credentials without publishing the edit.
        let staged = store
            .save_service_draft(edit, Some(Secret::new("cancelled-test-key")))
            .unwrap();
        store.discard_service_draft(&staged.id).unwrap();
        assert!(store.draft(&staged.id).is_none());
        assert!(
            store
                .publish_service(&staged.id, BindingScope::Defaults)
                .is_err()
        );
        assert!(store.save_service_draft(staged, None).is_err());

        let reopened = open(directory.path(), store.vault());
        assert_eq!(reopened.default_refs().llm, Some(active.id.clone()));
        assert_eq!(reopened.versions().count(), 1);
        let mut config = ConfigFile::default();
        config.llm.enabled = true;
        let runtime = resolve_for_execution(&reopened, &config, &submitted_refs)
            .unwrap()
            .into_config();
        assert_eq!(runtime.llm.model, "active-model");
        assert_eq!(runtime.llm.api_key, "test-only-secret");
    }

    #[test]
    fn cancelling_an_unstaged_service_edit_does_not_write_settings() {
        let (directory, mut store) = isolated();
        let edit = ServiceDraft::new(ServicePurpose::Ai);
        store.discard_service_draft(&edit.id).unwrap();
        assert!(!directory.path().join("services.json").exists());
        assert!(store.services.discarded_drafts.is_empty());
    }

    #[test]
    fn group_recovery_preserves_corrupt_file_and_keeps_other_group_current() {
        let (directory, mut store) = isolated();
        let mut first = store.generation().clone();
        first.ai_summary = true;
        store.save_generation(first).unwrap();
        let mut second = store.generation().clone();
        second.ai_proofread = true;
        store.save_generation(second).unwrap();
        let mut app = store.application().clone();
        app.desktop.reduce_motion = true;
        store.save_application(app).unwrap();
        std::fs::write(directory.path().join("generation.json"), b"broken settings").unwrap();
        let recovered = open(directory.path(), store.vault());
        assert!(recovered.generation().ai_summary);
        assert!(!recovered.generation().ai_proofread);
        assert!(recovered.application().desktop.reduce_motion);
        assert!(
            recovered
                .issues()
                .iter()
                .any(|issue| issue.kind == SettingsIssueKind::Recovered)
        );
        let preserved = std::fs::read_dir(directory.path().with_extension("recovery"))
            .unwrap()
            .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
            .collect::<Vec<_>>();
        assert!(preserved.iter().any(|bytes| bytes == b"broken settings"));
    }

    #[test]
    fn failed_group_commit_does_not_publish_intent_or_change_other_groups() {
        let (directory, mut store) = isolated();
        store.save_generation(store.generation().clone()).unwrap();
        // A directory in place of the target makes writes fail even when tests run as root.
        std::fs::remove_file(directory.path().join("generation.json")).unwrap();
        std::fs::create_dir(directory.path().join("generation.json")).unwrap();
        let mut next = store.generation().clone();
        next.ai_summary = true;
        store.save_generation(next).unwrap();
        assert!(failed(&settle(&mut store), PreferenceGroup::Generation));
        assert!(
            store
                .generation_intent()
                .is_some_and(|intent| intent.ai_summary)
        );
        store.save_application(store.application().clone()).unwrap();
        assert!(!failed(&settle(&mut store), PreferenceGroup::Application));
        assert!(
            store
                .issues()
                .iter()
                .any(|issue| issue.group == PreferenceGroup::Generation)
        );
        // The unwritten edit is never what a relaunch applies.
        let reopened = open(directory.path(), store.vault());
        assert!(!reopened.generation().ai_summary);
    }

    #[test]
    fn broken_unbacked_group_requires_explicit_reset_and_keeps_original() {
        let (directory, store) = isolated();
        std::fs::write(
            directory.path().join("generation.json"),
            b"original broken content",
        )
        .unwrap();
        let mut reopened = open(directory.path(), store.vault());
        assert!(reopened.is_blocked(PreferenceGroup::Generation));
        assert!(
            reopened
                .save_generation(GenerationPreferences::default())
                .is_err()
        );
        reopened.reset_group(PreferenceGroup::Generation).unwrap();
        assert!(!reopened.is_blocked(PreferenceGroup::Generation));
        assert!(
            std::fs::read_dir(directory.path().with_extension("recovery"))
                .unwrap()
                .count()
                >= 1
        );
    }

    #[test]
    fn secrets_never_enter_saved_groups_or_nonsecret_configuration() {
        let (directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "model");
        store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        for entry in std::fs::read_dir(directory.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("test-only-secret"));
            // "api_key" is a legitimate authentication-mode value, never a secret field.
            assert!(!text.contains("\"api_key\":"));
        }
    }

    #[test]
    fn deleting_a_service_blocks_dispatch_and_clears_defaults() {
        let (directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "model");
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        store.delete_service(&version.service_id).unwrap();
        let reopened = open(directory.path(), store.vault());
        assert!(reopened.version(&version.id).is_none());
        assert!(reopened.default_refs().llm.is_none());
        assert!(reopened.check_dispatch(&version.id).is_err());
        assert!(
            reopened
                .config_for_refs(&ConfigFile::default(), &reopened.default_refs())
                .is_ok()
        );
        let mut config = ConfigFile::default();
        config.llm.summarize = true;
        assert!(
            reopened
                .config_for_refs(
                    &config,
                    &ServiceRefs {
                        llm: Some(version.id.clone()),
                        ..ServiceRefs::default()
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn endpoint_normalization_refuses_protocol_conflicts_and_embedded_credentials() {
        assert_eq!(
            normalize_endpoint("https://example.test/v1/", ServiceProtocol::AiChat).unwrap(),
            "https://example.test/v1/chat/completions"
        );
        assert_eq!(
            normalize_endpoint(
                "http://127.0.0.1:8080/v1/audio/transcriptions",
                ServiceProtocol::SpeechTranscriptions
            )
            .unwrap(),
            "http://127.0.0.1:8080/v1/audio/transcriptions"
        );
        assert!(
            normalize_endpoint(
                "https://example.test/v1/chat/completions",
                ServiceProtocol::SpeechTranscriptions
            )
            .is_err()
        );
        assert!(
            normalize_endpoint(
                "https://user:secret@example.test/v1",
                ServiceProtocol::AiChat
            )
            .is_err()
        );
        assert!(
            normalize_endpoint(
                "https://example.test/v1?api_key=secret",
                ServiceProtocol::AiChat
            )
            .is_err()
        );
    }

    #[test]
    fn ollama_and_codex_endpoints_normalize_to_their_dialects() {
        // Ollama：主机地址、/v1 基础地址与完整接口地址都归一到 OpenAI 兼容端点
        for address in [
            "http://localhost:11434",
            "http://localhost:11434/",
            "http://localhost:11434/v1",
            "http://localhost:11434/v1/chat/completions",
        ] {
            assert_eq!(
                normalize_endpoint(address, ServiceProtocol::OllamaChat).unwrap(),
                "http://localhost:11434/v1/chat/completions",
                "address {address}"
            );
        }
        assert!(
            normalize_endpoint(
                "http://localhost:11434/v1/responses",
                ServiceProtocol::OllamaChat
            )
            .is_err()
        );
        // Codex：固定地址归一到 Responses 端点，幂等
        let endpoint = normalize_endpoint(CODEX_ADDRESS, ServiceProtocol::CodexResponses).unwrap();
        assert_eq!(endpoint, course2md::provider::CODEX_RESPONSES_URL);
        assert_eq!(
            normalize_endpoint(&endpoint, ServiceProtocol::CodexResponses).unwrap(),
            endpoint
        );
        assert!(
            normalize_endpoint("https://example.test/v1/responses", ServiceProtocol::AiChat)
                .is_err()
        );
        assert!(
            normalize_endpoint(
                "https://example.test/v1/chat/completions",
                ServiceProtocol::CodexResponses
            )
            .is_err()
        );
    }

    #[test]
    fn keyless_services_publish_and_map_to_cli_providers() {
        for (protocol, address, provider) in [
            (
                ServiceProtocol::OllamaChat,
                "http://localhost:11434",
                course2md::llm::LlmProvider::Ollama,
            ),
            (
                ServiceProtocol::CodexResponses,
                CODEX_ADDRESS,
                course2md::llm::LlmProvider::Codex,
            ),
        ] {
            let (directory, mut store) = isolated();
            let mut draft = ServiceDraft::new(ServicePurpose::Ai);
            draft.protocol = protocol;
            draft.authentication = Authentication::None;
            draft.address = address.into();
            draft.model = "fixture-model".into();
            let draft = store.save_service_draft(draft, None).unwrap();
            let version = store
                .publish_service(&draft.id, BindingScope::Defaults)
                .unwrap();
            assert_eq!(version.config.protocol, protocol);
            assert_eq!(version.config.credential, None);

            let mut config = ConfigFile::default();
            config.llm.enabled = true;
            let resolved = store
                .config_for_refs(&config, &store.default_refs())
                .unwrap();
            assert_eq!(resolved.llm.provider, provider);
            assert!(resolved.llm.api_key.is_empty());
            assert_eq!(resolved.llm.model, "fixture-model");
            if protocol == ServiceProtocol::CodexResponses {
                // CLI 约定：Codex 的 base_url 留空（方言端点固定）
                assert!(resolved.llm.base_url.is_empty());
            } else {
                assert!(resolved.llm.base_url.starts_with("http"));
            }

            // 持久化往返：新协议版本在重启后仍完整可用
            let reopened = open(directory.path(), store.vault());
            assert_eq!(
                reopened.version(&version.id).unwrap().config,
                version.config
            );
            let resolved = reopened
                .config_for_refs(&config, &reopened.default_refs())
                .unwrap();
            assert_eq!(resolved.llm.provider, provider);
        }
    }

    #[test]
    fn stale_test_result_cannot_mark_changed_fields_as_passed() {
        let (_directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "model-a");
        let config = draft.configuration().unwrap();
        let evidence = ServiceTestEvidence {
            fingerprint: config.fingerprint("proofread-v1"),
            contract: "proofread-v1".into(),
            tested_at: 1,
            outcome: TestOutcome::Passed,
            message: "校对用途测试通过".into(),
            details: Vec::new(),
            http_status: None,
        };
        let mut renamed = config.clone();
        renamed.name = "A different display name".into();
        assert_eq!(
            config.fingerprint("proofread-v1"),
            renamed.fingerprint("proofread-v1")
        );
        let mut edit = draft.clone();
        edit.model = "model-b".into();
        store.save_service_draft(edit, None).unwrap();
        let current = store.draft(&draft.id).unwrap().configuration().unwrap();
        assert_ne!(
            current.fingerprint(&evidence.contract),
            evidence.fingerprint
        );
    }

    #[test]
    fn new_defaults_are_local_independent_and_do_not_export_extra_files() {
        let (_directory, store) = isolated();
        let preferences = store.generation();
        assert!(!preferences.ai_proofread && !preferences.ai_summary && !preferences.vision);
        assert_eq!(preferences.options.formats.as_deref(), Some([].as_slice()));
        let mut preferences = preferences.clone();
        preferences.ai_summary = true;
        preferences.vision = true;
        let mut config = ConfigFile::default();
        config.defaults.out = Some(PathBuf::from("/test-library"));
        config.llm.api_key = "test-only-old-global-key".into();
        preferences.apply_to(&mut config);
        assert!(!config.llm.enabled && config.llm.summarize && !config.llm.vision);
        assert_eq!(config.defaults.out, Some(PathBuf::from("/test-library")));
        assert!(config.llm.api_key.is_empty());
    }

    #[test]
    fn evidence_without_http_status_remains_readable() {
        let evidence: ServiceTestEvidence = serde_json::from_value(serde_json::json!({
            "fingerprint": "f", "contract": "c", "tested_at": 1,
            "outcome": "model_refused", "message": "m", "details": ["用途：x", "HTTP 429"]
        }))
        .unwrap();
        assert_eq!(evidence.http_status, None);
    }

    #[test]
    fn permission_failure_names_the_actionable_cause_without_a_temporary_filename() {
        let error = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/isolated/settings/.tmpIrEDzg",
        ))
        .context("无法保存此组设置的恢复副本");
        assert_eq!(
            save_failure_message(PreferenceGroup::Application, &error),
            "应用偏好未保存：设置目录暂时无法写入"
        );
    }

    #[test]
    fn failed_ordinary_edits_are_recovered_without_becoming_active() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("preferences");
        let mut store = open(&root, Arc::new(MemoryCredentialVault::new()));
        std::fs::create_dir_all(root.join("generation.json")).unwrap();
        std::fs::create_dir_all(root.join("application.json")).unwrap();
        let mut generation = store.generation().clone();
        generation.prompt_draft = Some("Keep the technical terms.".into());
        generation.ai_summary = true;
        let mut application = store.application().clone();
        application.font_scale = 2.0;
        store.save_generation(generation.clone()).unwrap();
        store.save_application(application.clone()).unwrap();
        let results = settle(&mut store);
        assert!(failed(&results, PreferenceGroup::Generation));
        assert!(failed(&results, PreferenceGroup::Application));
        assert!(store.unsaved_intents_are_preserved());
        let mut reopened = open(&root, store.vault());
        assert!(!reopened.generation().ai_summary);
        assert_eq!(reopened.application().font_scale, 1.0);
        assert_eq!(reopened.generation_intent(), Some(&generation));
        assert_eq!(reopened.application_intent(), Some(&application));
        std::fs::remove_dir(root.join("generation.json")).unwrap();
        std::fs::remove_dir(root.join("application.json")).unwrap();
        reopened.blocked.clear();
        reopened.save_generation(generation).unwrap();
        reopened.save_application(application).unwrap();
        assert!(
            settle(&mut reopened)
                .iter()
                .all(|(_, result)| result.is_ok())
        );
        assert!(reopened.generation_intent().is_none());
        assert!(reopened.application_intent().is_none());
        let saved = open(&root, reopened.vault());
        assert!(saved.generation().ai_summary);
        assert_eq!(saved.application().font_scale, 2.0);
    }

    #[test]
    fn recovery_failure_is_distinct_from_a_durable_unapplied_edit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("preferences");
        let mut store = open(&root, Arc::new(MemoryCredentialVault::new()));
        std::fs::create_dir_all(root.join("generation.json")).unwrap();
        std::fs::write(
            root.with_extension("recovery"),
            b"blocked recovery location",
        )
        .unwrap();
        let mut edit = store.generation().clone();
        edit.ai_summary = true;
        store.save_generation(edit).unwrap();
        assert!(failed(&settle(&mut store), PreferenceGroup::Generation));
        assert!(!store.unsaved_intents_are_preserved());
        let reopened = open(&root, store.vault());
        assert!(!reopened.generation().ai_summary);
        assert!(reopened.generation_intent().is_none());
    }

    #[test]
    fn service_recovery_retains_drafts_but_never_replaces_the_active_version() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("preferences");
        let mut store = open(&root, Arc::new(MemoryCredentialVault::new()));
        let draft = complete_draft(&mut store, "active-model");
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        // The verified backup represents the current committed version before the failure.
        std::fs::copy(root.join("services.json"), root.join("services.json.bak")).unwrap();
        std::fs::remove_file(root.join("services.json")).unwrap();
        std::fs::create_dir(root.join("services.json")).unwrap();
        let mut edit = ServiceDraft::from_version(&version);
        edit.model = "unapplied-model".into();
        let saved = store
            .save_service_draft(edit, Some(Secret::new("isolated-new-key")))
            .unwrap();
        assert!(store.is_recovery_draft(&saved.id));
        let mut reopened = open(&root, store.vault());
        assert_eq!(
            reopened.default_refs().llm.as_deref(),
            Some(version.id.as_str())
        );
        assert_eq!(reopened.draft(&saved.id).unwrap().model, "unapplied-model");
        let recovery = std::fs::read_to_string(
            root.with_extension("recovery")
                .join(format!("{}.json", saved.id)),
        )
        .unwrap();
        assert!(!recovery.contains("isolated-new-key"));
        std::fs::remove_dir(root.join("services.json")).unwrap();
        let published = reopened
            .publish_service(&saved.id, BindingScope::CurrentTask)
            .unwrap();
        let again = open(&root, reopened.vault());
        assert!(again.draft(&saved.id).is_none());
        assert!(again.version(&published.id).is_some());
        assert_eq!(again.default_refs().llm, Some(version.id));
    }

    #[test]
    fn restoring_a_pre_delete_backup_cannot_reenable_service_dispatch() {
        let (directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "model");
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        let backup = std::fs::read(directory.path().join("services.json")).unwrap();
        store.delete_service(&version.service_id).unwrap();
        std::fs::write(directory.path().join("services.json"), backup).unwrap();
        let recovered = open(directory.path(), store.vault());
        assert!(recovered.version(&version.id).is_none());
        assert!(recovered.is_service_retired(&version.service_id));
        assert!(recovered.check_dispatch(&version.id).is_err());
    }

    #[test]
    fn preview_uses_loaded_services_but_dispatch_rechecks_delete_markers() {
        let (directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "model");
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        let mut config = ConfigFile::default();
        config.llm.enabled = true;
        let refs = store.default_refs();
        let preview = store.config_for_preview(&config, &refs).unwrap();
        assert_eq!(preview.llm.model, "model");
        let markers = directory.path().join("stopped-services");
        std::fs::create_dir_all(&markers).unwrap();
        std::fs::write(markers.join(&version.service_id), b"deleted\n").unwrap();
        assert!(store.config_for_preview(&config, &refs).is_ok());
        assert!(store.config_for_refs(&config, &refs).is_err());
        assert!(resolve_for_execution(&store, &config, &refs).is_err());
        store.delete_service(&version.service_id).unwrap();
        assert!(store.version(&version.id).is_none());
        assert!(store.config_for_preview(&config, &refs).is_err());
    }

    #[test]
    fn previously_stopped_services_are_removed_on_open() {
        let (directory, mut store) = isolated();
        let draft = complete_draft(&mut store, "model");
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        let mut services = serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(directory.path().join("services.json")).unwrap(),
        )
        .unwrap();
        services["value"]["stopped"] = serde_json::json!({ version.service_id.clone(): 1 });
        std::fs::write(
            directory.path().join("services.json"),
            serde_json::to_vec(&services).unwrap(),
        )
        .unwrap();
        let reopened = open(directory.path(), store.vault());
        assert!(reopened.version(&version.id).is_none());
        assert!(reopened.default_refs().llm.is_none());
        assert!(reopened.is_service_retired(&version.service_id));
        assert!(
            reopened
                .versions()
                .all(|item| item.service_id != version.service_id)
        );
    }

    #[test]
    fn credential_bearing_urls_never_enter_incomplete_draft_storage() {
        let (directory, mut store) = isolated();
        for address in [
            "https://user:never-save-me@host.test",
            "user:never-save-me@host.test",
            "https:/user:never-save-me@host.test",
            "https://host.test?key=never-save-me",
        ] {
            let mut draft = ServiceDraft::new(ServicePurpose::Ai);
            draft.address = address.into();
            let error = store
                .save_service_draft(draft, None)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("never-save-me"));
        }
        assert_eq!(store.service_drafts().count(), 0);
        assert!(!directory.path().join("services.json").exists());
    }

    #[test]
    fn unsupported_text_scale_is_not_published_or_restored_as_active() {
        let (_directory, mut store) = isolated();
        let mut app = store.application().clone();
        app.font_scale = f32::NAN;
        assert!(store.save_application(app).is_err());
        assert_eq!(store.application().font_scale, 1.0);
        assert!(!store.unsaved_intents_are_preserved());
    }

    #[test]
    fn theme_changes_survive_restart_without_changing_generation_defaults() {
        use crate::palettes::{Appearance, PaletteId};
        let (directory, mut store) = isolated();
        let generation = store.generation().clone();
        let mut app = store.application().clone();
        app.appearance.mode = Appearance::System;
        app.appearance.select(PaletteId::CatppuccinLatte);
        app.appearance.select(PaletteId::TokyoNight);
        store.save_application(app.clone()).unwrap();
        let reopened = open(directory.path(), store.vault());
        assert_eq!(reopened.application(), &app);
        assert_eq!(reopened.generation(), &generation);
        assert_eq!(
            reopened.application().appearance.resolve(false),
            PaletteId::CatppuccinLatte
        );
        assert_eq!(
            reopened.application().appearance.resolve(true),
            PaletteId::TokyoNight
        );
    }

    #[test]
    fn failed_theme_save_preserves_active_palette_and_recovers_the_unapplied_choice() {
        use crate::palettes::PaletteId;
        let (directory, mut store) = isolated();
        store.save_application(store.application().clone()).unwrap();
        std::fs::remove_file(directory.path().join("application.json")).unwrap();
        std::fs::create_dir(directory.path().join("application.json")).unwrap();
        let active = store.application().clone();
        let mut edit = active.clone();
        edit.appearance.select(PaletteId::Nord);
        store.save_application(edit.clone()).unwrap();
        assert!(failed(&settle(&mut store), PreferenceGroup::Application));
        assert_eq!(store.application_intent(), Some(&edit));
        let reopened = open(directory.path(), store.vault());
        assert_eq!(reopened.application().appearance, active.appearance);
        assert_eq!(reopened.application_intent(), Some(&edit));
    }

    #[test]
    fn pre_theme_application_preferences_get_defaults_without_losing_user_settings() {
        let app: ApplicationPreferences =
            serde_json::from_str(r#"{"font_scale":1.25,"desktop":{"reduce_motion":true}}"#)
                .unwrap();
        assert_eq!(app.font_scale, 1.25);
        assert!(app.desktop.reduce_motion);
        assert_eq!(app.appearance, crate::palettes::ThemePreferences::default());
    }

    #[test]
    fn library_move_relocates_only_its_models_and_keeps_failed_old_reference_protected() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("preferences");
        let old = directory.path().join("old-library");
        let new = directory.path().join("new-library");
        let outside = directory.path().join("external-models");
        assert!(relative_path_inside(&old.join("../external-models"), &old).is_none());
        assert_eq!(
            relative_path_inside(&outside.join("../old-library/models"), &old),
            Some(PathBuf::from("models"))
        );
        let mut store = open(&root, Arc::new(MemoryCredentialVault::new()));
        let mut config = store.generation().clone();
        config.options.model_dir = Some(outside.clone());
        store.save_generation(config).unwrap();
        settle(&mut store);
        store.relocate_generation_paths(&old, &new).unwrap();
        assert_eq!(
            store.generation().options.model_dir.as_ref(),
            Some(&outside)
        );
        let mut config = store.generation().clone();
        config.options.model_dir = Some(old.join("models"));
        store.save_generation(config).unwrap();
        settle(&mut store);
        std::fs::remove_file(root.join("generation.json")).unwrap();
        std::fs::create_dir(root.join("generation.json")).unwrap();
        store.relocate_generation_paths(&old, &new).unwrap();
        // Until the writer confirms, the file on disk still names the old library.
        assert!(store.references_storage_path(&old));
        assert!(failed(&settle(&mut store), PreferenceGroup::Generation));
        assert!(store.references_storage_path(&old));
        assert!(store.references_storage_path(&new));
        assert!(store.unsaved_intents_are_preserved());
        std::fs::remove_dir(root.join("generation.json")).unwrap();
        store
            .save_generation(store.generation_intent().unwrap().clone())
            .unwrap();
        settle(&mut store);
        assert!(!store.references_storage_path(&old));
        assert!(store.references_storage_path(&new));
    }
    #[test]
    fn background_preferences_writer_serializes_and_flushes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("generation.json");
        let writer = ThreadWriter::new(root.path().to_path_buf());
        writer
            .send(PreferenceSignal::Write {
                path: path.clone(),
                bytes: b"{\"ok\":true}".to_vec(),
                group: PreferenceGroup::Generation,
                revision: 1,
            })
            .unwrap();
        writer.flush();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"ok\":true}");
        assert!(
            writer
                .try_result()
                .is_some_and(|result| result.result.is_ok())
        );
    }
    /// The production background writer.
    fn store_with_async_writer(root: &std::path::Path) -> Store {
        Store::open(
            root.to_path_buf(),
            Arc::new(crate::credentials::MemoryCredentialVault::new()),
        )
    }

    #[test]
    fn async_generation_save_reports_pending_until_written() {
        let root = tempfile::tempdir().unwrap();
        let mut store = store_with_async_writer(root.path());
        let value = store.generation().clone();
        store.save_generation(value).unwrap();
        assert!(store.is_pending(PreferenceGroup::Generation));
        assert!(store.generation_intent().is_some());
        store.flush();
        let completed = store.process_writes();
        assert_eq!(completed.len(), 1);
        assert!(completed[0].1.is_ok());
        assert!(!store.is_pending(PreferenceGroup::Generation));
        assert!(store.generation_intent().is_none());
        store.flush();
        assert!(root.path().join("generation.json").is_file());
        assert!(!root.path().join("generation-intent.json").exists());
    }

    #[test]
    fn failed_async_save_keeps_the_recovery_intent() {
        let root = tempfile::tempdir().unwrap();
        let mut store = store_with_async_writer(root.path());
        std::fs::create_dir(root.path().join("generation.json")).unwrap();
        let value = store.generation().clone();
        store.save_generation(value).unwrap();
        store.flush();
        let completed = store.process_writes();
        assert_eq!(completed.len(), 1);
        assert!(completed[0].1.is_err());
        assert!(!store.is_pending(PreferenceGroup::Generation));
        assert!(store.generation_intent().is_some());
        assert!(store.unsaved_intents_are_preserved());
    }
    #[test]
    fn async_service_publish_keeps_recovery_until_written() {
        let root = tempfile::tempdir().unwrap();
        let mut store = store_with_async_writer(root.path());
        let draft = complete_draft(&mut store, "test-model");
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        assert!(store.is_pending(PreferenceGroup::Services));
        assert!(store.has_pending_services());
        store.flush();
        let completed = store.process_writes();
        assert!(completed.iter().all(|(_, result)| result.is_ok()));
        assert!(!store.is_pending(PreferenceGroup::Services));
        assert!(!store.has_pending_services());
        store.flush();
        assert!(!root.path().join(format!("{}.json", draft.id)).exists());
        assert!(store.services.versions.contains_key(&version.id));
    }
    #[test]
    fn failed_service_publish_keeps_intent_and_can_retry() {
        let root = tempfile::tempdir().unwrap();
        let mut store = store_with_async_writer(root.path());
        let draft = complete_draft(&mut store, "retry-model");
        store.flush();
        let _ = store.process_writes();
        let services = root.path().join(PreferenceGroup::Services.filename());
        std::fs::remove_file(&services).unwrap();
        std::fs::create_dir(&services).unwrap();
        let version = store
            .publish_service(&draft.id, BindingScope::Defaults)
            .unwrap();
        store.flush();
        let completed = store.process_writes();
        assert!(completed.iter().any(|(_, result)| result.is_err()));
        assert!(store.has_pending_services());
        assert!(
            store
                .recovery_root()
                .join(format!("{}.json", draft.id))
                .exists()
        );
        std::fs::remove_dir(&services).unwrap();
        store.retry_services().unwrap();
        store.flush();
        let completed = store.process_writes();
        assert!(completed.iter().all(|(_, result)| result.is_ok()));
        assert!(!store.has_pending_services());
        store.flush();
        assert!(store.services.versions.contains_key(&version.id));
        assert!(
            !store
                .recovery_root()
                .join(format!("{}.json", draft.id))
                .exists()
        );
    }
}
