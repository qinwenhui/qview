//! QLogApp — central application state and `eframe::App` implementation.
//! Dispatches rendering to sub-modules (menu, toolbar, statusbar, viewer,
//! dialogs) and handles keyboard shortcuts.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use egui::Context;
use qview_core::annotation::AnnotationStore;
use qview_core::engine::Engine;
use tokio::runtime::Runtime;

use crate::{log_debug, log_error, log_info, log_warn};
use crate::agent::{AgentPanelState, EguiAgentSink};
use crate::config::AppConfig;
use crate::style::Theme;
use crate::theme_data::ThemeColors;
use crate::layout::VisualRowModel;
use crate::viewer::HugeLineCache;

// ---------------------------------------------------------------------------
// QLogApp
// ---------------------------------------------------------------------------

/// What to do once the user confirms discarding an unsaved-modified file.
#[derive(Debug, Clone)]
pub enum DiscardAction {
    Open(std::path::PathBuf),
    New,
    Close,
    Exit,
}

pub struct QLogApp {
    // ---- engine ----
    /// 共享 Engine（GUI 与 Agent 持有同一份 `Arc<parking_lot::Mutex<Engine>>`）。
    /// GUI 渲染每帧短临界区 `.lock()`；Agent 工具同样 `.lock()`。非重入，渲染中禁 re-lock。
    pub engine: Option<Arc<parking_lot::Mutex<Engine>>>,
    pub path: Option<PathBuf>,

    // ---- agent (P3) ----
    /// Agent 面板共享状态。
    pub agent_state: AgentPanelState,
    /// 器灵浮动聊天窗口是否显示（工具栏按钮切换；窗口不阻塞主面板操作）。
    pub show_agent_window: bool,
    /// 设置页 API Key 是否明文显示（密码框切换；会话内记住）。
    pub agent_show_key: bool,
    /// 器灵窗口打开后下一帧自动聚焦输入框（每次打开只聚焦一次）。
    pub agent_focus_input: bool,
    /// 全局 tokio runtime 句柄（用于 spawn Agent runtime 任务）。
    #[allow(dead_code)]
    pub tokio_rt: Option<Arc<Runtime>>,
    /// Agent 装配依赖（DocumentService / SearchService / AnnotationService）。
    /// 首次 `init_agent` 创建后常驻；`rebuild_agent_runtime` 复用同一套服务
    /// （避免重建时重新 mmap 当前文件）。
    pub agent_deps: Option<qview_agent::AgentDeps>,
    /// 本地结构化存储（AI 会话历史 / 文件元数据）。`init_agent` 时构造。
    pub store: Option<Arc<dyn qview_store::Storage>>,
    /// 历史会话对话框是否显示。
    pub show_history: bool,
    /// 历史会话列表缓存（`None` = 未加载 / 加载中）。
    pub history_sessions: Arc<Mutex<Option<Vec<qview_store::SessionMeta>>>>,
    /// 器灵浮动窗口内嵌历史列表视图（true 时消息区换成历史会话列表）。
    pub agent_show_history: bool,
    /// 「⌘ 工具记录」浮层（true 时消息区上盖半透明浮层显示当前会话工具记录）。
    pub agent_show_tool_log: bool,
    /// 器灵窗口当前位置（嵌主窗口时仅顶条可拖动；`None` = 用默认位置）。
    pub agent_area_pos: Option<egui::Pos2>,
    /// 最近打开文件缓存（菜单「最近打开」读取；来源 = store `files` 表，top 10）。
    /// 独立于 `config.recent_files`（那是迁移前的遗留字段）。
    pub recent_files: Arc<Mutex<Vec<PathBuf>>>,
    /// 搜索历史缓存（top 20；来源 = store `search_history` 表）。
    /// 独立于 `config.search_history`（遗留字段）。
    pub search_history: Arc<Mutex<Vec<String>>>,
    /// UI 每帧发布的视口快照（`get_viewport` 工具读）。主线程写、agent 后台读。
    pub viewport_info: qview_application::tools::SharedViewport,
    /// 当前文件在 Agent DocumentService 里的 id（`open_file` 注册、`close_file` 注销）。
    pub agent_doc_id: Option<qview_application::protocol::DocumentId>,
    /// Agent 高亮范围（ViewIntent::HighlightRange 累积，终态清空）。
    /// `(start, end, kind)` — viewer 绘制左侧色条。
    pub agent_highlights: Vec<(u64, u64, qview_application::protocol::view_intent::HighlightKind)>,
    /// Agent 视图过滤器（ViewIntent::ApplyFilter；点击时间线条目才应用）。
    pub agent_filter: Option<qview_application::protocol::view_intent::FilterSpec>,
    /// 调试：上一次记录的器灵窗口 / 顶条 / 内容最小矩形（值变化才打日志，用于排查
    /// "窗口被内容撑大 / 顶条没贴满宽度 / 内容超出背景"。排查完可删）。
    pub debug_agent_win_rect: Option<egui::Rect>,
    pub debug_agent_header_rect: Option<egui::Rect>,
    pub debug_agent_content_rect: Option<egui::Rect>,
    /// 字体定义（子视口需要单独 `set_fonts`；字体字节是 Arc 共享，克隆廉价）。
    pub font_defs: Option<egui::FontDefinitions>,
    /// 器灵子视口是否已设过位置/尺寸（避免每帧 `with_position` 把窗口钉死）。
    pub agent_viewport_pos_set: bool,
    /// 器灵子窗口的原生 HWND（0=未找到）。用于给 OS 窗口设圆角区域。
    pub agent_hwnd: isize,
    /// 上一次给器灵窗口设置的圆角区域（物理 w,h,radius；(-1,-1,-1)=尚未设置）。
    pub agent_round_region: (i32, i32, i32),

    // ---- search ----
    pub search_input: String,
    pub search_hits: Vec<u64>,      // sparse samples only
    pub search_total_count: usize,  // actual total hit count
    pub search_hit_idx: usize,
    pub search_status: String,
    pub search_lines: Vec<u64>,
    pub search_query: String,
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub whole_word: bool,

    // ---- navigation ----
    pub scroll_y: f64,
    pub h_scroll: f64,
    pub max_content_w: f64,
    pub scrollbar_dragging: bool,
    /// Stable word-wrap row-height multiplier — computed once per frame
    /// from viewport width by the viewer.  Used in jump_hit, goto_line, etc.
    pub wrap_height_mult: f64,
    /// Set once per file: exact maximum pixel width of the widest line,
    /// measured by scanning every line with the current font (small files only).
    pub full_width_scan_done: bool,
    /// When background indexing started — used to log the build duration.
    pub index_started_at: Option<std::time::Instant>,
    /// When the last search was submitted — used to log the search duration.
    pub search_started_at: Option<std::time::Instant>,
    /// Last query for which the viewer logged multi-line highlight diagnostics.
    pub last_hl_query: String,
    /// Cached parsed search query for the viewer's match highlighting (regex /
    /// case-insensitive / whole-word aware). Rebuilt when `last_hl_query`
    /// changes; `None` when there is no search or the query doesn't parse.
    pub parsed_search_q: Option<qview_core::search::Query>,

    // ---- text selection ----
    pub selection: Option<(u64, usize, u64, usize)>, // (start_line, start_col, end_line, end_col)
    pub pointer_was_down: bool,  // previous-frame pointer state for just-pressed detection
    /// True while a selection drag is active (press inside content, still held).
    /// Lets the viewer extend the selection even when the pointer leaves the
    /// content area (auto-scroll at the edges).
    pub selecting: bool,
    pub pending_copy_text: Option<String>, // deferred copy (overrides TextEdit's copy at end of frame)

    // ---- visible range (set by viewer each frame, read by jump_hit) ----
    pub first_visible_line: u64,
    pub last_visible_line: u64,

    // ---- 超长行视觉行模型（viewer 每帧构建，app 侧跳转/滚动复用） ----
    /// 超长行列表：(物理行, 原始字节长度)，懒构建（文件内容变化时失效重建）。
    pub huge_lines: Vec<(u64, u64)>,
    /// 构建 key：(effective_line_count, mmap.size)，用于失效检测。
    pub huge_lines_built: Option<(u64, u64)>,
    /// 超长行分块缓存（LRU，预算 32 MiB）。
    pub huge_chunk_cache: Vec<HugeLineCache>,
    /// 编辑器修改过超长行 → 置 true，viewer 下一帧清缓存重建（否则 cache.text 是
    /// 旧快照，渲染/点击的 col 与编辑器 read_line 的当前文本错位 → 插入偏移）。
    pub huge_cache_dirty: std::cell::Cell<bool>,
    /// 每帧构建的视觉行模型。
    pub visual_model: Option<VisualRowModel>,

    // ---- UI state ----
    pub status_msg: String,
    pub status_msg_until: Option<std::time::Instant>,
    pub goto_input: String,

    // ---- display settings (runtime) ----
    pub font_size: f32,
    pub row_h: f64,
    pub show_line_numbers: bool,
    pub level_coloring: bool,
    pub word_wrap: bool,
    pub show_whitespace: bool,
    pub show_indent_guides: bool,

    // ---- font management ----
    pub available_fonts: Vec<String>,
    pub selected_font: usize,

    // ---- themes ----
    pub themes: Vec<Theme>,
    pub current_theme_idx: usize,

    // ---- dialogs ----
    pub show_about: bool,
    pub show_donate: bool,
    pub show_help: bool,
    pub show_shortcuts: bool,
    pub show_settings: bool,
    pub show_file_properties: bool,
    pub show_index_manager: bool,
    pub show_encoding_confirm: bool,  // "切换编码并重新加载?" confirmation
    pub pending_encoding: String,     // encoding to switch to when confirmed
    pub settings_tab: usize,

    // ---- annotations (批注) ----
    pub annotation_store: AnnotationStore,
    /// Current file's annotations, file order (rebuilt by `reload_annotations`).
    pub annotations: Vec<qview_core::annotation::Annotation>,
    /// Line numbers with a visible marker (one per annotation, its start line).
    pub annotated_lines: HashSet<u64>,
    /// "添加/编辑批注" input window.
    pub show_annotation_dialog: bool,
    /// Annotation list panel.
    pub show_annotation_list: bool,
    /// TextEdit buffer for the note body.
    pub annotation_input: String,
    /// `Some(id)` = editing an existing annotation, `None` = new one.
    pub annotation_edit_id: Option<u64>,
    /// Currently highlighted entry in the annotation list.
    pub annotation_selected_id: Option<u64>,

    // ---- edit mode ----
    /// Editing is disabled by default (pure preview); flipped by the toolbar.
    pub edit_mode: bool,
    /// Caret position (line, char col) while editing. `None` outside edit mode.
    pub edit_cursor: Option<(u64, usize)>,
    /// 编辑模式下的 IME 组合（preedit / marked text）串；空串表示无组合。
    /// 中文输入法组合期间，egui-winit 只通过 `Event::Ime(ImeEvent::Preedit)`
    /// 送达组合串，编辑器用它绘制下划线标记文本，`Commit` 时才真正插入。
    pub edit_ime_preedit: String,
    /// A background save (in-place or save-as) is in flight.
    pub edit_saving: bool,
    /// The last background save's target path (for the "已另存为" message).
    pub last_save_path: Option<PathBuf>,
    /// Open the save-as file dialog next frame.
    pub save_as_requested: bool,
    /// Confirm before discarding a modified file (close / open another / exit).
    pub pending_discard: Option<DiscardAction>,
    /// An in-place save just finished; re-anchor annotations once the background
    /// index for the NEW file content is ready (line numbers need a fresh index).
    pub pending_reanchor: bool,
    /// An unsaved NEW file is open: `path` is a temp backing file. Saving it
    /// prompts for a destination, then the working file switches to that path
    /// (there is no 另存为 while in this state). `true` also makes the recent
    /// list skip the temp path.
    pub is_new_file: bool,
    /// Set when the user confirmed an exit — stop intercepting close.
    pub exit_requested: bool,
    pub exit_confirmed: bool,

    // ---- config ----
    pub config: AppConfig,

    // ---- diagnostics ----
    pub boot_instant: Option<std::time::Instant>,
    pub mem_snapshot_taken_2s: bool,
    pub mem_snapshot_taken_5s: bool,
}

