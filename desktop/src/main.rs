#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod backend;
mod theme;
mod views;
use backend::{Completed, Course, Event, Job};
use gpui::{prelude::*, *};
use gpui_component::{
    input::{Input, InputState},
    *,
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    time::Duration,
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
    Output,
    Search,
    AsrUrl,
    AsrKey,
    AsrModel,
    LlmUrl,
    LlmKey,
    LlmModel,
}
const STAGES: [(&str, &str); 7] = [
    ("fetch", "读取课程"),
    ("download", "下载视频"),
    ("scenes", "提取画面"),
    ("audio", "提取音频"),
    ("transcribe", "语音转写"),
    ("llm", "整理文字"),
    ("render", "生成笔记"),
];
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

struct Desktop {
    page: Page,
    result_origin: Page,
    result_tab: usize,
    settings_tab: usize,
    show_options: bool,
    show_logs: bool,
    environment: Option<backend::Environment>,
    scrolls: [ScrollHandle; 5],
    inputs: BTreeMap<Field, Entity<InputState>>,
    config: course2md::settings::ConfigFile,
    config_error: bool,
    provider: usize,
    source_mode: usize,
    llm: bool,
    keep_video: bool,
    resume: bool,
    formats: [bool; 3],
    job: Option<Job>,
    kind: Kind,
    cancelling: bool,
    closing: bool,
    task_status: String,
    stages: BTreeMap<String, String>,
    progress: BTreeMap<String, (u64, u64)>,
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

actions!(course2md_desktop, [Quit]);

impl Desktop {
    fn request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(job) = &self.job {
            job.cancel();
            self.closing = true;
            self.cancelling = true;
            self.task_status = "正在停止任务并关闭…".into();
            cx.notify();
            false
        } else {
            true
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
                "粘贴 YouTube、Bilibili 链接，或选择本地视频",
                String::new(),
            ),
            (
                Field::Output,
                "课程笔记保存位置",
                output.display().to_string(),
            ),
            (Field::Search, "搜索课程标题", String::new()),
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
        let subscriptions = inputs
            .values()
            .map(|input| cx.observe(input, |_, _, cx| cx.notify()))
            .collect();
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
        let poll = cx.spawn(async |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(80)).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            page: Page::New,
            result_origin: Page::Library,
            result_tab: 0,
            settings_tab: 0,
            show_options: false,
            show_logs: false,
            environment: None,
            scrolls: std::array::from_fn(|_| ScrollHandle::new()),
            inputs,
            config,
            config_error,
            provider,
            source_mode,
            llm,
            keep_video,
            resume,
            formats,
            job: None,
            kind: Kind::Convert,
            cancelling: false,
            closing: false,
            task_status: "尚无运行中的任务".into(),
            stages: BTreeMap::new(),
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
        this.refresh_environment(cx);
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
                cx.notify();
            });
        })
        .detach();
    }
    fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
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
    fn output(&self, cx: &App) -> PathBuf {
        course2md::config::expand_tilde(self.value(Field::Output, cx).into())
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
                if self.value(Field::Output, cx).is_empty() {
                    self.message = Some("请选择笔记保存目录。".into());
                    return;
                }
                let formats = ["md", "html", "json"]
                    .into_iter()
                    .zip(self.formats)
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
                    SOURCES[self.source_mode].0.into(),
                    "--formats".into(),
                    formats.join(","),
                    if self.llm { "--llm" } else { "--no-llm" }.into(),
                    if self.resume {
                        "--resume"
                    } else {
                        "--no-resume"
                    }
                    .into(),
                ];
                if self.provider > 0 {
                    args.extend(["--provider".into(), PROVIDERS[self.provider].0.into()]);
                }
                args.push(
                    if self.keep_video {
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
                self.job = Some(job);
                self.kind = kind;
                self.cancelling = false;
                self.show_logs = kind != Kind::Convert;
                self.scrolls[Page::Task as usize].set_offset(point(px(0.), px(0.)));
                self.stages.clear();
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
            return;
        }
        for event in events {
            match event {
                Event::Log { message } => self.logs.push_back(message),
                Event::Stage { stage, status } => {
                    self.stages.insert(stage, status);
                }
                Event::Progress {
                    stage,
                    current,
                    total,
                    message,
                } => {
                    self.progress.insert(stage, (current, total));
                    if let Some(message) = message {
                        self.task_status = message;
                    }
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
                        cx.quit();
                        return;
                    }
                    if cancelled {
                        self.task_status = "任务已取消，可修改设置后重试".into();
                    } else if success && (self.kind != Kind::Convert || self.pending_done.is_some())
                    {
                        self.task_status = "已完成".into();
                        self.completed = self.pending_done.take();
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
        if self.loading {
            return;
        }
        self.loading = true;
        let root = self.output(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend::library(&root) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(courses) => this.courses = courses,
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
        config.asr_api.base_url = self.value(Field::AsrUrl, cx);
        config.asr_api.api_key = self.value(Field::AsrKey, cx);
        config.asr_api.model = self.value(Field::AsrModel, cx);
        config.llm.base_url = self.value(Field::LlmUrl, cx);
        config.llm.api_key = self.value(Field::LlmKey, cx);
        config.llm.model = self.value(Field::LlmModel, cx);
        config.llm.enabled = self.llm;
        config.defaults.out = Some(self.output(cx));
        config.defaults.keep_video = Some(self.keep_video);
        config.defaults.resume = Some(self.resume);
        use course2md::config::{AsrProvider::*, TranscriptSource::*};
        config.defaults.provider = [
            None,
            Some(Coreml),
            Some(Gpu),
            Some(Cpu),
            Some(Npu),
            Some(Api),
        ][self.provider];
        config.defaults.transcript_source = Some([Auto, Subtitle, Asr][self.source_mode]);
        use course2md::config::OutputFormat::*;
        config.defaults.formats = Some(
            [Md, Html, Json]
                .into_iter()
                .zip(self.formats)
                .filter(|(_, selected)| *selected)
                .map(|(format, _)| format)
                .collect(),
        );
        config
    }
    fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        if self.config_error {
            self.message = Some("请先在外部编辑器中修正原配置，然后重新加载。".into());
            return;
        }
        let mut config = self.edited_settings(cx);
        if let Err(error) =
            course2md::options::resolve("configuration".into(), &Default::default(), &config)
                .and_then(|cfg| cfg.validate())
        {
            self.message = Some(format!("未保存：{error:#}"));
            return;
        }
        if config.llm.enabled
            && let Err(error) = course2md::llm::validate(&config.llm)
        {
            self.message = Some(format!("未保存：{error:#}"));
            return;
        }
        config.llm.disable_hint = true;
        match course2md::settings::save(&config) {
            Ok(_) => {
                self.config = config;
                self.sync_settings(window, cx);
                self.message = Some("设置已保存。下一次任务会使用新设置。".into());
            }
            Err(error) => self.message = Some(format!("保存失败：{error:#}")),
        }
        cx.notify();
    }
    fn sync_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.llm = cfg.llm.enabled;
        self.keep_video = cfg.defaults.keep_video.unwrap_or(false);
        self.resume = cfg.defaults.resume.unwrap_or(false);
        self.provider = cfg
            .defaults
            .provider
            .and_then(|provider| {
                PROVIDERS
                    .iter()
                    .position(|(id, _)| *id == provider.as_str())
            })
            .unwrap_or(0);
        self.source_mode = match cfg.defaults.transcript_source.unwrap_or_default() {
            course2md::config::TranscriptSource::Auto => 0,
            course2md::config::TranscriptSource::Subtitle => 1,
            course2md::config::TranscriptSource::Asr => 2,
        };
        use course2md::config::OutputFormat::*;
        self.formats = cfg
            .defaults
            .formats
            .as_ref()
            .map(|formats| {
                [
                    formats.contains(&Md),
                    formats.contains(&Html),
                    formats.contains(&Json),
                ]
            })
            .unwrap_or([true, true, false]);
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

fn main() {
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(|cx| {
            gpui_component::init(cx);
            gpui_component::set_locale("zh-CN");
            theme::init(cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::centered(size(px(1140.), px(820.)), cx)),
                    window_min_size: Some(size(px(860.), px(620.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("course2md".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| Desktop::new(window, cx));
                    let weak = view.downgrade();
                    let quit_view = weak.clone();
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
            cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
            cx.set_menus([gpui::Menu::new("course2md")
                .items([gpui::MenuItem::action("退出 course2md", Quit)])]);
            cx.activate(true);
        });
}
