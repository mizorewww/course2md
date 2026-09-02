#!/usr/bin/env bash
set -euo pipefail

# 安装 course2md 预编译二进制到 ~/bin（或 COURSE2MD_BIN_DIR）。
# 本脚本只装本体；外部依赖（ffmpeg/yt-dlp/llama-server/uv）由安装后的
# `course2md setup` 自动下载到私有目录，无需手动安装。
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash

REPO="${COURSE2MD_REPO:-mizorewww/course2md}"
BIN_DIR="${COURSE2MD_BIN_DIR:-$HOME/bin}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os-$arch" in
  darwin-arm64|darwin-aarch64) ASSET="course2md-macos-arm64" ;;
  darwin-x86_64) ASSET="course2md-macos-x86_64" ;;
  linux-x86_64|linux-amd64) ASSET="course2md-linux-x86_64" ;;
  linux-aarch64|linux-arm64) ASSET="course2md-linux-aarch64" ;;
  *)
    echo "暂无预编译包：$os $arch。请用 cargo install --path . 从源码安装。" >&2
    exit 1
    ;;
esac

mkdir -p "$BIN_DIR"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

if command -v gh >/dev/null 2>&1; then
  gh release download -R "$REPO" -p "$ASSET" -O "$tmp" --clobber
else
  url="$(
    COURSE2MD_REPO="$REPO" ASSET="$ASSET" python3 - <<'PY'
import json, os, urllib.request
repo = os.environ["COURSE2MD_REPO"]
asset = os.environ["ASSET"]
with urllib.request.urlopen(f"https://api.github.com/repos/{repo}/releases/latest") as r:
    data = json.load(r)
for a in data.get("assets", []):
    if a["name"] == asset:
        print(a["browser_download_url"])
        break
else:
    raise SystemExit(f"release 中找不到 {asset}")
PY
  )"
  curl -fsSL "$url" -o "$tmp"
fi

install -m 755 "$tmp" "$BIN_DIR/course2md"
echo "已安装：$BIN_DIR/course2md"

# MLX Metal kernels：CoreML 推理需要与二进制同目录（仅 macOS arm64）
if [ "$os-$arch" = "darwin-arm64" ] || [ "$os-$arch" = "darwin-aarch64" ]; then
  mlb="$(mktemp)"
  trap 'rm -f "$tmp" "$mlb"' EXIT
  if command -v gh >/dev/null 2>&1; then
    if gh release download -R "$REPO" -p "mlx-macos-arm64.metallib" -O "$mlb" --clobber 2>/dev/null; then
      install -m 644 "$mlb" "$BIN_DIR/mlx.metallib"
      echo "已安装：$BIN_DIR/mlx.metallib（CoreML 推理所需）"
    fi
  else
    url="$(
      COURSE2MD_REPO="$REPO" python3 - <<'PY'
import json, os, urllib.request
repo = os.environ["COURSE2MD_REPO"]
with urllib.request.urlopen(f"https://api.github.com/repos/{repo}/releases/latest") as r:
    data = json.load(r)
for a in data.get("assets", []):
    if a["name"] == "mlx-macos-arm64.metallib":
        print(a["browser_download_url"])
        break
PY
    )"
    if [ -n "$url" ] && curl -fsSL "$url" -o "$mlb" 2>/dev/null; then
      install -m 644 "$mlb" "$BIN_DIR/mlx.metallib"
      echo "已安装：$BIN_DIR/mlx.metallib（CoreML 推理所需）"
    fi
  fi
fi

echo "请确保 PATH 包含 $BIN_DIR，例如：export PATH=\"\$HOME/bin:\$PATH\""
echo "下一步：$BIN_DIR/course2md setup  # 体检并自动安装缺失的外部工具（ffmpeg/yt-dlp/llama-server）"
echo "首次运行会自动下载识别模型（macOS CoreML 约 1-2GB；其他平台 llama.cpp GGUF 约 2.4GB）。"
