#!/usr/bin/env python3
"""Generate the ITF conformance traces of the hosted timers and approval fencing models.

Run on Boat (pinned Quint), not as a gate: the gates replay the committed traces against
the real code (crates/norn-cell-do/tests/hosted_timers_quint.rs and
approval_fencing_quint.rs) and `check_variants.py --static` refuses traces whose model
changed since they were generated.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main() -> int:
    manifest = json.loads((HERE / "manifest.json").read_text(encoding="utf-8"))
    quint = os.environ.get("QUINT", "quint")
    version = subprocess.check_output([quint, "--version"], text=True).strip()
    if version != manifest["quint"]:
        raise SystemExit(f"Quint version {version}, expected {manifest['quint']}")
    destination = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "traces"
    destination.mkdir(parents=True, exist_ok=True)
    for stale in destination.glob("*.json"):
        stale.unlink()
    entries = []
    for model in manifest["models"]:
        profile = model["conformance"]
        source = HERE / model["path"]
        for seed in range(1, profile["seeds"] + 1):
            name = f"{source.stem}-{seed:02}.json"
            path = destination / name
            command = [quint, "run", str(source), "--init", profile["init"], "--step", profile["step"],
                       "--invariant", profile["invariant"], "--max-samples", "1",
                       "--max-steps", str(profile["maxSteps"]), "--seed", str(seed), "--out-itf", str(path)]
            result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            if result.returncode:
                raise SystemExit(f"{name}: {result.stdout}")
            trace = json.loads(path.read_text(encoding="utf-8"))
            if len(trace.get("states", [])) < 2:
                raise SystemExit(f"{name}: no transitions")
            meta = trace["#meta"]
            meta.pop("timestamp", None)
            meta.pop("description", None)
            meta.update(source=model["path"], quint=version, seed=seed)
            path.write_text(json.dumps(trace, separators=(",", ":")) + "\n", encoding="utf-8")
            entries.append({"file": name, "model": model["path"], "seed": seed,
                            "states": len(trace["states"]),
                            "modelSha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                            "traceSha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    index = {"schemaVersion": "norn.verification.hosted-timers-v1.traces/1", "quint": version, "traces": entries}
    (destination / "index.json").write_text(json.dumps(index, indent=2) + "\n", encoding="utf-8")
    print(f"generated {len(entries)} traces ({sum(e['states'] - 1 for e in entries)} transitions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
