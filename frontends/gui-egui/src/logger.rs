//! Structured file logger for qview.
//!
//! Writes timestamped, levelled log entries to `qview.log` in the data
//! directory.  Auto-rotates when the file exceeds 5 MiB (keeps 3 backups).
//! Every call is flushed immediately so entries survive a crash.
//!
//! ## Timestamps
//!
//! All timestamps are LOCAL time (matches the filesystem / task manager clock,
//! so `qview.log` lines line up with file mtimes).  Windows uses the OS
//! `GetLocalTime` (DST-aware); non-Windows falls back to a UTC conversion.
//! The format is:
//!
//! ```text
//! [2026-08-04 14:30:12.345] INFO  app         | 打开文件: C:\logs\app.log (1.2 GiB, 50M 行)
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! info!("app", "打开文件: {}", path);
//! warn!("menu", "缓存目录不存在: {}", dir);
//! error!("engine", "索引失败: {}", e);
//! debug!("search", "cursor={} total={}", c, t);
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Log level
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info  => "INFO ",
            Level::Warn  => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

/// 最小落盘级别：默认 **Info**——只记用户可见的生命周期事件，高频 debug/trace
/// 不落盘，避免长期运行（AI 会话 / 搜索 / 跳转）把 qview.log 写爆。
///
/// 排查问题时用环境变量临时放开：`QVIEW_LOG_LEVEL=debug`（或 warn/error）。
fn min_level() -> Level {
    static MIN: std::sync::OnceLock<Level> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| match std::env::var("QVIEW_LOG_LEVEL").as_deref() {
        Ok("debug") => Level::Debug,
        Ok("warn") => Level::Warn,
        Ok("error") => Level::Error,
        _ => Level::Info,
    })
}

// ---------------------------------------------------------------------------
// Logger singleton
// ---------------------------------------------------------------------------

const MAX_SIZE: u64 = 5 * 1024 * 1024; // 5 MiB
const MAX_BACKUPS: usize = 3;

pub struct Logger {
    writer: Mutex<BufWriter<File>>,
    path: PathBuf,
    approximate_size: Mutex<u64>,
}

impl Logger {
    /// Create the log file (and parent directories) inside `data_dir`.
    /// Returns `None` when the file cannot be opened — the app still works
    /// fine without logging.
    pub fn init(data_dir: &std::path::Path) -> Option<&'static Logger> {
        let _ = fs::create_dir_all(data_dir);
        let path = data_dir.join("qview.log");

        Self::rotate_if_needed(&path);

        let file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(_) => return None,
        };

        let logger = Logger {
            writer: Mutex::new(BufWriter::with_capacity(8192, file)),
            approximate_size: Mutex::new(0),
            path,
        };

        let leaked: &'static Logger = Box::leak(Box::new(logger));
        Some(leaked)
    }

    /// Rotate log files: qview.log → qview.1.log → qview.2.log → qview.3.log
    fn rotate_if_needed(path: &std::path::Path) {
        let need_rotate = match fs::metadata(path) {
            Ok(meta) => meta.len() >= MAX_SIZE,
            Err(_) => return,
        };
        if !need_rotate {
            return;
        }
        // Shift backups: 2→3, 1→2
        for i in (1..MAX_BACKUPS).rev() {
            let old = path.with_file_name(format!("qview.{}.log", i));
            let newer = path.with_file_name(format!("qview.{}.log", i + 1));
            let _ = fs::rename(&old, &newer);
        }
        // Move current → .1
        let backup = path.with_file_name("qview.1.log");
        let _ = fs::rename(path, &backup);
    }

    /// Panic-hook 专用写入：**best-effort，绝不阻塞**。
    /// 若 panic 恰好发生在日志写入持锁期间，用 `try_lock` 直接跳过，
    /// 避免 hook 里再次锁同一 Mutex 造成死锁（把闪退变成假死）。
    fn write_panic(&self, msg: &str) {
        let line = format!(
            "[{}] {:<5} {:<12} | {}\n",
            timestamp_local(),
            Level::Error.tag(),
            "panic",
            msg
        );
        if let Ok(mut w) = self.writer.try_lock() {
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
        }
    }

    /// Thread-safe write + flush.
    pub fn write(&self, level: Level, module: &str, msg: &str) {
        // 级别过滤：低于 min_level()（默认 Debug）不落盘。
        if level < min_level() {
            return;
        }
        let ts = timestamp_local();

        let module = if module.len() > 12 { &module[..12] } else { module };

        let line = format!(
            "[{}] {:<5} {:<12} | {}\n",
            ts,
            level.tag(),
            module,
            msg,
        );

        let added = line.len() as u64;
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
        }

        // Lazy rotation check (every ~256 KiB).
        if let Ok(mut sz) = self.approximate_size.lock() {
            *sz += added;
            if *sz > 256 * 1024 {
                *sz = 0;
                drop(sz);
                Self::rotate_if_needed(&self.path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Local timestamp formatting (zero-dependency)
// ---------------------------------------------------------------------------

/// Format: `2026-08-04 14:30:12.345` in LOCAL time.
///
/// Windows: `GetLocalTime` from kernel32 — the OS resolves the local zone and
/// DST, so timestamps match the filesystem clock exactly (a UTC logger is easy
/// to mistake for stale data when cross-checking file mtimes).
#[cfg(windows)]
fn timestamp_local() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        dow: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(lp_system_time: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    // SAFETY: `t` is a valid, writable SYSTEMTIME buffer owned by this stack frame.
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis
    )
}

/// Non-Windows fallback: UTC conversion (the zero-dependency option without
/// libc/chrono).  Same format; timestamps just read as UTC there.
#[cfg(not(windows))]
fn timestamp_local() -> String {
    let dur = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return "----.--.-- --:--:--.---".to_string(),
    };
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();

    let hms = secs % 86400;
    let h = hms / 3600;
    let m = (hms % 3600) / 60;
    let s = hms % 60;

    let (y, mo, d) = days_to_date(secs / 86400);

    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}.{millis:03}")
}

