use clap::Parser;
use course2md::cli::{Cli, Command, ConfigCmd, LlmCmd, ModelsCmd, RunOpts};
use course2md::{config, doctor, llm, models, pipeline, progress, settings, wizard};
use tracing_subscriber::EnvFilter;

fn init_logging(verbose: u8, quiet: bool, json: bool) {
    let default = if quiet {
        "error"
    } else if verbose >= 2 {
        "debug"
    } else {
        "info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into());
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
            .with_target(true)
            .init();
    } else {
        // 默认档：个人 CLI 时间戳是纯噪音，compact 单行
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .without_time()
            .compact()
            .init();
    }
}

/// 配置文件 + CLI 覆盖 -> 生效 LLM 设置。
fn resolve_llm(opts: &RunOpts, file: &settings::ConfigFile) -> llm::LlmSettings {
    let mut s = file.llm.clone();
    if opts.no_llm {
        s.enabled = false;
    } else if opts.llm {
        s.enabled = true;
    }
    if opts.no_llm_vision {
        s.vision = false;
    } else if opts.llm_vision {
        s.vision = true;
    }
    if let Some(v) = &opts.llm_base_url {
        s.base_url = v.clone();
    }
    if let Some(v) = &opts.llm_api_key {
        s.api_key = v.clone();
    }
    if let Some(v) = &opts.llm_model {
        s.model = v.clone();
    }
    if let Some(v) = &opts.llm_prompt {
        s.prompt = Some(v.clone());
    }
    if opts.no_llm_hint {
        s.disable_hint = true;
    }
    s
}

/// 优先级：CLI 显式参数 > 配置文件 [defaults] > 内置默认。
fn run_opts_to_cfg(
    source: String,
    opts: &RunOpts,
    file: &settings::ConfigFile,
) -> anyhow::Result<config::PipelineConfig> {
    let d = &file.defaults;
    use config::{OutputFormat, SlideMode};
    let out_root = opts
        .out
        .clone()
        .or_else(|| d.out.clone())
        .map(config::expand_tilde)
        .unwrap_or_else(|| config::DEFAULT_OUT_DIR.into());
    Ok(config::PipelineConfig {
        url: source,
        // out_dir 此处只是初值；pipeline 里会改写为 {out_root}/{platform}/{title}/{id}/
        out_dir: out_root.clone(),
        out_root,
        similarity: opts.similarity.or(d.similarity).unwrap_or(config::DEFAULT_SIMILARITY),
        sample_interval: opts
            .sample_interval
            .or(d.sample_interval)
            .unwrap_or(config::DEFAULT_SAMPLE_INTERVAL),
        cooldown: opts.cooldown.or(d.cooldown).unwrap_or(config::DEFAULT_COOLDOWN),
        max_height: opts
            .max_height
            .or(d.max_height)
            .unwrap_or(config::DEFAULT_MAX_HEIGHT)
            .clamp(240, 2160),
        slide_mode: opts
            .slide_mode
            .or(d.slide_mode)
            .unwrap_or(SlideMode::Stable),
        stable_secs: opts
            .stable_secs
            .or(d.stable_secs)
            .unwrap_or(config::DEFAULT_STABLE_SECS)
            .clamp(0.0, 10.0),
        roi: match &opts.roi {
            Some(s) => Some(config::Roi::parse(s)?),
            None => match &d.roi {
                Some(s) => Some(config::Roi::parse(s)?),
                None => None,
            },
        },
        threads: opts.threads.or(d.threads).unwrap_or(config::DEFAULT_THREADS),
        provider: opts
            .provider
            .or(d.provider)
            .unwrap_or_else(config::default_provider_hint),
        max_speech: opts
            .max_speech
            .or(d.max_speech)
            .unwrap_or(config::DEFAULT_MAX_SPEECH),
        formats: opts
            .formats
            .clone()
            .or_else(|| d.formats.clone())
            .unwrap_or_else(|| vec![OutputFormat::Md, OutputFormat::Html]),
        model_dir: config::model_dir_from(opts.model_dir.as_deref().or(d.model_dir.as_deref())),
        keep_video: opts.keep_video || d.keep_video.unwrap_or(false),
        no_download: opts.no_download || d.no_download.unwrap_or(false),
        resume: config::resolve_resume(opts.resume, opts.no_resume, d.resume),
        llm: resolve_llm(opts, file),
        asr_api: resolve_asr_api(opts, file),
        asr_model: opts.asr_model.clone().or_else(|| d.asr_model.clone()),
        transcript_source: opts
            .transcript_source
            .or(d.transcript_source)
            .unwrap_or_default(),
    })
}

