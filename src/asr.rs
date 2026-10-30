//! 语音识别：ffmpeg 静音检测分段 + llama.cpp（Qwen3-ASR）。
//!
//! 通过 `llama-server` 常驻进程走 GPU（macOS Metal / NVIDIA CUDA / CPU），
//! 跨平台只依赖 PATH 上的 llama.cpp。

use crate::config::PipelineConfig;
use crate::progress::Stage;
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------- 超参数（VAD 切分参数见 normalize_segments 附近） ----------

/// ffmpeg 静音检测：低于 -28dB 且持续 0.4s 视为静音（课件停顿切分点）
const SILENCEDETECT_AF: &str = "silencedetect=noise=-28dB:d=0.4";
/// llama-server 上下文长度（token）：chunk 已被 VAD 限长在 max_speech 内，4096 足够
const LLAMA_CTX: &str = "4096";
/// 单 chunk 生成上限（token），server 的 -n 与请求的 max_tokens 共用。
/// apple CoreML shim 用 448（speech-swift 侧 Qwen3 的既有默认），两条后端各自调优。
const LLAMA_MAX_TOKENS: u32 = 256;
/// 采样参数：转写要确定性输出
const LLAMA_TEMPERATURE: f64 = 0.0;
/// HTTP 请求最多尝试次数（1 次首发 + 2 次重试）
const MAX_ATTEMPTS: u32 = 3;
/// `llama-server --help` / `--list-devices` 探测超时
const HELP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const GPU_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// llama-server /health 就绪等待上限（首次加载模型较慢）
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(300);
/// 云端 STT 单次请求超时
const API_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// llama-server 单 chunk 转写超时
const LLAMA_HTTP_TIMEOUT: Duration = Duration::from_secs(180);
/// ffmpeg silencedetect 上限：整段音频 VAD 只做一次，但不得无限挂死
const VAD_TIMEOUT: Duration = Duration::from_secs(900);
/// ffmpeg 单段切音频上限（chunk 最长 max_speech 秒，处理应远快于此）
const CUT_TIMEOUT: Duration = Duration::from_secs(120);
/// 云端 STT 并发 worker 数：网络往返是主要瓶颈
const WORKERS: usize = 4;
/// 空结果熔断：VAD 已判定这些分片含语音，端点却在没有任何成功的情况下
/// 连续返回空文本——端点实际上无法处理音频输入（配置错误/网关丢音频体），
/// 继续只会空烧请求。单片为空仍是合法静音（见 record 处注释）。
const EMPTY_RESULT_TRIPWIRE: usize = 10;

/// 空结果熔断器：缓冲空结果，出现任何非空结果即复位（端点正常，静音属实）；
/// 空结果连续达到阈值时熔断。缓冲的空结果由调用方在确认不熔断后再落 checkpoint，
/// 避免熔断后空记录把分片标成"已完成静音"，修好之后重跑也救不回来。
#[derive(Default)]
struct EmptyTripwire {
    pending_empty: usize,
    seen_success: bool,
}

impl EmptyTripwire {
    /// 记录一条结果，返回是否已熔断。
    fn record(&mut self, empty: bool) -> bool {
        if empty {
            self.pending_empty += 1;
        } else {
            self.seen_success = true;
        }
        !self.seen_success && self.pending_empty >= EMPTY_RESULT_TRIPWIRE
    }

    fn tripped_error(&self) -> anyhow::Error {
        anyhow::anyhow!(
            "语音服务连续 {} 个分片返回空文本；请检查语音服务地址与模型是否支持音频输入 / The speech service returned empty text for {} consecutive segments; check that the endpoint and model support audio input",
            self.pending_empty,
            self.pending_empty
        )
    }
}

/// 空结果熔断 + 缓冲落账（run_chunks 串行路径与 run_api 并行路径共用一套语义）：
/// 每个 chunk 结果先经 tripwire 计数，连续空结果熔断时**不落任何 checkpoint**
/// （空记录会把分片标成「已完成静音」，掩盖真实的识别失败）；空结果先缓冲，
/// 出现非空结果（或正常收尾）才批量落账。并行路径结果乱序到达也安全：
/// 乱序只改变 pending 的相对顺序，批量落账全部写同一空文本，checkpoint 内容一致。
#[derive(Default)]
struct EmptyResultTracker {
    tripwire: EmptyTripwire,
    pending_empty: Vec<(f64, f64)>,
}

impl EmptyResultTracker {
    /// 处理一个 chunk 的转写结果：非空结果落账，空结果缓冲。
    /// Err = 需要中止（熔断触发或 checkpoint 写盘失败），错误原样上抛。
    fn record_outcome(
        &mut self,
        cp: &mut crate::checkpoint::Checkpoint,
        start: f64,
        end: f64,
        text: &str,
    ) -> Result<()> {
        let empty = text.trim().is_empty();
        if self.tripwire.record(empty) {
            // 熔断不落账（见 struct 注释）
            return Err(self.tripwire.tripped_error());
        }
        if empty {
            self.pending_empty.push((start, end));
            return Ok(());
        }
        self.flush_pending(cp)?;
        cp.record(start, end, text)
    }

    /// 正常收尾：把缓冲的空结果（真实静音）落 checkpoint，避免静音 chunk 反复重跑。
    /// 出错（含熔断）时调用方不得调用本方法——pending 必须随任务失败丢弃。
    fn finish(self, cp: &mut crate::checkpoint::Checkpoint) -> Result<()> {
        let mut this = self;
        this.flush_pending(cp)
    }

    /// 批量落账缓冲的空结果；写盘失败即中断，剩余 pending 保留待重跑。
    fn flush_pending(&mut self, cp: &mut crate::checkpoint::Checkpoint) -> Result<()> {
        for (start, end) in std::mem::take(&mut self.pending_empty) {
            cp.record(start, end, "")?;
        }
        Ok(())
    }
}

