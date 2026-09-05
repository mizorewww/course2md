//! course2md 桌面端后端：spawn CLI（--json NDJSON 事件流）并转发事件到前端。
//!
//! 进程模型：std::process + 线程（不引 tokio）。
//! - stdout 行：能解析成带 "type" 字段的 JSON → 原样转发；否则包成 log 事件（容错）。
//! - stderr 行：一律包成 level=info 的 log 事件，message 保持原文。
//! - 事件经 `job-event`（{job_id, event}）发到前端；进程退出发 `job-exit`（{job_id, code}）。
//! - 取消：unix 下子进程 setsid 独立进程组，cancel 时 killpg 连带杀掉
//!   ffmpeg / llama-server 等子孙；Windows 用 taskkill /T /F。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Job 表
// ---------------------------------------------------------------------------

struct JobHandle {
    cancel: std::sync::mpsc::Sender<()>,
    kind: JobKind,
}

/// 任务类别：转换与模型下载分开计数——同类拒绝并发，不同类允许各一个（S1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Conversion,
    Download,
}

impl JobKind {
    fn busy_msg(&self) -> &'static str {
        match self {
            Self::Conversion => "已有任务进行中",
            Self::Download => "已有模型下载进行中",
        }
    }
}

static JOBS: LazyLock<Mutex<HashMap<String, JobHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_job_id() -> String {
    let n = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("job-{ts}-{n}")
}

// ---------------------------------------------------------------------------
// CLI 二进制解析
// ---------------------------------------------------------------------------

/// 解析顺序：env COURSE2MD_BIN → 当前 exe 同目录 course2md（sidecar 落地位置）
/// → exe 同目录 binaries/course2md（防布局差异）→ PATH（含 GUI 应用常见缺失的
/// homebrew 等目录）。返回 (路径, 来源)。
fn resolve_cli() -> Result<(PathBuf, &'static str), String> {
    if let Ok(p) = std::env::var("COURSE2MD_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok((p, "env"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = format!("course2md{}", std::env::consts::EXE_SUFFIX);
            for cand in [dir.join(&name), dir.join("binaries").join(&name)] {
                if cand.is_file() {
                    return Ok((cand, "bundled"));
                }
            }
        }
    }
    if let Some(p) = which("course2md") {
        return Ok((p, "path"));
    }
    Err("找不到 course2md CLI：请设置 COURSE2MD_BIN 或把 course2md 加入 PATH".into())
}

