//! 语音识别：一个任务一条流程，后端只负责把音频片段变成文字。
//!
//! [`run`] 负责 checkpoint、语音分段（ffmpeg 静音检测，Apple 用 Silero）、切片、并发、
//! 空结果熔断与进度；后端实现 [`Transcribe`] 并在 [`Backend`] 加一个分支即可。

use crate::checkpoint::{AsrIdentity, Checkpoint};
use crate::config::{AsrProvider, PipelineConfig};
use crate::dispatch::TaskControl;
use crate::progress::Stage;
use crate::settings::{AsrApi, AsrApiMode};
use crate::timeline::TranscriptEvent;
use crate::tr;
use anyhow::{Context, Result};
use serde_json::{Value, json};

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
/// 继续只会空烧请求。单片为空仍是合法静音。
const EMPTY_RESULT_TRIPWIRE: usize = 10;

/// A loaded speech engine. Dropping it stops whatever it started.
pub(crate) trait Transcribe: Sync {
    /// Writes one chunk of the task's audio for [`Self::transcribe`].
    fn cut(&self, wav: &Path, seg: Seg, chunk: &Path) -> Result<()> {
        cut_wav(wav, seg.cut_start, seg.cut_end, chunk)
    }
    /// The text of one 16 kHz mono WAV chunk; empty when it holds no speech.
    fn transcribe(&self, chunk: &Path, seg: Seg) -> Result<String>;
    /// How many chunks may run at once.
    fn workers(&self) -> usize {
        1
    }
    /// Adds what the engine knows about a failed run to its error.
    fn explain(&self, error: anyhow::Error) -> anyhow::Error {
        error
    }
}

/// The selected backend, resolved before its checkpoint opens.
enum Backend {
    Llama(LlamaServer),
    Npu(String),
    #[cfg(apple_native)]
    Coreml(String),
    Api(AsrApi),
}

impl Backend {
    /// Fails before any work when the selection cannot run at all.
    async fn resolve(cfg: &PipelineConfig, control: &TaskControl) -> Result<Self> {
        Ok(match cfg.provider {
            AsrProvider::Api => Self::Api(cfg.asr_api.clone()),
            AsrProvider::Npu => Self::Npu(crate::npu::resolve_npu_model(cfg.asr_model.as_deref())),
            #[cfg(apple_native)]
            AsrProvider::Coreml => Self::Coreml(crate::apple::resolve_model(
                cfg.asr_model.as_deref().filter(|s| !s.trim().is_empty()),
            )?),
            #[cfg(not(apple_native))]
            AsrProvider::Coreml => anyhow::bail!(tr!(
                "此版本不支持 Apple 识别，请改用 GPU 或 CPU 识别 / This build lacks Apple transcription; use GPU or CPU transcription."
            )),
            AsrProvider::Gpu | AsrProvider::Cpu => {
                let llama =
                    crate::models::ensure_llama_or_download(&cfg.model_dir, control).await?;
                Self::Llama(LlamaServer {
                    model: llama.model,
                    mmproj: llama.mmproj,
                    provider: cfg.provider,
                    gpu_layers: cfg.gpu_layers,
                    mmproj_offload: cfg.mmproj_offload,
                    threads: cfg.threads,
                })
            }
        })
    }

    /// What decides whether saved progress can be reused.
    fn identity(&self, max_speech: f32) -> AsrIdentity {
        match self {
            // CPU/GPU paths share the same concrete GGUF model identity.
            Self::Llama(_) => {
                AsrIdentity::new("llama", &crate::models::llama_gguf_identity(), max_speech)
            }
            Self::Npu(model) => AsrIdentity::new("npu", model, max_speech),
            #[cfg(apple_native)]
            Self::Coreml(model) => AsrIdentity::new("coreml", model, max_speech),
            // 同一模型在 transcriptions / chat 两种端点下的输出可能不同，身份须含模式
            Self::Api(api) => {
                let base = api.base_url.trim().trim_end_matches('/');
                let model = format!("{base}:{}:{}", api.mode, api.model);
                AsrIdentity::new("api", &model, max_speech)
            }
        }
    }

    /// The speech in the audio, cut to `max_speech`.
    fn segments(&self, wav: &Path, max_speech: f64) -> Result<Vec<Seg>> {
        match self {
            #[cfg(apple_native)]
            Self::Coreml(_) => crate::apple::segments(wav, max_speech),
            _ => ffmpeg_vad(wav, max_speech),
        }
    }

    fn load(self, control: &TaskControl) -> Result<Box<dyn Transcribe>> {
        Ok(match self {
            Self::Llama(server) => Box::new(server.start(control)?),
            Self::Npu(model) => Box::new(crate::npu::NpuWorker::start(&model, control)?),
            #[cfg(apple_native)]
            Self::Coreml(model) => Box::new(crate::apple::CoremlAsr::load_for_task(&model)?),
            Self::Api(api) => Box::new(CloudSpeech::new(&api, control)?),
        })
    }
}

