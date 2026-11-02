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