impl Default for QLogApp {
    fn default() -> Self {
        let config = AppConfig::load();
        // 原始 LLM 请求日志按配置开关应用（默认关；设置面板 → AI 可实时开）。
        config.apply_llm_raw_log();
        let themes = Theme::all_builtin();
        let annotation_store =
            AnnotationStore::load(&Self::annotation_store_path());
        let theme_idx = themes
            .iter()
            .position(|t| t.name == config.gui.theme)
            .unwrap_or(0);

        Self {
            engine: None,
            path: None,

            // ---- agent (P3) ----
            agent_state: crate::agent::AgentPanelState::default(),
            show_agent_window: false,
            agent_show_key: false,
            agent_focus_input: false,
            tokio_rt: None,
            agent_deps: None,
            // 本地结构化存储（会话历史 / 文件元数据）。`init_agent` 时构造。
            store: None,
            show_history: false,
            history_sessions: Arc::new(Mutex::new(None)),
            agent_show_history: false,
            agent_show_tool_log: false,
            agent_area_pos: None,
            recent_files: Arc::new(Mutex::new(Vec::new())),
            search_history: Arc::new(Mutex::new(Vec::new())),
            viewport_info: qview_application::tools::SharedViewport::default(),
            agent_doc_id: None,
            agent_highlights: Vec::new(),
            agent_filter: None,
            debug_agent_win_rect: None,
            debug_agent_header_rect: None,
            debug_agent_content_rect: None,
            font_defs: None,
            agent_viewport_pos_set: false,
            agent_hwnd: 0,
            agent_round_region: (-1, -1, -1),

            search_input: String::new(),
            search_hits: Vec::new(),
            search_total_count: 0,
            search_hit_idx: 0,
            search_status: String::new(),
            search_lines: Vec::new(),
            search_query: String::new(),
            case_sensitive: config.gui.case_sensitive,
            use_regex: config.gui.use_regex,
            whole_word: config.gui.whole_word,

            scroll_y: 0.0,
            h_scroll: 0.0,
            max_content_w: 0.0,
            scrollbar_dragging: false,
            wrap_height_mult: 2.5,
            full_width_scan_done: false,
            index_started_at: None,
            search_started_at: None,
            last_hl_query: String::new(),
            parsed_search_q: None,

            selection: None,
            pointer_was_down: false,
            selecting: false,
            pending_copy_text: None,

            first_visible_line: 0,
            last_visible_line: 0,

            huge_lines: Vec::new(),
            huge_lines_built: None,
            huge_chunk_cache: Vec::new(),
            huge_cache_dirty: std::cell::Cell::new(false),
            visual_model: None,

            status_msg: String::new(),
            status_msg_until: None,
            goto_input: String::new(),

            font_size: config.gui.font_size,
            row_h: config.gui.row_height,
            show_line_numbers: config.gui.show_line_numbers,
            level_coloring: config.gui.level_coloring,
            word_wrap: config.gui.word_wrap,
            show_whitespace: config.gui.show_whitespace,
            show_indent_guides: false,

            available_fonts: vec!["内置等宽".to_string()],
            selected_font: 0,

            themes,
            current_theme_idx: theme_idx,

            show_about: false,
            show_donate: false,
            show_help: false,
            show_shortcuts: false,
            show_settings: false,
            show_file_properties: false,
            show_index_manager: false,
            show_encoding_confirm: false,
            pending_encoding: String::new(),
            settings_tab: 0,

            annotation_store,
            annotations: Vec::new(),
            annotated_lines: HashSet::new(),
            show_annotation_dialog: false,
            show_annotation_list: false,
            annotation_input: String::new(),
            annotation_edit_id: None,
            annotation_selected_id: None,

            edit_mode: false,
            edit_cursor: None,
            edit_ime_preedit: String::new(),
            edit_saving: false,
            last_save_path: None,
            save_as_requested: false,
            pending_discard: None,
            pending_reanchor: false,
            is_new_file: false,
            exit_requested: false,
            exit_confirmed: false,

            config,

            boot_instant: None,
            mem_snapshot_taken_2s: false,
            mem_snapshot_taken_5s: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Setup (called from main.rs before eframe::App::update)
// ---------------------------------------------------------------------------

impl QLogApp {
    /// Initialise fonts, create working directories, and apply the current
    /// theme from config.
    pub fn init_fonts_and_theme(&mut self, ctx: &Context) {
        self.boot_instant = Some(std::time::Instant::now());        let (fonts, discovered) = crate::fonts::discover_fonts();
        log_info!("app", "发现 {} 种字体 (已加载 {} 份字体数据)", discovered.len(), fonts.font_data.len());
        self.available_fonts = discovered;

        // Honor the persisted font choice.  The installer writes
        // `gui.font_family` as the file stem of a bundled font (e.g.
        // "NotoSansSC-VF"), and 设置 also saves the current selection here.
        // Fall back to 0 (first discovered) when the name no longer exists.
        self.selected_font = self
            .available_fonts
            .iter()
            .position(|n| *n == self.config.gui.font_family)
            .unwrap_or(0);
        log_info!(
            "app",
            "默认字体: {} (index={})",
            self.available_fonts
                .get(self.selected_font)
                .map(|s| s.as_str())
                .unwrap_or("?"),
            self.selected_font
        );

        // 存一份供子视口（独立 AI 窗口）复用；字体字节是 Arc 共享，克隆廉价。
        self.font_defs = Some(fonts.clone());
        ctx.set_fonts(fonts);

        // Ensure the engine's index directory exists so the background
        // indexer can write .qli files without errors.
        if let Some(ref dir) = self.config.engine.index_dir {
            let _ = std::fs::create_dir_all(dir);
            log_info!("app", "索引目录: {}", dir.display());
        }

        // Apply the default or configured theme.
        if let Some(theme) = self.themes.get(self.current_theme_idx) {
            log_info!("app", "初始主题: {}", theme.name);
            theme.apply_to(ctx);
        }
    }

    /// Get the colour palette of the currently active theme.
    pub fn current_theme_colors(&self) -> &ThemeColors {
        &self.themes[self.current_theme_idx].colors
    }

    /// 初始化 Agent 运行时（P3）：构造常驻 AgentDeps + 首次装配。
    pub fn init_agent(&mut self, ctx: Context, rt: Arc<Runtime>) {
        // 存下全局 runtime：egui 事件循环线程不在任何 tokio 上下文里，
        // 后面所有后台任务都必须走 `spawn_tokio`（Runtime::spawn 任意线程可用）。
        self.tokio_rt = Some(rt);

        // 本地结构化存储（会话历史 / 文件元数据）。损坏自动回退 NullStore，绝不阻塞启动。
        if self.store.is_none() {
            match self.config.store_path() {
                Some(p) => {
                    let display = p.display().to_string();
                    self.store = Some(qview_store::open_store_or_null(p));
                    log_info!("agent", "本地存储已打开: {}", display);
                }
                None => {
                    self.store = Some(Arc::new(qview_store::NullStore));
                }
            }
        }
        // 最近打开 / 搜索历史：旧 config.json 里的遗留数据一次性迁入 store 后清空，
        // 再把 store 数据载入内存缓存（菜单「最近打开」启动即可用，无空窗）。
        self.migrate_legacy_recents();
        self.reload_recent_files();
        self.reload_search_history();

        if self.agent_deps.is_none() {
            // qview-application 服务（UI 自己的依赖）— 常驻，重建时复用
            let docs = Arc::new(qview_application::service::DocumentService::default());
            // Agent 侧 open_document 新建 Engine 时复用主视图的引擎配置
            // （index_dir 等），使两者命中同一个 `.qli` 缓存 → 行数精确、读行秒级。
            docs.set_engine_config(self.config.engine.clone());
            let search = Arc::new(qview_application::service::SearchService::new(docs.clone()));
            // 与 GUI 自己的 annotation_store 共用同一文件（data/annotations.json），
            // 否则器灵读到的是另一份空文件，永远看不到 GUI 里已有的批注。
            let ann = Arc::new(
                qview_application::service::annotation::AnnotationService::with_path(
                    docs.clone(),
                    Self::annotation_store_path(),
                ),
            );
            self.agent_deps = Some(qview_agent::AgentDeps {
                docs,
                search,
                annotations: ann,
                viewport: self.viewport_info.clone(),
                store: self.store.clone(),
            });
        }
        self.rebuild_agent_runtime(&ctx);
    }

    /// 首次运行把内置默认系统提示词写入外部文件（仅当文件不存在；用户已编辑的不覆盖）。
    /// 用**静态**部分 seed（不含动态「当前会话策略」——加载时由 resolve_system_prompt 自动追加，
    /// 避免重复）。之后用户直接编辑 `data/system_prompt.md` 测试，无需重新编译。
    fn seed_prompt_file(path: &std::path::Path) {
        if path.exists() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let content = qview_agent::runtime::static_system_prompt();
        match std::fs::write(path, content) {
            Ok(_) => log_info!("agent", "已生成系统提示词文件: {}", path.display()),
            Err(e) => log_warn!("agent", "生成系统提示词文件失败: {e}"),
        }
    }

    /// 用当前 `config.agent` 重建 Agent 运行时（设置面板"AI"选项卡改动后调用）。
    ///
    /// - 取消进行中的 session
    /// - 复用常驻 AgentDeps（当前打开文件已注册，无需重新 mmap）
    /// - 重新装配 LLM / 工具 / 权限 / 审计 → 新 handle + 新 sink 订阅
    pub fn rebuild_agent_runtime(&mut self, ctx: &Context) {
        use std::sync::Arc;

        // 0) 取消进行中的 session（旧 handle 即将被替换）
        if let Some(sid) = self.agent_state.active_session.lock().clone() {
            if let Some(h) = self.agent_state.handle.lock().clone() {
                let h2 = h.clone();
                self.spawn_tokio(async move {
                    let _ = h2.cancel_within(sid, Duration::from_secs(1)).await;
                });
            }
        }

        let Some(deps) = self.agent_deps.clone() else {
            log_error!("agent", "rebuild_agent_runtime: agent_deps 未初始化");
            return;
        };

        // 1) 从 AppConfig 派生 AgentConfig；allow_tools 为空 → 放开全部（本地安全默认）
        let mut agent_config = self.config.agent.clone();
        if agent_config.allow_tools.is_empty() {
            agent_config.allow_all_tools();
        }
        if agent_config.instance_id.is_empty() {
            agent_config.instance_id = "qview-agent-egui".into();
        }
        // 系统提示词外部文件（可编辑测试）：缺省 `{config_dir}/system_prompt.md`。
        // 首次运行自动 seed（已存在不覆盖）；之后直接改这个 md 文件 → 重启 / 重建
        // 即生效，无需重新编译。加载时缺失 / 为空自动回退内置默认。
        if agent_config.system_prompt_file.is_none() {
            if let Some(dir) = crate::config::AppConfig::config_dir() {
                let p = dir.join("system_prompt.md");
                Self::seed_prompt_file(&p);
                agent_config.system_prompt_file = Some(p);
            }
        }
        // 审批策略：只对「写盘」类副作用审批（导出报告 = 保存文件），其余一律自动放行。
        // 显式覆盖旧配置里可能残留的 [Reversible, Mutating, Destructive]，
        // 让 annotate_create 等常规操作不再等审批（用户要求）。
        agent_config.require_approval = vec![
            qview_application::protocol::SideEffect::Mutating,
            qview_application::protocol::SideEffect::Destructive,
        ];

        // 2) 一站式装配：LLM / 工具 / 权限 / 审计 / Worker / Runtime
        let provider = agent_config.provider.provider;
        let handle = match agent_config.build(deps) {
            Ok(h) => h,
            Err(e) => {
                log_error!("agent", "AgentConfig::build 失败: {e:#}");
                return;
            }
        };
        self.agent_state.set_handle(handle.clone());

        // 3) 订阅 sink（Weak 订阅语义：必须保留 sink 强引用）
        let sink = Arc::new(EguiAgentSink::new(
            self.agent_state.events.clone(),
            ctx.clone(),
        ));
        let _guard = handle.subscribe(sink.clone());
        self.agent_state.sink_keepalive = Some(sink);

        // 4) 若当前已打开文件但未注册到新 DocumentService，补注册（复用共享 Engine，
        //    不重新 open）
        if self.agent_doc_id.is_none() {
            let engine_arc = self.engine.as_ref().cloned();
            if let (Some(deps), Some(arc), Some(path)) =
                (self.agent_deps.as_ref(), engine_arc, self.path.clone())
            {
                match deps.docs.register(arc, path) {
                    Ok(id) => {
                        self.agent_doc_id = Some(id);
                        log_info!("agent", "Agent DocumentService 注册当前文件 id={}", id.get());
                    }
                    Err(e) => log_warn!("agent", "Agent 注册当前文件失败: {e:#}"),
                }
            }
        }

        log_info!("agent", "Agent runtime 装配完成 (provider={provider:?})");
    }

    /// 从主线程安全地 spawn 到全局 tokio runtime。
    ///
    /// egui 事件循环线程**不在**任何 tokio runtime 上下文中，直接 `tokio::spawn`
    /// 会 panic（"there is no reactor running"）；而本应用 `#![windows_subsystem
    /// = "windows"]` 没有控制台，panic 消息不可见 —— 表现为"点发送进程直接退出"。
    /// 因此 GUI 侧所有 spawn 一律走 `Runtime::spawn`（任意线程均可调用）。
    pub fn spawn_tokio<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        match &self.tokio_rt {
            Some(rt) => {
                let _ = rt.spawn(fut);
            }
            None => log_error!("agent", "spawn_tokio: tokio_rt 未初始化，任务已丢弃"),
        }
    }

    /// Switch to a theme by name and apply it immediately.
    pub fn switch_theme(&mut self, name: &str, ctx: &Context) {
        if let Some(idx) = self
            .themes
            .iter()
            .position(|t| t.name.to_lowercase().starts_with(&name.to_lowercase()))
        {
            log_debug!("app", "切换主题: {} (index={})", self.themes[idx].name, idx);
            self.current_theme_idx = idx;
            self.config.gui.theme = self.themes[idx].name.clone();
            self.themes[idx].apply_to(ctx);
            self.save_config();
        }
    }

    /// 处理 Agent 事件 + 渲染器灵浮动聊天窗口 / 审批弹窗。
    ///
    /// - 事件流 → 聊天转录（气泡式）＋阶段 / 审批 / 终态状态
    /// - 浮动 `Window` 非模态（不阻塞主面板操作），`Order::Foreground` 置顶
    fn render_agent_panel(&mut self, ctx: &Context) {
        use crate::agent::{project, ChatMsg};

        // 1) 拉取并处理本帧新事件 → 转录 / 状态
        // 注意：事件被 drain（std::mem::take）后**必须消费掉**，不能放回缓冲 ——
        // 否则下一帧会再次 drain → 重新处理（再推一遍气泡 / 再应用一次 intent /
        // 再重置 phase）→ 无限循环刷新。
        let events = self.agent_state.drain_events();
        for e in &events {
            match e {
                qview_agent::event::AgentEvent::PhaseChanged { phase, .. } => {
                    *self.agent_state.current_phase.lock() = *phase;
                }
                qview_agent::event::AgentEvent::SessionStarted { session_id, .. } => {
                    // 立刻标记会话为活动 + 进入 Routing 阶段，让 typing_bubble 在
                    // 意图分类（start_session_with 会阻塞在 IntentRouter::classify）
                    // 期间就显示小Q风格的进度气泡（如「容我想想…」）。否则要等整个
                    // start_session_with 返回、panel.rs 的 send() 才设置
                    // active_session —— 那之前界面一片死寂，用户以为卡死了。
                    // （phase 必须设成非终态：上次会话结束时是 Done，typing_bubble
                    //   对终态直接 return，光设 active_session 也不显示。）
                    *self.agent_state.active_session.lock() = Some(session_id.clone());
                    *self.agent_state.current_phase.lock() = qview_agent::event::Phase::Routing;
                    // 用户气泡已在 send() 记录；这里避免重复。
                    // 新任务开始 → 清掉上一任务的高亮（旧标记不再适用）。
                    // **不再清 tool_log**：多轮对话同一会话复用 id，工具记录要跨轮累积
                    // （新建会话时才清空，见 agent_new_session）。
                    self.agent_highlights.clear();
                }
                qview_agent::event::AgentEvent::MessageEmitted { .. } => {
                    // 模型中间正文**不再入转录**：唯一最终回复 = SessionFinished 的
                    // summary（提示词已要求模型把真正内容写进 summary，assistant 正文
                    // 只放工具调用）。之前两条回复就是"正文 + summary 各写一遍"。
                    // 完整过程（含中间正文/工具行）在「历史会话」里仍可回看。
                }
                qview_agent::event::AgentEvent::ProgressNote { text, .. } => {
                    // 项目经理实时进度交代（report_progress 工具）：普通文本不实时显示，
                    // 只有调 report_progress 才能让用户看到中间进度 → 入转录为 Note。
                    if !text.trim().is_empty() {
                        push_chat(&self.agent_state, ChatMsg::Note { text: text.clone() });
                    }
                }
                qview_agent::event::AgentEvent::ToolCallStarted {
                    tool, input, session_id, ..
                } => {
                    // 工具调用**不**入转录（用户要求：不一行行显示，只要最终回答）。
                    // 只记录在飞工具，messages() 底部的实时气泡据此显示「调用工具 {name} …」。
                    *self.agent_state.in_flight_tool.lock() = Some(tool.clone());
                    let detail = serde_json::to_string(input)
                        .ok()
                        .filter(|s| !s.is_empty() && s != "null")
                        .map(|s| s.chars().take(80).collect::<String>())
                        .unwrap_or_default();
                    log_debug!("agent", "器灵调用工具: {tool} 输入={detail}");
                    // 记录到「工具调用日志」（先占位，Finished 补结果）
                    let input_str = serde_json::to_string(input).unwrap_or_default();
                    let mut log = self.agent_state.tool_log.lock();
                    let seq = log.len() as u64;
                    log.push(qview_store::ToolCallRecord {
                        session_id: session_id.clone(),
                        seq,
                        tool: tool.clone(),
                        input: input_str.chars().take(160).collect(),
                        output: String::new(),
                        duration_ms: 0,
                        is_error: false,
                        at_ms: crate::agent::now_ms(),
                    });
                }
                qview_agent::event::AgentEvent::ToolCallFinished {
                    tool,
                    output_summary,
                    duration_ms,
                    is_error,
                    session_id,
                    ..
                } => {
                    // 工具名用事件自带的（并行调用时共享 in_flight_tool 槽会串名）
                    log_debug!(
                        "agent",
                        "器灵工具完成: {tool} {}ms err={} 结果={}",
                        duration_ms,
                        is_error,
                        output_summary
                    );
                    // 清掉在飞工具气泡（仅当名字匹配 —— 并行调用时另一个还在跑）
                    if self.agent_state.in_flight_tool.lock().as_ref() == Some(tool) {
                        *self.agent_state.in_flight_tool.lock() = None;
                    }
                    // 补全工具调用记录 + 落库（整会话覆盖写，单事务）
                    {
                        let mut log = self.agent_state.tool_log.lock();
                        if let Some(rec) = log
                            .iter_mut()
                            .find(|r| r.tool == *tool && r.output.is_empty())
                        {
                            rec.output = output_summary.clone();
                            rec.duration_ms = *duration_ms;
                            rec.is_error = *is_error;
                            rec.at_ms = crate::agent::now_ms();
                        }
                        if let Some(store) = self.store.clone() {
                            let calls = log.clone();
                            let sid = session_id.clone();
                            self.spawn_tokio(async move {
                                let _ = store.save_tool_calls(&sid, &calls);
                            });
                        }
                    }
                    // 器灵改了批注 → GUI 实时刷新（agent 的 AnnotationService 与 GUI 同文件；
                    // GUI 内存 store 需重新从磁盘加载才能看到器灵写入的批注）
                    if !is_error
                        && matches!(tool.as_str(), "annotate_create" | "annotate_update" | "annotate_delete")
                    {
                        self.annotation_store =
                            AnnotationStore::load(&Self::annotation_store_path());
                        self.reload_annotations();
                    }
                }
                qview_agent::event::AgentEvent::ViewIntentEmitted { intent, .. } => {
                    // 自动投影 intent；FocusLine（跳转）按用户要求自动应用，ApplyFilter 留给气泡内点击
                    project::apply_intent(self, intent);
                    push_chat(&self.agent_state, ChatMsg::Intent(intent.clone()));
                }
                qview_agent::event::AgentEvent::ProposalCreated { proposal, .. } => {
                    push_chat(
                        &self.agent_state,
                        ChatMsg::Note { text: format!("📝 提案: {}", proposal.reason) },
                    );
                }
                qview_agent::event::AgentEvent::ApprovalRequired { proposal_id, tool, reason, .. } => {
                    *self.agent_state.pending_proposal.lock() = Some((
                        *proposal_id,
                        tool.clone(),
                        reason.clone(),
                    ));
                    push_chat(
                        &self.agent_state,
                        ChatMsg::Note { text: format!("⚠ 等待审批: {reason}") },
                    );
                }
                qview_agent::event::AgentEvent::SessionFinished { summary, .. } => {
                    *self.agent_state.active_session.lock() = None;
                    *self.agent_state.in_flight_tool.lock() = None;
                    *self.agent_state.current_phase.lock() = qview_agent::event::Phase::Done;
                    log_info!("agent", "器灵会话完成: {}", summary);
                    // summary 是**唯一最终回复**（提示词已要求模型把完整答复写进 summary）。
                    // 逐字与最后一条 Agent 气泡相同才跳过（防精确重复）。
                    let mut t = self.agent_state.transcript.lock();
                    let dup = matches!(
                        t.last(),
                        Some(crate::agent::ChatMsg::Agent { text: last, .. })
                            if last == summary
                    );
                    if !dup && !summary.trim().is_empty() {
                        t.push(crate::agent::ChatMsg::Agent {
                            text: summary.clone(),
                            is_error: false,
                        });
                    }
                    drop(t);
                    // 高亮保留到下次会话/换文件（不复位），让用户能看到 AI 标记了哪些行
                }
                qview_agent::event::AgentEvent::Cancelled { .. } => {
                    *self.agent_state.active_session.lock() = None;
                    *self.agent_state.in_flight_tool.lock() = None;
                    *self.agent_state.current_phase.lock() = qview_agent::event::Phase::Cancelled;
                    push_chat(&self.agent_state, ChatMsg::Note { text: "■ 任务已取消".into() });
                }
                qview_agent::event::AgentEvent::Failed { error, .. } => {
                    *self.agent_state.active_session.lock() = None;
                    *self.agent_state.in_flight_tool.lock() = None;
                    *self.agent_state.current_phase.lock() = qview_agent::event::Phase::Failed;
                    // 失败原因写进 qview.log（含 LLM 层展开的错误链），方便排查
                    log_error!("agent", "器灵会话失败: {error}");
                    push_chat(
                        &self.agent_state,
                        ChatMsg::Agent { text: error.clone(), is_error: true },
                    );
                    // 高亮保留（不随失败清空），用户仍能看到 AI 标记过的行
                }
                _ => {}
            }
        }
        // 2) 器灵聊天窗口。用 eframe 多视口把面板渲染进**独立原生子窗口**
        //    （可拖出主窗口之外）。show_viewport_immediate 的闭包是 FnMut、
        //    同步执行，不需要 'static —— 可以直接借用 &mut self 渲染面板。
        if self.show_agent_window {
            let sr = ctx.screen_rect();
            let child_w = crate::agent::panel::AGENT_WIN_W;
            let child_h =
                (sr.height() * crate::agent::panel::AGENT_WIN_H_RATIO).clamp(500.0, 880.0);
            let pos = egui::pos2(sr.right() - child_w - 16.0, sr.top() + 54.0);

            // 无系统标题栏（无边框窗口）：用自定义蓝色顶条当拖动把手
            // （panel.rs header 里发 StartDrag），窗口更干净。
            // 注意：要用 `with_decorations(false)`——`with_titlebar_shown(false)`
            // 在 egui-winit 创建窗口时被忽略（egui-winit lib.rs 里 titlebar_shown
            // 是 `_titlebar_shown`），只有 decorations 决定是否带系统边框。
            let mut builder = egui::ViewportBuilder::default()
                .with_title("器灵 AI")
                .with_decorations(false)
                .with_min_inner_size([320.0, 420.0]);
            // 位置/尺寸只在首帧设一次，之后交给用户拖动/缩放
            if !self.agent_viewport_pos_set {
                builder = builder
                    .with_inner_size([child_w, child_h])
                    .with_position(pos);
                self.agent_viewport_pos_set = true;
            }

            let style = ctx.style(); // 子窗口沿用主题
            let font_defs = self.font_defs.clone();
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("qview_agent_child"),
                builder,
                |child_ctx, class| {
                    // 子视口独立 Context：需要重新应用样式与字体
                    child_ctx.set_style(style.clone());
                    if let Some(f) = &font_defs {
                        child_ctx.set_fonts(f.clone());
                    }
                    // 用户点了子窗口的 OS 关闭按钮 → 关掉（停止渲染该视口）
                    if child_ctx.input(|i| i.viewport().close_requested()) {
                        self.show_agent_window = false;
                        return;
                    }
                    // 多视口不可用时 egui 退化为嵌在主窗口（ViewportClass::Embedded）
                    let detached = !matches!(class, egui::ViewportClass::Embedded);
                    crate::agent::AgentPanel::new(self).show_window(child_ctx, detached);
                    // 审批弹窗也放进聊天窗口（写操作确认）
                    crate::agent::approval::show_modal(child_ctx, &self.agent_state, &*self);
                    // 给原生 OS 窗口设圆角（egui 里画的是圆角背景，但 OS 窗口本体是方角）
                    if detached {
                        self.apply_agent_rounded_corners(child_ctx);
                    }
                },
            );
        }
    }

    /// 给器灵子窗口的原生 OS 窗口设圆角区域。
    ///
    /// 背景：egui 里画的是圆角（radius=12）背景，但底下 Win32 窗口本体是方角，
    /// 圆角外露出方窗角。这里用 `SetWindowRgn + CreateRoundRectRgn` 把 OS 窗口
    /// 本身裁成圆角（Win10 兼容；`DWMWA_WINDOW_CORNER_PREFERENCE` 只支持 Win11）。
    /// 窗口尺寸/缩放变化时区域会失效 → 每帧校验尺寸，变了才重设。
    #[cfg(windows)]
    fn apply_agent_rounded_corners(&mut self, child_ctx: &egui::Context) {
        const RADIUS_LP: f32 = 12.0; // 与 panel.rs 背景圆角一致（逻辑像素）

        let hwnd = if self.agent_hwnd != 0 && crate::win32::is_window(self.agent_hwnd) {
            self.agent_hwnd
        } else {
            // 窗口未创建（首帧）或已被销毁（关掉重开）→ 重新按标题找
            let h = crate::win32::find_window_by_title("器灵 AI");
            if h == 0 {
                return; // 还没创建，下一帧再试
            }
            self.agent_hwnd = h;
            self.agent_round_region = (-1, -1, -1); // 新窗口，之前的区域缓存作废
            h
        };

        let scale = child_ctx.pixels_per_point();
        let sr = child_ctx.screen_rect();
        let w = (sr.width() * scale).round() as i32;
        let h = (sr.height() * scale).round() as i32;
        let r = (RADIUS_LP * scale).round() as i32;
        if self.agent_round_region != (w, h, r) {
            crate::win32::set_rounded_region(hwnd, w, h, r);
            self.agent_round_region = (w, h, r);
        }
    }

    /// 非 Windows 平台：器灵窗口圆角由系统/合成器处理，这里 no-op。
    #[cfg(not(windows))]
    fn apply_agent_rounded_corners(&mut self, _child_ctx: &egui::Context) {}

    /// 切换器灵浮动窗口显示。
    pub fn toggle_agent_window(&mut self) {
        self.show_agent_window = !self.show_agent_window;
        log_info!("agent", "器灵浮动窗口: {}", if self.show_agent_window { "打开" } else { "关闭" });
        if self.show_agent_window {
            // 每次打开重置：子窗口首帧重新定位到主窗口右上角
            self.agent_viewport_pos_set = false;
            self.agent_focus_input = true; // 下一帧自动聚焦输入框
            // 首次打开：若从未有过消息，提示一句欢迎语 + 配置状态
            let mut t = self.agent_state.transcript.lock();
            if t.is_empty() {
                t.push(crate::agent::ChatMsg::Agent {
                    text: "✨ 嗨！我是小Q～ 我能帮你搜报错、看上下文、标重点、出报告。日志、代码、配置文件都行，超大文件也不虚。直接开问吧！🚀".into(),
                    is_error: false,
                });
                // 未配置真实 LLM 时给出提示
                let provider = self.config.agent.provider.provider;
                if provider == qview_agent::config::LlmProvider::Mock
                    && self.config.agent.provider.mock_script_path.is_none()
                    && self.config.agent.provider.mock_static.is_none()
                {
                    t.push(crate::agent::ChatMsg::Note {
                        text: "ℹ 当前为 Mock（离线）模式，回复是演示文本。要接入真实 AI，请到「设置 → AI」填 Provider 与 API Key。".into(),
                    });
                }
            }
        }
    }
    /// Reset the horizontal-scroll width cache.  Called when text display
    /// metrics change (font size, font family, row height) — otherwise the
    /// "largest width ever seen" accumulator keeps a stale, too-wide value
    /// after the user shrinks the font.
    pub fn invalidate_content_width(&mut self) {
        self.max_content_w = 0.0;
        self.full_width_scan_done = false;
    }

    /// Persist current settings to disk.
    pub fn save_config(&mut self) {
        self.config.gui.font_size = self.font_size;
        self.config.gui.row_height = self.row_h;
        self.config.gui.show_line_numbers = self.show_line_numbers;
        self.config.gui.word_wrap = self.word_wrap;
        self.config.gui.show_whitespace = self.show_whitespace;
        self.config.gui.level_coloring = self.level_coloring;
        self.config.gui.case_sensitive = self.case_sensitive;
        self.config.gui.use_regex = self.use_regex;
        self.config.gui.whole_word = self.whole_word;
        if let Some(ref name) = self
            .available_fonts
            .get(self.selected_font)
        {
            self.config.gui.font_family = name.to_string();
        }
        log_debug!("app", "保存配置");
        self.config.save();
    }
}

// ---------------------------------------------------------------------------
// File / search / navigation helpers
// ---------------------------------------------------------------------------

impl QLogApp {
    /// Handle files dragged and dropped onto the window.
    fn handle_dropped_files(&mut self, ctx: &Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        // Take the first dropped file.
        if let Some(file) = dropped.into_iter().next() {
            if let Some(ref path) = file.path {
                log_info!("app", "拖放文件: {}", path.display());
                self.try_open(path.clone());
            }
        }
    }

