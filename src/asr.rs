//! 语音识别：ffmpeg 静音检测分段 + llama.cpp（Qwen3-ASR）。
//!
//! 通过 `llama-server` 常驻进程走 GPU（macOS Metal / NVIDIA CUDA / CPU），
//! 跨平台只依赖 PATH 上的 llama.cpp。

use crate::config::PipelineConfig;
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------- 超参数（审查后统一收拢到文件顶部） ----------

/// ffmpeg 静音检测：低于 -28dB 且持续 0.4s 视为静音（课件停顿切分点）
const SILENCEDETECT_AF: &str = "silencedetect=noise=-28dB:d=0.4";
/// llama-server 上下文长度（token）：chunk 已被 VAD 限长在 max_speech 内，4096 足够
const LLAMA_CTX: &str = "4096";
/// llama-server 单 chunk 生成上限（token）
const LLAMA_MAX_GEN: &str = "256";
/// 采样参数：转写要确定性输出（temperature 0）。
/// max_tokens 与 server 的 -n 对齐；apple CoreML shim 用 448，因为那是
/// speech-swift 侧 Qwen3 模型的既有默认，两条后端各自调优，勿盲目对齐。
const LLAMA_TEMPERATURE: f64 = 0.0;
const LLAMA_MAX_TOKENS: u32 = 256;
/// llama-server /health 就绪等待上限（首次加载模型较慢）
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(300);
/// 云端 STT 单次请求超时
const API_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// llama-server 单 chunk 转写超时
const LLAMA_HTTP_TIMEOUT: Duration = Duration::from_secs(180);
/// 云端 STT 并发 worker 数：网络往返是主要瓶颈
const WORKERS: usize = 4;
/// HTTP 重试：最多 3 次（1 次首发 + 2 次重试），指数退避 1s → 2s；
/// 4xx 是确定性错误（鉴权/参数）不重试，5xx 与网络错误重试
const MAX_ATTEMPTS: u32 = 3;
const RETRY_BACKOFF_BASE: Duration = Duration::from_secs(1);

/// 清洗 Qwen3 转写中的提示词残留。
pub fn sanitize_qwen_text(s: &str) -> String {
    let mut t = s.trim();
    if let Some(p) = t.find("</asr_text>") {
        t = t[..p].trim();
    }
    if let Some(p) = t.rfind("<asr_text>") {
        t = t[p + "<asr_text>".len()..].trim();
    }
    let lower = t.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("language ")
        && let Some(i) = rest.find(char::is_whitespace)
    {
        // 原串对齐
        let skip = "language ".len() + i + 1;
        if skip <= t.len() {
            t = t[skip..].trim();
        }
    }
    t.to_string()
}

/// 打开 checkpoint → spawn_blocking 执行 → 成功才 finish → join。
/// 四个 provider 分支共享这套骨架（coreml 此前写法不一致，统一为「成功才 finish」）。
async fn run_with_cp<F>(
    cfg: &PipelineConfig,
    identity: &crate::checkpoint::AsrIdentity,
    f: F,
) -> Result<Vec<TranscriptEvent>>
where
    F: FnOnce(&mut crate::checkpoint::Checkpoint) -> Result<Vec<TranscriptEvent>> + Send + 'static,
{
    let mut cp = crate::checkpoint::Checkpoint::open(&cfg.out_dir, cfg.resume, identity)?;
    tokio::task::spawn_blocking(move || {
        let r = f(&mut cp);
        if r.is_ok() {
            cp.finish()?;
        }
        r
    })
    .await
    .context("ASR 线程 join 失败")?
}

pub async fn run(cfg: &PipelineConfig, wav: &std::path::Path) -> Result<Vec<TranscriptEvent>> {
    use crate::checkpoint::AsrIdentity;
    use crate::config::AsrProvider;

    if cfg.provider == AsrProvider::Api {
        // 同一模型在 transcriptions / chat 两种端点下的输出可能不同，身份须含模式
        let model_id = format!("{}:{}", cfg.asr_api.mode, cfg.asr_api.model);
        let id = AsrIdentity::new("api", &model_id, cfg.max_speech);
        let api = cfg.asr_api.clone();
        let max_speech = cfg.max_speech as f64;
        let wav = wav.to_path_buf();
        return run_with_cp(cfg, &id, move |cp| run_api(&api, &wav, max_speech, cp)).await;
    }
    if cfg.provider == AsrProvider::Npu {
        let model = crate::npu::resolve_npu_model(cfg.asr_model.as_deref());
        let id = AsrIdentity::new("npu", &model, cfg.max_speech);
        let max_speech = cfg.max_speech as f64;
        let wav = wav.to_path_buf();
        return run_with_cp(cfg, &id, move |cp| {
            crate::npu::run_npu(&model, &wav, max_speech, cp)
        })
        .await;
    }
    if cfg.provider == AsrProvider::Coreml {
        #[cfg(apple_native)]
        {
            let wav = wav.to_path_buf();
            let max_speech = cfg.max_speech as f64;
            let model = crate::apple::resolve_model(
                cfg.asr_model.as_deref().filter(|s| !s.trim().is_empty()),
            )?;
            let id = AsrIdentity::new("coreml", &model, cfg.max_speech);
            let joined = run_with_cp(cfg, &id, move |cp| {
                let tmp = crate::runtime::TempWorkDir::new("asr")?;
                crate::apple::run_coreml(&wav, max_speech, &model, tmp.path(), cp)
            })
            .await;
            match joined {
                Ok(events) => return Ok(events), // 空 = VAD 无语音（终态，不再回落）
                Err(e) => tracing::warn!("CoreML 后端失败（{e:#}），回落 llama-server"),
            }
        }
        #[cfg(not(apple_native))]
        {
            anyhow::bail!(
                "此构建未包含 Apple CoreML 后端（仅 macOS Apple Silicon 构建支持）。请用 --provider gpu 或 cpu"
            );
        }
    }
    // 剩余 Gpu/Cpu 走 llama-server（Metal/CUDA/Vulkan/CPU）
    let offload = OffloadOpts {
        provider: cfg.provider,
        gpu_layers: cfg.gpu_layers,
        mmproj_offload: cfg.mmproj_offload,
    };
    let threads = cfg.threads;
    let max_speech = cfg.max_speech;
    let llama = crate::models::ensure_llama_or_download(&cfg.model_dir).await?;
    // coreml 回落场景：身份随实际转写后端（llama/qwen3），旧 coreml 进度作废，
    // 避免同一 checkpoint 混入两个模型的转写文本。
    let id = AsrIdentity::new("llama", crate::models::llama_gguf_identity(), cfg.max_speech);
    let model = llama.model;
    let mmproj = llama.mmproj;
    let wav = wav.to_path_buf();
    run_with_cp(cfg, &id, move |cp| {
        run_blocking(&wav, &model, &mmproj, offload, threads, max_speech, cp)
    })
    .await
}

