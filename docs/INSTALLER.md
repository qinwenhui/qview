# qview 跨平台打包发布说明

> qview 有 **4 个前端**：跨平台 egui GUI（最完善，唯一带正式安装包）、Windows 原生、
> macOS 原生、终端 TUI。本文说明每个平台怎么打包、打包出什么、资源（字体/图片）怎么落地，
> 以及各前端的完善程度。全部产物可重现，无需任何手工步骤。

作者：qinwh · 2026-08-14

---

## 0. 四个前端与完善度（先看这张表）

| 前端 | 平台 | 形态 / 技术 | 完善度 | 打包方式 |
|------|------|------------|--------|----------|
| `qview-gui-egui` | Win / macOS / Linux | egui + glow 图形界面 | **★★★ 最完善** | Windows 安装包 + macOS `.app` |
| `qview`（TUI） | 任何有终端的环境 | ratatui + crossterm | ★ 最基础 | 无打包，裸二进制 |
| `qview-gui-native` | Windows | Win32 / GDI（免 GUI 框架） | ★★ 对标 egui，但**无 AI / 无编辑** | 无打包，裸 exe |
| `qview-gui-macos` | macOS | AppKit / CoreText | ★ 极简 | `.app`（`package.sh`） |

说明：

- **`qview-gui-egui` 是主推版本**：唯一带正式安装向导的前端。功能最全——完整编辑
  （撤销/重做、行内编辑、保存/另存为）、AI 器灵小Q（对话式日志分析，可定位/过滤/批注）、
  批注系统、6 套主题、超长行自动换行、会话历史回看、搜索（SIMD 字面量 + 正则双引擎）、
  多编码、索引缓存等。三个 GUI 前端与 TUI 共享同一个 `qview-core` 引擎。
- **`qview-gui-native`（Windows 原生）**：对标 egui 的轻量实现——6 套主题、精确搜索高亮
  与视口锚定跳转、文本选择/复制/批注、word wrap、空白字符与缩进参考线、编码切换、
  设置/缓存管理/批注对话框、拖拽打开。但**没有 AI 器灵，也没有编辑功能**。空载 ~12 MiB。
- **`qview-gui-macos`（macOS 原生）**：AppKit / CoreText 极简实现——浏览、选择、设置、
  主题、状态栏、工具栏。只保证基础浏览体验。
- **`qview`（TUI）**：vim 键位、`tail -f`、搜索、行内查看。纯文本终端，无窗口、无图形资源。
  空载 ~1 MiB。

> 结论：**想开箱即用全功能 → 用 egui 前端**（Windows 装安装包 / macOS 跑 `.app`）；
> Windows 上要省内存且接受功能裁剪 → 原生前端；远程 SSH / 服务器 → TUI。

---

## 1. 资源策略（唯一事实来源 + 各平台差异）

**所有可发布资源只放在一个地方**：`frontends/gui-egui/assets/`

| 想要的内容 | 放到 | 打包自动处理                                     |
|-------|---|--------------------------------------------|
| 中文字体  | `frontends/gui-egui/assets/*.ttf` | 各平台按下方策略落地，exe/.app 启动时自动发现                |
| 界面主题样式 | `frontends/gui-egui/assets/themes/*.json` | 同上，设置里可切换                                  |
| 程序图标  | `frontends/gui-egui/assets/icon.ico` | Windows 嵌入 exe 资源 + 侧车；macOS 转 icns 放 Dock |
| 捐赠收款码 | `frontends/gui-egui/assets/donate_*.png` | 同上，捐赠弹窗读取（作者穷啊）                            |

资源在各平台的**落地机制**（`assets.rs` + `build.rs` + `package.sh` 决定）：

