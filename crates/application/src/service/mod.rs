//! qview application 服务层（架构文档 §5.2.1 / §6）。
//!
//! 把 `qview_core::Engine` 包装为类型化 handle：
//! - `DocumentService`：DocumentId ↔ Engine 实例
//! - `SearchService`：分页 / 截断 / 脱敏的搜索接口
//! - `AnnotationService`：批注读写（落盘 AnnotationStore）
//! - `PathBlacklist`：系统目录黑名单（器灵不得打开 / 写入 / 列出）

pub mod access;
pub mod annotation;
pub mod document;
pub mod search;

pub use access::{PathBlacklist, DEFAULT_BLACKLIST};
pub use annotation::AnnotationService;
pub use document::DocumentService;
pub use search::{SearchHit, SearchService, SearchSummary};
