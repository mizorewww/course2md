//! Intel NPU 后端（OpenVINO WhisperPipeline）。
//!
//! 在具备 Intel Core Ultra (AI Boost) NPU 的设备上，
//! 通过 OpenVINO GenAI 将 Whisper 模型编译并在 NPU 上高速运行。
//! 通过轻量 Python 伴随进程常驻 127.0.0.1:{port}，
//! course2md 逐 chunk 提交并保存 checkpoint。

use crate::checkpoint::Checkpoint;
use crate::progress::Stage;
use crate::timeline::TranscriptEvent;
use anyhow::Result;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

// ---------- 设备与启动器 ----------

/// Whether this machine has an Intel NPU. doctor, the setup wizard, the
/// default backend and the desktop app share this answer; it is computed once.
pub fn device_present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(detect_device)
}

/// Linux: an accel subsystem node bound to Intel's `intel_vpu` driver
/// (other vendors' NPUs, such as AMD XDNA, use the same subsystem).
#[cfg(target_os = "linux")]
fn detect_device() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/accel") else {
        return Path::new("/dev/accel/accel0").exists();
    };
    entries.flatten().any(|entry| {
        std::fs::read_link(entry.path().join("device/driver"))
            .ok()
            .and_then(|driver| driver.file_name().map(|name| name == "intel_vpu"))
            .unwrap_or(false)
    })
}

/// PowerShell query for the Windows NPU device. "Intel(R) AI Boost" sits in
/// the "Neural processors" (ComputeAccelerator) device class; only a device
/// whose driver is working counts.
#[cfg(any(windows, test))]
const WINDOWS_DEVICE_QUERY: &str = "Get-CimInstance -ClassName Win32_PnPEntity \
    -Filter \"Name LIKE '%AI Boost%' OR (PNPClass = 'ComputeAccelerator' AND Manufacturer LIKE '%Intel%')\" \
    | Where-Object { $_.Status -eq 'OK' } | Select-Object -ExpandProperty Name";

/// `powershell -EncodedCommand` takes base64 of UTF-16LE text, which keeps
/// the query's quotes out of Windows command-line quoting.
#[cfg(any(windows, test))]
fn powershell_encoded(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(windows)]
fn query_windows_devices() -> Result<crate::runtime::BoundedOutput> {
    let shell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let mut cmd = Command::new(shell);
    cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(powershell_encoded(WINDOWS_DEVICE_QUERY));
    crate::runtime::hide_console(&mut cmd);
    crate::runtime::run_bounded("PowerShell", &mut cmd, Duration::from_secs(20))
}

#[cfg(windows)]
fn detect_device() -> bool {
    match query_windows_devices() {
        Ok(out) if out.status.success() => !out.stdout.trim().is_empty(),
        Ok(out) => {
            tracing::debug!(stderr = %out.stderr.trim(), "npu device query failed");
            false
        }
        Err(e) => {
            tracing::debug!("npu device query: {e:#}");
            false
        }
    }
}

/// Intel NPUs only exist on Linux and Windows PCs.
#[cfg(not(any(target_os = "linux", windows)))]
fn detect_device() -> bool {
    false
}

/// The packages the worker script imports.
const WORKER_PACKAGES: [&str; 3] = ["openvino-genai", "huggingface_hub", "numpy"];
/// openvino-genai publishes wheels for these Python versions.
const WORKER_PYTHON: &str = ">=3.10";

/// What starts the NPU worker script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launcher {
    /// `uv run` installs the worker's packages into a cached environment.
    Uv(PathBuf),
    /// A Python that already imports the worker's packages.
    Python(PathBuf),
}

impl Launcher {
    fn command(&self) -> Command {
        match self {
            Self::Uv(uv) => {
                let mut cmd = Command::new(uv);
                // --no-project: a pyproject.toml in the working directory must
                // not become the worker's environment.
                cmd.args(["run", "--no-project", "--python", WORKER_PYTHON]);
                for package in WORKER_PACKAGES {
                    cmd.args(["--with", package]);
                }
                cmd.arg("python");
                cmd
            }
            Self::Python(python) => Command::new(python),
        }
    }
}

