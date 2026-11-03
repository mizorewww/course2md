//! The processing choices a note is generated with: what the workbench edits
//! for the current input, and what a task's configuration implies.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConversionOptions {
    #[serde(with = "crate::provider_serde")]
    pub provider: Option<c2m_config::config::AsrProvider>,
    #[serde(with = "text_source_index")]
    pub source_mode: c2m_config::config::TranscriptSource,
    pub llm: bool,
    #[serde(default)]
    pub summarize: bool,
    #[serde(default)]
    pub vision: bool,
    /// Bilingual notes: target language code, or None for no translation.
    #[serde(default)]
    pub translate: Option<String>,
    pub keep_video: bool,
    pub formats: [bool; 3],
}

impl ConversionOptions {
    /// Whether the options use a cloud recognition service.
    pub fn uses_cloud_provider(&self) -> bool {
        self.provider == Some(c2m_config::config::AsrProvider::Api)
    }

    pub fn from_config(config: &c2m_config::settings::ConfigFile) -> Self {
        let provider = config.defaults.provider;
        let source_mode = config.defaults.transcript_source.unwrap_or_default();
        let llm = config.llm.enabled;
        let keep_video = config.defaults.keep_video.unwrap_or(false);
        let formats = config
            .defaults
            .formats
            .as_ref()
            .map(|formats| {
                use c2m_config::config::OutputFormat::*;
                [
                    formats.contains(&Md),
                    formats.contains(&Html),
                    formats.contains(&Json),
                ]
            })
            .unwrap_or([true, true, false]);
        Self {
            provider,
            source_mode,
            llm,
            summarize: config.llm.summarize,
            vision: config.llm.vision,
            translate: config.llm.translate.clone(),
            keep_video,
            formats,
        }
    }

    /// These options as a task plan records them: the configuration keeps
    /// only what takes effect (vision only with proofreading).
    pub fn as_planned(&self) -> Self {
        let mut config = c2m_config::settings::ConfigFile::default();
        self.apply_to(&mut config);
        Self::from_config(&config)
    }
    pub fn apply_to(&self, config: &mut c2m_config::settings::ConfigFile) {
        use c2m_config::config::OutputFormat;
        config.defaults.provider = self.provider;
        config.defaults.transcript_source = Some(self.source_mode);
        config.defaults.keep_video = Some(self.keep_video);
        config.defaults.resume = Some(true);
        config.defaults.formats = Some(
            [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json]
                .into_iter()
                .zip(self.formats)
                .filter_map(|(f, selected)| selected.then_some(f))
                .collect(),
        );
        config.llm.enabled = self.llm;
        config.llm.summarize = self.summarize;
        config.llm.vision = self.llm && self.vision;
        config.llm.translate = self.translate.clone();
        config.llm.disable_hint = true;
    }
}

impl Default for ConversionOptions {
    fn default() -> Self {
        let mut value = Self::from_config(&Default::default());
        value.formats = [false; 3];
        value
    }
}

pub fn providers() -> [(&'static str, &'static str); 6] {
    [
        ("", c2m_i18n::tr!("自动")),
        ("coreml", c2m_i18n::tr!("Apple 芯片")),
        ("gpu", "GPU"),
        ("cpu", "CPU"),
        ("npu", "Intel NPU"),
        ("api", c2m_i18n::tr!("云端服务")),
    ]
}

/// Choice id of a recognition engine, as in `providers()`; automatic
/// selection is the empty id.
pub(crate) fn provider_id(provider: Option<c2m_config::config::AsrProvider>) -> &'static str {
    provider.map_or("", |provider| provider.as_str())
}

/// The canonical display name of a recognition engine; the desktop's one source for choosing and describing it.
/// Matches the ids of `providers()` one to one; None means 「自动」.
pub fn provider_label(provider: Option<c2m_config::config::AsrProvider>) -> &'static str {
    let id = provider_id(provider);
    providers()
        .iter()
        .find(|(provider_id, _)| *provider_id == id)
        .map(|(_, label)| *label)
        .unwrap_or(c2m_i18n::tr!("自动"))
}

pub fn provider_from_id(id: &str) -> Option<c2m_config::config::AsrProvider> {
    c2m_config::config::AsrProvider::from_id(id)
}

/// The text source choice is stored as its historical index (0 auto,
/// 1 subtitles, 2 speech recognition).
mod text_source_index {
    use c2m_config::config::TranscriptSource;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &TranscriptSource,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(match value {
            TranscriptSource::Auto => 0,
            TranscriptSource::Subtitle => 1,
            TranscriptSource::Asr => 2,
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<TranscriptSource, D::Error> {
        Ok(match u64::deserialize(deserializer)? {
            1 => TranscriptSource::Subtitle,
            2 => TranscriptSource::Asr,
            _ => TranscriptSource::Auto,
        })
    }
}
