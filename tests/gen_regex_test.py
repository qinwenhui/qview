#!/usr/bin/env python3
"""Generate a deterministic log file designed for manually testing qview's
regex search, plus an "expected hit count" table for a battery of test
patterns so you can verify qview's exact totals against ground truth.

The file mixes many independently countable marker classes (levels, services,
worker ids, IPs, durations, status codes, error markers, Chinese text), each
with a controllable frequency, so a regex can be validated by comparing the hit
count qview reports to the number printed here.

Usage:
    python gen_regex_test.py [--lines N] [--out PATH] [--ms PER] [--seed N]
                             [--no-count] [--head N]

Examples:
    # 1,000,000 lines (~150 MB), default output regex_test.log
    python gen_regex_test.py

    # 100,000 lines for a quick pass
    python gen_regex_test.py --lines 100000

    # Skip the (slow) battery counting for very large files
    python gen_regex_test.py --lines 10000000 --no-count

The expected-count table is printed to stdout and written to
<out>.expected.txt.  All RNG is seeded, so regeneration is byte-identical and
every count below is exact and reproducible on any machine.
"""

import argparse
import re
import sys
import time
from datetime import datetime, timedelta

SEED = 20260805          # fixed: deterministic output on every machine
DEFAULT_LINES = 1_000_000
DEFAULT_MS = 250         # ms per line → ~2.9 days per 1M lines

BASE = datetime(2026, 1, 1, 0, 0, 0)

# ---------------------------------------------------------------------------
# Line shape (each line is a realistic, searchable log entry):
#   [2026-01-01 00:00:00.000] [INFO] [api] worker-07 req=a1b2c3d4e5f60708 \
#       ip=192.168.1.55 dur=452us status=404 "request processed" seq=0000000000
# ---------------------------------------------------------------------------

LEVELS = [("INFO", 60), ("WARN", 20), ("ERROR", 15), ("DEBUG", 5)]
SERVICES = ["auth", "api", "db", "cache", "gateway", "job"]   # "worker" avoided: collides with worker-NN
STATUS = (
    [200] * 30 + [201] * 8 + [204] * 8 + [301] * 4 +          # 2xx/3xx: 50%
    [400] * 8 + [401] * 6 + [403] * 4 + [404] * 12 +          # 4xx: 30%
    [500] * 10 + [502] * 5 + [503] * 5                        # 5xx: 20%
)
assert len(STATUS) == 100

# Message pool.  Marker words are chosen so word-boundary tests discriminate:
#   TIMEOUT (standalone) vs TIMEOUTRETRY (prefix)  —  \bTIMEOUT\b vs \bTIMEOUT
#   ERROR / ERROR-CODE-404 / ERROR-500 / ERROR-503  —  literal vs \b vs \bERROR
#   ERRORS (no boundary after ERROR)                —  \bERROR\b does NOT hit it
# No message contains a lowercase "error"/"timeout" so counts are identical
# whether or not case-sensitivity (Aa) is on — matching qview's default state.
MSG_POOL = [
    ("request processed", 10),
    ("user logged in", 8),
    ("cache miss", 8),
    ("connection established", 6),
    ("heartbeat ok", 6),
    ("deadline exceeded: TIMEOUT", 6),
    ("TIMEOUTRETRY queued", 3),
    ("ERROR: request failed", 6),
    ("request failed with ERRORS", 3),
    ("request failed: ERROR-CODE-404", 5),
    ("HTTP ERROR-500 recorded", 4),
    ("HTTP ERROR-503 recorded", 3),
    ("queue full: BUFFER-OVERFLOW", 3),
    ("node crashed: SEGFAULT", 3),
    ("retrying after COOLDOWN", 3),
    ("gc pause", 4),
    ("用户登录成功", 2),
    ("磁盘空间不足", 2),
]
MSG_TOTAL = sum(w for _, w in MSG_POOL)   # 85