fn run_blocking(
    wav: &Path,
    model: &Path,
    mmproj: &Path,
    offload: OffloadOpts,
    threads: i32,
    max_speech: f32,
    cp: &mut crate::checkpoint::Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let segs = ffmpeg_vad(wav, max_speech)?;
    tracing::info!(segs = segs.len(), "vad");
    if segs.is_empty() {
        tracing::warn!("未检测到语音（VAD 结果为空），跳过识别");
        return Ok(vec![]);
    }

    let bin = find_llama_server()?;
    let port = crate::runtime::free_port()?;
    let caps = llama_server_caps(&bin);
    // 用户要求 mmproj 留 CPU 但 llama.cpp 太旧不认识该 flag 时，不能静默忽略
    if !offload.mmproj_offload && !caps.no_mmproj_offload {
        tracing::warn!(
            "当前 llama-server 不支持 --no-mmproj-offload（旧版 llama.cpp），\
             mmproj 仍会卸载到 GPU；升级 llama.cpp 后生效"
        );
    }
    let args = build_server_args(model, mmproj, offload, caps, threads, port);
    tracing::info!(
        bin = %bin.display(),
        port,
        gpu_layers = offload.gpu_layers,
        mmproj_offload = offload.mmproj_offload,
        "llama-server"
    );
    tracing::debug!(args = %args.join(" "), "llama-server spawn");
    // 存档实际 spawn 参数：失败 run.json 会附带（诊断 GPU hang 的关键证据）
    if let Ok(mut g) = LAST_LLAMA_SPAWN_ARGS.lock() {
        *g = Some(args.clone());
    }
    let mut child = spawn_server(&bin, &args)?;
    let stderr_tail = child
        .take_stderr()
        .map(|s| crate::runtime::drain_stderr(s, "llama_server"))
        .unwrap_or_default();
    let base = format!("http://127.0.0.1:{port}");
    // 子进程秒退（端口冲突/模型损坏）会立即报错，而不是等满 300s；
    // 失败时附上 llama-server 自己的 stderr 尾部，诊断信息不因 piped 而丢失
    if let Err(e) = crate::runtime::wait_ready(
        &base,
        SERVER_READY_TIMEOUT,
        &mut child,
        Some("\"status\":\"ok\""),
    ) {
        return Err(e.context(format!(
            "llama-server 启动失败，其 stderr 尾部：\n{}",
            stderr_tail.tail()
        )));
    }
    tracing::info!(
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "server ready"
    );

    // 共享 agent（连接复用），不再每个 chunk 新建
    let client = ureq::AgentBuilder::new()
        .timeout(LLAMA_HTTP_TIMEOUT)
        .build();
    let tmp = crate::runtime::TempWorkDir::new("asr")?;
    let r = run_chunks(wav, &segs, cp, tmp.path(), "asr", |_i, _seg, chunk| {
        transcribe_file(&client, &base, chunk).map(|t| {
            let t = sanitize_qwen_text(&t);
            (!t.is_empty()).then_some(t)
        })
    });
    // llama-server 退出清理双保险（issue #12：孤儿进程占着 /dev/kfd 会加剧 ROCm 问题）：
    // 成功路径在这里主动 kill+wait；错误路径（? 早退 / panic unwind 经过的 drop）
    // 由 ManagedChild 的 Drop 兜底 kill+wait，任何离开 run_blocking 的路径都不孤儿。
    child.kill();
    let _ = child.wait();
    let events = r.map_err(|e| {
        // 转写中途失败大概率是 server 侧问题，附上 stderr 尾部便于定位
        let tail = stderr_tail.tail();
        if tail.is_empty() {
            e
        } else {
            e.context(format!("llama-server stderr 尾部：\n{}", tail))
        }
    })?;
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(events)
}