    pub fn open_file(&mut self, path: PathBuf) {
        log_info!("app", "打开文件: {}", path.display());
        // Stop the previous file's background jobs before replacing the engine.
        // Otherwise a stale 27 GB scan keeps burning disk (streaming at ~2.4 GB/s)
        // while the user opens something else.
        if let Some(arc) = &mut self.engine {
            let mut e = arc.lock();
            e.cancel_search();
            e.cancel_index();
        }
        let engine_config = self.config.engine.clone();
        let t0 = std::time::Instant::now();
        match Engine::with_config(path.clone(), engine_config) {
            Ok(mut engine) => {
                let size = engine.mmap.size();
                let encoding = engine.encoding.name();
                let load_ms = t0.elapsed().as_millis();
                if engine.index.is_complete() {
                    // Index already built (small file or .qli cache hit).
                    let lines = engine.effective_line_count();
                    let label = if size <= self.config.engine.small_file_threshold {
                        "即时加载"
                    } else {
                        "缓存"
                    };
                    log_info!("app",
                        "打开成功: {} ({} 行, {}, 编码={}, 策略={}, 耗时={}ms)",
                        path.display(), lines, crate::viewer::human_bytes(size),
                        encoding, label, load_ms);
                    self.flash_status(
                        format!(
                            "已打开 · {} 行 · {} ({})",
                            lines,
                            crate::viewer::human_bytes(size),
                            label,
                        ),
                        4,
                    );
                } else {
                    // Large file without cache — background indexing needed.
                    log_info!("app",
                        "打开成功(待索引): {} ({}, 编码={}, 耗时={}ms)",
                        path.display(), crate::viewer::human_bytes(size),
                        encoding, load_ms);
                    engine.submit_build_index();
                    self.index_started_at = Some(std::time::Instant::now());
                    self.flash_status(
                        format!(
                            "已打开 · 正在索引 · {}",
                            crate::viewer::human_bytes(size),
                        ),
                        2,
                    );
                }
                self.search_input.clear();
                self.clear_search();
                self.path = Some(path.clone());
                // 最近打开：同步更新内存缓存（菜单立即可见），后台落库 files 表。
                // 新文件（未保存的临时文件）不进最近打开，也不进 files 表。
                if !self.is_new_file {
                    {
                        let mut recents = self.recent_files.lock();
                        recents.retain(|p| p != &path);
                        recents.insert(0, path.clone());
                        recents.truncate(10);
                    }
                }
                // 共享 Engine：GUI 与 Agent 持有同一份 Arc<Mutex<Engine>>（消除双 mmap）
                let arc: Arc<parking_lot::Mutex<Engine>> = Arc::new(parking_lot::Mutex::new(engine));
                self.engine = Some(arc.clone());

                // Agent：把当前 Engine 注册到常驻 DocumentService（工具才能拿到 document_id）。
                // register 不重新 open → 同一份 mmap / 索引；不受 2GiB 拦截。
                if let (Some(deps), Some(apath)) = (self.agent_deps.as_ref(), self.path.clone()) {
                    match deps.docs.register(arc.clone(), apath) {
                        Ok(id) => {
                            self.agent_doc_id = Some(id);
                            log_info!("agent", "Agent DocumentService 注册当前文件 id={}", id.get());
                        }
                        Err(e) => log_warn!("agent", "Agent 注册文档失败: {e:#}"),
                    }
                }

                // 文件元数据落库（后台线程；DB I/O 不碰渲染热路径）
                // 用 `self.path`（此时已 clone 设好），避免 move 后的 `path`。
                // 新文件（未保存的临时文件）不进 files 表。
                if !self.is_new_file {
                    let store = self.store.clone();
                    let canonical = self
                        .path
                        .as_ref()
                        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
                    if let (Some(store), Some(canonical)) = (store, canonical) {
                        let p = canonical.display().to_string();
                        let sz = size;
                        let enc = encoding.to_string();
                        self.spawn_tokio(async move {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let prev = store.load_file(&p).ok().flatten();
                            let meta = qview_store::FileMeta {
                                path: p,
                                last_opened_at_ms: now,
                                open_count: prev.map(|f| f.open_count).unwrap_or(0) + 1,
                                size_bytes: sz,
                                encoding: enc,
                            };
                            let _ = store.record_file(&meta);
                        });
                    }
                }

                self.scroll_y = 0.0;
                self.h_scroll = 0.0;
                self.max_content_w = 0.0;
                self.wrap_height_mult = 2.5;
                self.full_width_scan_done = false;
                // A stale selection from the previous file must not persist.
                self.selection = None;
                self.selecting = false;
                // Caret resets to the top of the new file (edit mode may stay on).
                self.edit_cursor = if self.edit_mode { Some((0, 0)) } else { None };
                // Annotation panels belong to the previous file; reload the
                // current file's annotations and drop any stale input window.
                self.show_annotation_dialog = false;
                self.show_annotation_list = false;
                self.annotation_edit_id = None;
                self.annotation_input.clear();
                self.pending_reanchor = false;
                self.reload_annotations();
                self.save_config();
            }
            Err(e) => {
                log_error!("app", "打开文件失败: {} — {}", path.display(), e);
                self.flash_status(format!("打开失败: {}", e), 6);
            }
        }
    }

