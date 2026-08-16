//! 系统目录黑名单：器灵（Agent）不得打开 / 列出 / 写入的路径。
//!
//! 设计原则：
//! - **只限制 Agent 侧**（`DocumentService` / 工具），不限制用户从 GUI 手动打开。
//! - 匹配基于 **canonical 路径的分段前缀**：先 `canonicalize`（解析软链 / `..` / `\\?\` 前缀），
//!   再用路径片段做前缀匹配；大小写一律不敏感（黑名单宁可多拦，不可漏拦）。
//! - 规则表支持两个通配片段：`*` 匹配任意**一段**（如 `C:\Users\*\AppData`），
//!   `*:` 匹配任意盘符段（如 `*:\$Recycle.Bin` 拦任意盘的回收站）。
//! - 分段前缀匹配天然避免误伤：`/etc` 拦 `/etc/shadow`，但不拦 `/etcetera`。
//! - **白名单例外**优先于黑名单：例外规则命中的路径**放行**（`AppData\Local\Temp` 是
//!   qview 新建文件的临时落点 + Windows 用户临时目录，必须放行）。
//!
//! 性能：匹配只在 open / 工具调用时做（O(路径片段数 × 规则数)，规则 ≤ ~50），
//! 不在渲染热路径上 —— 对主路径零影响（见 [[performance-first]]）。

use std::path::Path;
use std::sync::Arc;

/// 默认黑名单：Windows / Linux / macOS 三平台系统关键目录。
///
/// 加载方式：`PathBlacklist::default()`（`DocumentService` 构造时自动使用）。
/// 斜杠统一用 `/` 或 `\` 均可；规则只对当前平台生效（别的平台规则天然不匹配）。
pub const DEFAULT_BLACKLIST: &[&str] = &[
    // ── Windows ──
    r"C:\Windows",                   // 系统核心（System32 / SysWOW64 / drivers …）
    r"C:\Program Files",             // 已安装程序
    r"C:\Program Files (x86)",
    r"C:\ProgramData",               // 全局程序数据
    r"C:\PerfLogs",                  // 性能日志
    r"C:\Recovery",                  // 系统恢复
    r"C:\Windows.old",               // 旧系统副本
    r"C:\System Volume Information", // 系统还原点
    r"C:\$Recycle.Bin",              // 回收站
    r"C:\Documents and Settings",    // 旧用户目录（Users 的软链）
    r"*:\Users\*\AppData",           // 用户配置 / 凭据（浏览器、密钥、缓存；任意盘，含重定向的用户目录）
    r"*:\Users\*\ntuser.dat",
    r"C:\pagefile.sys",
    r"C:\hiberfil.sys",
    r"C:\swapfile.sys",
    r"*:\System Volume Information", // 任意盘的还原点
    r"*:\$Recycle.Bin",              // 任意盘的回收站
    // ── Linux ──
    "/etc",          // 系统配置（shadow / ssh / passwd …）
    "/usr",          // 系统程序与库
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/boot",         // 内核 / 引导
    "/root",         // root 家目录（密钥 …）
    "/dev",
    "/proc",         // 虚拟文件系统
    "/sys",
    "/run",
    "/var",          // 系统运行数据（mail / spool / log）
    "/opt",          // 第三方软件安装
    // ── macOS ──
    "/private",      // /etc /var /tmp 软链的真实位置
    "/System",
    "/Library",      // 顶层系统库
    "/Applications", // 已安装应用
    "/cores",        // 崩溃转储
    // ── 跨平台：家目录下的敏感子目录 ──
    "/home/*/.ssh",
    "/home/*/.gnupg",
    "/Users/*/.ssh",
    "/Users/*/.gnupg",
    "/Users/*/Library", // macOS 用户库（Keychains / Safari 数据 …）
];

/// 黑名单的白名单例外（命中则放行，优先于黑名单）。
///
/// 只豁免**用户可写、非系统关键**的临时目录（qview 新建文件 / 报告导出 / 批注落盘都用它）：
/// - `AppData\Local\Temp`：Windows 用户临时目录（qview 新建文件 `create_new_file` 的落点）；
///   用盘符通配 `*:`，与黑名单 `*:\Users\*\AppData` 对齐。
/// - `/private/var/folders/*/*/T`：macOS 用户临时目录（`TMPDIR` 的真实位置）。
///   `/var` 是 `private/var` 的软链，canonicalize 后会落在 `/private/var/folders/<xx>/<yy>/T`，
///   而 `/private` 整段在黑名单里（覆盖 `/etc` `/var` `/tmp` 软链真实位置）——不例外的话
///   器灵在 macOS 上连自己的临时文件都打不开。只豁免 `T/`（temp），不豁免同级的
///   `C/`（caches，可能含其它应用敏感数据），宁可多拦。
pub const DEFAULT_BLACKLIST_EXCEPTIONS: &[&str] = &[
    r"*:\Users\*\AppData\Local\Temp",
    r"/private/var/folders/*/*/T",
];

/// 解析后的黑名单。
pub struct PathBlacklist {
    rules: Vec<Rule>,
    /// 白名单例外（命中则放行）。
    exceptions: Vec<Rule>,
}

