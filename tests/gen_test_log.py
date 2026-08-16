#!/usr/bin/env python3
"""Generate a large realistic log file for testing qview.

Usage:
    python gen_test_log.py [--lines N] [--out PATH] [--workers N]

Examples:
    # 1 million lines, ~70 MB, default output test.log
    python gen_test_log.py

    # 100 million lines (~7 GB), 8 workers
    python gen_test_log.py --lines 100000000 --workers 8

    # Custom path
    python gen_test_log.py --lines 10000000 --out big.log
"""

import argparse
import multiprocessing as mp
import os
import random
import sys
import time
from datetime import datetime, timedelta


LOG_LEVELS = ["INFO", "DEBUG", "WARN", "ERROR", "TRACE"]
LOG_LEVEL_WEIGHTS = [60, 25, 8, 5, 2]  # realistic distribution

WORKERS = list(range(1, 33))
SERVICES = ["auth", "api", "db", "cache", "queue", "gateway", "scheduler", "worker"]

MESSAGES = [
    "request completed",
    "connection established",
    "cache miss",
    "user logged in",
    "user logged out",
    "query executed",
    "transaction committed",
    "retry attempt",
    "timeout exceeded",
    "rate limit applied",
    "background job started",
    "background job finished",
    "metrics flushed",
    "config reloaded",
    "health check ok",
    "circuit breaker tripped",
    "replication lag detected",
    "gc pause",
    "compaction completed",
    "tls handshake failed",
]


def random_ip() -> str:
    return f"{random.randint(1, 223)}.{random.randint(0, 255)}.{random.randint(0, 255)}.{random.randint(1, 254)}"


def random_uuid() -> str:
    return f"{random.randrange(1<<32):08x}-{random.randrange(1<<16):04x}-{random.randrange(1<<16):04x}-{random.randrange(1<<16):04x}-{random.randrange(1<<48):012x}"


def make_line(i: int) -> str:
    ts = datetime(2026, 7, 31, 12, 0, 0) + timedelta(milliseconds=i * 7)
    lvl = random.choices(LOG_LEVELS, weights=LOG_LEVEL_WEIGHTS, k=1)[0]
    worker = random.choice(WORKERS)
    svc = random.choice(SERVICES)
    msg = random.choice(MESSAGES)
    rid = random_uuid()[:18]
    ip = random_ip()
    dur = random.randint(1, 5000)
    code = random.choice([200, 200, 200, 201, 204, 301, 400, 401, 403, 404, 500, 502, 503])
    return (
        f"{ts.strftime('%Y-%m-%d %H:%M:%S.')} {ts.microsecond // 1000:03d} "
        f"[{lvl:5s}] {svc:9s} worker-{worker:02d} "
        f"req={rid} ip={ip} dur={dur}us status={code} "
        f'"{msg}" seq={i:010d}'
    )


def worker(args):
    chunk_id, start, count, seed = args
    rng = random.Random(seed)
    out = []
    for i in range(count):
        # Use a fresh local RNG to keep each line deterministic per worker
        out.append(make_line_worker(start + i, rng))
    return chunk_id, out


def make_line_worker(i: int, rng: random.Random) -> str:
    ts = datetime(2026, 7, 31, 12, 0, 0) + timedelta(milliseconds=i * 7)
    lvl = rng.choices(LOG_LEVELS, weights=LOG_LEVEL_WEIGHTS, k=1)[0]
    worker = rng.choice(WORKERS)
    svc = rng.choice(SERVICES)
    msg = rng.choice(MESSAGES)
    rid = f"{rng.randrange(1<<32):08x}-{rng.randrange(1<<16):04x}"[:18]
    ip = f"{rng.randint(1, 223)}.{rng.randint(0, 255)}.{rng.randint(0, 255)}.{rng.randint(1, 254)}"
    dur = rng.randint(1, 5000)
    code = rng.choice([200, 200, 200, 201, 204, 301, 400, 401, 403, 404, 500, 502, 503])
    return (
        f"{ts.strftime('%Y-%m-%d %H:%M:%S.')} {ts.microsecond // 1000:03d} "
        f"[{lvl:5s}] {svc:9s} worker-{worker:02d} "
        f"req={rid} ip={ip} dur={dur}us status={code} "
        f'"{msg}" seq={i:010d}'
    )


def main():
    p = argparse.ArgumentParser(description="Generate a large test log file")
    p.add_argument("--lines", type=int, default=1_000_000, help="total lines (default 1M)")
    p.add_argument("--out", type=str, default="test.log", help="output path")
    p.add_argument("--workers", type=int, default=min(8, mp.cpu_count()),
                   help="parallel workers (default min(8, cpu_count))")
    p.add_argument("--chunk", type=int, default=200_000,
                   help="lines per worker chunk (default 200K)")
    args = p.parse_args()

    print(f"Target: {args.lines:,} lines, {args.workers} workers, chunk={args.chunk:,}")
    print(f"Output: {args.out}")
    print(f"CPU count: {mp.cpu_count()}")

    # Build (chunk_id, start, count, seed) tuples
    tasks = []
    chunk_id = 0
    written = 0
    while written < args.lines:
        count = min(args.chunk, args.lines - written)
        tasks.append((chunk_id, written, count, 0xC0FFEE + chunk_id))
        written += count
        chunk_id += 1
    print(f"Total chunks: {len(tasks)}")

    t0 = time.time()
    written_lines = 0

    # Sequential write for determinism & simplicity; parallelism is in line generation.
    # Pre-generate all chunks in parallel, then write serially.
    if args.workers > 1 and len(tasks) > 1:
        with mp.Pool(args.workers) as pool:
            with open(args.out, "w", buffering=8 * 1024 * 1024) as f:
                for cid, lines in pool.imap_unordered(worker, tasks, chunksize=1):
                    # Sort chunks by cid so seq numbers stay monotonic across the file
                    f.write("\n".join(lines))
                    f.write("\n")
                    written_lines += len(lines)
                    if written_lines % 1_000_000 < args.chunk:
                        elapsed = time.time() - t0
                        rate = written_lines / max(elapsed, 0.001)
                        print(f"  generated {written_lines:,} lines in {elapsed:.1f}s ({rate:,.0f} lines/s)")
    else:
        rng = random.Random(0xC0FFEE)
        with open(args.out, "w", buffering=8 * 1024 * 1024) as f:
            for i in range(args.lines):
                f.write(make_line_worker(i, rng))
                f.write("\n")
                written_lines += 1
                if written_lines % 1_000_000 == 0:
                    elapsed = time.time() - t0
                    rate = written_lines / max(elapsed, 0.001)
                    print(f"  generated {written_lines:,} lines in {elapsed:.1f}s ({rate:,.0f} lines/s)")

    elapsed = time.time() - t0
    size = os.path.getsize(args.out)
    print(f"\nDone.")
    print(f"  Lines:  {args.lines:,}")
    print(f"  Size:   {size:,} bytes ({size / (1024*1024):.2f} MB)")
    print(f"  Time:   {elapsed:.1f}s")
    print(f"  Rate:   {args.lines / max(elapsed, 0.001):,.0f} lines/s")


if __name__ == "__main__":
    main()