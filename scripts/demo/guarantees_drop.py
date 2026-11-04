#!/usr/bin/env python3
"""PROOF-BIND-1: print the existing matrix and admitted demo pack law inventory."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import guarantees_matrix as matrix  # noqa: E402

PACKS = {"jewelry": "registry/core/artifacts", "data": "registry/packs/demo-erpnext/artifacts"}


def dependency_closure(available, roots):
    """Select exact declared dependencies; native admission checks semantic closure."""
    catalog = {(a["artifactId"], a["version"]): a for a in available}
    if len(catalog) != len(available):
        raise ValueError("ambiguous artifact identity in pack inputs")
    selected, pending = {}, list(roots)
    while pending:
        artifact = pending.pop()
        key = artifact["artifactId"], artifact["version"]
        if key in selected:
            continue
        selected[key] = artifact
        for dependency in artifact.get("dependencies", []):
            target = catalog.get((dependency["artifactId"], dependency.get("version")))
            if target is None:
                if dependency.get("required") is False:
                    continue
                raise ValueError("pack dependency is not retained at its exact version")
            pending.append(target)
    return [selected[key] for key in sorted(selected)]


def pack_rows(inventory, artifact_ids):
    """Only admitted LawIR owns law identities; scenarios cannot supply them."""
    rows = []
    for entry in inventory["laws"]:
        if entry["pin"]["artifactId"] not in artifact_ids:
            continue
        law = entry["law"]
        rows.append({"law": law["id"], "pin": entry["pin"], "subject": law["subject"],
                     "level": "unknown", "reason": "checked goal evidence missing"})
    return {"level": "unknown", "declarationStatus": "present" if rows else "missing",
            "message": "checked goal evidence missing → unknown" if rows else "declaration missing → unknown",
            "laws": rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--result", type=Path, help="existing full verification result; stale means unknown")
    args = parser.parse_args()
    source = matrix.collect_git_identity(ROOT)
    raw = subprocess.check_output(["git", "archive", source["commit"], "--", *PACKS.values()], cwd=ROOT)
    artifacts, selections, pins = [], {}, {}
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        for item in archive:
            if not item.isfile() or not item.name.endswith(".json"):
                continue
            data = archive.extractfile(item).read()
            document = json.loads(data)
            if not isinstance(document, dict) or "artifactId" not in document:
                continue
            artifacts.append(document)
            pins[item.name] = hashlib.sha256(data).hexdigest()
            if document.get("kind") == "law":
                for name, directory in PACKS.items():
                    if item.name.startswith(directory + "/"):
                        selections.setdefault(name, []).append(document["artifactId"])
    melt = subprocess.check_output(["bun", "tools/norn-ts/src/cli.ts", "compile",
        "scripts/demo/jewelry.source.ts", "--org", "org_demo", "--env", "dev"], cwd=ROOT)
    compiled = json.loads(melt)
    roots = [a for a in artifacts if a.get("kind") == "law"
             or a["artifactId"] == "std.value_types.entity"] + compiled
    artifacts = dependency_closure(artifacts + compiled, roots)
    checked = subprocess.run(["cargo", "run", "--locked", "--quiet", "-p", "norn-checker",
        "--example", "pack_guarantees"], input=json.dumps(artifacts), text=True,
        stdout=subprocess.PIPE, cwd=ROOT, check=True)
    inventory = json.loads(checked.stdout)
    document = matrix.generate(args.result)
    document["packs"] = {name: pack_rows(inventory, selections.get(name, [])) for name in PACKS}
    document["generatedObligations"] = matrix.challenge_rows(inventory["challenge"])
    document["packSourceDigest"] = inventory["sourceDigest"]
    document["packInputs"] = pins
    document["inputDigest"] = matrix.digest({"matrix": document["inputDigest"], "packs": pins,
        "source": inventory["sourceDigest"]})
    if matrix.collect_git_identity(ROOT) != source:
        parser.error("source changed while generating the pack inventory; regenerate from clean main")
    os.set_blocking(sys.stdout.fileno(), True)
    print(json.dumps(document, ensure_ascii=False, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