struct Rule {
    /// 规则原文（错误信息里展示用）。
    display: String,
    /// 规范化后的路径片段。
    segments: Vec<String>,
}

/// 大小写一律不敏感（黑名单宁可多拦，不可漏拦）。
const CASE_INSENSITIVE: bool = true;

impl PathBlacklist {
    /// 用 `DEFAULT_BLACKLIST` + `DEFAULT_BLACKLIST_EXCEPTIONS` 构建。
    pub fn default() -> Arc<Self> {
        let mut bl = Self::new(DEFAULT_BLACKLIST.iter().map(|s| s.to_string()));
        for s in DEFAULT_BLACKLIST_EXCEPTIONS {
            bl.exceptions.push(Rule {
                segments: normalize_str(s),
                display: s.to_string(),
            });
        }
        Arc::new(bl)
    }

    /// 从规则字符串表构建（`*` 单段通配；`*:` 盘符通配），无白名单例外。
    pub fn new<I: IntoIterator<Item = String>>(iter: I) -> Self {
        let rules = iter
            .into_iter()
            .map(|display| Rule {
                segments: normalize_str(&display),
                display,
            })
            .collect();
        Self {
            rules,
            exceptions: Vec::new(),
        }
    }

    /// 追加一条白名单例外（命中则放行；配置扩展用）。
    pub fn add_exception(&mut self, rule: impl Into<String>) {
        let display = rule.into();
        self.exceptions.push(Rule {
            segments: normalize_str(&display),
            display,
        });
    }

    /// 规则数量（调试用）。
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// 判断 path 是否命中黑名单；命中返回命中的规则原文。
    pub fn is_blocked(&self, path: &Path) -> Option<&str> {
        // canonical 失败（文件不存在 / 权限）时退回原路径：仍按原样做分段匹配，
        // 这样 `/etc/shadow` 之类即使不存在也能被拦住。
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let target = normalize_path(&canonical);
        // 白名单例外优先：命中例外则放行（如 AppData\Local\Temp）。
        for e in &self.exceptions {
            if segments_match(&e.segments, &target) {
                return None;
            }
        }
        for r in &self.rules {
            if segments_match(&r.segments, &target) {
                return Some(&r.display);
            }
        }
        None
    }
}

/// 规则片段 vs 目标片段的前缀匹配（`*` 任意一段；`*:` 任意盘符段）。
fn segments_match(rule: &[String], target: &[String]) -> bool {
    if target.len() < rule.len() {
        return false;
    }
    for (r, t) in rule.iter().zip(target.iter()) {
        match r.as_str() {
            "*" => {}
            "*:" => {
                if !t.ends_with(':') {
                    return false;
                }
            }
            _ => {
                if r != t {
                    return false;
                }
            }
        }
    }
    true
}

/// 把规则字符串规范化为片段（处理 `\\?\` 前缀 + 分隔符 + 大小写）。
fn normalize_str(s: &str) -> Vec<String> {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    split_segments(s)
}

/// 把路径规范化为片段。
fn normalize_path(p: &Path) -> Vec<String> {
    normalize_str(&p.to_string_lossy())
}

fn split_segments(s: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == '/' || ch == '\\' {
            push_segment(&mut segs, &mut cur);
        } else {
            cur.push(ch);
        }
    }
    push_segment(&mut segs, &mut cur);
    segs
}

