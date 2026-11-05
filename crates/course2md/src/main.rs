use anyhow::Context;
use clap::{CommandFactory, FromArgMatches};
use course2md::cli::{ChildProgress, Cli, Command, ConfigCmd, LlmCmd, ModelsCmd};
use course2md::{config, doctor, llm, models, pipeline, progress, settings, wizard};
use course2md::{eprintln_tr, println_tr, tr};
use tracing_subscriber::EnvFilter;

fn init_logging(verbose: u8, quiet: bool, json: bool) {
    let default = if quiet {
        "error"
    } else if verbose >= 2 {
        "debug"
    } else if verbose == 1 || json {
        "info"
    } else {
        "warn"
    };
    let filter = if quiet {
        EnvFilter::new("error")
    } else {
        std::env::var(EnvFilter::DEFAULT_ENV)
            .ok()
            .and_then(|spec| EnvFilter::try_new(engine_targets(&spec)).ok())
            .unwrap_or_else(|| default.into())
    };
    use tracing_subscriber::prelude::*;
    let logs = tracing_subscriber::registry()
        .with(filter)
        .with(transport_cap());
    if json {
        // NDJSON 模式：stdout 只出 {"type":"log",...} 行（其余事件由各阶段显式 emit）
        logs.with(progress::JsonLogLayer).init();
    } else if verbose >= 2 {
        // 调试档：完整格式（时间 + target）
        logs.with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(true),
        )
        .init();
    } else {
        // 默认档：个人 CLI 时间戳是纯噪音，compact 单行
        logs.with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .without_time()
                .compact(),
        )
        .init();
    }
}

/// HTTP and TLS crates whose debug and trace output carries what a request
/// sends: ureq-proto dumps the raw request, `Authorization` header included,
/// and ureq logs URLs, which can hold a login token. Logs reach the terminal
/// and, through `--json`/`run-task`, the desktop, so these crates never log
/// below `info` whatever `RUST_LOG` asks for.
const TRANSPORT_CRATES: [&str; 7] = [
    "ureq",
    "rustls",
    "tokio_rustls",
    "native_tls",
    "openssl",
    "hyper",
    "h2",
];

/// A filter that every log event must also pass: everything at any level,
/// except the [`TRANSPORT_CRATES`] (and any target they prefix) above `info`.
fn transport_cap() -> tracing_subscriber::filter::Targets {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_targets(TRANSPORT_CRATES.map(|name| (name, LevelFilter::INFO)))
}

/// The engine's crates, which log under their own targets since the split.
const ENGINE_CRATES: [&str; 11] = [
    "c2m_ai",
    "c2m_artifact",
    "c2m_asr",
    "c2m_config",
    "c2m_core",
    "c2m_dispatch",
    "c2m_engine",
    "c2m_i18n",
    "c2m_process",
    "c2m_protocol",
    "c2m_source",
];

/// A `RUST_LOG` written for 2.0.1, where every engine module logged under
/// `course2md`: each `course2md` directive also covers the engine crates
/// (`course2md::asr=debug` becomes `c2m_*::asr=debug` too).
fn engine_targets(spec: &str) -> String {
    let mut directives = Vec::new();
    for directive in spec.split(',').filter(|d| !d.trim().is_empty()) {
        directives.push(directive.to_owned());
        let rest = directive.trim().strip_prefix("course2md");
        if let Some(rest) = rest.filter(|rest| rest.is_empty() || rest.starts_with(['=', ':', '[']))
        {
            directives.extend(ENGINE_CRATES.iter().map(|name| format!("{name}{rest}")));
        }
    }
    directives.join(",")
}

/// `course2md ... | head` closes the pipe early, and `println!` then panics. A reader
/// that stops reading is not an error: end as a program killed by SIGPIPE would.
fn end_quietly_on_closed_pipe() {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        let closed = message.contains("os error 32)") || message.contains("os error 232)");
        if message.starts_with("failed printing to std") && closed {
            std::process::exit(141);
        }
        report(info);
    }));
}