/// Find a launcher on `path` (the process PATH when `None`): uv first, then a
/// Python that imports the worker's packages. Windows ships `python.exe`
/// stubs that only open the Microsoft Store, so a Python counts only after it
/// has run the imports.
pub fn find_launcher(path: Option<&OsStr>) -> Option<Launcher> {
    let lookup = |name: &str| match path {
        Some(path) => which::which_in(name, Some(path), std::env::current_dir().ok()?).ok(),
        None => crate::runtime::which(name),
    };
    if let Some(uv) = lookup("uv") {
        return Some(Launcher::Uv(uv));
    }
    let names = if cfg!(windows) {
        ["python", "python3"]
    } else {
        ["python3", "python"]
    };
    names
        .into_iter()
        .filter_map(lookup)
        .find(|python| python_has_worker_packages(python))
        .map(Launcher::Python)
}

fn python_has_worker_packages(python: &Path) -> bool {
    let mut cmd = Command::new(python);
    cmd.args(["-c", "import openvino_genai, numpy, huggingface_hub"]);
    worker_env(&mut cmd);
    crate::runtime::hide_console(&mut cmd);
    crate::runtime::run_bounded("Python", &mut cmd, Duration::from_secs(60))
        .is_ok_and(|out| out.status.success())
}

/// Python writes its pipes in the ANSI code page on Windows (GBK, cp1252):
/// the worker's messages must arrive as UTF-8 whatever the system locale.
fn worker_env(cmd: &mut Command) {
    cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
}

// ---------- 超参数 ----------

/// NPU worker 启动期间允许的最长静默（首次模型编译可能需要 1-2 分钟）。
/// 只要还在输出（uv 安装依赖、模型下载进度）就继续等：首次使用要下载约 1GB。
const NPU_READY_TIMEOUT: Duration = Duration::from_secs(300);
/// 启动总时长上限（慢速网络下的首次下载）
const NPU_READY_LIMIT: Duration = Duration::from_secs(3 * 60 * 60);
/// NPU 单 chunk 转写请求超时
const NPU_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// 优雅关闭通知的请求超时（worker 可能正忙，别卡死主流程）
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);
/// POST /shutdown 后等待 worker 自行退出的上限
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// SIGTERM 后再给的短等待（仍未退由 ManagedChild Drop 的 kill 兜底）
const SIGTERM_WAIT: Duration = Duration::from_millis(500);

const NPU_WORKER_SCRIPT: &str = r#"
import http.server
import json
import sys
import os
import wave
import time

try:
    import openvino_genai as ov_genai
    import numpy as np
except ImportError as e:
    sys.stderr.write("错误 / Error: 缺少 openvino_genai 或 numpy: " + str(e) + "\n请安装 / Install: pip install openvino-genai numpy 或使用 uv / or use uv\n")
    sys.exit(1)

model_arg = sys.argv[1]
port = int(sys.argv[2])  # Rust 侧永远显式传参（free_port 动态分配）
device = sys.argv[3]
kind = sys.argv[4]
# 会话令牌：本机任意进程不应能中断转写或占用推理（127.0.0.1 不等于可信）
token = sys.argv[5]

model_path = model_arg
if not os.path.isdir(model_path):
    try:
        from huggingface_hub import snapshot_download
        sys.stderr.write("[NPU] 正在下载/加载 ASR 模型 " + model_arg + "...\n")
        sys.stderr.flush()
        model_path = snapshot_download(model_arg)
    except Exception as e:
        # 不静默更换模型：请求什么模型就报什么错（换模型须用户显式 --asr-model）
        sys.stderr.write("错误 / Error: 模型下载失败 / model download failed " + model_arg + ": " + str(e) + "\n（可显式指定 --asr-model whisper 使用 Whisper / pass --asr-model whisper for Whisper）\n")
        sys.exit(1)

# OpenVINO 看不到 NPU 时（驱动缺失或过旧）编译报错晦涩，先给出可操作的说明
try:
    import openvino as ov
    available = ov.Core().available_devices