/// 在 PATH 里找可执行文件；unix 上额外找 GUI 应用 PATH 里常缺的位置。
fn which(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    #[cfg(unix)]
    {
        // 从 Finder/Dock 启动的 GUI 应用 PATH 只有 /usr/bin:/bin…，brew 工具在 /opt/homebrew/bin
        if let Some(home) = home_dir() {
            dirs.push(home.join("bin"));
            dirs.push(home.join(".local").join("bin"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    #[cfg(windows)]
    let exe = format!("{name}.exe");
    #[cfg(not(windows))]
    let exe = name.to_string();
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// 事件转发
// ---------------------------------------------------------------------------

fn emit_event(app: &AppHandle, job_id: &str, event: serde_json::Value) {
    if let Err(e) = app.emit(
        "job-event",
        serde_json::json!({ "job_id": job_id, "event": event }),
    ) {
        eprintln!("emit job-event 失败: {e}");
    }
}

/// stdout 一行：带 "type" 的 JSON → 原样；否则包成 log。
fn stdout_line_to_event(line: &str) -> serde_json::Value {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if v.get("type").is_some() {
            return v;
        }
    }
    serde_json::json!({ "type": "log", "level": "info", "message": line })
}

/// stderr 一行：匹配 Error:/error:/error[ 前缀的升级为 error 级（S5），其余 info。
fn stderr_line_to_event(line: &str) -> serde_json::Value {
    let t = line.trim_start();
    let level = if t.starts_with("Error:") || t.starts_with("error:") || t.starts_with("error[") {
        "error"
    } else {
        "info"
    };
    serde_json::json!({ "type": "log", "level": level, "message": line })
}

/// 逐行读取 → 事件；50ms 窗口内的多行合并成一次 emit（M9 限频）。
/// 单条窗口直接发原事件；多条发 {type:"logs", logs:[...]}，前端两种都兼容。
fn spawn_line_reader(
    app: AppHandle,
    job_id: String,
    reader: impl BufRead + Send + 'static,
    is_stderr: bool,
) -> std::thread::JoinHandle<()> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::sync_channel::<serde_json::Value>(512);
    std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            let event = if is_stderr {
                stderr_line_to_event(line)
            } else {
                stdout_line_to_event(line)
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let mut buf: Vec<serde_json::Value> = Vec::new();
        let mut deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                Ok(ev) => buf.push(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
                    if buf.len() == 1 {
                        emit_event(&app, &job_id, buf.remove(0));
                    } else if !buf.is_empty() {
                        emit_event(
                            &app,
                            &job_id,
                            serde_json::json!({ "type": "logs", "logs": buf }),
                        );
                        buf = Vec::new();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if !buf.is_empty() {
                        emit_event(
                            &app,
                            &job_id,
                            serde_json::json!({ "type": "logs", "logs": buf }),
                        );
                    }
                    break;
                }
            }
        }
    })
}

/// 杀进程组/进程树：unix killpg（子进程已 setsid，pgid==pid），windows taskkill /T /F。
#[cfg(unix)]
const SIG_KILL: i32 = libc::SIGKILL;
#[cfg(windows)]
const SIG_KILL: i32 = 9;

fn kill_process_tree(pid: u32, sig: i32) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as i32, sig);
    }
    #[cfg(windows)]
    {
        let _ = sig;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// spawn CLI 子进程并接管输出转发，返回 job_id。start_job / download_models 共用。
/// 同类任务已在跑时拒绝并发（S1）。
fn spawn_cli_job(app: &AppHandle, args: &[String], kind: JobKind) -> Result<String, String> {
    let mut jobs = JOBS.lock().map_err(|e| e.to_string())?;
    if jobs.values().any(|j| j.kind == kind) {
        return Err(kind.busy_msg().to_string());
    }
    let (bin, _) = resolve_cli()?;
    let mut cmd = Command::new(&bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // macOS 打包场景：mlx.metallib 只能放 Contents/Resources/（codesign 把
    // MacOS/ 下文件都当代码），MLX 的最后一条搜索路径是 CWD 下的
    // default.metallib——故 bundled sidecar 用 exe 旁的 ../Resources 作 CWD。
    // dev/PATH 场景该目录不存在，保持继承父进程 CWD。
    #[cfg(target_os = "macos")]
    if let Some(res) = bin
        .parent()
        .map(|p| p.join("../Resources"))
        .filter(|p| p.join("default.metallib").is_file())
    {
        cmd.current_dir(res);
    }
    #[cfg(unix)]
    {
        // 独立进程组：cancel 时 killpg 一锅端（含 ffmpeg/llama-server 等子孙）
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child: Child = cmd
        .spawn()
        .map_err(|e| format!("启动 {} 失败: {e}", bin.display()))?;
    let pid = child.id();
    let job_id = next_job_id();

    let (cancel, cancellation) = std::sync::mpsc::channel();
    jobs.insert(job_id.clone(), JobHandle { cancel, kind });
    drop(jobs);
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(spawn_line_reader(app.clone(), job_id.clone(), BufReader::new(out), false));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(spawn_line_reader(app.clone(), job_id.clone(), BufReader::new(err), true));
    }

    let app2 = app.clone();
    let job_id2 = job_id.clone();
    std::thread::spawn(move || {
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Err(_) => { kill_process_tree(pid, SIG_KILL); let _ = child.wait(); break None; }
                Ok(None) => {}
            }
            if cancellation.try_recv().is_ok() {
                kill_process_tree(pid, SIG_KILL);
                break child.wait().ok().and_then(|status| status.code());
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        };
        for reader in readers { let _ = reader.join(); }
        JOBS.lock().map(|mut m| m.remove(&job_id2)).ok();
        if let Err(e) = app2.emit(
            "job-exit",
            serde_json::json!({ "job_id": job_id2, "code": code }),
        ) {
            eprintln!("emit job-exit 失败: {e}");
        }
    });

    Ok(job_id)
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ProviderInfo {
    id: String,
    label: String,
    available: bool,
    recommended: bool,
    note: String,
}

#[derive(Serialize)]
pub struct EnvInfo {
    os: String,
    arch: String,
    apple_silicon: bool,
    has_ffmpeg: bool,
    has_ffprobe: bool,
    has_ytdlp: bool,
    has_llama_server: bool,
    cli_path: String,
    cli_version: String,
    cli_source: String,
    providers: Vec<ProviderInfo>,
}

#[tauri::command]
fn detect_environment() -> Result<EnvInfo, String> {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let apple_silicon = os == "macos" && arch == "aarch64";
    let intel_linux = os == "linux" && arch == "x86_64";

    let has_ffmpeg = which("ffmpeg").is_some();
    let has_ffprobe = which("ffprobe").is_some();
    let has_ytdlp = which("yt-dlp").is_some();
    let has_llama_server = which("llama-server").is_some();

    let (cli_path, cli_version, cli_source) = match resolve_cli() {
        Ok((p, source)) => {
            let ver = Command::new(&p)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".into());
            (p.display().to_string(), ver, source.to_string())
        }
        Err(e) => (String::new(), String::new(), e),
    };

    // 推荐优先级：Apple Silicon → coreml；Intel Linux → npu；有 llama-server → gpu
    let coreml_rec = apple_silicon;
    let npu_rec = !coreml_rec && intel_linux;
    let gpu_rec = !coreml_rec && !npu_rec && has_llama_server;
    let providers = vec![
        ProviderInfo {
            id: "coreml".into(),
            label: "CoreML（Apple 原生）".into(),
            available: apple_silicon,
            recommended: coreml_rec,
            note: if apple_silicon {
                "Neural Engine 加速，零外部依赖，推荐".into()
            } else {
                "仅 Apple Silicon macOS 可用".into()
            },
        },
        ProviderInfo {
            id: "gpu".into(),
            label: "GPU（llama.cpp）".into(),
            available: has_llama_server,
            recommended: gpu_rec,
            note: if has_llama_server {
                "Metal/CUDA/Vulkan 加速，速度快".into()
            } else {
                "需要安装 llama.cpp（llama-server）".into()
            },
        },
        ProviderInfo {
            id: "npu".into(),
            label: "NPU（Intel）".into(),
            available: intel_linux,
            recommended: npu_rec,
            note: if intel_linux {
                "Intel Core Ultra NPU 硬件加速".into()
            } else {
                "仅 Intel Linux/Windows 可用".into()
            },
        },
        ProviderInfo {
            id: "cpu".into(),
            label: "CPU".into(),
            available: true,
            recommended: false,
            note: "纯 CPU 运行，通用兜底，较慢".into(),
        },
        ProviderInfo {
            id: "api".into(),
            label: "云端 API".into(),
            available: true,
            recommended: false,
            note: "免本地模型下载，需配置 OpenAI 兼容端点与 API Key".into(),
        },
    ];

    Ok(EnvInfo {
        os,
        arch,
        apple_silicon,
        has_ffmpeg,
        has_ffprobe,
        has_ytdlp,
        has_llama_server,
        cli_path,
        cli_version,
        cli_source,
        providers,
    })
}

/// JS 侧按 camelCase 传参；除 source 外全部可选，None 就不传对应 flag。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOpts {
    pub source: String,
    pub out: Option<String>,
    pub provider: Option<String>,
    pub asr_model: Option<String>,
    pub asr_api_base_url: Option<String>,
    pub asr_api_key: Option<String>,
    pub asr_api_model: Option<String>,
    pub asr_api_mode: Option<String>,
    pub similarity: Option<f64>,
    pub sample_interval: Option<f64>,
    pub cooldown: Option<f64>,
    pub max_height: Option<u32>,
    pub slide_mode: Option<String>,
    pub formats: Option<Vec<String>>,
    pub llm: Option<bool>,
    pub no_llm: Option<bool>,
    pub keep_video: Option<bool>,
    pub resume: Option<bool>,
    pub transcript_source: Option<String>,
    pub threads: Option<i32>,
}

fn push_flag(args: &mut Vec<String>, flag: &str, val: impl std::fmt::Display) {
    args.push(flag.to_string());
    args.push(val.to_string());
}

fn push_opt(args: &mut Vec<String>, flag: &str, val: &Option<impl std::fmt::Display>) {
    if let Some(v) = val {
        push_flag(args, flag, v);
    }
}

impl JobOpts {
    fn to_args(&self) -> Vec<String> {
        let mut args = vec![self.source.clone()];
        push_opt(&mut args, "--out", &self.out);
        push_opt(&mut args, "--provider", &self.provider);
        push_opt(&mut args, "--asr-model", &self.asr_model);
        push_opt(&mut args, "--asr-api-base-url", &self.asr_api_base_url);
        push_opt(&mut args, "--asr-api-key", &self.asr_api_key);
        push_opt(&mut args, "--asr-api-model", &self.asr_api_model);
        push_opt(&mut args, "--asr-api-mode", &self.asr_api_mode);
        push_opt(&mut args, "--similarity", &self.similarity);
        push_opt(&mut args, "--sample-interval", &self.sample_interval);
        push_opt(&mut args, "--cooldown", &self.cooldown);
        push_opt(&mut args, "--max-height", &self.max_height);
        push_opt(&mut args, "--slide-mode", &self.slide_mode);
        if let Some(v) = &self.formats {
            if !v.is_empty() {
                push_flag(&mut args, "--formats", v.join(","));
            }
        }
        if self.llm == Some(true) {
            args.push("--llm".into());
        }
        if self.no_llm == Some(true) {
            args.push("--no-llm".into());
        }
        if self.keep_video == Some(true) {
            args.push("--keep-video".into());
        }
        if self.resume == Some(true) {
            args.push("--resume".into());
        }
        push_opt(&mut args, "--transcript-source", &self.transcript_source);
        push_opt(&mut args, "--threads", &self.threads);
        // 永远附加 --json：GUI 依赖 NDJSON 事件流
        args.push("--json".into());
        args
    }
}

#[tauri::command]
fn start_job(app: AppHandle, opts: JobOpts) -> Result<String, String> {
    spawn_cli_job(&app, &opts.to_args(), JobKind::Conversion)
}

#[tauri::command]
fn cancel_job(job_id: String) -> Result<bool, String> {
    let jobs = JOBS.lock().map_err(|e| e.to_string())?;
    Ok(jobs.get(&job_id).is_some_and(|job| job.cancel.send(()).is_ok()))
}

#[tauri::command]
fn download_models(app: AppHandle) -> Result<String, String> {
    let args: Vec<String> = ["models", "download", "--json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    spawn_cli_job(&app, &args, JobKind::Download)
}

#[tauri::command]
fn models_list() -> Result<String, String> {
    let (bin, _) = resolve_cli()?;
    let out = Command::new(&bin)
        .args(["models", "list"])
        .output()
        .map_err(|e| format!("启动 {} 失败: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!(
            "models list 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

// ---------------------------------------------------------------------------
// 设置（复刻 CLI 的 config 路径逻辑：XDG_CONFIG_HOME/course2md/config.toml；
// Windows %APPDATA%\course2md；否则 ~/.config/course2md/config.toml）
// ---------------------------------------------------------------------------

fn config_path() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("course2md").join("config.toml");
    }
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("APPDATA") {
            return PathBuf::from(d).join("course2md").join("config.toml");
        }
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("course2md")
        .join("config.toml")
}

// ---------------------------------------------------------------------------
// 类型化配置（复刻 CLI src/settings.rs 的 schema；不 path-depend 根 crate）
// 命名约定：TOML/JSON 字段 snake_case，enum 一律 lowercase 字符串。
// Option = 未设置（回落 CLI 内置默认），不写盘。
// 不加 deny_unknown_fields：CLI 加新字段时老 app 仍能读。
// ---------------------------------------------------------------------------

/// `[defaults]`：全部可选（对齐 CLI settings::Defaults）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DefaultsDto {
    pub out: Option<String>,
    pub similarity: Option<f64>,
    pub sample_interval: Option<f64>,
    pub cooldown: Option<f64>,
    pub slide_mode: Option<String>, // "first" | "stable"
    pub stable_secs: Option<f64>,
    pub max_height: Option<u32>,
    pub roi: Option<String>,
    pub threads: Option<i32>,
    pub provider: Option<String>, // "coreml" | "gpu" | "npu" | "cpu" | "api"
    pub asr_model: Option<String>,
    pub transcript_source: Option<String>, // "auto" | "subtitle" | "asr"
    pub max_speech: Option<f32>,
    pub formats: Option<Vec<String>>, // "md" | "html" | "json"
    pub model_dir: Option<String>,
    pub keep_video: Option<bool>,
    pub no_download: Option<bool>,
    pub resume: Option<bool>,
}

/// `[llm]`：默认 enabled=false、concurrency=None（= 内置 8），其余空（对齐 CLI llm::LlmSettings）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmDto {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 自定义校对指令；None 或空串都视为未设置
    pub prompt: Option<String>,
    pub disable_hint: bool,
    pub vision: bool,
    pub summarize: bool,
    /// None = 回落内置默认 8
    pub concurrency: Option<usize>,
}

fn default_asr_api_base_url() -> String {
    "https://openrouter.ai/api/v1".into()
}
fn default_asr_api_model() -> String {
    "qwen/qwen3-asr-flash-2026-02-10".into()
}

/// `[asr_api]`（对齐 CLI settings::AsrApi 的内置默认）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AsrApiDto {
    #[serde(default = "default_asr_api_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_asr_api_model")]
    pub model: String,
    /// "transcriptions" | "chat"；空串 = 未设置 = transcriptions
    #[serde(default)]
    pub mode: String,
}