/// 清洗 Qwen3 转写中的提示词残留。
pub fn sanitize_qwen_text(s: &str) -> String {
    let mut t = s.trim();
    if let Some(p) = t.find("</asr_text>") {
        t = t[..p].trim();
    }
    if let Some(p) = t.rfind("<asr_text>") {
        t = t[p + "<asr_text>".len()..].trim();
    }
    const LANGUAGE_PREFIX: &str = "language ";
    if t.get(..LANGUAGE_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(LANGUAGE_PREFIX))
        && let Some((_, rest)) = t[LANGUAGE_PREFIX.len()..].split_once(char::is_whitespace)
    {
        t = rest.trim();
    }
    t.to_string()
}

/// 打开 checkpoint → spawn_blocking 执行 → 成功才 finish → join（各 provider 共用）。
async fn run_with_cp<F>(
    work_dir: &Path,
    identity: &crate::checkpoint::AsrIdentity,
    f: F,
) -> Result<Vec<TranscriptEvent>>
where
    F: FnOnce(&mut crate::checkpoint::Checkpoint) -> Result<Vec<TranscriptEvent>> + Send + 'static,
{
    // A work directory belongs to one task identity (--no-resume picks a new one),
    // so the progress saved in it is always this task's to resume.
    let mut cp = crate::checkpoint::Checkpoint::open(work_dir, true, identity)?;
    tokio::task::spawn_blocking(move || {
        let r = f(&mut cp);
        if r.is_ok() {
            cp.finish()?;
        }
        r
    })
    .await
    .context(crate::tr!(
        "语音识别任务未能完成 / Speech recognition task did not complete"
    ))?
}