except Exception as e:
    available = None
    sys.stderr.write("[NPU] 无法列出 OpenVINO 设备 / could not list OpenVINO devices: " + str(e) + "\n")
if available is not None and device not in available:
    if sys.platform == "win32":
        driver = "https://www.intel.com/content/www/us/en/download/794734/intel-npu-driver-windows.html"
    else:
        driver = "https://github.com/intel/linux-npu-driver/releases"
    sys.stderr.write("错误 / Error: OpenVINO 未发现 " + device + "（可用 / available: " + ", ".join(available) + "）。请安装或更新 Intel NPU 驱动 / Install or update the Intel NPU driver: " + driver + "\n")
    sys.exit(1)

sys.stderr.write("[NPU] 正在将模型加载/编译至 " + device + "（首次编译可能需要 1~2 分钟）...\n")
sys.stderr.flush()
t0 = time.time()

if kind != "whisper":
    # 非 Whisper 仓库只能交给 ASRPipeline；不可用或加载失败直接报错，不回退到 Whisper
    # （静默换模型会让转写来源不可追溯）
    if not hasattr(ov_genai, "ASRPipeline"):
        sys.stderr.write("错误 / Error: 此 openvino-genai 版本只能加载 Whisper 模型 / this openvino-genai only loads Whisper models: " + model_arg + "\n")
        sys.exit(1)
    try:
        pipe = ov_genai.ASRPipeline(model_path, device)
        gen_cfg = ov_genai.ASRGenerationConfig()
    except Exception as e_asr:
        sys.stderr.write("错误 / Error: 模型加载失败 / model load failed " + model_arg + ": " + str(e_asr) + "\n（Intel NPU 支持 OpenVINO 导出的 Whisper 模型，可指定 --asr-model whisper / Intel NPU runs OpenVINO Whisper exports; pass --asr-model whisper）\n")
        sys.exit(1)
else:
    try:
        pipe = ov_genai.WhisperPipeline(model_path, device)
    except Exception as e_whisper:
        sys.stderr.write("错误 / Error: 模型加载失败 / model load failed " + model_arg + ": " + str(e_whisper) + "\n")
        sys.exit(1)
    gen_cfg_path = os.path.join(model_path, "generation_config.json")
    if os.path.isfile(gen_cfg_path):
        gen_cfg = ov_genai.WhisperGenerationConfig(gen_cfg_path)
    else:
        gen_cfg = ov_genai.WhisperGenerationConfig()
    # 语言由模型自动检测，不强制中文（硬编码 <|zh|> 会把英文课转成中文幻觉输出）