impl Default for AsrApiDto {
    fn default() -> Self {
        Self {
            base_url: default_asr_api_base_url(),
            api_key: String::new(),
            model: default_asr_api_model(),
            mode: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConfigDto {
    pub defaults: DefaultsDto,
    pub llm: LlmDto,
    pub asr_api: AsrApiDto,
}

#[derive(Serialize)]
pub struct ConfigResponse {
    path: String,
    exists: bool,
    config: ConfigDto,
}

/// 读取配置文件为结构化 DTO。文件不存在 → 全默认（exists=false）；
/// 存在但解析失败 → 硬错误（toml 报错自带行列），不静默回落默认。
#[tauri::command]
fn get_config() -> Result<ConfigResponse, String> {
    let p = config_path();
    if !p.is_file() {
        return Ok(ConfigResponse {
            path: p.display().to_string(),
            exists: false,
            config: ConfigDto::default(),
        });
    }
    let s = std::fs::read_to_string(&p).map_err(|e| format!("无法读取配置文件 {}: {e}", p.display()))?;
    let config: ConfigDto = toml::from_str(&s)
        .map_err(|e| format!("配置文件解析失败（修正后重试；本次不回退默认值）：{}\n{e}", p.display()))?;
    Ok(ConfigResponse {
        path: p.display().to_string(),
        exists: true,
        config,
    })
}

fn check_enum(field: &str, val: &Option<String>, allowed: &[&str]) -> Result<(), String> {
    if let Some(v) = val {
        if !allowed.contains(&v.as_str()) {
            return Err(format!(
                "defaults.{field} 只能是 {}，收到 \"{v}\"",
                allowed.join(" / ")
            ));
        }
    }
    Ok(())
}

fn check_positive(field: &str, val: Option<f64>) -> Result<(), String> {
    if let Some(v) = val {
        if !(v > 0.0 && v.is_finite()) {
            return Err(format!("{field} 必须是正数"));
        }
    }
    Ok(())
}

fn validate_config(cfg: &ConfigDto) -> Result<ConfigDto, String> {
    let d = &cfg.defaults;
    check_enum("provider", &d.provider, &["coreml", "gpu", "npu", "cpu", "api"])?;
    check_enum("slide_mode", &d.slide_mode, &["first", "stable"])?;
    check_enum("transcript_source", &d.transcript_source, &["auto", "subtitle", "asr"])?;
    if let Some(formats) = &d.formats {
        for f in formats {
            if !["md", "html", "json"].contains(&f.as_str()) {
                return Err(format!("defaults.formats 只能是 md / html / json，收到 \"{f}\""));
            }
        }
    }
    if let Some(v) = d.similarity {
        if !(v > 0.0 && v <= 1.0) {
            return Err(format!("defaults.similarity 必须在 (0, 1] 区间，收到 {v}"));
        }
    }
    check_positive("defaults.sample_interval", d.sample_interval)?;
    check_positive("defaults.stable_secs", d.stable_secs)?;
    check_positive("defaults.max_speech", d.max_speech.map(|v| v as f64))?;
    if let Some(v) = d.cooldown {
        if !(v >= 0.0 && v.is_finite()) {
            return Err(format!("defaults.cooldown 不能为负数，收到 {v}"));
        }
    }
    if let Some(t) = d.threads {
        if t <= 0 {
            return Err(format!("defaults.threads 必须是正整数，收到 {t}"));
        }
    }
    if let Some(mh) = d.max_height {
        if mh == 0 {
            return Err("defaults.max_height 必须是正整数".into());
        }
    }
    let mode = cfg.asr_api.mode.trim();
    if !mode.is_empty() && !["transcriptions", "chat"].contains(&mode) {
        return Err(format!("asr_api.mode 只能是 transcriptions / chat，收到 \"{mode}\""));
    }
    if let Some(c) = cfg.llm.concurrency {
        if c == 0 {
            return Err("llm.concurrency 必须是正整数".into());
        }
    }
    // 对齐 CLI llm::validate：启用润色就必须有 base_url 和 model
    if cfg.llm.enabled {
        if cfg.llm.base_url.trim().is_empty() {
            return Err("已开启 LLM 润色，但 llm.base_url 未配置".into());
        }
        if cfg.llm.model.trim().is_empty() {
            return Err("已开启 LLM 润色，但 llm.model 未配置".into());
        }
    }

    // 归一化（空 = 未设置，不落盘）
    let mut out = cfg.clone();
    let clean = |v: &mut Option<String>| {
        if v.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
            *v = None;
        }
    };
    clean(&mut out.defaults.out);
    clean(&mut out.defaults.roi);
    clean(&mut out.defaults.asr_model);
    clean(&mut out.defaults.model_dir);
    if out.defaults.formats.as_deref().map(|f| f.is_empty()).unwrap_or(true) {
        out.defaults.formats = None;
    }
    if out.llm.prompt.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
        out.llm.prompt = None;
    }
    Ok(out)
}

// 写盘视图：None / 空串 / false 一律跳过，保持 config.toml 干净、可读。
#[derive(Serialize)]
struct DefaultsOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_interval: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cooldown: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slide_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stable_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    roi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threads: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    asr_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_speech: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    formats: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_video: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_download: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resume: Option<bool>,
}

