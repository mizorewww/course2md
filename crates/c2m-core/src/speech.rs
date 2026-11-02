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
/// (`course2md models prepare`, or the desktop's prepare action).
#[derive(Debug)]
pub struct ModelRequired {
    pub provider: AsrProvider,
    pub model: String,
}

impl std::fmt::Display for ModelRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&tr!(
            "识别模型 {} 尚未下载，请先下载并准备模型，再继续 / The speech model {} is not downloaded yet; prepare it, then continue",
            self.model,
            self.model
        ))
    }
}

impl std::error::Error for ModelRequired {}