/// 顺序 chunk 执行器：统一切音频、断点跳过、进度条、记录（含空结果）、
/// chunk 清理与收尾排序。backend 只需提供「chunk 文件 → 文本」函数。
/// Ok(None) = 后端确认无语音内容（同样记录完成，避免静音段反复重跑）。
pub(crate) fn run_chunks(
    wav: &Path,
    segs: &[Seg],
    cp: &mut crate::checkpoint::Checkpoint,
    tmp_dir: &Path,
    label: &str,
    mut transcribe: impl FnMut(usize, Seg, &Path) -> Result<Option<String>>,
) -> Result<Vec<TranscriptEvent>> {
    let pb = indicatif::ProgressBar::new(segs.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(&format!(
            "{{spinner:.green}} {label} {{pos}}/{{len}} [{{bar:32.cyan/blue}}] {{elapsed}} {{msg}}"
        ))
        .unwrap()
        .progress_chars("##-"),
    );

    let mut err: Option<anyhow::Error> = None;
    for (i, seg) in segs.iter().copied().enumerate() {
        let (start, end) = (seg.start, seg.end);
        if cp.is_done(start, end) {
            pb.inc(1);
            continue; // 断点续跑：该 chunk 上次已完成
        }
        let chunk = tmp_dir.join(format!("c{i:04}.wav"));
        if let Err(e) = cut_wav(wav, seg.cut_start, seg.cut_end, &chunk) {
            err = Some(e);
            break;
        }
        match transcribe(i, seg, &chunk) {
            Ok(text) => {
                // 空结果也记录完成；写盘失败则中断且不标记完成
                if let Err(e) = cp.record(start, end, text.as_deref().unwrap_or("")) {
                    err = Some(e);
                    break;
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&chunk);
                err = Some(e);
                break;
            }
        }
        let _ = std::fs::remove_file(&chunk);
        pb.inc(1);
    }
    pb.finish_and_clear();
    if let Some(e) = err {
        return Err(e);
    }
    // 事件统一来自 checkpoint（历史 + 本次），按时间排序
    let mut all: Vec<TranscriptEvent> = cp.events().to_vec();
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(all)
}

