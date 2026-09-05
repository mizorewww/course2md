#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod activity;
mod backend;
mod icons;
mod library_ui;
mod onboarding;
mod organize;
mod source;
mod theme;
mod views;
use backend::{Completed, Course, Event, Job};
use gpui::{prelude::*, *};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    *,
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    New,
    Task,
    Library,
    Settings,
    Result,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Convert,
    Doctor,
    Models,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Source,
    FolderName,
    Output,
    Search,
    AsrUrl,
    AsrKey,
    AsrModel,
    LlmUrl,
    LlmKey,
    LlmModel,
}
const PROVIDERS: [(&str, &str); 6] = [
    ("", "自动"),
    ("coreml", "Apple 原生"),
    ("gpu", "GPU"),
    ("cpu", "CPU"),
    ("npu", "Intel NPU"),
    ("api", "云端 API"),
];
const SOURCES: [(&str, &str); 3] = [
    ("auto", "字幕优先"),
    ("subtitle", "仅字幕"),
    ("asr", "语音识别"),
];

#[derive(Clone)]
struct ConversionOptions {
    provider: usize,
    source_mode: usize,
    llm: bool,
    keep_video: bool,
    resume: bool,
    formats: [bool; 3],
}

impl ConversionOptions {
    fn from_config(config: &course2md::settings::ConfigFile) -> Self {
        let provider = config
            .defaults
            .provider
            .map(|p| {
                PROVIDERS
                    .iter()
                    .position(|(id, _)| *id == p.as_str())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let source_mode = match config.defaults.transcript_source.unwrap_or_default() {
            course2md::config::TranscriptSource::Auto => 0,
            course2md::config::TranscriptSource::Subtitle => 1,
            course2md::config::TranscriptSource::Asr => 2,
        };
        let llm = config.llm.enabled;
        let keep_video = config.defaults.keep_video.unwrap_or(false);
        let resume = config.defaults.resume.unwrap_or(false);
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
            keep_video,
            resume,
            formats,
        }
    }
}

struct Desktop {
    online: bool,
    last_source_input: String,
    completed_source: Option<String>,
    source_preview: Option<source::Source>,
    preview_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    preview_generation: u64,
    preview_workers: usize,
    preview_error: Option<String>,
    library: organize::Library,
    library_root: PathBuf,
    folder_filter: Option<u64>, // None = all; 0 = unfiled
    target_folder: Option<u64>,
    folder_editor: Option<Option<u64>>,
    delete_folder: Option<u64>,
    task_destination: Option<(PathBuf, Option<u64>, source::Source)>,
    page: Page,
    result_origin: Page,
    result_tab: usize,
    settings_tab: usize,
    show_options: bool,
    show_logs: bool,
    setup_open: bool,
    show_engine_details: bool,
    environment: Option<backend::Environment>,
    scrolls: [ScrollHandle; 5],
    inputs: BTreeMap<Field, Entity<InputState>>,
    config: course2md::settings::ConfigFile,
    config_error: bool,
    task_options: ConversionOptions,
    settings_options: ConversionOptions,
    job: Option<Job>,
    kind: Kind,
    cancelling: bool,
    closing: bool,
    task_status: String,
    progress: BTreeMap<String, activity::Activity>,
    settings_snapshot: course2md::settings::ConfigFile,
    settings_deadline: Option<Instant>,
    settings_status: String,
    system_titlebar: bool,
    desktop_settings: course2md::settings::DesktopSettings,
    last_tick: Instant,
    logs: VecDeque<String>,
    pending_done: Option<Completed>,
    completed: Option<Completed>,
    courses: Vec<Course>,
    loading: bool,
    preview: Option<backend::Preview>,
    message: Option<String>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

actions!(course2md_desktop, [Quit, OpenSettings]);

impl Desktop {
    fn editing_options(&self) -> &ConversionOptions {
        if self.page == Page::Settings || self.setup_open {
            &self.settings_options
        } else {
            &self.task_options
        }
    }
    fn editing_options_mut(&mut self) -> &mut ConversionOptions {
        if self.page == Page::Settings || self.setup_open {
            &mut self.settings_options
        } else {
            &mut self.task_options
        }
    }

