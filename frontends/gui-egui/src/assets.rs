//! 运行时资源加载。
//!
//! 策略：
//!   - **Windows**：强制走 sidecar（`<exe>/assets/<name>`），二进制**不含**
//!     任何资源数据；exe 体积约 13M（参考 qview-gui-native 的 12M）。
//!     `qview-bundle` 与 `gui/egui/build.rs` 都会在构建期把 `gui/egui/assets/`
//!     复制到 `target/release/assets/`（与 exe 同目录），运行时命中。
//!   - **macOS / Linux**：sidecar-first，缺失时回退到 `include_bytes!` 编译期
//!     嵌入，保持 `.app` / 单文件分发的自包含体验。
//!
//! Windows sidecar 缺失时的行为：函数返回 `Cow::Borrowed(&[])`（空字节），
//! 字体/图标/赞赏码将显示为空白或占位符。这是显式的"无 asset 可用"语义，
//! 比把 17M ttf 塞回 exe 更合理（[[performance-first]]：热路径零浪费）。
//!
//! 新增资源只需把文件放进 `gui/egui/assets/`，build.rs 会自动处理复制。

use std::borrow::Cow;

const FONT_FILE: &str = "NotoSansSC-VF.ttf";
const ICON_FILE: &str = "icon.ico";
const WECHAT_FILE: &str = "donate_wechat.png";
const ALIPAY_FILE: &str = "donate_alipay.png";

/// 内嵌字体的名称（与文件 stem 一致，供配置 `gui.font_family` 匹配）。
pub const FONT_NAME: &str = "NotoSansSC-VF";

/// 试读 `<current_exe>/../assets/<file>`；找不到返回 `None`。
#[inline]
fn sidecar(file: &str) -> Option<Vec<u8>> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("assets").join(file);
    std::fs::read(&dir).ok()
}

#[inline]
fn load_or_empty(file: &str, embed: &'static [u8]) -> Cow<'static, [u8]> {
    sidecar(file)
        .map(Cow::Owned)
        .unwrap_or_else(|| Cow::Borrowed(embed))
}

/// 中文字体（17 MB NotoSansSC-VF，OFL 协议，允许随二进制再分发）。
/// Windows 强制走 sidecar；其他平台 sidecar-first + embed fallback。
#[cfg(target_os = "windows")]
pub fn font_bytes() -> Cow<'static, [u8]> {
    load_or_empty(FONT_FILE, &[])
}

#[cfg(not(target_os = "windows"))]
pub fn font_bytes() -> Cow<'static, [u8]> {
    load_or_empty(
        FONT_FILE,
        include_bytes!("../assets/NotoSansSC-VF.ttf"),
    )
}

/// 窗口 / 任务栏图标（ico）。
#[cfg(target_os = "windows")]
pub fn icon_bytes() -> Cow<'static, [u8]> {
    load_or_empty(ICON_FILE, &[])
}

#[cfg(not(target_os = "windows"))]
pub fn icon_bytes() -> Cow<'static, [u8]> {
    load_or_empty(ICON_FILE, include_bytes!("../assets/icon.ico"))
}

/// 微信赞赏码（PNG）。
#[cfg(target_os = "windows")]
pub fn donate_wechat_png() -> Cow<'static, [u8]> {
    load_or_empty(WECHAT_FILE, &[])
}

#[cfg(not(target_os = "windows"))]
pub fn donate_wechat_png() -> Cow<'static, [u8]> {
    load_or_empty(WECHAT_FILE, include_bytes!("../assets/donate_wechat.png"))
}

/// 支付宝赞赏码（PNG）。
#[cfg(target_os = "windows")]
pub fn donate_alipay_png() -> Cow<'static, [u8]> {
    load_or_empty(ALIPAY_FILE, &[])
}

#[cfg(not(target_os = "windows"))]
pub fn donate_alipay_png() -> Cow<'static, [u8]> {
    load_or_empty(ALIPAY_FILE, include_bytes!("../assets/donate_alipay.png"))
}

/// "sidecar" 或 "embedded"，仅供日志/诊断。
pub fn font_source() -> &'static str {
    if sidecar(FONT_FILE).is_some() {
        "sidecar"
    } else {
        "embedded"
    }
}

/// "sidecar" 或 "embedded"，仅供日志/诊断。
pub fn icon_source() -> &'static str {
    if sidecar(ICON_FILE).is_some() {
        "sidecar"
    } else {
        "embedded"
    }
}
