//! Model evidence is loaded in the background; preparation uses the existing model job.
//! One surface reports a model everywhere: setup, settings and the workbench.
use crate::{
    Desktop, Kind, activity, provider_label, symbols,
    ui::{self, AsyncStatus},
};
use course2md::{
    config::{AsrProvider, canonical_model, model_dir_from, model_label},
    models::status::{CacheState, LocalModelStatus},
};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// One vocabulary for cache states across settings, setup and diagnostics.
pub(crate) fn cache_state_label(state: CacheState) -> (StatusKind, &'static str) {
    match state {
        CacheState::Loaded => (StatusKind::Success, course2md::tr!("已就绪")),
        CacheState::Cached => (StatusKind::Success, course2md::tr!("已下载")),
        CacheState::Partial => (StatusKind::Warning, course2md::tr!("未下载完整")),
        CacheState::Missing => (StatusKind::Neutral, course2md::tr!("未下载")),
        CacheState::Unsupported => (StatusKind::Warning, course2md::tr!("不支持")),
    }
}

/// Shown wherever a model cannot be prepared while another job runs.
pub(crate) fn job_busy() -> &'static str {
    course2md::tr!("当前任务完成后才能准备模型")
}

/// Preparation log lines shown when expanded; older lines stay in the task log.
const LOG_TAIL: usize = 80;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) provider: AsrProvider,
    pub(crate) model: String,
    pub(crate) root: PathBuf,
}
impl Request {
    /// Aliases share the canonical cache entry; an unsupported value is checked as typed.
    pub(crate) fn new(provider: AsrProvider, model: Option<&str>, root: &Path) -> Self {
        let model = course2md::config::effective_model(provider, model);
        Self {
            provider,
            model: canonical_model(provider, model).unwrap_or_else(|_| model.to_owned()),
            root: root.into(),
        }
    }
    pub(crate) fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.provider.as_str(),
            self.model,
            self.root.display()
        )
    }
}
struct Entry {
    request: Request,
    generation: u64,
    checking: bool,
    result: Option<Result<LocalModelStatus, String>>,
    cache_directories: BTreeSet<PathBuf>,
}
impl Entry {
    fn status(&self) -> Option<&LocalModelStatus> {
        self.result.as_ref().and_then(|result| result.as_ref().ok())
    }
}
/// How the last preparation ended, kept with the request it belongs to.
pub(crate) struct Outcome {
    key: String,
    pub(crate) message: String,
    pub(crate) error: bool,
    pub(crate) cancelled: bool,
}
#[derive(Default)]
pub(super) struct State {
    entries: BTreeMap<String, Entry>,
    /// The request the running model job is preparing; taken when it exits.
    preparing: Option<Request>,
    outcome: Option<Outcome>,
    /// The models whose files and preparation log are expanded.
    details: BTreeSet<String>,
}

/// A read-only view for the workbench. Checks and downloads keep their ownership here.
pub(crate) struct ModelSetupSnapshot {
    pub(crate) checking: bool,
    pub(crate) result: Option<Result<LocalModelStatus, String>>,
    pub(crate) device_issue: Option<DeviceIssue>,
    pub(crate) preparing: bool,
    pub(crate) notice: Option<(String, bool)>,
    pub(crate) cancelled: bool,
}

/// Local recognition engines in display order.
pub(crate) const LOCAL_ENGINES: [AsrProvider; 4] = [
    AsrProvider::Coreml,
    AsrProvider::Gpu,
    AsrProvider::Cpu,
    AsrProvider::Npu,
];

pub(crate) fn engine_icon(provider: Option<AsrProvider>) -> SymbolIcon {
    match provider {
        None => symbols::auto_fix_high(),
        Some(AsrProvider::Coreml | AsrProvider::Cpu) => symbols::memory(),
        Some(AsrProvider::Gpu) => symbols::developer_board(),
        Some(AsrProvider::Npu) => symbols::auto_awesome(),
        Some(AsrProvider::Api) => symbols::cloud(),
    }
}
/// Hardware this platform cannot have is not offered: Apple silicon engines only on
/// macOS, Intel NPU never there. A saved choice stays visible on any platform.
pub(crate) fn engine_offered(provider: AsrProvider, selected: Option<AsrProvider>) -> bool {
    Some(provider) == selected
        || match provider {
            AsrProvider::Coreml => cfg!(target_os = "macos"),
            AsrProvider::Npu => !cfg!(target_os = "macos"),
            _ => true,
        }
}
/// The one compatibility rule for a saved local model, aliases and NPU repositories included.
pub(crate) fn model_supported(provider: AsrProvider, model: &str) -> bool {
    provider != AsrProvider::Api && canonical_model(provider, model).is_ok()
}
pub(crate) fn unsupported_model() -> &'static str {
    course2md::tr!("所选识别方式不支持这个模型，请换一个")
}

