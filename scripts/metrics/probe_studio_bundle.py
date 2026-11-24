#!/usr/bin/env python3
"""Measure production Studio bundle sizes and emit METRIC lines for run_probes.py.

Usage:
    python3 scripts/metrics/probe_studio_bundle.py --studio-dir apps/studio
    python3 scripts/metrics/probe_studio_bundle.py --dist apps/studio/dist

Emits both full-dist totals (`studio.bundle.js.*`, `studio.bundle.css.gzip_bytes`,
`studio.bundle.total_dist_bytes`) and initial cold-load entry assets parsed directly
from `dist/index.html` (`studio.bundle.initial_js.gzip_bytes`,
`studio.bundle.initial_css.gzip_bytes`). Gzip sizes use deterministic Python gzip
(compresslevel=9, mtime=0). Standard library only.
"""

from __future__ import annotations

import argparse
import gzip
from html.parser import HTMLParser
import json
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlsplit

JS_SUFFIXES = frozenset({".js", ".mjs"})
CSS_SUFFIXES = frozenset({".css"})


def gzip_size(data: bytes) -> int:
    return len(gzip.compress(data, compresslevel=9, mtime=0))


class _EntryAssetParser(HTMLParser):
    """Collect local JS/CSS entry asset URLs referenced by dist/index.html."""

    def __init__(self) -> None:
        super().__init__()
        self.js_urls: list[str] = []
        self.css_urls: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr_map = {k.lower(): (v or "") for k, v in attrs}
        if tag.lower() == "script" and attr_map.get("src"):
            self.js_urls.append(attr_map["src"])
        elif tag.lower() == "link" and attr_map.get("href"):
            rels = {token.lower() for token in attr_map.get("rel", "").split()}
            if "modulepreload" in rels:
                self.js_urls.append(attr_map["href"])
            elif "stylesheet" in rels:
                self.css_urls.append(attr_map["href"])


def _resolve_local_assets(dist_dir: Path, urls: list[str]) -> list[Path]:
    root = dist_dir.resolve()
    resolved: list[Path] = []
    seen: set[Path] = set()
    for raw_url in urls:
        parsed = urlsplit(raw_url.strip())
        if parsed.scheme or parsed.netloc:
            continue
        rel = parsed.path.lstrip("/")
        if not rel:
            continue
        candidate = (root / rel).resolve()
        if not candidate.is_relative_to(root):
            raise RuntimeError(f"entry asset escapes dist directory: {raw_url!r}")
        if not candidate.is_file() or candidate.is_symlink():
            raise RuntimeError(f"entry asset referenced by index.html is missing: {raw_url!r}")
        if candidate not in seen:
            seen.add(candidate)
            resolved.append(candidate)
    return resolved


def parse_initial_assets(dist_dir: Path) -> tuple[list[Path], list[Path]]:
    index_html = dist_dir / "index.html"
    if not index_html.is_file() or index_html.is_symlink():
        raise RuntimeError(f"dist/index.html does not exist: {index_html}")
    parser = _EntryAssetParser()
    parser.feed(index_html.read_text(encoding="utf-8"))
    js_assets = _resolve_local_assets(dist_dir, parser.js_urls)
    css_assets = _resolve_local_assets(dist_dir, parser.css_urls)
    if not js_assets:
        raise RuntimeError(f"no local JS entry scripts found in {index_html}")
    return js_assets, css_assets


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
    gzip_cache: dict[Path, int] = {}

    for path in sorted(p for p in dist_dir.rglob("*") if p.is_file() and not p.is_symlink()):
        data = path.read_bytes()
        raw = len(data)
        total_dist_bytes += raw
        total_files += 1
        rel = path.relative_to(dist_dir).as_posix()
        suffix = path.suffix.lower()
        if suffix in JS_SUFFIXES:
            gz = gzip_size(data)
            gzip_cache[path.resolve()] = gz
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
            gz = gzip_size(data)
            gzip_cache[path.resolve()] = gz
            css_gzip_total += gz
            css_count += 1

    if js_count == 0:
        raise RuntimeError(f"no .js/.mjs files found under {dist_dir}")

    initial_js_files, initial_css_files = parse_initial_assets(dist_dir)
    initial_js_gzip = sum(gzip_cache.get(p, gzip_size(p.read_bytes())) for p in initial_js_files)
    initial_css_gzip = sum(gzip_cache.get(p, gzip_size(p.read_bytes())) for p in initial_css_files)

    return [
        {
            "metric": "studio.bundle.js.raw_bytes",
            "value": js_raw_total,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"js_files": js_count, "scope": "all_dist_js"},
        },
        {
            "metric": "studio.bundle.js.gzip_bytes",
            "value": js_gzip_total,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"js_files": js_count, "gzip_level": 9, "scope": "all_dist_js"},
        },
        {
            "metric": "studio.bundle.initial_js.gzip_bytes",
            "value": initial_js_gzip,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"entry_js_files": len(initial_js_files), "gzip_level": 9, "entry": "dist/index.html"},
        },
        {
            "metric": "studio.bundle.initial_css.gzip_bytes",
            "value": initial_css_gzip,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"entry_css_files": len(initial_css_files), "gzip_level": 9, "entry": "dist/index.html"},
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
            "workload": {"css_files": css_count, "gzip_level": 9, "scope": "all_dist_css"},
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
