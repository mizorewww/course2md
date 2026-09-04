#!/usr/bin/env bash
set -euo pipefail

# 构建 course2md 桌面客户端（Tauri v2）。
#
# 用法：
#   packaging/build-app.sh
#
# 流程：
#   1. 仓库根构建 release CLI（cargo build --release）
#   2. 按 host triple 把 target/release/course2md 拷到 app/src-tauri/binaries/course2md-<triple>
#      （Tauri sidecar 命名约定；tauri.conf.json 的 bundle.externalBin 引用它）
#   3. macOS 上把 MLX Metal kernels 拷成 app/src-tauri/binaries/mlx.metallib
#      （bundle.macOS.files 会把它打成 Contents/Resources/default.metallib——
#       codesign 把 Contents/MacOS/ 下文件都当代码，数据文件只能放 Resources/；
#       GUI spawn sidecar 时把 CWD 设为该目录，命中 MLX 的 CWD/default.metallib
#       兜底搜索路径；文件缺失 tauri build 会报错，找不到真实产物时写占位并警告）
#   4. cd app && pnpm install && pnpm tauri build
#
# macOS 签名与公证（可选）：设置以下环境变量后，tauri bundler 会自动
# Developer ID 签名 + notarytool 公证 + staple：
#   APPLE_SIGNING_IDENTITY="Developer ID Application: Name (TEAMID)"
#   APPLE_API_KEY_PATH=/path/AuthKey_XXXX.p8
#   APPLE_API_KEY=H68A75YKU9        # App Store Connect Key ID
#   APPLE_API_ISSUER=xxxxxxxx-...   # Issuer ID
# 未设置时按 ad-hoc 签名（本机可用，分发会被 Gatekeeper 拦）。

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

echo "==> 构建 release CLI"
cargo build --release

triple="$(rustc -vV | awk '/^host:/ {print $2}')"
bin_src="target/release/course2md"
bin_dir="app/src-tauri/binaries"
[ -f "$bin_src" ] || { echo "缺少 $bin_src" >&2; exit 1; }
mkdir -p "$bin_dir"
install -m 755 "$bin_src" "$bin_dir/course2md-$triple"
echo "==> sidecar: $bin_dir/course2md-$triple"

os="$(uname -s)"
if [ "$os" = "Darwin" ]; then
  mlb=""
  for cand in \
    "native/apple-asr/.build/out/Products/Release/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib" \
    "target/release/mlx.metallib" \
    ; do
    if [ -f "$cand" ]; then mlb="$cand"; break; fi
  done
  if [ -n "$mlb" ]; then
    install -m 644 "$mlb" "$bin_dir/mlx.metallib"
    echo "==> mlx.metallib: $mlb → $bin_dir/mlx.metallib"
  else
    echo "警告：未找到 MLX metallib（先构建 native/apple-asr）。CoreML 推理将不可用。" >&2
    # bundle.macOS.files 引用此文件，缺失会让 tauri build 失败 → 写占位
    [ -f "$bin_dir/mlx.metallib" ] || : > "$bin_dir/mlx.metallib"
  fi
fi

cd "$ROOT/app"
echo "==> pnpm install"
pnpm install
echo "==> pnpm tauri build"
pnpm tauri build
echo "==> 完成，产物在 app/src-tauri/target/release/bundle/"
