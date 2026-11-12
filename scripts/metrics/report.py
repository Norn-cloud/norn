#!/usr/bin/env python3
"""Render the metrics branch's trend view: README.md (GitHub renders it on the branch page)
and index.html (inline SVG sparklines). Both are regenerated from every record on each
append, so they are views, never data.

    python3 scripts/metrics/report.py --rev origin/metrics --out DIR   # write the views locally
"""

from __future__ import annotations

import argparse
import html
import json
import sys
from collections import defaultdict
from pathlib import Path

def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


SPARK = "▁▂▃▄▅▆▇█"
FAMILIES = {
    "fork": "Cell forks", "sim": "Deterministic simulation", "proof": "Proof gates",
    "tests": "Test counts", "ci": "CI task times (GitHub-hosted, never ratcheted)",
    "checker": "Checker test suite", "boat": "Boat usage",
}


def _key(record: dict) -> tuple:
    store = _sibling("record")
    return store.series_key(record)


def _source_md(url: str) -> str:
    return f"[link]({url})" if url.startswith(("https://", "http://")) else "thread"


def _source_html(url: str) -> str:
    return f'<a href="{html.escape(url)}">source</a>' if url.startswith(("https://", "http://")) else "thread"


def series(records: list[dict]) -> dict[str, list[dict]]:
    store = _sibling("record")
    out: dict[str, list[dict]] = defaultdict(list)
    for record in store.fold(records):
        out[record["metric"]].append(record)
    return {metric: sorted(rows, key=_key) for metric, rows in sorted(out.items())}


def number(value: float) -> str:
    if isinstance(value, int) or float(value).is_integer():
        return f"{int(value):,}"
    return f"{value:.4g}"


def sparkline(values: list[float]) -> str:
    if not values:
        return ""
    low, high = min(values), max(values)
    span = high - low
    return "".join(SPARK[0 if span == 0 else round((v - low) / span * (len(SPARK) - 1))] for v in values[-40:])


def family(metric: str) -> str:
    return metric.split(".", 1)[0]


def _cell(text: str) -> str:
    return str(text).replace("|", "\\|").replace("\n", " ")


def markdown(records: list[dict]) -> str:
    groups = series(records)
    lines = [
        "# Norn metrics",
        "",
        "Every performance and quality number Norn has measured, one JSON record per line in",
        "[`metrics/`](metrics), appended and never rewritten. Schema and tools:",
        "`scripts/metrics/` on `main`. This page and [`index.html`](index.html) are regenerated",
        "on every append.",
        "",
        f"{len(records):,} records across {len(groups):,} metrics (aliases folded, re-reports of one commit's value shown once). Each series runs in the order of the",
        "measured commits' dates, then measurement time; the trend shows the last 40. *est.* marks an",
        "estimate; runner `boat` series are the only ones the nightly ratchet gates.",
        "",
    ]
    by_family: dict[str, list[str]] = defaultdict(list)
    for metric in groups:
        by_family[family(metric)].append(metric)
    for name in sorted(by_family, key=lambda f: (f == "ci", f)):
        lines += [f"## {FAMILIES.get(name, name)}", "",
                  "| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |",
                  "|---|---:|---|---|---:|---|---|---|---|"]
        for metric in by_family[name]:
            rows = groups[metric]
            last = rows[-1]
            values = [r["value"] for r in rows]
            sha = last["commit"]["sha"]
            commit = f"`{sha[:10]}`" if sha else "none named"
            est = " *est.*" if last["estimate"] else ""
            runners = ", ".join(sorted({r["env"]["runner"] for r in rows}))
            lines.append(
                f"| `{metric}` | {number(last['value'])}{est} | {last['unit']} ({last['statistic']}) | {runners} | "
                f"{len(rows)} | {number(min(values))} / {number(max(values))} | {sparkline(values)} | {commit} | "
                f"{_source_md(last['source']['url'])} |")
        lines.append("")
    lines += ["## Latest caveats", "",
              "What limits each metric's latest value, in the measurer's words.", ""]
    for metric, rows in groups.items():
        if family(metric) == "ci":
            continue
        last = rows[-1]
        lines.append(f"- `{metric}`: {_cell(last['caveats'])}")
    lines.append("")
    return "\n".join(lines)