sys.stderr.write("[NPU] 模型在 " + device + " 就绪（耗时 " + f"{time.time()-t0:.2f}" + "s）\n")
sys.stderr.flush()

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if token and self.headers.get("X-Auth-Token") != token:
            self.send_response(403)
            self.end_headers()
            return
        if self.path == "/shutdown":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"status":"bye"}')
            import threading
            threading.Thread(target=lambda: (time.sleep(0.05), os._exit(0))).start()
            return

        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n)
        try:
            req = json.loads(body)
            # Rust 侧只走 path 分支（chunk 已切好落盘），不再支持 base64 内嵌
            wav_path = req.get("path")
            with wave.open(wav_path, "rb") as wf:
                frames = wf.readframes(wf.getnframes())
                samples = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

            res = pipe.generate(samples.tolist(), gen_cfg)
            text = res.texts[0].strip() if res.texts else ""
            resp = json.dumps({"text": text}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(resp)
        except Exception as e:
            err_resp = json.dumps({"error": str(e)}).encode("utf-8")
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(err_resp)

server = http.server.HTTPServer(("127.0.0.1", port), Handler)
sys.stderr.write("[NPU] 监听 http://127.0.0.1:" + str(port) + "\n")
sys.stderr.flush()
server.serve_forever()
"#;

/// 规范模型 id → OpenVINO 仓库；自定义仓库 ID 原样返回。
fn npu_repo(model: &str) -> &str {
    match model {
        "whisper" => "OpenVINO/whisper-large-v3-turbo-int8-ov",
        "whisper-tiny" => "OpenVINO/whisper-tiny-fp16-ov",
        "whisper-base" => "OpenVINO/whisper-base-fp16-ov",
        "whisper-small" => "OpenVINO/whisper-small-fp16-ov",
        other => other,
    }
}

/// NPU worker 实际加载的仓库（别名经 [`crate::config::canonical_model`] 规范化）。
/// 无法识别的名称原样透传，交给 validate_asr 报错。
pub fn resolve_npu_model(raw: Option<&str>) -> String {
    let raw = raw.unwrap_or("").trim();
    match crate::config::canonical_model(crate::config::AsrProvider::Npu, raw) {
        Ok(model) => npu_repo(&model).to_owned(),
        Err(_) => raw.to_owned(),
    }
}

/// `model_id` 由调用方经 [`resolve_npu_model`] 解析后传入（checkpoint 身份与
/// worker 加载必须是同一个模型，避免解析两次）。
pub fn run_npu(
    model_id: &str,
    wav: &Path,
    max_speech: f64,
    cp: &mut Checkpoint,
    control: &crate::dispatch::TaskControl,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let segs = crate::asr::ffmpeg_vad(wav, max_speech as f32)?;
    tracing::info!(segs = segs.len(), "npu vad");
    if segs.is_empty() {
        return Ok(crate::asr::no_speech_events());
    }

    let mut worker = NpuWorker::start(model_id, control)?;
    let (base, token) = (worker.base.clone(), worker.token.clone());
    let client = ureq::AgentBuilder::new().timeout(NPU_HTTP_TIMEOUT).build();

    let r = crate::asr::run_chunks(
        wav,
        &segs,
        cp,
        worker.tmp.path(),
        "npu asr",
        control,
        |chunk| {
            let req_body = serde_json::json!({
                "path": chunk.to_string_lossy(),
            });
            let resp = client
                .post(&format!("{base}/audio/transcriptions"))
                .set("X-Auth-Token", &token)
                .send_json(req_body)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Intel NPU 识别进程无响应 / Intel NPU worker did not respond: {e}"
                    )
                })?;
            let v: serde_json::Value = resp.into_json().map_err(|e| {
                anyhow::anyhow!("NPU 无法解析服务响应 / Could not parse the service response: {e}")
            })?;
            if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                anyhow::bail!("NPU 识别失败 / NPU transcription failed: {e}");
            }
            let text = v["text"].as_str().unwrap_or("").trim().to_string();
            let sanitized = crate::asr::sanitize_qwen_text(&text);
            Ok((!sanitized.is_empty()).then_some(sanitized))
        },
    );

    worker.stop();

    let events = r.map_err(|e| {
        worker.stderr.attach(
            e,
            crate::tr!("NPU 识别错误详情 / NPU transcription error details"),
        )
    })?;
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "npu asr done"
    );
    Ok(events)
}

/// 运行中的 NPU 伴随进程；Drop 时由 ManagedChild 兜底终止。
struct NpuWorker {
    tmp: crate::runtime::TempWorkDir,
    child: crate::runtime::ManagedChild,
    stderr: crate::runtime::StderrTail,
    base: String,
    token: String,
}

fn npu_kind(model_id: &str) -> &'static str {
    if model_id.to_ascii_lowercase().contains("whisper") {
        "whisper"
    } else {
        "qwen"
    }
}

