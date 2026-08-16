//! `Engine::huge_lines` — 索引驱动的超长行检测（替代旧的整文件扫描）。
//! 用真实临时文件 + 同步建索引，走 mmap + sparse index 完整路径。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use qview_core::engine::Engine;

const THRESHOLD: u64 = 64 * 1024;

/// 临时文件唯一名计数器（`as_nanos()` 在本机分辨率不足，并行测试会撞名互删文件）。
static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn open(contents: &[u8]) -> (Engine, PathBuf) {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir()
        .join(format!("qview-huge-lines-{}-{}.log", std::process::id(), seq));
    std::fs::write(&path, contents).unwrap();
    let mut e = Engine::new(path.clone()).unwrap();
    e.build_index_blocking().unwrap();
    (e, path)
}

#[test]
fn huge_line_in_middle() {
    // 行 0 短，行 1 超长（70KB），行 2 短
    let mut data = Vec::new();
    data.extend_from_slice(b"short\n");
    data.extend_from_slice(&vec![b'a'; 70 * 1024]);
    data.push(b'\n');
    data.extend_from_slice(b"mid\n");
    let (e, p) = open(&data);
    assert_eq!(e.huge_lines(THRESHOLD), Some(vec![(1, 70 * 1024)]));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn huge_line_without_trailing_newline() {
    // 行 1 超长且文件不以换行结尾（最后一行）
    let mut data = Vec::new();
    data.extend_from_slice(b"x\n");
    data.extend_from_slice(&vec![b'b'; 80 * 1024]);
    let (e, p) = open(&data);
    assert_eq!(e.huge_lines(THRESHOLD), Some(vec![(1, 80 * 1024)]));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn no_huge_lines_returns_empty_ok() {
    // 无超长行 → O(1) 短路，空列表
    let (e, p) = open(b"a\nb\nc\n");
    assert_eq!(e.huge_lines(THRESHOLD), Some(Vec::new()));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn huge_line_in_later_sparse_window() {
    // 前面塞 >128 行短行，把超长行挤到后面的稀疏窗口（SPARSE_FACTOR=128），
    // 验证窗口定向扫描的行号与跨窗口偏移都正确。
    let mut data = Vec::new();
    for i in 0..200 {
        data.extend_from_slice(format!("line-{i}\n").as_bytes());
    }
    data.extend_from_slice(&vec![b'c'; 100 * 1024]);
    data.push(b'\n');
    data.extend_from_slice(b"tail\n");
    let (e, p) = open(&data);
    assert_eq!(e.huge_lines(THRESHOLD), Some(vec![(200, 100 * 1024)]));
    let _ = std::fs::remove_file(&p);
}