def _svg(values: list[float], width: int = 160, height: int = 32) -> str:
    if len(values) < 2:
        return ""
    low, high = min(values), max(values)
    span = (high - low) or 1
    step = width / (len(values) - 1)
    points = " ".join(f"{i * step:.1f},{height - 2 - (v - low) / span * (height - 4):.1f}"
                      for i, v in enumerate(values))
    return (f'<svg viewBox="0 0 {width} {height}" width="{width}" height="{height}" role="img" '
            f'aria-label="trend"><polyline points="{points}" fill="none" stroke="var(--line)" '
            f'stroke-width="1.5"/></svg>')


def html_page(records: list[dict]) -> str:
    groups = series(records)
    sections = []
    by_family: dict[str, list[str]] = defaultdict(list)
    for metric in groups:
        by_family[family(metric)].append(metric)
    for name in sorted(by_family, key=lambda f: (f == "ci", f)):
        rows_html = []
        for metric in by_family[name]:
            rows = groups[metric]
            last = rows[-1]
            values = [r["value"] for r in rows[-60:]]
            sha = last["commit"]["sha"]
            rows_html.append(
                "<tr>"
                f"<td><code>{html.escape(metric)}</code></td>"
                f"<td class=num>{html.escape(number(last['value']))}{' <em>est.</em>' if last['estimate'] else ''}</td>"
                f"<td>{html.escape(last['unit'])} ({html.escape(last['statistic'])})</td>"
                f"<td>{html.escape(last['env']['runner'])}</td>"
                f"<td class=num>{len(rows)}</td>"
                f"<td>{_svg(values)}</td>"
                f"<td><code>{html.escape(sha[:10]) if sha else '—'}</code></td>"
                f"<td>{_source_html(last['source']['url'])}</td>"
                f"<td class=cav>{html.escape(last['caveats'])}</td>"
                "</tr>")
        sections.append(
            f"<h2>{html.escape(FAMILIES.get(name, name))}</h2><div class=wrap><table><thead><tr>"
            "<th>Metric</th><th>Latest</th><th>Unit</th><th>Runner</th><th>n</th><th>Trend</th>"
            "<th>Commit</th><th>Source</th><th>What limits it</th></tr></thead><tbody>"
            + "".join(rows_html) + "</tbody></table></div>")
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Norn metrics</title>
<style>
:root {{ --bg: #fbfbfa; --fg: #1d1d1b; --muted: #6b6b66; --rule: #e4e3df; --line: #2f6fdb; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg: #161615; --fg: #ecebe7; --muted: #a09f99; --rule: #2c2c2a; --line: #7aa7ff; }} }}
body {{ margin: 0; padding: 24px 16px; background: var(--bg); color: var(--fg);
  font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }}
main {{ max-width: 1200px; margin: 0 auto; }}
h1 {{ font-size: 22px; margin: 0 0 4px; }} h2 {{ font-size: 16px; margin: 28px 0 8px; }}
p {{ color: var(--muted); margin: 0 0 8px; }}
.wrap {{ overflow-x: auto; }}
table {{ border-collapse: collapse; width: 100%; }}
th, td {{ text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--rule); vertical-align: top; }}
th {{ font-weight: 600; color: var(--muted); font-size: 12px; }}
.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
.cav {{ color: var(--muted); max-width: 420px; font-size: 12px; }}
code {{ font-size: 12px; }} a {{ color: var(--line); }}
</style></head><body><main>
<h1>Norn metrics</h1>
<p>{len(records):,} records across {len(groups):,} metrics, appended to the <code>metrics</code> branch and never rewritten.
Only <code>boat</code> series are ratcheted; GitHub-hosted CI times are recorded for trend only.</p>
{''.join(sections)}
</main></body></html>
"""


def render_files(records: list[dict]) -> dict[str, bytes]:
    return {"README.md": markdown(records).encode(), "index.html": html_page(records).encode()}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=".")
    parser.add_argument("--rev", default="origin/metrics")
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    record = _sibling("record")
    records = record.all_records(record.read_store(Path(args.repo), args.rev))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    for name, data in render_files(records).items():
        (out / name).write_bytes(data)
    print(json.dumps({"records": len(records), "metrics": len(series(records)), "out": str(out)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