impl NpuWorker {
    fn start(model_id: &str, control: &crate::dispatch::TaskControl) -> Result<Self> {
        let t0 = Instant::now();
        let tmp = crate::runtime::TempWorkDir::new("npu")?;
        let script_path = tmp.path().join("npu_worker.py");
        crate::checkpoint::atomic_write(&script_path, NPU_WORKER_SCRIPT.as_bytes())?;

        let port = crate::runtime::free_port()?;
        // 每次会话一个随机令牌：worker 的 127.0.0.1 服务不应被本机其他进程驱使
        let token = crate::random_b64url(32)?;
        tracing::info!(model = %model_id, port, "starting npu worker");
        crate::progress::start(Stage::ModelLoad);
        let mut child = spawn_npu_worker(&script_path, model_id, port, npu_kind(model_id), &token)?;
        let stderr = child
            .take_stderr()
            .map(|s| crate::runtime::drain_stderr(s, "npu_worker"))
            .unwrap_or_default();
        let base = format!("http://127.0.0.1:{port}");

        let activity = std::cell::Cell::new((stderr.seen(), Instant::now()));
        let check = || -> Result<()> {
            control.check()?;
            let seen = stderr.seen();
            let (before, since) = activity.get();
            if seen != before {
                activity.set((seen, Instant::now()));
            } else if since.elapsed() > NPU_READY_TIMEOUT {
                anyhow::bail!(
                    "Intel NPU 服务 {:.0} 秒没有任何进展 / The Intel NPU service made no progress for {:.0}s",
                    NPU_READY_TIMEOUT.as_secs_f64(),
                    NPU_READY_TIMEOUT.as_secs_f64()
                );
            }
            Ok(())
        };
        if let Err(e) =
            crate::runtime::wait_ready(&base, NPU_READY_LIMIT, &mut child, Some("ok"), Some(&check))
        {
            crate::runtime::terminate_tree(&mut child);
            return Err(e.context(format!(
                "Intel NPU 服务启动失败/超时（首次模型编译可能需要更多时间） / Intel NPU service failed to start or timed out (the first model compilation may take longer); stderr tail:\n{}",
                stderr.tail()
            )));
        }
        tracing::info!(
            secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
            "npu ready"
        );
        crate::progress::done(Stage::ModelLoad);
        Ok(Self {
            tmp,
            child,
            stderr,
            base,
            token,
        })
    }

    /// 优雅关闭：POST /shutdown → 等待 → 终止进程树（避免 uv 衍生的孙子进程残留）
    /// → 短等 → 仍未退由 ManagedChild Drop 兜底。
    fn stop(&mut self) {
        let client = ureq::AgentBuilder::new().timeout(SHUTDOWN_TIMEOUT).build();
        let _ = client
            .post(&format!("{}/shutdown", self.base))
            .set("X-Auth-Token", &self.token)
            .send_json(serde_json::json!({}));
        if self.child.wait_within(SHUTDOWN_WAIT).is_err() {
            crate::runtime::terminate_tree(&mut self.child);
            let _ = self.child.wait_within(SIGTERM_WAIT);
        }
    }
}

impl Drop for NpuWorker {
    /// An error or cancellation between start and stop still ends the
    /// launcher's Python grandchild, which holds the port and the chunk files.
    fn drop(&mut self) {
        crate::runtime::terminate_tree(&mut self.child);
    }
}

/// Prepare the exact NPU model through the normal worker; no media is sent.
pub fn prepare_npu_model(model_id: &str, control: &crate::dispatch::TaskControl) -> Result<()> {
    NpuWorker::start(model_id, control)?.stop();
    Ok(())
}