/// 云端 STT：ffmpeg VAD 分段 + 逐段 POST /audio/transcriptions（OpenAI 兼容 / OpenRouter）。
fn run_api(
    api: &crate::settings::AsrApi,
    wav: &Path,
    max_speech: f64,
    cp: &mut crate::checkpoint::Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    // key 解析（非递归）：配置 > 非空环境变量；空值不覆盖（防无限递归）
    let api_key = if !api.api_key.trim().is_empty() {
        api.api_key.clone()
    } else {
        crate::config::asr_api_key_from_env()
            .context("云端 STT 未配置 API Key：在配置文件 [asr_api] 设置 api_key，或用 --asr-api-key / COURSE2MD_ASR_API_KEY（兼容旧名 OPENROUTER_API_KEY）")?
    };
    let segs = ffmpeg_vad(wav, max_speech as f32)?;
    tracing::info!(segs = segs.len(), endpoint = %api.base_url, model = %api.model, "api vad");
    if segs.is_empty() {
        tracing::warn!("未检测到语音（VAD 结果为空），跳过识别");
        return Ok(vec![]);
    }

    let tmp = crate::runtime::TempWorkDir::new("asr")?;
    let base = api.base_url.trim().trim_end_matches('/');
    let url = match api.mode {
        crate::settings::AsrApiMode::Transcriptions => format!("{base}/audio/transcriptions"),
        crate::settings::AsrApiMode::Chat => format!("{base}/chat/completions"),
    };
    let pb = indicatif::ProgressBar::new(segs.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let client = ureq::AgentBuilder::new()
        .timeout(API_HTTP_TIMEOUT)
        .build();
    // 断点续跑：预先过滤出未完成的 chunk（worker 只拿真正需要执行的任务）
    let pending: Vec<usize> = (0..segs.len())
        .filter(|&i| !cp.is_done(segs[i].start, segs[i].end))
        .collect();
    pb.set_position((segs.len() - pending.len()) as u64);

    // 有界并发（std::thread::scope + 借用，无需 Arc）：网络往返是主要瓶颈；
    // 结果经 channel 回收后记录。abort 后 in-flight 请求自然结束，无人 join 不到。
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<Option<String>>)>();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let abort = std::sync::atomic::AtomicBool::new(false);
    let mut err: Option<anyhow::Error> = None;
    std::thread::scope(|s| {
        for _ in 0..WORKERS {
            let tx = tx.clone();
            let target = ApiTarget {
                client: &client,
                url: &url,
                model: &api.model,
                key: &api_key,
                mode: api.mode,
            };
            let (tmp_dir, wav) = (tmp.path(), wav);
            let (segs, pending, next, abort) = (&segs, &pending, &next, &abort);
            s.spawn(move || loop {
                if abort.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let idx = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(i) = pending.get(idx).copied() else {
                    break;
                };
                let seg = segs[i];
                let r = transcribe_api(
                    &target,
                    &tmp_dir.join(format!("c{i:04}.wav")),
                    seg,
                    wav,
                );
                if tx.send((i, r)).is_err() {
                    break;
                }
            });
        }
        drop(tx);

        for (i, r) in rx {
            match r {
                Ok(text) => {
                    // 空结果（None）同样记录完成，避免静音 chunk 反复重跑
                    if let Err(e) =
                        cp.record(segs[i].start, segs[i].end, text.as_deref().unwrap_or(""))
                    {
                        if err.is_none() {
                            err = Some(e);
                        }
                        abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    if err.is_none() {
                        err = Some(e.context(format!("云端 STT 失败（chunk {i}）")));
                        abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            pb.inc(1);
        }
    });
    pb.finish_and_clear();
    if let Some(e) = err {
        return Err(e);
    }
    // 事件统一来自 checkpoint（收集循环里已 record），按时间排序
    let mut events: Vec<TranscriptEvent> = cp.events().to_vec();
    events.sort_by(|a, b| a.start.total_cmp(&b.start));
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(events)
}

/// POST JSON（带重试）：网络错误与 5xx 按 RETRY_BACKOFF_BASE 指数退避、
/// 共尝试 MAX_ATTEMPTS 次；4xx 是确定性错误（鉴权/参数），重试无意义直接失败。
fn post_json_retry(
    agent: &ureq::Agent,
    url: &str,
    key: Option<&str>,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut delay = RETRY_BACKOFF_BASE;
    for attempt in 1..=MAX_ATTEMPTS {
        let mut req = agent.post(url);
        if let Some(k) = key {
            req = req.set("Authorization", &format!("Bearer {k}"));
        }
        match req.send_json(body.clone()) {
            Ok(resp) => return resp.into_json().context("响应解析失败"),
            Err(e) => {
                let retryable = match &e {
                    ureq::Error::Status(code, _) => *code >= 500,
                    ureq::Error::Transport(_) => true,
                };
                if retryable && attempt < MAX_ATTEMPTS {
                    tracing::warn!(
                        attempt,
                        backoff_secs = delay.as_secs(),
                        "请求失败（{e}），稍后重试"
                    );
                    std::thread::sleep(delay);
                    delay *= 2;
                } else {
                    return Err(anyhow::anyhow!("请求失败: {e}"));
                }
            }
        }
    }
    unreachable!()
}

/// chat 模式下让多模态 LLM 转录的指令（保持与本地 ASR 输出风格一致：忠实、带自然标点）。
const CHAT_TRANSCRIBE_PROMPT: &str = "请将这段音频转录为文字。只输出转录内容本身（保留自然标点），\
不要添加任何解释、概括或格式标记。若没有语音内容，输出空字符串。";

/// 云端转录端点：地址、凭据与请求模式（worker 间共享借用）。
struct ApiTarget<'a> {
    client: &'a ureq::Agent,
    url: &'a str,
    model: &'a str,
    key: &'a str,
    mode: crate::settings::AsrApiMode,
}

/// 转写单个 chunk；Ok(None) = 无语音内容。
fn transcribe_api(
    t: &ApiTarget,
    chunk: &Path,
    seg: Seg,
    wav: &Path,
) -> Result<Option<String>> {
    use base64::Engine as _;
    cut_wav(wav, seg.cut_start, seg.cut_end, chunk).context("切分音频失败")?;
    let bytes =
        std::fs::read(chunk).with_context(|| format!("读取 chunk {}", chunk.display()))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let body = match t.mode {
        crate::settings::AsrApiMode::Transcriptions => serde_json::json!({
            "model": t.model,
            "input_audio": {"data": b64, "format": "wav"},
        }),
        crate::settings::AsrApiMode::Chat => serde_json::json!({
            "model": t.model,
            "temperature": 0.0,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": CHAT_TRANSCRIBE_PROMPT},
                    {"type": "input_audio", "input_audio": {"data": b64, "format": "wav"}},
                ],
            }],
        }),
    };
    let v = post_json_retry(t.client, t.url, Some(t.key), &body)?;
    if let Some(e) = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        anyhow::bail!("API 报错: {e}");
    }
    let text = match t.mode {
        crate::settings::AsrApiMode::Transcriptions => {
            v["text"].as_str().unwrap_or("").trim().to_string()
        }
        crate::settings::AsrApiMode::Chat => parse_chat_content(&v).trim().to_string(),
    };
    let _ = std::fs::remove_file(chunk);
    Ok(if text.is_empty() { None } else { Some(text) })
}

/// 从 chat/completions 响应取文本：content 通常是字符串；部分多模态端点
/// 返回 [{type:"text", text:...}] 分片数组，拼起来即可。
fn parse_chat_content(v: &serde_json::Value) -> String {
    let content = &v["choices"][0]["message"]["content"];
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn find_llama_server() -> Result<PathBuf> {
    crate::runtime::which("llama-server")
        .context("找不到 llama-server，请安装 llama.cpp 并加入 PATH")
}

/// `llama-server --help` 探测超时：进程启动很快，5s 足够；超时按不支持处理
const HELP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// llama-server 对 offload 相关新 flag 的支持情况（issue #12）。
/// --no-mmproj-offload / --no-op-offload / --device 都是新版 llama.cpp 才有的；
/// 盲目附加会让发行版仓库的旧 llama-server 直接启动失败，故逐条按 --help 探测。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LlamaServerCaps {
    /// `--device`（cpu 后端用于 `--device none` 禁用 GPU 设备）
    pub device: bool,
    /// `--no-op-offload`（禁止把 host 算子卸载到 GPU）
    pub no_op_offload: bool,
    /// `--no-mmproj-offload`（多模态 projector 留在 CPU）
    pub no_mmproj_offload: bool,
}