# ---------------------------------------------------------------------------
# Regex test battery.
# Each entry: (label, pattern-as-typed-in-qview, purpose).
# Counts are computed with re.IGNORECASE to mirror qview's default state
# (Aa OFF → qview wraps the pattern in `(?i)`).  All patterns stay within one
# line, so per-line counting equals qview's whole-file exact total.
# `py` overrides only exist where Python `re` can't express a Rust regex class.
# ---------------------------------------------------------------------------

BATTERY = [
    # ---- 字符类 character classes ----
    ("数字 \\d", r"\d", "所有数字（超大命中量：验证精确计数 + 密集采样路径）"),
    ("ASCII 数字 [0-9]", r"[0-9]", "所有 ASCII 数字（数据全 ASCII，与 \\d 相同）"),
    ("十六进制 [0-9a-f]", r"[0-9a-f]", "hex 字符：req 16 位 + seq 10 位 + 其他数字"),
    ("小写字母 [a-z]", r"[a-z]", "小写字符：hex 串 + 服务名 + 英文消息"),
    ("级别标签 \\[[A-Z]+\\]", r"\[[A-Z]+\]", "Aa 关（默认 (?i)）同时命中小写服务标签 → 2×行数；Aa 开则仅级别标签 → 行数"),
    ("服务标签2字 \\[.{2}\\]", r"\[.{2}\]", "点号量词：仅 [db] → db 服务行数"),
    ("服务标签3字 \\[.{3}\\]", r"\[.{3}\]", "仅 [api] 和 [job] → 每 6 行 2 个"),
    ("IP 地址", r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}", "每行 1 个 IP → 应等于行数"),
    ("非数字 [^0-9]", r"[^0-9]", "非数字字符（大命中量）"),

    # ---- 量词 quantifiers ----
    ("dur=任意位数", r"dur=\d+us", "每行 1 个 → 应等于行数"),
    ("数字连串 \\d+", r"\d+", "数字连串（run 模式：跨分块边界可能多计几个，见 docs/REGEX_TEST.md §4.8）"),
    ("dur 三位数", r"dur=\d{3}us", "dur∈[100,999]"),
    ("dur 四位数", r"dur=\d{4}us", "dur∈[1000,9999]"),
    ("dur 二到四位", r"dur=[0-9]{2,4}us", "dur≥10（几乎每行）"),
    ("状态码3位", r"status=\d{3}", "每行 1 个 → 应等于行数"),
    ("5xx 状态码", r"status=5\d{2}", "500/502/503 → 约 20%"),
    ("4xx/5xx", r"status=[45]\d{2}", "4xx+5xx → 约 50%"),
    ("404|500", r"status=(404|500)", "404+500"),
    ("16 位 hex", r"req=[0-9a-f]{16}", "每行 1 个 → 应等于行数"),
    ("8 位 hex", r"req=[0-9a-f]{8}", "req= 前缀限定 → 只命中第一段 8 位 → 每行 1 个（去掉前缀则会额外命中第二段与 seq）"),

    # ---- 锚点 anchors（必须写 (?m) 才是按行）----
    ("行首 (?m)^\\[", r"(?m)^\[", "所有行 → 应等于行数"),
    ("行尾 (?m)seq=...$", r"(?m)seq=\d+$", "所有行 → 应等于行数"),
    ("1月1/2日", r"(?m)^\[2026-01-0[12]", "1 月 1、2 日的行数（精确）"),
    ("日期开头", r"(?m)^\[20\d{2}-\d{2}-\d{2}", "所有行 → 应等于行数"),

    # ---- 分组与交替 groups & alternation ----
    ("(INFO|WARN)", r"\[(INFO|WARN)\]", "INFO+WARN 行数"),
    ("(INFO|ERROR|DEBUG)", r"\[(INFO|ERROR|DEBUG)\]", "非 WARN 行数"),
    ("全部级别", r"\[(INFO|WARN|ERROR|DEBUG)\]", "每行 1 个 → 应等于行数"),
    ("(ERROR-500|ERROR-503)", r"(ERROR-500|ERROR-503)", "两类错误码消息数"),
    ("(TIMEOUT|COOLDOWN)", r"(TIMEOUT|COOLDOWN)", "含 TIMEOUT/COOLDOWN 子串的消息（含 TIMEOUTRETRY）"),
    ("全部服务", r"\[(auth|api|db|cache|gateway|job)\]", "每行 1 个 → 应等于行数"),
    ("(api|db)", r"\b(api|db)\b", "api 或 db 服务 → 每 3 行 1 个"),

    # ---- 单词边界 word boundaries ----
    ("\\bERROR\\b", r"\bERROR\b", "独立 ERROR：ERROR 级别标签 + ERROR:/ERROR-CODE-404/ERROR-5xx 消息；不命中 ERRORS"),
    ("\\bERROR", r"\bERROR", "以 ERROR 开头的词：\\bERROR\\b + ERRORS"),
    ("ERROR", r"ERROR", "任意 ERROR 子串（含 ERRORS/ERROR-CODE-404/ERROR-5xx/级别标签）"),
    ("\\bTIMEOUT\\b", r"\bTIMEOUT\b", "独立 TIMEOUT：不命中 TIMEOUTRETRY"),
    ("\\bTIMEOUT", r"\bTIMEOUT", "在 \\bTIMEOUT\\b 基础上 + TIMEOUTRETRY"),
    ("TIMEOUT", r"TIMEOUT", "任意 TIMEOUT 子串（含 TIMEOUTRETRY）"),
    ("独立3位数字 \\b\\d{3}\\b", r"\b\d{3}\b", "独立 3 位数字：status + IP 的 192/168 + 时间戳毫秒 + 可能的 IP 段"),
    ("\\bapi\\b", r"\bapi\b", "api 服务 → 每 6 行 1 个"),

    # ---- 日期 / 时间 ----
    ("2026-01-01", r"2026-01-01", "1 月 1 日的行数（精确）"),
    ("时间 HH:MM:SS.mmm", r":\d{2}:\d{2}\.\d{3}", "时间戳时间部分 → 每行 1 个"),

    # ---- 大小写 case ----
    ("小写 error", r"error", "Aa 关（默认）：命中全部 ERROR（(?i) 生效）；Aa 开：0"),

    # ---- Unicode ----
    ("中文段 \\p{Han}+", r"\p{Han}+", "中文消息行数（每行 1 段）", r"[一-鿿]+"),
    ("汉字 \\p{Han}", r"\p{Han}", "汉字总数", r"[一-鿿]"),

    # ---- 特殊 / 语义 ----
    ("嵌套量词 ([0-9]+)+", r"([0-9]+)+", "嵌套量词：regex crate 保证线性时间，不会卡死"),
    ("非空白词 \\S+", r"\S+", "每行约 12-14 个词（12 个字段 + 消息内单词）"),
    ("空白段 \\s+", r"\s+", "每行 11 个字段间隔 + 消息内部空格"),
    ("ERROR 结构", r"ERROR-[A-Z]+-[0-9]+", "ERROR-CODE-404 结构 → 只命中该消息"),
    ("点号 \\.", r"\.", "每行 4 个点（时间戳 1 + IP 3）"),
    ("连字符 -", r"-", "时间戳 2 个 + 消息内连字符（按消息类型波动）"),
]