fn spawn_npu_worker(
    script: &Path,
    model: &str,
    port: u16,
    kind: &str,
    token: &str,
) -> Result<crate::runtime::ManagedChild> {
    // 优先使用 uv（自动处理隔离环境与依赖），若无则回退已装好依赖的 Python
    let Some(launcher) = find_launcher(None) else {
        anyhow::bail!(crate::tr!(
            "未找到 uv，也没有装好 openvino-genai 的 Python，无法启动 NPU 识别 / Intel NPU transcription needs uv, or Python with openvino-genai, numpy and huggingface_hub installed"
        ));
    };
    let mut cmd = launcher.command();
    worker_env(&mut cmd);
    crate::runtime::hide_console(&mut cmd);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // huggingface_hub downloads repository ids; a local directory needs no host.
    if !Path::new(model).is_dir()
        && std::env::var_os("HF_ENDPOINT").is_none()
        && let Some(mirror) = crate::hub::endpoints().hub_override()
    {
        cmd.env("HF_ENDPOINT", mirror);
    }
    cmd.arg(script)
        .arg(model)
        .arg(port.to_string())
        .arg("NPU")
        .arg(kind)
        .arg(token)
        .stdout(Stdio::null())
        // stderr 不能 inherit：脚本的输出会插进进度条重绘中间。piped + 后台 drain。
        .stderr(Stdio::piped());

    crate::runtime::ManagedChild::spawn("NPU worker", &mut cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌脚本必须能被 Python 解析。0.8.x 曾因 f-string 内嵌字面换行导致
    /// 整个 NPU 后端无法启动（SyntaxError 在编译期拦截，任何路径都跑不到）。
    /// 无 python3 的环境下跳过。
    #[test]
    fn worker_script_is_valid_python() {
        let Some(py) = crate::runtime::which("python3") else {
            eprintln!("skip: python3 not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("c2m-npu-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("npu_worker.py");
        std::fs::write(&script, NPU_WORKER_SCRIPT).unwrap();
        let out = std::process::Command::new(py)
            .arg("-m")
            .arg("py_compile")
            .arg(&script)
            .output()
            .expect("spawn python3");
        assert!(
            out.status.success(),
            "NPU worker 脚本存在语法错误：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uv_launcher_ignores_projects_and_requests_a_supported_python() {
        let cmd = Launcher::Uv(PathBuf::from("uv")).command();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(&args[..4], ["run", "--no-project", "--python", ">=3.10"]);
        assert_eq!(args.last(), Some(&"python"));
        for package in WORKER_PACKAGES {
            assert!(
                args.windows(2).any(|w| w == ["--with", package]),
                "{package}"
            );
        }
    }

    #[test]
    fn device_query_is_encoded_as_utf16_base64() {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(powershell_encoded(WINDOWS_DEVICE_QUERY))
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), WINDOWS_DEVICE_QUERY);
        assert!(WINDOWS_DEVICE_QUERY.contains("'%AI Boost%'"));
    }

    /// The query must run on stock Windows; the runner has no NPU, so only
    /// the exit status is checked.
    #[cfg(windows)]
    #[test]
    fn windows_device_query_runs() {
        let out = query_windows_devices().unwrap();
        assert!(out.status.success(), "{}", out.stderr);
    }

    fn fake_program(dir: &Path, name: &str, script: &str) {
        let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn launcher_prefers_uv_on_the_given_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_launcher(Some(dir.path().as_os_str())), None);
        fake_program(dir.path(), "uv", "#!/bin/sh\nexit 0\n");
        let Some(Launcher::Uv(uv)) = find_launcher(Some(dir.path().as_os_str())) else {
            panic!("uv not found");
        };
        assert_eq!(uv.parent(), Some(dir.path()));
    }

    /// A Python that cannot import the worker's packages (like the Windows
    /// Store stub, which exits 9009) is not a launcher.
    #[cfg(unix)]
    #[test]
    fn python_without_worker_packages_is_not_a_launcher() {
        let dir = tempfile::tempdir().unwrap();
        fake_program(dir.path(), "python3", "#!/bin/sh\nexit 9009\n");
        assert_eq!(find_launcher(Some(dir.path().as_os_str())), None);
        fake_program(dir.path(), "python", "#!/bin/sh\nexit 0\n");
        let Some(Launcher::Python(python)) = find_launcher(Some(dir.path().as_os_str())) else {
            panic!("python not found");
        };
        assert_eq!(python.file_name().unwrap(), "python");
    }

    #[test]
    fn npu_model_aliases() {
        assert_eq!(
            resolve_npu_model(Some("whisper")),
            "OpenVINO/whisper-large-v3-turbo-int8-ov"
        );
        assert_eq!(
            resolve_npu_model(None),
            "OpenVINO/whisper-large-v3-turbo-int8-ov"
        );
        assert_eq!(
            resolve_npu_model(Some("org/custom-model")),
            "org/custom-model"
        );
        // 别名大小写不敏感；自定义仓库名原样透传，大小写保持不变
        assert_eq!(
            resolve_npu_model(Some("WHISPER-LARGE")),
            resolve_npu_model(Some("whisper"))
        );
        assert_eq!(
            resolve_npu_model(Some("MyOrg/MyModel-INT8")),
            "MyOrg/MyModel-INT8"
        );
    }
}