/// 从 `--help` 文本解析支持情况（纯函数，便于单测）。
fn caps_from_help(help: &str) -> LlamaServerCaps {
    LlamaServerCaps {
        device: help.contains("--device"),
        no_op_offload: help.contains("--no-op-offload"),
        no_mmproj_offload: help.contains("--no-mmproj-offload"),
    }
}

/// 探测一次并缓存（OnceLock：llama-server 在进程生命周期内不会换版本）。
/// 探测失败（旧版无 --help 文本、超时、启动失败）→ 全 false，只保留 `-ngl`，
/// 不阻断主流程。
fn llama_server_caps(bin: &Path) -> LlamaServerCaps {
    static CAPS: std::sync::OnceLock<LlamaServerCaps> = std::sync::OnceLock::new();
    *CAPS.get_or_init(|| match probe_help_text(bin) {
        Some(text) => {
            let caps = caps_from_help(&text);
            tracing::debug!(
                device = caps.device,
                no_op_offload = caps.no_op_offload,
                no_mmproj_offload = caps.no_mmproj_offload,
                "llama-server flag 探测"
            );
            caps
        }
        None => {
            tracing::debug!("llama-server --help 探测失败，按旧版处理（仅 -ngl）");
            LlamaServerCaps::default()
        }
    })
}

/// 运行 `llama-server --help`，合并 stdout/stderr 文本；超时就杀掉并返回 None。
/// 帮助文本约几十 KB，小于管道缓冲区（64KB），先 wait 后读不会死锁。
fn probe_help_text(bin: &Path) -> Option<String> {
    let mut child = Command::new(bin)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + HELP_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
    use std::io::Read;
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut text);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut text);
    }
    Some(text)
}

/// llama-server GPU 卸载控制（由 PipelineConfig 解析而来，issue #12）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct OffloadOpts {
    pub provider: crate::config::AsrProvider,
    pub gpu_layers: u32,
    pub mmproj_offload: bool,
}

/// 最近一次 llama-server spawn 的完整参数（诊断用：失败 run.json 附带，issue #12）。
/// 单进程同一时刻只有一个 llama-server 实例，静态足够。
static LAST_LLAMA_SPAWN_ARGS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// 最近一次 llama-server spawn 的参数（未 spawn 过 → None），供失败诊断记录。
pub(crate) fn last_llama_spawn_args() -> Option<Vec<String>> {
    LAST_LLAMA_SPAWN_ARGS.lock().ok().and_then(|g| g.clone())
}

/// 组装 llama-server 启动参数（纯函数，便于单测；issue #12）：
/// - gpu 后端：`-ngl <gpu_layers>`；mmproj_offload=false 且 llama.cpp 支持时
///   追加 `--no-mmproj-offload`
/// - cpu 后端：`-ngl 0`，并按探测结果追加 `--device none --no-op-offload
///   --no-mmproj-offload`——新版 llama.cpp 下仅 -ngl 0 仍可能把 mmproj/部分
///   算子卸载到 GPU
/// - 不支持的 flag（caps=false）一律不加，兼容发行版仓库的旧 llama-server
fn build_server_args(
    model: &Path,
    mmproj: &Path,
    offload: OffloadOpts,
    caps: LlamaServerCaps,
    threads: i32,
    port: u16,
) -> Vec<String> {
    let cpu_only = offload.provider == crate::config::AsrProvider::Cpu;
    let ngl = if cpu_only { 0 } else { offload.gpu_layers };
    let mut args: Vec<String> = vec![
        "-m".into(),
        model.display().to_string(),
        "--mmproj".into(),
        mmproj.display().to_string(),
        "-ngl".into(),
        ngl.to_string(),
        "-c".into(),
        LLAMA_CTX.into(),
        "-n".into(),
        LLAMA_MAX_GEN.into(),
        "-t".into(),
        threads.to_string(),
        "--port".into(),
        port.to_string(),
        "--host".into(),
        "127.0.0.1".into(),
    ];
    if cpu_only {
        if caps.device {
            args.extend(["--device".into(), "none".into()]);
        }
        if caps.no_op_offload {
            args.push("--no-op-offload".into());
        }
        if caps.no_mmproj_offload {
            args.push("--no-mmproj-offload".into());
        }
    } else if !offload.mmproj_offload && caps.no_mmproj_offload {
        args.push("--no-mmproj-offload".into());
    }
    args
}

fn spawn_server(bin: &Path, args: &[String]) -> Result<crate::runtime::ManagedChild> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdout(Stdio::null())
        // stderr 不能 inherit：llama-server 每个 chunk 都打 slot timing 日志，
        // 会插在进度条重绘中间，破坏 indicatif 的原地更新（issue #4）。
        // 改为 piped + 后台 drain，尾部缓存用于失败诊断，debug 日志可转发。
        .stderr(Stdio::piped());
    crate::runtime::ManagedChild::spawn("llama-server", &mut cmd)
}

