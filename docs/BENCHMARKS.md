# Benchmarks

Measured 2026-09-01 on a 3-minute 1080p Chinese lecture clip (local file), release builds. The current default model, `qwen3-1.7b` (MLX), is not in these tables. Re-run `packaging/bench-mac.sh <video>`, which includes it, before quoting numbers for the default.

## macOS, Apple Silicon

`packaging/bench-mac.sh` records wall time, peak memory and average power (`powermetrics` at 1 Hz; idle baseline about 1.7 W CPU and 0.26 W GPU).

| Backend | Wall | ASR only | CPU | GPU | ANE | Peak memory |
|---|---:|---:|---:|---:|---:|---|
| `coreml` + qwen3-0.6b (ANE) | 47 s | 46.4 s | 6.7 W | 0.2 W | 3.5 W | 1.41 GB |
| `coreml` + whisper large-v3-turbo | 87 s | 85.3 s | 15.3 W | 0.3 W | 0.4 W | 1.51 GB |
| `gpu` (llama.cpp Metal, Qwen3-ASR 1.7B Q8) | 13 s | 11.2 s | 4.7 W | 16.0 W | — | 26 MB + 3.3 GB child |
| `cpu` (llama.cpp, same model) | 26 s | 25.6 s | 21.2 W | 0.6 W | — | 26 MB + 4.8 GB child |

Upstream figures put `qwen3-1.7b` at roughly 2× the accuracy and 3× the speed of 0.6B (WER 1.52% vs 3.02%, RTF 0.033 vs 0.098) with 2.7 GB RSS. For the lowest power draw on battery, use `--asr-model qwen3-0.6b`. On this sample, Whisper was slower and less accurate than Qwen3.

## Linux, Intel Core Ultra NPU

Intel Core Ultra X7 358H, CachyOS, OpenVINO GenAI NPU runtime.

| Backend | Wall | ASR only | Speed | Peak memory |
|---|---:|---:|---:|---|
| `npu` Whisper large-v3-turbo INT8 | 16 s | 15.1 s | ~12× real time | 18 MB + 557 MB child |
| `npu` Whisper tiny FP16 | 6 s | 4.6 s | ~39× real time | 18 MB + 180 MB child |
| `cpu` llama.cpp Qwen3-ASR 1.7B Q8 | 100 s | 98.4 s | ~1.8× real time | 18 MB + 3.5 GB child |
