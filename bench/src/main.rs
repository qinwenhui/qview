//! qview-bench — 标准行业性能测试工具。
//!
//! 生成 5 级标准测试日志（S≈10MB … XXL≈50GB），在 qview 真实引擎上测出
//! 打开 / 索引 / 搜索 / 导航 / 内存指标，产出可复现的 markdown 报告。
//!
//! ```
//! qview-bench gen  [dir] [--levels S,M,L] [--force]      # 生成测试数据
//! qview-bench run  [dir] [--levels ...] [--threads N] [--window MB] [--keep-cache]
//! qview-bench all  [dir] ...                             # gen + run
//! ```
//!
//! 说明：
//! - 生成是确定性的（固定种子），任何机器重复生成字节完全一致 → 结果可 A/B 对比。
//! - `--threads`/`--window` 只在本次进程生效（扫描池是进程级单例）；要对比不同
//!   参数请每次单独运行一次 `qview-bench run`。
//! - 默认先删除 `.qli` 测量"首次索引"；`--keep-cache` 保留（测量二次打开）。
//! - 超大级别（XXL 50GB）请预留足够磁盘空间，可先用 `--levels S,M,L` 试跑。

mod bench;
mod gen;
mod sys;

use std::path::PathBuf;

const DEFAULT_DIR: &str = "bench_data";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");

    let mut dir = DEFAULT_DIR.to_string();
    let mut levels: Option<Vec<String>> = None;
    let mut force = false;
    let mut keep_cache = false;
    let mut threads: Option<u32> = None;
    let mut window_mb: Option<u32> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--force" => force = true,
            "--keep-cache" => keep_cache = true,
            "--threads" => {
                if let Some(v) = args.get(i + 1) {
                    threads = v.parse().ok();
                    i += 1;
                }
            }
            "--window" => {
                if let Some(v) = args.get(i + 1) {
                    window_mb = v.parse().ok();
                    i += 1;
                }
            }
            "--levels" => {
                if let Some(v) = args.get(i + 1) {
                    levels =
                        Some(v.split(',').map(|s| s.trim().to_ascii_lowercase()).collect());
                    i += 1;
                }
            }
            a if a.starts_with('-') => eprintln!("未知参数: {a}"),
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }
    if let Some(p) = positional.first() {
        dir = p.clone();
    }
    let dir = PathBuf::from(dir);

    // Validate requested levels early.
    if let Some(sel) = &levels {
        for l in sel {
            if !gen::LEVELS.iter().any(|(name, _)| name == l) {
                eprintln!("未知级别 `{l}`（可用: S/M/L/XL/XXL，不区分大小写）");
            }
        }
    }

    match cmd {
        "gen" => gen_cli(&dir, &levels, force),
        "run" => bench::run_cli(&dir, &levels, threads, window_mb, keep_cache),
        "all" => {
            gen_cli(&dir, &levels, force);
            bench::run_cli(&dir, &levels, threads, window_mb, keep_cache);
        }
        _ => print_help(),
    }
}

fn gen_cli(dir: &std::path::Path, levels: &Option<Vec<String>>, force: bool) {
    use std::time::Instant;
    std::fs::create_dir_all(dir).expect("create dir");
    for (level, n) in gen::LEVELS {
        if !gen::level_selected(level, levels) {
            continue;
        }
        let f = dir.join(gen::level_file(level));
        if f.exists() && !force {
            println!("跳过 {level}: 已存在 {f:?}（用 --force 重建）");
            continue;
        }
        print!("生成 {level}: {} 行 → {f:?} ...", n);
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let t = Instant::now();
        gen::gen_file(&f, *n).expect("gen_file");
        let sz = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
        println!(
            " 完成 {:.1} MiB / {:.3} GB, {:.1}s",
            sz as f64 / (1024.0 * 1024.0),
            sz as f64 / 1e9,
            t.elapsed().as_secs_f64()
        );
    }
}

fn print_help() {
    println!(
        "qview-bench — 标准行业性能测试\n\
\n\
用法:\n\
  qview-bench gen  [目录] [--levels S,M,L] [--force]    生成 5 级标准测试日志\n\
  qview-bench run  [目录] [--levels ...] [--threads N] [--window MB] [--keep-cache]\n\
                                                        在真实引擎上跑基准，写 report.md\n\
  qview-bench all  [目录] [同上选项]                    生成 + 跑基准\n\
\n\
级别:  S=100k行(~10MB)  M=1M(~100MB)  L=10M(~1GB)  XL=100M(~10GB)  XXL=500M(~50GB)\n\
示例:\n\
  qview-bench all  ./data --levels S,M,L\n\
  qview-bench run  ./data --window 64 --threads 0\n"
    );
}