| 前端 / 平台 | 字体与图片如何到位 | 说明 |
|---|---|---|
| egui · **Windows** | **sidecar**：`build.rs` 构建期把整个 `assets/` 复制到 `target/release/assets/`（与 exe 同目录），运行时读 `<exe>/assets/<file>` | exe 本身**不含**任何资源（体积 ~9M）；**必须连同 `assets/` 目录一起分发**。安装包会全量带上。缺 `assets/` 时字体/图标/赞赏码显示为空白 |
| egui · **macOS / Linux** | **sidecar-first，缺失回退 `include_bytes!` 编译期内嵌** | 字体/图片已编进二进制，`.app` 里看不到独立文件；单文件 `.app` 自包含。`package.sh` 只需额外放 `icon.icns`（Dock 图标，macOS 系统强制要求） |
| native · **Windows** | **系统字体**（默认 Consolas，`EnumFontFamiliesExW` 枚举系统等宽字体） | 不依赖任何资源文件 |
| native · **macOS** | **系统字体**（Menlo / SF Mono / Monaco 候选） | 不依赖字体文件；仅 `.app` 里放 `icon.icns` |
| TUI | 无资源 | 纯文本终端 |

**澄清两个常见误解**：

1. **"macOS 打包把字体/图片都放到 .app 里了"** —— 对，但方式是**编译期内嵌进二进制**，
   你在 `.app` 里看不到独立的 `.ttf` / `.png` 文件；`.app` 里唯一独立的资源文件是
   `Contents/Resources/icon.icns`（Dock 图标）。
2. **"Windows 打包没放字体/图片"** —— exe 单文件确实不含，但安装包**会**把整个 `assets/`
   目录一起装到安装目录，所以**装完之后 `qview.exe` + `assets/` 是完整可用的**。
   只有当你把 `qview.exe` 单独拷走、不带 `assets/` 时，字体/图标才会缺失（显示空白/占位）。

---

## 2. Windows：egui 安装包（唯一正式安装向导）

### 2.1 打包命令与产物

```bash
cargo run --release -p qview-installer --bin qview-bundle
# → target/release/qview-setup-<version>.exe
```

一个自包含单文件安装包（egui 向导），内嵌 zstd 压缩载荷，**无需任何手工步骤**。
`<version>` 取自 workspace 的 `CARGO_PKG_VERSION`（当前 1.0.0）。

### 2.2 打包流水线（`dist/installer/src/bin/bundle.rs`）

1. `cargo build --release -p qview-gui-egui` —— 编译主程序 `qview-gui-egui.exe`
2. `cargo build --release -p qview-installer --no-default-features --features uninstaller`
   —— 编译卸载器（~0.7M，**不编译 egui 树**）
3. 组装 `target/install/qview-payload/`：
   `qview.exe`（改名自 `qview-gui-egui.exe`）+ `assets/` 全量 + `LICENSE` + `uninstall.exe`
4. `cargo build --release -p qview-installer --bin qview-setup` —— `build.rs` 读取
   `QVIEW_PAYLOAD_DIR`（指向载荷目录），zstd 压缩成 qpak 内嵌进 setup.exe。
   打包工具每次组装后戳一下 `.payload_stamp`（gitignored），`build.rs` 对它无条件
   声明 `rerun-if-changed`，保证内层构建必然重打包（避免 cargo 复用旧的空 qpak）
5. 重命名为 `qview-setup-<version>.exe`

产物：自包含单文件，约 23 MB 载荷压到 ~15 MB。

### 2.3 qpak 载荷格式

```
[u32 LE file_count]
  × file_count:
    [u32 LE name_len][name bytes, UTF-8, '/' 分隔]
    [u64 LE data_len][u64 LE data_offset]      // 在未压缩数据流中的位置
[zstd 压缩流：所有文件数据首尾拼接]
```

`setup.exe` 运行时任选 `%TEMP%\qview-setup-<pid>\staging` 解压，再复制到安装目录。

### 2.4 安装行为与布局

默认 `%LOCALAPPDATA%\Programs\qview`（用户目录，免管理员，`data/` 完全可写）。