impl From<&DefaultsDto> for DefaultsOut {
    fn from(d: &DefaultsDto) -> Self {
        Self {
            out: d.out.clone(),
            similarity: d.similarity,
            sample_interval: d.sample_interval,
            cooldown: d.cooldown,
            slide_mode: d.slide_mode.clone(),
            stable_secs: d.stable_secs,
            max_height: d.max_height,
            roi: d.roi.clone(),
            threads: d.threads,
            provider: d.provider.clone(),
            asr_model: d.asr_model.clone(),
            transcript_source: d.transcript_source.clone(),
            max_speech: d.max_speech,
            formats: d.formats.clone(),
            model_dir: d.model_dir.clone(),
            keep_video: d.keep_video,
            no_download: d.no_download,
            resume: d.resume,
        }
    }
}

#[derive(Serialize)]
struct LlmOut {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    enabled: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    base_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disable_hint: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    vision: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    summarize: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    concurrency: Option<usize>,
}

impl From<&LlmDto> for LlmOut {
    fn from(l: &LlmDto) -> Self {
        Self {
            enabled: l.enabled,
            base_url: l.base_url.trim().to_string(),
            api_key: l.api_key.trim().to_string(),
            model: l.model.trim().to_string(),
            prompt: l.prompt.clone(),
            disable_hint: l.disable_hint,
            vision: l.vision,
            summarize: l.summarize,
            concurrency: l.concurrency,
        }
    }
}

