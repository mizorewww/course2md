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
