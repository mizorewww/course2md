//! The processing choices a note is generated with: what the workbench edits
//! for the current input, and what a task's configuration implies.

impl ConversionOptions {
    /// Whether the options use a cloud recognition service.
    pub(crate) fn uses_cloud_provider(&self) -> bool {
        self.provider == Some(course2md::config::AsrProvider::Api)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConversionOptions {
    #[serde(with = "crate::provider_serde")]
    pub(crate) provider: Option<course2md::config::AsrProvider>,
    #[serde(with = "crate::workspace::text_source_index")]
    pub(crate) source_mode: course2md::config::TranscriptSource,
    pub(crate) llm: bool,
    #[serde(default)]
    pub(crate) summarize: bool,
    #[serde(default)]
    pub(crate) vision: bool,
    /// Bilingual notes: target language code, or None for no translation.
    #[serde(default)]
    pub(crate) translate: Option<String>,
    pub(crate) keep_video: bool,
    pub(crate) formats: [bool; 3],
}

impl ConversionOptions {
    pub(crate) fn from_config(config: &course2md::settings::ConfigFile) -> Self {
        let provider = config.defaults.provider;
        let source_mode = config.defaults.transcript_source.unwrap_or_default();
        let llm = config.llm.enabled;
        let keep_video = config.defaults.keep_video.unwrap_or(false);
        let formats = config
            .defaults
            .formats
            .as_ref()
            .map(|formats| {
                use course2md::config::OutputFormat::*;
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
}

impl Default for ConversionOptions {
    fn default() -> Self {
        let mut value = Self::from_config(&Default::default());
        value.formats = [false; 3];
        value
    }
}

impl ConversionOptions {
    pub(crate) fn apply_to(&self, config: &mut course2md::settings::ConfigFile) {
        use course2md::config::OutputFormat;
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
