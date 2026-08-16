#!/bin/bash
# 打包 qview.app（原生 AppKit 前端）。
#
# 用法：
#   ./frontends/gui-macos/scripts/package.sh            # 构建 release 并打包到 target/qview.app
#   ./frontends/gui-macos/scripts/package.sh --no-build # 只打包，不重新构建
#
# 产物：<workspace>/target/qview.app（含 ad-hoc 签名，本地可运行）。

set -euo pipefail

# 定位工作区根（脚本位于 <root>/frontends/gui-macos/scripts/）
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
CRATE="$ROOT/frontends/gui-macos"
TARGET="$ROOT/target"

BUILD=1
if [ "${1:-}" = "--no-build" ]; then
    BUILD=0
fi

if [ "$BUILD" = "1" ]; then
    echo "==> cargo build --release -p qview-gui-macos"
    (cd "$ROOT" && cargo build --release -p qview-gui-macos)
fi

APP="$TARGET/qview.app"
EXE="$TARGET/release/qview-gui-macos"

if [ ! -x "$EXE" ]; then
    echo "错误：找不到 release 二进制 $EXE（先运行 cargo build --release -p qview-gui-macos）" >&2
    exit 1
fi

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$EXE" "$APP/Contents/MacOS/qview-gui-macos"
cp "$CRATE/Info.plist" "$APP/Contents/Info.plist"

if [ -f "$CRATE/icon.icns" ]; then
    cp "$CRATE/icon.icns" "$APP/Contents/Resources/icon.icns"
fi

# 分发 GPL-3.0 许可文本。
if [ -f "$ROOT/LICENSE" ]; then
    cp "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
fi

# 签名（ad-hoc）：arm64/通用二进制在本机运行需要有效签名；未签名会被 Gatekeeper 拦截。
echo "==> ad-hoc codesign"
codesign --force --sign - "$APP" 2>/dev/null || {
    echo "警告：codesign 失败，跳过签名（可能仍可运行）" >&2
}

echo "==> 完成"
echo "    $APP"
echo "    运行：open \"$APP\"  或  $APP/Contents/MacOS/qview-gui-macos [文件]"
