#!/usr/bin/env python3
"""Render the gap report (markdown) from a run's results directory.

Usage: report.py RESULTS_DIR REUSE_JSON > gap-report.md
"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def j(path: Path, default=None):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return default if default is not None else {}


def table(rows: list[list], head: list[str]) -> str:
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def main() -> None:
    res, reuse = Path(sys.argv[1]), j(Path(sys.argv[2]))
    steps = [json.loads(l) for l in (res / "steps.jsonl").read_text().splitlines()] if (res / "steps.jsonl").exists() else []
    seed, asm, gen = j(res / "seed-report.json"), j(res / "assemble.json"), j(res / "gen.json")
    lc, ln, lr, lb = j(res / "load-careful.json"), j(res / "load-naive.json"), j(res / "load-replay.json"), j(res / "load-bpic.json")
    probe, probe_n, probe_s = j(res / "probe.json"), j(res / "probe-naive.json"), j(res / "probe-scale.json")
    chaos = j(res / "chaos-counters.json")
    print("## Steps\n")
    print(table([[s["step"], s["rc"], s["seconds"]] for s in steps], ["step", "exit", "s"]))
    print("\n## Seeding (real systems)\n")
    print(table([[k, v.get("ok", 0), v.get("failed", 0), v.get("fatal", 0), v.get("seconds", "")] for k, v in (seed.get("steps") or {}).items()], ["step", "ok", "failed", "fatal", "s"]))
    for e in (seed.get("errors") or [])[:12]:
        print(f"- `{e['step']}`: {e['error'][:220]}")
    print("\n## Admission\n")
    print(f"admitted: {asm.get('finalAdmission', {}).get('admitted')}, artifacts {len(asm.get('artifacts', []))}, dropped {asm.get('dropped')}")
    for m, r in (asm.get("modules") or {}).items():
        if not r.get("ok"):
            print(f"- module `{m}` did not compile: {json.dumps(r.get('diagnostics'))[:300]}")
    for d in (asm.get("admission") or {}).get("diagnostics", [])[:25]:
        print(f"- {d['severity']} `{d['code']}` {d.get('artifactId')}: {d['message'][:240]}")
    for name, load in (("careful (clean network)", lc), ("naive (hostile network)", ln), ("replay (idempotency)", lr)):
        print(f"\n## Load: {name}\n")
        print(table([[k, v.get("rows"), v.get("landed"), v.get("alreadyLanded"), v.get("quarantined"), v.get("rowsPerSecond"), v.get("callP95ms"), json.dumps(v.get("refusals"))[:80]] for k, v in load.items()],
                    ["export", "rows", "landed", "already", "quarantined", "rows/s", "p95 ms", "refusals"]))
        reasons = {}
        for k, v in load.items():
            for r, c in (v.get("reasons") or {}).items():
                reasons[f"{k}: {r[:160]}"] = c
        for r, c in sorted(reasons.items(), key=lambda x: -x[1])[:15]:
            print(f"- {c} × {r}")
    print("\n## Objects and laws\n")
    for label, p in (("careful", probe), ("naive", probe_n), ("at scale", probe_s)):
        c = p.get("counts") or {}
        if isinstance(c, dict) and "error" not in c:
            print(f"**{label}**: " + ", ".join(f"{t.split('.')[-1]} {v['objects']}" + (f" (+{v['quarantined']} held)" if v.get("quarantined") else "") for t, v in c.items() if v.get("objects") or v.get("quarantined")))
        if p.get("inbox"):
            print(f"- inbox by rule: {json.dumps(p['inbox'].get('byRule'), ensure_ascii=False)[:600]}")
        if p.get("holdings"):
            print(f"- holdings (#1402): {json.dumps(p['holdings'])}")
    print("\n## Questions\n")
    for label, p in (("careful", probe), ("at scale", probe_s)):
        q = p.get("questions") or {}
        for k, v in q.items():
            if isinstance(v, dict) and ("p50ms" in v or "error" in v):
                print(f"- {label} `{k}`: " + (f"status {v.get('status')}, rows {v.get('rows')}, p50 {v.get('p50ms')} ms, max {v.get('maxms')} ms" if "p50ms" in v else v["error"]))
    print("\n## Actions and MCP\n")
    print(f"```json\n{json.dumps(probe.get('actions'), indent=1)[:2500]}\n```")
    print(f"MCP agent: {json.dumps(probe.get('mcp'))[:900]}")
    print("\n## Chaos\n")
    print(f"proxy counters: {json.dumps(chaos)}")
    for mode in ("careful", "naive"):
        ex = j(res.parent / "exports" / mode / "extract-report.json")
        print(f"- extract {mode}: systems {json.dumps(ex.get('systems'))[:600]}; anomalies {json.dumps(ex.get('anomalies'))}; http {json.dumps(ex.get('http'))[:400]}")
    print("\n## Stress (BPIC 2019)\n")
    print(json.dumps(j(res / "bpic-convert.json")))
    tot = {k: sum(v.get(k) or 0 for v in lb.values()) for k in ("rows", "landed", "quarantined", "calls")}
    secs = sum(v.get("seconds") or 0 for v in lb.values())
    print(f"loaded {tot} in {round(secs)} s = {round(tot['rows'] / secs, 1) if secs else None} rows/s; host RSS {max([v.get('rssKb') or 0 for v in lb.values()] or [0]) // 1024} MiB")
    print("\n## Reuse\n")
    print(f"```json\n{json.dumps({k: v for k, v in reuse.items() if k != 'newArtifacts'}, indent=1)}\n```")


if __name__ == "__main__":
    main()
