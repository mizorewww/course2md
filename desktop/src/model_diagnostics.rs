//! Model evidence is loaded in the background; preparation uses the existing model job.
use super::{
    DetailGroup, settings_detail_group, settings_detail_row, settings_status_row, settings_value,
};
use crate::theme::*;
use crate::*;
use course2md::{
    config::{AsrProvider, canonical_model, model_dir_from, model_label},
    models::status::{CacheState, LocalModelStatus},
};
use gpui_component::button::*;
use std::path::Path;

/// One vocabulary for cache states across settings, setup and diagnostics.
pub(crate) fn cache_state_label(state: CacheState) -> (BadgeKind, &'static str) {
    match state {
        CacheState::Loaded => (BadgeKind::Success, course2md::tr!("已就绪")),
        CacheState::Cached => (BadgeKind::Success, course2md::tr!("已下载")),
        CacheState::Partial => (BadgeKind::Warning, course2md::tr!("未下载完整")),
        CacheState::Missing => (BadgeKind::Neutral, course2md::tr!("未下载")),
        CacheState::Unsupported => (BadgeKind::Warning, course2md::tr!("不支持")),
    }
}

fn model_status_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl IntoElement,
    divider: bool,
) -> Div {
    settings_status_row(id, label, value)
        .min_h(px(56.))
        .when(divider, |row| {
            row.border_b_1().border_color(color(HAIRLINE))
        })
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
        let model = model
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(course2md::config::DEFAULT_ASR_MODEL);
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
    cache_directories: std::collections::BTreeSet<PathBuf>,
}
impl Entry {
    fn result(&self) -> Option<&Result<LocalModelStatus, String>> {
        self.result.as_ref()
    }
    fn checking(&self) -> bool {
        self.checking
    }
}
/// How the last preparation ended, kept with the request it belongs to.
struct Outcome {
    key: String,
    message: String,
    error: bool,
    cancelled: bool,
}
#[derive(Default)]
pub(super) struct State {
    entries: BTreeMap<String, Entry>,
    /// The request the running model job is preparing; taken when it exits.
    preparing: Option<Request>,
    outcome: Option<Outcome>,
    details: bool,
    cache_details: std::collections::BTreeSet<String>,
}

