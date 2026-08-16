//! DocumentService：把 `qview_core::Engine` 包装为 DocumentId-keyed handle。
//!
//! 关键约束：
//! - **禁止**让工具接受 path 自由输入；只暴露 DocumentId。
//! - 文档大小超过策略上限 → 拒绝打开（架构 §11.2）。
//! - 所有只读 API 都返回不可变引用 / clone 的字符串，**不**让工具
//!   直接拿走 `Engine` 的所有权。
//!
//! ## 锁选择
//! 用 `parking_lot::Mutex<Engine>`（共享 Engine 后 GUI 主线程是**同步**渲染，不能 await）：
//! - `Engine` 含 `std::sync::mpsc::Receiver`（**不** `Sync`），所以
//!   `parking_lot::RwLock<Engine>: Sync` 不成立 → 不能用 RwLock。
//! - `parking_lot::Mutex<T>: Sync` 只需 `T: Send`；`Engine: Send` 成立。
//! - 渲染线程 `.lock()`（无竞争 ~20ns，每帧 1 次）；Agent 工具也是短临界区 `.lock()`。
//!
//! **纪律**：`parking_lot::MutexGuard` 非 Send → 工具内**严禁跨 `.await` 持锁**。
//! 读工具（read_context / search_text / inspect_matches / summarize_range / info）
//! 都是同步调用，天然满足；`SearchService::search` 已显式在轮询循环里 drop guard。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use parking_lot::{Mutex, RwLock as PLRwLock};

use qview_core::config::EngineConfig;
use qview_core::engine::Engine;

use crate::protocol::{DocumentId, PermissionPolicy};
use crate::service::access::PathBlacklist;

/// 文档元信息（get_document_info 工具的输出骨架）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentInfo {
    pub id: DocumentId,
    pub path: String,
    pub size_bytes: u64,
    pub total_lines: u64,
    pub is_indexed: bool,
    /// `total_lines` 是否为估算值（后台索引未完成时按 `文件字节数 / 80` 粗估；
    /// 索引完成后为精确值）。工具描述 / 提示词要求 LLM 在估算时如实说明。
    pub line_count_estimated: bool,
    pub encoding: String,
    pub modified: bool,
}

/// 文档服务：管理 DocumentId ↔ Engine 实例的映射。
pub struct DocumentService {
    /// 下一个 DocumentId。
    next_id: PLRwLock<u64>,
    /// 已打开文档。
    docs: PLRwLock<HashMap<DocumentId, Arc<Mutex<Engine>>>>,
    /// 路径 → DocumentId 反向索引（防止重复打开）。
    by_path: PLRwLock<HashMap<PathBuf, DocumentId>>,
    /// 当前权限策略（运行时可热替换）。
    policy: PLRwLock<PermissionPolicy>,
    /// 系统目录黑名单（器灵不得打开 / 写入；运行时可热替换）。
    blacklist: PLRwLock<Arc<PathBlacklist>>,
    /// Agent 侧新建 Engine 时使用的配置（GUI 注入 `config.engine`，
    /// 保证与主视图共享同一 `index_dir` 缓存；缺省 `EngineConfig::default()`）。
    engine_config: PLRwLock<EngineConfig>,
}

impl std::fmt::Debug for DocumentService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentService")
            .field("open_count", &self.docs.read().len())
            .finish()
    }
}

impl Default for DocumentService {
    fn default() -> Self {
        Self::new(PermissionPolicy::default())
    }
}

