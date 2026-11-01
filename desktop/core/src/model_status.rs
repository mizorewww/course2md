//! What is known about a recognition model and its device, and what can be done about it.
use course2md::{
    config::{AsrProvider, canonical_model},
    models::status::LocalModelStatus,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Shown wherever a model cannot be prepared while another job runs.
pub fn job_busy() -> &'static str {
    course2md::tr!("当前任务完成后才能准备模型")
}

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub provider: AsrProvider,
    pub model: String,
    pub root: PathBuf,
}

impl Request {
    /// Aliases share the canonical cache entry; an unsupported value is checked as typed.
    pub fn new(provider: AsrProvider, model: Option<&str>, root: &Path) -> Self {
        let model = course2md::config::effective_model(provider, model);
        Self {
            provider,
            model: canonical_model(provider, model).unwrap_or_else(|_| model.to_owned()),
            root: root.into(),
        }
    }
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.provider.as_str(),
            self.model,
            self.root.display()
        )
    }
}

pub struct Entry {
    pub request: Request,
    pub generation: u64,
    pub checking: bool,
    pub result: Option<Result<LocalModelStatus, String>>,
    pub cache_directories: BTreeSet<PathBuf>,
}

impl Entry {
    pub fn status(&self) -> Option<&LocalModelStatus> {
        self.result.as_ref().and_then(|result| result.as_ref().ok())
    }
}

/// How the last preparation ended, kept with the request it belongs to.
pub struct Outcome {
    pub key: String,
    pub message: String,
    pub error: bool,
    pub cancelled: bool,
}

#[derive(Default)]
pub struct State {
    pub entries: BTreeMap<String, Entry>,
    /// The request the running model job is preparing; taken when it exits.
    pub preparing: Option<Request>,
    pub outcome: Option<Outcome>,
    /// The models whose files and preparation log are expanded.
    pub details: BTreeSet<String>,
}

/// Hardware this platform cannot have is not offered: Apple silicon engines only on
/// macOS, Intel NPU never there. A saved choice stays visible on any platform.
pub fn engine_offered(provider: AsrProvider, selected: Option<AsrProvider>) -> bool {
    Some(provider) == selected
        || match provider {
            AsrProvider::Coreml => cfg!(target_os = "macos"),
            AsrProvider::Npu => !cfg!(target_os = "macos"),
            _ => true,
        }
}

/// The one compatibility rule for a saved local model, aliases and NPU repositories included.
pub fn model_supported(provider: AsrProvider, model: &str) -> bool {
    provider != AsrProvider::Api && canonical_model(provider, model).is_ok()
}

pub fn unsupported_model() -> &'static str {
    course2md::tr!("所选识别方式不支持这个模型，请换一个")
}

/// The one classification of why a local engine cannot run here. Messages,
/// repair actions and engine choices all derive from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceIssue {
    Engine,
    AppleHardware,
    AppleRuntime,
    LlamaMissing,
    /// Windows: llama-server is there but the Microsoft Visual C++ runtime it
    /// links against is not.
    VcRuntime,
    GpuAbsent,
    NpuDevice,
    NpuRuntime,
}

impl DeviceIssue {
    /// A short status for an engine choice.
    pub fn short(self) -> &'static str {
        match self {
            Self::Engine => course2md::tr!("需要重新安装应用"),
            Self::AppleHardware => course2md::tr!("需要 Apple 芯片的 Mac"),
            Self::GpuAbsent => course2md::tr!("未检测到可用显卡"),
            Self::NpuDevice => course2md::tr!("未检测到 Intel NPU"),
            Self::AppleRuntime | Self::LlamaMissing | Self::VcRuntime | Self::NpuRuntime => {
                course2md::tr!("需要安装识别组件")
            }
        }
    }
    /// The full explanation shown with repair actions.
    pub fn message(self) -> &'static str {
        match self {
            Self::Engine => course2md::tr!("转换程序无法运行，请重新安装应用"),
            Self::AppleHardware => course2md::tr!("当前系统不支持 Apple 芯片识别，请改用其他方式"),
            Self::AppleRuntime => {
                course2md::tr!("缺少 Apple 识别组件，请重新安装应用或改用其他方式")
            }
            Self::LlamaMissing => course2md::tr!("安装 llama.cpp 后才能用 GPU 或 CPU 识别"),
            Self::VcRuntime => {
                course2md::tr!("GPU 和 CPU 识别需要 Microsoft Visual C++ 运行库，安装后重新检查")
            }
            Self::GpuAbsent => course2md::tr!("未检测到可用显卡，可改用 CPU"),
            Self::NpuDevice => course2md::tr!("未检测到 Intel NPU，请改用其他方式"),
            Self::NpuRuntime => course2md::tr!("安装 uv 后才能用 Intel NPU 识别"),
        }
    }
    /// Whether installing something on this machine can clear the issue.
    pub fn installable(self) -> bool {
        matches!(
            self,
            Self::LlamaMissing | Self::VcRuntime | Self::NpuRuntime | Self::AppleRuntime
        )
    }
}

pub fn device_issue(
    provider: AsrProvider,
    environment: &crate::backend::Environment,
) -> Option<DeviceIssue> {
    if !environment.engine {
        return Some(DeviceIssue::Engine);
    }
    match provider {
        AsrProvider::Coreml if !cfg!(all(target_os = "macos", target_arch = "aarch64")) => {
            Some(DeviceIssue::AppleHardware)
        }
        AsrProvider::Coreml if !environment.apple => Some(DeviceIssue::AppleRuntime),
        AsrProvider::Gpu if !environment.llama => Some(DeviceIssue::LlamaMissing),
        AsrProvider::Gpu | AsrProvider::Cpu if !environment.vc_runtime => {
            Some(DeviceIssue::VcRuntime)
        }
        AsrProvider::Gpu if environment.gpu.is_none() => Some(DeviceIssue::GpuAbsent),
        AsrProvider::Cpu if !environment.llama => Some(DeviceIssue::LlamaMissing),
        AsrProvider::Npu if !environment.npu_device() => Some(DeviceIssue::NpuDevice),
        AsrProvider::Npu if !environment.npu_runtime() => Some(DeviceIssue::NpuRuntime),
        _ => None,
    }
}
