#!/usr/bin/env python3
"""Measure production Studio bundle sizes and emit METRIC lines for run_probes.py.

Usage:
    python3 scripts/metrics/probe_studio_bundle.py --studio-dir apps/studio
    python3 scripts/metrics/probe_studio_bundle.py --dist apps/studio/dist

When `--studio-dir` is given without `--dist`, runs `bun run build` in that
directory and measures `<studio-dir>/dist`. Gzip sizes use deterministic
Python gzip (compresslevel=9, mtime=0). Standard library only.
"""

from __future__ import annotations

import argparse
import gzip
import json
import subprocess
import sys
from pathlib import Path

JS_SUFFIXES = frozenset({".js", ".mjs"})
CSS_SUFFIXES = frozenset({".css"})


def gzip_size(data: bytes) -> int:
    return len(gzip.compress(data, compresslevel=9, mtime=0))


def measure_dist(dist_dir: Path) -> list[dict]:
    if not dist_dir.is_dir():
        raise RuntimeError(f"dist directory does not exist: {dist_dir}")

    js_raw_total = 0
    js_gzip_total = 0
    js_count = 0
    largest_raw = -1
    largest_raw_name = ""
    largest_gzip = -1
    largest_gzip_name = ""
    css_gzip_total = 0
    css_count = 0
    total_dist_bytes = 0
    total_files = 0

    for path in sorted(p for p in dist_dir.rglob("*") if p.is_file() and not p.is_symlink()):
        data = path.read_bytes()
        raw = len(data)
        total_dist_bytes += raw
        total_files += 1
        rel = path.relative_to(dist_dir).as_posix()
        suffix = path.suffix.lower()
        if suffix in JS_SUFFIXES:
            gz = gzip_size(data)
            js_raw_total += raw
            js_gzip_total += gz
            js_count += 1
            if raw > largest_raw:
                largest_raw = raw
                largest_raw_name = rel
            if gz > largest_gzip:
                largest_gzip = gz
                largest_gzip_name = rel
        elif suffix in CSS_SUFFIXES:
            css_gzip_total += gzip_size(data)
            css_count += 1

    if js_count == 0:
        raise RuntimeError(f"no .js/.mjs files found under {dist_dir}")

    return [
        {
            "metric": "studio.bundle.js.raw_bytes",
            "value": js_raw_total,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"js_files": js_count},
        },
        {
            "metric": "studio.bundle.js.gzip_bytes",
            "value": js_gzip_total,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"js_files": js_count, "gzip_level": 9},
        },
        {
            "metric": "studio.bundle.js.largest_chunk_raw_bytes",
            "value": largest_raw,
            "unit": "bytes",
            "statistic": "max",
            "workload": {"chunk": largest_raw_name, "js_files": js_count},
        },
        {
            "metric": "studio.bundle.js.largest_chunk_gzip_bytes",
            "value": largest_gzip,
            "unit": "bytes",
            "statistic": "max",
            "workload": {"chunk": largest_gzip_name, "js_files": js_count, "gzip_level": 9},
        },
        {
            "metric": "studio.bundle.css.gzip_bytes",
            "value": css_gzip_total,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"css_files": css_count, "gzip_level": 9},
        },
        {
            "metric": "studio.bundle.total_dist_bytes",
            "value": total_dist_bytes,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"total_files": total_files},
        },
    ]


def build_studio(studio_dir: Path) -> None:
    completed = subprocess.run(
        ["bun", "run", "build"],
        cwd=studio_dir,
        stdout=sys.stderr,
        stderr=sys.stderr,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"`bun run build` in {studio_dir} exited {completed.returncode}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--studio-dir", default="apps/studio", help="Studio package directory")
    parser.add_argument("--dist", help="existing dist directory to measure without running bun run build")
    args = parser.parse_args(argv)

    if args.dist:
        dist_dir = Path(args.dist)
    else:
        studio_dir = Path(args.studio_dir)
        build_studio(studio_dir)
        dist_dir = studio_dir / "dist"

    for item in measure_dist(dist_dir):
        print(f"METRIC {json.dumps(item, sort_keys=True)}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
