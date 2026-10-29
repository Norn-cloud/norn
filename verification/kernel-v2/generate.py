#!/usr/bin/env python3
"""Generate reproducible ITF conformance inputs from the pinned Quint models.

Run on Boat only. This is a trace producer, not a CI/lane gate script.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent

def generate():
    manifest = json.loads((HERE / "manifest.json").read_text())
    quint = os.environ.get("QUINT", "quint")
    version = subprocess.check_output([quint, "--version"], text=True).strip()
    if version != manifest["quint"]:
        raise SystemExit(f"Quint version {version}, expected {manifest['quint']}")
    destination = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "traces"
    destination.mkdir(parents=True, exist_ok=True)
    entries = []
    profiles = [(model, step, seed, manifest["traceProfile"]["maxSteps"])
        for model in manifest["models"]
        for step in (["step", "scenario_step"] if Path(model["path"]).stem == "quantity-ledger" else ["step"])
        for seed in manifest["traceProfile"]["seeds"]]
    profiles += [(next(m for m in manifest["models"] if m["path"] == extra["model"]),
        extra["step"], extra["seed"], extra["maxSteps"])
        for extra in manifest["traceProfile"]["supplemental"]]
    checked = set()
    for model, step, seed, max_steps in profiles:
        source = HERE / model["path"]
        if source not in checked:
            subprocess.run([quint, "typecheck", str(source)], check=True)
            checked.add(source)
        name = f"{source.stem}-{step}-{seed}.json"
        path = destination / name
        command = [quint, "run", str(source), "--backend", "rust", "--n-threads", "1",
            "--max-samples", "1", "--max-steps", str(max_steps),
            "--seed", str(seed), "--step", step, "--invariant", "invariant", "--out-itf", str(path)]
        result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode:
            raise SystemExit(f"{name}: {result.stdout}")
        trace = json.loads(path.read_text())
        if len(trace.get("states", [])) < 2:
            raise SystemExit(f"{name}: no transitions")
        meta = trace["#meta"]
        meta.pop("timestamp", None)
        meta.pop("description", None)
        meta.update(source=model["path"], quint=version, seed=seed, step=step)
        path.write_text(json.dumps(trace, separators=(",", ":")) + "\n")
        entries.append({"file": name, "seed": seed, "step": step,
            "states": len(trace["states"]), "modelSha256": hashlib.sha256(source.read_bytes()).hexdigest(),
            "traceSha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    index = {"schemaVersion":"norn.verification.kernel-v2.traces/1", "quint": version,
        "maxSteps":manifest["traceProfile"]["maxSteps"], "traces":entries}
    (destination / "index.json").write_text(json.dumps(index, indent=2) + "\n")
    print(f"generated {len(entries)} model traces ({sum(e['states'] - 1 for e in entries)} transitions)")

if __name__ == "__main__":
    generate()