/// 模型准备按钮文案的唯一权威来源（设置页与引导页共用）。
fn model_action_label(provider: AsrProvider, state: Option<&CacheState>) -> &'static str {
    match state {
        Some(CacheState::Missing) => course2md::tr!("下载并准备模型"),
        Some(CacheState::Partial) => course2md::tr!("继续准备模型"),
        Some(CacheState::Loaded) => course2md::tr!("重新检查是否可用"),
        Some(CacheState::Cached) if matches!(provider, AsrProvider::Cpu | AsrProvider::Gpu) => {
            course2md::tr!("检查模型文件")
        }
        Some(CacheState::Cached) => course2md::tr!("检查模型是否可用"),
        _ => course2md::tr!("继续准备模型"),
    }
}
/// Microsoft's permanent link to the latest supported Visual C++ runtime
/// installer for x64 (2015-2022), which llama.cpp's Windows build needs.
const VC_RUNTIME_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
const RELEASES_URL: &str = "https://github.com/mizorewww/course2md/releases";
const LLAMA_GUIDE_URL: &str = "https://github.com/ggml-org/llama.cpp#quick-start";
const UV_GUIDE_URL: &str = "https://docs.astral.sh/uv/getting-started/installation/";

/// The one classification of why a local engine cannot run here. Messages,
/// repair actions and engine choices all derive from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceIssue {
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
    pub(crate) fn short(self) -> &'static str {
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
    fn message(self) -> &'static str {
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
    pub(crate) fn installable(self) -> bool {
        matches!(
            self,
            Self::LlamaMissing | Self::VcRuntime | Self::NpuRuntime | Self::AppleRuntime
        )
    }
}