    /// Close the currently open file and release all per-file state:
    /// the mmap mapping, in-memory index, line cache, search results and
    /// selection are all freed. Persistent resources (recent-file list,
    /// search history, theme/font settings, .qli index cache on disk)
    /// are intentionally kept.
    pub fn close_file(&mut self) {
        if self.engine.is_none() {
            return;
        }
        log_info!("app", "关闭文件");
        // Stop background jobs (indexing / search) before dropping the engine.
        if let Some(arc) = &mut self.engine {
            let mut e = arc.lock();
            e.cancel_search();
            e.cancel_index();
        }
        // If this is an unsaved new file, remember its temp path so we can
        // delete it AFTER the engine (and its mmap handle) is released.
        let new_tmp = if self.is_new_file { self.path.clone() } else { None };
        self.is_new_file = false;
        // Drop the engine — releases mmap, index, line cache, search buffers.
        self.engine = None;
        self.path = None;

        // Agent：注销当前文件（仅删映射；Engine 随 GUI 的 Arc 释放 → mmap 解除）
        if let Some(id) = self.agent_doc_id.take() {
            if let Some(deps) = self.agent_deps.as_ref() {
                deps.docs.unregister(id);
                log_info!("agent", "Agent DocumentService 注销文件 id={}", id.get());
            }
        }
        // Agent 投影状态是 per-file 的，一并清空
        self.agent_highlights.clear();
        self.agent_filter = None;

        // Clear per-file search state (search match cache, highlights, status).
        self.clear_search();

        // Reset navigation & selection.
        self.scroll_y = 0.0;
        self.h_scroll = 0.0;
        self.max_content_w = 0.0;
        self.wrap_height_mult = 2.5;
        self.full_width_scan_done = false;
        self.selection = None;
        self.selecting = false;
        self.pointer_was_down = false;
        self.pending_copy_text = None;
        self.index_started_at = None;
        self.search_started_at = None;
        self.first_visible_line = 0;
        self.last_visible_line = 0;
        self.goto_input.clear();

        // Close file-dependent dialogs.
        self.show_file_properties = false;
        self.show_index_manager = false;
        // Clear annotation panels & current-file annotations (store is kept).
        self.annotations.clear();
        self.annotated_lines.clear();
        self.show_annotation_dialog = false;
        self.show_annotation_list = false;
        self.annotation_edit_id = None;
        self.annotation_selected_id = None;
        self.annotation_input.clear();
        // Editing is per-file: exit edit mode and drop the caret.
        self.edit_mode = false;
        self.edit_cursor = None;
        self.pending_discard = None;
        self.pending_reanchor = false;
        self.edit_saving = false;
        if let Some(t) = new_tmp {
            let _ = std::fs::remove_file(&t);
        }

        self.flash_status("已关闭", 2);
    }

    pub fn clear_search(&mut self) {
        log_debug!("app", "清空搜索");
        self.search_hits.clear();
        self.search_lines.clear();
        self.search_hit_idx = 0;
        self.search_total_count = 0;
        self.search_status.clear();
        self.search_started_at = None;
        self.search_query.clear();
        self.search_input.clear();
        self.parsed_search_q = None;
        if let Some(arc) = &mut self.engine {
            let mut e = arc.lock();
            let _ = e.submit_search(String::new(), qview_core::search::SearchOptions::default());
        }
    }

    pub fn run_search(&mut self) {
        let q_raw = self.search_input.clone();
        if q_raw.is_empty() {
            self.clear_search();
            if let Some(arc) = &mut self.engine {
                let mut e = arc.lock();
                let _ = e.submit_search(String::new(), qview_core::search::SearchOptions::default());
            }
            return;
        }
        // A multi-line LITERAL query copied from the viewer is LF-joined, but a
        // CRLF file stores '\r\n' — without normalising, a copied multi-line
        // query would never match. Regex queries are left as typed.
        let q = if !self.use_regex && q_raw.contains('\n') {
            if self.engine.as_ref().map_or(false, |arc| arc.lock().uses_crlf()) {
                q_raw.replace("\r\n", "\n").replace('\n', "\r\n")
            } else {
                q_raw.replace("\r\n", "\n")
            }
        } else {
            q_raw
        };
        // CRLF file stores '\r\n' — with the flag set, the engine rewrites regex
        // `$` anchors to `(?:\r?$)` so they match the file's actual line ends.
        // Computed before `engine` is locked below.
        let crlf = self.engine.as_ref().map_or(false, |arc| arc.lock().uses_crlf());
        let mut engine = match &mut self.engine {
            Some(arc) => arc.lock(),
            None => return,
        };
        log_info!("app", "搜索: \"{}\" (大小写敏感={}, 正则={}, 整词={})",
            q, self.case_sensitive, self.use_regex, self.whole_word);
        self.search_hits.clear();
        self.search_lines.clear();
        self.search_hit_idx = 0;
        self.search_total_count = 0;
        self.search_query = q.clone();
        self.search_status = "搜索中...".to_string();
        self.search_started_at = Some(std::time::Instant::now());
        // 搜索历史：同步更新内存缓存（统计/未来联想可见），异步落库。
        // q 随后会被 submit_search 移动走，这里先留一份。
        let q_hist = q.clone();
        {
            let mut sh = self.search_history.lock();
            sh.retain(|s| s != &q_hist);
            sh.insert(0, q_hist.clone());
            sh.truncate(20);
        }

        let opts = qview_core::search::SearchOptions {
            case_sensitive: self.case_sensitive,
            use_regex: self.use_regex,
            whole_word: self.whole_word,
            crlf,
        };
        // Cache the parsed query for the viewer's highlighting, from the SAME
        // options as the search — so re-searching the same string with a
        // different toggle (e.g. regex on/off) refreshes the highlight too.
        self.parsed_search_q = qview_core::search::parse_query(&q, &opts).ok();
        if let Err(e) = engine.submit_search(q, opts) {
            log_error!("app", "提交搜索失败: {}", e);
            self.search_status = format!("搜索错误: {}", e);
        }
        // 落库必须在 engine guard 释放之后（spawn_tokio 借 &self，与 &mut engine 冲突）
        drop(engine);
        if let Some(store) = self.store.clone() {
            self.spawn_tokio(async move {
                let _ = store.record_search(&q_hist);
            });
        }
    }

    /// 旧估算行高（无视觉行模型时的回退）。
    fn fallback_row_h(&self) -> f64 {
        if self.word_wrap {
            self.row_h * self.wrap_height_mult
        } else {
            self.row_h
        }
    }

    /// 文件底部可滚动到的最大 scroll_y（视觉行模型：含超长行展开）。
    fn max_scroll_px(&self, total_lines: u64) -> f64 {
        if let Some(m) = &self.visual_model {
            (m.content_rows(total_lines) as f64 * m.row_h as f64).max(0.0)
        } else {
            (total_lines as f64 * self.fallback_row_h()).max(0.0)
        }
    }

    /// 把滚动位置设置到 `line` 的视觉行起点（超长行 → 行首视觉行）。
    fn scroll_to_line_start(&mut self, line: u64) {
        if let Some(m) = &self.visual_model {
            self.scroll_y = m.line_to_visual(line) as f64 * m.row_h as f64;
        } else {
            self.scroll_y = line as f64 * self.fallback_row_h();
        }
    }

    /// 把滚动位置设置到命中字节所在的视觉行 —— 超长行内滚到匹配行附近，
    /// 让当前命中真正可见（修复"长行里匹配到了但滚不到/看不到高亮"）。
    /// 调用方已持有 engine guard，**不再 lock**（非重入，避免死锁）。
    fn scroll_to_byte_with(&mut self, engine: &Engine, byte: u64) {
        let line = engine.line_of_byte(byte);
        if let Some(m) = &self.visual_model {
            let line_start = engine.line_byte_range(line).map(|(s, _)| s).unwrap_or(0);
            let row_in = m.row_in_line_for_byte(byte.saturating_sub(line_start));
            let v = m.line_to_visual(line) + row_in;
            // 往上留 ~4 行上下文，命中行不贴顶
            self.scroll_y = (v as f64 * m.row_h as f64 - 4.0 * m.row_h as f64).max(0.0);
        } else {
            self.scroll_y = line as f64 * self.fallback_row_h();
        }
    }

    pub fn jump_hit(&mut self, delta: i64) {
        // 先 clone Arc 再锁：guard 借的是本地 Arc，不是 self.engine，
        // 后续 self.* 可变调用才不与锁冲突。
        let Some(arc) = self.engine.clone() else { return };
        let engine = arc.lock();
        if engine.search.is_empty() {
            return;
        }

        let t0 = std::time::Instant::now();
        let cursor_before = engine.search.cursor();
        // 视口顶/底行的物理行号（用视觉行模型，超长行展开不影响锚定）。
        let (first_visible, last_visible) = if let Some(m) = &self.visual_model {
            let vt = (self.scroll_y / m.row_h as f64).floor() as u64;
            (m.visual_to_line(vt), m.visual_to_line(vt + 80))
        } else {
            let effective_row_h = self.fallback_row_h();
            let f = (self.scroll_y / effective_row_h).floor() as u64;
            (f, f + 80)
        };

        // ── Viewport anchoring ─────────────────────────────────────────
        // 专业浏览器语义：视口里已高亮的命中 → 顺着它继续（相对跳转）；
        // 视口里没有高亮（用户拖动滚动条去了别处）→ 把搜索锚定到视口顶行
        // 之后第一个命中，再从这里开始找。这样匹配几万个时拖到目标区域后
        // 点“下一个”就从该区域第一行开始，而不是从内部游标继续。
        let cursor_line = engine.search.current().map(|m| engine.line_of_byte(m.byte));
        let cursor_visible = cursor_line.map_or(false, |l| l >= first_visible && l <= last_visible);

        log_debug!("app", "jump_hit: delta={delta} cursor_before={cursor_before} cursor_line={cursor_line:?} visible={first_visible}..={last_visible} cursor_visible={cursor_visible}");

        if !cursor_visible {
            let max_line = engine.effective_line_count().saturating_sub(1);
            let top_line = first_visible.min(max_line);
            let first_byte = engine.read_line(top_line).start_byte;
            log_debug!("app", "jump_hit anchoring: top_line={top_line} first_byte={first_byte}");
            if engine.search.seek_to_byte(first_byte) {
                let anchored = engine.search.cursor();
                log_debug!("app", "jump_hit anchored to cursor={anchored}");
                if delta > 0 {
                    // The anchored match IS the "next" result.
                    let total = engine.search.len();
                    if let Some(m) = engine.search.jump(anchored) {
                        let line = engine.line_of_byte(m.byte);
                        self.scroll_to_byte_with(&engine, m.byte);
                        self.search_hit_idx = anchored;
                        self.flash_status(
                            format!("命中 {}/{} · 行 {}", anchored + 1, total, line + 1),
                            3,
                        );
                        self.search_status = format!("{}/{} 条匹配", anchored + 1, total);
                    }
                    return;
                }
                // delta < 0: fall through to relative jump_by below — now
                // relative to the anchored cursor (the match just above the
                // viewport; wraps to the last match when the anchor is #0).
            } else {
                // No match at/after the viewport — wrap.
                log_debug!("app", "jump_hit anchor failed (no match after viewport), wrapping");
                if delta > 0 {
                    if let Some(m) = engine.search.first() {
                        let cursor = engine.search.cursor();
                        let total = engine.search.len();
                        let line = engine.line_of_byte(m.byte);
                        self.scroll_to_byte_with(&engine, m.byte);
                        self.search_hit_idx = cursor;
                        self.flash_status(
                            format!("已到最后 · 命中 {}/{} · 行 {}", cursor + 1, total, line + 1),
                            3,
                        );
                        self.search_status = format!("{}/{} 条匹配", cursor + 1, total);
                    }
                } else {
                    if let Some(m) = engine.search.last() {
                        let cursor = engine.search.cursor();
                        let total = engine.search.len();
                        let line = engine.line_of_byte(m.byte);
                        self.scroll_to_byte_with(&engine, m.byte);
                        self.search_hit_idx = cursor;
                        self.flash_status(
                            format!("已到头 · 命中 {}/{} · 行 {}", cursor + 1, total, line + 1),
                            3,
                        );
                        self.search_status = format!("{}/{} 条匹配", cursor + 1, total);
                    }
                }
                return;
            }
        }

        // ── Relative navigation (cursor visible, or anchored "prev") ──
        if let Some(m) = engine.search.jump_by(delta) {
            let t1 = t0.elapsed();
            let line = engine.line_of_byte(m.byte);
            let cursor = engine.search.cursor();
            let total = engine.search.len();
            self.scroll_to_byte_with(&engine, m.byte);
            self.search_hit_idx = cursor;
            log_debug!("app", "jump_hit cursor={cursor} byte={} line={line} jump_by={:?} line_of_byte={:?}",
                m.byte, t1, t0.elapsed() - t1);
            self.flash_status(
                format!("命中 {}/{} · 行 {}", cursor + 1, total, line + 1),
                3,
            );
            self.search_status = format!("{}/{} 条匹配", cursor + 1, total);
        }
    }