impl DocumentService {
    /// 用默认策略构造（含默认系统目录黑名单）。
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            next_id: PLRwLock::new(1),
            docs: PLRwLock::new(HashMap::new()),
            by_path: PLRwLock::new(HashMap::new()),
            policy: PLRwLock::new(policy),
            blacklist: PLRwLock::new(PathBlacklist::default()),
            engine_config: PLRwLock::new(EngineConfig::default()),
        }
    }

    /// 用自定义黑名单构造（用于配置化加载）。
    pub fn with_blacklist(policy: PermissionPolicy, blacklist: Arc<PathBlacklist>) -> Self {
        Self {
            next_id: PLRwLock::new(1),
            docs: PLRwLock::new(HashMap::new()),
            by_path: PLRwLock::new(HashMap::new()),
            policy: PLRwLock::new(policy),
            blacklist: PLRwLock::new(blacklist),
            engine_config: PLRwLock::new(EngineConfig::default()),
        }
    }

    /// 设置 Agent 新建 Engine 时使用的配置（GUI 注入 `config.engine`，
    /// 使缓存目录与主视图一致，命中同一个 `.qli`）。
    pub fn set_engine_config(&self, config: EngineConfig) {
        *self.engine_config.write() = config;
    }

    /// 当前 Engine 配置快照。
    pub fn engine_config(&self) -> EngineConfig {
        self.engine_config.read().clone()
    }

    /// 替换策略（线程安全）。
    pub fn set_policy(&self, policy: PermissionPolicy) {
        *self.policy.write() = policy;
    }

    /// 当前策略快照。
    pub fn policy(&self) -> PermissionPolicy {
        self.policy.read().clone()
    }

    /// 替换黑名单（线程安全；热加载配置用）。
    pub fn set_blacklist(&self, blacklist: Arc<PathBlacklist>) {
        *self.blacklist.write() = blacklist;
    }

    /// 当前黑名单引用。
    pub fn blacklist(&self) -> Arc<PathBlacklist> {
        self.blacklist.read().clone()
    }

    /// 判断 path 是否命中系统目录黑名单；命中返回命中的规则原文。
    /// 供写 / 目录工具做前置拦截（读工具只能拿到 doc_id，天然被 open 挡住）。
    pub fn is_blocked(&self, path: &std::path::Path) -> Option<String> {
        self.blacklist.read().is_blocked(path).map(str::to_string)
    }

    /// 查询路径是否已在文档列表（**不创建 / 不 mmap Engine**）。返回 DocumentId。
    pub fn lookup(&self, path: &std::path::Path) -> Option<DocumentId> {
        let canonical = self.canonical(path.to_path_buf()).ok()?;
        self.by_path.read().get(&canonical).copied()
    }

    /// 打开一个文档，返回 DocumentId。
    ///
    /// **仅 CLI / MCP / 独立打开兜底**（没有 GUI 共享 Engine 的场景）。
    /// 不限制文件大小：Engine mmap 按需分页，内存与文件大小无关。
    /// **幂等**：路径已在文档列表 → 直接返回已有 id，不重复 mmap / 新建 Engine。
    ///
    /// 使用 `set_engine_config` 注入的配置（缺省 `EngineConfig::default()`），
    /// 让 Agent 侧引擎与主视图共享同一个 `index_dir` 缓存：命中 `.qli` 时
    /// 立即得到精确行数与 O(1) 读行，不再退化为估算 + 线性扫描。
    /// 缓存未命中（首次打开大文件）时提交后台索引并派线程轮询至完成，
    /// 避免索引永远停在「未完成」态。
    pub fn open(&self, path: PathBuf) -> anyhow::Result<DocumentId> {
        let canonical = self.canonical(path)?;
        if let Some(id) = self.by_path.read().get(&canonical).copied() {
            return Ok(id);
        }

        let engine = Engine::with_config(canonical.clone(), self.engine_config())
            .with_context(|| format!("open engine for {}", canonical.display()))?;
        let arc = Arc::new(Mutex::new(engine));

        let id = self.register(arc.clone(), canonical)?;

        // 大文件缓存未命中：后台索引器已随 Engine 启动，但 Agent 侧没有 GUI
        // 逐帧 poll 的机制 —— 这里补一个轮询线程，直到索引完成（`total_lines`
        // 变精确、读行走稀疏索引快路径）。
        {
            let mut e = arc.lock();
            if !e.index.is_complete() {
                e.submit_build_index();
            }
        }
        if !arc.lock().index.is_complete() {
            spawn_index_poller(arc);
        }
        Ok(id)
    }

    /// 注册一个已存在的 Engine 实例（GUI 打开时传入自己的 Engine），返回 DocumentId。
    ///
    /// - **不重新打开文件**：Engine 已由 GUI 打开，Agent 直接共享同一实例。
    /// - 同一路径重复注册 → 返回已有 id（幂等）。
    /// - Engine 生命周期由调用方（GUI）持有；这里只存 `Arc` 引用。
    pub fn register(
        &self,
        engine: Arc<Mutex<Engine>>,
        path: PathBuf,
    ) -> anyhow::Result<DocumentId> {
        let canonical = self.canonical(path)?;

        // 命中反向索引：同一路径已有文档，分两种情况——
        // - 传入的是**同一** Arc（GUI 多处重复注册共享 Engine）→ 幂等返回。
        // - 传入的是**不同**实例（Agent 先经 open_document 自建了引擎，GUI 随后打开
        //   同一文件并注册自己的共享 Engine）→ 用新实例**替换**，让 document_id 指向
        //   GUI 主视图那份：行数精确、免双 mmap、编辑立即可见。
        if let Some(id) = self.by_path.read().get(&canonical).copied() {
            let same = self
                .docs
                .read()
                .get(&id)
                .map(|a| Arc::ptr_eq(a, &engine))
                .unwrap_or(false);
            if !same {
                self.docs.write().insert(id, engine);
            }
            return Ok(id);
        }

        let id = {
            let mut next = self.next_id.write();
            let id = DocumentId(*next);
            *next += 1;
            id
        };

        self.docs.write().insert(id, engine);
        self.by_path.write().insert(canonical, id);
        Ok(id)
    }

    /// 注销文档（仅删映射；Engine 由调用方 / Arc 持有生命周期）。
    pub fn unregister(&self, id: DocumentId) -> bool {
        let removed = self.docs.write().remove(&id).is_some();
        if removed {
            self.by_path.write().retain(|_, v| *v != id);
        }
        removed
    }

    /// 关闭文档。语义与 `unregister` 相同（Engine 由 Arc 计数释放，mmap 在最后
    /// 一个引用 drop 时解除）；保留旧名以兼容历史调用方。
    pub fn close(&self, id: DocumentId) -> bool {
        self.unregister(id)
    }

    /// canonicalize + 系统目录黑名单拦截。先于反向索引，杜绝热替换后旧 id 复用。
    fn canonical(&self, path: PathBuf) -> anyhow::Result<PathBuf> {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if let Some(rule) = self.blacklist.read().is_blocked(&canonical) {
            anyhow::bail!("path blocked by system blacklist ({rule})");
        }
        Ok(canonical)
    }

    /// 当前打开的文档数量。
    pub fn len(&self) -> usize {
        self.docs.read().len()
    }

    /// 列出当前已注册文档（id, canonical path）。读 `by_path` 反向索引，
    /// 不需要锁 Engine，任意线程可调。
    pub fn list_paths(&self) -> Vec<(DocumentId, PathBuf)> {
        let mut out: Vec<(DocumentId, PathBuf)> = self
            .by_path
            .read()
            .iter()
            .map(|(p, id)| (*id, p.clone()))
            .collect();
        out.sort_by_key(|(id, _)| id.get());
        out
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.docs.read().is_empty()
    }

    /// 取某个文档的 Engine handle。返回 `None` 表示未打开。
    pub fn engine(&self, id: DocumentId) -> Option<Arc<Mutex<Engine>>> {
        self.docs.read().get(&id).cloned()
    }

    /// 读取文档元信息（get_document_info 工具的输入）。
    ///
    /// 注意：`info` 内部需要读 Engine，但因为 Engine 不可 `Sync`，
    /// 这里直接拿 `Mutex<Engine>` 的所有权短暂锁一次（parking_lot，无 await 持锁）。
    pub async fn info(&self, id: DocumentId) -> Option<DocumentInfo> {
        let arc = self.docs.read().get(&id)?.clone();
        let e = arc.lock();
        let path = e.path.clone();
        let is_indexed = e.index.is_complete();
        Some(DocumentInfo {
            id,
            path: path.display().to_string(),
            size_bytes: e.known_size,
            total_lines: e.effective_line_count(),
            is_indexed,
            // 未完成索引时 effective_line_count() 返回 `字节数/80` 的粗估
            line_count_estimated: !is_indexed,
            encoding: e.encoding.name().to_string(),
            modified: e.is_modified(),
        })
    }

    /// 校验 DocumentId 存在；不存在 → 抛错。
    pub async fn require(&self, id: DocumentId) -> anyhow::Result<Arc<Mutex<Engine>>> {
        match self.engine(id) {
            Some(e) => Ok(e),
            None => anyhow::bail!("unknown document: {id}"),
        }
    }
}