/// One repair an engine obstacle offers.
enum Repair {
    Open(&'static str),
    Copy(&'static str),
}

fn repairs(issue: DeviceIssue) -> Vec<(&'static str, SymbolIcon, &'static str, Repair)> {
    let guide = |id, label, url| (id, symbols::open_in_new(), label, Repair::Open(url));
    match issue {
        DeviceIssue::Engine | DeviceIssue::AppleRuntime => vec![(
            "reinstall",
            symbols::download(),
            course2md::tr!("下载完整应用"),
            Repair::Open(RELEASES_URL),
        )],
        DeviceIssue::LlamaMissing => {
            let help = guide(
                "llama-help",
                course2md::tr!("查看 llama.cpp 安装说明"),
                LLAMA_GUIDE_URL,
            );
            if cfg!(target_os = "macos") {
                vec![
                    (
                        "copy-llama",
                        symbols::content_copy(),
                        course2md::tr!("复制 Homebrew 安装命令"),
                        Repair::Copy("brew install llama.cpp"),
                    ),
                    help,
                ]
            } else {
                vec![help]
            }
        }
        DeviceIssue::VcRuntime => vec![(
            "vc-runtime",
            symbols::download(),
            course2md::tr!("下载 Microsoft Visual C++ 运行库"),
            Repair::Open(VC_RUNTIME_URL),
        )],
        DeviceIssue::NpuRuntime => vec![guide(
            "uv-help",
            course2md::tr!("查看 uv 安装说明"),
            UV_GUIDE_URL,
        )],
        DeviceIssue::AppleHardware | DeviceIssue::GpuAbsent | DeviceIssue::NpuDevice => Vec::new(),
    }
}

pub(crate) fn device_issue(
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

impl Desktop {
    pub(crate) fn setup_model_snapshot(
        &self,
        provider: AsrProvider,
        model: Option<&str>,
        root: &Path,
    ) -> ModelSetupSnapshot {
        let request = Request::new(provider, model, root);
        let entry = self
            .settings_ui
            .model_diagnostics
            .entries
            .get(&request.key());
        let outcome = self.model_outcome(&request);
        ModelSetupSnapshot {
            checking: entry.is_none_or(|entry| entry.checking),
            result: entry.and_then(|entry| entry.result.clone()),
            device_issue: self.engine_issue(provider),
            preparing: self.is_preparing(&request),
            notice: outcome.map(|outcome| (outcome.message.clone(), outcome.error)),
            cancelled: outcome.is_some_and(|outcome| outcome.cancelled),
        }
    }

    pub(crate) fn prepare_setup_model(
        &mut self,
        provider: AsrProvider,
        model: Option<&str>,
        root: &Path,
        cx: &mut Context<Self>,
    ) {
        self.begin_model_preparation(Request::new(provider, model, root), cx);
    }

    /// The request a running model job is preparing.
    fn active_preparation(&self) -> Option<&Request> {
        (self.kind == Kind::Models && self.job.is_some())
            .then_some(self.settings_ui.model_diagnostics.preparing.as_ref())
            .flatten()
    }
    pub(crate) fn is_preparing(&self, request: &Request) -> bool {
        self.active_preparation() == Some(request)
    }
    /// How the last preparation of this model ended, if it was the last one.
    pub(crate) fn model_outcome(&self, request: &Request) -> Option<&Outcome> {
        let key = request.key();
        let outcome = self.settings_ui.model_diagnostics.outcome.as_ref()?;
        (outcome.key == key).then_some(outcome)
    }
    /// The checked file state; `None` while checking or after a failed check.
    pub(crate) fn model_cache_state(&self, request: &Request) -> Option<CacheState> {
        self.settings_ui
            .model_diagnostics
            .entries
            .get(&request.key())
            .filter(|entry| !entry.checking)
            .and_then(Entry::status)
            .map(|status| status.state)
    }
    /// Whether the guide's forward action should wait behind a prepare action.
    pub(crate) fn model_awaits_download(&self, request: &Request) -> bool {
        self.job.is_none()
            && self.environment.is_some()
            && self.engine_issue(request.provider).is_none()
            && matches!(
                self.model_cache_state(request),
                Some(CacheState::Missing | CacheState::Partial)
            )
    }

    /// What a surface says this model is doing, in one short phrase.
    pub(crate) fn model_reading(&self, request: &Request) -> (StatusKind, &'static str) {
        if self.is_preparing(request) {
            return (StatusKind::Progress, course2md::tr!("正在准备模型"));
        }
        if self.model_outcome(request).is_some_and(|o| o.cancelled) {
            return (StatusKind::Neutral, course2md::tr!("模型准备已暂停"));
        }
        let entry = self
            .settings_ui
            .model_diagnostics
            .entries
            .get(&request.key());
        if self.environment.is_none() || entry.is_none_or(|entry| entry.checking) {
            return (StatusKind::Progress, course2md::tr!("正在检查模型"));
        }
        if let Some(issue) = self.engine_issue(request.provider) {
            return (StatusKind::Warning, issue.short());
        }
        match entry.and_then(|entry| entry.result.as_ref()) {
            Some(Ok(status)) => cache_state_label(status.state),
            _ => (StatusKind::Warning, course2md::tr!("检查未完成")),
        }
    }

    fn default_model_request(&self) -> Request {
        let defaults = &self.preferences.generation().options;
        Request::new(
            defaults
                .provider
                .unwrap_or_else(|| self.recommended_local_provider()),
            defaults.asr_model.as_deref(),
            &model_dir_from(defaults.model_dir.as_deref()),
        )
    }
    pub fn ensure_model_diagnostic(
        &mut self,
        provider: AsrProvider,
        model: Option<&str>,
        root: &Path,
        cx: &mut Context<Self>,
    ) {
        self.check_model_request(Request::new(provider, model, root), false, cx);
    }
    fn check_model_request(&mut self, request: Request, force: bool, cx: &mut Context<Self>) {
        if request.provider == AsrProvider::Api {
            return;
        }
        let key = request.key();
        let state = &mut self.settings_ui.model_diagnostics;
        if !force && state.entries.contains_key(&key) {
            return;
        }
        let generation = state
            .entries
            .get(&key)
            .map_or(1, |entry| entry.generation + 1);
        state.entries.insert(
            key.clone(),
            Entry {
                request: request.clone(),
                generation,
                checking: true,
                result: None,
                cache_directories: Default::default(),
            },
        );
        // inspect() 会对数 GB 的模型文件做同步 SHA-256，必须离开 executor；
        // 见 crate::spawn_blocking_io 的说明
        let work = crate::spawn_blocking_io(move || {
            let result =
                course2md::models::status::inspect(request.provider, &request.model, &request.root)
                    .map_err(|error| format!("{error:#}"));
            let directories = result
                .as_ref()
                .ok()
                .into_iter()
                .flat_map(|status| &status.parts)
                .filter(|part| part.path.is_dir())
                .map(|part| part.path.clone())
                .collect();
            (result, directories)
        });
        cx.spawn(async move |this, cx| {
            let Ok((result, cache_directories)) = work.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(entry) = this.settings_ui.model_diagnostics.entries.get_mut(&key)
                    && entry.generation == generation
                {
                    entry.checking = false;
                    entry.result = Some(result);
                    entry.cache_directories = cache_directories;
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub fn refresh_model_diagnostics(&mut self, cx: &mut Context<Self>) {
        let mut requests = self
            .settings_ui
            .model_diagnostics
            .entries
            .values()
            .map(|entry| (entry.request.key(), entry.request.clone()))
            .collect::<BTreeMap<_, _>>();
        let default = self.default_model_request();
        requests.insert(default.key(), default);
        if let Some(request) = self.active_preparation().cloned() {
            requests.insert(request.key(), request);
        }
        for request in requests.into_values() {
            self.check_model_request(request, true, cx);
        }
    }
    pub(super) fn ensure_settings_model_diagnostic(&mut self, cx: &mut Context<Self>) {
        self.check_model_request(self.default_model_request(), false, cx);
    }
    pub fn model_preparation_args(&self) -> Vec<String> {
        let request = self
            .settings_ui
            .model_diagnostics
            .preparing
            .clone()
            .unwrap_or_else(|| self.default_model_request());
        vec![
            "models".into(),
            "prepare".into(),
            "--provider".into(),
            request.provider.as_str().into(),
            "--model".into(),
            request.model,
            "--dir".into(),
            request.root.display().to_string(),
            "--json".into(),
        ]
    }
    fn begin_model_preparation(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.job.is_some() {
            self.message = Some(job_busy().into());
            cx.notify();
            return;
        }
        self.follow_setup_model(&request);
        let state = &mut self.settings_ui.model_diagnostics;
        state.preparing = Some(request);
        state.outcome = None;
        self.start_model_preparation(cx);
    }
    pub fn model_preparation_finished(
        &mut self,
        success: bool,
        cancelled: bool,
        cx: &mut Context<Self>,
    ) {
        let request = self
            .settings_ui
            .model_diagnostics
            .preparing
            .take()
            .unwrap_or_else(|| self.default_model_request());
        let name = model_label(&request.model);
        let message = if success {
            course2md::tr!("{} 已准备完成", name)
        } else if cancelled {
            course2md::tr!("{} 的准备已暂停", name)
        } else {
            course2md::tr!(
                "{} 的准备未完成：{}",
                name,
                self.job_events
                    .error
                    .as_deref()
                    .unwrap_or(course2md::tr!("准备失败"))
            )
        };
        self.settings_ui.model_diagnostics.outcome = Some(Outcome {
            key: request.key(),
            message,
            error: !success && !cancelled,
            cancelled: cancelled && !success,
        });
        self.refresh_model_diagnostics(cx);
        cx.notify();
    }
    /// Why the engine cannot run here; `None` while detection is pending.
    pub(crate) fn engine_issue(&self, provider: AsrProvider) -> Option<DeviceIssue> {
        device_issue(provider, self.environment.as_ref()?)
    }

    /// Why no engine can run, in one quiet line, followed by its repair.
    pub(crate) fn engine_obstacle(
        &self,
        id: &str,
        issue: DeviceIssue,
        cx: &mut Context<Self>,
    ) -> Div {
        self.engine_obstacle_with(id, issue, true, cx)
    }

    /// The obstacle and its repair, without its own 重新检查 where the
    /// surrounding surface already offers one (the 运行检查 summary).
    pub(crate) fn engine_obstacle_with(
        &self,
        id: &str,
        issue: DeviceIssue,
        recheck: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let actions = repairs(issue)
            .into_iter()
            .map(|(name, icon, label, repair)| {
                material_button(
                    SharedString::from(format!("{id}-{name}")),
                    ButtonVariant::Text,
                )
                .icon(icon)
                .child(label)
                .on_click(move |_, _, cx| match repair {
                    Repair::Open(url) => cx.open_url(url),
                    Repair::Copy(text) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.into()))
                    }
                })
                .into_any_element()
            })
            .chain(
                (recheck && issue.installable() && issue != DeviceIssue::AppleRuntime).then(|| {
                    material_button(
                        SharedString::from(format!("{id}-recheck")),
                        ButtonVariant::Text,
                    )
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新检查"))
                    .disabled(self.environment.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_environment(cx)))
                    .into_any_element()
                }),
            );
        let actions = h_flex()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .children(actions)
            .into_any_element();
        ui::async_status(
            SharedString::from(id.to_owned()),
            &AsyncStatus::Warning(issue.message().into()),
            Some(actions),
            cx,
        )
        .unwrap_or_else(div)
    }

    /// The workbench's model status: one chip and the action that moves it on.
    pub(crate) fn model_status_line(
        &self,
        provider: AsrProvider,
        model: Option<&str>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let request = Request::new(provider, model, root);
        match self.engine_issue(provider) {
            Some(issue) => {
                self.engine_obstacle(&format!("import-engine-{}", request.key()), issue, cx)
            }
            None => self.model_surface(&request, false, window, cx),
        }
    }

    /// The one report of a model. `full` adds its name, engine and details, for a
    /// surface where the model is the subject; the workbench shows the rest.
    pub(crate) fn model_surface(
        &self,
        request: &Request,
        full: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = request.key();
        let id = |part: &str| SharedString::from(format!("model-{part}-{key}"));
        let state = &self.settings_ui.model_diagnostics;
        let entry = state.entries.get(&key);
        let preparing = self.is_preparing(request);
        let checking = self.environment.is_none() || entry.is_none_or(|entry| entry.checking);
        let status = entry.and_then(Entry::status);
        let issue = self.engine_issue(request.provider);
        let outcome = self.model_outcome(request);
        let scale = TileScale::Regular;

        let (kind, reading) = self.model_reading(request);
        let chip = h_flex()
            .gap(ICON_GAP)
            .items_center()
            .child(status_chip(kind).child(reading))
            .when(preparing || checking, |row| {
                row.child(spinner(id("spinner"), reading, cx))
            });
        let header = if full {
            scale
                .row()
                .flex_wrap()
                .child(scale.text(
                    id("name"),
                    model_label(&request.model),
                    Some(provider_label(Some(request.provider)).into()),
                ))
                .child(chip)
        } else {
            h_flex().child(chip)
        };

        let mut lines: Vec<(&str, AsyncStatus)> = Vec::new();
        if !preparing && !checking {
            if let Some(Err(error)) = entry.and_then(|entry| entry.result.as_ref()) {
                lines.push((
                    "check-error",
                    AsyncStatus::Failed(course2md::tr!("检查失败：{}", error).into()),
                ));
            }
            if status.is_some_and(|status| status.state == CacheState::Unsupported) {
                lines.push((
                    "unsupported",
                    AsyncStatus::Warning(course2md::tr!("请选择这种识别方式支持的模型").into()),
                ));
            }
            if let Some(error) = status.and_then(|status| status.last_error.as_ref()) {
                lines.push((
                    "last-error",
                    AsyncStatus::Warning(course2md::tr!("未完成：{}", error).into()),
                ));
            }
        }
        if let Some(outcome) = outcome.filter(|_| !preparing) {
            if outcome.error {
                lines.push((
                    "outcome",
                    AsyncStatus::Failed(outcome.message.clone().into()),
                ));
            } else if !outcome.cancelled && full {
                lines.push((
                    "outcome",
                    AsyncStatus::Success(outcome.message.clone().into()),
                ));
            }
        }
        let prepare_allowed = self.environment.is_some()
            && issue.is_none()
            && status.is_some_and(|status| status.can_prepare);
        if self.job.is_some() && !preparing && prepare_allowed {
            lines.push(("job-busy", AsyncStatus::Warning(job_busy().into())));
        }

        let mut actions = h_flex().min_w_0().gap(rem(8.)).items_center().flex_wrap();
        if preparing {
            actions = actions.child(
                material_button(id("pause"), ButtonVariant::Outlined)
                    .icon(symbols::pause())
                    .child(if self.cancelling {
                        course2md::tr!("正在暂停…")
                    } else {
                        course2md::tr!("暂停准备")
                    })
                    .disabled(self.cancelling)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(job) = &this.job {
                            job.cancel();
                            this.cancelling = true;
                            cx.notify();
                        }
                    })),
            );
        } else if prepare_allowed {
            let state = status.map(|status| status.state);
            let (variant, icon) = match state {
                Some(CacheState::Loaded) => (ButtonVariant::Text, symbols::refresh()),
                // The workbench's main action is 开始转换, not this one.
                Some(CacheState::Missing | CacheState::Partial) if full => {
                    (ButtonVariant::Filled, symbols::download())
                }
                _ => (ButtonVariant::Tonal, symbols::download()),
            };
            let label = if outcome.is_some_and(|outcome| outcome.cancelled) {
                course2md::tr!("继续准备")
            } else {
                model_action_label(request.provider, state.as_ref())
            };
            let target = request.clone();
            actions = actions.child(
                material_button(id("prepare"), variant)
                    .icon(icon)
                    .child(label)
                    .disabled(self.job.is_some() || checking)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_model_preparation(target.clone(), cx)
                    })),
            );
        }
        let target = request.clone();
        actions = actions.when(!preparing, |row| {
            row.child(
                material_button(id("recheck"), ButtonVariant::Text)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新检查"))
                    .disabled(checking)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.check_model_request(target.clone(), true, cx)
                    })),
            )
        });
        let open = state.details.contains(&key);
        if full {
            let toggled = key.clone();
            actions = actions.child(
                material_button(id("details"), ButtonVariant::Text)
                    .icon(if open {
                        symbols::keyboard_arrow_up()
                    } else {
                        symbols::info()
                    })
                    .child(if open {
                        course2md::tr!("收起详情")
                    } else {
                        course2md::tr!("模型详情")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let details = &mut this.settings_ui.model_diagnostics.details;
                        if !details.remove(&toggled) {
                            details.insert(toggled.clone());
                        }
                        cx.notify();
                    })),
            );
        }

        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .child(header)
            .when_some(issue.filter(|_| full), |view, issue| {
                view.child(self.engine_obstacle(&id("obstacle"), issue, cx))
            })
            .children(
                lines
                    .into_iter()
                    .filter_map(|(name, line)| ui::async_status(id(name), &line, None, cx)),
            )
            .when(preparing, |view| {
                view.child(self.model_transfers(window, cx))
            })
            .child(actions)
            .when(full, |view| {
                view.child(motion::disclosure(
                    id("details-content"),
                    open,
                    self.model_details(request, entry),
                ))
            })
    }

    /// The running job's transfers. The region keeps its height before the first
    /// sample, so incoming progress does not move the actions.
    fn model_transfers(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let transfers: Vec<_> = self
            .job_events
            .stages
            .iter()
            .filter(|(stage, progress)| {
                stage.is_model_transfer() && progress.has_samples() && !progress.done
            })
            .collect();
        let view = v_flex().w_full().min_w_0().min_h(rem(80.)).gap(rem(12.));
        if transfers.is_empty() {
            return view.child(semantic_label(
                "model-awaiting-progress",
                course2md::tr!("等待准备进度"),
                Icon::from(symbols::download()),
            ));
        }
        view.children(
            transfers
                .into_iter()
                .enumerate()
                .map(|(index, (stage, progress))| {
                    ui::transfer_status(
                        ("model-download-progress", index),
                        activity::model_transfer_phase(stage, progress.phase),
                        &progress.transfer_metrics(stage, true),
                        progress.fraction(),
                        window,
                        cx,
                    )
                }),
        )
    }

    /// Size, files and the last preparation's log.
    fn model_details(&self, request: &Request, entry: Option<&Entry>) -> Div {
        let key = request.key();
        let id = |part: String| SharedString::from(format!("model-{part}-{key}"));
        let scale = TileScale::Regular;
        let mut details = v_flex().w_full().min_w_0().gap(rem(12.));
        let Some(entry) = entry else {
            return details;
        };
        if let Some(status) = entry.status() {
            if status.bytes > 0 {
                details = details.child(scale.support(
                    id("size".into()),
                    course2md::tr!("占用空间 {}", activity::bytes(status.bytes)),
                ));
            }
            for (index, part) in status.parts.iter().enumerate() {
                let path = part.path.clone();
                let open = course2md::tr!("打开 {} 的模型文件夹：{}", part.name, path.display());
                details = details.child(
                    scale
                        .row()
                        .items_start()
                        .child(
                            scale
                                .text(
                                    id(format!("part-{index}")),
                                    part.name.clone(),
                                    Some(path.display().to_string().into()),
                                )
                                .when(!part.missing.is_empty(), |text| {
                                    text.child(scale.support(
                                        id(format!("missing-{index}")),
                                        course2md::tr!(
                                            "缺少或尚未验证：{}",
                                            part.missing.join(course2md::tr!("、"))
                                        ),
                                    ))
                                }),
                        )
                        .when(entry.cache_directories.contains(&path), |row| {
                            row.child(
                                MaterialTooltip::plain(
                                    id(format!("open-tip-{index}")),
                                    course2md::tr!("打开模型文件夹"),
                                )
                                .trigger(
                                    IconButton::new(
                                        id(format!("open-{index}")),
                                        symbols::folder_open(),
                                        open,
                                    )
                                    .on_click(move |_, _, cx| cx.open_with_system(&path)),
                                ),
                            )
                        }),
                );
            }
        }
        // Logs belong to the last model job: shown while it runs and after it ends.
        if self.kind == Kind::Models
            && !self.job_events.logs.is_empty()
            && (self.is_preparing(request) || self.model_outcome(request).is_some())
        {
            let logs = &self.job_events.logs;
            details = details
                .child(section_title(
                    id("log-heading".into()),
                    course2md::tr!("准备日志"),
                ))
                .child(scale.support(
                    id("log".into()),
                    logs[logs.len().saturating_sub(LOG_TAIL)..].join("\n"),
                ));
        }
        details
    }

    /// 模型管理 under the default recognition model: its readiness, another
    /// model being prepared and the last preparation's result.
    pub(super) fn model_management_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let request = self.default_model_request();
        let other = self
            .active_preparation()
            .filter(|active| **active != request)
            .cloned();
        let elsewhere = self
            .settings_ui
            .model_diagnostics
            .outcome
            .as_ref()
            .filter(|outcome| outcome.key != request.key() && other.is_none())
            .map(|outcome| {
                if outcome.error {
                    AsyncStatus::Failed(outcome.message.clone().into())
                } else {
                    AsyncStatus::Success(outcome.message.clone().into())
                }
            });
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(24.))
            .child(self.model_surface(&request, true, window, cx))
            .when_some(other, |view, active| {
                view.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap(SECTION_TITLE_GAP)
                        .child(section_title(
                            "other-active-model",
                            course2md::tr!("正在准备的模型"),
                        ))
                        .child(self.model_surface(&active, true, window, cx)),
                )
            })
            .children(
                elsewhere
                    .and_then(|status| ui::async_status("model-prepare-result", &status, None, cx)),
            )
    }

    /// Device and runtime facts behind local recognition, shown in 诊断详情
    /// and copied with it.
    pub(super) fn hardware_facts(&self) -> Vec<(&'static str, &'static str, String)> {
        let request = self.default_model_request();
        let Some(environment) = &self.environment else {
            return Vec::new();
        };
        let mut facts: Vec<_> = [
            (
                "cpu-device",
                course2md::tr!("CPU 架构"),
                match std::env::consts::ARCH {
                    "aarch64" if cfg!(target_os = "macos") => course2md::tr!("Apple 芯片 (arm64)"),
                    "aarch64" => "ARM (arm64)",
                    "x86_64" => "Intel / AMD (x86_64)",
                    other => other,
                },
            ),
            (
                "apple-device",
                course2md::tr!("Apple 平台"),
                if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                    course2md::tr!("原生运行于 Apple 芯片")
                } else {
                    course2md::tr!("当前平台不适用")
                },
            ),
            (
                "gpu-device",
                "GPU",
                environment.gpu.as_deref().unwrap_or(if environment.llama {
                    course2md::tr!("未检测到")
                } else {
                    course2md::tr!("无法检查（缺少 llama-server）")
                }),
            ),
            (
                "npu-device",
                "Intel NPU",
                if environment.npu_device() {
                    course2md::tr!("已检测到设备")
                } else {
                    course2md::tr!("未检测到设备")
                },
            ),
            (
                "npu-runtime",
                course2md::tr!("NPU 启动器"),
                if environment.npu_runtime() {
                    course2md::tr!("已找到")
                } else {
                    course2md::tr!("未找到 uv 或 Python")
                },
            ),
            (
                "apple-runtime",
                course2md::tr!("Apple 识别程序"),
                if environment.apple {
                    course2md::tr!("已检测到")
                } else {
                    course2md::tr!("不完整")
                },
            ),
        ]
        .into_iter()
        .filter(|(id, _, _)| match *id {
            "apple-device" | "apple-runtime" => cfg!(target_os = "macos"),
            "npu-device" | "npu-runtime" => {
                !cfg!(target_os = "macos") || request.provider == AsrProvider::Npu
            }
            _ => true,
        })
        .map(|(id, label, value)| (id, label, value.to_owned()))
        .collect();
        let available = LOCAL_ENGINES
            .into_iter()
            .filter(|provider| device_issue(*provider, environment).is_none())
            .map(|provider| provider_label(Some(provider)))
            .collect::<Vec<_>>();
        if !available.is_empty() {
            facts.push((
                "available-local-model-providers",
                course2md::tr!("可用识别方式"),
                available.join(course2md::tr!("、")),
            ));
        }
        facts
    }

    pub(super) fn model_hardware_details(&self, cx: &mut Context<Self>) -> Option<Div> {
        let request = self.default_model_request();
        let environment = self.environment.as_ref()?;
        let facts = self.hardware_facts().into_iter().map(|(id, label, value)| {
            ListItem::new(id, label)
                .trailing_text(value)
                .density(ListDensity::Compact)
        });
        let alternatives = LOCAL_ENGINES
            .into_iter()
            .filter(|provider| {
                self.engine_issue(request.provider).is_some()
                    && *provider != request.provider
                    && device_issue(*provider, environment).is_none()
            })
            .map(|provider| {
                material_button(
                    SharedString::from(format!("choose-available-model-{}", provider.as_str())),
                    ButtonVariant::Outlined,
                )
                .icon(symbols::mic())
                .child(course2md::tr!(
                    "默认改用 {}",
                    provider_label(Some(provider))
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut value = this.generation_edit_base();
                    value.select_provider(Some(provider));
                    value.options.asr_model =
                        Some(course2md::config::default_model(provider).into());
                    if this.commit_generation(value, cx) {
                        this.refresh_model_diagnostics(cx);
                    }
                }))
            });
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(SECTION_TITLE_GAP)
                .child(section_title(
                    "model-hardware-heading",
                    course2md::tr!("设备与运行时"),
                ))
                .child(segmented_list(facts))
                .child(h_flex().gap(rem(8.)).flex_wrap().children(alternatives)),
        )
    }
    /// The checked cache state of the default model, when a check finished
    /// and the recognition method can run.
    pub(super) fn default_model_state(&self) -> Option<CacheState> {
        let request = self.default_model_request();
        if request.provider == AsrProvider::Api || self.engine_issue(request.provider).is_some() {
            return None;
        }
        self.settings_ui
            .model_diagnostics
            .entries
            .get(&request.key())
            .and_then(Entry::status)
            .map(|status| status.state)
    }
    /// One-line readiness conclusion for the generation group main flow; the full
    /// panel stays behind 技术详情.
    pub(super) fn default_model_conclusion(&self) -> (bool, String) {
        let request = self.default_model_request();
        if request.provider == AsrProvider::Api {
            return (true, course2md::tr!("使用语音服务识别").into());
        }
        if self.engine_issue(request.provider).is_some() {
            return (
                false,
                course2md::tr!("所选识别方式暂不可用，可在模型管理中查看原因").into(),
            );
        }
        let entry = self
            .settings_ui
            .model_diagnostics
            .entries
            .get(&request.key());
        if entry.is_none_or(|entry| entry.checking) {
            return (true, course2md::tr!("正在检查识别模型…").into());
        }
        match entry.and_then(|entry| entry.result.as_ref()) {
            Some(Err(_)) => (
                false,
                course2md::tr!("模型检查未完成，可在模型管理中查看原因并重试").into(),
            ),
            Some(Ok(status)) => match status.state {
                state @ (CacheState::Loaded | CacheState::Cached) => {
                    (true, cache_state_label(state).1.into())
                }
                CacheState::Missing | CacheState::Partial => {
                    (true, course2md::tr!("首次识别时会自动下载所选模型").into())
                }
                CacheState::Unsupported => (
                    false,
                    course2md::tr!("当前识别方式不支持所选模型，请选择其他模型").into(),
                ),
            },
            None => (true, course2md::tr!("尚未检查识别模型").into()),
        }
    }
}