    fn request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.settings_deadline.take().is_some() {
            self.save_settings(cx);
        }
        if self.preview_workers > 0 {
            if let Some(cancel) = &self.preview_cancel {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.closing = true;
        }
        if let Some(job) = &self.job {
            job.cancel();
            self.closing = true;
            self.cancelling = true;
            self.task_status = "正在停止任务并关闭…".into();
            cx.notify();
            false
        } else {
            self.preview_workers == 0
        }
    }
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (config, message, config_error) = match course2md::settings::load() {
            Ok(config) => (config, None, false),
            Err(error) => (
                Default::default(),
                Some(format!("配置文件读取失败：{error:#}。请先修正配置文件。")),
                true,
            ),
        };
        let output = config
            .defaults
            .out
            .clone()
            .map(course2md::config::expand_tilde)
            .unwrap_or_else(backend::default_output);
        let fields = [
            (
                Field::Source,
                "粘贴 YouTube 或 Bilibili 视频链接",
                String::new(),
            ),
            (
                Field::Output,
                "课程笔记保存位置",
                output.display().to_string(),
            ),
            (Field::Search, "搜索课程标题", String::new()),
            (Field::FolderName, "文件夹名称", String::new()),
            (
                Field::AsrUrl,
                "https://api.example.com/v1",
                config.asr_api.base_url.clone(),
            ),
            (Field::AsrKey, "API Key", config.asr_api.api_key.clone()),
            (Field::AsrModel, "转写模型", config.asr_api.model.clone()),
            (
                Field::LlmUrl,
                "https://api.example.com/v1",
                config.llm.base_url.clone(),
            ),
            (Field::LlmKey, "API Key", config.llm.api_key.clone()),
            (Field::LlmModel, "模型名称", config.llm.model.clone()),
        ];
        let inputs: BTreeMap<_, _> = fields
            .into_iter()
            .map(|(field, placeholder, value)| {
                (
                    field,
                    cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .default_value(value)
                            .masked(matches!(field, Field::AsrKey | Field::LlmKey))
                    }),
                )
            })
            .collect();
        let mut subscriptions: Vec<Subscription> = inputs
            .iter()
            .map(|(field, input)| {
                let field = *field;
                cx.observe(input, move |this: &mut Self, _, cx| {
                    if field == Field::Source {
                        let value = this.value(Field::Source, cx);
                        if value != this.last_source_input {
                            this.last_source_input = value;
                            this.invalidate_source();
                        }
                    }
                    if field == Field::Search {
                        this.scrolls[Page::Library as usize].set_offset(point(px(0.), px(0.)));
                    }
                    cx.notify();
                })
            })
            .collect();
        subscriptions.push(cx.subscribe(
            &inputs[&Field::Source],
            |this: &mut Self, _, event, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && this.page == Page::New
                    && this.online
                    && this.preview_cancel.is_none()
                {
                    this.inspect_source(cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &inputs[&Field::FolderName],
            |this: &mut Self, _, event, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_folder(cx);
                }
            },
        ));
        let options = ConversionOptions::from_config(&config);
        let poll = cx.spawn_in(window, async |this, cx| {
            let mut busy = false;
            loop {
                smol::Timer::after(Duration::from_millis(if busy { 100 } else { 500 })).await;
                match this.update_in(cx, |this, _, cx| {
                    this.poll(cx);
                    if this
                        .settings_deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        this.settings_deadline = None;
                        this.save_settings(cx);
                    }
                    this.job.is_some() || this.settings_deadline.is_some()
                }) {
                    Ok(active) => busy = active,
                    Err(_) => break,
                }
            }
        });
        let mut this = Self {
            page: Page::Library,
            online: true,
            last_source_input: String::new(),
            completed_source: None,
            source_preview: None,
            preview_cancel: None,
            preview_generation: 0,
            preview_workers: 0,
            preview_error: None,
            library: Default::default(),
            library_root: output,
            folder_filter: None,
            target_folder: None,
            folder_editor: None,
            delete_folder: None,
            task_destination: None,
            result_origin: Page::Library,
            result_tab: 0,
            settings_tab: 0,
            show_options: false,
            show_logs: false,
            setup_open: false,
            show_engine_details: false,
            environment: None,
            scrolls: std::array::from_fn(|_| ScrollHandle::new()),
            inputs,
            settings_snapshot: config.clone(),
            desktop_settings: config.desktop.clone(),
            system_titlebar: config.desktop.system_titlebar,
            settings_deadline: None,
            settings_status: String::new(),
            last_tick: Instant::now(),
            config,
            config_error,
            task_options: options.clone(),
            settings_options: options,
            job: None,
            kind: Kind::Convert,
            cancelling: false,
            closing: false,
            task_status: "尚无运行中的任务".into(),
            progress: BTreeMap::new(),
            logs: VecDeque::new(),
            pending_done: None,
            completed: None,
            courses: vec![],
            loading: false,
            preview: None,
            message,
            _subscriptions: subscriptions,
            _poll: poll,
        };
        this.settings_snapshot = this.edited_settings(cx);
        cx.set_reduce_motion(this.desktop_settings.reduce_motion);
        this.refresh_environment(cx);
        this.refresh_library(cx);
        this
    }
    fn refresh_environment(&mut self, cx: &mut Context<Self>) {
        self.environment = None;
        let task = cx
            .background_executor()
            .spawn(async { backend::Environment::detect() });
        cx.spawn(async move |this, cx| {
            let environment = task.await;
            let _ = this.update(cx, |this, cx| {
                this.environment = Some(environment);
                if this.settings_options.provider == 0
                    && let Some(choice) = this
                        .engine_choices(cx)
                        .into_iter()
                        .find(|choice| (1..=4).contains(&choice.index) && choice.selectable)
                {
                    this.settings_options.provider = choice.index;
                    if this.job.is_none() && this.task_options.provider == 0 {
                        this.task_options.provider = choice.index;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        if page == Page::New && self.page == Page::Library {
            self.target_folder = self.folder_filter.filter(|id| *id != 0);
        }
        if self.page != page {
            self.show_engine_details = false;
        }
        self.page = page;
        self.message = None;
        if page == Page::Library {
            self.refresh_library(cx);
        }
        cx.notify();
    }
    fn value(&self, field: Field, cx: &App) -> String {
        self.inputs[&field].read(cx).value().trim().to_string()
    }
    fn input(&self, field: Field, label: &'static str) -> impl IntoElement {
        v_flex()
            .gap_2()
            .w_full()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(Input::new(&self.inputs[&field]).aria_label(label))
    }
    fn output(&self, _cx: &App) -> PathBuf {
        self.config
            .defaults
            .out
            .clone()
            .map(course2md::config::expand_tilde)
            .unwrap_or_else(backend::default_output)
    }
    fn pick(&mut self, directory: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some(
                if directory {
                    "选择保存目录"
                } else {
                    "选择课程视频"
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            let field = if directory {
                                Field::Output
                            } else {
                                Field::Source
                            };
                            this.inputs[&field].update(cx, |state, cx| {
                                state.set_value(path.display().to_string(), window, cx)
                            });
                            if !directory {
                                this.inspect_source(cx);
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.message = Some(format!("无法打开文件选择器：{error:#}")),
                    Err(error) => this.message = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn start(&mut self, kind: Kind, cx: &mut Context<Self>) {
        cx.notify();
        if self.job.is_some() {
            self.page = Page::Task;
            return;
        }
        self.message = None;
        if self.config_error {
            self.message = Some("请先修正配置文件，再开始任务。".into());
            return;
        }
        let args = match kind {
            Kind::Doctor => vec!["doctor".into()],
            Kind::Models => vec![
                "models".into(),
                "download".into(),
                "--json".into(),
                "--dir".into(),
                course2md::config::model_dir_from(self.config.defaults.model_dir.as_deref())
                    .display()
                    .to_string(),
            ],
            Kind::Convert => {
                if self
                    .source_preview
                    .as_ref()
                    .is_none_or(|source| source.input != self.value(Field::Source, cx))
                {
                    self.preview_error = Some("请先预览并确认课程内容".into());
                    return;
                }
                let source = self.value(Field::Source, cx);
                let source = course2md::config::expand_tilde(source.into())
                    .display()
                    .to_string();
                if let Some(environment) = &self.environment {
                    if !environment.ffmpeg || !environment.ffprobe {
                        self.message = Some(
                            "缺少视频处理工具。请在设置 → 运行环境中查看安装方式，然后重新检测。"
                                .into(),
                        );
                        return;
                    }
                    if source.starts_with("http") && !environment.ytdlp {
                        self.message =
                            Some("在线课程需要 yt-dlp。请在设置 → 运行环境中查看安装方式。".into());
                        return;
                    }
                }
                if !course2md::config::looks_like_source(&source) {
                    self.message = Some("请输入有效的视频链接，或选择存在的本地视频文件。".into());
                    return;
                }
                if self.output(cx).as_os_str().is_empty() {
                    self.message = Some("请选择笔记保存目录。".into());
                    return;
                }
                let formats = ["md", "html", "json"]
                    .into_iter()
                    .zip(self.task_options.formats)
                    .filter(|(_, selected)| *selected)
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>();
                if formats.is_empty() {
                    self.message = Some("至少选择一种输出格式。".into());
                    return;
                }
                let mut args = vec![
                    source,
                    "--json".into(),
                    "--out".into(),
                    self.output(cx).display().to_string(),
                    "--transcript-source".into(),
                    SOURCES[self.task_options.source_mode].0.into(),
                    "--formats".into(),
                    formats.join(","),
                    if self.task_options.llm {
                        "--llm"
                    } else {
                        "--no-llm"
                    }
                    .into(),
                    if self.task_options.resume {
                        "--resume"
                    } else {
                        "--no-resume"
                    }
                    .into(),
                ];
                if self.task_options.provider > 0 {
                    args.extend([
                        "--provider".into(),
                        PROVIDERS[self.task_options.provider].0.into(),
                    ]);
                }
                args.push(
                    if self.task_options.keep_video {
                        "--keep-video"
                    } else {
                        "--no-keep-video"
                    }
                    .into(),
                );
                args
            }
        };
        match Job::start(args) {
            Ok(job) => {
                if kind == Kind::Convert {
                    self.task_destination = self
                        .source_preview
                        .clone()
                        .map(|source| (self.output(cx), self.target_folder, source));
                }
                self.job = Some(job);
                self.kind = kind;
                self.cancelling = false;
                self.show_logs = false;
                self.scrolls[Page::Task as usize].set_offset(point(px(0.), px(0.)));
                self.progress.clear();
                self.logs.clear();
                self.completed = None;
                self.pending_done = None;
                self.task_status = match kind {
                    Kind::Convert => "正在准备课程笔记",
                    Kind::Doctor => "正在检查环境",
                    Kind::Models => "正在下载模型",
                }
                .into();
                self.page = Page::Task;
            }
            Err(error) => self.message = Some(format!("{error:#}")),
        }
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let events: Vec<_> = self
            .job
            .as_ref()
            .map(|job| job.events.try_iter().take(512).collect())
            .unwrap_or_default();
        if events.is_empty() {
            if self.job.is_some() && self.last_tick.elapsed() >= Duration::from_secs(1) {
                self.last_tick = Instant::now();
                cx.notify();
            }
            return;
        }
        for event in events {
            match event {
                Event::Log { message } => self.logs.push_back(message),
                Event::Stage { stage, status } => {
                    if status == "start" {
                        self.progress
                            .insert(stage.clone(), activity::Activity::new());
                    } else if status == "done" {
                        self.progress
                            .entry(stage.clone())
                            .or_insert_with(activity::Activity::new)
                            .done = true;
                    }
                }
                Event::Progress {
                    stage,
                    current,
                    total,
                    message,
                } => {
                    self.progress
                        .entry(stage)
                        .or_insert_with(activity::Activity::new)
                        .update(current, total, message);
                }
                Event::Workers { stage, workers } => {
                    self.progress
                        .entry(stage)
                        .or_insert_with(activity::Activity::new)
                        .workers = workers;
                }
                Event::Error { message } => {
                    self.logs.push_back(message.clone());
                    self.message = Some(message);
                }
                Event::Done(done) => self.pending_done = Some(done),
                Event::Exit { success, cancelled } => {
                    self.job = None;
                    self.cancelling = false;
                    if self.closing {
                        if self.preview_workers == 0 {
                            cx.quit();
                        }
                        return;
                    }
                    if cancelled {
                        self.task_status = "任务已取消，可修改设置后重试".into();
                    } else if success && (self.kind != Kind::Convert || self.pending_done.is_some())
                    {
                        self.task_status = "已完成".into();
                        self.completed = self.pending_done.take();
                        if let (Some(done), Some((root, folder, source))) =
                            (&self.completed, self.task_destination.take())
                        {
                            self.completed_source = Some(source.input.clone());
                            if let Err(e) = source::save_cover(&source, &done.out_dir) {
                                self.message = Some(format!("笔记已完成，但封面保存失败：{e:#}"));
                            }
                            if let Err(e) = organize::Library::edit(&root, |library| {
                                library.assign(
                                    &root,
                                    &done.out_dir,
                                    folder.filter(|id| library.folders.contains_key(id)),
                                )
                            }) {
                                self.message = Some(format!("笔记已完成，但归档失败：{e:#}"));
                            }
                            self.refresh_library(cx);
                        }
                        if self.page == Page::Task
                            && let Some(done) = self.completed.clone()
                        {
                            self.open_course(Course::from_completed(&done), cx);
                        }
                    } else {
                        self.task_status = "任务未完成".into();
                        self.show_logs = true;
                        if self.message.is_none() {
                            self.message =
                                Some("引擎退出，未生成完整结果。请查看下方日志并重试。".into());
                        }
                    }
                }
            }
            while self.logs.len() > 400 {
                self.logs.pop_front();
            }
        }
        cx.notify();
    }
    fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let root = self.output(cx);
        if self.library_root != root {
            self.library_root = root.clone();
            self.library = Default::default();
            self.courses.clear();
            self.folder_filter = None;
            self.target_folder = None;
            self.folder_editor = None;
            self.delete_folder = None;
        }
        if self.loading {
            return;
        }
        self.loading = true;
        let task_root = root.clone();
        let task = cx.background_executor().spawn(async move {
            Ok::<_, anyhow::Error>((
                backend::library(&task_root)?,
                organize::Library::load(&task_root)?,
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                if this.output(cx) != root {
                    this.refresh_library(cx);
                    return;
                }
                match result {
                    Ok((courses, library)) => {
                        if this.library_root != root {
                            this.folder_filter = None;
                            this.target_folder = None;
                        }
                        this.library_root = root;
                        this.courses = courses;
                        this.library = library;
                    }
                    Err(error) => this.message = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn open_course(&mut self, course: Course, cx: &mut Context<Self>) {
        self.loading = true;
        self.result_origin = self.page;
        let task = cx
            .background_executor()
            .spawn(async move { backend::read_preview(course) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(preview) => {
                        this.result_tab = if preview.has_markdown { 0 } else { 2 };
                        this.preview = Some(preview);
                        this.scrolls[Page::Result as usize].set_offset(point(px(0.), px(0.)));
                        this.page = Page::Result;
                    }
                    Err(error) => this.message = Some(format!("读取笔记失败：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn edited_settings(&self, cx: &App) -> course2md::settings::ConfigFile {
        let mut config = self.config.clone();
        config.desktop = self.desktop_settings.clone();
        config.asr_api.base_url = self.value(Field::AsrUrl, cx);
        config.asr_api.api_key = self.value(Field::AsrKey, cx);
        config.asr_api.model = self.value(Field::AsrModel, cx);
        config.llm.base_url = self.value(Field::LlmUrl, cx);
        config.llm.api_key = self.value(Field::LlmKey, cx);
        config.llm.model = self.value(Field::LlmModel, cx);
        config.llm.enabled = self.settings_options.llm;
        config.defaults.out = Some(course2md::config::expand_tilde(
            self.value(Field::Output, cx).into(),
        ));
        config.defaults.keep_video = Some(self.settings_options.keep_video);
        config.defaults.resume = Some(self.settings_options.resume);
        use course2md::config::{AsrProvider::*, TranscriptSource::*};
        config.defaults.provider = [
            None,
            Some(Coreml),
            Some(Gpu),
            Some(Cpu),
            Some(Npu),
            Some(Api),
        ][self.settings_options.provider];
        config.defaults.transcript_source =
            Some([Auto, Subtitle, Asr][self.settings_options.source_mode]);
        use course2md::config::OutputFormat::*;
        config.defaults.formats = Some(
            [Md, Html, Json]
                .into_iter()
                .zip(self.settings_options.formats)
                .filter(|(_, selected)| *selected)
                .map(|(format, _)| format)
                .collect(),
        );
        config
    }
    fn missing_setting(&self, cx: &App) -> Option<(Field, &'static str)> {
        if self.value(Field::Output, cx).is_empty() {
            return Some((Field::Output, "请选择笔记保存目录"));
        }
        if self.settings_options.provider == 5 && self.settings_options.source_mode != 1 {
            for (field, label) in [
                (Field::AsrUrl, "请填写云端 API 服务地址"),
                (Field::AsrModel, "请填写云端识别模型"),
                (Field::AsrKey, "请填写云端 API Key"),
            ] {
                if self.value(field, cx).is_empty()
                    && !(field == Field::AsrKey
                        && course2md::config::asr_api_key_from_env().is_some())
                {
                    return Some((field, label));
                }
            }
        }
        None
    }
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        if self.config_error {
            self.settings_status = "配置文件损坏，请修正后重新加载".into();
            return;
        }
        if let Some((_, message)) = self.missing_setting(cx) {
            self.settings_status = format!("未保存：{message}");
            return;
        }
        let mut config = self.edited_settings(cx);
        if let Err(error) =
            course2md::options::resolve("configuration".into(), &Default::default(), &config)
                .and_then(|cfg| cfg.validate())
        {
            self.settings_status = format!("未保存：{error:#}");
            return;
        }
        if config.llm.enabled
            && let Err(error) = course2md::llm::validate(&config.llm)
        {
            self.settings_status = format!("未保存：{error:#}");
            return;
        }
        config.llm.disable_hint = true;
        match course2md::settings::save(&config) {
            Ok(_) => {
                self.config = config;
                self.task_options = ConversionOptions::from_config(&self.config);
                self.settings_snapshot = self.edited_settings(cx);
                cx.set_reduce_motion(self.desktop_settings.reduce_motion);
                self.refresh_library(cx);
                self.settings_status = "已自动保存".into();
            }
            Err(error) => self.settings_status = format!("保存失败：{error:#}"),
        }
        cx.notify();
    }
    fn sync_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.desktop_settings = self.config.desktop.clone();
        self.settings_snapshot = self.config.clone();
        let cfg = &self.config;
        for (field, value) in [
            (Field::AsrUrl, cfg.asr_api.base_url.clone()),
            (Field::AsrKey, cfg.asr_api.api_key.clone()),
            (Field::AsrModel, cfg.asr_api.model.clone()),
            (Field::LlmUrl, cfg.llm.base_url.clone()),
            (Field::LlmKey, cfg.llm.api_key.clone()),
            (Field::LlmModel, cfg.llm.model.clone()),
        ] {
            self.inputs[&field].update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.settings_options = ConversionOptions::from_config(cfg);
        if self.settings_options.provider == 0
            && let Some(choice) = self
                .engine_choices(cx)
                .into_iter()
                .find(|c| (1..=4).contains(&c.index) && c.selectable)
        {
            self.settings_options.provider = choice.index;
        }
        self.task_options = self.settings_options.clone();
        let output = cfg
            .defaults
            .out
            .clone()
            .map(course2md::config::expand_tilde)
            .unwrap_or_else(backend::default_output);
        self.inputs[&Field::Output].update(cx, |state, cx| {
            state.set_value(output.display().to_string(), window, cx)
        });
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        if let Some(cancel) = &self.preview_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(|cx| {
            gpui_component::init(cx);
            gpui_component::set_locale("zh-CN");
            theme::init(cx);
            let system_titlebar = course2md::settings::load()
                .map(|c| c.desktop.system_titlebar)
                .unwrap_or(false);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::centered(size(px(1140.), px(820.)), cx)),
                    window_min_size: Some(size(px(860.), px(620.))),
                    ..if system_titlebar {
                        WindowOptions {
                            titlebar: Some(TitlebarOptions {
                                title: Some("course2md".into()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }
                    } else {
                        TitleBar::window_options()
                    }
                },
                |window, cx| {
                    window.set_window_title("course2md");
                    let view = cx.new(|cx| Desktop::new(window, cx));
                    if !view.read(cx).desktop_settings.setup_completed {
                        view.update(cx, |_, cx| {
                            cx.defer_in(window, |this, window, cx| {
                                this.open_setup(window, cx);
                            })
                        });
                    }
                    let weak = view.downgrade();
                    let quit_view = weak.clone();
                    let settings_view = weak.clone();
                    cx.on_action(move |_: &OpenSettings, cx| {
                        let _ =
                            settings_view.update(cx, |this, cx| this.navigate(Page::Settings, cx));
                    });
                    cx.on_action(move |_: &Quit, cx| {
                        if quit_view
                            .update(cx, |this, cx| this.request_close(cx))
                            .unwrap_or(true)
                        {
                            cx.quit();
                        }
                    });
                    window.on_window_should_close(cx, move |_, cx| {
                        weak.update(cx, |this, cx| this.request_close(cx))
                            .unwrap_or(true)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("无法创建 course2md 窗口");
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.bind_keys([
                KeyBinding::new("secondary-q", Quit, None),
                KeyBinding::new("secondary-,", OpenSettings, None),
            ]);
            cx.set_menus([gpui::Menu::new("course2md").items([
                gpui::MenuItem::action("设置…", OpenSettings),
                gpui::MenuItem::action("退出 course2md", Quit),
            ])]);
            cx.activate(true);
        });
}