fn transcribe_file(client: &ureq::Agent, base: &str, wav: &Path) -> Result<String> {
    let bytes = std::fs::read(wav)?;
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let body = serde_json::json!({
        "temperature": LLAMA_TEMPERATURE,
        "max_tokens": LLAMA_MAX_TOKENS,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Transcribe the audio."},
                {"type": "input_audio", "input_audio": {"data": b64, "format": "wav"}}
            ]
        }]
    });
    let v = post_json_retry(client, &format!("{base}/v1/chat/completions"), None, &body)
        .context("llama-server 识别请求失败")?;
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        anyhow::bail!("llama-server 返回空文本: {v}");
    }
    Ok(text)
}

pub(crate) fn ffmpeg_vad(wav: &Path, max_speech: f32) -> Result<Vec<Seg>> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-i"])
        .arg(wav)
        .args(["-af", SILENCEDETECT_AF, "-f", "null", "-"])
        .output()
        .context("ffmpeg silencedetect")?;
    if !out.status.success() {
        anyhow::bail!(
            "ffmpeg silencedetect 失败（{}）：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    let log = String::from_utf8_lossy(&out.stderr);
    // 时长只探测一次，传入 normalize_segments（此前两处各 ffprobe 一次）；
    // 失败至少告警——静默落 0.0 会让末段语音丢失。
    let dur = match crate::media::probe_duration_blocking(wav) {
        Some(d) => d,
        None => {
            tracing::warn!("ffprobe 时长探测失败，按 0 处理（末段语音可能丢失）");
            0.0
        }
    };
    let mut silences: Vec<(f64, f64)> = vec![];
    let mut start: Option<f64> = None;
    for line in log.lines() {
        if let Some(v) = line.split("silence_start:").nth(1) {
            match v.trim().parse::<f64>() {
                Ok(t) => start = Some(t),
                // 解析失败不能静默丢弃：0.0 会产生倒挂区间进 invert_silence
                Err(e) => tracing::warn!("silencedetect silence_start 解析失败（{v:?}）：{e}"),
            }
        } else if let Some(v) = line.split("silence_end:").nth(1) {
            match v.split_whitespace().next().unwrap_or("").parse::<f64>() {
                Ok(end) => {
                    if let Some(s) = start.take() {
                        silences.push((s, end));
                    }
                }
                Err(e) => tracing::warn!("silencedetect silence_end 解析失败（{v:?}）：{e}"),
            }
        }
    }
    normalize_segments(invert_silence(dur, &silences), max_speech as f64, wav, dur)
}

/// 最终送入 ASR 的分段：`start/end` 是事件时间（用于时间线），`cut_*` 是切音频范围（含静音填充）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub start: f64,
    pub end: f64,
    pub cut_start: f64,
    pub cut_end: f64,
}

/// 音频逐 100ms 的 RMS 能量（用于在目标切点附近找最近的静音最低点）。
pub struct Energy {
    hop: f64,
    rms: Vec<f32>,
}

impl Energy {
    pub fn load(wav: &Path) -> Result<Self> {
        // 直接解析 16k 单声道 s16 wav（extract_audio 的固定产物）
        let data = std::fs::read(wav).with_context(|| format!("读取音频失败 {}", wav.display()))?;
        let (body, _) =
            find_pcm_body(&data).ok_or_else(|| anyhow::anyhow!("无法解析 wav PCM 数据"))?;
        let mut samples = Vec::with_capacity(body.len() / 2);
        for c in body.as_chunks::<2>().0 {
            samples.push(i16::from_le_bytes(*c));
        }
        const HOP: usize = 1600; // 100ms @16k
        let mut rms = Vec::with_capacity(samples.len() / HOP + 1);
        for ch in samples.chunks(HOP) {
            let s: f64 = ch
                .iter()
                .map(|&v| (v as f64 / 32768.0).powi(2))
                .sum::<f64>()
                / ch.len() as f64;
            rms.push((s.sqrt()) as f32);
        }
        Ok(Self { hop: 0.1, rms })
    }

    /// [a,b]（秒）内能量最低的时刻；无数据时返回 None。
    fn quietest(&self, a: f64, b: f64) -> Option<f64> {
        let i0 = (a / self.hop).ceil() as usize;
        let i1 = (b / self.hop).floor() as usize;
        if i1 <= i0 || i0 >= self.rms.len() {
            return None;
        }
        let i1 = i1.min(self.rms.len() - 1);
        // total_cmp：rms 来自非负能量的 sqrt，理论上无 NaN，但不赌 partial_cmp 不 panic
        let (bi, _) = self.rms[i0..=i1]
            .iter()
            .enumerate()
            .min_by(|x, y| x.1.total_cmp(y.1))?;
        Some((i0 + bi) as f64 * self.hop + self.hop / 2.0)
    }
}

/// 跳过 wav 头，返回 (PCM body, sample_rate)。
fn find_pcm_body(data: &[u8]) -> Option<(&[u8], u32)> {
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let mut rate = 0;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        match id {
            b"fmt " => {
                if pos + 8 + 16 <= data.len() {
                    rate = u32::from_le_bytes([
                        data[pos + 12],
                        data[pos + 13],
                        data[pos + 14],
                        data[pos + 15],
                    ]);
                }
            }
            b"data" => {
                let end = (pos + 8 + size).min(data.len());
                return Some((&data[pos + 8..end], rate));
            }
            _ => {}
        }
        pos += 8 + size + (size & 1); // chunk 对齐
    }
    None
}