/// Transcribes `wav` with the selected backend, resuming the task's saved progress:
/// a work directory belongs to one task identity (--no-resume picks a new one).
pub async fn run(
    cfg: &PipelineConfig,
    wav: &Path,
    job: &crate::job::Job,
) -> Result<Vec<TranscriptEvent>> {
    let backend = Backend::resolve(cfg, &job.control).await?;
    let mut cp = Checkpoint::open(&job.dir, true, &backend.identity(cfg.max_speech))?;
    let (wav, control) = (wav.to_path_buf(), job.control.clone());
    let max_speech = cfg.max_speech as f64;
    let events = tokio::task::spawn_blocking(move || {
        let events = transcribe_all(backend, &wav, max_speech, &mut cp, &control)?;
        cp.finish()?;
        Ok(events)
    })
    .await
    .context(tr!(
        "语音识别任务未能完成 / Speech recognition task did not complete"
    ))?;
    if cfg.provider == AsrProvider::Coreml {
        return events.context(tr!(
            "已选的 Apple 识别方式未完成；任务参数和进度已保留 / Selected Apple transcription failed; task settings retained"
        ));
    }
    events
}

/// Every backend's one flow. Results are recorded as they arrive, so a resumed task
/// skips finished chunks; the first error stops the pool from taking more.
fn transcribe_all(
    backend: Backend,
    wav: &Path,
    max_speech: f64,
    cp: &mut Checkpoint,
    control: &TaskControl,
) -> Result<Vec<TranscriptEvent>> {
    let started = Instant::now();
    let segs = backend.segments(wav, max_speech)?;
    tracing::info!(segs = segs.len(), "vad");
    if segs.is_empty() {
        tracing::warn!(
            "未检测到语音，将仅保留截图 / No speech detected; keeping slides without a transcript"
        );
        return Ok(Vec::new());
    }
    let engine = backend.load(control)?;
    tracing::info!(
        secs = format_args!("{:.1}", started.elapsed().as_secs_f64()),
        "asr ready"
    );
    let tmp = crate::runtime::temp_work_dir("asr")?;
    let pending: Vec<(usize, Seg)> = segs
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, seg)| !cp.is_done(seg.start, seg.end))
        .collect();
    let bar = crate::progress::Bar::new(Stage::Transcribe, segs.len() as u64).with_template(
        "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {eta} {msg}",
    );
    bar.set_position((segs.len() - pending.len()) as u64);
    if engine.workers() > 1 {
        crate::progress::emit(&crate::progress::Event::Workers {
            stage: Stage::Transcribe,
            workers: engine.workers().min(pending.len()),
        });
    }
    let state = std::sync::Mutex::new((&mut *cp, EmptyResults::default()));
    let result = crate::pool::map_pool(pending, engine.workers(), control, |(index, seg)| {
        let chunk = tmp.path().join(format!("c{index:04}.wav"));
        let text = engine
            .cut(wav, seg, &chunk)
            .and_then(|()| engine.transcribe(&chunk, seg));
        let _ = std::fs::remove_file(&chunk);
        let text = text?;
        let mut guard = crate::runtime::lock_recover(&state);
        let (cp, empties) = &mut *guard;
        empties.record(cp, seg.start, seg.end, &text)?;
        // Every collected result, empty included, advances once.
        bar.inc(1);
        Ok(())
    });
    let (cp, mut empties) = state
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner());
    // A clean finish records buffered empty results (real silence), so silent
    // chunks are not transcribed again on resume.
    let result = result.and_then(|_| empties.flush(cp));
    bar.finish();
    result.map_err(|error| engine.explain(error))?;
    let events = cp.sorted_events();
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", started.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(events)
}

/// Holds empty results back from the checkpoint until a result with text shows the
/// endpoint works. Recording them first would mark chunks as finished silence that a
/// run after fixing the endpoint could never recover. Arrival order does not matter:
/// each buffered record is the same empty text.
#[derive(Default)]
struct EmptyResults {
    pending: Vec<(f64, f64)>,
    seen_text: bool,
}

impl EmptyResults {
    fn record(&mut self, cp: &mut Checkpoint, start: f64, end: f64, text: &str) -> Result<()> {
        if !text.trim().is_empty() {
            self.seen_text = true;
            self.flush(cp)?;
            return cp.record(start, end, text);
        }
        self.pending.push((start, end));
        anyhow::ensure!(
            self.seen_text || self.pending.len() < EMPTY_RESULT_TRIPWIRE,
            "语音服务连续 {0} 个分片返回空文本；请检查语音服务地址与模型是否支持音频输入 / The speech service returned empty text for {0} consecutive segments; check that the endpoint and model support audio input",
            self.pending.len()
        );
        Ok(())
    }

