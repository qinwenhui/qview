//! SearchService：把 `qview_core::Engine` 的搜索能力包装为**独立的**类型化接口。
//!
//! 关键约束：
//! - **不触碰引擎的交互式搜索槽**：通过 `Engine::spawn_search` 每次启动一个
//!   独立 worker + 独立结果，GUI 的搜索状态（结果 / 进度 / 高亮）不受影响。
//! - **并发闸**：同时只允许一个 agent 搜索在跑（`Semaphore(1)`）。否则并发搜索
//!   各自全量扫描大文件 → 磁盘 I/O 争抢、互相拖慢，甚至全部超时。
//! - 同步等待搜索完成（小文件即时；大文件最多 `wait_timeout` 时间，超时错误里
//!   带扫描进度，方便上层决定是缩小范围还是重试）。
//! - 输出截断 + `truncated` 标记（架构 §6.2 #6）。
//! - 不暴露 raw mmap 给上层。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde::Serialize;

use qview_core::search::{BlockIndex, SearchOptions, SearchProgress};

use crate::protocol::DocumentId;
use crate::service::document::DocumentService;

/// 单条搜索命中（line-based）。
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// 命中的行号（0-based）。
    pub line: u64,
    /// 该行的原文（截断到 4 KiB）。
    pub text: String,
}

/// 搜索结果汇总。
#[derive(Debug, Clone, Serialize)]
pub struct SearchSummary {
    /// 命中总数。
    pub total: u64,
    /// 实际返回的命中数（已截断到 `limit`）。
    pub returned: usize,
    /// 是否截断。
    pub truncated: bool,
    /// 实际耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 命中列表。
    pub hits: Vec<SearchHit>,
}

/// 搜索服务（与 DocumentService 配合使用）。
#[derive(Clone)]
pub struct SearchService {
    docs: Arc<DocumentService>,
    /// 单次同步等待超时（默认 120s：一次大文件全量扫描可达几十秒）。
    wait_timeout: Duration,
    /// 并发闸：同一时刻只跑一个 agent 搜索。排队等待**不计入** `wait_timeout`
    /// （超时从真正开始扫描算起）。
    gate: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for SearchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchService")
            .field("wait_timeout_ms", &self.wait_timeout.as_millis())
            .field("gate_permits", &self.gate.available_permits())
            .finish()
    }
}

impl SearchService {
    /// 用默认超时构造。
    pub fn new(docs: Arc<DocumentService>) -> Self {
        Self {
            docs,
            wait_timeout: Duration::from_secs(120),
            gate: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// 自定义同步等待超时。
    pub fn with_wait_timeout(mut self, t: Duration) -> Self {
        self.wait_timeout = t;
        self
    }

    /// 异步执行一次搜索。
    ///
    /// **每次搜索完全独立**：`Engine::spawn_search` 启动自己的 worker 和结果
    /// （`BlockIndex`），不占用、不污染引擎的交互式搜索槽（GUI 搜索状态无感），
    /// 并发调用互不干扰。轮询只读**自己**的 worker 通道，全程不跨 await 持锁。
    pub async fn search(
        &self,
        document_id: DocumentId,
        query: &str,
        opts: SearchOptions,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<SearchSummary> {
        let engine = self.docs.require(document_id).await?;

        // 并发闸：等上一个搜索结束再开始。等待期间不持引擎锁、不启动 worker。
        let _permit = self.gate.acquire().await?;

        // 启动独立搜索（短临界区）。`file_bytes` 留给超时消息报扫描进度用。
        let (bg, file_bytes) = {
            let e = engine.lock();
            let file_bytes = e.mmap.size();
            let bg = e
                .spawn_search(query.to_string(), opts)
                .with_context(|| format!("spawn_search({query:?})"))?;
            (bg, file_bytes)
        };

        // 轮询**自己的** worker：poll 只读 mpsc 通道，不持引擎锁。
        let start = std::time::Instant::now();
        let mut index: Option<Arc<BlockIndex>> = None;
        let mut fail: Option<String> = None;
        loop {
            tokio::task::yield_now().await;
            while let Some(p) = bg.poll() {
                match p {
                    SearchProgress::Done(idx) => index = Some(idx),
                    SearchProgress::Failed(e) => fail = Some(e),
                    SearchProgress::Cancelled => fail = Some("cancelled".to_string()),
                    // Started / Percent：仅进度，忽略。
                    _ => {}
                }
            }
            if index.is_some() {
                break;
            }
            if let Some(e) = &fail {
                anyhow::bail!("search failed: {e}");
            }
            if start.elapsed() > self.wait_timeout {
                bg.cancel();
                anyhow::bail!(
                    "search timed out after {}s (scanned {:.1} GiB / {:.1} GiB); \
                     the file is large — narrow the scope or retry",
                    self.wait_timeout.as_secs(),
                    bg.scanned_bytes() as f64 / (1 << 30) as f64,
                    file_bytes as f64 / (1 << 30) as f64,
                );
            }
        }
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let index = index.expect("loop breaks only when index is Some");

        // 结果收集（短临界区；全部同步读，无 await）。命中按全局序号取
        // `offset..offset+limit`，超出总数即停。
        let (total, hits, truncated) = {
            let e = engine.lock();
            let total = index.total_count() as u64;
            let mut hits = Vec::with_capacity(limit.min(total as usize));
            for n in offset..offset + limit {
                let Some(byte) = index.get(n) else {
                    break;
                };
                let line = e.line_of_byte(byte);
                let raw = e.read_line(line);
                let text = if raw.text.len() > 4096 {
                    format!("{}…[+{} chars]", &raw.text[..4096], raw.text.len() - 4096)
                } else {
                    raw.text
                };
                hits.push(SearchHit { line, text });
            }
            let truncated = (offset as u64) + (hits.len() as u64) < total;
            (total, hits, truncated)
        };

        Ok(SearchSummary {
            total,
            returned: hits.len(),
            truncated,
            elapsed_ms,
            hits,
        })
    }
}
