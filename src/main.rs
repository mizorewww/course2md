use anyhow::Context;
use clap::{CommandFactory, FromArgMatches};
use course2md::cli::{Cli, Command, ConfigCmd, LlmCmd, ModelsCmd};
use course2md::{config, doctor, llm, models, pipeline, progress, settings, wizard};
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
        EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into())
    };
    if json {
        // NDJSON 模式：stdout 只出 {"type":"log",...} 行（其余事件由各阶段显式 emit）
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(filter)
            .with(progress::json_log_layer())
            .init();
    } else if verbose >= 2 {
        // 调试档：完整格式（时间 + target）
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .init();
    } else {
        // 默认档：个人 CLI 时间戳是纯噪音，compact 单行
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(false)
            .without_time()
            .compact()
            .init();
    }
}

fn main() -> std::process::ExitCode {
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
                    message: course2md::tr!("参数无效：{}", error),
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
    // Machine protocol messages stay in the source language for stable consumers;
    // the desktop pins this command to Simplified Chinese.
    if matches!(cli.command, Some(Command::RunTask)) {
        course2md::i18n::set_lang(course2md::i18n::Lang::ZhHans);
    } else if let Some(value) = &cli.lang {
        match course2md::i18n::Lang::from_code(value) {
            Some(lang) => course2md::i18n::set_lang(lang),
            None if value == "auto" => {
                course2md::i18n::init_from_env();
            }
            None => {
                eprintln!(
                    "{}",
                    course2md::tr!(
                        "未知的语言：{}。可用：auto、en、zh-Hans、zh-Hant、ja",
                        value
                    )
                );
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let json = cli.opts.json
        || matches!(&cli.command, Some(Command::RunTask))
        || matches!(
            &cli.command,
            Some(Command::Models {
                cmd: ModelsCmd::Download { json: true, .. }
                    | ModelsCmd::Prepare { json: true, .. }
                    | ModelsCmd::Inspect { json: true, .. }
            })
        );
    let quiet =
        cli.opts.quiet || matches!(&cli.command, Some(Command::Summarize(args)) if args.quiet);
    if json || matches!(cli.command, Some(Command::RunTask)) {
        // Machine consumers parse message text; keep the protocol's source language.
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
            if json {
                progress::emit(&progress::Event::Error { message });
            } else {
                course2md::eprintln_tr!("错误：{}", message);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, no_args: bool) -> anyhow::Result<()> {
    if let Some(platform) = cli.logout {
        return course2md::login::logout(platform);
    }
    if let Some(platform) = cli.login {
        anyhow::ensure!(
            cli.source.as_deref().is_none_or(config::looks_like_source),
            course2md::tr!(
                "--login 后只能跟视频链接或本地视频路径 / After --login, provide only a video URL or local file path"
            )
        );
        course2md::login::login(platform)?;
        if cli.source.is_none() {
            return Ok(());
        }
    }
    match cli.command {
        Some(Command::RunTask) => {
            course2md::dispatch::watch_parent();
            let request = course2md::execution::Request::read(std::io::stdin().lock())?;
            Ok(course2md::runtime::block_on(course2md::execution::run(
                request,
            ))??)
        }
        Some(Command::Models { cmd }) => match cmd {
            ModelsCmd::Inspect { target, json } => {
                let root = configured_model_dir(target.dir.as_deref())?;
                let status = models::status::inspect(target.provider, &target.model, &root)?;
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
                    &target.model,
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
                    course2md::println_tr!("模型准备完成 / Model preparation finished.");
                    print_model_status(&status);
                }
                Ok(())
            }
            ModelsCmd::Download { dir, json } => {
                let root = configured_model_dir(dir.as_deref())?;
                course2md::runtime::block_on(models::download_models(&root))??;
                progress::emit(&progress::Event::ModelReady {
                    operation: progress::ModelOperation::Download,
                    dir: root.clone(),
                    provider: None,
                    model: None,
                    status: None,
                });
                if !json {
                    println!("模型已就绪 / Models ready: {}", root.display());
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
                    println!("已保存并启用 / Saved and enabled: {}", path.display());
                    match llm::test_connection(&cfg.llm) {
                        Ok(()) => course2md::println_tr!("连接测试通过 / Connection test passed."),
                        Err(e) => anyhow::bail!(
                            "配置已保存，但连接测试失败；请检查服务地址、密钥和模型 / Settings saved, but connection test failed; check endpoint, key and model: {e:#}"
                        ),
                    }
                }
                LlmCmd::Status => llm::print_status(&settings::load()?),
                LlmCmd::Disable => {
                    let mut cfg = settings::load()?;
                    cfg.llm.enabled = false;
                    let path = settings::save(&cfg)?;
                    println!(
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
                            course2md::tr!("配置已存在：{}。使用 --force 替换", path.display())
                        );
                    }
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(&path, settings::template())?;
                    println!(
                        "已生成配置模板 / Configuration template created: {}",
                        path.display()
                    );
                    println!("{}", course2md::auth::BILIBILI_SETUP_TIP);
                }
                ConfigCmd::Show => settings::print_effective(&settings::load()?)?,
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
                println!(
                    "没有需要清除的 API 配置 / No saved API settings to clear: {}",
                    path.display()
                );
            } else {
                println!(
                    "已清除 API 配置 / Cleared API settings ({}): {}",
                    cleared.join(", "),
                    path.display()
                );
            }
            if !args.asr {
                course2md::println_tr!(
                    "云端语音识别密钥未清除；如需清除：course2md remove --asr / Cloud speech key kept; to clear it: course2md remove --asr"
                );
            }
            Ok(())
        }
        None => {
            let source = match cli.source {
                Some(source) => normalize_source(&source)?,
                None => {
                    anyhow::ensure!(
                        no_args,
                        course2md::tr!("缺少视频来源。示例：course2md ./lecture.mp4")
                    );
                    println!("{}\n", course2md::tr!("course2md — 将视频转换为图文笔记"));
                    println!("{}", course2md::tr!(course2md::cli::QUICK_START));
                    course2md::println_tr!("全部参数 / All options: course2md --help");
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
            #[cfg_attr(not(apple_native), allow(unused_mut))]
            let mut cfg = course2md::options::resolve(source, &cli.opts, &file)?;
            // coreml 首次使用时先确定模型（可能交互询问），模型缓存准备与转写才会是同一个模型
            #[cfg(apple_native)]
            if cfg.provider == config::AsrProvider::Coreml
                && cfg.transcript_source != config::TranscriptSource::Subtitle
            {
                cfg.asr_model = Some(course2md::apple::resolve_model(cfg.asr_model.as_deref())?);
            }
            tracing::info!(out = %cfg.out_dir.display(), provider = %cfg.provider, "start");
            Ok(course2md::runtime::block_on(pipeline::run(&cfg))??)
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

/// 笔记版本目录：课程目录取 current.json 指向的版本，版本目录直接使用。
fn note_version_dir(dir: &std::path::Path) -> anyhow::Result<Option<std::path::PathBuf>> {
    if dir.join("current.json").is_file() {
        let current: course2md::artifact::CurrentVersion =
            serde_json::from_slice(&std::fs::read(dir.join("current.json"))?)?;
        let manifest = course2md::artifact::safe_asset_path(dir, &current.manifest)?;
        let version = manifest.parent().context(course2md::tr!(
            "笔记清单位置无效 / Invalid note manifest location"
        ))?;
        return Ok(Some(version.to_path_buf()));
    }
    Ok(dir
        .join("manifest.json")
        .is_file()
        .then(|| dir.to_path_buf()))
}

/// 递归收集笔记（可传单份笔记或存放多份笔记的上级目录）；单条目读取失败只告警并跳过。
fn collect_targets(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) {
    match note_version_dir(dir) {
        Ok(Some(version)) => return out.push(version),
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

/// `summarize` 子命令：为一份笔记补写 AI 摘要，保存为新版笔记（幂等，--force 重新生成）。
fn summarize_note(
    file: &settings::ConfigFile,
    dir: &std::path::Path,
    force: bool,
    out: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use course2md::{artifact, execution};
    let base = artifact::read_manifest(&dir.join("manifest.json"))?;
    artifact::validate_version(dir, &base)?;
    let document: artifact::Document =
        serde_json::from_slice(&std::fs::read(dir.join(&base.document))?)?;
    if document.summary.is_some() && !force {
        if !progress::is_quiet() {
            println!(
                "已有摘要，笔记保持不变 / Summary already exists: {}",
                dir.display()
            );
        }
        return Ok(());
    }
    let mut settings = file.llm.clone();
    settings.api_key.clear();
    let mut task_id = format!(
        "summary-{}",
        execution::digest(&serde_json::to_vec(
            &serde_json::json!({"base_version":base.version_id,"settings":settings})
        )?)
    );
    if force {
        task_id.push_str(&format!(
            "-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
    }
    let course_dir = dir
        .parent()
        .and_then(|p| p.parent())
        .context(course2md::tr!(
            "笔记版本位置无效 / Invalid note version location"
        ))?
        .to_path_buf();
    let work = course_dir.join(".work").join(&task_id);
    execution::bind_work_dir(
        &work,
        &serde_json::json!({"source_id":base.source_id,"task_id":task_id,"settings":settings,"base_version":base.version_id}),
    )?;
    let _lock = course2md::runtime::lock_file(&work.join(".task.lock"))?;
    let _dispatch = course2md::dispatch::install(&work, None, &Default::default())?;
    let target = artifact::Target {
        task_id: task_id.clone(),
        course_id: base.course_id.clone(),
        source_id: base.source_id.clone(),
        version_id: task_id,
        course_dir,
    };
    if artifact::published(&target)?.is_some() {
        if !progress::is_quiet() {
            println!(
                "摘要已保存 / Summary already saved: {}",
                target.version_dir().display()
            );
        }
        return Ok(());
    }
    let speech = pipeline::all_speech(&document.sections);
    let summary = course2md::runtime::block_on(course2md::summarize::summarize(
        &file.llm,
        &speech,
        &document.meta,
    ))??;
    let mut outcomes = base.outcomes;
    outcomes.summary = artifact::Outcome::succeeded();
    outcomes.exports.clear();
    course2md::runtime::block_on(artifact::publish(
        &target,
        dir,
        &document.meta,
        &document.sections,
        Some(&summary),
        &[],
        outcomes,
    ))??;
    if let Some(out) = out {
        let file = out.join(format!(
            "{}.summary.md",
            course2md::summarize::sanitize_filename(&document.meta.title)
        ));
        let text = course2md::summarize::render_standalone_md(&document.meta.title, &summary);
        course2md::checkpoint::atomic_write(&file, text.as_bytes())?;
        if !progress::is_quiet() {
            println!("已导出摘要 / Summary exported: {}", file.display());
        }
    }
    if !progress::is_quiet() {
        println!(
            "摘要已作为新版笔记保存 / Summary saved as a new note version: {}",
            target.version_dir().display()
        );
    }
    Ok(())
}

/// `models inspect/prepare` 的人类可读摘要；--json 时仍输出完整状态 JSON。
fn print_model_status(status: &models::status::LocalModelStatus) {
    use models::status::CacheState;
    let state = match status.state {
        CacheState::Missing => course2md::tr!("缺失 / Missing"),
        CacheState::Partial => course2md::tr!("不完整 / Incomplete"),
        CacheState::Cached => course2md::tr!("已缓存（未验证加载）/ Cached (load not verified)"),
        CacheState::Loaded => course2md::tr!("已就绪 / Ready"),
        CacheState::Unsupported => course2md::tr!("不支持 / Unsupported"),
    };
    println!(
        "模型 / Model: {} · {} · {}",
        status.model, status.provider, state
    );
    println!("占用空间 / Disk usage: {}", format_size(status.bytes));
    for part in &status.parts {
        println!("  {}: {}", part.name, part.path.display());
        for file in &part.missing {
            course2md::println_tr!("    缺失：{}", file);
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
            course2md::println_tr!("该后端支持的模型：{}", available)
        }
        _ if status.can_prepare => println!(
            "下载或修复 / Download or repair: course2md models prepare --provider {} --model {}",
            status.provider, status.model
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
            anyhow::anyhow!(course2md::tr!(
                "链接无效；请提供完整的视频链接 / Invalid URL; provide a complete video URL"
            ))
        })?;
        anyhow::ensure!(
            url.host_str().is_some(),
            course2md::tr!("链接缺少主机名 / URL is missing a host")
        );
        return Ok(source.to_owned());
    }
    if config::known_video_site(source).is_some() {
        return Ok(format!("https://{source}"));
    }
    anyhow::bail!(
        "{}\n{}",
        course2md::tr!("找不到视频文件或无法识别链接：{}", source),
        course2md::tr!("请检查路径；含空格的路径需加引号")
    )
}
