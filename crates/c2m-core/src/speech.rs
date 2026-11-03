//! Which speech recognition backend a task uses.

use crate::tr;
use serde::{Deserialize, Serialize};

/// ASR 后端。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::VariantArray,
    strum::IntoStaticStr,
    strum::EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum AsrProvider {
    Coreml,
    Gpu,
    Cpu,
    Npu,
    Api,
}

impl AsrProvider {
    pub fn from_id(id: &str) -> Option<Self> {
        id.parse().ok()
    }

    pub fn as_str(&self) -> &'static str {
        self.into()
    }

    /// 面向用户的识别方式名称（中文 / English）。
    pub fn label(&self) -> (&'static str, &'static str) {
        match self {
            Self::Coreml => (tr!("Apple 芯片本机识别"), "Apple silicon on-device"),
            Self::Gpu => (tr!("显卡加速"), "GPU"),
            Self::Cpu => (tr!("CPU（较慢）"), "CPU (slower)"),
            Self::Npu => (tr!("Intel NPU 加速"), "Intel NPU"),
            Self::Api => (tr!("云端语音识别"), "Cloud speech service"),
        }
    }
}

/// A task needs a local speech model that is not downloaded, and may not download it
/// on its own: a download of a gigabyte or more waits for the person to agree to it
/// (`course2md models prepare`, the desktop's prepare action, or a yes at the terminal).
#[derive(Debug)]
pub struct ModelRequired {
    pub provider: AsrProvider,
    pub model: String,
    pub remedy: Remedy,
}

/// What the person can do about a [`ModelRequired`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// Prepare the model (the desktop's action), then continue the task.
    Prepare,
    /// The command line may not download without consent: run this `models prepare`
    /// command first, or pass `--download-models`.
    Command(String),
    /// The person said no at the terminal; this command prepares it later.
    Declined(String),
}

impl std::fmt::Display for ModelRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let model = &self.model;
        f.write_str(&match &self.remedy {
            Remedy::Prepare => tr!(
                "识别模型 {} 尚未下载，请先下载并准备模型，再继续 / The speech model {} is not downloaded yet; prepare it, then continue",
                model,
                model
            ),
            Remedy::Command(command) => tr!(
                "识别模型 {} 尚未下载，未经同意不会下载。请先运行 {} 准备模型，或加 --download-models 允许本次下载 / The speech model {} is not downloaded, and it is not downloaded without your consent. Prepare it first with {}, or pass --download-models to allow the download in this run",
                model,
                command,
                model,
                command
            ),
            Remedy::Declined(command) => tr!(
                "未下载识别模型 {}。之后可运行 {} 准备模型，再重新运行本命令 / The speech model {} was not downloaded. To prepare it later, run {}, then run this command again",
                model,
                command,
                model,
                command
            ),
        })
    }
}

impl std::error::Error for ModelRequired {}