fn main() -> std::process::ExitCode {
    // SAFETY: the first statement of main, before any thread exists.
    #[cfg(apple_native)]
    unsafe {
        course2md::apple::pin_cache_dir()
    };
    end_quietly_on_closed_pipe();
    let mut args: Vec<_> = std::env::args_os().collect();
    let lang_arg = course2md::cli::take_lang_arg(&mut args);
    // 近似扫描（先于 clap 解析，解析失败也要走对输出模式）：把值恰为 "--json"/
    // "run-task" 的其他参数值（如 --llm-prompt "--json"）误判为 NDJSON 是可接受的
    // 代价——它只影响解析失败时的错误格式，不影响成功路径的行为。
    let json_requested = args
        .iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "--json" || a == "run-task");
    let requested = lang_arg
        .as_deref()
        .and_then(course2md::i18n::Lang::from_code)
        .unwrap_or_else(course2md::i18n::Lang::detect);
    course2md::i18n::set_lang(requested);
    let command = course2md::cli::localize_command(Cli::command());
    let cli = match command.try_get_matches_from(&args) {
        Ok(matches) => match Cli::from_arg_matches(&matches) {
            Ok(mut cli) => {
                cli.lang = lang_arg.clone();
                cli
            }
            Err(error) => {
                let rendered = course2md::cli::localize_rendered(&error.render().to_string());
                eprint!("{rendered}");
                return std::process::ExitCode::from(error.exit_code() as u8);
            }
        },
        Err(error) => {
            if error.use_stderr() && json_requested {
                progress::set_json_mode();
                progress::emit(&progress::Event::Error {
                    message: tr!("参数无效：{}", error),
                    code: progress::ErrorCode::InvalidArguments,
                    parts: Vec::new(),
                    path: None,
                });
            } else if course2md::i18n::lang() == course2md::i18n::Lang::En {
                let _ = error.print();
            } else {
                let rendered = course2md::cli::localize_rendered(&error.render().to_string());
                print!("{rendered}");
            }
            return std::process::ExitCode::from(error.exit_code() as u8);
        }
    };
    if let Some(value) = &cli.lang {
        match course2md::i18n::Lang::from_code(value) {
            Some(lang) => course2md::i18n::set_lang(lang),
            None if value == "auto" => {
                course2md::i18n::init_from_env();
            }
            None => {
                eprintln!(
                    "{}",
                    tr!(
                        "未知的语言：{}。可用：auto、en、zh-Hans、zh-Hant、ja",
                        value
                    )
                );
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let helper = match &cli.command {
        Some(Command::AppleCache { progress, .. }) => Some(*progress),
        _ => None,
    };
    let json = cli.opts.json
        || matches!(&cli.command, Some(Command::RunTask))
        || helper == Some(ChildProgress::Json)
        || matches!(
            &cli.command,
            Some(Command::Models {
                cmd: ModelsCmd::Download { json: true, .. }
                    | ModelsCmd::Prepare { json: true, .. }
                    | ModelsCmd::Inspect { json: true, .. }
            })
        );
    let quiet = cli.opts.quiet
        || helper == Some(ChildProgress::Quiet)
        || matches!(&cli.command, Some(Command::Summarize(args)) if args.quiet);
    // Content choices (the subtitle track a run reads) follow the person's
    // language in every output mode.
    course2md::i18n::set_person_lang(course2md::i18n::lang());
    if json {
        // Machine consumers (--json, run-task) parse message text; keep the
        // protocol's source language whatever --lang says.
        course2md::i18n::set_lang(course2md::i18n::Lang::ZhHans);
    }
    if json {
        progress::set_json_mode();
    }
    progress::set_quiet(quiet);
    init_logging(cli.opts.verbose, quiet, json);
    match run(cli, args.len() == 1) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // 向导的「退出，稍后下载」是正常结束，不是失败
            if error
                .downcast_ref::<course2md::wizard::ExitWizard>()
                .is_some()
            {
                return std::process::ExitCode::SUCCESS;
            }
            let message = format!("{error:#}");
            // A helper's parent reads its error from stderr and decides what
            // the task reports, so the helper never emits an error event.
            if json && helper.is_none() {
                progress::emit(&progress::Event::Error {
                    code: progress::ErrorCode::of(&error),
                    message,
                    parts: progress::kept_parts(&error),
                    path: progress::blocked_path(&error),
                });
            } else if helper.is_some() {
                // The parent puts the helper's words into its own error.
                eprintln!("{message}");
            } else {
                eprintln_tr!("错误：{}", message);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, no_args: bool) -> anyhow::Result<()> {
    if let Some(platform) = cli.logout {
        return course2md::cli::logout(platform);
    }
    if let Some(platform) = cli.login {
        anyhow::ensure!(
            cli.source.as_deref().is_none_or(config::looks_like_source),
            tr!(
                "--login 后只能跟视频链接或本地视频路径 / After --login, provide only a video URL or local file path"
            )
        );
        course2md::cli::login(platform, cli.method, cli.browser.as_deref())?;
        if cli.source.is_none() {
            return Ok(());
        }
    }
    match cli.command {
        Some(Command::RunTask) => {
            let parent = course2md::dispatch::ParentProcess::current();
            let request = course2md::execution::Request::read(std::io::stdin().lock())?;
            Ok(course2md::runtime::block_on(course2md::execution::run(
                request, parent,
            ))??)
        }
        Some(Command::AppleCache { model, .. }) => {
            #[cfg(apple_native)]
            {
                course2md::apple::prepare_cache_here(&model)
            }
            #[cfg(not(apple_native))]
            {
                let _ = model;
                anyhow::bail!(tr!(
                    "此版本不支持 Apple 识别，请改用 GPU 或 CPU 识别 / This build lacks Apple transcription; use GPU or CPU transcription."
                ))
            }
        }
        Some(Command::Models { cmd }) => match cmd {
            ModelsCmd::Inspect { target, json } => {
                let root = configured_model_dir(target.dir.as_deref())?;
                let status = models::status::inspect(target.provider, target.model(), &root)?;
                if json {
                    println!("{}", serde_json::to_string(&status)?);
                } else {
                    print_model_status(&status);
                }
                Ok(())
            }
            ModelsCmd::Prepare { target, json } => {
                let root = configured_model_dir(target.dir.as_deref())?;
                let status = course2md::runtime::block_on(models::prepare(
                    target.provider,
                    target.model(),
                    &root,
                ))??;
                progress::emit(&progress::Event::ModelReady {
                    operation: progress::ModelOperation::Prepare,
                    dir: root.clone(),
                    provider: Some(target.provider),
                    model: Some(status.model.clone()),
                    status: Some(Box::new(status.clone())),
                });
                if !json {
                    println_tr!("模型准备完成 / Model preparation finished.");
                    print_model_status(&status);
                }
                Ok(())
            }
            ModelsCmd::Download { dir, json } => {
                let root = configured_model_dir(dir.as_deref())?;
                course2md::runtime::block_on(models::download_models(
                    &root,
                    &course2md::dispatch::TaskControl::default(),
                ))??;
                progress::emit(&progress::Event::ModelReady {
                    operation: progress::ModelOperation::Download,
                    dir: root.clone(),
                    provider: None,
                    model: None,
                    status: None,
                });
                if !json {
                    println_tr!("模型已就绪 / Models ready: {}", root.display());
                }
                Ok(())
            }
            ModelsCmd::List { dir } => {
                let root = configured_model_dir(dir.as_deref())?;
                models::list_models(&root);
                Ok(())
            }
        },
        Some(Command::Llm { cmd }) => {
            match cmd {
                LlmCmd::Setup {
                    base_url,
                    api_key,
                    model,
                    disable_hint,
                } => {
                    let cfg = llm::setup_interactive(
                        settings::load()?,
                        base_url,
                        api_key,
                        model,
                        disable_hint,
                    )?;
                    let path = settings::save(&cfg)?;
                    println_tr!("已保存并启用 / Saved and enabled: {}", path.display());
                    match llm::test_connection(&cfg.llm) {
                        Ok(()) => println_tr!("连接测试通过 / Connection test passed."),
                        Err(e) => anyhow::bail!(tr!(
                            "配置已保存，但连接测试失败；请检查服务地址、密钥和模型 / Settings saved, but connection test failed; check endpoint, key and model: {}",
                            format!("{e:#}")
                        )),
                    }
                }
                LlmCmd::Status => llm::print_status(&settings::load()?),
                LlmCmd::Disable => {
                    let mut cfg = settings::load()?;
                    cfg.llm.enabled = false;
                    let path = settings::save(&cfg)?;
                    println_tr!(
                        "已关闭 AI 校对，凭据保留 / AI proofreading disabled; credentials kept: {}",
                        path.display()
                    );
                }
            }
            Ok(())
        }
        Some(Command::Doctor) => doctor::run(),
        Some(Command::Config { cmd }) => {
            match cmd {
                ConfigCmd::Init { force } => {
                    let path = settings::config_path();
                    if path.is_file() && !force {
                        anyhow::bail!(
                            "{}",
                            tr!("配置已存在：{}。使用 --force 替换", path.display())
                        );
                    }
                    let path = course2md::options::write_template()?;
                    println_tr!(
                        "已生成配置模板 / Configuration template created: {}",
                        path.display()
                    );
                    println!("{}", course2md::auth::BILIBILI_SETUP_TIP);
                }
                ConfigCmd::Show => course2md::options::print_effective(&settings::load()?)?,
            }
            Ok(())
        }
        Some(Command::Summarize(args)) => {
            let file = settings::load()?;
            llm::validate(&file.llm)?;
            let mut targets: Vec<std::path::PathBuf> = vec![];
            for dir in &args.dirs {
                collect_targets(dir, &mut targets, 0);
            }
            if targets.is_empty() {
                anyhow::bail!(
                    "未找到笔记：{0} / No notes found in: {0}",
                    args.dirs
                        .iter()
                        .map(|d| d.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            for dir in &targets {
                summarize_note(&file, dir, args.force, args.out.as_deref())?;
            }
            Ok(())
        }
        Some(Command::Remove(args)) => {
            let mut cfg = settings::load()?;
            let mut cleared: Vec<String> = vec![];
            // 只列出确实非空、本次真正清掉的字段（空配置不冒充「已清除」）
            for (name, value) in [
                ("llm.api_key", &cfg.llm.api_key),
                ("llm.base_url", &cfg.llm.base_url),
                ("llm.model", &cfg.llm.model),
            ] {
                if !value.is_empty() {
                    cleared.push(name.to_string());
                }
            }
            if course2md::login::codex::remove_credentials()? {
                cleared.push("codex".to_string());
            }
            cfg.llm = course2md::llm::LlmSettings::default();
            if args.asr {
                if !cfg.asr_api.api_key.is_empty() {
                    cleared.push("asr_api.api_key".to_string());
                }
                cfg.asr_api.api_key.clear();
            }
            let path = settings::save(&cfg)?;
            if cleared.is_empty() {
                println_tr!(
                    "没有需要清除的 API 配置 / No saved API settings to clear: {}",
                    path.display()
                );
            } else {
                println_tr!(
                    "已清除 API 配置 / Cleared API settings ({}): {}",
                    cleared.join(", "),
                    path.display()
                );
            }
            if !args.asr {
                println_tr!(
                    "云端语音识别密钥未清除；如需清除：course2md remove --asr / Cloud speech key kept; to clear it: course2md remove --asr"
                );
            }
            Ok(())
        }
        None => {
            let source = match cli.source {
                Some(source) => normalize_source(&source)?,
                None => {
                    if !no_args {
                        return Err(course2md::error::invalid_input(anyhow::anyhow!(tr!(
                            "缺少视频来源。示例：course2md ./lecture.mp4"
                        ))));
                    }
                    println!("{}\n", tr!("course2md — 将视频转换为图文笔记"));
                    println!("{}", tr!(course2md::cli::QUICK_START));
                    println_tr!("全部参数 / All options: course2md --help");
                    return Ok(());
                }
            };
            let file = settings::load()?;
            let initial = course2md::options::resolve(source.clone(), &cli.opts, &file)?;
            // 仅为 wizard 前快速失败；pipeline::run 开头的全量 validate 才是入口契约
            // （桌面端走 run_task 不经这里，两处各自服务自己的调用方）。
            initial.validate()?;
            if initial.llm.enabled {
                llm::validate(&initial.llm)?;
            }
            if !std::path::Path::new(&source).is_file() {
                course2md::error::require_ytdlp()?;
            }
            // 首次使用向导：未配置转写方式 + 交互终端时引导配置并写盘（非交互原样返回）；
            // json 模式显式跳过——stdout 必须保持纯 NDJSON，不能混进交互提示
            let file = if cli.opts.json || cli.opts.quiet {
                file
            } else {
                wizard::maybe_run(&cli.opts, &file)?
            };
            let mut cfg = course2md::options::resolve(source, &cli.opts, &file)?;
            // coreml 首次使用时先确定模型（可能交互询问），模型缓存准备与转写才会是同一个模型
            #[cfg_attr(not(apple_native), allow(unused_mut))]
            let mut agreed = false;
            #[cfg(apple_native)]
            if cfg.provider == config::AsrProvider::Coreml
                && cfg.transcript_source != config::TranscriptSource::Subtitle
            {
                let (model, chosen) = course2md::apple::choose_model(cfg.asr_model.as_deref())?;
                cfg.asr_model = Some(model);
                agreed = chosen;
            }
            // The CLI may take the cloud transcription key from the environment;
            // desktop tasks carry fully resolved credentials instead.
            if cfg.provider == config::AsrProvider::Api
                && cfg.asr_api.api_key.is_empty()
                && let Some(key) = config::asr_api_key_from_env()
            {
                cfg.asr_api.api_key = key;
            }
            // A missing speech model is downloaded only with the person's consent.
            use course2md::consent::{self, Consent};
            let ask = consent::ask_terminal;
            let models = match consent::decide(consent::Situation {
                opted_in: cli.opts.download_models
                    || consent::env_opt_in(
                        std::env::var_os(consent::DOWNLOAD_MODELS_ENV).as_deref(),
                    ),
                agreed,
                interactive: course2md::runtime::interactive(),
                json: cli.opts.json,
                quiet: cli.opts.quiet,
            }) {
                Consent::Allowed => pipeline::ModelDownload::Allowed,
                Consent::Ask => pipeline::ModelDownload::Ask(&ask),
                Consent::Refused => pipeline::ModelDownload::Refused,
            };
            tracing::info!(out = %cfg.out_root.display(), provider = %cfg.provider, "start");
            Ok(course2md::runtime::block_on(pipeline::run(&cfg, models))??)
        }
    }
}

/// `--dir` > 配置文件 defaults.model_dir > 系统缓存。所有 models 子命令共用。
fn configured_model_dir(dir: Option<&std::path::Path>) -> anyhow::Result<std::path::PathBuf> {
    if let Some(dir) = dir {
        return Ok(config::model_dir_from(Some(dir)));
    }
    let file = settings::load()?;
    Ok(config::model_dir_from(file.defaults.model_dir.as_deref()))
}

/// The note a folder holds. A course an older app wrote as versions (the course
/// folder, or one of its `versions/<id>` folders) is flattened into one note first.
fn note_dir(dir: &std::path::Path) -> anyhow::Result<Option<std::path::PathBuf>> {
    use course2md::artifact;
    let course = dir
        .parent()
        .filter(|versions| versions.file_name().is_some_and(|name| name == "versions"))
        .and_then(|versions| versions.parent())
        .filter(|course| artifact::is_legacy(course));
    let note = course.unwrap_or(dir);
    if let Err(error) = artifact::recover(note) {
        tracing::warn!(
            "跳过无法读取的笔记 / Skipped unreadable note {}: {error:#}",
            note.display()
        );
    }
    if artifact::is_legacy(note) {
        artifact::flatten_legacy(note)?;
        tracing::info!(
            "已将笔记的旧版本整理为一份笔记 / Turned the note's versions into one note: {}",
            note.display()
        );
    }
    Ok(note
        .join("manifest.json")
        .is_file()
        .then(|| note.to_path_buf()))
}

/// 递归收集笔记（可传单份笔记或存放多份笔记的上级目录）；单条目读取失败只告警并跳过。
fn collect_targets(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) {
    // A replacement a crash interrupted is completed before the folder is read.
    for attention in course2md::artifact::recover_folder(dir) {
        use course2md::artifact::NoteAttention;
        match attention {
            NoteAttention::Blocked { swap, .. } => eprintln!(
                "{}",
                course2md::outcome::recovery_blocked_message(&swap, course2md::i18n::lang())
            ),
            NoteAttention::Kept { swap, copies, .. } => eprintln_tr!(
                "笔记旁保留了 {} 份副本，请查看：{}",
                copies.len(),
                swap.display()
            ),
            NoteAttention::Failed { note, message } => tracing::warn!(
                "跳过无法读取的笔记 / Skipped unreadable note {}: {message}",
                note.display()
            ),
        }
    }
    match note_dir(dir) {
        Ok(Some(note)) => {
            if !out.contains(&note) {
                out.push(note);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                "跳过无法读取的笔记 / Skipped unreadable note {}: {error:#}",
                dir.display()
            );
            return;
        }
    }
    if depth >= 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            collect_targets(&path, out, depth + 1);
        }
    }
}

/// `summarize` 子命令：为一份笔记补写 AI 摘要，更新原笔记（幂等，--force 重新生成）。
/// It runs the same summary-only reprocess task the desktop runs.
fn summarize_note(
    file: &settings::ConfigFile,
    dir: &std::path::Path,
    force: bool,
    out: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use course2md::{artifact, execution};
    let dir = course2md::config::canonicalize(dir)?;
    let base = artifact::read_manifest(&dir.join("manifest.json"))?;
    let document: artifact::Document =
        serde_json::from_slice(&std::fs::read(dir.join(&base.document))?)?;
    if document.summary.is_some() && !force {
        if !progress::is_quiet() {
            println_tr!(
                "已有摘要，笔记保持不变 / Summary already exists: {}",
                dir.display()
            );
        }
        return Ok(());
    }
    let mut settings = file.llm.clone();
    settings.api_key.clear();
    // A note's task id was its version id, so a summary started before notes were
    // flat continues from its saved progress.
    let mut task_id = format!(
        "summary-{}",
        &course2md::digest::json_digest(&serde_json::json!({
            "operation": "reprocess-summary",
            "base_version": base.task_id,
            "settings": settings,
        }))?[..32]
    );
    if force {
        task_id.push_str(&format!(
            "-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
    }
    let source = if document.meta.webpage_url.trim().is_empty() {
        base.source_id.clone()
    } else {
        document.meta.webpage_url.clone()
    };
    let request = execution::Request {
        schema: execution::REQUEST_SCHEMA,
        operation: execution::Operation::Reprocess {
            note_dir: dir.clone(),
            base_task_id: base.task_id.clone(),
            components: vec![execution::Component::Summary],
            prior_work_dir: None,
        },
        task_id: task_id.clone(),
        course_id: base.course_id.clone(),
        version_id: None,
        source,
        source_id: base.source_id.clone(),
        title: base.title.clone(),
        author: document.meta.uploader.clone(),
        duration: document.meta.duration.max(0.),
        subtitle: None,
        subtitle_events: None,
        config: file.clone(),
        allow_unauthenticated_asr: false,
        work_dir: artifact::work_dir(&dir, &task_id)?,
        course_dir: dir.clone(),
        control_path: None,
        service_versions: Default::default(),
    };
    request.validate()?;
    course2md::runtime::block_on(execution::run(
        request,
        course2md::dispatch::ParentProcess::current(),
    ))??;
    if let Some(out) = out {
        let manifest = artifact::read_manifest(&dir.join("manifest.json"))?;
        let saved: artifact::Document =
            serde_json::from_slice(&std::fs::read(dir.join(&manifest.document))?)?;
        let summary = saved
            .summary
            .context(tr!("笔记缺少摘要 / The note has no summary"))?;
        let file = out.join(format!(
            "{}.summary.md",
            course2md::summary_doc::sanitize_filename(&saved.meta.title)
        ));
        let text = course2md::summary_doc::render_standalone_md(&saved.meta.title, &summary);
        course2md::checkpoint::atomic_write(&file, text.as_bytes())?;
        if !progress::is_quiet() {
            println_tr!("已导出摘要 / Summary exported: {}", file.display());
        }
    }
    if !progress::is_quiet() {
        println_tr!("摘要已加入笔记：{}", dir.display());
    }
    Ok(())
}

/// `models inspect/prepare` 的人类可读摘要；--json 时仍输出完整状态 JSON。
fn print_model_status(status: &models::status::LocalModelStatus) {
    use models::status::CacheState;
    let state = match status.state {
        CacheState::Missing => tr!("缺失 / Missing"),
        CacheState::Partial => tr!("不完整 / Incomplete"),
        CacheState::Cached => tr!("已缓存（未验证加载）/ Cached (load not verified)"),
        CacheState::Loaded => tr!("已就绪 / Ready"),
        CacheState::Unsupported => tr!("不支持 / Unsupported"),
    };
    println_tr!(
        "模型 / Model: {} · {} · {}",
        status.model,
        status.provider,
        state
    );
    println_tr!("占用空间 / Disk usage: {}", format_size(status.bytes));
    for part in &status.parts {
        println!("  {}: {}", part.name, part.path.display());
        for file in &part.missing {
            println_tr!("    缺失：{}", file);
        }
    }
    match status.state {
        CacheState::Cached | CacheState::Loaded => {}
        CacheState::Unsupported => {
            let available = config::local_models(status.provider)
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ");
            println_tr!("该后端支持的模型：{}", available)
        }
        _ if status.can_prepare => println_tr!(
            "下载或修复 / Download or repair: course2md models prepare --provider {} --model {}",
            status.provider,
            status.model
        ),
        _ => {}
    }
}

fn format_size(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}

/// Accept existing files and normalize URLs pasted without a scheme.
fn normalize_source(source: &str) -> anyhow::Result<String> {
    if std::path::Path::new(source).is_file() {
        return Ok(source.to_owned());
    }
    let source = source.trim();
    if source.starts_with("http://") || source.starts_with("https://") {
        let url = url::Url::parse(source).map_err(|_| {
            anyhow::anyhow!(tr!(
                "链接无效；请提供完整的视频链接 / Invalid URL; provide a complete video URL"
            ))
        })?;
        anyhow::ensure!(
            url.host_str().is_some(),
            tr!("链接缺少主机名 / URL is missing a host")
        );
        return Ok(source.to_owned());
    }
    if config::known_video_site(source).is_some() {
        return Ok(format!("https://{source}"));
    }
    Err(course2md::error::invalid_input(anyhow::anyhow!(
        "{}\n{}",
        tr!("找不到视频文件或无法识别链接：{}", source),
        tr!("请检查路径；含空格的路径需加引号")
    )))
}

#[cfg(test)]
mod tests {
    #[test]
    fn course2md_log_directives_cover_the_engine_crates() {
        let spec = super::engine_targets("warn,course2md=debug");
        assert!(spec.starts_with("warn,course2md=debug,c2m_ai=debug,"));
        assert!(spec.contains(",c2m_source=debug"));
        let spec = super::engine_targets("course2md::asr=trace");
        assert!(spec.contains("c2m_asr::asr=trace"));
        assert_eq!(
            super::engine_targets("course2md_other=info"),
            "course2md_other=info"
        );
        assert_eq!(super::engine_targets("info"), "info");
        let filter =
            tracing_subscriber::EnvFilter::try_new(super::engine_targets("course2md=debug"));
        assert!(filter.is_ok());
    }

    #[test]
    fn transport_crates_never_log_below_info() {
        use tracing::Level;
        let cap = super::transport_cap();
        for target in [
            "ureq",
            "ureq_proto::util",
            "ureq::run",
            "rustls::conn",
            "hyper_util",
        ] {
            assert!(!cap.would_enable(target, &Level::DEBUG), "{target}");
            assert!(cap.would_enable(target, &Level::INFO), "{target}");
        }
        for target in ["course2md", "c2m_ai::llm", "c2m_source::fetch"] {
            assert!(cap.would_enable(target, &Level::TRACE), "{target}");
        }
    }
}