```
{安装目录}/
├── qview.exe            # 主程序
├── uninstall.exe        # 卸载器（随包自带）
├── LICENSE
├── assets/              # 字体 / 主题 / 收款码 / 图标（sidecar，必需）
└── data/
    ├── config.json      # 初始配置（向导选择项）
    ├── index/           # 索引缓存（.qli）
    ├── qview.db         # 本地结构化存储（会话历史 / 文件元数据，首次启动生成）
    └── uninstall.json   # 卸载清单
```

向导页面：欢迎 → 安装目录 → 选项（主题 / 字体 / 索引方式 / 扫描线程 / 快捷方式 / 文件关联）
→ 确认 → 完成。浅色淡蓝主题，左侧步骤指示条，底部统一尺寸按钮。

初始配置只写向导选择的字段，其余由 app 的 `#[serde(default)]` 补齐：

```json
{
  "version": "1.0.0",
  "gui":    { "theme": "Dark Pro", "font_family": "NotoSansSC-VF" },
  "engine": { "index_build_mode": "sparse", "scan_threads": 0 }
}
```

注册表全部落在 `HKCU`（免管理员）：

```
HKCU\Software\Classes\.log\OpenWithProgIds\qview        （文件关联 · 只加值不劫持默认）
HKCU\Software\Classes\qview\shell\open\command  = "C:\...\qview.exe" "%1"
HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\qview   （控制面板卸载项）
```

### 2.5 卸载（安全设计）

1. **没有卸载清单时拒绝删除任何文件**（无法确认目录归属，防误删）
2. 确认卸载（选「否」立即退出）
3. 询问是否保留 `data/index` 索引缓存
4. 删注册表（OpenWithProgIds 只删 `qview` 值，不删父键）→ 快捷方式 → 目录
5. 目录深度防御：仅当其中有 `qview.exe` / `uninstall.exe` 才删
6. 自删除：后台 `cmd` 延迟 1 秒后 `rmdir /S /Q`

---

## 3. macOS：egui `.app` 与原生 `.app`

macOS 有两个前端各有一个打包脚本，产物同名：

| 脚本 | 打包谁 | 产物 |
|------|--------|------|
| `frontends/gui-egui/scripts/package.sh` | egui 版（跨平台 GUI） | `target/qview.app` |
| `frontends/gui-macos/scripts/package.sh` | 原生 AppKit 版 | `target/qview.app` |

用法：

```bash
./frontends/gui-egui/scripts/package.sh             # 构建 release 并打包
./frontends/gui-egui/scripts/package.sh --no-build  # 只打包，不重新构建
```

产物结构：

```
target/qview.app/
├── Contents/
│   ├── Info.plist          # 来自对应 crate 的 Info.plist
│   ├── MacOS/qview-gui-egui | qview-gui-macos   # 主程序二进制
│   └── Resources/icon.icns # Dock 图标（egui 版复用 gui-macos 的 icns）
```

资源说明：

- **egui 版**：字体 / 窗口图标 / 赞赏码**编译期内嵌**进二进制（见 §1），`.app` 里不需要
  `assets/` 目录；只额外放 `icon.icns`。Dock 图标复用 `frontends/gui-macos/icon.icns`
  （egui 前端没有单独的 icns）。
- **原生版**：用系统字体（Menlo / SF Mono / Monaco），不依赖字体文件；只放 `icon.icns`。

签名：**ad-hoc**（`codesign --force --sign -`，本机可运行）。**未做 notarize / 公证**，
分发给别的机器首次双击可能被 Gatekeeper 拦截——右键 → 打开即可绕过。

⚠️ **同名覆盖**：两个脚本都输出 `target/qview.app`。依次跑两个脚本，最后跑的那个会把
前一个覆盖。打包前确认你要的是哪个前端。

裸二进制调试：`target/release/qview-gui-egui` / `target/release/qview-gui-macos` 可直接跑，
但 Finder 双击裸二进制会弹 Terminal 承载 stdout（这就是打包 `.app` 的原因）。

---