def fmt_int(n):
    return f"{n:,}"


def main():
    p = argparse.ArgumentParser(description="Generate deterministic regex-test log data")
    p.add_argument("--lines", type=int, default=DEFAULT_LINES, help="total lines")
    p.add_argument("--out", type=str, default="regex_test.log", help="output path")
    p.add_argument("--ms", type=int, default=DEFAULT_MS, help="milliseconds per line (timestamp step)")
    p.add_argument("--seed", type=int, default=SEED, help="RNG seed")
    p.add_argument("--no-count", action="store_true", help="skip the battery hit-counting pass")
    p.add_argument("--head", type=int, default=0, help="print the first N generated lines")
    args = p.parse_args()

    import random
    rng = random.Random(args.seed)

    # Precompile the battery (with IGNORECASE to match qview's default Aa-off).
    compiled = []
    for entry in BATTERY:
        label, pat, note = entry[0], entry[1], entry[2]
        py_pat = entry[3] if len(entry) > 3 else pat
        compiled.append((label, pat, note, re.compile(py_pat, re.IGNORECASE)))
    counts = {label: 0 for label, _, _, _ in compiled}

    lvl = [w for _, w in LEVELS]              # weights
    svc = SERVICES

    t0 = time.time()
    written = 0
    est_size = 0
    print(f"目标: {args.lines:,} 行, 种子={args.seed}, {args.ms}ms/行")
    print(f"输出: {args.out}")
    print()

    # newline="\n": keep LF endings even on Windows (text mode would translate
    # to \r\n, which breaks regex `$` anchors — a \r between digits and \n).
    with open(args.out, "w", encoding="utf-8", newline="\n", buffering=8 * 1024 * 1024) as f:
        for i in range(args.lines):
            ts = BASE + timedelta(milliseconds=i * args.ms)
            level = rng.choices([x[0] for x in LEVELS], weights=lvl, k=1)[0]
            service = rng.choice(svc)
            worker = rng.randint(1, 32)
            req = "".join(rng.choice("0123456789abcdef") for _ in range(16))
            ip = f"192.168.{rng.randrange(256)}.{rng.randrange(1, 255)}"
            dur = rng.randint(1, 9999)
            status = rng.choice(STATUS)
            msg = rng.choices([m for m, _ in MSG_POOL], weights=[w for _, w in MSG_POOL], k=1)[0]

            line = (
                f"[{ts:%Y-%m-%d %H:%M:%S}.{ts.microsecond // 1000:03d}] "
                f"[{level}] [{service}] worker-{worker:02d} "
                f"req={req} ip={ip} dur={dur}us status={status} "
                f'"{msg}" seq={i:010d}'
            )
            f.write(line)
            f.write("\n")
            written += 1
            est_size += len(line.encode("utf-8")) + 1
            if args.head and written <= args.head:
                print(f"  {line}")

            # Count this line against every battery pattern. Count WITH the
            # trailing '\n' so `[^0-9]` / `\s+` include the newline char, matching
            # qview's whole-file counting (it scans the raw bytes, newlines and all).
            if not args.no_count:
                for label, _, _, rx in compiled:
                    counts[label] += sum(1 for _ in rx.finditer(line + "\n"))

            if written % 200_000 == 0:
                el = time.time() - t0
                print(f"  已生成 {written:,} 行 in {el:.1f}s ({written / max(el, 0.001):,.0f} 行/s)")

    elapsed = time.time() - t0
    size_mb = est_size / (1024 * 1024)
    print(f"\n完成。{written:,} 行, 约 {size_mb:.1f} MB, {elapsed:.1f}s "
          f"({written / max(elapsed, 0.001):,.0f} 行/s)\n")

    # Expected-count table.
    lines_txt = []
    header = f"{'测试项':<24} {'正则（在 qview 中输入）':<34} {'期望命中':>14}"
    lines_txt.append(header)
    lines_txt.append("-" * 76)
    for label, pat, note, _ in compiled:
        lines_txt.append(f"{label:<24} {pat:<34} {fmt_int(counts[label]):>14}")
    lines_txt.append("")
    lines_txt.append("口径: 命中数按 qview 默认搜索状态（Aa 大小写关 = (?i)）逐行（含换行符）统计。")
    lines_txt.append("      单字符与带字面量前缀的模式与 qview 全文件精确总数一致；")
    lines_txt.append("      \\S+ / \\d+ 等'连串'模式在分块边界可能比 qview 少几个（见 docs/REGEX_TEST.md §4.8）。")
    lines_txt.append("验证方法: 在 qview 打开本文件 → 工具栏点 .* 开启正则 → 输入上表正则 → "
                     "对比状态栏显示的总命中数与期望值。")
    expected_path = args.out + ".expected.txt"
    with open(expected_path, "w", encoding="utf-8") as ef:
        ef.write("\n".join(lines_txt))
        ef.write("\n")
    print("\n".join(lines_txt))
    print(f"\n对照表已写入 {expected_path}")


if __name__ == "__main__":
    main()