pub async fn run(
    cfg: &PipelineConfig,
    wav: &std::path::Path,
    job: &crate::execution::Job,
) -> Result<Vec<TranscriptEvent>> {
    use crate::checkpoint::AsrIdentity;
    use crate::config::AsrProvider;
    let control = &job.control;

    if cfg.provider == AsrProvider::Api {
        // 同一模型在 transcriptions / chat 两种端点下的输出可能不同，身份须含模式
        let model_id = format!(
            "{}:{}:{}",
            cfg.asr_api.base_url.trim().trim_end_matches('/'),
            cfg.asr_api.mode,
            cfg.asr_api.model
        );
        let id = AsrIdentity::new("api", &model_id, cfg.max_speech);
        let api = cfg.asr_api.clone();
        let max_speech = cfg.max_speech as f64;
        let wav = wav.to_path_buf();
        let control = control.clone();
        return run_with_cp(&job.dir, &id, move |cp| {
            run_api(&api, &wav, max_speech, cp, &control)
        })
        .await;
    }
    if cfg.provider == AsrProvider::Npu {
        let model = crate::npu::resolve_npu_model(cfg.asr_model.as_deref());
        let id = AsrIdentity::new("npu", &model, cfg.max_speech);
        let max_speech = cfg.max_speech as f64;
        let wav = wav.to_path_buf();
        let control = control.clone();
        return run_with_cp(&job.dir, &id, move |cp| {
            crate::npu::run_npu(&model, &wav, max_speech, cp, &control)
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
            let control = control.clone();
            let joined = run_with_cp(&job.dir, &id, move |cp| {
                let tmp = crate::runtime::TempWorkDir::new("asr")?;
                crate::apple::run_coreml(&wav, max_speech, &model, tmp.path(), cp, &control)
            })
            .await;
            return joined.context(crate::tr!("已选的 Apple 识别方式未完成；任务参数和进度已保留 / Selected Apple transcription failed; task settings retained"));
        }
        #[cfg(not(apple_native))]
        {
            anyhow::bail!(crate::tr!(
                "此版本不支持 Apple 识别，请改用 GPU 或 CPU 识别 / This build lacks Apple transcription; use GPU or CPU transcription."
            ));
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
    let llama = crate::models::ensure_llama_or_download(&cfg.model_dir, control).await?;
    // CPU/GPU paths share the same concrete GGUF model identity.
    let id = AsrIdentity::new(
        "llama",
        &crate::models::llama_gguf_identity(),
        cfg.max_speech,
    );
    let server = LlamaServer {
        model: llama.model,
        mmproj: llama.mmproj,
        offload,
        threads,
    };
    let wav = wav.to_path_buf();
    let control = control.clone();
    run_with_cp(&job.dir, &id, move |cp| {
        run_blocking(&wav, &server, max_speech, cp, &control)
    })
    .await
}

/// The local llama-server a transcription starts: model files and placement.
struct LlamaServer {
    model: PathBuf,
    mmproj: PathBuf,
    offload: OffloadOpts,
    threads: i32,
}

fn run_blocking(
    wav: &Path,
    server: &LlamaServer,
    max_speech: f32,
    cp: &mut crate::checkpoint::Checkpoint,
    control: &crate::dispatch::TaskControl,
) -> Result<Vec<TranscriptEvent>> {
    let offload = server.offload;
    let t0 = Instant::now();
    let segs = ffmpeg_vad(wav, max_speech)?;
    tracing::info!(segs = segs.len(), "vad");
    if segs.is_empty() {
        return Ok(no_speech_events());
    }

    let bin = find_llama_server()?;
    if offload.provider != crate::config::AsrProvider::Cpu {
        let devices = gpu_devices(&bin)?;
        anyhow::ensure!(
            !devices.is_empty(),
            "选择了 GPU 识别，但没有找到可用的显卡 / GPU transcription is selected, but no usable GPU was found. {}",
            GPU_SETUP_HINT
        );
        tracing::info!(devices = %devices.join(", "), "GPU backend detected");
    }
    let port = crate::runtime::free_port()?;
    let caps = llama_server_caps(&bin);
    let cpu_only = offload.provider == crate::config::AsrProvider::Cpu;
    if cpu_only && !(caps.device && caps.no_op_offload && caps.no_mmproj_offload) {
        let devices = gpu_devices(&bin).context(
            crate::tr!("当前 llama.cpp 版本过旧，无法只用 CPU 识别，请升级后重试 / This llama.cpp is too old to transcribe on CPU only; upgrade and retry"),
        )?;
        anyhow::ensure!(
            devices.is_empty(),
            crate::tr!(
                "当前 llama.cpp 版本过旧，无法只用 CPU 识别，请升级后重试 / This llama.cpp is too old to transcribe on CPU only; upgrade and retry"
            )
        );
    } else if !cpu_only && !offload.mmproj_offload {
        anyhow::ensure!(
            caps.no_mmproj_offload,
            crate::tr!(
                "当前 llama.cpp 版本过旧，不支持此显卡设置，请升级后重试 / This llama.cpp is too old for this GPU setting; upgrade and retry"
            )
        );
    }
    let args = build_server_args(server, caps, port);
    tracing::info!(
        bin = %bin.display(),
        port,
        gpu_layers = if cpu_only { 0 } else { offload.gpu_layers },
        mmproj_offload = !cpu_only && offload.mmproj_offload,
        "llama-server"
    );
    tracing::debug!(args = %args.join(" "), "llama-server spawn");
    crate::progress::start(Stage::ModelLoad);
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
        Some("ok"),
        Some(&|| control.check()),
    ) {
        if e.downcast_ref::<crate::dispatch::Failure>().is_some() {
            return Err(e); // 暂停/取消不是启动故障
        }
        return Err(stderr_tail.attach(
            e,
            crate::tr!("无法启动识别服务 / Could not start the speech recognition service"),
        ));
    }
    tracing::info!(
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "server ready"
    );

    crate::progress::done(Stage::ModelLoad);

    // 共享 agent：各 chunk 复用连接
    let client = crate::runtime::http_agent_keeping_errors(LLAMA_HTTP_TIMEOUT);
    let tmp = crate::runtime::TempWorkDir::new("asr")?;
    let r = run_chunks(wav, &segs, cp, tmp.path(), "asr", control, |chunk| {
        transcribe_file(&client, &base, chunk, control).map(|t| {
            let t = sanitize_qwen_text(&t);
            (!t.is_empty()).then_some(t)
        })
    });
    // llama-server 退出清理双保险（孤儿进程会占住 GPU 设备）：
    // 成功路径在这里主动 kill+wait；错误路径（? 早退 / panic unwind 经过的 drop）
    // 由 ManagedChild 的 Drop 兜底 kill+wait，任何离开 run_blocking 的路径都不孤儿。
    child.kill();
    let _ = child.wait();
    let events = r.map_err(|e| {
        // 转写中途失败大概率是 server 侧问题，附上 stderr 与启动参数便于定位
        stderr_tail
            .attach(
                e,
                crate::tr!("识别服务错误详情 / llama-server error details"),
            )
            .context(format!(
                "llama-server 启动参数 / arguments: {}",
                args.join(" ")
            ))
    })?;
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(events)
}

/// 所有后端共用的「VAD 后没有语音」收尾：只保留截图，并记录空结果。
pub(crate) fn no_speech_events() -> Vec<TranscriptEvent> {
    tracing::warn!(
        "未检测到语音，将仅保留截图 / No speech detected; keeping slides without a transcript"
    );
    Vec::new()
}

/// 顺序 chunk 执行器：统一切音频、断点跳过、进度条、记录（含空结果）、
/// chunk 清理与收尾排序。backend 只需提供「chunk 文件 → 文本」函数。
/// Ok(None) = 后端确认无语音内容（同样记录完成，避免静音段反复重跑）。
///
/// 双缓冲流水线：转写当前 chunk 期间由后台线程预切下一个未完成 chunk
///（ffmpeg 切音频约 30-80ms，不占用串行路径）；失败语义与 checkpoint
/// record 顺序与纯串行版完全一致。
pub(crate) fn run_chunks(
    wav: &Path,
    segs: &[Seg],
    cp: &mut crate::checkpoint::Checkpoint,
    tmp_dir: &Path,
    label: &str,
    control: &crate::dispatch::TaskControl,
    mut transcribe: impl FnMut(&Path) -> Result<Option<String>>,
) -> Result<Vec<TranscriptEvent>> {
    let pb = crate::progress::Bar::new(Stage::Transcribe, segs.len() as u64).with_template(&format!(
        "{{spinner:.green}} {label} {{pos}}/{{len}} [{{bar:32.cyan/blue}}] {{elapsed}} {{eta}} {{msg}}"
    ));

    let mut err: Option<anyhow::Error> = None;
    let mut tracker = EmptyResultTracker::default();
    std::thread::scope(|scope| {
        let mut prefetch: Option<(usize, std::thread::ScopedJoinHandle<'_, Result<()>>)> = None;
        for (i, seg) in segs.iter().copied().enumerate() {
            control.check()?;
            let (start, end) = (seg.start, seg.end);
            if cp.is_done(start, end) {
                pb.inc(1);
                continue; // 断点续跑：该 chunk 上次已完成
            }
            let chunk = tmp_dir.join(format!("c{i:04}.wav"));
            // 取本 chunk 的预切结果；未预切（首个待处理 chunk）则现切
            let cut = match prefetch.take() {
                Some((j, handle)) => {
                    debug_assert_eq!(j, i, "预切游标与当前 chunk 对齐");
                    handle.join().unwrap_or_else(|_| {
                        Err(anyhow::anyhow!(crate::tr!(
                            "音频切分线程异常终止 / Audio split thread terminated unexpectedly"
                        )))
                    })
                }
                None => cut_wav(wav, seg.cut_start, seg.cut_end, &chunk),
            };
            if let Err(e) = cut {
                err = Some(e);
                break;
            }
            // 预切下一个未完成 chunk（与本 chunk 的转写并行）
            if let Some(j) = (i + 1..segs.len()).find(|&j| !cp.is_done(segs[j].start, segs[j].end))
            {
                let next_seg = segs[j];
                let next_chunk = tmp_dir.join(format!("c{j:04}.wav"));
                prefetch = Some((
                    j,
                    scope.spawn(move || {
                        cut_wav(wav, next_seg.cut_start, next_seg.cut_end, &next_chunk)
                    }),
                ));
            }
            match transcribe(&chunk) {
                Ok(text) => {
                    let text = text.as_deref().unwrap_or("");
                    if let Err(e) = tracker.record_outcome(cp, start, end, text) {
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
        Ok::<(), anyhow::Error>(())
    })?;
    pb.finish();
    // 正常收尾：缓冲的空结果（真实静音）落 checkpoint
    if err.is_none()
        && let Err(e) = tracker.finish(cp)
    {
        err = Some(e);
    }
    if let Some(e) = err {
        return Err(e);
    }
    // 事件统一来自 checkpoint（历史 + 本次），按时间排序
    Ok(cp.sorted_events())
}

/// 云端 STT：ffmpeg VAD 分段 + 逐段 POST /audio/transcriptions（OpenAI 兼容 / OpenRouter）。
fn run_api(
    api: &crate::settings::AsrApi,
    wav: &Path,
    max_speech: f64,
    cp: &mut crate::checkpoint::Checkpoint,
    control: &crate::dispatch::TaskControl,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    // 凭据已在任务预检时解析（含环境变量）；空 key = 明确选择的无鉴权服务
    let api_key = api.api_key.trim().to_string();
    let segs = ffmpeg_vad(wav, max_speech as f32)?;
    tracing::info!(segs = segs.len(), endpoint = %api.base_url, model = %api.model, "api vad");
    if segs.is_empty() {
        return Ok(no_speech_events());
    }

    let tmp = crate::runtime::TempWorkDir::new("asr")?;
    let url = crate::config::asr_endpoint(api)?;
    let pb = crate::progress::Bar::new(Stage::Transcribe, segs.len() as u64).with_template(
        "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {eta} {msg}",
    );

    let client = crate::runtime::http_agent_keeping_errors(API_HTTP_TIMEOUT);
    // 断点续跑：预先过滤出未完成的 chunk（worker 只拿真正需要执行的任务）
    let pending: Vec<usize> = (0..segs.len())
        .filter(|&i| !cp.is_done(segs[i].start, segs[i].end))
        .collect();
    pb.set_position((segs.len() - pending.len()) as u64);
    crate::progress::emit(&crate::progress::Event::Workers {
        stage: Stage::Transcribe,
        workers: WORKERS.min(pending.len()),
    });

    // Network round trips dominate, so requests run on the shared worker
    // pool. Each result is recorded into the checkpoint as it arrives (resume
    // keeps finished chunks); the first error or an empty-result tripwire
    // stops the pool from taking more chunks.
    let target = ApiTarget {
        client: &client,
        url: &url,
        model: &api.model,
        key: &api_key,
        mode: api.mode,
        control,
    };
    let state = std::sync::Mutex::new((cp, EmptyResultTracker::default()));
    let result = crate::runtime::map_pool(pending.clone(), WORKERS, control, |i| {
        let seg = segs[i];
        let text = transcribe_api(&target, &tmp.path().join(format!("c{i:04}.wav")), seg, wav)
            .with_context(|| crate::tr!("云端语音识别失败：{}", i))?;
        let mut guard = crate::runtime::lock_recover(&state);
        let (cp, tracker) = &mut *guard;
        tracker.record_outcome(cp, seg.start, seg.end, text.as_deref().unwrap_or(""))?;
        // Every collected result, empty included, advances once, as serially.
        pb.inc(1);
        Ok(())
    });
    let (cp, tracker) = state
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner());
    // A clean finish records buffered empty results (real silence), so silent
    // chunks are not transcribed again on resume.
    let err = result.err().or_else(|| tracker.finish(cp).err());
    pb.finish();
    if let Some(e) = err {
        return Err(e);
    }
    // 事件统一来自 checkpoint（收集循环里已 record），按时间排序
    let events: Vec<TranscriptEvent> = cp.sorted_events();
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(events)
}

/// 本机 llama-server：私有本地服务，不计费，不走请求账本。
fn post_local_json(
    agent: &ureq::Agent,
    url: &str,
    body: &serde_json::Value,
    control: &crate::dispatch::TaskControl,
) -> Result<serde_json::Value> {
    use crate::dispatch::Failure;
    let bytes = serde_json::to_vec(body)?;
    crate::runtime::retry(control, MAX_ATTEMPTS, || {
        let request = agent.post(url).header("Content-Type", "application/json");
        match crate::dispatch::receive(request.send(&bytes)) {
            Ok(response) if (200..300).contains(&response.status) => {
                serde_json::from_slice(&response.body).map_err(Failure::local)
            }
            Ok(response) => Err(Failure {
                status: Some(response.status),
                retryable: response.status == 429 || response.status >= 500,
                ..Failure::local(crate::tr!(
                    "本机识别请求失败（HTTP {}） / Local transcription request failed (HTTP {})",
                    response.status,
                    response.status
                ))
            }),
            Err(error) => Err(Failure {
                retryable: true,
                ..Failure::local(error.message)
            }),
        }
    })
}

/// 云端识别：每次发送前先落请求账本（结果不确定时绝不自动重发）。
/// 响应在账本里按模式校验，取值时不必再检查一遍。
fn post_cloud(
    t: &ApiTarget,
    seg: Seg,
    content_type: &str,
    body: &[u8],
    identity: &serde_json::Value,
) -> Result<serde_json::Value> {
    let description = crate::tr!(
        "识别视频 {}–{} 的声音 / Transcribe video audio {}–{}",
        crate::render::fmt_ts(seg.start),
        crate::render::fmt_ts(seg.end),
        crate::render::fmt_ts(seg.start),
        crate::render::fmt_ts(seg.end)
    );
    crate::runtime::retry(t.control, MAX_ATTEMPTS, || {
        let send = || {
            let request = t.client.post(t.url).header("Content-Type", content_type);
            let request = if t.key.is_empty() {
                request
            } else {
                request.header("Authorization", &format!("Bearer {}", t.key))
            };
            crate::dispatch::receive(request.send(body))
        };
        t.control.request(
            &crate::dispatch::RequestSpec {
                service: "asr",
                purpose: "transcription",
                description: &description,
                endpoint: t.url,
                identity,
                resend: crate::dispatch::Resend::AskUser,
            },
            send,
            |value| {
                anyhow::ensure!(
                    value.get("error").is_none_or(serde_json::Value::is_null),
                    crate::tr!("语音服务返回了错误 / The speech service returned an error")
                );
                let has_text = match t.mode {
                    crate::settings::AsrApiMode::Transcriptions => value["text"].is_string(),
                    crate::settings::AsrApiMode::Chat => chat_content_has_text(value),
                };
                anyhow::ensure!(
                    has_text,
                    crate::tr!("语音服务没有返回文字 / The speech service returned no text")
                );
                Ok(())
            },
        )
    })
    .map(|reply| reply.value)
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
    control: &'a crate::dispatch::TaskControl,
}

/// 转写单个 chunk；Ok(None) = 无语音内容。
fn transcribe_api(t: &ApiTarget, chunk: &Path, seg: Seg, wav: &Path) -> Result<Option<String>> {
    use base64::Engine as _;
    cut_wav(wav, seg.cut_start, seg.cut_end, chunk)
        .context(crate::tr!("无法切分音频 / Could not split audio"))?;
    let bytes = std::fs::read(chunk)
        .with_context(|| format!("读取音频片段 / Reading audio segment: {}", chunk.display()))?;
    let identity = serde_json::json!({"model":t.model,"mode":t.mode,"audio_sha256":crate::execution::digest(&bytes),"segment_start":seg.start,"segment_end":seg.end,"cut_start":seg.cut_start,"cut_end":seg.cut_end});
    let v = match t.mode {
        crate::settings::AsrApiMode::Transcriptions => {
            let (content_type, body) = transcription_form(t.model, &bytes);
            post_cloud(t, seg, &content_type, &body, &identity)?
        }
        crate::settings::AsrApiMode::Chat => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let body = serde_json::json!({
                "model": t.model,
                "temperature": 0.0,
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": CHAT_TRANSCRIBE_PROMPT},
                    {"type": "input_audio", "input_audio": {"data": b64, "format": "wav"}}
                ]}]
            });
            post_cloud(
                t,
                seg,
                "application/json",
                &serde_json::to_vec(&body)?,
                &identity,
            )?
        }
    };
    let text = match t.mode {
        crate::settings::AsrApiMode::Transcriptions => {
            v["text"].as_str().unwrap_or_default().trim().to_string()
        }
        crate::settings::AsrApiMode::Chat => parse_chat_content(&v).trim().to_string(),
    };
    let _ = std::fs::remove_file(chunk);
    Ok(if text.is_empty() { None } else { Some(text) })
}

/// Standard OpenAI-compatible file upload, also accepted by OpenRouter.
/// Built here rather than with ureq's multipart form: that API is outside ureq's
/// version promise, and its form is consumed by sending, while a retry sends these
/// same bytes again.
fn transcription_form(model: &str, audio: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!(
        "course2md-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").into_bytes();
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// chat/completions 响应的 message.content 是否携带文本：字符串形式，或
/// 多模态分片数组 [{type:"text", text:...}] 中任一片含 text 字段。
fn chat_content_has_text(v: &serde_json::Value) -> bool {
    let content = &v["choices"][0]["message"]["content"];
    content.is_string()
        || content
            .as_array()
            .is_some_and(|parts| parts.iter().any(|part| part["text"].is_string()))
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

pub const GPU_SETUP_HINT: &str = "请安装支持显卡的 llama.cpp 和驱动，或改用 CPU 识别。 / Install a GPU-capable llama.cpp and driver, or use CPU transcription.";

/// A positive -ngl is only a request: CPU-only llama.cpp builds ignore it.
/// Probe with a deadline so a failing driver cannot hang the task or doctor.
pub fn gpu_devices(bin: &Path) -> Result<Vec<String>> {
    let mut cmd = Command::new(bin);
    cmd.arg("--list-devices");
    let out = crate::runtime::run_bounded("llama-server", &mut cmd, GPU_PROBE_TIMEOUT)
        .with_context(|| crate::tr!("无法检测显卡：{}", GPU_SETUP_HINT))?;
    anyhow::ensure!(
        out.status.success(),
        "无法检测显卡，请更新 llama.cpp 和显卡驱动 / Could not detect the GPU; update llama.cpp and the GPU driver. {GPU_SETUP_HINT}"
    );
    parse_gpu_devices(&out.stdout)
}

/// `--list-devices` 也会报告 BLAS/Accelerate 这类 CPU 后端行；只有这些前缀才是 GPU。
/// 桌面端（desktop/src/backend.rs）共用同一判定，避免两处白名单漂移。
pub fn is_gpu_device_id(id: &str) -> bool {
    ["MTL", "CUDA", "Vulkan", "SYCL", "ROCm"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

fn parse_gpu_devices(output: &str) -> Result<Vec<String>> {
    let (_, rows) = output.split_once("Available devices:").context(crate::tr!(
        "无法读取显卡列表，请更新 llama.cpp / Could not read the GPU list; update llama.cpp"
    ))?;
    Ok(rows
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.split_once(':').is_some_and(|(name, description)| {
                is_gpu_device_id(name) && !description.trim().is_empty()
            })
        })
        .map(str::to_owned)
        .collect())
}

fn find_llama_server() -> Result<PathBuf> {
    crate::tools::locate("llama-server")
        .map(|tool| tool.path)
        .context(crate::tr!(
            "未找到 llama-server。请安装 llama.cpp 并加入 PATH"
        ))
}

/// llama-server 对 offload 相关新 flag 的支持情况。
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
    // Keyed by binary: a different llama-server (a changed setting or upgrade)
    // must be probed again rather than reuse another build's flags.
    static CAPS: std::sync::Mutex<Vec<(PathBuf, LlamaServerCaps)>> =
        std::sync::Mutex::new(Vec::new());
    if let Some((_, caps)) = crate::runtime::lock_recover(&CAPS)
        .iter()
        .find(|(path, _)| path == bin)
    {
        return *caps;
    }
    let caps = match probe_help_text(bin) {
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
    };
    crate::runtime::lock_recover(&CAPS).push((bin.to_path_buf(), caps));
    caps
}

/// 超时或非零退出返回 None（按旧版处理）。
fn probe_help_text(bin: &Path) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--help");
    let out = crate::runtime::run_bounded("llama-server", &mut cmd, HELP_PROBE_TIMEOUT).ok()?;
    out.status
        .success()
        .then(|| format!("{}\n{}", out.stdout, out.stderr))
}

/// llama-server GPU 卸载控制（由 PipelineConfig 解析而来）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct OffloadOpts {
    pub provider: crate::config::AsrProvider,
    pub gpu_layers: u32,
    pub mmproj_offload: bool,
}

/// 组装 llama-server 启动参数（纯函数，便于单测）：
/// - gpu 后端：`-ngl <gpu_layers>`；mmproj_offload=false 且 llama.cpp 支持时
///   追加 `--no-mmproj-offload`
/// - cpu 后端：`-ngl 0`，并按探测结果追加 `--device none --no-op-offload
///   --no-mmproj-offload`——新版 llama.cpp 下仅 -ngl 0 仍可能把 mmproj/部分
///   算子卸载到 GPU
/// - 不支持的 flag（caps=false）一律不加，兼容发行版仓库的旧 llama-server
fn build_server_args(server: &LlamaServer, caps: LlamaServerCaps, port: u16) -> Vec<String> {
    let LlamaServer {
        model,
        mmproj,
        offload,
        threads,
    } = server;
    let (offload, threads) = (*offload, *threads);
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
        LLAMA_MAX_TOKENS.to_string(),
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
        // 会插在进度条重绘中间，破坏 indicatif 的原地更新。
        // 改为 piped + 后台 drain，尾部缓存用于失败诊断，debug 日志可转发。
        .stderr(Stdio::piped());
    crate::runtime::ManagedChild::spawn("llama-server", &mut cmd)
}

fn transcribe_file(
    client: &ureq::Agent,
    base: &str,
    wav: &Path,
    control: &crate::dispatch::TaskControl,
) -> Result<String> {
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
    let v = post_local_json(
        client,
        &format!("{base}/v1/chat/completions"),
        &body,
        control,
    )
    .context(crate::tr!(
        "本地语音识别请求失败 / Local transcription request failed"
    ))?;
    let choice = &v["choices"][0];
    anyhow::ensure!(
        !choice.is_null(),
        crate::tr!(
            "本机识别服务返回的内容无法读取 / The local speech service response could not be read"
        )
    );
    // 空文本按无语音处理（与云端 transcribe_api 的 Ok(None) 同语义）：
    // VAD 切出的近静音段在 llama-server 上常返回空，不该把整次 ASR 判死
    Ok(choice["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

pub(crate) fn ffmpeg_vad(wav: &Path, max_speech: f32) -> Result<Vec<Seg>> {
    // run_bounded：stdin 关闭 + 超时强制 kill（ffmpeg 在 GUI/管道 stdin 上可能挂死）
    let mut cmd = Command::new(crate::tools::program("ffmpeg"));
    cmd.args(["-hide_banner", "-nostdin", "-i"]).arg(wav).args([
        "-af",
        SILENCEDETECT_AF,
        "-f",
        "null",
        "-",
    ]);
    let out = crate::runtime::run_bounded("ffmpeg", &mut cmd, VAD_TIMEOUT)?;
    if !out.status.success() {
        anyhow::bail!(
            "无法分析音频中的停顿：{0} / Could not analyze pauses in the audio: {0}",
            crate::error::tail_lines(&out.stderr, 3)
        );
    }
    let log = &out.stderr;
    // 时长缺失时不能按 0 处理：末段语音（或整段无停顿的音频）会被当成静音丢掉
    let dur = ffmpeg_input_duration(log)
        .or_else(|| crate::media::probe_duration_blocking(wav))
        .filter(|d| d.is_finite() && *d > 0.0)
        .context(crate::tr!(
            "无法读取音频时长 / Could not read the audio duration"
        ))?;
    let mut silences: Vec<(f64, f64)> = vec![];
    let mut start: Option<f64> = None;
    for line in log.lines() {
        if let Some(v) = line.split("silence_start:").nth(1) {
            match v.trim().parse::<f64>() {
                Ok(t) => start = Some(t),
                // 解析失败不能静默丢弃：0.0 会产生倒挂区间进 invert_silence
                Err(e) => {
                    tracing::warn!("无法读取静音起点 / Could not read silence start ({v:?}): {e}")
                }
            }
        } else if let Some(v) = line.split("silence_end:").nth(1) {
            match v.split_whitespace().next().unwrap_or("").parse::<f64>() {
                Ok(end) => {
                    if let Some(s) = start.take() {
                        silences.push((s, end));
                    }
                }
                Err(e) => {
                    tracing::warn!("无法读取静音终点 / Could not read silence end ({v:?}): {e}")
                }
            }
        }
    }
    normalize_segments(invert_silence(dur, &silences), max_speech as f64, wav, dur)
}

/// ffmpeg 输入信息里的 `Duration: HH:MM:SS.xx`（秒）。
fn ffmpeg_input_duration(log: &str) -> Option<f64> {
    let rest = log.split("Duration:").nth(1)?.trim_start();
    let stamp = rest.split([',', ' ', '\n']).next()?;
    let mut parts = stamp.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?)
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
    /// 流式读取 16k 单声道 s16 wav（extract_audio 的固定产物）：
    /// 分块读入并滑动累计每个 100ms 窗口的平方和，只保留 rms 数组——
    /// 不把整文件和全部样本读进内存（2h 音频约 1GB）。
    pub fn load(wav: &Path) -> Result<Self> {
        use std::io::{BufReader, Read, Seek};
        let file = std::fs::File::open(wav)
            .with_context(|| format!("无法读取音频 / Could not read audio: {}", wav.display()))?;
        let mut reader = BufReader::new(file);
        // RIFF 头 + 逐 chunk 头扫描，定位 PCM data 块（流式等价于原 find_pcm_body）
        let mut header = [0u8; 12];
        reader
            .read_exact(&mut header)
            .ok()
            .filter(|_| &header[0..4] == b"RIFF" && &header[8..12] == b"WAVE")
            .ok_or_else(|| {
                anyhow::anyhow!(crate::tr!(
                    "无法解析 wav PCM 数据 / Cannot parse wav PCM data"
                ))
            })?;
        let data_size = loop {
            let mut ch = [0u8; 8];
            if reader.read_exact(&mut ch).is_err() {
                anyhow::bail!(crate::tr!(
                    "无法解析 wav PCM 数据 / Cannot parse wav PCM data"
                ));
            }
            let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
            if &ch[0..4] == b"data" {
                break size;
            }
            // 跳过其他 chunk（含奇数对齐字节）
            reader.seek(std::io::SeekFrom::Current((size + (size & 1)) as i64))?;
        };
        let mut rms = Vec::new();
        let mut buf = vec![0u8; 128 * 1024];
        let mut half: Option<u8> = None; // 跨 read 边界的样本低字节
        let mut sum = 0.0f64;
        let mut n_in_hop = 0usize;
        let mut remaining = data_size;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = reader.read(&mut buf[..want])?;
            if n == 0 {
                break; // data 块声明长度超出文件实际（截断文件）：按实际读到的计
            }
            remaining -= n as u64;
            let mut i = 0;
            if let Some(lo) = half.take() {
                rms_accumulate(
                    i16::from_le_bytes([lo, buf[0]]),
                    &mut sum,
                    &mut n_in_hop,
                    &mut rms,
                );
                i = 1;
            }
            while i + 1 < n {
                rms_accumulate(
                    i16::from_le_bytes([buf[i], buf[i + 1]]),
                    &mut sum,
                    &mut n_in_hop,
                    &mut rms,
                );
                i += 2;
            }
            if i < n {
                half = Some(buf[i]);
            }
        }
        if n_in_hop > 0 {
            rms.push((sum / n_in_hop as f64).sqrt() as f32);
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

/// 累计一个样本进当前 100ms 窗口；窗口满则写入 rms 并归零重开。
fn rms_accumulate(sample: i16, sum: &mut f64, n_in_hop: &mut usize, rms: &mut Vec<f32>) {
    const HOP: usize = 1600; // 100ms @16k
    *sum += (sample as f64 / 32768.0).powi(2);
    *n_in_hop += 1;
    if *n_in_hop == HOP {
        rms.push((*sum / HOP as f64).sqrt() as f32);
        *sum = 0.0;
        *n_in_hop = 0;
    }
}

const PAD: f64 = 0.25; // 切音频时向两侧静音各延展的秒数
const SPLIT_WINDOW: f64 = 3.0; // 在目标切点 ± 此窗口内寻找静音最低点
const MIN_PIECE: f64 = 1.0; // 硬切产生的最短片段
/// 切点距段起点的最小秒数：防止能量异常时切点贴到起点，产生超短 chunk 无限递归
const MIN_HARD_CUT: f64 = 0.5;

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
    // VAD 成功但无语音（或全被短段过滤）→ 空分段。
    // 不把整段音频当语音兜底：静音课件会诱发 ASR 幻觉。
    let mut raw = speech;
    raw.retain(|(a, b)| b - a >= 0.2);
    if raw.is_empty() {
        return Ok(vec![]);
    }
    // 能量曲线只在有超长段需要切分时才读（要流式扫描整个音频）
    let energy = if raw.iter().any(|(a, b)| b - a > max_speech) {
        Energy::load(wav)
            .inspect_err(|e| {
                tracing::warn!(
                    "无法读取音频能量，长段将按固定长度切分 / Could not read audio energy; long segments are split at fixed lengths: {e:#}"
                )
            })
            .ok()
    } else {
        None
    };

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
                (e + PAD).min(speech_hi)
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
    let cut = cut.clamp(s + MIN_HARD_CUT, target);
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
    let mut cmd = Command::new(crate::tools::program("ffmpeg"));
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-ss",
    ])
    .arg(format!("{start:.3}"))
    .arg("-t")
    .arg(format!("{dur:.3}"))
    .arg("-i")
    .arg(src)
    .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
    .arg(dest);
    let out = crate::runtime::run_bounded("ffmpeg", &mut cmd, CUT_TIMEOUT)?;
    if !out.status.success() {
        anyhow::bail!(crate::tr!("无法切分音频 / ffmpeg could not split audio"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn gpu_probe_distinguishes_devices_from_cpu_only_and_unknown_output() {
        assert!(
            super::parse_gpu_devices("Available devices:\n  (none)\n")
                .unwrap()
                .is_empty()
        );
        let devices = super::parse_gpu_devices("Available devices:\n  Vulkan0: Intel Arc B390 (23719 MiB)\n  CUDA0: NVIDIA GPU (8192 MiB)\n").unwrap();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].contains("Intel Arc B390"));
        // BLAS/Accelerate 是 CPU 后端行，不得当作 GPU 报告（macOS 上 `--list-devices` 会列出它）。
        let devices = super::parse_gpu_devices("Available devices:\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n  MTL0: Apple M3 Max (110100 MiB, 110100 MiB free)\n").unwrap();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].contains("Apple M3 Max"));
        assert!(
            super::parse_gpu_devices(
                "Available devices:\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n"
            )
            .unwrap()
            .is_empty()
        );
        assert!(super::parse_gpu_devices("unknown option --list-devices").is_err());
    }

    use super::*;

    #[test]
    fn empty_tripwire_fires_only_without_any_success() {
        let mut t = EmptyTripwire::default();
        for _ in 0..EMPTY_RESULT_TRIPWIRE - 1 {
            assert!(!t.record(true));
        }
        assert!(t.record(true), "连续空结果达到阈值必须熔断");
        assert!(t.tripped_error().to_string().contains("空文本"));

        // 一旦出现非空结果（端点其实正常），静音属实，不再熔断
        let mut t = EmptyTripwire::default();
        assert!(!t.record(true));
        assert!(!t.record(false));
        for _ in 0..EMPTY_RESULT_TRIPWIRE * 2 {
            assert!(!t.record(true));
        }
    }

    #[test]
    fn transcription_upload_preserves_audio_bytes() {
        let audio = b"RIFF\0\xff\r\nWAVE";
        let (content_type, body) = transcription_form("whisper-1", audio);
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        assert!(body.starts_with(format!("--{boundary}\r\n").as_bytes()));
        assert!(body.windows(audio.len()).any(|slice| slice == audio));
        assert!(body.ends_with(format!("\r\n--{boundary}--\r\n").as_bytes()));
        assert!(String::from_utf8_lossy(&body).contains("name=\"file\"; filename=\"audio.wav\""));
    }

    #[test]
    fn vad_reads_duration_from_ffmpeg_and_keeps_speech_without_pauses() {
        let log = "Input #0, wav, from 'a.wav':\n  Duration: 00:01:02.50, bitrate: 256 kb/s\n";
        assert_eq!(ffmpeg_input_duration(log), Some(62.5));
        assert_eq!(ffmpeg_input_duration("  Duration: N/A, bitrate: N/A"), None);
        // 整段没有停顿：只要时长已知，就是一整段语音而不是空结果
        assert_eq!(invert_silence(62.5, &[]), vec![(0.0, 62.5)]);
    }

    #[test]
    fn sanitize_and_vad_invert() {
        assert_eq!(
            sanitize_qwen_text("**language Chinese<asr_text>你好世界。"),
            "你好世界。"
        );
        assert_eq!(sanitize_qwen_text("内容</asr_text>尾巴"), "内容");
        // 语言名后跟多字节空白（全角空格）时不得在字符中间切片
        assert_eq!(sanitize_qwen_text("language Chinese\u{3000}你好"), "你好");
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

    // ---------- GPU 卸载控制 ----------

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
        build_server_args(
            &LlamaServer {
                model: "/m.gguf".into(),
                mmproj: "/mm.gguf".into(),
                offload,
                threads: 4,
            },
            caps,
            8081,
        )
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

    #[cfg(unix)]
    #[test]
    fn help_probe_handles_output_larger_than_a_pipe_buffer() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("llama-server");
        std::fs::write(&script, "#!/bin/sh\nhead -c 262144 /dev/zero | tr '\\000' x\nprintf '\\n--device --no-op-offload --no-mmproj-offload\\n'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let help = probe_help_text(&script).expect("long help must not time out");
        assert!(help.len() > 262144);
        let caps = caps_from_help(&help);
        assert!(caps.device && caps.no_op_offload && caps.no_mmproj_offload);
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

#[cfg(test)]
mod cloud_tests {
    use super::*;

    /// Cloud transcription runs its chunks on the shared worker pool and
    /// records every result into the checkpoint, so a resumed task sends
    /// nothing for chunks already transcribed.
    #[test]
    fn cloud_chunks_are_transcribed_on_the_pool_and_checkpointed() {
        if crate::runtime::which("ffmpeg").is_none() {
            assert!(std::env::var_os("CI").is_none(), "ffmpeg is required in CI");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("speech.wav");
        // Three tones separated by silence: voice activity finds three segments.
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("aevalsrc=sin(440*2*PI*t)*between(mod(t\\,3)\\,0\\,1.5):d=8.5:s=16000")
            .args(["-ac", "1", "-ar", "16000"])
            .arg(&wav)
            .status()
            .unwrap();
        assert!(status.success());
        let segments = ffmpeg_vad(&wav, 30.).unwrap().len();
        assert!(segments >= 2, "{segments} segments");
        let body = r#"{"text":"spoken"}"#;
        let (base, _server) = crate::test_http::serve(
            (0..segments)
                .map(|_| crate::test_http::json_response(body))
                .collect(),
        );
        let api = crate::settings::AsrApi {
            base_url: base,
            api_key: String::new(),
            model: "test-asr".into(),
            mode: crate::settings::AsrApiMode::Transcriptions,
        };
        let identity = crate::checkpoint::AsrIdentity::new("api", "test-asr", 30.);
        let mut cp = crate::checkpoint::Checkpoint::open(dir.path(), true, &identity).unwrap();
        let events = run_api(&api, &wav, 30., &mut cp, &Default::default()).unwrap();
        assert_eq!(events.len(), segments);
        assert!(events.iter().all(|event| event.text == "spoken"));
        drop(cp);
        // Resuming finds every chunk done and sends nothing (the fake server
        // has no responses left).
        let mut cp = crate::checkpoint::Checkpoint::open(dir.path(), true, &identity).unwrap();
        let again = run_api(&api, &wav, 30., &mut cp, &Default::default()).unwrap();
        assert_eq!(again.len(), segments);
    }
}