    /// A failed write stops here; what is left stays pending and runs again.
    fn flush(&mut self, cp: &mut Checkpoint) -> Result<()> {
        for (start, end) in std::mem::take(&mut self.pending) {
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

/// The local llama-server a transcription starts: model files and placement.
struct LlamaServer {
    model: PathBuf,
    mmproj: PathBuf,
    provider: AsrProvider,
    gpu_layers: u32,
    mmproj_offload: bool,
    threads: i32,
}

impl LlamaServer {
    fn cpu_only(&self) -> bool {
        self.provider == AsrProvider::Cpu
    }

    fn start(self, control: &TaskControl) -> Result<LlamaEngine> {
        let bin = crate::tools::locate("llama-server")
            .map(|tool| tool.path)
            .context(tr!("未找到 llama-server。请安装 llama.cpp 并加入 PATH"))?;
        if !self.cpu_only() {
            let devices = gpu_devices(&bin)?;
            anyhow::ensure!(
                !devices.is_empty(),
                "选择了 GPU 识别，但没有找到可用的显卡 / GPU transcription is selected, but no usable GPU was found. {}",
                GPU_SETUP_HINT
            );
            tracing::info!(devices = %devices.join(", "), "GPU backend detected");
        }
        // Probed each start: a failed probe (an old build, a timeout) means only -ngl.
        let caps =
            probe_help_text(&bin).map_or_else(Default::default, |help| caps_from_help(&help));
        let too_old = tr!(
            "当前 llama.cpp 版本过旧，无法只用 CPU 识别，请升级后重试 / This llama.cpp is too old to transcribe on CPU only; upgrade and retry"
        );
        if self.cpu_only() && !(caps.device && caps.no_op_offload && caps.no_mmproj_offload) {
            let devices = gpu_devices(&bin).context(too_old)?;
            anyhow::ensure!(devices.is_empty(), too_old);
        } else if !self.cpu_only() && !self.mmproj_offload {
            anyhow::ensure!(
                caps.no_mmproj_offload,
                tr!(
                    "当前 llama.cpp 版本过旧，不支持此显卡设置，请升级后重试 / This llama.cpp is too old for this GPU setting; upgrade and retry"
                )
            );
        }
        let port = crate::runtime::free_port()?;
        let args = build_server_args(&self, caps, port);
        tracing::info!(bin = %bin.display(), args = %args.join(" "), "llama-server");
        crate::progress::start(Stage::ModelLoad);
        let mut child = crate::runtime::ManagedChild::spawn(
            "llama-server",
            Command::new(&bin)
                .args(&args)
                .stdout(Stdio::null())
                // stderr 不能 inherit：llama-server 每个 chunk 都打 slot timing 日志，
                // 会破坏进度条的原地更新。piped + 后台 drain，尾部用于失败诊断。
                .stderr(Stdio::piped()),
        )?;
        let stderr = child
            .take_stderr()
            .map(|s| crate::runtime::drain_stderr(s, "llama_server"))
            .unwrap_or_default();
        let base = format!("http://127.0.0.1:{port}");
        // 子进程秒退（端口冲突/模型损坏）会立即报错，而不是等满 300s
        if let Err(e) =
            crate::runtime::wait_ready(&base, SERVER_READY_TIMEOUT, &mut child, &|| control.check())
        {
            if e.downcast_ref::<crate::dispatch::Failure>().is_some() {
                return Err(e); // 暂停/取消不是启动故障
            }
            return Err(stderr.attach(
                e,
                tr!("无法启动识别服务 / Could not start the speech recognition service"),
            ));
        }
        crate::progress::done(Stage::ModelLoad);
        Ok(LlamaEngine {
            _child: child,
            stderr,
            args,
            client: crate::runtime::http_agent_keeping_errors(LLAMA_HTTP_TIMEOUT),
            url: format!("{base}/v1/chat/completions"),
            control: control.clone(),
        })
    }
}

/// A running llama-server. The child's kill-on-drop keeps an orphan from holding the GPU.
struct LlamaEngine {
    _child: crate::runtime::ManagedChild,
    stderr: crate::runtime::StderrTail,
    args: Vec<String>,
    client: ureq::Agent,
    url: String,
    control: TaskControl,
}

impl Transcribe for LlamaEngine {
    fn transcribe(&self, chunk: &Path, _seg: Seg) -> Result<String> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(std::fs::read(chunk)?);
        let body = json!({
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
        let reply = post_local_json(&self.client, &self.url, &body, &self.control).context(tr!(
            "本地语音识别请求失败 / Local transcription request failed"
        ))?;
        let choice = &reply["choices"][0];
        anyhow::ensure!(
            !choice.is_null(),
            tr!(
                "本机识别服务返回的内容无法读取 / The local speech service response could not be read"
            )
        );
        // VAD 切出的近静音段在 llama-server 上常返回空，不该把整次 ASR 判死
        Ok(sanitize_qwen_text(
            choice["message"]["content"].as_str().unwrap_or(""),
        ))
    }

    /// 转写中途失败大概率是 server 侧问题，附上 stderr 与启动参数便于定位。
    fn explain(&self, error: anyhow::Error) -> anyhow::Error {
        self.stderr
            .attach(error, tr!("识别服务错误详情 / llama-server error details"))
            .context(format!(
                "llama-server 启动参数 / arguments: {}",
                self.args.join(" ")
            ))
    }
}

/// 本机 llama-server：私有本地服务，不计费，不走请求账本。
fn post_local_json(
    agent: &ureq::Agent,
    url: &str,
    body: &Value,
    control: &TaskControl,
) -> Result<Value> {
    use crate::dispatch::Failure;
    let bytes = serde_json::to_vec(body)?;
    crate::pool::retry(control, MAX_ATTEMPTS, || {
        let request = agent.post(url).header("Content-Type", "application/json");
        match crate::dispatch::receive(request.send(&bytes)) {
            Ok(response) if (200..300).contains(&response.status) => {
                serde_json::from_slice(&response.body).map_err(Failure::local)
            }
            Ok(response) => Err(Failure {
                status: Some(response.status),
                retryable: response.status == 429 || response.status >= 500,
                ..Failure::local(tr!(
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

/// chat 模式下让多模态 LLM 转录的指令（保持与本地 ASR 输出风格一致：忠实、带自然标点）。
const CHAT_TRANSCRIBE_PROMPT: &str = "请将这段音频转录为文字。只输出转录内容本身（保留自然标点），\
不要添加任何解释、概括或格式标记。若没有语音内容，输出空字符串。";

/// 云端 STT：逐段 POST（OpenAI 兼容 / OpenRouter），每次发送前先落请求账本。
struct CloudSpeech {
    client: ureq::Agent,
    url: String,
    model: String,
    /// Resolved at preflight (environment included); empty is a chosen no-auth service.
    key: String,
    mode: AsrApiMode,
    control: TaskControl,
}

impl CloudSpeech {
    fn new(api: &AsrApi, control: &TaskControl) -> Result<Self> {
        Ok(Self {
            client: crate::runtime::http_agent_keeping_errors(API_HTTP_TIMEOUT),
            url: crate::config::asr_endpoint(api)?,
            model: api.model.clone(),
            key: api.api_key.trim().to_string(),
            mode: api.mode,
            control: control.clone(),
        })
    }

    /// 结果不确定时绝不自动重发；响应在账本里按模式校验，取值时不必再检查一遍。
    fn post(&self, seg: Seg, content_type: &str, body: &[u8], identity: &Value) -> Result<Value> {
        let (start, end) = (
            crate::timeline::fmt_ts(seg.start),
            crate::timeline::fmt_ts(seg.end),
        );
        let description = tr!(
            "识别视频 {}–{} 的声音 / Transcribe video audio {}–{}",
            start,
            end,
            start,
            end
        );
        crate::pool::retry(&self.control, MAX_ATTEMPTS, || {
            let send = || {
                let request = self
                    .client
                    .post(&self.url)
                    .header("Content-Type", content_type);
                let request = if self.key.is_empty() {
                    request
                } else {
                    request.header("Authorization", &format!("Bearer {}", self.key))
                };
                crate::dispatch::receive(request.send(body))
            };
            self.control.request(
                &crate::dispatch::RequestSpec {
                    service: "asr",
                    purpose: "transcription",
                    description: &description,
                    endpoint: &self.url,
                    identity,
                    resend: crate::dispatch::Resend::AskUser,
                },
                send,
                |value| {
                    anyhow::ensure!(
                        value.get("error").is_none_or(Value::is_null),
                        tr!("语音服务返回了错误 / The speech service returned an error")
                    );
                    anyhow::ensure!(
                        reply_text(self.mode, value).is_some(),
                        tr!("语音服务没有返回文字 / The speech service returned no text")
                    );
                    Ok(())
                },
            )
        })
        .map(|reply| reply.value)
    }
}

impl Transcribe for CloudSpeech {
    /// A paid request's ledger identity hashes these bytes, so they must not change: a
    /// request sent but unconfirmed by an earlier version is found again by this identity
    /// and is never resent without authorization. Hence ffmpeg, with exactly these
    /// arguments, and not the in-process cut.
    fn cut(&self, wav: &Path, seg: Seg, chunk: &Path) -> Result<()> {
        let mut cmd = Command::new(crate::tools::program("ffmpeg"));
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-ss",
        ])
        .arg(format!("{:.3}", seg.cut_start))
        .arg("-t")
        .arg(format!("{:.3}", (seg.cut_end - seg.cut_start).max(0.05)))
        .arg("-i")
        .arg(wav)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(chunk);
        let out = crate::runtime::run_bounded("ffmpeg", &mut cmd, CUT_TIMEOUT)?;
        anyhow::ensure!(
            out.status.success(),
            crate::tr!("无法切分音频 / Could not split audio")
        );
        Ok(())
    }

    fn transcribe(&self, chunk: &Path, seg: Seg) -> Result<String> {
        use base64::Engine as _;
        let bytes = std::fs::read(chunk).with_context(|| {
            format!("读取音频片段 / Reading audio segment: {}", chunk.display())
        })?;
        let identity = json!({"model":self.model,"mode":self.mode,"audio_sha256":crate::digest::digest(&bytes),"segment_start":seg.start,"segment_end":seg.end,"cut_start":seg.cut_start,"cut_end":seg.cut_end});
        let reply = match self.mode {
            AsrApiMode::Transcriptions => {
                let (content_type, body) = transcription_form(&self.model, &bytes);
                self.post(seg, &content_type, &body, &identity)
            }
            AsrApiMode::Chat => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let body = json!({
                    "model": self.model,
                    "temperature": 0.0,
                    "messages": [{"role": "user", "content": [
                        {"type": "text", "text": CHAT_TRANSCRIBE_PROMPT},
                        {"type": "input_audio", "input_audio": {"data": b64, "format": "wav"}}
                    ]}]
                });
                self.post(
                    seg,
                    "application/json",
                    &serde_json::to_vec(&body)?,
                    &identity,
                )
            }
        }
        .with_context(|| tr!("云端语音识别失败：{}", crate::timeline::fmt_ts(seg.start)))?;
        Ok(reply_text(self.mode, &reply)
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    fn workers(&self) -> usize {
        WORKERS
    }
}

/// The transcript in a service's reply, `None` when the reply carries no text at all:
/// `text` from a transcription endpoint; from chat, `message.content`, a string or
/// multimodal parts `[{type:"text", text:...}]` joined.
fn reply_text(mode: AsrApiMode, reply: &Value) -> Option<String> {
    if mode == AsrApiMode::Transcriptions {
        return reply["text"].as_str().map(str::to_owned);
    }
    let content = &reply["choices"][0]["message"]["content"];
    content.as_str().map(str::to_owned).or_else(|| {
        let parts: Vec<&str> = content
            .as_array()?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
        (!parts.is_empty()).then(|| parts.concat())
    })
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

pub const GPU_SETUP_HINT: &str = "请安装支持显卡的 llama.cpp 和驱动，或改用 CPU 识别。 / Install a GPU-capable llama.cpp and driver, or use CPU transcription.";

/// A positive -ngl is only a request: CPU-only llama.cpp builds ignore it.
/// Probe with a deadline so a failing driver cannot hang the task or doctor.
pub fn gpu_devices(bin: &Path) -> Result<Vec<String>> {
    let mut cmd = Command::new(bin);
    cmd.arg("--list-devices");
    let out = crate::runtime::run_bounded("llama-server", &mut cmd, GPU_PROBE_TIMEOUT)
        .with_context(|| tr!("无法检测显卡：{}", GPU_SETUP_HINT))?;
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
    let (_, rows) = output.split_once("Available devices:").context(tr!(
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

/// llama-server 对 offload 相关新 flag 的支持情况。这些 flag 都是新版才有的，
/// 盲目附加会让发行版仓库的旧 llama-server 直接启动失败，故逐条按 --help 探测。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LlamaServerCaps {
    /// `--device`（cpu 后端用于 `--device none` 禁用 GPU 设备）
    device: bool,
    /// `--no-op-offload`（禁止把 host 算子卸载到 GPU）
    no_op_offload: bool,
    /// `--no-mmproj-offload`（多模态 projector 留在 CPU）
    no_mmproj_offload: bool,
}

fn caps_from_help(help: &str) -> LlamaServerCaps {
    LlamaServerCaps {
        device: help.contains("--device"),
        no_op_offload: help.contains("--no-op-offload"),
        no_mmproj_offload: help.contains("--no-mmproj-offload"),
    }
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

/// 组装 llama-server 启动参数：
/// - gpu 后端：`-ngl <gpu_layers>`；mmproj_offload=false 且 llama.cpp 支持时
///   追加 `--no-mmproj-offload`
/// - cpu 后端：`-ngl 0`，并按探测结果追加 `--device none --no-op-offload
///   --no-mmproj-offload`——新版 llama.cpp 下仅 -ngl 0 仍可能把 mmproj/部分
///   算子卸载到 GPU
/// - 不支持的 flag（caps=false）一律不加，兼容发行版仓库的旧 llama-server
fn build_server_args(server: &LlamaServer, caps: LlamaServerCaps, port: u16) -> Vec<String> {
    let cpu_only = server.cpu_only();
    let ngl = if cpu_only { 0 } else { server.gpu_layers };
    let mut args = [
        "-m",
        &server.model.display().to_string(),
        "--mmproj",
        &server.mmproj.display().to_string(),
        "-ngl",
        &ngl.to_string(),
        "-c",
        LLAMA_CTX,
        "-n",
        &LLAMA_MAX_TOKENS.to_string(),
        "-t",
        &server.threads.to_string(),
        "--port",
        &port.to_string(),
        "--host",
        "127.0.0.1",
    ]
    .map(String::from)
    .to_vec();
    if cpu_only {
        if caps.device {
            args.extend(["--device".into(), "none".into()]);
        }
        if caps.no_op_offload {
            args.push("--no-op-offload".into());
        }
    }
    if caps.no_mmproj_offload && (cpu_only || !server.mmproj_offload) {
        args.push("--no-mmproj-offload".into());
    }
    args
}

fn ffmpeg_vad(wav: &Path, max_speech: f64) -> Result<Vec<Seg>> {
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
    // 这个时长决定末段的边界，也就是 checkpoint 的 key
    let dur = ffmpeg_input_duration(log).map_or_else(|| wav_duration(wav), Ok)?;
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
    normalize_segments(invert_silence(dur, &silences), max_speech, wav, dur)
}

/// ffmpeg 输入信息里的 `Duration: HH:MM:SS.xx`（秒）。
fn ffmpeg_input_duration(log: &str) -> Option<f64> {
    let rest = log.split("Duration:").nth(1)?.trim_start();
    let stamp = rest.split([',', ' ', '\n']).next()?;
    let mut parts = stamp.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?)
}

fn open_wav(wav: &Path) -> Result<hound::WavReader<std::io::BufReader<std::fs::File>>> {
    hound::WavReader::open(wav)
        .with_context(|| format!("无法读取音频 / Could not read audio: {}", wav.display()))
}

/// 时长缺失时不能按 0 处理：末段语音（或整段无停顿的音频）会被当成静音丢掉。
pub(crate) fn wav_duration(wav: &Path) -> Result<f64> {
    let reader = open_wav(wav)?;
    let duration = reader.duration() as f64 / reader.spec().sample_rate as f64;
    anyhow::ensure!(
        duration > 0.0,
        tr!("无法读取音频时长 / Could not read the audio duration")
    );
    Ok(duration)
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
struct Energy {
    hop: f64,
    rms: Vec<f32>,
}

impl Energy {
    /// 流式读取 16k 单声道 wav（extract_audio 的固定产物），只保留 rms 数组：
    /// 2h 音频约 1GB，不能整个读进内存。读错（截断文件）时按实际读到的计。
    fn load(wav: &Path) -> Result<Self> {
        const HOP: usize = 1600; // 100ms @16k
        let mut rms = Vec::new();
        let (mut sum, mut count) = (0.0f64, 0usize);
        for sample in open_wav(wav)?.samples::<i16>().map_while(|s| s.ok()) {
            sum += (sample as f64 / 32768.0).powi(2);
            count += 1;
            if count == HOP {
                rms.push((sum / HOP as f64).sqrt() as f32);
                (sum, count) = (0.0, 0);
            }
        }
        if count > 0 {
            rms.push((sum / count as f64).sqrt() as f32);
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
        let (bi, _) = self.rms[i0..=i1]
            .iter()
            .enumerate()
            .min_by(|x, y| x.1.total_cmp(y.1))?;
        Some((i0 + bi) as f64 * self.hop + self.hop / 2.0)
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
/// VAD 成功但无语音 → 空分段；不把整段音频当语音兜底：静音课件会诱发 ASR 幻觉。
pub(crate) fn normalize_segments(
    speech: Vec<(f64, f64)>,
    max_speech: f64,
    wav: &Path,
    dur: f64,
) -> Result<Vec<Seg>> {
    let mut raw = speech;
    raw.retain(|(a, b)| b - a >= 0.2);
    // 能量曲线只在有超长段需要切分时才读（要流式扫描整个音频）
    let energy = raw
        .iter()
        .any(|(a, b)| b - a > max_speech)
        .then(|| Energy::load(wav))
        .and_then(|loaded| {
            loaded
                .inspect_err(|e| {
                    tracing::warn!(
                        "无法读取音频能量，长段将按固定长度切分 / Could not read audio energy; long segments are split at fixed lengths: {e:#}"
                    )
                })
                .ok()
        });

    let mut pieces: Vec<(f64, f64)> = vec![];
    for &(s, e) in &raw {
        split_smart(s, e, max_speech, energy.as_ref(), &mut pieces);
    }

    // 填充：VAD 外边界向真实静音扩展 0.25s（限制来自相邻原始语音段，而非本段自身）；
    // max_speech 内部切点已在能量最低点，不额外填充（避免相邻 chunk 重复文本）。
    Ok(pieces
        .iter()
        .map(|&(s, e)| {
            let host = raw
                .iter()
                .position(|&(rs, re)| s >= rs - 1e-6 && e <= re + 1e-6);
            let (first, last, speech_lo, speech_hi) = host.map_or((true, true, 0.0, dur), |h| {
                let (rs, re) = raw[h];
                (
                    (s - rs).abs() < 1e-6,
                    (re - e).abs() < 1e-6,
                    if h > 0 { raw[h - 1].1 } else { 0.0 },
                    raw.get(h + 1).map_or(dur, |next| next.0),
                )
            });
            Seg {
                start: s,
                end: e,
                cut_start: if first {
                    (s - PAD).max(speech_lo).max(0.0)
                } else {
                    s
                },
                cut_end: if last {
                    (e + PAD).min(speech_hi).max(e)
                } else {
                    e
                },
            }
        })
        .collect())
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

/// One chunk of the task's audio as its own file, cut in process: the audio is
/// `extract_audio`'s PCM WAV, so a local backend needs no ffmpeg start per chunk.
fn cut_wav(src: &Path, start: f64, end: f64, dest: &Path) -> Result<()> {
    let cut = || -> Result<()> {
        let mut reader = open_wav(src)?;
        let spec = reader.spec();
        let rate = spec.sample_rate as f64;
        reader.seek((start.max(0.0) * rate) as u32)?;
        let samples = ((end - start).max(0.05) * rate) as usize * spec.channels as usize;
        let mut writer = hound::WavWriter::create(dest, spec)?;
        for sample in reader.samples::<i16>().take(samples) {
            writer.write_sample(sample?)?;
        }
        writer.finalize()?;
        Ok(())
    };
    cut().context(tr!("无法切分音频 / Could not split audio"))
}

/// 平台默认后端提示（doctor 展示与 main 实际选择保持同一逻辑）。
/// 注意 NPU 只在 llama-server 缺席时兜底：装了 llama-server 的 Intel 机器默认仍走 gpu，
/// 避免把「能跑 GPU」的机器静默降级到更慢的 NPU 管线。
pub fn default_provider_hint() -> crate::config::AsrProvider {
    if cfg!(apple_native) {
        crate::config::AsrProvider::Coreml
    } else if crate::tools::locate("llama-server").is_none() && crate::npu::device_present() {
        crate::config::AsrProvider::Npu
    } else {
        crate::config::AsrProvider::Gpu
    }
}

/// Whether this build includes the native Apple speech backend.
pub fn apple_native_available() -> bool {
    cfg!(apple_native)
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
        let dir = tempfile::tempdir().unwrap();
        let identity = AsrIdentity::new("api", "m", 30.);
        let mut cp = Checkpoint::open(dir.path(), true, &identity).unwrap();
        let mut empties = EmptyResults::default();
        let t = |i: usize| i as f64;
        for i in 0..EMPTY_RESULT_TRIPWIRE - 1 {
            empties.record(&mut cp, t(i), t(i + 1), " ").unwrap();
        }
        let error = empties.record(&mut cp, 100., 101., "").unwrap_err();
        assert!(
            error.to_string().contains("空文本"),
            "连续空结果达到阈值必须熔断"
        );
        assert!(!cp.is_done(t(0), t(1)), "熔断不落账");

        // 一旦出现非空结果（端点其实正常），静音属实，不再熔断，缓冲的空结果落账
        let mut empties = EmptyResults::default();
        empties.record(&mut cp, 0., 1., "").unwrap();
        empties.record(&mut cp, 1., 2., "有字").unwrap();
        assert!(cp.is_done(0., 1.));
        for i in 2..EMPTY_RESULT_TRIPWIRE * 2 {
            empties.record(&mut cp, t(i), t(i + 1), "").unwrap();
        }
    }

    /// A 16 kHz mono WAV: tone, except for silence around `quiet` seconds.
    fn tone_with_gap(path: &Path, seconds: u32, quiet: f64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..seconds * 16000 {
            let t = f64::from(i) / 16000.;
            let level = if (t - quiet).abs() < 0.5 { 0. } else { 8000. };
            let sample = level * (t * 440. * std::f64::consts::TAU).sin();
            writer.write_sample(sample as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn long_speech_splits_at_its_quietest_point_and_chunks_keep_their_samples() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        tone_with_gap(&wav, 30, 18.);
        assert!((wav_duration(&wav).unwrap() - 30.).abs() < 1e-6);
        let segs = normalize_segments(vec![(0., 30.)], 20., &wav, 30.).unwrap();
        assert_eq!(segs.len(), 2, "{segs:?}");
        assert!((segs[0].end - 18.).abs() < 0.5, "{segs:?}");
        // Only the outer edges get padding, and they are clamped to the audio.
        assert_eq!(segs[0].cut_start, 0.);
        assert_eq!(segs[0].cut_end, segs[0].end);

        let chunk = dir.path().join("c.wav");
        cut_wav(&wav, 1.5, 3.5, &chunk).unwrap();
        let reader = hound::WavReader::open(&chunk).unwrap();
        assert_eq!(reader.spec().sample_rate, 16000);
        assert_eq!(reader.duration(), 32000);
        // A cut past the end holds what exists.
        cut_wav(&wav, 29., 40., &chunk).unwrap();
        assert_eq!(wav_duration(&chunk).unwrap(), 1.);
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
        let parse_chat_content = |v: &Value| reply_text(AsrApiMode::Chat, v);
        // 字符串形式（主流 OpenAI 兼容端点）
        let v = serde_json::json!({"choices":[{"message":{"content":"你好世界"}}]});
        assert_eq!(parse_chat_content(&v).unwrap(), "你好世界");
        // 分片数组形式（部分多模态端点）
        let v = serde_json::json!({"choices":[{"message":{"content":[
            {"type":"text","text":"你好"},
            {"type":"text","text":"世界"}
        ]}}]});
        assert_eq!(parse_chat_content(&v).unwrap(), "你好世界");
        // 缺字段/异常响应 → 没有文字，账本按协议失败处理
        let v = serde_json::json!({"choices":[]});
        assert_eq!(parse_chat_content(&v), None);
    }

    // ---------- GPU 卸载控制 ----------

    fn args_of(
        provider: crate::config::AsrProvider,
        gpu_layers: u32,
        mmproj_offload: bool,
        caps: LlamaServerCaps,
    ) -> Vec<String> {
        build_server_args(
            &LlamaServer {
                model: "/m.gguf".into(),
                mmproj: "/mm.gguf".into(),
                provider,
                gpu_layers,
                mmproj_offload,
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

    /// A receipt as the version before the in-process cut wrote it: its identity hashes
    /// a chunk from this exact ffmpeg command. The task must find that receipt, stop
    /// without sending, and resend only under the receipt's own authorization.
    #[test]
    fn a_request_left_unconfirmed_by_an_earlier_version_waits_for_authorization() {
        use crate::dispatch::{ControlSource, Receipt, SNAPSHOT_PREFIX, State};
        if which::which("ffmpeg").is_err() {
            assert!(std::env::var_os("CI").is_none(), "ffmpeg is required in CI");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("speech.wav");
        let chunk = dir.path().join("old-chunk.wav");
        let ffmpeg = |args: Vec<String>| {
            let status = Command::new("ffmpeg").args(&args).status().unwrap();
            assert!(status.success(), "{args:?}");
        };
        let text = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        let tone = "aevalsrc=sin(440*2*PI*t):d=3:s=16000";
        let (quiet, input) = (
            ["-hide_banner", "-loglevel", "error"],
            ["-f", "lavfi", "-i"],
        );
        let mut args = text(&quiet);
        args.extend(text(&input));
        args.extend(text(&[
            tone,
            "-ac",
            "1",
            "-ar",
            "16000",
            wav.to_str().unwrap(),
        ]));
        ffmpeg(args);
        let segs = ffmpeg_vad(&wav, 30.).unwrap();
        assert_eq!(segs.len(), 1, "{segs:?}");
        let seg = segs[0];
        let start = format!("{:.3}", seg.cut_start);
        let length = format!("{:.3}", (seg.cut_end - seg.cut_start).max(0.05));
        let mut args = text(&quiet);
        args.extend(text(&[
            "-nostdin", "-y", "-ss", &start, "-t", &length, "-i",
        ]));
        args.extend(text(&[wav.to_str().unwrap(), "-ac", "1", "-ar", "16000"]));
        args.extend(text(&["-c:a", "pcm_s16le", chunk.to_str().unwrap()]));
        ffmpeg(args);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let api = AsrApi {
            base_url: base,
            api_key: "key".into(),
            model: "test-asr".into(),
            mode: AsrApiMode::Transcriptions,
        };
        let endpoint = crate::config::asr_endpoint(&api).unwrap();
        // The formulas here are the old version's, written out on purpose.
        let digest = crate::digest::digest;
        let payload = json!({"model":"test-asr","mode":api.mode,"audio_sha256":digest(&std::fs::read(&chunk).unwrap()),"segment_start":seg.start,"segment_end":seg.end,"cut_start":seg.cut_start,"cut_end":seg.cut_end});
        let service_version = format!("{SNAPSHOT_PREFIX}{}", digest(endpoint.as_bytes()));
        let mut identity = json!({"service_version":service_version,"purpose":"transcription","endpoint":endpoint,"payload":payload});
        identity.sort_all_objects();
        let stable_id = digest(&serde_json::to_vec(&identity).unwrap());
        let requests = dir.path().join("requests");
        std::fs::create_dir_all(&requests).unwrap();
        let receipt_path = requests.join(format!("{stable_id}.json"));
        let receipt = Receipt {
            schema: 1,
            stable_id: stable_id.clone(),
            request_id: format!("{stable_id}.1"),
            purpose: "transcription".into(),
            description: String::new(),
            service_version,
            attempt: 1,
            state: State::Sending,
            http_status: None,
            response: None,
            message: None,
            unsupported_response_format: false,
            retry_authorized: None,
        };
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let control_path = dir.path().join("control.json");
        let open_control = || {
            crate::dispatch::TaskControl::open(
                dir.path(),
                ControlSource {
                    control_path: Some(control_path.clone()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let identity = AsrIdentity::new("api", "test-asr", 30.);
        let run = |control: &TaskControl| {
            let mut cp = Checkpoint::open(dir.path(), true, &identity).unwrap();
            transcribe_all(Backend::Api(api.clone()), &wav, 30., &mut cp, control)
        };
        let saved = || -> Receipt {
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap()
        };

        // Without authorization: blocked on this receipt, and nothing reached the service.
        listener.set_nonblocking(true).unwrap();
        let error = run(&open_control()).unwrap_err();
        let failure = error.downcast_ref::<crate::dispatch::Failure>();
        assert!(failure.is_some_and(|f| f.uncertain()), "{error:#}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "the request was sent again"
        );
        // Matched by identity: this exact receipt moved on, and no other was started.
        assert_eq!(saved().state, State::Uncertain);
        assert_eq!(saved().attempt, 1);
        assert_eq!(std::fs::read_dir(&requests).unwrap().count(), 1);

        // Authorizing that attempt sends it once more, under the same identity.
        std::fs::write(
            &control_path,
            serde_json::to_vec(&json!({"resend":[format!("{stable_id}.1")]})).unwrap(),
        )
        .unwrap();
        listener.set_nonblocking(false).unwrap();
        let server = crate::test_http::serve_on(
            listener,
            vec![crate::test_http::json_response(r#"{"text":"spoken"}"#)],
        );
        let events = run(&open_control()).unwrap();
        assert_eq!(server.join().unwrap().len(), 1);
        assert_eq!(events[0].text, "spoken");
        assert_eq!(saved().state, State::Completed);
        assert_eq!(saved().attempt, 2);
    }

    /// Cloud transcription runs its chunks on the shared worker pool and
    /// records every result into the checkpoint, so a resumed task sends
    /// nothing for chunks already transcribed.
    #[test]
    fn cloud_chunks_are_transcribed_on_the_pool_and_checkpointed() {
        if which::which("ffmpeg").ok().is_none() {
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
        let cloud = || Backend::Api(api.clone());
        let events = transcribe_all(cloud(), &wav, 30., &mut cp, &Default::default()).unwrap();
        assert_eq!(events.len(), segments);
        assert!(events.iter().all(|event| event.text == "spoken"));
        drop(cp);
        // Resuming finds every chunk done and sends nothing (the fake server
        // has no responses left).
        let mut cp = crate::checkpoint::Checkpoint::open(dir.path(), true, &identity).unwrap();
        let again = transcribe_all(cloud(), &wav, 30., &mut cp, &Default::default()).unwrap();
        assert_eq!(again.len(), segments);
    }
}