    /// Called when search results arrive: anchor the cursor to the first
    /// match at or after the current viewport so that "next" starts from
    /// where the user is looking.
    fn anchor_search_to_viewport(&mut self) {
        let engine = match &self.engine {
            Some(arc) => arc.lock(),
            None => return,
        };
        if engine.search.is_empty() {
            return;
        }
        let first_visible = if let Some(m) = &self.visual_model {
            let vt = (self.scroll_y / m.row_h as f64).floor() as u64;
            m.visual_to_line(vt)
        } else {
            (self.scroll_y / self.fallback_row_h()).floor() as u64
        };
        let first_byte = engine.read_line(first_visible).start_byte;
        let t0 = std::time::Instant::now();
        let total = engine.search.len();
        if engine.search.seek_to_byte(first_byte) {
            let cursor = engine.search.cursor();
            self.search_hit_idx = cursor;
            // The search poll reports a hardcoded "1/N". Reflect the REAL
            // anchored cursor so "上一个/下一个" navigation isn't misleading
            // (e.g. searching while scrolled mid-file anchors to a later match).
            self.search_status = format!("{}/{} 条匹配", cursor + 1, total);
            log_debug!("app", "anchor_search: first_visible={first_visible} cursor={cursor} total={total} took={:?}", t0.elapsed());
        } else {
            // No match at/after the viewport — the cursor stays at the first
            // match, which is where the status already points.
            self.search_status = if total > 0 {
                format!("1/{total} 条匹配")
            } else {
                "无匹配".to_string()
            };
            log_debug!("app", "anchor_search: first_visible={first_visible} NO_MATCH took={:?}", t0.elapsed());
        }
    }

    /// Extract selected text from the current selection range. Returns the
    /// text if a selection is active and valid, otherwise `None`.
    ///
    /// Selection columns are character indices (from the viewer's pixel-based
    /// coordinate mapping), so we must convert to byte indices for proper
    /// `&str` slicing.
    pub fn copy_selection_text(&self) -> Option<String> {

        let (start_line, start_col, end_line, end_col) = self.selection?;
        let engine = self.engine.as_ref()?.lock();
        let (from_line, from_col, to_line, to_col) = if start_line <= end_line {
            (start_line, start_col, end_line, end_col)
        } else {
            (end_line, end_col, start_line, start_col)
        };
        let mut text = String::new();
        for ln in from_line..=to_line {
            let raw = engine.read_line(ln);
            let line_text = raw.text.trim_end_matches('\n').trim_end_matches('\r');
            let nchars = line_text.chars().count();
            // The visible highlight clamps anchors at EOL; the copy must do the
            // same.  In particular, a from- or to-anchor at/beyond a short
            // line's end yields an EMPTY slice — it must NOT fall through and
            // copy the whole line, otherwise an out-of-line anchor (e.g. a
            // click in the blank space right of a short line) silently copies
            // an entire unselected line.
            let slice = if ln == from_line && ln == to_line {
                let lo = from_col.min(to_col).min(nchars);
                let hi = from_col.max(to_col).min(nchars);
                (lo < hi).then(|| &line_text[char_col_to_byte(line_text, lo)..char_col_to_byte(line_text, hi)])
            } else if ln == from_line {
                let fc = from_col.min(nchars);
                (fc < nchars).then(|| &line_text[char_col_to_byte(line_text, fc)..])
            } else if ln == to_line {
                let tc = to_col.min(nchars);
                (tc > 0).then(|| &line_text[..char_col_to_byte(line_text, tc)])
            } else {
                // Middle lines: the whole line.
                Some(&line_text[..])
            };
            if let Some(s) = slice {
                text.push_str(s);
            }
            if ln < to_line {
                text.push('\n');
            }
        }
        if text.is_empty() { None } else { Some(text) }
    }

    // -------------------------------------------------------------------
    // Annotations (批注)
    // -------------------------------------------------------------------

    /// data_dir/annotations.json — the central annotation store.
    pub fn annotation_store_path() -> PathBuf {
        let dir = crate::config::AppConfig::config_dir()
            .unwrap_or_else(|| PathBuf::from("data"));
        dir.join("annotations.json")
    }

    /// Rebuild `annotations` + `annotated_lines` from the store for the
    /// currently open file (empty when no file is open).
    pub fn reload_annotations(&mut self) {
        self.annotations = self
            .path
            .as_ref()
            .map(|p| self.annotation_store.for_file(p).to_vec())
            .unwrap_or_default();
        // One marker per annotation, on its start line.
        self.annotated_lines = self.annotations.iter().map(|a| a.start_line).collect();
    }

    /// Build an `Annotation` from the current selection and save it.
    /// `text` is the (already validated, non-empty) note body.
    pub fn add_annotation_from_selection(&mut self, text: String) {
        // clone Arc：guard 借本地 Arc，后续 self.* 可变调用不冲突
        let (Some(arc), Some(path)) = (self.engine.clone(), self.path.clone()) else {
            self.flash_status("未打开文件", 2);
            return;
        };
        let Some((s_line, s_col, e_line, e_col)) = self.selection else {
            self.flash_status("请先选中要批注的内容", 2);
            return;
        };
        // 先取选中快照、**后**锁 engine：copy_selection_text 内部会再 `lock` 同一个
        // parking_lot::Mutex（不可重入），若此刻已持有锁 → 同线程二次锁死锁
        // （表现：点保存进程无响应、CPU/内存都不高）。排序：快照 → 锁 → 算字节。
        let snapshot = match self.copy_selection_text() {
            Some(s) => s,
            None => {
                self.flash_status("请先选中要批注的内容", 2);
                return;
            }
        };
        let (from_line, from_col, to_line, to_col) = if (s_line, s_col) <= (e_line, e_col) {
            (s_line, s_col, e_line, e_col)
        } else {
            (e_line, e_col, s_line, s_col)
        };
        let engine = arc.lock();
        let start_byte = annotation_byte(&engine, from_line, from_col);
        let end_byte = annotation_byte(&engine, to_line, to_col);

        // Cap the snapshot at a CHAR boundary without appending a marker — the
        // stored bytes must be a true byte-prefix of the selection so an
        // edit-save re-anchor can search for them in the file.
        let mut snapshot = snapshot;
        if snapshot.len() > qview_core::annotation::MAX_SELECTED_SNAPSHOT {
            let cap = qview_core::annotation::MAX_SELECTED_SNAPSHOT;
            let mut cut = cap;
            while !snapshot.is_char_boundary(cut) {
                cut -= 1;
            }
            snapshot.truncate(cut);
        }

        let ann = qview_core::annotation::Annotation {
            id: 0,
            file_key: String::new(),
            start_byte,
            end_byte,
            start_line: from_line,
            end_line: to_line,
            start_col: from_col,
            end_col: to_col,
            selected_text: snapshot,
            text,
            created_at: crate::logger::now(),
            color: 0,
            stale: false,
        };
        self.annotation_store.add(&path, ann);
        if self.annotation_store.save().is_err() {
            log_warn!("app", "批注保存失败");
        }
        self.reload_annotations();
        log_debug!(
            "app",
            "添加批注: 行 {}..{} (共 {} 条)",
            from_line + 1,
            to_line + 1,
            self.annotations.len()
        );
        self.flash_status(format!("已添加批注 (共 {} 条)", self.annotations.len()), 3);
    }

    /// Save the annotation input dialog: edit an existing annotation when
    /// `annotation_edit_id` is set, else create a new one from the selection.
    pub fn save_annotation_dialog(&mut self) {
        let text = self.annotation_input.trim().to_string();
        if text.is_empty() {
            self.flash_status("批注内容不能为空", 2);
            return;
        }
        if let Some(id) = self.annotation_edit_id {
            if let Some(ref path) = self.path.clone() {
                if self.annotation_store.set_text(path, id, text) {
                    if self.annotation_store.save().is_err() {
                        log_warn!("app", "批注保存失败");
                    }
                    self.reload_annotations();
                    log_debug!("app", "编辑批注 id={}", id);
                    self.flash_status("批注已更新", 3);
                }
            }
        } else {
            self.add_annotation_from_selection(text);
        }
        self.show_annotation_dialog = false;
        self.annotation_edit_id = None;
        self.annotation_input.clear();
    }

    /// Delete an annotation by id (current file only).
    pub fn remove_annotation(&mut self, id: u64) {
        if let Some(ref path) = self.path.clone() {
            if self.annotation_store.remove(path, id) {
                if self.annotation_store.save().is_err() {
                    log_warn!("app", "批注保存失败");
                }
                self.reload_annotations();
                log_debug!("app", "删除批注 id={}", id);
                self.flash_status(format!("已删除批注 (剩 {} 条)", self.annotations.len()), 3);
            }
        }
    }

    /// Jump to an annotation: scroll so its start line is at the top and light
    /// up the annotated range with the existing selection highlight.
    pub fn jump_to_annotation(&mut self, id: u64) {
        let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        self.scroll_to_line_start(a.start_line);
        self.h_scroll = 0.0;
        self.selection = Some((a.start_line, a.start_col, a.end_line, a.end_col));
        log_debug!("app", "跳转到批注 id={} 行 {}..{}", id, a.start_line + 1, a.end_line + 1);
        self.flash_status(format!("批注: 行 {}–{}", a.start_line + 1, a.end_line + 1), 3);
    }

    /// After an edit-save, re-anchor every annotation of the current file to
    /// its new position: the selected-text snapshot is searched in the saved
    /// file near its old offset; found → position updated, not found → stale.
    pub fn reanchor_annotations(&mut self) {
        // clone Arc：guard 借本地 Arc，后续 self.* 可变调用不冲突
        let (Some(arc), Some(path)) = (self.engine.clone(), self.path.clone()) else {
            return;
        };
        let engine = arc.lock();
        // Snapshot the fields we need (avoids borrow conflicts with the store).
        let items: Vec<(u64, u64, u64, u64, usize, u64, usize, String)> = self
            .annotations
            .iter()
            .map(|a| {
                (
                    a.id,
                    a.start_byte,
                    a.start_line,
                    a.start_col as u64,
                    a.start_col,
                    a.end_line,
                    a.end_col,
                    a.selected_text.clone(),
                )
            })
            .collect();
        let mut changed = false;
        for (id, old_byte, old_start_line, _, _start_col, old_end_line, old_end_col, selected) in items
        {
            if selected.is_empty() {
                continue;
            }
            if let Some(pos) = qview_core::annotation::find_nearest(
                engine.mmap.as_slice(),
                selected.as_bytes(),
                old_byte,
            ) {
                let start_line = engine.line_of_byte(pos);
                let raw = engine.read_line(start_line);
                let text = raw.text.trim_end_matches('\n').trim_end_matches('\r');
                let rel = pos.saturating_sub(raw.start_byte).min(text.len() as u64) as usize;
                let start_col = byte_col_to_char(text, rel);
                // Preserve the selection's line span + last-line column.
                let end_line = start_line + (old_end_line - old_start_line);
                let start_byte = annotation_byte(&engine, start_line, start_col);
                let end_byte = annotation_byte(&engine, end_line, old_end_col);
                if self.annotation_store.update_position(
                    &path,
                    id,
                    start_byte,
                    end_byte,
                    start_line,
                    end_line,
                    start_col,
                    old_end_col,
                ) {
                    changed = true;
                }
            } else if self.annotation_store.set_stale(&path, id, true) {
                changed = true;
            }
        }
        if changed {
            let _ = self.annotation_store.save();
            self.reload_annotations();
            log_info!("app", "批注重锚定完成 (改动 {} 条)", changed);
        }
    }

    // -------------------------------------------------------------------
    // Edit mode (编辑)
    // -------------------------------------------------------------------

    /// Flip the edit-mode toggle. Editing is off by default (pure preview).
    pub fn toggle_edit_mode(&mut self) {
        self.edit_mode = !self.edit_mode;
        if self.edit_mode {
            if self.edit_cursor.is_none() {
                // Put the caret at the top of the visible viewport.
                let top = if let Some(m) = &self.visual_model {
                    let vt = (self.scroll_y / m.row_h as f64).floor() as u64;
                    m.visual_to_line(vt)
                } else {
                    (self.scroll_y / self.fallback_row_h()).floor() as u64
                };
                self.edit_cursor = Some((top, 0));
            }
            log_info!("app", "进入编辑模式");
            self.flash_status("编辑模式已开启", 2);
        } else {
            self.edit_cursor = None;
            self.edit_ime_preedit.clear();
            log_info!("app", "退出编辑模式");
            self.flash_status("编辑模式已关闭", 2);
        }
    }

    pub fn is_modified(&self) -> bool {
        self.engine.as_ref().map_or(false, |arc| arc.lock().is_modified())
    }

    /// File is larger than the in-place-editing cap → save only via 另存为.
    pub fn file_exceeds_edit_cap(&self) -> bool {
        self.engine.as_ref().map_or(false, |arc| {
            arc.lock().mmap.size() > self.config.gui.max_editable_bytes
        })
    }

    /// Ctrl+S: save edits back to the original (background). Over the cap,
    /// routes to 另存为 instead. A NEW file has no original to write back to —
    /// saving it prompts for a destination. The write never blocks the UI.
    pub fn save_file(&mut self) {
        if self.edit_saving {
            return;
        }
        if self.is_new_file {
            // Saving an unsaved new file = choosing where to put it.
            self.save_as_requested = true;
            return;
        }
        if !self.is_modified() {
            return;
        }
        if self.file_exceeds_edit_cap() {
            self.flash_status("文件过大，原文件写回被禁用，请用另存为", 3);
            self.save_as_requested = true;
            return;
        }
        if let Some(arc) = self.engine.as_mut() {
            if arc.lock().submit_save() {
                self.edit_saving = true;
                log_info!("app", "提交后台保存");
                self.flash_status("正在保存…", 2);
            }
        }
    }

    /// Ctrl+Shift+S / toolbar: pick a destination, then save a copy there.
    pub fn request_save_as(&mut self) {
        self.save_as_requested = true;
    }

    /// Write the current edits to `dst` (background). The working file and its
    /// edit state stay untouched.
    pub fn save_file_as_to(&mut self, dst: PathBuf) {
        if self.edit_saving {
            return;
        }
        if let Some(arc) = self.engine.as_mut() {
            if arc.lock().submit_save_as(dst.clone()) {
                self.edit_saving = true;
                self.last_save_path = Some(dst);
                log_info!("app", "提交另存为");
                self.flash_status("正在另存…", 2);
            }
        }
    }