/// 云端 STT 配置合并：CLI > 配置文件 > 默认（OpenRouter）。
fn resolve_asr_api(opts: &RunOpts, file: &settings::ConfigFile) -> crate::settings::AsrApi {
    let mut a = file.asr_api.clone();
    if let Some(v) = &opts.asr_api_base_url {
        a.base_url = v.clone();
    }
    if let Some(v) = &opts.asr_api_key {
        a.api_key = v.clone();
    }
    if let Some(v) = &opts.asr_api_model {
        a.model = v.clone();
    }
    if let Some(v) = opts.asr_api_mode {
        a.mode = v;
    }
    a
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Models { cmd }) => match cmd {
            ModelsCmd::Download { dir, json } => {
                if json {
                    progress::set_json_mode();
                }
                init_logging(0, false, json);
                let root = config::model_dir_from(dir.as_deref());
                tokio::runtime::Runtime::new()?.block_on(models::download_models(&root))?;
                Ok(())
            }
            ModelsCmd::List { dir } => {
                init_logging(0, false, false);
                let root = config::model_dir_from(dir.as_deref());
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
                    println!("已写入并开启：{}", path.display());
                    match llm::test_connection(&cfg.llm) {
                        Ok(()) => println!("连接测试通过。"),
                        Err(e) => eprintln!("连接测试未通过（已保存配置）：{e:#}"),
                    }
                }
                LlmCmd::Status => llm::print_status(&settings::load()?),
                LlmCmd::Disable => {
                    let mut cfg = settings::load()?;
                    cfg.llm.enabled = false;
                    let path = settings::save(&cfg)?;
                    println!("已关闭 LLM 润色（凭据保留）：{}", path.display());
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
                        anyhow::bail!("配置文件已存在：{}（--force 覆盖）", path.display());
                    }
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(&path, settings::TEMPLATE)?;
                    println!("已生成配置模板：{}", path.display());
                    println!("按需取消注释并修改；命令行参数优先于此文件。");
                }
                ConfigCmd::Show => settings::print_effective(&settings::load()?),
            }
            Ok(())
        }
        Some(Command::Summarize(args)) => {
            init_logging(0, false, false);
            let file = settings::load()?;
            if !file.llm.enabled {
                anyhow::bail!("LLM 未启用：请先运行 course2md llm setup 配置 API Key");
            }
            let rt = tokio::runtime::Runtime::new()?;
            let mut targets: Vec<std::path::PathBuf> = vec![];
            for dir in &args.dirs {
                collect_targets(dir, &mut targets, 0)?;
            }
            if targets.is_empty() {
                anyhow::bail!(
                    "未找到包含 timeline.jsonl 的输出目录：{}",
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
                println!("没有需要清除的 API 配置：{}", path.display());
            } else {
                println!("已清除 API 配置（{}）：{}", cleared.join(", "), path.display());
            }
            if !args.asr {
                println!("提示：--asr 可同时清除云端 STT（[asr_api]）的 API Key。");
            }
            Ok(())
        }
        None => {
            let source = match cli.source {
                Some(s) if config::looks_like_source(&s) => s,
                _ => {
                    use clap::CommandFactory;
                    let mut cmd = Cli::command();
                    cmd.print_help()?;
                    std::process::exit(2);
                }
            };
            if cli.opts.json {
                progress::set_json_mode();
            }
            init_logging(cli.opts.verbose, cli.opts.quiet, cli.opts.json);
            let file = settings::load()?;
            // 首次使用向导：无配置文件 + 交互终端时引导配置并写盘（非交互原样返回）；
            // json 模式显式跳过——stdout 必须保持纯 NDJSON，不能混进交互提示
            let file = if cli.opts.json {
                file
            } else {
                wizard::maybe_run(&cli.opts, &file)?
            };
            let cfg = run_opts_to_cfg(source, &cli.opts, &file)?;
            // 全量预检在 pipeline::run 开头做（下载/抽帧/模型加载之前，毫秒级失败）
            tracing::info!(out = %cfg.out_dir.display(), provider = %cfg.provider, "start");
            let result = tokio::runtime::Runtime::new()?.block_on(pipeline::run(&cfg));
            if let Err(e) = &result {
                // json 模式：传播前先把错误作为协议事件发出去（human 模式 emit 为 no-op）
                progress::emit(serde_json::json!({
                    "type": "error",
                    "message": format!("{e:#}"),
                }));
            }
            result
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
        "缺少 {}（不是有效的 course2md 输出目录？）",
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
        "{} 中没有语音事件",
        timeline_path.display()
    );

    if !force {
        let has_md = md_path.is_file()
            && course2md::summarize::contains_summary(&std::fs::read_to_string(&md_path)?);
        let has_html = html_path.is_file()
            && course2md::summarize::contains_html_summary(&std::fs::read_to_string(&html_path)?);
        if has_md && has_html {
            println!("跳过（已有总结，--force 可覆盖）：{}", dir.display());
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
        println!("已导出总结：{}", target.display());
    }
    println!(
        "已写入总结（要点 {} 条 / 大纲 {} 节）：{}",
        sm.key_points.len(),
        sm.outline.len(),
        dir.display()
    );
    Ok(())
}