/// Convert days since 1970-01-01 to (year, month, day).  UTC.
#[cfg(not(windows))]
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Howard Hinnant's civil-from-days algorithm (epoch: 1970-01-01).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

pub(crate) static LOGGER: std::sync::OnceLock<&'static Logger> = std::sync::OnceLock::new();

/// Initialise the global logger.  Safe to call multiple times — only the
/// first call takes effect.
pub fn init(data_dir: &std::path::Path) {
    if let Some(l) = Logger::init(data_dir) {
        let _ = LOGGER.set(l);
    }
    install_panic_hook();
}

/// 把 Rust panic 写进 qview.log。本应用 `#![windows_subsystem="windows"]`，
/// panic 消息只发往 stderr（无控制台 → 不可见），表现为"闪退且日志无报错"
/// （app.rs 里也有同款注释）。装 hook 后所有 panic（含后台线程）都以 ERROR
/// 级别落盘，排查不用靠猜。先跑默认 hook 输出 stderr，再写日志。
fn install_panic_hook() {
    use std::panic;
    let prev = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        prev(info);
        // `format!("{info}")` 自带 `src/xxx.rs:行:列`，是定位闪退的关键。
        let msg = format!("{info}");
        // backtrace 只取前若干行，防止把 5 MiB 日志刷爆；`strip=symbols`
        // 的 release 下只有地址，但调用栈形状仍能看出崩溃在哪一层。
        let bt = std::backtrace::Backtrace::force_capture();
        let bt_str = bt.to_string();
        let bt_head: Vec<&str> = bt_str.lines().take(16).collect();
        if let Some(l) = LOGGER.get() {
            l.write_panic(&format!("{msg}\n{}", bt_head.join("\n")));
        }
    }));
}

/// Current LOCAL time in the logger's format (`2026-08-06 14:30:12.345`).
/// Used for user-data timestamps (annotations) so they line up with the log.
pub fn now() -> String {
    timestamp_local()
}

// ---------------------------------------------------------------------------
// Convenience macros
// ---------------------------------------------------------------------------

#[macro_export]
macro_rules! log_info {
    ($module:expr, $($arg:tt)*) => {{
        if let Some(l) = $crate::logger::LOGGER.get() {
            l.write($crate::logger::Level::Info, $module, &format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! log_debug {
    ($module:expr, $($arg:tt)*) => {{
        if let Some(l) = $crate::logger::LOGGER.get() {
            l.write($crate::logger::Level::Debug, $module, &format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($module:expr, $($arg:tt)*) => {{
        if let Some(l) = $crate::logger::LOGGER.get() {
            l.write($crate::logger::Level::Warn, $module, &format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! log_error {
    ($module:expr, $($arg:tt)*) => {{
        if let Some(l) = $crate::logger::LOGGER.get() {
            l.write($crate::logger::Level::Error, $module, &format!($($arg)*));
        }
    }};
}

// ---------------------------------------------------------------------------
// tracing → qview.log 桥接
// ---------------------------------------------------------------------------
//
// contexa-rs / qview-agent 用 `tracing` 记日志，但本应用没有全局 subscriber，
// 那些事件（LLM 错误链、round 进度、工具调用等）此前全部丢失，排查全靠猜。
// 这里装一个极简 Subscriber，把 tracing 事件按级别写入同一个 qview.log，
// 格式与 log_*! 一致：`[时间] LEVEL 模块 | 消息 [字段...]`。

/// 捕获事件字段：message 单独取，其余字段拼成 `k=v`。
struct FieldCollector {
    message: String,
    fields: Vec<String>,
}

impl FieldCollector {
    fn push(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn render(&self) -> String {
        let mut s = self.message.clone();
        if !self.fields.is_empty() {
            s.push_str("  [");
            s.push_str(&self.fields.join(", "));
            s.push(']');
        }
        s
    }
}

impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.push(field, &format!("{value:?}"));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field, value);
    }
}

/// 最小 Subscriber：tracing 事件 → qview.log。
struct QviewTracingSink;

impl tracing::Subscriber for QviewTracingSink {
    fn enabled(&self, _meta: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _a: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _a: &tracing::span::Id, _b: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut c = FieldCollector {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut c);
        if c.message.is_empty() {
            return;
        }
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            _ => Level::Debug,
        };
        if let Some(l) = LOGGER.get() {
            l.write(level, event.metadata().target(), &c.render());
        }
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// 安装 tracing → qview.log 桥接。幂等（只生效一次）。
pub fn init_tracing() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(QviewTracingSink);
    });
}