const PAD: f64 = 0.25; // 切音频时向两侧静音各延展的秒数
const SPLIT_WINDOW: f64 = 3.0; // 在目标切点 ± 此窗口内寻找静音最低点
const MIN_PIECE: f64 = 1.0; // 硬切产生的最短片段

/// VAD 后处理：能量感知切分 + 静音填充。
/// - 超过 max_speech 的段在 [target-3s, target+3s] 窗口内选能量最低点切（避开词中切断）
/// - 切音频时向两侧静音各填充 0.25s（只进静音、不进相邻语音，故无重复文本）
///
/// `dur` 由调用方一次性探测传入（避免每段各 ffprobe 一次）。
pub fn normalize_segments(
    speech: Vec<(f64, f64)>,
    max_speech: f64,
    wav: &Path,
    dur: f64,
) -> Result<Vec<Seg>> {
    let energy = Energy::load(wav).ok();
    // VAD 成功但无语音（或全被短段过滤）→ 空分段。
    // 不再把整段音频当语音兜底：静音课件会诱发 ASR 幻觉。
    let mut raw = speech;
    raw.retain(|(a, b)| b - a >= 0.2);
    if raw.is_empty() {
        return Ok(vec![]);
    }

    let mut pieces: Vec<(f64, f64)> = vec![];
    for &(s, e) in &raw {
        split_smart(s, e, max_speech, energy.as_ref(), &mut pieces);
    }

    // 填充：VAD 外边界向真实静音扩展 0.25s（限制来自相邻原始语音段，而非本段自身）；
    // max_speech 内部切点已在能量最低点，不额外填充（避免相邻 chunk 重复文本）。
    let segs = pieces
        .iter()
        .map(|&(s, e)| {
            let host_idx = raw
                .iter()
                .position(|&(rs, re)| s >= rs - 1e-6 && e <= re + 1e-6);
            // 该 piece 是否是其所在 raw 段的第一片/最后一片（外边界才能 pad）
            let (is_first_of_host, is_last_of_host) = match host_idx {
                Some(h) => {
                    let (rs, re) = raw[h];
                    let first = (s - rs).abs() < 1e-6;
                    let last = (re - e).abs() < 1e-6;
                    (first, last)
                }
                None => (true, true),
            };
            // 前一个原始语音段的终点（跨段静音的上限）
            let speech_lo = match host_idx {
                Some(h) if h > 0 => raw[h - 1].1,
                _ => 0.0,
            };
            let speech_hi = match host_idx {
                Some(h) if h + 1 < raw.len() => raw[h + 1].0,
                _ => dur,
            };
            let cut_start = if is_first_of_host {
                (s - PAD).max(speech_lo).max(0.0)
            } else {
                s
            };
            let cut_end = if is_last_of_host {
                let hi = if dur > 0.0 { speech_hi } else { e + PAD };
                (e + PAD).min(hi)
            } else {
                e
            };
            Seg {
                start: s,
                end: e,
                cut_start,
                cut_end: cut_end.max(e),
            }
        })
        .collect();
    Ok(segs)
}

/// 递归切分：优先在静音最低点切，找不到则回退硬切。
fn split_smart(s: f64, e: f64, max: f64, energy: Option<&Energy>, out: &mut Vec<(f64, f64)>) {
    if e - s <= max {
        out.push((s, e));
        return;
    }
    let target = s + max;
    // 只在 [target-3s, target] 内找静音最低点：任何 piece 都不超过 max（硬上限，
    // ASR 后端常有上下文长度限制）
    let w0 = (target - SPLIT_WINDOW).max(s + MIN_PIECE.min(max / 2.0));
    let w1 = target;
    let cut = energy
        .and_then(|en| en.quietest(w0.max(s), w1))
        .unwrap_or(target);
    let cut = cut.clamp(s + 0.5, target);
    out.push((s, cut));
    split_smart(cut, e, max, energy, out);
}

fn invert_silence(dur: f64, sil: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut t = 0.0;
    let mut out = vec![];
    for &(s, e) in sil {
        if s > t + 0.15 {
            out.push((t, s));
        }
        t = e.max(t);
    }
    if dur > t + 0.15 {
        out.push((t, dur));
    }
    out
}

