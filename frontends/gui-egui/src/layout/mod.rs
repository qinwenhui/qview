//! 排版度量模块 —— 统一的「格子系统」。
//!
//! 视口被抽象成等宽格子（cell/grid）：格宽 = ASCII 字符宽，格高 = 行高，
//! 由字号 + 字体决定（`metrics::CharMetrics`）。CJK / emoji 等全宽字符占 2 格。
//! 换行 = 格子在宽度方向排满就折行（`row_cache::HugeLayout` 缓存超长行每个视觉
//! 行的元数据）。字节 / 字符 / 视觉列 / 视觉行 / 像素之间的换算**只允许**通过
//! 本模块进行 —— 顶层（viewer / editor / app）不再各自实现坐标换算，杜绝
//! 「字节 vs 字符 vs 视觉列」三套坐标互不相通导致的偏移类 bug。
//!
//! 本模块对外是完整坐标 API 全集，顶层按需选用；部分换算方法当前未被调用方
//! 使用是「预留」而非死代码（避免模块随使用情况收缩导致换算逻辑散回顶层）。
#![allow(dead_code)]

pub mod mapping;
pub mod metrics;
pub mod row_cache;

pub use mapping::{ViewMapping, VisualRowModel, CHUNK_LINE_BYTES};
pub use metrics::CharMetrics;
pub use row_cache::HugeLayout;