    /// Poll a background save; returns true once it finished. Re-anchors
    /// annotations only after an IN-PLACE save (the file changed).
    pub fn poll_save(&mut self) -> bool {
        let was_copy = self.engine.as_ref().map_or(false, |arc| arc.lock().save_is_copy);
        let (done, msg, ok) = match self.engine.as_mut() {
            Some(arc) => arc.lock().poll_bg_save(),
            None => (false, None, false),
        };
        if let Some(m) = msg {
            log_info!("app", "保存结果: {}", m);
            self.flash_status(m, 3);
        }
        if done {
            self.edit_saving = false;
            // A new file's first save finished: the temp backing file is no
            // longer needed — switch the working document to the real path
            // (fresh engine → fresh index, path, recent-list entry).
            if self.is_new_file && ok {
                let tmp = self.path.clone();
                let dst = self.last_save_path.clone();
                let prev_cursor = self.edit_cursor;
                self.is_new_file = false;
                self.engine = None;
                self.path = None;
                if let Some(t) = tmp {
                    let _ = std::fs::remove_file(&t);
                }
                if let Some(d) = dst {
                    self.open_file(d);
                }
                // Keep editing where the user left off (open_file reset the
                // caret to the top).
                if let Some(c) = prev_cursor {
                    self.edit_cursor = Some(c);
                }
                return done;
            }
            // The saved file may have fewer lines than the edited view; clamp
            // the caret so it still points somewhere valid.
            if let Some((line, col)) = self.edit_cursor {
                let total = self
                    .engine
                    .as_ref()
                    .map(|arc| arc.lock().effective_line_count())
                    .unwrap_or(0);
                if total == 0 {
                    self.edit_cursor = None;
                } else {
                    self.editor_set_cursor(line.min(total - 1), col);
                }
            }
            // An in-place save rewrote the file: re-anchor its annotations ONCE
            // the background index for the new content is ready (line numbers
            // come from that index). Deferred to `poll_background_tasks`.
            if !was_copy && ok {
                self.pending_reanchor = true;
            }
        }
        done
    }

    /// If the current file is modified, intercept a leave action and confirm.
    /// Returns true when intercepted (the action will run after confirmation).
    pub fn request_discard(&mut self, action: DiscardAction) -> bool {
        if self.is_modified() {
            log_info!("app", "有未保存修改，需确认后再继续");
            self.pending_discard = Some(action);
            true
        } else {
            false
        }
    }

    /// The discard-confirm dialog's destructive button: drop edits and proceed.
    pub fn confirm_discard(&mut self) {
        if let Some(action) = self.pending_discard.take() {
            match action {
                DiscardAction::Open(p) => {
                    log_debug!("app", "确认丢弃修改并打开: {}", p.display());
                    self.drop_new_file();
                    self.open_file(p);
                }
                DiscardAction::New => {
                    log_debug!("app", "确认丢弃修改并新建");
                    self.create_new_file();
                }
                DiscardAction::Close => {
                    log_debug!("app", "确认丢弃修改并关闭");
                    self.close_file();
                }
                DiscardAction::Exit => {
                    log_debug!("app", "确认丢弃修改并退出");
                    self.drop_new_file();
                    self.exit_requested = true;
                }
            }
        }
    }

    /// Open another file, confirming first if the current one is modified.
    pub fn try_open(&mut self, path: PathBuf) {
        if self.request_discard(DiscardAction::Open(path.clone())) {
            return;
        }
        // Releasing an unsaved new file must happen BEFORE `open_file` swaps
        // the engine out (Windows can't delete a file that's still mmap'd).
        self.drop_new_file();
        self.open_file(path);
    }

    /// Create a new blank file: confirm first if the current one is modified,
    /// then open an empty temp file as the working document.
    pub fn request_new_file(&mut self) {
        if self.request_discard(DiscardAction::New) {
            return;
        }
        self.create_new_file();
    }

    /// Open a fresh, empty document backed by a temp file. The file stays
    /// marked "新文件" until the user saves it to a real destination.
    pub(crate) fn create_new_file(&mut self) {
        log_info!("app", "新建文件");
        self.drop_new_file();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!("qview-new-{nanos}.txt"));
        // Seed with a single '\n' so the engine sees ONE editable empty line
        // (a 0-byte file has no line to insert/replace into). Saving writes it
        // back normally — an untouched new file becomes a one-empty-line file.
        if std::fs::write(&tmp, b"\n").is_err() {
            self.flash_status("新建文件失败: 无法创建临时文件", 4);
            return;
        }
        self.is_new_file = true;
        self.open_file(tmp.clone());
        if self.engine.is_none() {
            // open_file failed — undo the new-file state and remove the temp.
            self.is_new_file = false;
            self.path = None;
            let _ = std::fs::remove_file(&tmp);
            self.flash_status("新建文件失败", 4);
            return;
        }
        // A brand-new file starts in edit mode so the user can type immediately.
        self.edit_mode = true;
        self.edit_cursor = Some((0, 0));
        log_info!("app", "新文件自动进入编辑模式");
    }

    /// Discard an unsaved new file: drop the engine (releases the mmap on the
    /// temp backing file), clear the path and delete the temp from disk.
    /// No-op when no new file is open.
    fn drop_new_file(&mut self) {
        if !self.is_new_file {
            return;
        }
        let tmp = self.path.clone();
        self.is_new_file = false;
        self.engine = None;
        self.path = None;
        if let Some(t) = tmp {
            let _ = std::fs::remove_file(&t);
        }
    }

    /// Close the current file, confirming first if it is modified.
    pub fn try_close(&mut self) {
        if self.request_discard(DiscardAction::Close) {
            return;
        }
        self.close_file();
    }

    /// Exit the app, confirming first if the current file is modified.
    pub fn request_exit(&mut self) {
        if self.is_modified() {
            self.pending_discard = Some(DiscardAction::Exit);
        } else {
            // An unsaved empty new file has no edits worth confirming — just
            // clean up its temp file before closing.
            self.drop_new_file();
            self.exit_requested = true;
        }
    }

    /// Return the text of the line currently at the top of the viewport.
    pub fn current_line_text(&self) -> Option<String> {
        let engine = self.engine.as_ref()?.lock();
        let line_no = if let Some(m) = &self.visual_model {
            let vt = (self.scroll_y / m.row_h as f64).floor() as u64;
            m.visual_to_line(vt)
        } else {
            (self.scroll_y / self.fallback_row_h()).floor() as u64
        };
        Some(engine.read_line(line_no).text)
    }

    pub fn goto_line(&mut self) {
        if let Ok(n) = self.goto_input.parse::<u64>() {
            if n > 0 {
                // 先短临界区取 total（避免持锁时再 &mut self）
                let total = self.engine.as_ref().map(|arc| arc.lock().effective_line_count());
                if let Some(total) = total {
                    if n <= total {
                        let target = n.saturating_sub(1);
                        log_debug!("app", "跳转到行: {} (offset={})", n, target);
                        self.scroll_to_line_start(target);
                        self.flash_status(format!("跳转到第 {} 行", n), 3);
                    } else {
                        log_debug!("app", "跳转行号越界: 输入 {} 行, 文件共 {} 行", n, total);
                        self.flash_status(format!("跳转失败: 文件只有 {} 行", total), 4);
                    }
                }
            }
        }
        self.goto_input.clear();
    }

    // -------------------------------------------------------------------
    // Agent ViewIntent 投影（架构 §9.2/§9.3）
    // -------------------------------------------------------------------

    /// `ViewIntent::FocusLine`：跳到指定行（主视图同步；仅用户点击时间线触发）。
    pub fn agent_jump_to_line(&mut self, line: u64) {
        let Some(arc) = self.engine.as_ref() else {
            self.flash_status("未打开文件，无法跳转", 2);
            return;
        };
        let total = arc.lock().effective_line_count();
        if total == 0 {
            return;
        }
        let l = line.min(total.saturating_sub(1));
        self.scroll_to_line_start(l);
        self.h_scroll = 0.0;
        log_debug!("agent", "ViewIntent::FocusLine → 行 {}", l + 1);
        self.flash_status(format!("器灵: 跳到第 {} 行", l + 1), 3);
    }

    /// `ViewIntent::ApplyFilter`：应用 Agent 视图过滤器（主视图淡化不匹配行）。
    pub fn agent_set_filter(&mut self, filter: qview_application::protocol::view_intent::FilterSpec) {
        log_debug!("agent", "ViewIntent::ApplyFilter → {filter:?}");
        self.agent_filter = Some(filter);
        self.flash_status("器灵: 已应用临时过滤器（点击时间线可清除）", 3);
    }

    /// 清除 Agent 视图过滤器。
    pub fn agent_clear_filter(&mut self) {
        if self.agent_filter.take().is_some() {
            log_debug!("agent", "清除 Agent 视图过滤器");
            self.flash_status("已清除器灵过滤器", 2);
        }
    }