#[derive(Serialize)]
struct AsrApiOut {
    #[serde(skip_serializing_if = "String::is_empty")]
    base_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    mode: String,
}

impl From<&AsrApiDto> for AsrApiOut {
    fn from(a: &AsrApiDto) -> Self {
        Self {
            base_url: a.base_url.trim().to_string(),
            api_key: a.api_key.trim().to_string(),
            model: a.model.trim().to_string(),
            mode: a.mode.trim().to_string(),
        }
    }
}

#[derive(Serialize)]
struct ConfigOut {
    defaults: DefaultsOut,
    asr_api: AsrApiOut,
    llm: LlmOut,
}

/// 写含 API key 的配置文件：tmp（unix 0600）→ fsync → rename，
/// 语义对齐 CLI settings::save / write_private。
#[cfg(unix)]
fn write_config_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("创建 {} 失败: {e}", tmp.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} → {} 失败: {e}", tmp.display(), path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

#[cfg(not(unix))]
fn write_config_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} → {} 失败: {e}", tmp.display(), path.display()))
}

#[tauri::command]
fn save_config(cfg: ConfigDto) -> Result<(), String> {
    let cfg = validate_config(&cfg)?;
    let out = ConfigOut {
        defaults: DefaultsOut::from(&cfg.defaults),
        asr_api: AsrApiOut::from(&cfg.asr_api),
        llm: LlmOut::from(&cfg.llm),
    };
    let toml_text = toml::to_string_pretty(&out).map_err(|e| format!("序列化配置失败: {e}"))?;
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    }
    // 覆盖前备份旧配置（用户可能有手改内容），失败只告警不阻断
    if p.is_file() {
        let bak = p.with_extension("toml.bak");
        let _ = std::fs::copy(&p, &bak);
    }
    write_config_private(&p, toml_text.as_bytes())?;
    Ok(())
}

