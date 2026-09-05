use clap::Parser;
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
    let args: Vec<_> = std::env::args_os().collect();
    let json_requested = args
        .iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "--json");
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() && json_requested {
                progress::set_json_mode();
                progress::emit(
                    serde_json::json!({"type": "error", "message": format!("参数无效 / Invalid arguments: {error}")}),
                );
            } else {
                if error.use_stderr() {
                    eprintln!("参数无效，请参考下方帮助 / Invalid arguments; see the help below.");
                }
                let _ = error.print();
            }
            return std::process::ExitCode::from(error.exit_code() as u8);
        }
    };
    let json = cli.opts.json
        || matches!(
            &cli.command,
            Some(Command::Models {
                cmd: ModelsCmd::Download { json: true, .. }
            })
        );
    if json {
        progress::set_json_mode();
    }
    progress::set_quiet(cli.opts.quiet);
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let message = format!("{error:#}");
            if json {
                progress::emit(serde_json::json!({"type": "error", "message": message}));
            } else {
                eprintln!("错误 / Error: {message}");
                eprintln!(
                    "帮助 / Help: course2md --help · 环境检查 / Check environment: course2md doctor"
                );
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Command::Models { cmd }) => match cmd {
            ModelsCmd::Download { dir, json } => {
                if json {
                    progress::set_json_mode();
                }
                init_logging(0, false, json);
                let file = settings::load()?;
                let root =
                    config::model_dir_from(dir.as_deref().or(file.defaults.model_dir.as_deref()));
                tokio::runtime::Runtime::new()?.block_on(models::download_models(&root))?;
                progress::emit(
                    serde_json::json!({"type":"done", "model_dir": root.display().to_string()}),
                );
                if !json {
                    println!("模型已就绪 / Models ready: {}", root.display());
                }
                Ok(())
            }
            ModelsCmd::List { dir } => {
                init_logging(0, false, false);
                let file = settings::load()?;
                let root =
                    config::model_dir_from(dir.as_deref().or(file.defaults.model_dir.as_deref()));
                models::list_models(&root);
                Ok(())
            }
        },
        Some(Command::Llm { cmd }) => {
            init_logging(0, false, false);
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
                        Ok(()) => println!("连接测试通过 / Connection test passed."),
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
                        "已关闭 AI 润色，凭据保留 / AI proofreading disabled; credentials kept: {}",
                        path.display()
                    );
                }
            }
            Ok(())
        }
        Some(Command::Doctor) => {
            init_logging(0, false, false);
            doctor::run()
        }
        Some(Command::Config { cmd }) => {
            init_logging(0, false, false);
            match cmd {
                ConfigCmd::Init { force } => {
                    let path = settings::config_path();
                    if path.is_file() && !force {
                        anyhow::bail!(
                            "配置已存在 / Config already exists: {}. 使用 --force 替换 / Use --force to replace it.",
                            path.display()
                        );
                    }
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(&path, settings::TEMPLATE)?;
                    println!(
                        "已生成配置模板 / Configuration template created: {}",
                        path.display()
                    );
                    println!(
                        "按需取消注释并修改；命令行参数优先。/ Uncomment and edit as needed; CLI options take priority."
                    );
                }
                ConfigCmd::Show => settings::print_effective(&settings::load()?),
            }
            Ok(())
        }
        Some(Command::Summarize(args)) => {
            init_logging(0, false, false);
            let file = settings::load()?;
            if !file.llm.enabled {
                anyhow::bail!(
                    "AI 总结需要先配置服务 / Set up AI before summarizing: course2md llm setup"
                );
            }
            let rt = tokio::runtime::Runtime::new()?;
            let mut targets: Vec<std::path::PathBuf> = vec![];
            for dir in &args.dirs {
                collect_targets(dir, &mut targets, 0)?;
            }
            if targets.is_empty() {
                anyhow::bail!(
                    "未找到笔记目录 / No note directories containing timeline.jsonl found: {}. 请先转换视频 / Convert a video first.",
                    args.dirs
                        .iter()
                        .map(|d| d.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            for dir in &targets {
                summarize_dir(&file, dir, args.force, args.out.as_deref(), &rt)?;
            }
            Ok(())
        }
        Some(Command::Remove(args)) => {
            init_logging(0, false, false);
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
            cfg.llm.api_key.clear();
            cfg.llm.base_url.clear();
            cfg.llm.model.clear();
            cfg.llm.enabled = false;
            cfg.llm.summarize = false;
            cfg.llm.prompt = None;
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
                println!(
                    "云端语音密钥单独保留；使用 remove --asr 可清除。/ To also clear the cloud speech key, run: course2md remove --asr"
                );
            }
            Ok(())
        }
        None => {
            let source = match cli.source {
                Some(source) => normalize_source(&source)?,
                None => {
                    anyhow::ensure!(
                        std::env::args_os().len() == 1,
                        "缺少视频来源 / Missing video source. 示例 / Example: course2md ./lecture.mp4"
                    );
                    println!(
                        "course2md — 将视频转换为图文笔记 / Turn videos into illustrated notes\n"
                    );
                    println!("{}", course2md::cli::QUICK_START);
                    println!("全部参数 / All options: course2md --help");
                    return Ok(());
                }
            };
            if cli.opts.json {
                progress::set_json_mode();
            }
            init_logging(cli.opts.verbose, cli.opts.quiet, cli.opts.json);
            let file = settings::load()?;
            let initial = course2md::options::resolve(source.clone(), &cli.opts, &file)?;
            initial.validate()?;
            if initial.llm.enabled {
                llm::validate(&initial.llm)?;
            }
            if !std::path::Path::new(&source).is_file() {
                course2md::error::require_cmd("yt-dlp")?;
            }
            // 首次使用向导：无配置文件 + 交互终端时引导配置并写盘（非交互原样返回）；
            // json 模式显式跳过——stdout 必须保持纯 NDJSON，不能混进交互提示
            let file = if cli.opts.json || cli.opts.quiet {
                file
            } else {
                wizard::maybe_run(&cli.opts, &file)?
            };
            let cfg = course2md::options::resolve(source, &cli.opts, &file)?;
            // 全量预检在 pipeline::run 开头做（下载/抽帧/模型加载之前，毫秒级失败）
            tracing::info!(out = %cfg.out_dir.display(), provider = %cfg.provider, "start");
            tokio::runtime::Runtime::new()?.block_on(pipeline::run(&cfg))
        }
    }
}

