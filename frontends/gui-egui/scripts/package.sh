#!/bin/bash
# 打包 qview.app（egui/glow 前端）——去掉 Finder 双击裸二进制时弹出的 Terminal「小黑框」。
#
# 用法：
#   ./frontends/gui-egui/scripts/package.sh            # 构建 release 并打包到 target/qview.app
#   ./frontends/gui-egui/scripts/package.sh --no-build # 只打包，不重新构建
#
# 产物：<workspace>/target/qview.app（含 ad-hoc 签名，本地可运行）。
#
# 为什么需要 .app：macOS 在 Finder 里双击一个**裸 Mach-O 二进制**时，会用 Terminal 打开
# 它来承载 stdout/stderr——那个黑框就是调试用的控制台。包成 .app 后 `open qview.app`
# 直接启动 GUI，不再弹 Terminal。调试时仍可用命令行 `target/release/qview-gui-egui` 直跑。
#
# 资源（中文字体 / 窗口图标 / 赞赏码）已在编译期嵌入可执行文件（src/assets.rs 的
# include_bytes!，macOS/Linux 端走 embed；Windows 端走 sidecar + build.rs 自动复制），
# 所以 .app 里**不需要**复制 assets/ —— 只有 Dock 图标 icon.icns 由 macOS 系统读取、
# 必须放在 Contents/Resources/。

set -euo pipefail

# 定位工作区根（脚本位于 <root>/frontends/gui-egui/scripts/）
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
CRATE="$ROOT/frontends/gui-egui"
TARGET="$ROOT/target"

BUILD=1
if [ "${1:-}" = "--no-build" ]; then
    BUILD=0
fi

if [ "$BUILD" = "1" ]; then
    echo "==> cargo build --release -p qview-gui-egui"
    (cd "$ROOT" && cargo build --release -p qview-gui-egui)
fi

APP="$TARGET/qview.app"
EXE="$TARGET/release/qview-gui-egui"

if [ ! -x "$EXE" ]; then
    echo "错误：找不到 release 二进制 $EXE（先运行 cargo build --release -p qview-gui-egui）" >&2
    exit 1
fi

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$EXE" "$APP/Contents/MacOS/qview-gui-egui"
cp "$CRATE/Info.plist" "$APP/Contents/Info.plist"

# 复用 macOS 前端的 icns 图标（egui 前端没有单独的 icns）。
if [ -f "$ROOT/frontends/gui-macos/icon.icns" ]; then
    cp "$ROOT/frontends/gui-macos/icon.icns" "$APP/Contents/Resources/icon.icns"
fi

# 运行时资源（字体 / 窗口图标 / 赞赏码）已内嵌进二进制（src/assets.rs），无需复制。

# 分发许可文本：GPL-3.0（应用）+ OFL-1.1（内嵌的 Noto 字体，随字体分发必需）。
if [ -f "$ROOT/LICENSE" ]; then
    cp "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
fi
if [ -f "$CRATE/assets/OFL.txt" ]; then
    cp "$CRATE/assets/OFL.txt" "$APP/Contents/Resources/OFL.txt"
fi

# 签名（ad-hoc）：arm64/通用二进制在本机运行需要有效签名；未签名会被 Gatekeeper 拦截。
echo "==> ad-hoc codesign"
codesign --force --sign - "$APP" 2>/dev/null || {
    echo "警告：codesign 失败，跳过签名（可能仍可运行）" >&2
}

echo "==> 完成"
echo "    $APP"
echo "    运行：open \"$APP\"  或  $APP/Contents/MacOS/qview-gui-egui [文件]"
