//! AnnotationService：包装 `qview_core::AnnotationStore`。
//!
//! 设计：与 GUI 共用**同一个批注文件**（`<data_dir>/annotations.json`，架构 §9.5），
//! 并**每次操作前从磁盘 reload** —— 因为 GUI 持有自己的内存 store，器灵这边必须
//! 重新读取才能看到用户刚加的批注；写前 reload 也避免用陈旧内存覆盖 GUI 的改动。
//! 文件很小（批注数量级），不在热路径上，reload 开销可忽略（见 [[performance-first]]）。
//!
//! 写操作（add / remove / set_text）需要走 GuardedTool 审批（架构 §6.3）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::json;

use qview_core::annotation::{Annotation, AnnotationStore};

use crate::protocol::DocumentId;
use crate::service::document::DocumentService;

/// 批注服务（线程安全）。
pub struct AnnotationService {
    store: Arc<RwLock<AnnotationStore>>,
    /// 落盘路径（与 GUI 的 `data/annotations.json` 一致）。
    path: PathBuf,
    docs: Arc<DocumentService>,
}

impl std::fmt::Debug for AnnotationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnotationService").finish()
    }
}

impl AnnotationService {
    /// 用默认路径（`<exe>/data/annotations.json`，与 GUI 一致）构造。
    pub fn new(docs: Arc<DocumentService>) -> Self {
        let path = Self::default_path();
        let store = AnnotationStore::load(&path);
        Self {
            store: Arc::new(RwLock::new(store)),
            path,
            docs,
        }
    }

    /// 自定义路径（GUI 传入 `annotation_store_path()` 保证与 GUI 同一文件）。
    pub fn with_path(docs: Arc<DocumentService>, path: PathBuf) -> Self {
        let store = AnnotationStore::load(&path);
        Self {
            store: Arc::new(RwLock::new(store)),
            path,
            docs,
        }
    }

    /// 落盘路径（默认）：`QVIEW_DATA_DIR` 覆盖，否则 `exe 同目录/data/annotations.json`。
    ///
    /// 与 GUI 的 `annotation_store_path()`（`{exe_dir}/data/annotations.json`）一致，
    /// 保证器灵 / MCP / CLI 与 GUI 读写同一个批注文件。
    pub fn default_path() -> PathBuf {
        let base = std::env::var("QVIEW_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("data")
            });
        base.join("annotations.json")
    }

    /// 当前 store 的落盘路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 每次操作前从磁盘 reload：GUI 可能刚写过（用户手动加批注），
    /// 写前 reload 也避免用陈旧内存覆盖 GUI 的改动。
    fn reload(&self) {
        *self.store.write() = AnnotationStore::load(&self.path);
    }

    /// 列出指定文档的全部批注（先 reload，保证看到 GUI 刚写入的）。
    pub async fn list(&self, doc_id: DocumentId) -> Vec<Annotation> {
        let Some(engine) = self.docs.engine(doc_id) else {
            return Vec::new();
        };
        let path = engine.lock().path.clone();
        drop(engine);
        self.reload();
        self.store.read().for_file(&path).to_vec()
    }

    /// 创建批注（写操作 — 由 GuardedTool 包装）。
    pub async fn create(
        &self,
        doc_id: DocumentId,
        start_byte: u64,
        end_byte: u64,
        start_line: u64,
        end_line: u64,
        start_col: usize,
        end_col: usize,
        selected_text: String,
        text: String,
    ) -> Result<u64, String> {
        let engine = self
            .docs
            .engine(doc_id)
            .ok_or_else(|| format!("unknown document: {doc_id}"))?;
        let path = engine.lock().path.clone();
        drop(engine);

        self.reload(); // 写前 reload，避免覆盖 GUI 在内存里已改、已落盘的批注

        let a = Annotation {
            id: 0,
            file_key: String::new(),
            start_byte,
            end_byte,
            start_line,
            end_line,
            start_col,
            end_col,
            selected_text,
            text,
            created_at: now_local_str(),
            color: 0,
            stale: false,
        };
        let id = {
            let mut store = self.store.write();
            store.add(&path, a)
        };
        let _ = self.store.read().save();
        Ok(id)
    }

    /// 移除批注。
    pub async fn remove(&self, doc_id: DocumentId, id: u64) -> Result<bool, String> {
        let engine = self
            .docs
            .engine(doc_id)
            .ok_or_else(|| format!("unknown document: {doc_id}"))?;
        let path = engine.lock().path.clone();
        drop(engine);

        self.reload(); // 写前 reload

        let removed = {
            let mut store = self.store.write();
            store.remove(&path, id)
        };
        if removed {
            let _ = self.store.read().save();
        }
        Ok(removed)
    }

    /// 修改批注文本。
    pub async fn set_text(&self, doc_id: DocumentId, id: u64, text: String) -> Result<bool, String> {
        let engine = self
            .docs
            .engine(doc_id)
            .ok_or_else(|| format!("unknown document: {doc_id}"))?;
        let path = engine.lock().path.clone();
        drop(engine);

        self.reload(); // 写前 reload

        let changed = {
            let mut store = self.store.write();
            store.set_text(&path, id, text)
        };
        if changed {
            let _ = self.store.read().save();
        }
        Ok(changed)
    }

    /// 当前总批注数。
    pub fn total_count(&self) -> usize {
        // 没公共迭代器，只能 sum-of-files。
        // AnnotationStore 没暴露迭代；这里用 serde 解析路径再 count 是浪费；
        // 暂返回 0 — UI 端若要统计可走 list per file。
        0
    }

    /// 输出 JSON 摘要（导出报告工具用）。
    pub fn snapshot_json(&self) -> serde_json::Value {
        // AnnotationStore.files 是 private；只能按 known path 调用 for_file。
        // 由于当前 DocumentService 不暴露 path 列表，这里返回空结构。
        // P5 改进：让 AnnotationService 维护 path → annotations 的索引。
        json!({"files": {}, "total": 0})
    }
}