#[tauri::command]
fn default_out_root() -> String {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("course2md")
        .display()
        .to_string()
}

// ---------------------------------------------------------------------------
// 历史记录与结果读取
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct HistoryItem {
    dir: String,
    title: String,
    platform: String,
    slides: u64,
    segments: u64,
    elapsed_secs: f64,
    modified: u64,
}

fn title_for_dir(dir: &Path) -> String {
    // 同目录 meta.json 的 "title"（VideoMeta 序列化产物）
    if let Ok(s) = std::fs::read_to_string(dir.join("meta.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            if let Some(t) = v.get("title").and_then(|t| t.as_str()) {
                if !t.is_empty() {
                    return t.to_string();
                }
            }
        }
    }
    // 布局是 <platform>/<title>/<id>/，父目录名即标题 slug
    dir.parent()
        .and_then(|p| p.file_name())
        .or_else(|| dir.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn read_run_json(dir: &Path) -> Option<serde_json::Value> {
    let s = std::fs::read_to_string(dir.join("run.json")).ok()?;
    serde_json::from_str(&s).ok()
}

fn history_item(dir: &Path) -> Option<HistoryItem> {
    let run = read_run_json(dir)?;
    let num = |key: &str| run.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let modified = std::fs::metadata(dir.join("run.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(HistoryItem {
        dir: dir.display().to_string(),
        title: title_for_dir(dir),
        platform: run
            .pointer("/source/platform")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        slides: num("sections"),
        segments: num("speech_segments"),
        elapsed_secs: run
            .get("elapsed_secs")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        modified,
    })
}

fn collect_history(dir: &Path, depth: u32, out: &mut Vec<HistoryItem>) {
    if depth > 3 {
        return;
    }
    if dir.join("run.json").is_file() {
        if let Some(item) = history_item(dir) {
            out.push(item);
        }
        return; // run 目录是叶子，不再下钻
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // 单个目录坏了跳过
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_history(&p, depth + 1, out);
        }
    }
}

