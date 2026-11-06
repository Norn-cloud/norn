#!/usr/bin/env python3
"""Generate one guarded/unguarded create writer per benchmark law and run the real proof driver."""
import itertools
import json
from pathlib import Path
import re
import subprocess
import sys

UNIT = {"system": "ucum", "code": "1"}
TYPES = {"text": {"kind": "text"}, "qty": {"kind": "decimal", "scale": 0, "unit": UNIT},
         "money": {"kind": "decimal", "scale": 2, "currencySource": {"links": [], "property": "currency"}}}
BOUND = (-3, 4)


def envelope(artifact, kind, payload, dependencies):
    return {"artifactId": artifact, "dependencies": dependencies, "irVersion": "0.1", "kind": kind,
            "lifecycle": "draft", "name": artifact, "namespace": "bench", "payload": payload,
            "provenance": {"source": "authoring_sdk", "sourcePath": "scripts/bench_proofs.py"},
            "scope": {"environmentId": "dev", "orgId": "org_demo"}, "version": "1.0.0"}


def declarations(law, props, guarded):
    subject = "bench.Subject"
    fields = [{"id": name, "type": TYPES[kind], **({"default": "0"} if kind == "qty" else {})}
              for name, kind in props.items()]
    units = [{"artifactId": "core.units", "kind": "entity", "required": True, "version": "1.0.0"}]
    entity = {"artifactId": f"{subject}.entity", "kind": "entity", "required": True, "version": "1.0.0"}
    schema = {name: ({"type": "string"} if kind == "text" else {"type": "number", "nornType": TYPES[kind]})
              for name, kind in props.items()}
    action = {"actionId": "bench.create", "title": "create", "idempotent": True, "ledgered": True,
              "scopeRequired": ["orgId", "environmentId"],
              "inputSchema": {"type": "object", "additionalProperties": False, "required": list(props), "properties": schema},
              "outputSchema": {"type": "object", "additionalProperties": False, "required": ["id"],
                               "properties": {"id": {"type": "string"}}},
              "effectProgram": {"kind": "upsert_input_object", "objectIdField": "id", "objectKind": subject,
                                "entityArtifactId": f"{subject}.entity", "entityArtifactVersion": "1.0.0",
                                "writeMode": {"kind": "create_only"}},
              "writes": [{"change": "created", "entityKind": subject}]}
    if guarded:
        action["preconditions"] = [law["expression"].replace("self.", "input.")]
    return [
        envelope(f"{subject}.entity", "entity", {"entityVersion": "2.0", "objectType": {
            "name": subject, "identity": [{"name": "primary", "properties": ["id"]}], "properties": fields}}, units),
        envelope("bench.create.action", "action", action, units + [entity]),
        envelope(f"bench.{law['id']}.law", "law", {
            "id": law["id"], "lawVersion": "0.1", "modality": {"kind": "always"}, "owner": "bench",
            "provenance": {"source": "authoring_sdk", "sourcePath": "scripts/bench_proofs.py"},
            "rationale": law["requirement"], "response": {"kind": "predicate", "expression": law["expression"]},
            "scope": {"objectTypes": [subject], "root": {"kind": "entity", "id": subject}},
            "severity": "error", "subject": subject}, [entity]),
    ]


def states(expression, fields, step):
    python = expression.replace("self.", "")
    lower, upper = BOUND
    domain = [value / step for value in range(lower * step, upper * step + 1)]
    enabled = sum(bool(eval(python, {}, dict(zip(fields, values))))  # noqa: S307 - checked-in literals only
                  for values in itertools.product(domain, repeat=len(fields)))
    return enabled + 1


def laws(root):
    rules = json.loads((root / "fixtures/bench/erp-rules.json").read_text())["rules"]
    for rule in rules:
        if "expression" in rule:
            fields = sorted(set(re.findall(r"self\.(\w+)", rule["expression"])))
            yield ({"id": rule["id"], "expression": rule["expression"], "requirement": rule["requirement"]},
                   {"id": "text", **dict.fromkeys(fields, "qty")}, fields, 1)
    cases = json.loads((root / "fixtures/bench/clause-cases.json").read_text())["cases"]
    for case in cases:
        for law in case.get("laws", []):
            entity = next(e for e in case["entities"] if e["name"] == law["on"])
            fields = sorted(set(re.findall(r"self\.(\w+)", law["expression"])))
            keep = {"id", "currency", "status", *fields}
            props = {name: kind for name, kind in entity["props"].items() if name in keep}
            step = 100 if "money" in props.values() else 1
            yield ({"id": law["id"], "expression": law["expression"], "requirement": law["text"]}, props, fields, step)


def main():
    root, binary, tools, work = Path.cwd(), sys.argv[1], sys.argv[2], Path(sys.argv[3])
    base = [json.loads((root / f"registry/core/artifacts/{n}.json").read_text()) for n in ("core.Process.interface", "core.units")]
    (work / "base.json").write_text(json.dumps(base))
    only = set(sys.argv[4:])
    for law, props, fields, step in laws(root):
        if only and law["id"] not in only:
            continue
        directory = work / law["id"]
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "original.json").write_text(json.dumps(declarations(law, props, True)))
        (directory / "mutant.json").write_text(json.dumps(declarations(law, props, False)))
        numeric = [f for f in fields if props.get(f) != "text"]
        try:
            expected, computed = states(law["expression"], numeric, step), True
        except (NameError, TypeError, SyntaxError):
            # The driver needs an integer; 0 is a placeholder, reported as "not computed".
            expected, computed = 0, False
        profile = {"profile": "norn.bench.one-create/1", "action": "bench.create.action", "identity": "sample",
                   "inputs": {f: [str(BOUND[0]), str(BOUND[1])] for f in numeric}, "seed": 0,
                   "expectedStates": expected, "exclusions": {"authorization": "pre-authorized; outside profile"}}
        reported = expected if computed else "not computed"
        (directory / "bounds.json").write_text(json.dumps(profile))
        try:
            run = subprocess.run([sys.executable, "scripts/run-generated-proofs.py", binary, str(directory / "original.json"),
                                  str(directory / "mutant.json"), str(work / "base.json"), str(directory / "bounds.json"),
                                  str(directory / "jobs"), tools], capture_output=True, text=True, timeout=500, check=False)
        except subprocess.TimeoutExpired:
            print("BENCH-PROOF " + json.dumps({"id": law["id"], "returncode": None, "expectedStates": reported,
                                                "report": None, "stderr": "proof driver timed out"}), flush=True)
            continue
        report = None
        for line in reversed(run.stdout.splitlines()):
            try:
                report = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
        print("BENCH-PROOF " + json.dumps({"id": law["id"], "returncode": run.returncode, "expectedStates": reported,
                                            "report": report, "stderr": run.stderr[-400:] if report is None else ""}), flush=True)


if __name__ == "__main__":
    main()