fn now_local_str() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{now}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_sees_external_writes_after_reload() {
        // 回归：GUI 先往共享文件写批注，器灵的 AnnotationService（早已构造）再 list
        // → reload 后必须能看到，否则就是"GUI 有批注、器灵说 0 条"的 bug。
        let path = std::env::temp_dir().join(format!(
            "qview-ann-ext-{}.json",
            uuid::Uuid::new_v4()
        ));
        let docs = Arc::new(DocumentService::default());
        let log = std::env::temp_dir().join(format!("qview-ann-ext-log-{}.log", uuid::Uuid::new_v4()));
        std::fs::write(&log, "line1\nline2\n").unwrap();
        let id = docs.open(log.clone()).unwrap();

        // 器灵侧先构造（内存 store 此刻为空文件）
        let svc = AnnotationService::with_path(docs.clone(), path.clone());
        assert!(svc.list(id).await.is_empty());

        // 模拟 GUI：用另一个 AnnotationStore 往同一文件写一条批注并落盘
        let mut gui_store = AnnotationStore::load(&path);
        gui_store.add(
            &log,
            Annotation {
                id: 0,
                file_key: String::new(),
                start_byte: 0,
                end_byte: 5,
                start_line: 0,
                end_line: 1,
                start_col: 0,
                end_col: 5,
                selected_text: "line1".into(),
                text: "GUI 手动加的批注".into(),
                created_at: String::new(),
                color: 0,
                stale: false,
            },
        );
        gui_store.save().expect("外部写入的批注应能保存");

        // 器灵再 list → reload 后应看到外部写入
        let list = svc.list(id).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].text, "GUI 手动加的批注");

        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn create_and_list_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "qview-ann-{}.json",
            uuid::Uuid::new_v4()
        ));
        let docs = Arc::new(DocumentService::default());
        let log = std::env::temp_dir().join(format!("qview-ann-log-{}.log", uuid::Uuid::new_v4()));
        std::fs::write(&log, "line1\nline2\nline3\n").unwrap();
        let id = docs.open(log.clone()).unwrap();

        let svc = AnnotationService::with_path(docs.clone(), path.clone());
        let ann_id = svc
            .create(
                id,
                0, 6, 0, 1, 0, 5,
                "line1".into(),
                "test note".into(),
            )
            .await
            .unwrap();
        let list = svc.list(id).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, ann_id);
        assert_eq!(list[0].text, "test note");

        // 持久化：重新加载
        let svc2 = AnnotationService::with_path(docs.clone(), path.clone());
        let list2 = svc2.list(id).await;
        assert_eq!(list2.len(), 1);

        let _ = svc.remove(id, ann_id).await;
        assert_eq!(svc.list(id).await.len(), 0);

        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&path);
    }
}
