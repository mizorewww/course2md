#!/usr/bin/env bash
set -euo pipefail

# 安装 course2md 预编译二进制到 ~/bin（或 COURSE2MD_BIN_DIR）。
# Install the prebuilt course2md binary into ~/bin (or COURSE2MD_BIN_DIR).
#   curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash

REPO="${COURSE2MD_REPO:-mizorewww/course2md}"
BIN_DIR="${COURSE2MD_BIN_DIR:-$HOME/bin}"
RELEASE="https://github.com/${REPO}/releases/latest/download"
YTDLP_RELEASE="https://github.com/yt-dlp/yt-dlp/releases/latest/download"

say() { printf '%s\n' "$@"; }
warn() { printf '%s\n' "$@" >&2; }

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
apple=0
case "$os-$arch" in
  darwin-arm64|darwin-aarch64) ASSET="course2md-macos-arm64"; YTDLP="yt-dlp_macos"; apple=1 ;;
  darwin-x86_64) ASSET="course2md-macos-x86_64"; YTDLP="yt-dlp_macos" ;;
  linux-x86_64|linux-amd64) ASSET="course2md-linux-x86_64"; YTDLP="yt-dlp_linux" ;;
  linux-aarch64|linux-arm64) ASSET="course2md-linux-aarch64"; YTDLP="yt-dlp_linux_aarch64" ;;
  *)
    warn "暂无 $os/$arch 的预编译版本，请从源码安装：cargo install --path . / No prebuilt binary for $os/$arch; build from source with cargo install --path ."
    exit 1
    ;;
esac

if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
  warn "缺少 ffmpeg（含 ffprobe）/ ffmpeg (with ffprobe) is required:"
  warn "  macOS:         brew install ffmpeg"
  warn "  Arch/CachyOS:  sudo pacman -S ffmpeg"
  warn "  Debian/Ubuntu: sudo apt install ffmpeg"
  exit 1
fi

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Nothing is installed without a matching entry in the release's SHA256SUMS. Every
# release since v2.0.0 publishes one, and this script installs only the latest.
sums="$tmp/SHA256SUMS"
if ! curl -fsSL "$RELEASE/SHA256SUMS" -o "$sums"; then
  warn "无法下载校验和文件，已停止安装 / Could not download SHA256SUMS; nothing was installed."
  exit 1
fi
verify() {
  local file="$1" name="$2" expected
  expected="$(awk -v n="$name" '$2 == n || $2 == "*" n {print $1}' "$sums")"
  if [ -z "$expected" ]; then
    warn "校验和文件中没有 ${name}，已停止安装 / $name is not listed in SHA256SUMS; nothing was installed."
    exit 1
  fi
  if [ "$(sha256 "$file")" != "$expected" ]; then
    warn "校验和不匹配：$name / Checksum mismatch: $name"
    exit 1
  fi
}

mkdir -p "$BIN_DIR"
curl -fsSL "$RELEASE/$ASSET" -o "$tmp/course2md"
verify "$tmp/course2md" "$ASSET"
install -m 755 "$tmp/course2md" "$BIN_DIR/course2md"
say "已安装 / Installed: $BIN_DIR/course2md"

# Apple 本机语音识别需要与程序放在同一目录的 mlx.metallib。
if [ "$apple" = 1 ]; then
  if curl -fsSL "$RELEASE/mlx-macos-arm64.metallib" -o "$tmp/mlx.metallib"; then
    verify "$tmp/mlx.metallib" "mlx-macos-arm64.metallib"
    install -m 644 "$tmp/mlx.metallib" "$BIN_DIR/mlx.metallib"
  else
    warn "警告：Apple 本机语音识别组件下载失败，将无法在本机识别语音。重新运行安装命令即可重试。"
    warn "Warning: the Apple speech component failed to download; local recognition is unavailable. Re-run the installer to retry."
  fi
fi

# 依赖的许可声明放在程序旁；较早的版本没有这个文件。
# Dependency license notices sit beside the binary; older releases do not publish them.
if curl -fsSL "$RELEASE/course2md-third-party-licenses.txt" -o "$tmp/licenses.txt"; then
  verify "$tmp/licenses.txt" "course2md-third-party-licenses.txt"
  install -m 644 "$tmp/licenses.txt" "$BIN_DIR/course2md-third-party-licenses.txt"
fi

# 在线视频需要 yt-dlp：PATH 中已有就沿用，否则在程序旁安装官方独立版本。
if command -v yt-dlp >/dev/null 2>&1; then
  say "使用已安装的 yt-dlp / Using installed yt-dlp: $(command -v yt-dlp)"
elif curl -fsSL --max-time 300 "$YTDLP_RELEASE/$YTDLP" -o "$tmp/yt-dlp" &&
     curl -fsSL "$YTDLP_RELEASE/SHA2-256SUMS" -o "$tmp/yt-dlp.sums" &&
     [ "$(sha256 "$tmp/yt-dlp")" = "$(awk -v n="$YTDLP" '$2 == n {print $1}' "$tmp/yt-dlp.sums")" ]; then
  install -m 755 "$tmp/yt-dlp" "$BIN_DIR/yt-dlp"
  say "已安装 / Installed: $BIN_DIR/yt-dlp"
else
  warn "警告：yt-dlp 安装失败，在线视频需要它（本地文件不受影响）。/ Warning: yt-dlp could not be installed; online videos need it (local files still work)."
  warn "  macOS: brew install yt-dlp    其他 / other: pipx install yt-dlp"
fi

if [ "$apple" != 1 ] && ! command -v llama-server >/dev/null 2>&1; then
  warn "提示：本机 GPU/CPU 语音识别需要 llama-server；只用字幕、云端或 NPU 识别时无需安装。"
  warn "Note: local GPU/CPU recognition needs llama-server; subtitles, cloud and NPU recognition work without it."
  warn "  https://github.com/ggml-org/llama.cpp#quick-start"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "请把 $BIN_DIR 加入 PATH / Add $BIN_DIR to PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
say "运行 course2md doctor 检查环境 / Run course2md doctor to check your setup."
