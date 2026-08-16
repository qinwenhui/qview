# qview 标准行业性能测试（`qview-bench`）

> 一个可复现的行业基准测试工具：生成 5 级标准测试日志（S≈10MB … XXL≈50GB），
> 在 qview 真实引擎上测出打开 / 索引 / 搜索 / 导航 / 内存指标，产出 markdown 报告。

## 为什么是可复现的

- **数据生成是确定性的**：固定种子 RNG，任何机器重复生成字节完全一致。
- **测的是生产代码路径**：每个指标都走 GUI 同款引擎（`Engine` / `run_search` /
  `BlockIndex::get` / `line_of_byte`），不是合成微基准。
- **指标定义贴合行业**：打开时间、二次打开、全文件搜索、导航跳转、进程内存、CPU 占用。

## 用法

```bash
# 生成测试数据（默认目录 bench_data/）
cargo run --release -p qview-bench -- gen
cargo run --release -p qview-bench -- gen ./data --levels S,M,L   # 只要部分级别
cargo run --release -p qview-bench -- gen ./data --force          # 重建已存在的

# 跑基准 → 写 bench_data/report.md
cargo run --release -p qview-bench -- run ./data
cargo run --release -p qview-bench -- run ./data --window 64 --threads 0
cargo run --release -p qview-bench -- run ./data --levels S,M,L --keep-cache

# 一条龙
cargo run --release -p qview-bench -- all ./data
```

### 参数

| 参数 | 作用 |
|---|---|
| `--levels S,M,L,XL,XXL` | 只测部分级别（省时间/磁盘） |
| `--window MB` | 扫描窗口（16–256），默认 64 |
| `--threads N` | 扫描线程，0=自动（核数−1），默认 0 |
| `--keep-cache` | 不删 `.qli`（测"二次打开"场景）；默认先删以测"首次索引" |
| `--force`（gen） | 重新生成已存在的文件 |

**注意**：
- `--threads` / `--window` 只在**本次进程**生效（扫描池是进程级单例）。A/B 对比不同
  参数请每次单独运行一次 `run`。
- `gen` 生成 XXL（50GB）需要较多磁盘空间与时间，建议先 `--levels S,M,L` 试跑。
- 为公平对比，跑基准前可清空系统缓存（Windows `RAMMap → Empty Standby List`，
  Linux `sync && echo 3 > /proc/sys/vm/drop_caches`）。qview 的扫描走
  `FILE_FLAG_NO_BUFFERING`，本身不污染系统缓存。

## 测试文件规格

| 级别 | 文件 | 行数 | 约大小 | 场景 |
|---|---|---|---|---|
| S | test_s.log | 10 万 | 10 MB | 小型服务单次启动 |
| M | test_m.log | 100 万 | 100 MB | 中型应用半天日志 |
| L | test_l.log | 1000 万 | 1 GB | 大型服务全天日志 |
| XL | test_xl.log | 1 亿 | 10 GB | 集群聚合一天日志 |
| XXL | test_xxl.log | 5 亿 | 50 GB | 极端场景 / 长期归档 |

每行格式（80–200 字节，长度随机，模拟真实日志）：

```
[2026-08-05 10:23:45.123] [INFO] [auth-service] [a1b2c3d4] Request processed successfully in 123ms
```

- 日志级别按比例：INFO 60% / WARN 25% / ERROR 10% / DEBUG 5%
- 时间戳递增（100ms/行），跨年自动进位
- 内置**可搜索标记**（约 1% 的命中率，可对结果数）：
  - `ERROR-CODE-404` — 字面量搜索目标（不含 `ERROR-<数字>`，正则目标不命中它）
  - `ERROR-500` / `ERROR-503` — 正则目标 `ERROR-\d{3}`
  - `TIMEOUT:` — 附属字面量目标

## 报告的指标

| 指标 | 含义 |
|---|---|
| 打开(mmap) | 从打开到 mmap 就绪（大文件即首屏可用） |
| 首次索引 | 无 `.qli` 缓存时建索引耗时（真实生产路径：流式 NO_BUFFERING + memchr + 并行） |
| 二次打开(.qli) | 建过索引后再次打开的缓存命中耗时 |
| 跳转末尾 | 定位并解码最后一行 |
| 字面量搜索 | `ERROR-CODE-404` 全文件单遍扫描，耗时 + 精确命中数 |
| 正则搜索 | `ERROR-\d{3}` 全文件单遍扫描，耗时 + 精确命中数（并测搜索 CPU 占用） |
| 命中间跳转 | `BlockIndex::get` 均值（与结果集大小几乎无关） |
| 命中定位行 | `line_of_byte` 均值（稀疏索引锚点 + memchr） |
| 进程内存 | 打开 + 全文件搜索后的工作集（流式扫描设计下应与文件大小基本无关） |


## 生产数据 vs 合成数据

`qview-bench` 用合成数据保证可复现；用真实生产日志测也完全支持（`run` 接收任意
目录的文件，但文件名需符合 `test_<级别>.log` 或直接用 GUI 实测）。