fn push_segment(segs: &mut Vec<String>, cur: &mut String) {
    if cur.is_empty() || cur == "." {
        cur.clear();
        return;
    }
    if CASE_INSENSITIVE {
        cur.make_ascii_lowercase();
    }
    segs.push(std::mem::take(cur));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_windows_system_tree() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new(r"C:\Windows\System32\drivers\etc\hosts")).is_some());
        assert!(bl.is_blocked(Path::new(r"C:\Windows\win.ini")).is_some());
        assert!(bl.is_blocked(Path::new(r"C:\Windows")).is_some());
        assert!(bl.is_blocked(Path::new(r"C:\Program Files\SomeApp\bin\app.exe")).is_some());
        assert!(bl.is_blocked(Path::new(r"C:\ProgramData\foo\config.ini")).is_some());
    }

    #[test]
    fn case_insensitive_on_all_platforms() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new(r"c:\windows\system32\config")).is_some());
        assert!(bl.is_blocked(Path::new("/ETC/SHADOW")).is_some());
    }

    #[test]
    fn blocks_unix_system_dirs() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new("/etc/shadow")).is_some());
        assert!(bl.is_blocked(Path::new("/usr/bin/bash")).is_some());
        assert!(bl.is_blocked(Path::new("/proc/self/mem")).is_some());
        assert!(bl.is_blocked(Path::new("/root/.bashrc")).is_some());
        assert!(bl.is_blocked(Path::new("/var/log/syslog")).is_some());
        assert!(bl.is_blocked(Path::new("/dev/null")).is_some());
    }

    #[test]
    fn wildcard_appdata_blocks_any_user() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Roaming\Chrome\Login Data")).is_some());
        // Local\Programs 等非 Temp 的 AppData 子目录仍被拦
        assert!(bl.is_blocked(Path::new(r"C:\Users\bob\AppData\Local\Programs\App\config.ini")).is_some());
        assert!(bl.is_blocked(Path::new(r"C:\Users\bob\AppData\Roaming\file")).is_some());
        // 任意盘（重定向的用户目录 / 双系统）同样被拦
        assert!(bl.is_blocked(Path::new(r"D:\Users\bob\AppData\Roaming\file")).is_some());
        // 非 AppData 的用户目录不拦
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\Documents\report.log")).is_none());
        assert!(bl.is_blocked(Path::new(r"D:\Users\alice\Documents\report.log")).is_none());
    }

    #[test]
    fn wildcard_secret_dirs_in_homes() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new("/home/alice/.ssh/id_rsa")).is_some());
        assert!(bl.is_blocked(Path::new("/Users/alice/.gnupg/secring.gpg")).is_some());
        assert!(bl.is_blocked(Path::new("/Users/alice/Library/Keychains/login.keychain-db")).is_some());
        assert!(bl.is_blocked(Path::new("/home/alice/work/a.log")).is_none());
    }

    #[test]
    fn any_drive_recycle_and_restore() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new(r"D:\$Recycle.Bin\S-1-5-21\file")).is_some());
        assert!(bl.is_blocked(Path::new(r"E:\System Volume Information\restore")).is_some());
        // 普通目录不误伤
        assert!(bl.is_blocked(Path::new(r"D:\work\data\a.log")).is_none());
    }

    #[test]
    fn segment_prefix_does_not_hit_lookalike_names() {
        let bl = PathBlacklist::default();
        // /etc 不拦 /etcetera；C:\Windows 不拦 C:\WindowsUpdate
        assert!(bl.is_blocked(Path::new("/etcetera")).is_none());
        assert!(bl.is_blocked(Path::new(r"C:\WindowsUpdate\cache")).is_none());
        // 但前缀命中：/etc/ 下的一切都被拦
        assert!(bl.is_blocked(Path::new("/etc/ssh/sshd_config")).is_some());
    }

    #[test]
    fn user_own_files_are_not_blocked() {
        let bl = PathBlacklist::default();
        assert!(bl.is_blocked(Path::new("/home/alice/reports/2026.log")).is_none());
        assert!(bl.is_blocked(Path::new("/tmp/tmpfile")).is_none());
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\Downloads\a.log")).is_none());
        assert!(bl.is_blocked(Path::new(r"D:\data\app.log")).is_none());
    }

    #[test]
    fn appdata_local_temp_is_exempt() {
        let bl = PathBlacklist::default();
        // Windows 临时目录（qview 新建文件落点）必须放行，任意盘
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Local\Temp\qview-new-123.txt")).is_none());
        assert!(bl.is_blocked(Path::new(r"C:\Users\bob\AppData\Local\Temp\sub\x.log")).is_none());
        assert!(bl.is_blocked(Path::new(r"D:\Users\bob\AppData\Local\Temp\sub\x.log")).is_none());
        // 但同级的其它 AppData 子目录仍被拦
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Local\Microsoft\Windows\Explorer")).is_some());
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Roaming\Chrome\Login Data")).is_some());
    }

    #[test]
    fn macos_user_temp_is_exempt_but_caches_not() {
        let bl = PathBlacklist::default();
        // macOS TMPDIR 的真实位置：/var/folders → canonical → /private/var/folders/<xx>/<yy>/T
        assert!(bl.is_blocked(Path::new("/private/var/folders/vh/7zhlgjyd4v72d0nwc_xmcvxw0000gn/T/qview-new-1.log")).is_none());
        assert!(bl.is_blocked(Path::new("/private/var/folders/zz/zyxvpxvq6csfxvn_n000012m00008n/T/sub/x.log")).is_none());
        // 同级的 C/（caches）仍被 /private 拦（可能含其它应用敏感数据）
        assert!(bl.is_blocked(Path::new("/private/var/folders/vh/7zhlgjyd4v72d0nwc_xmcvxw0000gn/C/com.apple.Safari")).is_some());
        // 共享的 /private/tmp（非 per-user）仍被拦
        assert!(bl.is_blocked(Path::new("/private/tmp/somebody.log")).is_some());
        // 结构不满足 foldes/*/*/T（缺 T 段）→ 不豁免，仍被 /private 拦
        assert!(bl.is_blocked(Path::new("/private/var/folders/etc/x")).is_some());
        // 普通用户目录不受 /private 影响（对照）
        assert!(bl.is_blocked(Path::new("/Users/qinwh/Projects/qview/foo.log")).is_none());
    }

    #[test]
    fn custom_blacklist_has_no_default_exceptions() {
        // 自定义黑名单（`new`）不带 AppData 例外；只有显式 add_exception 才放行
        let mut bl = PathBlacklist::new(vec![r"C:\Users\*\AppData".to_string()]);
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Local\Temp\x")).is_some());
        bl.add_exception(r"C:\Users\*\AppData\Local\Temp");
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Local\Temp\x")).is_none());
        assert!(bl.is_blocked(Path::new(r"C:\Users\alice\AppData\Roaming\x")).is_some());
    }
}
