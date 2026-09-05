//! Intel NPU 后端（OpenVINO WhisperPipeline）。
//!
//! 在具备 Intel Core Ultra (AI Boost) NPU 的设备上，
//! 通过 OpenVINO GenAI 将 Whisper 模型编译并在 NPU 上高速运行。
//! 通过轻量 Python 伴随进程常驻 127.0.0.1:{port}，
//! course2md 逐 chunk 提交并保存 checkpoint。

use crate::checkpoint::Checkpoint;
use crate::timeline::TranscriptEvent;
use anyhow::Result;

use std::path::Path;
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
    sys.stderr.write("Error: 缺少 openvino_genai 或 numpy: " + str(e) + "\n请安装: pip install openvino-genai numpy 或使用 uv\n")
    sys.exit(1)

model_arg = sys.argv[1] if len(sys.argv) > 1 else "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO"
port = int(sys.argv[2])  # Rust 侧永远显式传参（free_port 动态分配），不留默认端口
device = sys.argv[3] if len(sys.argv) > 3 else "NPU"

model_path = model_arg
if not os.path.isdir(model_path):
    try:
        from huggingface_hub import snapshot_download
        print("[NPU] 正在下载/加载 ASR 模型 " + model_arg + "...", flush=True)
        model_path = snapshot_download(model_arg)
    except Exception as e:
        # 不静默更换模型：请求什么模型就报什么错（换模型须用户显式 --asr-model）
        sys.stderr.write("Error: 模型下载失败 " + model_arg + ": " + str(e) + "\n（可显式指定 --asr-model whisper 使用 Whisper）\n")
        sys.exit(1)

print("[NPU] 正在将模型加载/编译至 " + device + "（首次编译可能需要 1~2 分钟）...", flush=True)
t0 = time.time()
is_qwen = "qwen" in str(model_arg).lower() or "qwen" in str(model_path).lower()

if is_qwen and hasattr(ov_genai, "ASRPipeline"):
    try:
        pipe = ov_genai.ASRPipeline(model_path, device)
        gen_cfg = getattr(ov_genai, "ASRGenerationConfig", lambda: None)()
    except Exception as e_qwen:
        # 加载失败直接报错退出，不回退到 Whisper（静默换模型会让转写来源不可追溯）
        sys.stderr.write("Error: Qwen3 ASR 加载失败: " + str(e_qwen) + "\n（如需 Whisper 请显式指定 --asr-model whisper）\n")
        sys.exit(1)
else:
    pipe = ov_genai.WhisperPipeline(model_path, device)
    gen_cfg_path = os.path.join(model_path, "generation_config.json")
    if os.path.isfile(gen_cfg_path):
        gen_cfg = ov_genai.WhisperGenerationConfig(gen_cfg_path)
    else:
        gen_cfg = ov_genai.WhisperGenerationConfig()
    # 语言由模型自动检测，不强制中文（硬编码 <|zh|> 会把英文课转成中文幻觉输出）

print("[NPU] 模型在 " + device + " 就绪（耗时 " + f"{time.time()-t0:.2f}" + "s）", flush=True)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        if self.path in ("/health", "/v1/health"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path in ("/shutdown", "/v1/shutdown", "/exit"):
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
print("[NPU] 监听 http://127.0.0.1:" + str(port), flush=True)
server.serve_forever()
"#;

pub(crate) fn npu_model_alias(raw: &str) -> Option<&'static str> {
    Some(match raw.trim().to_ascii_lowercase().as_str() {
        "whisper" | "turbo" | "whisper-turbo" | "whisper-large" | "large" => {
            "OpenVINO/whisper-large-v3-turbo-int8-ov"
        }
        "tiny" | "whisper-tiny" => "OpenVINO/whisper-tiny-fp16-ov",
        "base" | "whisper-base" => "OpenVINO/whisper-base-fp16-ov",
        "small" | "whisper-small" => "OpenVINO/whisper-small-fp16-ov",
        "qwen3-0.6b" | "0.6b" => "dseditor/Qwen3-ASR-0.6B-INT8_ASYM-OpenVINO",
        "qwen3" | "qwen3-1.7b" | "1.7b" | "" => "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO",
        _ => return None,
    })
}