/// 后台轮询 Agent 侧引擎的索引进度，直到完成。
///
/// GUI 主视图每帧调 `poll_bg_index`，但 Agent 通过 `DocumentService::open`
/// 新建的引擎无人轮询，索引会一直停在「未完成」态（`effective_line_count`
/// 永远返回估算、读行永远走线性扫描）。这里用独立线程 drain 进度消息，
/// 完成后即退出并释放引擎引用。短临界区 `parking_lot::Mutex` 锁，不跨 await。
fn spawn_index_poller(engine: Arc<Mutex<Engine>>) {
    std::thread::spawn(move || {
        loop {
            let done = {
                let mut e = engine.lock();
                if e.index.is_complete() {
                    true
                } else {
                    let (done, _msg) = e.poll_bg_index();
                    done
                }
            };
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_returns_increasing_ids() {
        let tmp = tempfile_in_tests();
        let svc = DocumentService::default();
        let a = svc.open(tmp.clone()).unwrap();
        let b = svc.open(tmp.clone()).unwrap();
        assert_eq!(a, b);
        let tmp2 = tempfile_in_tests();
        let c = svc.open(tmp2.clone()).unwrap();
        assert_ne!(a, c);
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&tmp2);
    }

    #[tokio::test]
    async fn lookup_and_open_are_idempotent() {
        let tmp = tempfile_in_tests();
        let svc = DocumentService::default();
        // 未打开 → lookup None
        assert!(svc.lookup(&tmp).is_none());
        let id = svc.open(tmp.clone()).unwrap();
        // 打开后 → lookup 命中同一 id
        assert_eq!(svc.lookup(&tmp), Some(id));
        // 重复 open → 同一 id（幂等，不再新建 Engine）
        let id2 = svc.open(tmp.clone()).unwrap();
        assert_eq!(id, id2);
        let _ = std::fs::remove_file(&tmp);
    }

    /// 共享实例：register 同一份 Engine → 任意侧（GUI / Agent）改动同一内容可见；
    /// 不检查大小限制（2GiB 拦截不对 GUI 打开的文件生效）。
    #[tokio::test]
    async fn register_shared_engine_is_single_instance() {
        let tmp = tempfile_in_tests();

        // 用户手动构造 Engine（模拟 GUI open_file），再 register
        let engine = Engine::with_config(
            tmp.clone(),
            qview_core::config::EngineConfig::default(),
        )
        .unwrap();
        let arc: Arc<Mutex<Engine>> = Arc::new(Mutex::new(engine));
        let svc = DocumentService::default();
        let id = svc.register(arc.clone(), tmp.clone()).unwrap();

        // register 幂等：同路径再注册 → 同一个 id
        let id2 = svc.register(arc.clone(), tmp.clone()).unwrap();
        assert_eq!(id, id2);

        // 取回的是**同一个** Arc → 共享
        let got = svc.require(id).await.unwrap();
        assert!(Arc::ptr_eq(&arc, &got), "register 后 engine() 应返回同一实例");

        // GUI 侧写入 → Agent 侧读到（同一实例，无需重新 open）
        {
            let mut e = arc.lock();
            assert!(e.replace_logical_line(0, b"modified line1".to_vec()));
        }
        let e = arc.lock();
        assert_eq!(e.read_line(0).text, "modified line1");
        drop(e);

        // unregister 只删映射，Arc 仍存活（GUI 还持有）
        assert!(svc.unregister(id));
        assert!(svc.require(id).await.is_err(), "unregister 后不可再取");
        assert!(Arc::strong_count(&arc) >= 1, "Engine 由 GUI 持有，不因 unregister 释放");

        let _ = std::fs::remove_file(&tmp);
    }

    /// 同一路径注册**不同**实例（Agent 先 open_document 自建引擎，GUI 后打开共享
    /// 引擎）→ 后者替换前者，document_id 保持不变。同一 Arc 重复注册仍幂等。
    /// 这是「器灵拿到索引未完成 / 行数估算」竞态的回归测试。
    #[tokio::test]
    async fn register_replaces_different_instance_same_path() {
        let tmp = tempfile_in_tests();

        let e1 = Engine::with_config(
            tmp.clone(),
            qview_core::config::EngineConfig::default(),
        )
        .unwrap();
        let a1: Arc<Mutex<Engine>> = Arc::new(Mutex::new(e1));
        let svc = DocumentService::default();
        let id = svc.register(a1.clone(), tmp.clone()).unwrap();

        // 第二份引擎（模拟 GUI 的共享引擎），注册到同一路径
        let e2 = Engine::with_config(
            tmp.clone(),
            qview_core::config::EngineConfig::default(),
        )
        .unwrap();
        let a2: Arc<Mutex<Engine>> = Arc::new(Mutex::new(e2));
        let id2 = svc.register(a2.clone(), tmp.clone()).unwrap();

        assert_eq!(id, id2, "同路径 document_id 应保持不变");
        let got = svc.require(id).await.unwrap();
        assert!(
            Arc::ptr_eq(&got, &a2),
            "同路径新实例应替换旧实例（GUI 共享引擎胜出）"
        );

        // 同一实例重复注册 → 幂等，不替换
        let id3 = svc.register(a2.clone(), tmp.clone()).unwrap();
        assert_eq!(id3, id2);
        let got3 = svc.require(id3).await.unwrap();
        assert!(Arc::ptr_eq(&got3, &a2), "同一 Arc 重复注册应幂等");

        let _ = std::fs::remove_file(&tmp);
    }

    /// open 使用注入的 EngineConfig（index_dir）→ Agent 侧与 GUI 共享同一缓存：
    /// 首次打开后台索引 + 轮询线程补完成；再次打开（新 service 模拟重启）命中
    /// `.qli` 立即精确行数 + is_indexed:true。这是「行数估算错误」的回归测试。
    #[tokio::test]
    async fn open_uses_configured_engine_config_and_loads_cache() {
        use std::time::Instant;

        let tmp = tempfile_in_tests();
        let index_dir = std::env::temp_dir().join(format!(
            "qview-idx-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&index_dir).unwrap();

        // 把 small_file_threshold 设 1，让 18 字节测试文件走「大文件 + 磁盘缓存」路径
        let cfg = qview_core::config::EngineConfig {
            small_file_threshold: 1,
            index_cache_enabled: true,
            index_dir: Some(index_dir.clone()),
            ..qview_core::config::EngineConfig::default()
        };

        // 第一次 open：无缓存 → 后台索引 + 轮询线程，完成后 is_indexed:true
        let svc = DocumentService::default();
        svc.set_engine_config(cfg.clone());
        let id = svc.open(tmp.clone()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let info = loop {
            let i = svc.info(id).await.unwrap();
            if i.is_indexed {
                break i;
            }
            assert!(
                Instant::now() < deadline,
                "后台索引未在 5s 内完成（轮询线程未生效？）"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(info.total_lines, 3, "索引完成后行数应为精确值 3");
        assert!(!info.line_count_estimated);

        // .qli 已写入配置的 index_dir
        let qli_exists = std::fs::read_dir(&index_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".qli"));
        assert!(qli_exists, "应在配置的 index_dir 写入 .qli");

        // 第二次 open（新 service，模拟重启）：命中缓存 → 立即精确行数
        let svc2 = DocumentService::default();
        svc2.set_engine_config(cfg.clone());
        let id2 = svc2.open(tmp.clone()).unwrap();
        let info2 = svc2.info(id2).await.unwrap();
        assert!(info2.is_indexed, "命中 .qli 缓存应立即 is_indexed:true");
        assert_eq!(info2.total_lines, 3);
        assert!(!info2.line_count_estimated);

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&index_dir);
    }

    fn tempfile_in_tests() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("qview-app-test-{}.log", uuid::Uuid::new_v4()));
        std::fs::write(&p, b"line1\nline2\nline3\n").unwrap();
        p
    }
}
