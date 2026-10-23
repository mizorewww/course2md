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

use std::path::Path;

/// Intel NPU 设备节点（Linux accel 子系统）。doctor / wizard / 默认后端提示共用。
pub const NPU_DEVICE_PATH: &str = "/dev/accel/accel0";
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------- 超参数 ----------

/// NPU worker /health 就绪等待上限（首次模型编译可能需要 1-2 分钟）
const NPU_READY_TIMEOUT: Duration = Duration::from_secs(300);
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

sys.stderr.write("[NPU] 正在将模型加载/编译至 " + device + "（首次编译可能需要 1~2 分钟）...\n")
sys.stderr.flush()
t0 = time.time()

if kind == "qwen" and hasattr(ov_genai, "ASRPipeline"):
    try:
        pipe = ov_genai.ASRPipeline(model_path, device)
        gen_cfg = getattr(ov_genai, "ASRGenerationConfig", lambda: None)()
    except Exception as e_qwen:
        # 加载失败直接报错退出，不回退到 Whisper（静默换模型会让转写来源不可追溯）
        sys.stderr.write("错误 / Error: Qwen3 ASR 加载失败 / load failed: " + str(e_qwen) + "\n（如需 Whisper 请显式指定 --asr-model whisper / pass --asr-model whisper for Whisper）\n")
        sys.exit(1)
else:
    pipe = ov_genai.WhisperPipeline(model_path, device)
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
        "qwen3-0.6b" => "dseditor/Qwen3-ASR-0.6B-INT8_ASYM-OpenVINO",
        "qwen3-1.7b" => "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO",
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
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let segs = crate::asr::ffmpeg_vad(wav, max_speech as f32)?;
    tracing::info!(segs = segs.len(), "npu vad");
    if segs.is_empty() {
        return Ok(crate::asr::no_speech_events());
    }

    let mut worker = NpuWorker::start(model_id)?;
    let (base, token) = (worker.base.clone(), worker.token.clone());
    let client = ureq::AgentBuilder::new().timeout(NPU_HTTP_TIMEOUT).build();

    let r = crate::asr::run_chunks(wav, &segs, cp, worker.tmp.path(), "npu asr", |chunk| {
        let req_body = serde_json::json!({
            "path": chunk.to_string_lossy(),
        });
        let resp = client
            .post(&format!("{base}/audio/transcriptions"))
            .set("X-Auth-Token", &token)
            .send_json(req_body)
            .map_err(|e| {
                anyhow::anyhow!("Intel NPU 识别进程无响应 / Intel NPU worker did not respond: {e}")
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
    });

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
    fn start(model_id: &str) -> Result<Self> {
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

        if let Err(e) = crate::runtime::wait_ready(
            &base,
            NPU_READY_TIMEOUT,
            &mut child,
            Some("ok"),
            Some(&crate::dispatch::check_control),
        ) {
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

    /// 优雅关闭：POST /shutdown → 等待 → SIGTERM 进程组（避免 uv 衍生的孙子进程残留）
    /// → 短等 → 仍未退由 ManagedChild Drop 兜底。
    fn stop(&mut self) {
        let client = ureq::AgentBuilder::new().timeout(SHUTDOWN_TIMEOUT).build();
        let _ = client
            .post(&format!("{}/shutdown", self.base))
            .set("X-Auth-Token", &self.token)
            .send_json(serde_json::json!({}));
        if self.child.wait_within(SHUTDOWN_WAIT).is_err() {
            #[cfg(unix)]
            {
                let pid = self.child.id() as i32;
                unsafe {
                    libc::kill(-pid, libc::SIGTERM);
                }
            }
            let _ = self.child.wait_within(SIGTERM_WAIT);
        }
    }
}

/// Prepare the exact NPU model through the normal worker; no media is sent.
pub fn prepare_npu_model(model_id: &str) -> Result<()> {
    NpuWorker::start(model_id)?.stop();
    Ok(())
}

fn spawn_npu_worker(
    script: &Path,
    model: &str,
    port: u16,
    kind: &str,
    token: &str,
) -> Result<crate::runtime::ManagedChild> {
    // 优先使用 uv（自动处理隔离环境与依赖），若无则回退系统 python3
    let mut cmd = if crate::runtime::which("uv").is_some() {
        let mut c = Command::new("uv");
        c.args([
            "run",
            "--with",
            "openvino-genai",
            "--with",
            "huggingface_hub",
            "--with",
            "numpy",
            "python",
        ]);
        c
    } else if let Some(python) = ["python3", "python"]
        .into_iter()
        .find(|python| crate::runtime::which(python).is_some())
    {
        Command::new(python)
    } else {
        anyhow::bail!(crate::tr!(
            "未找到 Python/uv，无法启动 NPU 识别 / Install Python or uv to use Intel NPU transcription"
        ));
    };

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
    fn npu_model_aliases() {
        assert_eq!(
            resolve_npu_model(Some("whisper")),
            "OpenVINO/whisper-large-v3-turbo-int8-ov"
        );
        assert_eq!(
            resolve_npu_model(None),
            "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO"
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