/// 递归收集包含 timeline.jsonl 的输出目录（支持直接传单个输出目录或整个输出根）。
fn collect_targets(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    depth: usize,
) -> anyhow::Result<()> {
    if dir.join("timeline.jsonl").is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    if depth >= 5 || !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            collect_targets(&entry.path(), out, depth + 1)?;
        }
    }
    Ok(())
}

/// `summarize` 子命令：对已有输出目录补写 LLM 总结（幂等，--force 可覆盖）。
fn summarize_dir(
    file: &settings::ConfigFile,
    dir: &std::path::Path,
    force: bool,
    out: Option<&std::path::Path>,
    rt: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    use course2md::timeline::TimelineEvent;
    let timeline_path = dir.join("timeline.jsonl");
    let md_path = dir.join("course.md");
    let html_path = dir.join("course.html");
    anyhow::ensure!(
        timeline_path.is_file(),
        "缺少笔记时间线 / Missing note timeline: {}. 请指定转换生成的目录 / Use a converted note directory.",
        timeline_path.display()
    );
    let mut events: Vec<course2md::timeline::TranscriptEvent> = vec![];
    for line in std::fs::read_to_string(&timeline_path)?.lines() {
        if let Ok(TimelineEvent::Speech(s)) = serde_json::from_str::<TimelineEvent>(line) {
            events.push(s);
        }
    }
    anyhow::ensure!(
        !events.is_empty(),
        "{} 中没有可总结的文字 / No transcript text to summarize",
        timeline_path.display()
    );

    if !force {
        let has_md = md_path.is_file()
            && course2md::summarize::contains_summary(&std::fs::read_to_string(&md_path)?);
        let has_html = html_path.is_file()
            && course2md::summarize::contains_html_summary(&std::fs::read_to_string(&html_path)?);
        if has_md && has_html {
            println!(
                "已有总结，跳过 / Summary exists; skipped: {}. 使用 --force 替换 / Use --force to replace.",
                dir.display()
            );
            return Ok(());
        }
    }

    let meta = course2md::fetch::VideoMeta {
        title: String::new(),
        uploader: String::new(),
        duration: 0.0,
        webpage_url: String::new(),
        extractor: String::new(),
        id: String::new(),
    };
    let sm = rt.block_on(course2md::summarize::summarize(&file.llm, &events, &meta))?;

    if md_path.is_file() {
        let md = std::fs::read_to_string(&md_path)?;
        let md = if course2md::summarize::contains_summary(&md) {
            course2md::summarize::strip_md_summary(&md)
        } else {
            md
        };
        std::fs::write(&md_path, course2md::summarize::insert_into_md(&md, &sm))?;
    }
    if html_path.is_file() {
        let html = std::fs::read_to_string(&html_path)?;
        let html = if course2md::summarize::contains_html_summary(&html) {
            course2md::summarize::strip_html_summary(&html)
        } else {
            html
        };
        std::fs::write(
            &html_path,
            course2md::summarize::insert_into_html(&html, &sm),
        )?;
    }
    // 可选：导出独立总结文件到 -o 指定目录
    if let Some(out_dir) = out {
        std::fs::create_dir_all(out_dir)?;
        // 输出目录形如 .../<标题>/<id>，取父目录名作为标题
        let title = dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "视频".into());
        let fname = format!(
            "{}.summary.md",
            course2md::summarize::sanitize_filename(&title)
        );
        let target = out_dir.join(fname);
        std::fs::write(
            &target,
            course2md::summarize::render_standalone_md(&title, &sm),
        )?;
        println!("已导出总结 / Summary exported: {}", target.display());
    }
    println!(
        "已写入总结 / Summary saved ({} key points / {} outline sections): {}",
        sm.key_points.len(),
        sm.outline.len(),
        dir.display()
    );
    Ok(())
}

/// Accept existing files and normalize URLs pasted without a scheme.
fn normalize_source(source: &str) -> anyhow::Result<String> {
    if std::path::Path::new(source).is_file() {
        return Ok(source.to_owned());
    }
    let source = source.trim();
    if source.starts_with("http://") || source.starts_with("https://") {
        let url = url::Url::parse(source).map_err(|_| {
            anyhow::anyhow!(
                "链接无效；请提供完整的视频链接 / Invalid URL; provide a complete video URL"
            )
        })?;
        anyhow::ensure!(
            url.host_str().is_some(),
            "链接缺少主机名 / URL is missing a host"
        );
        return Ok(source.to_owned());
    }
    if [
        "bilibili.com/",
        "www.bilibili.com/",
        "youtube.com/",
        "www.youtube.com/",
        "youtu.be/",
    ]
    .iter()
    .any(|host| source.starts_with(host))
    {
        return Ok(format!("https://{source}"));
    }
    anyhow::bail!(
        "找不到视频文件或无法识别链接 / Video file not found or URL not recognized: {source}\n请检查路径；含空格的路径需加引号 / Check the path and quote paths containing spaces."
    )
}