## 4. 其它前端构建（无打包脚本）

这三个前端没有正式安装包/打包脚本，`cargo build` 出裸二进制即可分发：

| 前端 | 命令 | 产物 |
|------|------|------|
| Windows 原生 | `cargo build --release -p qview-gui-native` | `target/release/qview-gui-native.exe` |
| TUI | `cargo build --release -p qview` | `target/release/qview.exe`（macOS/Linux 为 `qview`） |
| macOS 原生 | `cargo build --release -p qview-gui-macos`（要 `.app` 再跑 `package.sh`） | `target/release/qview-gui-macos` |

- **Windows 原生**：Win32/GDI，无 GUI 框架依赖，用系统字体（无需任何资源文件），单文件可拷走。
- **TUI**：纯终端，无窗口无资源。远程/服务器场景唯一选择。
- macOS 原生：见 §3 的 `.app` 打包。

---

## 5. 模块划分与耦合（`dist/installer`）

```
dist/installer/
├── Cargo.toml
├── build.rs            # ① 条件嵌入图标（有 icon.ico 才嵌）② 压缩载荷为 qpak
└── src/
    ├── lib.rs          # 库入口：manifest 恒编译；install/qpak 按 feature 门控
    ├── manifest.rs     # UninstallManifest（serde-only，卸载器可读）
    ├── install.rs      # 安装动作：复制/配置/快捷方式/注册表/卸载清单
    ├── qpak.rs         # 载荷解压（解压器，与 build.rs 的打包器对称）
    ├── main.rs         # 安装向导 UI（bin: qview-setup）
    └── bin/
        ├── bundle.rs   # 打包流水线（bin: qview-bundle）
        └── uninstall.rs# 卸载器（bin: qview-uninstall，feature 隔离无 egui）
```

**耦合点**（全部是弱耦合）：
- `crates/core`：仅用 `IndexBuildMode` 生成 config.json，不依赖引擎其它部分
- `frontends/gui-egui/assets/`：打包时只读该目录，不 import 任何 gui 代码
- 三个 bin 通过 feature（`installer` / `uninstaller`）隔离，卸载器不链接 egui 树

**依赖注入点**：`build.rs` 通过 `QVIEW_PAYLOAD_DIR` 环境变量接收载荷路径——打包工具
和构建脚本解耦，源码树里不需要常驻中间目录。

---

## 6. 测试

```bash
cargo test -p qview-installer        # qpak 解压往返 / config.json 生成 / 目录复制
cargo test -p qview-gui-egui         # 最小 config.json 可被 AppConfig 解析
cargo test -p qview-core
```

Windows 向导 GUI 与注册表写入需实机验证：运行 setup.exe 走一遍安装，检查桌面快捷方式、
.log 右键「打开方式」、控制面板卸载项，再卸载确认清理干净。macOS `.app` 打包用
`package.sh --no-build` 冒烟，确认 `open target/qview.app` 能启动且 Dock 图标正常。

---

## 7. 注意事项

- **版本号**：全程读 `CARGO_PKG_VERSION`（workspace 单一来源，当前 1.0.0），不要手写。
  安装包产物名随版本：`qview-setup-<version>.exe`。
- **平台限制**：`qview-installer` 仅面向 Windows（注册表 HKCU / Shell 快捷方式）。非 Windows
  构建请 `--exclude qview-installer`。
- **Windows exe 不含资源**：分发时务必连同 `assets/` 目录；只拷单文件会缺字体/图标。
- **macOS 两个 `.app` 同名**：egui 与原生版都输出 `target/qview.app`，避免混淆。
- **未签名 / 未公证**：Windows `setup.exe` 未签名，macOS `.app` 仅 ad-hoc 签名未 notarize，
  分发到未信任机器可能被 SmartScreen / Gatekeeper 拦截。
- **源码树干净**：打包产物一律在 `target/`；若发现 `dist/installer/payload/` 之类残留说明是
  旧版本产物，删除即可。