/// A read-only view for setup. Checks and downloads keep the same ownership as Settings.
pub(crate) struct ModelSetupSnapshot {
    pub(crate) checking: bool,
    pub(crate) result: Option<Result<LocalModelStatus, String>>,
    pub(crate) device_issue: Option<String>,
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

/// Choice id of an engine; automatic selection is the empty id.
pub(crate) use crate::provider_id as engine_id;
pub(crate) fn engine_from_id(id: &str) -> Option<AsrProvider> {
    LOCAL_ENGINES
        .into_iter()
        .find(|provider| provider.as_str() == id)
}
pub(crate) fn engine_icon(provider: Option<AsrProvider>) -> Icon {
    match provider {
        None => icons::auto_fix(),
        Some(AsrProvider::Coreml) => icons::computer(),
        Some(AsrProvider::Gpu) => icons::grid_view(),
        Some(AsrProvider::Cpu) => icons::storage(),
        Some(AsrProvider::Npu) => icons::sparkles(),
        Some(AsrProvider::Api) => icons::cloud(),
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
pub(crate) fn model_action_label(
    provider: AsrProvider,
    state: Option<&CacheState>,
) -> &'static str {
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
/// The one classification of why a local engine cannot run here. Messages,
/// repair actions and engine choices all derive from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceIssue {
    Engine,
    AppleHardware,
    AppleRuntime,
    LlamaMissing,
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
            Self::AppleRuntime | Self::LlamaMissing | Self::NpuRuntime => {
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
            Self::LlamaMissing => course2md::tr!("缺少 GPU/CPU 识别组件 llama.cpp，安装后重新检查"),
            Self::GpuAbsent => course2md::tr!("未检测到可用显卡，可改用 CPU"),
            Self::NpuDevice => course2md::tr!("未检测到 Intel NPU，请改用其他方式"),
            Self::NpuRuntime => course2md::tr!("Intel NPU 识别需要 Python（uv），安装后重新检查"),
        }
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
        let key = Request::new(provider, model, root).key();
        let state = &self.settings_ui.model_diagnostics;
        let entry = state.entries.get(&key);
        let outcome = state.outcome.as_ref().filter(|outcome| outcome.key == key);
        ModelSetupSnapshot {
            checking: entry.is_none_or(Entry::checking),
            result: entry.and_then(|entry| entry.result.clone()),
            device_issue: self.model_device_issue(provider),
            preparing: self.is_preparing(&key),
            notice: outcome.map(|outcome| (outcome.message.clone(), outcome.error)),
            cancelled: outcome.is_some_and(|outcome| outcome.cancelled),
        }
    }

    /// The request a running model job is preparing.
    fn active_preparation(&self) -> Option<&Request> {
        (self.kind == Kind::Models && self.job.is_some())
            .then_some(self.settings_ui.model_diagnostics.preparing.as_ref())
            .flatten()
    }
    fn is_preparing(&self, key: &str) -> bool {
        self.active_preparation()
            .is_some_and(|active| active.key() == key)
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
        let request = Request::new(provider, model, root);
        if request.provider == AsrProvider::Api {
            return;
        }
        if self
            .settings_ui
            .model_diagnostics
            .entries
            .contains_key(&request.key())
        {
            return;
        }
        self.check_model_request(request, false, cx);
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
        let state = &mut self.settings_ui.model_diagnostics;
        state.preparing = Some(request);
        state.outcome = None;
        state.details = false;
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
        let name = course2md::config::model_label(&request.model);
        let message = if success {
            course2md::tr!("{} 已准备完成", name)
        } else if cancelled {
            course2md::tr!("{} 的准备已暂停", name)
        } else {
            course2md::tr!(
                "{} 的准备未完成：{}",
                name,
                self.task_error
                    .as_deref()
                    .unwrap_or(course2md::tr!("准备失败"))
            )
        };
        let error = !success && !cancelled;
        let cancelled = cancelled && !success;
        self.onboarding.apply_model_preparation_result(
            &request.key(),
            (message.clone(), error),
            cancelled,
        );
        self.settings_ui.model_diagnostics.outcome = Some(Outcome {
            key: request.key(),
            message,
            error,
            cancelled,
        });
        self.refresh_model_diagnostics(cx);
        cx.notify();
    }
    /// Why the engine cannot run here; `None` while detection is pending.
    pub(crate) fn engine_issue(&self, provider: AsrProvider) -> Option<DeviceIssue> {
        device_issue(provider, self.environment.as_ref()?)
    }
    fn model_device_issue(&self, provider: AsrProvider) -> Option<String> {
        self.engine_issue(provider)
            .map(|issue| issue.message().to_owned())
    }
    fn model_runtime_repair(&self, provider: AsrProvider, key: &str) -> Div {
        let mut view = v_flex().w_full().min_w_0().gap_2();
        let issue = self.engine_issue(provider);
        if matches!(issue, Some(DeviceIssue::Engine | DeviceIssue::AppleRuntime)) {
            return view.child(
                control(SharedString::from(format!("model-reinstall-{key}")))
                    .settings_text()
                    .icon(icons::download())
                    .label(course2md::tr!("下载完整应用"))
                    .self_start()
                    .on_click(|_, _, cx| {
                        cx.open_url("https://github.com/mizorewww/course2md/releases")
                    }),
            );
        }
        if issue == Some(DeviceIssue::LlamaMissing) {
            view = view.child(
                control(SharedString::from(format!(
                    "model-llama-install-help-{key}"
                )))
                .settings_text()
                .icon(icons::external_link())
                .label(course2md::tr!("查看 llama.cpp 安装说明"))
                .self_start()
                .on_click(|_, _, cx| {
                    cx.open_url("https://github.com/ggml-org/llama.cpp#quick-start")
                }),
            );
            if cfg!(target_os = "macos") {
                view = view
                    .child(
                        settings_value(
                            SharedString::from(format!("model-llama-install-command-{key}")),
                            course2md::tr!("需要 Homebrew；安装后重新打开应用"),
                        )
                        .text_sm(),
                    )
                    .child(
                        control(SharedString::from(format!(
                            "model-copy-llama-install-{key}"
                        )))
                        .settings_text()
                        .icon(icons::content_copy())
                        .label(course2md::tr!("复制安装命令"))
                        .self_start()
                        .on_click(|_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                "brew install llama.cpp".into(),
                            ))
                        }),
                    );
            } else {
                view = view.child(
                    settings_value(
                        SharedString::from(format!("model-llama-path-help-{key}")),
                        course2md::tr!(
                            "安装 llama.cpp，确保可在终端直接运行 llama-server，然后重新打开应用"
                        ),
                    )
                    .text_sm(),
                );
            }
        }
        if issue == Some(DeviceIssue::NpuRuntime) {
            view = view.child(
                control(SharedString::from(format!(
                    "model-python-install-help-{key}"
                )))
                .settings_text()
                .icon(icons::external_link())
                .label(course2md::tr!("查看 uv 安装说明"))
                .self_start()
                .on_click(|_, _, cx| {
                    cx.open_url("https://docs.astral.sh/uv/getting-started/installation/")
                }),
            );
        }
        view
    }
    pub fn model_readiness_panel(
        &self,
        provider: AsrProvider,
        model: Option<&str>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        self.model_readiness_panel_with(provider, model, root, true, window, cx)
    }
    /// `provider_row`：是否展示「本机引擎」行。工作台的选择器已是方式事实来源，传 false；
    /// 设置页的模型管理面板传 true（面板独立承担状态摘要）。
    pub fn model_readiness_panel_with(
        &self,
        provider: AsrProvider,
        model: Option<&str>,
        root: &Path,
        provider_row: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let request = Request::new(provider, model, root);
        let key = request.key();
        let entry = self.settings_ui.model_diagnostics.entries.get(&key);
        let checking = entry.is_none_or(Entry::checking);
        let status = entry
            .and_then(Entry::result)
            .and_then(|result| result.as_ref().ok());
        let (mut view, cache_action, cache_details) =
            self.model_status_section(provider, &request, provider_row, cx);
        let (actions, extras) = self.model_prepare_actions(&request, checking, status, window, cx);
        for extra in extras {
            view = view.child(extra);
        }
        view.child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(actions.w_auto())
                .children(cache_action),
        )
        .when_some(cache_details, |view, details| view.child(details))
    }

    /// Status rows: engine, model name, device issue with repair guidance,
    /// check progress, cache state summary and expandable cache details.
    fn model_status_section(
        &self,
        provider: AsrProvider,
        request: &Request,
        provider_row: bool,
        cx: &mut Context<Self>,
    ) -> (Div, Option<Div>, Option<AnyElement>) {
        let key = request.key();
        let entry = self.settings_ui.model_diagnostics.entries.get(&key);
        let device_issue = self.model_device_issue(provider);
        let mut cache_details = None;
        let mut cache_action = None;
        let mut view = v_flex().w_full().min_w_0();
        if provider_row {
            view = view.child(model_status_row(
                SharedString::from(format!("model-provider-label-{key}")),
                course2md::tr!("本机引擎"),
                settings_value(
                    SharedString::from(format!("model-provider-{key}")),
                    crate::provider_label(Some(provider)),
                )
                .w_auto(),
                true,
            ));
        }
        view = view.child(model_status_row(
            SharedString::from(format!("model-name-label-{key}")),
            course2md::tr!("模型"),
            settings_value(
                SharedString::from(format!("model-name-{key}")),
                model_label(&request.model),
            )
            .w_auto(),
            true,
        ));
        if let Some(issue) = &device_issue {
            view = view
                .child(settings_detail_row(
                    SharedString::from(format!("model-device-label-{key}")),
                    course2md::tr!("设备状态"),
                    settings_value(
                        SharedString::from(format!("model-device-problem-{key}")),
                        issue.clone(),
                    )
                    .text_color(color(DANGER)),
                ))
                .child(self.model_runtime_repair(provider, &key));
        }
        let checking = entry.is_none_or(Entry::checking);
        let status = entry
            .and_then(Entry::result)
            .and_then(|result| result.as_ref().ok());
        if checking {
            view = view.child(model_status_row(
                SharedString::from(format!("model-checking-label-{key}")),
                course2md::tr!("模型状态"),
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(crate::motion::spinner(
                        SharedString::from(format!("model-check-spinner-{key}")),
                        cx,
                    ))
                    .child(
                        settings_value(
                            SharedString::from(format!("model-checking-{key}")),
                            course2md::tr!("正在检查模型…"),
                        )
                        .w_auto()
                        .role(Role::Status),
                    ),
                false,
            ));
        }
        if let Some(Err(error)) = entry.and_then(Entry::result) {
            view = view.child(settings_detail_row(
                SharedString::from(format!("model-check-error-label-{key}")),
                course2md::tr!("检查结果"),
                settings_value(
                    SharedString::from(format!("model-check-error-{key}")),
                    course2md::tr!("检查失败：{}", error),
                )
                .text_color(color(DANGER)),
            ));
        }
        if let Some(status) = status {
            let (kind, label, description) = if self.is_preparing(&key) {
                (BadgeKind::Progress, course2md::tr!("准备中"), None)
            } else {
                let (kind, label) = cache_state_label(status.state);
                let description = (status.state == CacheState::Unsupported)
                    .then(|| course2md::tr!("请选择这种识别方式支持的模型"));
                (kind, label, description)
            };
            view = view.child(model_status_row(
                SharedString::from(format!("model-state-label-{key}")),
                course2md::tr!("模型状态"),
                badge(kind).text_size(TEXT_SETTINGS_AUX).child(label),
                false,
            ));
            if let Some(description) = description {
                view = view.child(
                    theme::supporting_info(
                        SharedString::from(format!("model-state-{key}")),
                        description,
                    )
                    .text_size(TEXT_SETTINGS_AUX),
                );
            }
            let detail_key = key.clone();
            let cache_open = self
                .settings_ui
                .model_diagnostics
                .cache_details
                .contains(&key);
            let mut cache = v_flex().w_full().min_w_0().gap_3();
            for (index, part) in status.parts.iter().enumerate() {
                let path = part.path.clone();
                let mut part_details = v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(
                        settings_value(
                            SharedString::from(format!("model-cache-path-{key}-{index}")),
                            part.path.display().to_string(),
                        )
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(GRAY)),
                    )
                    .when(
                        entry.is_some_and(|entry| entry.cache_directories.contains(&path)),
                        |view| {
                            view.child(
                                quiet(SharedString::from(format!(
                                    "open-model-cache-{key}-{index}"
                                )))
                                .settings_text()
                                .icon(icons::folder_open())
                                .label(course2md::tr!("打开模型文件夹"))
                                .accessibility_label(course2md::tr!(
                                    "打开 {} 的模型文件夹：{}",
                                    part.name,
                                    part.path.display()
                                ))
                                .self_end()
                                .on_click(move |_, _, cx| cx.open_with_system(&path)),
                            )
                        },
                    );
                if !part.missing.is_empty() {
                    part_details = part_details.child(settings_value(
                        SharedString::from(format!("model-missing-files-{key}-{index}")),
                        course2md::tr!(
                            "缺少或尚未验证：{}",
                            part.missing.join(course2md::tr!("、"))
                        ),
                    ));
                }
                cache = cache.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .p_4()
                        .bg(color(SURFACE))
                        .border_1()
                        .border_color(color(CARD_LINE))
                        .rounded(RADIUS_CARD)
                        .child(
                            semantic_label(
                                SharedString::from(format!("model-cache-part-label-{key}-{index}")),
                                part.name.clone(),
                                icons::storage(),
                            )
                            .w_full()
                            .min_w_0(),
                        )
                        .child(part_details),
                );
            }
            // One unit in the action row, which does the wrapping.
            cache_action = Some(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .child(
                        quiet(SharedString::from(format!("model-cache-details-{key}")))
                            .settings_text()
                            .ml(-crate::theme::QUIET_INSET)
                            .icon(icons::folder_open())
                            .label(if cache_open {
                                course2md::tr!("收起模型文件")
                            } else {
                                course2md::tr!("查看模型文件")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this
                                    .settings_ui
                                    .model_diagnostics
                                    .cache_details
                                    .remove(&detail_key)
                                {
                                    this.settings_ui
                                        .model_diagnostics
                                        .cache_details
                                        .insert(detail_key.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .when(status.bytes > 0, |row| {
                        row.child(
                            settings_value(
                                SharedString::from(format!("model-cached-size-{key}")),
                                course2md::tr!("占用空间 {}", crate::activity::bytes(status.bytes)),
                            )
                            // Sized by its text, not the row it wraps in.
                            .w_auto()
                            .whitespace_nowrap()
                            .text_size(TEXT_SETTINGS_AUX)
                            .text_color(color(GRAY)),
                        )
                    }),
            );
            cache_details = Some(crate::motion::disclosure(
                SharedString::from(format!("model-cache-content-{key}")),
                cache_open,
                cache,
            ));
            if let Some(error) = &status.last_error {
                view = view.child(settings_detail_row(
                    SharedString::from(format!("model-last-error-label-{key}")),
                    course2md::tr!("上次准备"),
                    settings_value(
                        SharedString::from(format!("model-last-error-{key}")),
                        course2md::tr!("未完成：{}", error),
                    )
                    .text_color(color(DANGER)),
                ));
            }
        }
        (view, cache_action, cache_details)
    }

    /// Action row (recheck / pause / prepare) and the transient rows it owns:
    /// prepare progress, download transfers, and queue/network-scope notes.
    fn model_prepare_actions(
        &self,
        request: &Request,
        checking: bool,
        status: Option<&LocalModelStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Div, Vec<AnyElement>) {
        let mut extras: Vec<AnyElement> = Vec::new();
        let request = request.clone();
        let provider = request.provider;
        let key = &request.key();
        let device_issue = self.engine_issue(provider);
        let active = self.is_preparing(key);
        let request_for_check = request.clone();
        let recheck = outline_pill(SharedString::from(format!("recheck-model-{key}")))
            .settings_text()
            .icon(icons::refresh())
            .label(course2md::tr!("重新检查模型"))
            .disabled(checking || active)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.check_model_request(request_for_check.clone(), true, cx)
            }));
        let mut actions = h_flex().w_full().min_w_0().gap_2().flex_wrap();

        let prepare_allowed = self.environment.is_some()
            && device_issue.is_none()
            && status.is_some_and(|status| status.can_prepare);
        if active {
            extras.push(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(crate::motion::spinner("model-prepare-spinner", cx))
                    .child(
                        settings_value(
                            "active-model-prepare",
                            course2md::tr!("正在准备 {}", model_label(&request.model)),
                        )
                        .role(Role::Status),
                    )
                    .into_any_element(),
            );
            for (index, (stage, progress)) in self
                .progress
                .iter()
                .filter(|(stage, progress)| {
                    stage.is_model_transfer() && progress.has_samples() && !progress.done
                })
                .enumerate()
            {
                extras.push(
                    crate::motion::transfer_status(
                        ("model-download-progress", index),
                        crate::activity::model_transfer_phase(stage, progress.phase),
                        &progress.transfer_metrics(stage, true),
                        progress.fraction(),
                        window,
                        cx,
                    )
                    .into_any_element(),
                );
            }
            actions = actions.child(
                control("stop-model-preparation")
                    .settings_text()
                    .icon(icons::pause())
                    .label(if self.cancelling {
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
            let loaded = status.is_some_and(|status| status.state == CacheState::Loaded);
            let label = model_action_label(provider, status.map(|status| &status.state));
            actions = actions.child(
                control(SharedString::from(format!("prepare-model-{key}")))
                    .settings_text()
                    .icon(if loaded {
                        icons::refresh()
                    } else {
                        icons::download()
                    })
                    .when(loaded, |button| button.ghost())
                    .when(!loaded, |button| button.primary())
                    .label(label)
                    .accessibility_label(course2md::tr!(
                        "{}：{} · {}",
                        label,
                        crate::provider_label(Some(provider)),
                        model_label(&request.model)
                    ))
                    .disabled(self.job.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_model_preparation(request.clone(), cx)
                    })),
            );
            if self.job.is_some() && !loaded {
                extras.push(
                    settings_value(
                        SharedString::from(format!("model-waits-for-job-{key}")),
                        job_busy(),
                    )
                    .text_sm()
                    .into_any_element(),
                );
            }
        }
        (actions.child(recheck), extras)
    }

    /// 模型管理 under the default recognition model: its readiness, another
    /// model being prepared, the last preparation's result and its log.
    pub(super) fn model_management_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let request = self.default_model_request();
        let mut view = v_flex()
            .w_full()
            .min_w_0()
            .gap_6()
            .child(self.model_readiness_panel(
                request.provider,
                Some(&request.model),
                &request.root,
                window,
                cx,
            ));
        if let Some(active) = self.active_preparation()
            && active.key() != request.key()
        {
            view = view.child(
                settings_detail_group(
                    "other-active-model",
                    icons::storage(),
                    course2md::tr!("正在准备的模型"),
                )
                .after(self.model_readiness_panel(
                    active.provider,
                    Some(&active.model),
                    &active.root,
                    window,
                    cx,
                )),
            );
        }
        let outcome = self.settings_ui.model_diagnostics.outcome.as_ref();
        if let Some(outcome) = outcome {
            view = view.child(
                settings_value("model-prepare-result", outcome.message.clone())
                    .role(Role::Status)
                    .text_color(color(if outcome.error { DANGER } else { INK })),
            );
        }
        // Logs belong to the last model job: shown while it runs and after it ends.
        if self.kind == Kind::Models
            && !self.logs.is_empty()
            && (self.job.is_some() || outcome.is_some())
        {
            view = view.child(
                control("model-preparation-details")
                    .settings_text()
                    .icon(icons::info())
                    .ghost()
                    .label(if self.settings_ui.model_diagnostics.details {
                        course2md::tr!("收起准备详情")
                    } else {
                        course2md::tr!("查看准备详情")
                    })
                    .self_start()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_ui.model_diagnostics.details =
                            !this.settings_ui.model_diagnostics.details;
                        cx.notify();
                    })),
            );
            if self.settings_ui.model_diagnostics.details {
                view = view.child(
                    settings_detail_group(
                        "model-preparation-log-heading",
                        icons::task(),
                        course2md::tr!("准备日志"),
                    )
                    .child(
                        settings_value("model-preparation-log", {
                            let tail = self.logs.len().saturating_sub(LOG_TAIL);
                            let lines: Vec<&str> =
                                self.logs.range(tail..).map(String::as_str).collect();
                            lines.join("\n")
                        })
                        .text_size(TEXT_SETTINGS_AUX),
                    ),
                );
            }
        }
        view
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
            .map(|provider| crate::provider_label(Some(provider)))
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

    pub(super) fn model_hardware_details(&self, cx: &mut Context<Self>) -> Option<DetailGroup> {
        let request = self.default_model_request();
        let environment = self.environment.as_ref()?;
        let hardware = self.hardware_facts().into_iter().fold(
            settings_detail_group(
                "model-hardware-heading",
                icons::computer(),
                course2md::tr!("设备与运行时"),
            ),
            |group, (id, label, value)| {
                group.row(
                    SharedString::from(format!("{id}-label")),
                    label,
                    accessible_text(id, value),
                )
            },
        );
        if self.model_device_issue(request.provider).is_none() {
            return Some(hardware);
        }
        let alternatives = LOCAL_ENGINES.into_iter().filter(|provider| {
            *provider != request.provider && device_issue(*provider, environment).is_none()
        });
        Some(
            hardware.after(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(alternatives.map(|provider| {
                        control(SharedString::from(format!(
                            "choose-available-model-{}",
                            provider.as_str()
                        )))
                        .settings_text()
                        .icon(icons::mic())
                        .label(course2md::tr!(
                            "默认改用 {}",
                            crate::provider_label(Some(provider))
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut value = this.generation_edit_base();
                            value.select_provider(Some(provider));
                            value.options.asr_model =
                                Some(course2md::config::DEFAULT_ASR_MODEL.into());
                            if this.commit_generation(value, cx) {
                                this.refresh_model_diagnostics(cx);
                            }
                        }))
                    })),
            ),
        )
    }
    /// One-line readiness conclusion for the generation group main flow; the full
    /// panel stays behind 技术详情.
    pub(super) fn default_model_conclusion(&self) -> (bool, String) {
        let request = self.default_model_request();
        if request.provider == AsrProvider::Api {
            return (true, course2md::tr!("使用语音服务识别").into());
        }
        if self.model_device_issue(request.provider).is_some() {
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
        if entry.is_none_or(Entry::checking) {
            return (true, course2md::tr!("正在检查识别模型…").into());
        }
        if entry.and_then(Entry::result).is_some_and(Result::is_err) {
            return (
                false,
                course2md::tr!("模型检查未完成，可在模型管理中查看原因并重试").into(),
            );
        }
        match entry
            .and_then(Entry::result)
            .and_then(|result| result.as_ref().ok())
            .map(|status| &status.state)
        {
            Some(state @ (CacheState::Loaded | CacheState::Cached)) => {
                (true, cache_state_label(*state).1.into())
            }
            Some(CacheState::Missing | CacheState::Partial) => {
                (true, course2md::tr!("首次识别时会自动下载所选模型").into())
            }
            Some(CacheState::Unsupported) => (
                false,
                course2md::tr!("当前识别方式不支持所选模型，请选择其他模型").into(),
            ),
            None => (true, course2md::tr!("尚未检查识别模型").into()),
        }
    }
}
