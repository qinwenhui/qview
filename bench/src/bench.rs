//! The benchmark runner: measures qview's production engine (mmap + sparse
//! index + windowed NO_BUFFERING search) on the standard test files and
//! renders a markdown report with empty competitor columns.
//!
//! Every metric goes through the same code paths the GUI uses
//! (`Engine`, `run_search`, `BlockIndex::get`, `line_of_byte`) — the numbers
//! are what a user actually gets, not synthetic micro-benchmarks.

use qview_core::config::{EngineConfig, SearchConfig};
use qview_core::engine::Engine;
use qview_core::file::MmapBackend;
use qview_core::search::{parse_query, run_search, Query, SearchOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::gen;
use crate::sys;

#[derive(Default)]
pub struct FileResult {
    pub level: String,
    pub size_bytes: u64,
    pub line_count: u64,
    /// mmap open (for small files this includes the synchronous in-memory index).
    pub open_ms: f64,
    /// First index build, no `.qli` (0 if the file was already indexed).
    pub index_ms: f64,
    /// Reopen after index → `.qli` cache hit.
    pub open_cached_ms: f64,
    /// Resolve + decode the last line.
    pub jump_end_us: f64,
    /// Literal search on `ERROR-CODE-404`.
    pub lit_ms: f64,
    pub lit_hits: usize,
    /// Regex search on `ERROR-\d{3}`.
    pub regex_ms: f64,
    pub regex_hits: usize,
    /// Average `BlockIndex::get` across the result set (per navigation).
    pub nav_get_us: f64,
    /// Average `line_of_byte` across hit offsets (per jump line resolution).
    pub nav_line_of_byte_us: f64,
    /// Process RSS after this level's open + full-file searches (WorkingSet /
    /// VmRSS), matching the industry metric "open + full search → memory".
    pub rss_kb: u64,
    /// Average process-wide CPU utilisation during the regex search (0–100 %).
    pub cpu_pct: f64,
    pub fail: Option<String>,
}

pub fn bench_file(level: &str, path: &Path, cfg: &EngineConfig, keep_cache: bool) -> FileResult {
    let mut r = FileResult {
        level: level.to_string(),
        ..Default::default()
    };

    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) => {
            r.fail = Some(format!("metadata: {e}"));
            return r;
        }
    };
    r.size_bytes = size;

    // Fresh build: drop any stale `.qli` (unless the caller wants to reuse it).
    if !keep_cache {
        if let Some(cp) = cfg.cache_path(path) {
            let _ = std::fs::remove_file(&cp);
        }
    }

    // ---- open (mmap) + first index ----
    let t = Instant::now();
    let mut engine = match Engine::with_config(path.to_path_buf(), cfg.clone()) {
        Ok(e) => e,
        Err(e) => {
            r.fail = Some(format!("open: {e}"));
            return r;
        }
    };
    r.open_ms = ms(t.elapsed());

    r.index_ms = if engine.index.is_complete() {
        0.0 // cache hit or small file
    } else {
        let t = Instant::now();
        if let Err(e) = engine.build_index_blocking() {
            r.fail = Some(format!("index: {e}"));
            return r;
        }
        ms(t.elapsed())
    };
    r.line_count = engine.total_lines;

    // ---- jump to end ----
    if engine.total_lines > 0 {
        let t = Instant::now();
        let _ = engine.read_line(engine.total_lines - 1);
        r.jump_end_us = us(t.elapsed());
    }
    drop(engine);

    // ---- reopen: `.qli` cache hit ----
    let t = Instant::now();
    let engine2 = match Engine::with_config(path.to_path_buf(), cfg.clone()) {
        Ok(e) => e,
        Err(e) => {
            r.fail = Some(format!("reopen: {e}"));
            return r;
        }
    };
    r.open_cached_ms = ms(t.elapsed());

    // ---- searches (both are full single-pass scans) ----
    let sw = engine2.scan_window;
    let sconf = cfg.search.clone();

    let lit_q = parse_query(
        gen::LITERAL,
        // Benchmark data is LF (gen writes newline="\n"), so no CRLF handling.
        &SearchOptions { case_sensitive: true, use_regex: false, whole_word: false, crlf: false },
    )
    .expect("literal parse");
    let (lit_ms, lit_idx) = measure_search(&lit_q, &engine2.mmap, &sconf, sw);
    r.lit_ms = lit_ms;
    r.lit_hits = lit_idx.total_count();

    let re_q = parse_query(
        gen::REGEX_PAT,
        &SearchOptions { case_sensitive: true, use_regex: true, whole_word: false, crlf: false },
    )
    .expect("regex parse");
    let cpu0 = sys::process_cpu_ns();
    let t = Instant::now();
    let re_idx = match run_search(&re_q, &engine2.mmap, &sconf, sw) {
        Ok(idx) => idx,
        Err(e) => {
            r.fail = Some(format!("regex search: {e}"));
            return r;
        }
    };
    r.regex_ms = ms(t.elapsed());
    r.cpu_pct = cpu_pct(sys::process_cpu_ns() - cpu0, t.elapsed());
    r.regex_hits = re_idx.total_count();

    // ---- navigation ----
    r.nav_get_us = avg_get(&lit_idx);
    r.nav_line_of_byte_us = avg_line_of_byte(&engine2, &lit_idx);

    // ---- memory after open + searches (current working set) ----
    r.rss_kb = sys::current_rss_kb();
    r
}