#[tauri::command]
fn list_history(out_root: String) -> Result<Vec<HistoryItem>, String> {
    let mut items = Vec::new();
    collect_history(Path::new(&out_root), 0, &mut items);
    items.sort_by_key(|i| std::cmp::Reverse(i.modified));
    Ok(items)
}

#[derive(Serialize)]
pub struct ResultData {
    markdown: Option<String>,
    has_html: bool,
    frames: Vec<String>,
    /// 目录里实际存在的产物文件（L3：文件 tab 只列这些）
    files: Vec<String>,
    run_json: Option<serde_json::Value>,
    title: String,
}

#[tauri::command]
fn read_result(out_dir: String) -> Result<ResultData, String> {
    let dir = Path::new(&out_dir);
    if !dir.is_dir() {
        return Err(format!("目录不存在: {out_dir}"));
    }
    let markdown = std::fs::read_to_string(dir.join("course.md")).ok();
    let has_html = dir.join("course.html").is_file();
    let files: Vec<String> = ["course.md", "course.html", "structured.json", "timeline.jsonl", "run.json"]
        .iter()
        .filter(|name| dir.join(name).is_file())
        .map(|s| s.to_string())
        .collect();
    let mut frames: Vec<String> = std::fs::read_dir(dir.join("frames"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
                        .unwrap_or(false)
                })
                .map(|p| p.display().to_string())
                .collect()
        })
        .unwrap_or_default();
    frames.sort();
    Ok(ResultData {
        markdown,
        has_html,
        frames,
        files,
        run_json: read_run_json(dir),
        title: title_for_dir(dir),
    })
}