    /// 当前文档上下文提示（**总是注入**，避免 AI 在无文件时瞎猜 document_id）。
    /// - 有注册文件：给出 document_id，让工具填对。
    /// - 打开了文件但未注册（如超大小限制）：如实说明，AI 知道访问不了。
    /// - 没打开文件：明确禁止调用文档工具，直接回答。
    pub fn agent_doc_hint(&self) -> String {
        match (self.agent_doc_id, self.path.as_ref()) {
            (Some(id), Some(path)) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                format!(
                    "当前文档: \"{name}\" (document_id={})\n工具请使用该 id。",
                    id.get()
                )
            }
            (None, Some(path)) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                format!(
                    "当前打开了文件 \"{name}\"，但未注册给 AI（可能超过文档大小限制），AI 无法访问其内容。"
                )
            }
            _ => "当前没有打开任何文件。不要调用 get_document_info / search_text / read_context 等文档工具，直接回答用户。".to_string(),
        }
    }

    /// 组装多轮对话历史（把转录里之前的 User / Agent 轮次转成文本块）。
    ///
    /// 用于 `start_session_with` 的 `conversation_history`：同一对话里 LLM 每轮都要
    /// 能看到前几轮，否则每个问题是"失忆"的新会话（AI 标准会话流）。
    ///
    /// - 排除**即将作为 query 发送的最后一条 User**（它由任务自身携带，避免重复）。
    /// - 跳过工具行 / 视图意图 / 系统 Note（噪声不进上下文）。
    /// - 上限 ~20000 字符，只保留**最近的**轮次（倒序收集再反转），防止长对话烧 token。
    ///   最终截断由 runtime 按阶段做（分类器 2000 / ReAct 12000），这里放宽让窗口有料。
    pub fn agent_conversation_history(&self) -> Option<String> {
        use crate::agent::ChatMsg;
        const MAX_CHARS: usize = 20000;

        let msgs = self.agent_state.transcript.lock();
        let end = if matches!(msgs.last(), Some(ChatMsg::User { .. })) {
            msgs.len().saturating_sub(1)
        } else {
            msgs.len()
        };
        let mut parts: Vec<String> = Vec::new();
        let mut chars = 0usize;
        for m in msgs[..end].iter().rev() {
            let line = match m {
                ChatMsg::User { text } => format!("用户：{text}"),
                ChatMsg::Agent { text, .. } => format!("小Q：{text}"),
                _ => continue,
            };
            if chars + line.len() > MAX_CHARS {
                break;
            }
            chars += line.len();
            parts.push(line);
        }
        drop(msgs);
        if parts.is_empty() {
            return None;
        }
        parts.reverse();
        Some(parts.join("\n"))
    }

    /// 调试：器灵窗口 / 顶条 / 内容最小矩形 vs 期望尺寸，值变化才打日志。
    ///
    /// `content` = 内容区实际最小矩形（`scope` 的 min_rect）。若 `内容宽` 大于
    /// `fixed 宽`，说明有内容把布局撑宽了（"内容超出背景"）；正常应为 0 或负。
    pub fn debug_agent_rects(
        &mut self,
        win: egui::Rect,
        header: egui::Rect,
        content: egui::Rect,
        fixed: egui::Vec2,
    ) {
        const TOL: f32 = 0.5;
        let win_changed = self.debug_agent_win_rect.map_or(true, |p| {
            (p.min - win.min).length() > TOL || (p.size() - win.size()).length() > TOL
        });
        let hdr_changed = self.debug_agent_header_rect.map_or(true, |p| {
            (p.min - header.min).length() > TOL || (p.size() - header.size()).length() > TOL
        });
        let content_changed = self.debug_agent_content_rect.map_or(true, |p| {
            (p.size() - content.size()).length() > TOL
        });
        self.debug_agent_win_rect = Some(win);
        self.debug_agent_header_rect = Some(header);
        self.debug_agent_content_rect = Some(content);
        if win_changed || hdr_changed || content_changed {
            log_info!(
                "agent",
                "器灵窗口调试: window={}x{}@({},{}) header={}x{}@({},{}) 内容={}x{} fixed={}x{} 差={}x{}",
                win.width(), win.height(), win.min.x as i32, win.min.y as i32,
                header.width(), header.height(), header.min.x as i32, header.min.y as i32,
                content.width(), content.height(),
                fixed.x, fixed.y,
                (win.width() - fixed.x) as i32, (win.height() - fixed.y) as i32,
            );
        }
    }

    /// 触发历史会话列表重新加载（后台线程拉取，完成后写回 `history_sessions`）。
    /// 只读共享状态（Arc 锁内写），`&self` 即可 —— 悬浮窗口渲染闭包里可直接调用。
    pub fn request_history_reload(&self) {
        *self.history_sessions.lock() = None; // 标记加载中
        let Some(store) = self.store.clone() else { return };
        let target = self.history_sessions.clone();
        self.spawn_tokio(async move {
            let list = store.recent_sessions(50).unwrap_or_default();
            *target.lock() = Some(list);
        });
    }

    /// 后台加载一个历史会话，把消息映射回 `transcript`（只读回看，不重放）。
    pub fn open_history_session(&self, session_id: &str) {
        use crate::agent::ChatMsg;
        use qview_store::StoreRole;

        // 回看历史会替换当前转录 → 当前"对话上下文"失效：下一条消息开启全新会话
        //（否则会用历史会话的消息去续一个已结束的 session_id，产生错乱）。
        *self.agent_state.conversation_id.lock() = None;
        let Some(store) = self.store.clone() else { return };
        let sid = session_id.to_string();
        let transcript = self.agent_state.transcript.clone();
        let scroll = self.agent_state.scroll_to_bottom.clone();
        let tool_log = self.agent_state.tool_log.clone();
        self.spawn_tokio(async move {
            // 同步加载该会话的工具调用记录（浮层可回看）
            let calls = store.tool_calls_for_session(&sid).unwrap_or_default();
            *tool_log.lock() = calls;
            match store.load_session(&sid) {
                Ok(Some(sess)) => {
                    let mut msgs: Vec<ChatMsg> =
                        Vec::with_capacity(sess.messages.len() + 1);
                    msgs.push(ChatMsg::Note {
                        text: format!("📂 历史会话：{}", sess.meta.goal),
                    });
                    for m in sess.messages {
                        match m.role {
                            StoreRole::User => msgs.push(ChatMsg::User { text: m.content }),
                            StoreRole::Assistant => {
                                msgs.push(ChatMsg::Agent { text: m.content, is_error: false });
                            }
                            StoreRole::System => msgs.push(ChatMsg::Note { text: m.content }),
                            StoreRole::Tool => {
                                let detail: String = m.content.chars().take(80).collect();
                                msgs.push(ChatMsg::Note { text: format!("🛠 {detail}") });
                            }
                        }
                    }
                    *transcript.lock() = msgs;
                    *scroll.lock() = true;
                }
                Ok(None) => {
                    let mut t = transcript.lock();
                    t.push(ChatMsg::Note { text: "⚠ 会话不存在或已被清理".into() });
                }
                Err(e) => crate::log_error!("agent", "加载历史会话失败: {e:#}"),
            }
        });
    }

    /// 新建会话：取消进行中的任务、清空转录与投影状态，回到干净起始态。
    /// 悬浮窗口底部状态栏「✚ 新建会话」触发。
    pub fn agent_new_session(&mut self) {
        use qview_agent::event::Phase;

        // 取消进行中的 session（旧 handle 若在跑，先停掉）
        if let (Some(h), Some(sid)) = (
            self.agent_state.handle.lock().clone(),
            self.agent_state.active_session.lock().clone(),
        ) {
            let h2 = h.clone();
            self.spawn_tokio(async move {
                let _ = h2.cancel_within(sid, Duration::from_secs(1)).await;
            });
        }
        *self.agent_state.active_session.lock() = None;
        // 新会话：下次发送将开启全新 session_id（一次对话一个会话）
        *self.agent_state.conversation_id.lock() = None;
        *self.agent_state.in_flight_tool.lock() = None;
        *self.agent_state.pending_proposal.lock() = None;
        *self.agent_state.current_phase.lock() = Phase::Done;
        self.agent_state.transcript.lock().clear();
        self.agent_state.events.lock().clear();
        *self.agent_state.tool_log.lock() = Vec::new();
        // 投影状态是 per-session 的，一并清空
        self.agent_highlights.clear();
        self.agent_filter = None;
        // 关闭历史 / 工具记录浮层（新会话从干净态开始）
        self.agent_show_history = false;
        self.agent_show_tool_log = false;
        // 一句欢迎语，提示可开始
        self.agent_state.transcript.lock().push(crate::agent::ChatMsg::Agent {
            text: "✨ 新会话已开始 — 直接开问吧！".into(),
            is_error: false,
        });
        log_info!("agent", "新建会话");
    }

    // -------------------------------------------------------------------
    // 最近打开 / 搜索历史（config.json → store 迁移）
    // -------------------------------------------------------------------

    /// 一次性迁移旧 config.json 里的「最近打开 / 搜索历史」到本地存储，然后清空 config。
    ///
    /// 顺序保持：config 列表是「最近优先」，给递减的时间戳让 `load_files` /
    /// `recent_searches` 的倒序排序还原原顺序。启动时同步写（几笔小事务，
    /// 非热路径，避免异步与随后的 reload 竞态丢数据）。
    fn migrate_legacy_recents(&mut self) {
        let Some(store) = self.store.clone() else { return };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut changed = false;
        if !self.config.recent_files.is_empty() {
            let files: Vec<qview_store::FileMeta> = self
                .config
                .recent_files
                .iter()
                .enumerate()
                .map(|(i, p)| qview_store::FileMeta {
                    path: p.display().to_string(),
                    last_opened_at_ms: now.saturating_sub(i as u64),
                    open_count: 0,
                    size_bytes: 0,
                    encoding: String::new(),
                })
                .collect();
            self.config.recent_files.clear();
            for f in &files {
                let _ = store.record_file(f);
            }
            changed = true;
            log_info!("app", "迁移最近打开 {} 条到本地存储", files.len());
        }
        if !self.config.search_history.is_empty() {
            let queries: Vec<String> = self.config.search_history.drain(..).collect();
            for (i, q) in queries.iter().enumerate() {
                // 直接写库（时间戳递减还原顺序）；不调 record_search（它每次 +1 计数）。
                let entry = qview_store::SearchEntry {
                    query: q.clone(),
                    last_used_at_ms: now.saturating_sub(i as u64),
                    use_count: 1,
                };
                let _ = store.save_search_entry(&entry);
            }
            changed = true;
            log_info!("app", "迁移搜索历史 {} 条到本地存储", queries.len());
        }
        if changed {
            self.save_config();
        }
    }

    /// 从 store 载入最近打开文件到内存缓存（启动 / 清空后调用；同步小读）。
    /// 过滤已不存在的路径（菜单里不展示指向已删文件的死条目）。
    pub fn reload_recent_files(&self) {
        let mut cache = self.recent_files.lock();
        cache.clear();
        if let Some(store) = self.store.clone() {
            if let Ok(files) = store.load_files(10) {
                for f in files {
                    let p = PathBuf::from(&f.path);
                    if p.exists() {
                        cache.push(p);
                    }
                }
            }
        }
    }

    /// 从 store 载入搜索历史到内存缓存（启动 / 清空后调用；同步小读）。
    pub fn reload_search_history(&self) {
        let mut cache = self.search_history.lock();
        cache.clear();
        if let Some(store) = self.store.clone() {
            if let Ok(entries) = store.recent_searches(20) {
                cache.extend(entries.into_iter().map(|e| e.query));
            }
        }
    }

    pub fn loading_state(&self) -> Option<(f32, String)> {
        let engine = self.engine.as_ref()?.lock();
        if let Some(ref p) = engine.index_progress {
            let frac = Self::progress_frac(p).unwrap_or(0.0);
            Some((frac, p.clone()))
        } else if let Some(ref p) = engine.search_progress {
            let frac = Self::progress_frac(p).unwrap_or(0.0);
            Some((frac, p.clone()))
        } else if let Some(pct) = engine.save_progress {
            Some((pct as f32 / 100.0, format!("正在保存… {}%", pct)))
        } else {
            None
        }
    }

    /// Handle Ctrl+C for log-selection copy BEFORE any widgets run.
    /// Also services deferred copies from the right-click context menu.
    fn handle_copy_shortcut(&mut self, ctx: &Context) {
        // Check for deferred copy (right-click context menu in viewer).
        let pending = ctx.data_mut(|d| {
            let val = d.get_persisted::<bool>(egui::Id::new("pending_copy")).unwrap_or(false);
            if val {
                d.insert_persisted(egui::Id::new("pending_copy"), false);
            }
            val
        });

        // ── Ctrl+C detection ──────────────────────────────────────────
        // On Windows, winit/egui converts Ctrl+C into a system Copy command
        // internally and does NOT expose the C key-press event to the input
        // event list.  We therefore check the physical key state directly
        // via GetAsyncKeyState, bypassing the entire winit/egui stack.
        #[cfg(windows)]
        let (ctrl_held, c_just_pressed) = unsafe {
            extern "system" {
                fn GetAsyncKeyState(vk: i32) -> i16;
            }
            const VK_CONTROL: i32 = 0x11;
            const VK_C: i32 = 0x43;
            let ctrl = (GetAsyncKeyState(VK_CONTROL) as u16) & 0x8000 != 0;
            let c = (GetAsyncKeyState(VK_C) as u16) & 0x0001 != 0;
            (ctrl, c)
        };
        #[cfg(not(windows))]
        let (ctrl_held, c_just_pressed) = (false, false);

        // Egui-level fallback (works on non-Windows, or when events aren't
        // intercepted).
        //
        // On macOS, egui-winit intercepts Cmd+C and turns it into an
        // `egui::Event::Copy`, returning WITHOUT pushing a `Key::C` key event
        // to the input list — so `key_pressed(Key::C)` never fires there.
        // Detect the copy event directly so the log-selection copy works on
        // macOS too.  (Windows keeps the GetAsyncKeyState path as a backup;
        // both paths are idempotent.)
        let (egui_ctrl, egui_cmd, egui_shift, egui_c, egui_ins, copy_event) = ctx.input(|i| {
            (
                i.modifiers.ctrl,
                i.modifiers.command,
                i.modifiers.shift,
                i.key_pressed(egui::Key::C),
                i.key_pressed(egui::Key::Insert),
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
            )
        });

        let ctrl_effective = ctrl_held || egui_ctrl || egui_cmd;
        let c_effective = c_just_pressed || egui_c || copy_event;
        let shift = egui_shift;

        let do_copy = pending
            || (ctrl_effective && !shift && c_effective)
            || (ctrl_effective && egui_ins);

        if !do_copy {
            return;
        }

        if let Some(text) = self.copy_selection_text() {
            // Defer the actual copy to end-of-frame so our copy overrides
            // whatever the search/goto TextEdits write in the same frame.
            log_debug!("app", "复制选中: {} 个字符", text.len());
            self.pending_copy_text = Some(text);
        } else {
            self.flash_status("未能复制（选中区域无效）", 2);
        }
    }

    /// Apply any deferred copy at the END of the frame, after all widgets
    /// have had a chance to handle Ctrl+C.  This ensures our log-content
    /// copy overrides whatever the search/goto TextEdits wrote.
    fn flush_copy(&mut self, ctx: &Context) {
        if let Some(text) = self.pending_copy_text.take() {
            ctx.copy_text(text.clone());
            self.flash_status(format!("已复制 {} 个字符到剪贴板", text.len()), 3);
        }
    }

    /// Set a status-bar message that auto-clears after `secs` seconds.
    pub fn flash_status(&mut self, msg: impl Into<String>, secs: u64) {
        self.status_msg = msg.into();
        self.status_msg_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(secs));
    }

    fn clear_expired_status(&mut self) {
        if let Some(deadline) = self.status_msg_until {
            if std::time::Instant::now() >= deadline {
                self.status_msg.clear();
                self.status_msg_until = None;
            }
        }
    }

    fn progress_frac(text: &str) -> Option<f32> {
        let pct = text.rsplit('%').next()?.rsplit(' ').next()?;
        let n: f32 = pct.parse().ok()?;
        Some((n / 100.0).clamp(0.0, 1.0))
    }

    /// Clear all cached data except the currently open file's index.
    /// Returns `(deleted_count, deleted_bytes)`.
    pub fn clear_cache(&mut self) -> (usize, u64) {
        let mut deleted_count = 0usize;
        let mut deleted_bytes = 0u64;

        // Figure out which index file belongs to the currently open file.
        let keep_path: Option<std::path::PathBuf> = self
            .path
            .as_ref()
            .map(|p| self.config.engine.index_path(p));

        // Delete .qli files from the index directory, skipping the current
        // file's index.
        if let Some(ref index_dir) = self.config.engine.index_dir {
            if let Ok(entries) = std::fs::read_dir(index_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("qli") {
                        let skip = keep_path.as_ref().map(|kp| kp == &path).unwrap_or(false);
                        if skip {
                            continue;
                        }
                        if let Ok(meta) = entry.metadata() {
                            deleted_bytes += meta.len();
                        }
                        if std::fs::remove_file(&path).is_ok() {
                            deleted_count += 1;
                        }
                    }
                }
            }
        }

        // 清空最近打开 + 搜索历史：内存缓存立即可见，store 表后台清。
        let n_recent = self.recent_files.lock().len();
        let n_hist = self.search_history.lock().len();
        log_debug!("app", "清空缓存: 删除 {} 个索引文件, 释放 {}, 同时清空最近文件({}条)和搜索历史({}条)",
            deleted_count, crate::viewer::human_bytes(deleted_bytes),
            n_recent, n_hist);
        self.recent_files.lock().clear();
        self.search_history.lock().clear();
        if let Some(store) = self.store.clone() {
            self.spawn_tokio(async move {
                let _ = store.clear_files();
                let _ = store.clear_searches();
            });
        }
        self.save_config();

        (deleted_count, deleted_bytes)
    }
}

// ---------------------------------------------------------------------------
// Agent 聊天转录辅助（free functions，供 render_agent_panel 使用）
// ---------------------------------------------------------------------------

/// 追加一条聊天消息（上限 500 条，超出丢最旧）。
fn push_chat(state: &AgentPanelState, msg: crate::agent::ChatMsg) {
    let mut t = state.transcript.lock();
    t.push(msg);
    if t.len() > 500 {
        let excess = t.len() - 500;
        t.drain(0..excess);
    }
}

/// Convert a character index in `s` to its byte index (0-based).  Clamps to the
/// string length for out-of-range / at-EOL anchors.
pub fn char_col_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Convert a byte index in `s` to its character column (0-based).  Clamps to a
/// char boundary; out-of-range byte indices land at the end.
fn byte_col_to_char(s: &str, byte_idx: usize) -> usize {
    let b = byte_idx.min(s.len());
    let mut b = b;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    s[..b].chars().count()
}

/// Absolute file byte offset of the character at (line, col).  Columns are
/// character indices in the CR/LF-stripped decoded line, matching the viewer's
/// selection model.  `read_line`'s text keeps the trailing newline, so strip it
/// before mapping — the stripped prefix shares the same `start_byte`.
fn annotation_byte(engine: &Engine, line: u64, col: usize) -> u64 {
    let raw = engine.read_line(line);
    let text = raw.text.trim_end_matches('\n').trim_end_matches('\r');
    let nchars = text.chars().count();
    let c = col.min(nchars);
    raw.start_byte + char_col_to_byte(text, c) as u64
}

// ---------------------------------------------------------------------------
// eframe::App
// ---------------------------------------------------------------------------