fn measure_search(
    q: &Query,
    mmap: &MmapBackend,
    sconf: &SearchConfig,
    sw: u64,
) -> (f64, qview_core::search::BlockIndex) {
    let t = Instant::now();
    let idx = match run_search(q, mmap, sconf, sw) {
        Ok(i) => i,
        Err(e) => panic!("search failed: {e}"),
    };
    (ms(t.elapsed()), idx)
}

/// Average `BlockIndex::get(n)` cost over hits spread across the result set.
fn avg_get(idx: &qview_core::search::BlockIndex) -> f64 {
    let stored = idx.stored_count();
    if stored == 0 {
        return 0.0;
    }
    let total = idx.total_count().max(1);
    let k = stored.min(2000);
    let mut sum = 0.0;
    for j in 0..k {
        let n = (j as u64) * (total as u64) / (k as u64);
        let t = Instant::now();
        let _ = idx.get(n as usize);
        sum += us(t.elapsed());
    }
    sum / k as f64
}

/// Average `line_of_byte` (sparse-anchor + memchr) over hit offsets.
fn avg_line_of_byte(engine: &Engine, idx: &qview_core::search::BlockIndex) -> f64 {
    let stored = idx.stored_count();
    if stored == 0 {
        return 0.0;
    }
    let k = stored.min(200);
    let mut sum = 0.0;
    for j in 0..k {
        let n = (j as u64) * (stored as u64) / (k as u64);
        if let Some(off) = idx.get(n as usize) {
            let t = Instant::now();
            let _ = engine.line_of_byte(off);
            sum += us(t.elapsed());
        }
    }
    sum / k as f64
}

fn cpu_pct(cpu_ns: u64, wall: Duration) -> f64 {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).max(1) as f64;
    let w = wall.as_nanos() as f64;
    if w <= 0.0 || cpu_ns == 0 {
        0.0
    } else {
        (cpu_ns as f64 / w * 100.0 / cores).clamp(0.0, 100.0)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}
fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

// ---------------------------------------------------------------------------
// CLI + report
// ---------------------------------------------------------------------------

pub fn run_cli(
    dir: &Path,
    levels: &Option<Vec<String>>,
    threads: Option<u32>,
    window_mb: Option<u32>,
    keep_cache: bool,
) {
    let mut cfg = EngineConfig::default();
    if let Some(t) = threads {
        cfg.scan_threads = t;
    }
    if let Some(w) = window_mb {
        cfg.scan_window_mb = w;
    }

    println!(
        "qview-bench run | 线程={} 窗口={}MB 索引方式={:?} 缓存清理={}",
        cfg.scan_threads, cfg.scan_window_mb, cfg.index_build_mode, !keep_cache
    );

    // Builds the (process-global) scan pool once; report the real thread count.
    let pool_threads = qview_core::parallel::scan_pool().current_num_threads();

    let mut results: Vec<FileResult> = Vec::new();
    for (level, _) in gen::LEVELS {
        if !gen::level_selected(level, levels) {
            continue;
        }
        let path = dir.join(gen::level_file(level));
        if !path.exists() {
            println!("跳过 {level}: 文件不存在 {path:?}（先运行 `qview-bench gen`）");
            continue;
        }
        print!("基准 {level} ({path:?}) ...");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let r = bench_file(level, &path, &cfg, keep_cache);
        match &r.fail {
            Some(f) => println!(" 失败: {f}"),
            None => println!(
                " 索引={:.1}ms 二次打开={:.1}ms 字面量={:.1}ms({}命中) 正则={:.1}ms({}命中) RSS={:.0}MB",
                r.index_ms,
                r.open_cached_ms,
                r.lit_ms,
                r.lit_hits,
                r.regex_ms,
                r.regex_hits,
                r.rss_kb as f64 / 1024.0
            ),
        }
        results.push(r);
    }

    if results.is_empty() {
        eprintln!("没有可测试的文件。先 `qview-bench gen` 生成测试数据。");
        return;
    }

    let report = render_report(dir, &results, &cfg, pool_threads);
    let out = dir.join("report.md");
    if let Err(e) = std::fs::write(&out, report) {
        eprintln!("写报告失败: {e}");
    } else {
        println!("\n报告已写入: {}", out.display());
    }
}

fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = gen::civil_from_days(secs / 86_400);
    format!("{y:04}-{m:02}-{d:02}")
}

fn gb(v: u64) -> String {
    format!("{:.3} GB", v as f64 / 1e9)
}
fn fmt_ms(v: f64) -> String {
    if v <= 0.0 {
        "—".into()
    } else if v >= 1000.0 {
        format!("{:.2} s", v / 1000.0)
    } else if v >= 10.0 {
        format!("{:.0} ms", v)
    } else if v >= 1.0 {
        format!("{:.1} ms", v)
    } else {
        format!("{:.2} ms", v)
    }
}
fn fmt_us(v: f64) -> String {
    if v <= 0.0 {
        "—".into()
    } else if v >= 1000.0 {
        format!("{:.2} ms", v / 1000.0)
    } else if v >= 1.0 {
        format!("{:.0} µs", v)
    } else {
        format!("{:.2} µs", v)
    }
}
fn fmt_int(v: usize) -> String {
    let s = format!("{v}");
    // group thousands with commas
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

fn render_report(
    dir: &Path,
    results: &[FileResult],
    cfg: &EngineConfig,
    pool_threads: usize,
) -> String {
    let mut o = String::new();
    o.push_str("# qview 标准性能测试报告\n\n");
    o.push_str(&format!(
        "- 测试日期：{}\n- 程序版本：v{}（release，工作区优化配置）\n",
        today(),
        env!("CARGO_PKG_VERSION")
    ));
    o.push_str(&format!(
        "- 系统：{} · {} 逻辑核 · 扫描线程 {}（配置 {}，自动=核数−1）\n",
        std::env::consts::OS,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        pool_threads,
        cfg.scan_threads
    ));
    o.push_str(&format!(
        "- 参数：扫描窗口 {} MB · 索引方式 {:?} · 搜索采样间隔 {} / 上限 {}\n",
        cfg.scan_window_mb,
        cfg.index_build_mode,
        cfg.search.sample_interval,
        cfg.search.max_samples
    ));
    o.push_str(&format!(
        "- 数据目录：{}\n- 数据生成：固定种子（确定性，可复现）\n\n",
        dir.display()
    ));
    o.push_str(
        "> 竞品列（Notepad++ / VS Code / EmEditor）留空，按测试方案 §三 手工填入。\n\n",
    );

    // 1. files
    o.push_str("## 1. 测试文件\n\n");
    o.push_str("| 级别 | 文件 | 大小 | 行数 | 平均行长 |\n|---|---|---|---|---|\n");
    for r in results {
        let avg_len = if r.line_count > 0 { r.size_bytes as f64 / r.line_count as f64 } else { 0.0 };
        o.push_str(&format!(
            "| {} | {} | {} | {} | {:.0} B |\n",
            r.level.to_uppercase(),
            gen::level_file(&r.level),
            gb(r.size_bytes),
            fmt_int(r.line_count as usize),
            avg_len
        ));
    }

    // 2. open / index
    o.push_str("\n## 2. 打开与索引\n\n");
    o.push_str(
        "| 级别 | 打开(mmap) | 首次索引 | 二次打开(.qli) | 索引吞吐 |\n|---|---|---|---|---|\n",
    );
    for r in results {
        let tp = if r.index_ms > 0.0 {
            format!("{:.2} GB/s", r.size_bytes as f64 / 1e9 / (r.index_ms / 1000.0))
        } else {
            "—".into()
        };
        o.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            r.level.to_uppercase(),
            fmt_ms(r.open_ms),
            fmt_ms(r.index_ms),
            fmt_ms(r.open_cached_ms),
            tp
        ));
    }
    o.push_str(
        "\n> 小文件（≤10 MB）在打开时同步建索引，`打开` 含建索引；大文件 `打开` 仅 mmap，首次建索引单独计时。\n",
    );

    // 3. searches
    o.push_str("\n## 3. 搜索（全文件单遍扫描）\n\n");
    o.push_str(&format!("### 3.1 字面量 `{}`\n\n", gen::LITERAL));
    o.push_str(
        "| 级别 | 命中数 | qview | Notepad++ | VS Code | EmEditor |\n|---|---|---|---|---|---|\n",
    );
    for r in results {
        o.push_str(&format!(
            "| {} | {} | {} |  |  |  |\n",
            r.level.to_uppercase(),
            fmt_int(r.lit_hits),
            fmt_ms(r.lit_ms)
        ));
    }
    o.push_str(&format!("\n### 3.2 正则 `{}`\n\n", gen::REGEX_PAT));
    o.push_str(
        "| 级别 | 命中数 | qview | Notepad++ | VS Code | EmEditor |\n|---|---|---|---|---|---|\n",
    );
    for r in results {
        o.push_str(&format!(
            "| {} | {} | {} |  |  |  |\n",
            r.level.to_uppercase(),
            fmt_int(r.regex_hits),
            fmt_ms(r.regex_ms)
        ));
    }
    o.push_str(&format!("\n> 附属字面量目标 `{}` 同样可搜索。\n", gen::TIMEOUT));

    // 4. navigation
    o.push_str("\n## 4. 导航\n\n");
    o.push_str(
        "| 级别 | 跳转末尾 | 命中间跳转 `get` | 命中定位行 `line_of_byte` |\n|---|---|---|---|\n",
    );
    for r in results {
        o.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            r.level.to_uppercase(),
            fmt_us(r.jump_end_us),
            fmt_us(r.nav_get_us),
            fmt_us(r.nav_line_of_byte_us)
        ));
    }

    // 5. resources
    o.push_str("\n## 5. 资源占用\n\n");
    o.push_str("| 级别 | 进程内存(开文件+搜索后) | 搜索平均 CPU 占用 |\n|---|---|---|\n");
    for r in results {
        let rss = if r.rss_kb > 0 {
            format!("{:.0} MB", r.rss_kb as f64 / 1024.0)
        } else {
            "—".into()
        };
        let cpu = if r.cpu_pct > 0.0 {
            format!("{:.0}%", r.cpu_pct)
        } else {
            "—".into()
        };
        o.push_str(&format!("| {} | {} | {} |\n", r.level.to_uppercase(), rss, cpu));
    }

    // 6. conclusions
    o.push_str("\n## 6. 结论与解读\n\n");
    if let Some(max) = results.iter().max_by_key(|r| r.size_bytes) {
        o.push_str("- 索引吞吐：");
        if max.index_ms > 0.0 {
            o.push_str(&format!(
                "最大文件 {}（{} 行）首次索引 {:.2}s → {:.2} GB/s；若接近磁盘顺序读上限说明瓶颈在磁盘而非 CPU。\n",
                gb(max.size_bytes),
                fmt_int(max.line_count as usize),
                max.index_ms / 1000.0,
                max.size_bytes as f64 / 1e9 / (max.index_ms / 1000.0)
            ));
        } else {
            o.push_str("（本次运行命中 .qli 缓存，未重建索引）\n");
        }
    }
    if let Some(mem) = results.iter().max_by_key(|r| r.rss_kb) {
        o.push_str(&format!(
            "- 进程内存 {:.0} MB（{} 级，最大文件 {}）：对比文件大小观察是否随文件线性增长——qview 流式扫描设计下应基本持平。\n",
            mem.rss_kb as f64 / 1024.0,
            mem.level.to_uppercase(),
            gb(mem.size_bytes)
        ));
    }
    if let Some(s) = results.iter().max_by_key(|r| r.lit_hits) {
        o.push_str(&format!(
            "- 最大命中结果集：{}（{} 级）——命中多时按采样间隔 {} 存储（内存有界）、总数仍精确。\n",
            fmt_int(s.lit_hits),
            s.level.to_uppercase(),
            cfg.search.sample_interval
        ));
    }
    o.push_str("\n---\n\n本报告由 `qview-bench` 生成：`cargo run --release -p qview-bench -- run <dir>`\n");
    o
}