/// S4：root 限定 + canonicalize 防越界 + 扩展名白名单。
#[tauri::command]
fn read_image(root: String, path: String) -> Result<String, String> {
    use base64::Engine;
    const ALLOWED_EXT: [&str; 5] = ["jpg", "jpeg", "png", "webp", "gif"];
    let p = Path::new(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_EXT.contains(&ext.as_str()) {
        return Err(format!("不允许的图片类型: {ext}"));
    }
    let canon_root = std::fs::canonicalize(&root)
        .map_err(|e| format!("root 目录无效 {root}: {e}"))?;
    let canon = std::fs::canonicalize(p).map_err(|e| format!("读取 {} 失败: {e}", p.display()))?;
    if !canon.starts_with(&canon_root) {
        return Err(format!("图片路径越界（不在 {root} 内）"));
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("读取 {} 失败: {e}", canon.display()))?;
    const MAX: u64 = 20 * 1024 * 1024;
    if meta.len() > MAX {
        return Err(format!("图片超过 20MB: {}", canon.display()));
    }
    let bytes = std::fs::read(&canon).map_err(|e| format!("读取 {} 失败: {e}", canon.display()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

// ---------------------------------------------------------------------------
// 系统交互：打开 / 显示文件、选择视频
// ---------------------------------------------------------------------------

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_path(path: String) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn pick_video_file(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let file = app
        .dialog()
        .file()
        .add_filter("Video", &["mp4", "mkv", "webm", "mov", "avi", "flv"])
        .blocking_pick_file();
    Ok(file.map(|f| f.to_string()))
}

/// L5：选择输出目录（dialog 目录模式）。
#[tauri::command]
fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let dir = app.dialog().file().blocking_pick_folder();
    Ok(dir.map(|f| f.to_string()))
}

// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            detect_environment,
            start_job,
            cancel_job,
            download_models,
            models_list,
            get_config,
            save_config,
            default_out_root,
            list_history,
            read_result,
            read_image,
            open_path,
            reveal_path,
            pick_video_file,
            pick_directory,
        ])
        .build(tauri::generate_context!())
        .expect("error while building course2md app");
    // M8：退出前杀掉所有子进程组，避免 CLI/ffmpeg/llama-server 变孤儿
    app.run(|_app, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            let handles: Vec<JobHandle> = JOBS
                .lock()
                .map(|mut m| m.drain().map(|(_, j)| j).collect())
                .unwrap_or_default();
            for h in handles {
                let _ = h.cancel.send(());
            }
        }
    });
}
