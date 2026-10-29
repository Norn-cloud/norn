#!/usr/bin/env python3
"""Bind bounded claims to model selectors and real-cell test selectors."""
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent

def validate():
    if not __debug__:
        raise RuntimeError("kernel-v2 validation requires Python assertions enabled")
    manifest = json.loads((HERE / "manifest.json").read_text())
    assert manifest["schemaVersion"] == "norn.verification.kernel-v2/1"
    assert manifest["quint"] == "0.32.0" and manifest["apalache"] == "0.56.1"
    harness = (ROOT / "crates/norn-testkit/tests/kernel_quint.rs").read_text()
    gates = json.loads((HERE.parent / "coverage-v1/promoted-gates.json").read_text())["gates"]
    gates = {gate["id"]: gate for gate in gates}
    assert "check_commit(&commit" in harness and "CellStore::open" in harness
    assert len(manifest["models"]) == 3
    for model in manifest["models"]:
        path = HERE / model["path"]
        assert path.is_file() and ".." not in Path(model["path"]).parts
        source = path.read_text()
        gate = gates["kernel-v2-" + path.stem]
        command = model.get("checkCommand", "quint verify verification/kernel-v2/" + model["path"] +
            " --backend apalache --invariants invariant --max-steps " + str(model["maxSteps"]))
        assert gate["command"] == command
        expected_results = [] if model.get("checkMode") in ("sampled", "exhaustive-scenarios") else ["target/verification-results/kernel-v2-" + path.stem + ".json"]
        assert gate["resultArtifacts"] == expected_results
        assert model["maxSteps"] > 0 and model["bounds"]
        assert re.search(r"fn " + re.escape(model["test"]) + r"\(\)", harness)
        assert "temporal safety = always(invariant)" in source
        for selector in model["invariants"]:
            assert re.search(r"def " + re.escape(selector) + r"\b", source)
        assert model["invariants"] and len(model["invariants"]) == len(set(model["invariants"]))
    profile = manifest["traceProfile"]
    assert profile["seeds"] and len(profile["seeds"]) == len(set(profile["seeds"]))
    assert profile["format"] == "ITF" and profile["maxSteps"] > 0
    index = json.loads((HERE / "traces/index.json").read_text())
    assert index["quint"] == manifest["quint"]
    assert index["maxSteps"] == profile["maxSteps"]
    entries = index["traces"]
    supplements = profile["supplemental"]
    assert len(entries) == profile["expectedTraces"] + profile["expectedSupplementalTraces"]
    assert sum(e["states"] - 1 for e in entries) == profile["expectedTransitions"] + profile["expectedSupplementalTransitions"]
    expected = {(Path(m["path"]).stem, step, seed) for m in manifest["models"]
        for step in (["step", "scenario_step"] if Path(m["path"]).stem == "quantity-ledger" else ["step"])
        for seed in profile["seeds"]}
    bounds = {identity: profile["maxSteps"] for identity in expected}
    bounds.update({(Path(extra["model"]).stem, extra["step"], extra["seed"]): extra["maxSteps"] for extra in supplements})
    expected = set(bounds)
    assert len(supplements) == profile["expectedSupplementalTraces"]
    assert sum(extra["maxSteps"] for extra in supplements) == profile["expectedSupplementalTransitions"]
    assert "#[ignore" not in harness, "all v2 tests must remain active"
    seen = set()
    for entry in entries:
        path = HERE / "traces" / entry["file"]
        assert Path(entry["file"]).name == entry["file"]
        stem = entry["file"].split("-" + entry["step"] + "-", 1)[0]
        identity = (stem, entry["step"], entry["seed"])
        assert identity in expected and identity not in seen
        seen.add(identity)
        source = HERE / "models" / (stem + ".qnt")
        assert entry["modelSha256"] == hashlib.sha256(source.read_bytes()).hexdigest()
        assert entry["traceSha256"] == hashlib.sha256(path.read_bytes()).hexdigest()
        raw = json.loads(path.read_text())
        assert raw["#meta"]["status"] == "ok"
        assert len(raw["states"]) == entry["states"] == bounds[identity] + 1
        assert raw["#meta"]["seed"] == entry["seed"] and raw["#meta"]["step"] == entry["step"]
    assert seen == expected
    print("validated 3 kernel-v2 models, 64 original traces (5120 transitions), and 1 supplemental trace (6 transitions)")

if __name__ == "__main__":
    validate()