pub fn resolve_npu_model(raw: Option<&str>) -> String {
    let raw = raw.unwrap_or("").trim();
    npu_model_alias(raw).unwrap_or(raw).to_string()
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
        tracing::warn!(
            "未检测到语音，将仅保留截图 / No speech detected; keeping slides without a transcript"
        );
        return Ok(vec![]);
    }

    // chunk 与 worker 脚本都放在临时目录（此前脚本写进用户的 out_dir/.workers/，
    // 污染输出目录）；原子写避免崩溃留下半截脚本
    let tmp = crate::runtime::TempWorkDir::new("npu")?;
    let script_path = tmp.path().join("npu_worker.py");
    crate::checkpoint::atomic_write(&script_path, NPU_WORKER_SCRIPT.as_bytes())?;

    let port = crate::runtime::free_port()?;
    tracing::info!(model = %model_id, port, "starting npu worker");
    crate::progress::stage("model-load", "start");
    let mut child = spawn_npu_worker(&script_path, model_id, port)?;
    let stderr_tail = child
        .take_stderr()
        .map(|s| crate::runtime::drain_stderr(s, "npu_worker"))
        .unwrap_or_default();
    let base = format!("http://127.0.0.1:{port}");

    // ManagedChild：此后任何 ? 早退都会在 Drop 中终止 worker，不再泄漏进程
    if let Err(e) = crate::runtime::wait_ready(
        &base,
        NPU_READY_TIMEOUT,
        &mut child,
        Some("\"status\":\"ok\""),
    ) {
        return Err(e.context(format!(
            "Intel NPU 服务启动失败/超时（首次模型编译可能需要更多时间），其 stderr 尾部：\n{}",
            stderr_tail.tail()
        )));
    }
    tracing::info!(
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "npu ready"
    );

    crate::progress::stage("model-load", "done");
    let client = ureq::AgentBuilder::new().timeout(NPU_HTTP_TIMEOUT).build();

    let r = crate::asr::run_chunks(wav, &segs, cp, tmp.path(), "npu asr", |_i, _seg, chunk| {
        let req_body = serde_json::json!({
            "path": chunk.to_string_lossy(),
        });
        let resp = client
            .post(&format!("{base}/audio/transcriptions"))
            .send_json(req_body)
            .map_err(|e| anyhow::anyhow!("NPU 转写请求失败，请检查网络和服务配置 / Request failed; check your connection and service settings: {e}"))?;
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

    // 优雅关闭：POST /shutdown → try_wait 轮询 ~2s → 未退再 SIGTERM 进程组
    // （避免 uv 衍生的孙子进程残留）→ 短等 → 仍未退由 ManagedChild Drop 兜底。
    let _ = client
        .post(&format!("{base}/shutdown"))
        .timeout(SHUTDOWN_TIMEOUT)
        .send_json(serde_json::json!({}));
    if !wait_exit(&mut child, SHUTDOWN_WAIT) {
        #[cfg(unix)]
        {
            let pid = child.id() as i32;
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
        }
        let _ = wait_exit(&mut child, SIGTERM_WAIT);
    }

    let events = r.map_err(|e| {
        let tail = stderr_tail.tail();
        if tail.is_empty() {
            e
        } else {
            e.context(format!(
                "NPU 识别错误详情 / NPU transcription error details:\n{tail}"
            ))
        }
    })?;
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "npu asr done"
    );
    Ok(events)
}

/// 非阻塞轮询等待子进程退出；true = 已在 timeout 内退出。
fn wait_exit(child: &mut crate::runtime::ManagedChild, timeout: Duration) -> bool {
    let t0 = Instant::now();
    loop {
        if child.try_wait().is_some() {
            return true;
        }
        if t0.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_npu_worker(script: &Path, model: &str, port: u16) -> Result<crate::runtime::ManagedChild> {
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
    } else if crate::runtime::which("python3").is_some() {
        Command::new("python3")
    } else {
        anyhow::bail!(
            "未找到 Python/uv，无法启动 NPU 识别 / Install Python or uv to use Intel NPU transcription"
        );
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.arg(script)
        .arg(model)
        .arg(port.to_string())
        .arg("NPU")
        .stdout(Stdio::null())
        // stderr 不能 inherit：脚本里全是 print，会插在进度条重绘中间，
        // 破坏 indicatif 的原地更新（同 issue #4）。piped + 后台 drain。
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
    fn custom_repository_case_is_preserved_and_aliases_are_shared() {
        assert_eq!(
            resolve_npu_model(Some("MyOrg/MyModel-INT8")),
            "MyOrg/MyModel-INT8"
        );
        assert_eq!(
            resolve_npu_model(Some("WHISPER-LARGE")),
            resolve_npu_model(Some("whisper"))
        );
        assert!(npu_model_alias("whisper-large").is_some());
    }

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
    }
}