impl eframe::App for QLogApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // ---- -1. 内存诊断 — 启动后延迟打印 2 份快照 ----
        if let Some(start) = self.boot_instant {
            let elapsed = start.elapsed();
            if !self.mem_snapshot_taken_2s && elapsed >= Duration::from_secs(2) {
                crate::mem_diag::write_report("2s after launch", self);
                self.mem_snapshot_taken_2s = true;
            }
            if !self.mem_snapshot_taken_5s && elapsed >= Duration::from_secs(5) {
                crate::mem_diag::write_report("5s after launch", self);
                self.mem_snapshot_taken_5s = true;
            }
        }

        // ---- 0. Apply current theme (every frame — cheap, ensures it sticks) ----
        self.themes[self.current_theme_idx].apply_to(ctx);

        // ---- 0b. Clear expired status messages ----
        self.clear_expired_status();

        // ---- 0b2. Exit intercept — confirm before dropping unsaved edits ----
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if self.exit_requested {
            self.exit_requested = false;
            self.exit_confirmed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if close_requested && !self.exit_confirmed && self.is_modified() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_discard = Some(DiscardAction::Exit);
        }

        // ---- 0c. Handle dropped files ----
        self.handle_dropped_files(ctx);

        // ---- 0c2. Ctrl+C copy selection (MUST run before toolbar widgets
        //            so TextEdit doesn't consume the key first). ----
        self.handle_copy_shortcut(ctx);

        // ---- 1. Poll background tasks ----
        self.poll_background_tasks();

        // ---- 1b. Poll a background save (re-anchors annotations on finish) ----
        self.poll_save();

        // ---- 1c. Save-as file dialog (user-triggered, blocking native picker) ----
        if self.save_as_requested {
            self.save_as_requested = false;
            let default_name = if self.is_new_file {
                "未命名.txt".to_string()
            } else {
                self.path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("out.log")
                    .to_string()
            };
            if let Some(dst) = rfd::FileDialog::new().set_file_name(default_name).save_file() {
                self.save_file_as_to(dst);
            } else {
                log_debug!("app", "取消另存为");
            }
        }

        // ---- 2. Menu bar ----
        crate::menu::render_menu_bar(ctx, self);

        // ---- 3. Toolbar ----
        crate::toolbar::render_toolbar(ctx, self);

        // ---- 3b. Edit-mode keyboard input (only when no widget wants the
        //          keyboard, i.e. the log content has focus). ----
        if self.edit_mode && !self.edit_saving {
            crate::editor::handle_edit_keys(ctx, self);
        }

        // ---- 4. Main viewer (virtual scroll) ----
        crate::viewer::render_central_panel(ctx, self);

        // ---- 5. Status bar ----
        crate::statusbar::render_status_bar(ctx, self);

        // ---- 5a. 发布视口快照（get_viewport 工具读）----
        // 在 render_central_panel 之后调用，此时 first/last_visible_line 已更新。
        {
            let snap = if self.path.is_some() {
                Some(qview_application::protocol::ViewportSnapshot {
                    first_visible_line: self.first_visible_line,
                    last_visible_line: self.last_visible_line,
                    selection: self.selection.map(|(sl, _sc, el, _ec)| (sl, el)),
                })
            } else {
                None
            };
            *self.viewport_info.lock() = snap;
        }

        // ---- 5b. Agent panel (right side) ----
        self.render_agent_panel(ctx);

        // ---- 6. Dialogs ----
        crate::dialogs::render_all(ctx, self);

        // ---- 6b. Agent 审批弹窗（主窗口也渲染）----
        // 关键：器灵窗口是独立子视口，只在小窗口的 child_ctx 渲染弹窗时，主窗口
        // 不重绘 → 用户看不到审批 → 30s 超时。这里在主 ctx 再渲染一次，保证弹窗
        // 出现在用户眼前（无论是否在器灵窗口）。
        crate::agent::approval::show_modal(ctx, &self.agent_state, &*self);

        // ---- 7. Global shortcuts ----
        self.handle_shortcuts(ctx);

        // ---- 7b. Flush deferred copy (after all widgets, overrides TextEdit) ----
        self.flush_copy(ctx);

        // ---- 8. Request next frame for bg polling ----
        // Only request frequent repaints while background tasks are running.
        // When idle, wake just before the status message expires to clear it;
        // otherwise egui sleeps until the next OS event (0 % CPU).
        let has_bg_work = self.engine.as_ref().map_or(false, |arc| {
            let e = arc.lock();
            e.bg_indexer.is_some() || e.bg_search.is_some()
        });
        if has_bg_work {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if let Some(deadline) = self.status_msg_until {
            let now = std::time::Instant::now();
            if deadline > now {
                let delay = deadline.duration_since(now) + Duration::from_millis(50);
                ctx.request_repaint_after(delay);
            }
        }

        // ---- 9. Track window state ----
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            let size = rect.size();
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            self.config.set_window_state([size.x, size.y], maximized);
        }
    }
}

// ---------------------------------------------------------------------------
// Background task polling
// ---------------------------------------------------------------------------

impl QLogApp {
    fn poll_background_tasks(&mut self) {
        // ---- Collect all data from engine first (avoids borrow conflicts) ----
        // Instant is Copy — snapshot the search start before borrowing `self`.
        let search_started = self.search_started_at;
        let (index_data, search_data) = {
            let mut engine = match &mut self.engine {
                Some(arc) => arc.lock(),
                None => return,
            };

            // index
            let (index_done, index_msg) = engine.poll_bg_index();
            let index_cancelled = index_msg
                .as_ref()
                .map(|m| m.contains("cancelled"))
                .unwrap_or(false);
            let index_failed = index_msg
                .as_ref()
                .map(|m| m.contains("failed"))
                .unwrap_or(false);
            let idx_info = if index_done && !index_cancelled && !index_failed {
                Some((engine.effective_line_count(), engine.mmap.size()))
            } else {
                None
            };
            // A successful background rebuild means the engine's index now
            // matches the current mmap (e.g. right after an in-place save).
            let index_succeeded = index_done && !index_cancelled && !index_failed;

            // search
            let (search_done, search_msg) = engine.poll_bg_search();
            let search_cancelled = search_msg
                .as_ref()
                .map(|m| m.contains("cancelled"))
                .unwrap_or(false);
            let srch_info = if search_done {
                if search_cancelled {
                    Some((Vec::new(), Vec::new(), 0, String::new(), "已取消".to_string(), false))
                } else if !engine.search.is_empty() {
                    let total = engine.search.len();
                    let status = if total == 0 {
                        "无匹配".to_string()
                    } else {
                        format!("1/{} 条匹配", total)
                    };
                    let elapsed_str = search_started.map_or_else(String::new, |t| {
                        let d = t.elapsed();
                        if d.as_secs() >= 1 {
                            format!("耗时={:.1}s", d.as_secs_f64())
                        } else {
                            format!("耗时={}ms", d.as_millis())
                        }
                    });
                    log_info!("app", "搜索完成: \"{}\" → {} 条匹配 (存储{}条, 采样间隔{}) {}",
                        engine.search.query(), total, engine.search.stored_count(),
                        engine.search.sample_interval(), elapsed_str);
                    Some((Vec::new(), Vec::new(), total, engine.search.query(), status, true))
                } else {
                    Some((Vec::new(), Vec::new(), 0, String::new(), "❌ 无匹配".to_string(), false))
                }
            } else {
                None
            };

            (IndexPoll { msg: index_msg, info: idx_info, succeeded: index_succeeded },
             SearchPoll { msg: search_msg, info: srch_info })
        };

        // ---- Apply results (no engine borrow) ----

        // index
        if let Some(msg) = index_data.msg {
            self.flash_status(msg, 5);
        }
        if let Some((lines, size)) = index_data.info {
            let size_str = crate::viewer::human_bytes(size);
            let elapsed_str = self.index_started_at.take().map_or_else(String::new, |t| {
                let d = t.elapsed();
                if d.as_secs() >= 1 {
                    format!("耗时={:.1}s", d.as_secs_f64())
                } else {
                    format!("耗时={}ms", d.as_millis())
                }
            });
            log_info!("app", "索引完成: {} 行, 文件大小 {} {}", lines, size_str, elapsed_str);
            self.flash_status(
                format!("索引完成 · {} 行 · {}", lines, size_str),
                5,
            );
        }
        // A save's background re-index just finished → the line numbers used by
        // annotation re-anchoring are now computed against the NEW file content.
        if index_data.succeeded && self.pending_reanchor {
            self.pending_reanchor = false;
            self.reanchor_annotations();
        }

        // search
        if let Some(msg) = search_data.msg {
            self.flash_status(msg, 5);
        }
        if let Some((hits, lines, total, query, status, just_completed)) = search_data.info {
            self.search_started_at = None;
            self.search_hits = hits;
            self.search_total_count = total;
            self.search_lines = lines;
            self.search_query = query;
            self.search_hit_idx = 0;
            self.search_status = status;
            if just_completed && total > 0 {
                self.anchor_search_to_viewport();
            }
        }

    }
}

/// Supported text encodings (key, display label).
pub const ENCODINGS: &[(&str, &str)] = &[
    ("UTF-8", "UTF-8 (Unicode)"),
    ("GBK", "GBK (简体中文)"),
    ("GB18030", "GB18030 (国标)"),
    ("GB2312", "GB2312 (简体中文)"),
    ("Big5", "Big5 (繁体中文)"),
    ("Shift_JIS", "Shift_JIS (日文)"),
    ("EUC-JP", "EUC-JP (日文)"),
    ("EUC-KR", "EUC-KR (韩文)"),
    ("windows-1252", "Windows-1252 (西欧)"),
    ("UTF-16LE", "UTF-16LE (Unicode)"),
    ("UTF-16BE", "UTF-16BE (Unicode)"),
];

/// Temporary structs to ferry data out of the engine borrow scope.
struct IndexPoll {
    msg: Option<String>,
    info: Option<(u64, u64)>, // (lines, size)
    /// The index was rebuilt successfully and now matches the current mmap.
    succeeded: bool,
}
struct SearchPoll {
    msg: Option<String>,
    info: Option<(Vec<u64>, Vec<u64>, usize, String, String, bool)>, // (hits, lines, total_count, query, status, just_completed)
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

impl QLogApp {
    fn handle_shortcuts(&mut self, ctx: &Context) {
        let input = ctx.input(|i| i.clone());

        // Whether keys were consumed (prevents text widgets from eating them).
        let consumed = false;

        // ---- Ctrl+C / Ctrl+Shift+C — handled early in handle_copy_shortcut() ----

        // ---- Esc: close topmost dialog or cancel ----
        if input.key_pressed(egui::Key::Escape) {
            if self.show_encoding_confirm {
                self.show_encoding_confirm = false;
                self.pending_encoding.clear();
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.show_donate {
                self.show_donate = false;
            } else if self.show_shortcuts {
                self.show_shortcuts = false;
            } else if self.show_help {
                self.show_help = false;
            } else if self.show_about {
                self.show_about = false;
            } else if self.show_file_properties {
                self.show_file_properties = false;
            } else if self.show_index_manager {
                self.show_index_manager = false;
            } else if let Some(arc) = self.engine.as_mut() {
                let mut e = arc.lock();
                e.cancel_search();
                e.cancel_index();
            }
            return;
        }

        // ---- Ctrl+N: New file ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::N) {
            log_debug!("app", "快捷键 新建文件");
            self.request_new_file();
            return;
        }

        // ---- Ctrl+O: Open file ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::O) {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("日志文件", &["log", "txt", "out", "err", "csv", "json", "xml", "yaml", "yml"])
                .add_filter("所有文件", &["*"])
                .pick_file()
            {
                self.try_open(path);
            }
            return;
        }

        // ---- Ctrl+S / Ctrl+Shift+S: Save / Save-as ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::S) {
            if input.modifiers.shift {
                log_debug!("app", "快捷键 另存为");
                self.request_save_as();
            } else {
                log_debug!("app", "快捷键 保存");
                self.save_file();
            }
            return;
        }

        // ---- Ctrl+R: Reload (guard against discarding edits) ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::R) {
            if self.is_new_file {
                // An unsaved new file has nothing on disk to reload.
                log_info!("app", "重新加载: 新文件，跳过");
                self.flash_status("新文件无需重新加载", 2);
            } else if let Some(ref path) = self.path.clone() {
                log_info!("app", "重新加载文件: {}", path.display());
                self.try_open(path.clone());
            }
            return;
        }

        // ---- Ctrl+F: Focus search ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::F) {
            ctx.memory_mut(|m| m.request_focus(
                egui::Id::new("toolbar_search")
            ));
            return;
        }

        // ---- Ctrl+L: Focus goto ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::L) {
            ctx.memory_mut(|m| m.request_focus(
                egui::Id::new("toolbar_goto")
            ));
            return;
        }

        // ---- Ctrl+I: File properties ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::I) {
            self.show_file_properties = true;
            return;
        }

        // ---- Ctrl+Plus/Equals: Increase font size ----
        if input.modifiers.ctrl && (input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals))
        {
            self.font_size = (self.font_size + 1.0).min(32.0);
            self.row_h = self.font_size as f64 * 1.4;
            log_debug!("app", "字体放大: size={} row_h={}", self.font_size, self.row_h);
            self.invalidate_content_width();
            return;
        }

        // ---- Ctrl+Minus: Decrease font size ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::Minus)
        {
            self.font_size = (self.font_size - 1.0).max(8.0);
            self.row_h = self.font_size as f64 * 1.4;
            log_debug!("app", "字体缩小: size={} row_h={}", self.font_size, self.row_h);
            self.invalidate_content_width();
            return;
        }

        // ---- Ctrl+0: Reset font size ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::Num0)
        {
            self.font_size = 13.0;
            self.row_h = 18.0;
            log_debug!("app", "字体重置: size=13 row_h=18");
            self.invalidate_content_width();
            return;
        }

        // ---- Ctrl+Shift+T: Cycle theme ----
        if input.modifiers.ctrl && input.modifiers.shift
            && input.key_pressed(egui::Key::T)
        {
            let next = (self.current_theme_idx + 1) % self.themes.len();
            self.current_theme_idx = next;
            self.config.gui.theme = self.themes[next].name.clone();
            self.themes[next].apply_to(ctx);
            log_debug!("app", "切换主题(快捷键): {}", self.themes[next].name);
            self.save_config();
            return;
        }

        // ---- F1: Help ----
        if input.key_pressed(egui::Key::F1) {
            self.show_help = true;
            return;
        }

        // ---- Ctrl+Shift+M: Mem dump ----
        if input.modifiers.ctrl && input.modifiers.shift
            && input.key_pressed(egui::Key::M)
        {
            crate::mem_diag::write_report("manual (Ctrl+Shift+M)", self);
            ctx.request_repaint(); // 立即刷新以防窗口未响应
            return;
        }

        // ---- Ctrl+Shift+N: 切到 no-fonts 模式 ----
        if input.modifiers.ctrl && input.modifiers.shift
            && input.key_pressed(egui::Key::N)
        {
            self.flash_status("无字体模式需设置环境变量 Q_LOG_NO_FONTS=1 后重新启动", 5);
            return;
        }

        // ---- F3: Next match ----
        if input.key_pressed(egui::Key::F3) {
            if input.modifiers.shift {
                self.jump_hit(-1);
            } else {
                self.jump_hit(1);
            }
            return;
        }

        // ---- Ctrl+G: Next match ----
        if input.modifiers.ctrl && input.key_pressed(egui::Key::G) {
            if input.modifiers.shift {
                self.jump_hit(-1);
            } else {
                self.jump_hit(1);
            }
            return;
        }

        // ---- Home: Jump to top ----
        if input.key_pressed(egui::Key::Home) {
            log_debug!("app", "跳转到顶部 (Home)");
            self.scroll_y = 0.0;
            return;
        }

        // ---- End: Jump to bottom ----
        if input.key_pressed(egui::Key::End) {
            if let Some(arc) = self.engine.as_ref() {
                let total = arc.lock().effective_line_count();
                log_debug!("app", "跳转到底部 (End): total_lines={}", total);
                self.scroll_y = self.max_scroll_px(total);
            }
            return;
        }

        // ---- PageUp/Down ----
        if input.key_pressed(egui::Key::PageUp) {
            let page = ctx.screen_rect().height() as f64 * 0.9;
            log_debug!("app", "PageUp: scroll_y {} -> {}", self.scroll_y, (self.scroll_y - page).max(0.0));
            self.scroll_y = (self.scroll_y - page).max(0.0);
            return;
        }
        if input.key_pressed(egui::Key::PageDown) {
            let page = ctx.screen_rect().height() as f64 * 0.9;
            if let Some(arc) = self.engine.as_ref() {
                let total = arc.lock().effective_line_count();
                let max_scroll = self.max_scroll_px(total);
                self.scroll_y = (self.scroll_y + page).min(max_scroll);
            } else {
                self.scroll_y += page;
            }
            log_debug!("app", "PageDown: scroll_y={}", self.scroll_y);
            return;
        }

        // Mark as used
        let _ = consumed;
    }
}