pub fn cut_wav(src: &Path, start: f64, end: f64, dest: &Path) -> Result<()> {
    let dur = (end - start).max(0.05);
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss"])
        .arg(format!("{start:.3}"))
        .arg("-t")
        .arg(format!("{dur:.3}"))
        .arg("-i")
        .arg(src)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(dest)
        .status()
        .context("ffmpeg cut")?;
    if !st.success() {
        anyhow::bail!("ffmpeg 切分失败");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_and_vad_invert() {
        assert_eq!(
            sanitize_qwen_text("**language Chinese<asr_text>你好世界。"),
            "你好世界。"
        );
        assert_eq!(sanitize_qwen_text("内容</asr_text>尾巴"), "内容");
        let s = invert_silence(10.0, &[(0.0, 1.0), (4.0, 5.0)]);
        assert!((s[0].0 - 1.0).abs() < 1e-6 && (s[0].1 - 4.0).abs() < 1e-6);
    }

    #[test]
    fn chat_content_parsing() {
        // 字符串形式（主流 OpenAI 兼容端点）
        let v = serde_json::json!({"choices":[{"message":{"content":"你好世界"}}]});
        assert_eq!(parse_chat_content(&v), "你好世界");
        // 分片数组形式（部分多模态端点）
        let v = serde_json::json!({"choices":[{"message":{"content":[
            {"type":"text","text":"你好"},
            {"type":"text","text":"世界"}
        ]}}]});
        assert_eq!(parse_chat_content(&v), "你好世界");
        // 缺字段/异常响应 → 空串（调用方按无语音处理）
        let v = serde_json::json!({"choices":[]});
        assert_eq!(parse_chat_content(&v), "");
    }

    // ---------- issue #12：GPU 卸载控制 ----------

    fn args_of(
        provider: crate::config::AsrProvider,
        gpu_layers: u32,
        mmproj_offload: bool,
        caps: LlamaServerCaps,
    ) -> Vec<String> {
        let offload = OffloadOpts {
            provider,
            gpu_layers,
            mmproj_offload,
        };
        build_server_args(Path::new("/m.gguf"), Path::new("/mm.gguf"), offload, caps, 4, 8081)
    }

    /// 取出 `-ngl` 后面的值
    fn ngl_of(args: &[String]) -> String {
        args[args.iter().position(|a| a == "-ngl").unwrap() + 1].clone()
    }

    #[test]
    fn args_gpu_backend() {
        use crate::config::AsrProvider::Gpu;
        let full = LlamaServerCaps {
            device: true,
            no_op_offload: true,
            no_mmproj_offload: true,
        };
        // 默认：全量卸载，mmproj 也卸载 → 无额外 flag
        let a = args_of(Gpu, 99, true, full);
        assert_eq!(ngl_of(&a), "99");
        assert!(!a.contains(&"--no-mmproj-offload".to_string()));
        // 限制层数 + mmproj 留 CPU
        let a = args_of(Gpu, 8, false, full);
        assert_eq!(ngl_of(&a), "8");
        assert!(a.contains(&"--no-mmproj-offload".to_string()));
        // 旧版 llama.cpp（无 caps）：mmproj_offload=false 也不加 flag，避免启动失败
        let a = args_of(Gpu, 8, false, LlamaServerCaps::default());
        assert_eq!(ngl_of(&a), "8");
        assert!(!a.contains(&"--no-mmproj-offload".to_string()));
        // gpu 后端即使探测到新 flag 也不加 --device/--no-op-offload
        assert!(!a.contains(&"--device".to_string()));
        assert!(!a.contains(&"--no-op-offload".to_string()));
    }

    #[test]
    fn args_cpu_backend() {
        use crate::config::AsrProvider::Cpu;
        let full = LlamaServerCaps {
            device: true,
            no_op_offload: true,
            no_mmproj_offload: true,
        };
        // 新版 llama.cpp：彻底禁用 GPU 的三件套全部附加（mmproj_offload=true 也一样，
        // cpu 后端的语义就是任何部分都不上 GPU）
        for mmproj_offload in [true, false] {
            let a = args_of(Cpu, 99, mmproj_offload, full);
            assert_eq!(ngl_of(&a), "0");
            let i = a.iter().position(|x| x == "--device").unwrap();
            assert_eq!(a[i + 1], "none");
            assert!(a.contains(&"--no-op-offload".to_string()));
            assert!(a.contains(&"--no-mmproj-offload".to_string()));
        }
        // 旧版 llama.cpp（--help 无新 flag）：只保留 -ngl 0
        let a = args_of(Cpu, 99, false, LlamaServerCaps::default());
        assert_eq!(ngl_of(&a), "0");
        assert!(!a.contains(&"--device".to_string()));
        assert!(!a.contains(&"--no-op-offload".to_string()));
        assert!(!a.contains(&"--no-mmproj-offload".to_string()));
        // 部分支持：只附加探测到的
        let a = args_of(
            Cpu,
            99,
            false,
            LlamaServerCaps {
                device: false,
                no_op_offload: true,
                no_mmproj_offload: false,
            },
        );
        assert!(!a.contains(&"--device".to_string()));
        assert!(a.contains(&"--no-op-offload".to_string()));
        assert!(!a.contains(&"--no-mmproj-offload".to_string()));
    }

    #[test]
    fn caps_parsing() {
        // 新版 llama.cpp（含全部 offload 控制 flag）
        let new_help = "\
            -dev,  --device <dev1,dev2,..>   comma-separated list of devices to use for offloading\n\
            --op-offload, --no-op-offload    whether to offload host tensor operations to device\n\
            --mmproj-offload, --no-mmproj-offload   whether to enable GPU offloading for multimodal projector\n";
        let caps = caps_from_help(new_help);
        assert_eq!(
            caps,
            LlamaServerCaps {
                device: true,
                no_op_offload: true,
                no_mmproj_offload: true,
            }
        );
        // 旧版 llama.cpp：只有 -ngl，没有新 flag
        let old_help = "\
            -ngl,  --gpu-layers N   number of layers to store in VRAM\n\
            -m MODEL  model path\n";
        assert_eq!(caps_from_help(old_help), LlamaServerCaps::default());
        // 空输出 / 探测失败文本
        assert_eq!(caps_from_help(""), LlamaServerCaps::default());
    }
}
