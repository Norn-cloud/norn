"""Generic offline differential parity and app scoreboard CLI (draft)."""
from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
import importlib.metadata
import json
import os
import random
import select
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

# Hypothesis keeps a cache directory; never let it land in the caller's tree.
os.environ.setdefault("HYPOTHESIS_STORAGE_DIRECTORY", str(Path(tempfile.gettempdir()) / "norn-port-hypothesis"))

from hypothesis import HealthCheck, Phase, find, given, seed, settings, strategies as st
from hypothesis_jsonschema import from_schema
from jsonschema import Draft202012Validator

from json_compare import canonical, equal

ROOT = Path(__file__).resolve().parent


def read(path):
    return json.loads(Path(path).read_text())


def write(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=True, allow_nan=False) + "\n")


def generated(schema, generator_seed, count):
    cases = []

    @seed(generator_seed)
    @settings(max_examples=count, database=None, deadline=None, phases=[Phase.generate],
              suppress_health_check=[HealthCheck.too_slow, HealthCheck.filter_too_much])
    @given(case=from_schema(schema), nonce=st.integers())
    def collect(case, nonce):
        cases.append(case)

    collect()
    if not equal(len(cases), count):
        raise RuntimeError(f"generator produced {len(cases)} cases; required {count}")
    return cases


def communicate(command, lines, timeout=120):
    result = subprocess.run(command, input="\n".join(json.dumps(line, ensure_ascii=True) for line in lines) + "\n",
                            text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"process failed ({result.returncode}): {result.stderr[-4000:]}")
    # JSON-lines uses LF only; Unicode separators are legal inside JSON strings.
    return [json.loads(line) for line in result.stdout.split("\n") if line]


class JsonLines:
    """Reuse an admitted evaluator during shrinking; one LF frame per call."""
    def __init__(self, command):
        self.errors = tempfile.TemporaryFile(mode="w+t")
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.errors, text=True, bufsize=1)

    def __enter__(self):
        return self

    def call(self, value):
        self.process.stdin.write(json.dumps(value, ensure_ascii=True) + "\n")
        self.process.stdin.flush()
        if not select.select([self.process.stdout], [], [], 120)[0]:
            raise TimeoutError("JSON-lines response deadline exceeded")
        line = self.process.stdout.readline()
        if not line:
            self.errors.seek(0)
            raise RuntimeError("JSON-lines process closed: " + self.errors.read()[-4000:])
        return json.loads(line)

    def __exit__(self, exc_type, exc_value, traceback):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        finally:
            self.process.stdout.close()
            self.errors.close()


def candidate(response):
    if not response["ok"]:
        raise RuntimeError(f"Norn evaluator refused: {response['error']}")
    output = response["output"]
    if output["facts"] or output["timers"] or not equal(len(output["commands"]), 1):
        raise RuntimeError("parity adapter expects exactly one candidate result record and no effects")
    return output["commands"][0]


def original(spec, source_root, cases):
    request = {"path": str((source_root / spec["source"]["path"]).resolve()),
               "symbol": spec["source"]["symbol"], "invocation": spec.get("invocation", {})}
    result = communicate([sys.executable, "-I", str(ROOT / "original.py")], [request, *cases])
    if not equal(len(result), len(cases) + 1):
        raise RuntimeError("original response count differs from input count")
    return result[0]["sourceSha256"], result[1:]


def eval_command(spec, defs, norn):
    command = [norn, "eval", "--defs", str(defs), "--org", "port-calibration", "--env", "offline",
               "--rule", spec["artifactId"]]
    self_contained(spec)
    return command


def self_contained(spec):
    """This CLI composes no registry packages: a unit's definitions are its whole set."""
    if spec.get("registry"):
        raise ValueError("registry composition is not available; parity definitions must be self-contained")


def admitted_context(specs, defs, norn):
    """Re-admit the complete candidate set through the authority.

    The set is every definition file in name order under an empty context,
    exactly what `norn eval` admits. CheckReceipt.artifactSet carries every
    artifact's canonical identity. Never substitute a home-grown digest here.
    """
    for spec in specs:
        self_contained(spec)
    documents = [path.read_text() for path in sorted(Path(defs).glob("*.json"))]
    with tempfile.TemporaryDirectory() as temp:
        artifacts, context = Path(temp) / "artifacts.json", Path(temp) / "context.json"
        artifacts.write_text("[" + ",".join(documents) + "]")
        context.write_text("{}\n")
        checked = subprocess.run([norn, "check", "--artifacts", str(artifacts), "--context", str(context)],
                                 text=True, capture_output=True, timeout=120)
    if checked.returncode:
        raise ValueError("current definition context refused: " + (checked.stderr or checked.stdout)[-2000:])
    report = json.loads(checked.stdout)
    if not report.get("admitted") or not report.get("receipt", {}).get("admitted"):
        raise ValueError("current definition context is not admitted")
    return {key: report[key] for key in ("receiptSha256", "receipt")}


def translation(spec, defs, norn, cases):
    responses = communicate(eval_command(spec, defs, norn), [{"slice": case, "event": {}} for case in cases])
    if not equal(len(responses), len(cases)):
        raise RuntimeError("evaluator response count differs from input count")
    return [candidate(response) for response in responses]


def run_unit(spec, source_root, defs, norn, output_dir, manifest_path):
    manifest_bytes = manifest_path.read_bytes()
    if not any(equal(spec, item) for item in json.loads(manifest_bytes)["units"]):
        raise ValueError("parity spec must match its current manifest unit")
    manifest_hash = hashlib.sha256(manifest_bytes).hexdigest()
    context = admitted_context([spec], defs, norn)
    schema = spec["generatorSchema"]
    seeds = spec["seeds"]
    validator = Draft202012Validator(schema)
    for case in seeds:
        validator.validate(case)
    cases = [*seeds, *generated(schema, spec["seed"], spec.get("cases", 1000))]
    digest, before = original(spec, source_root, cases)
    after = translation(spec, defs, norn, cases)
    divergences = []
    minimized = {}
    with ExitStack() as stack:
        source_stream = norn_stream = None
        for index, (case, left, right) in enumerate(zip(cases, before, after, strict=True)):
            if equal(left, right):
                continue
            # Keep EVERY failed case; share shrinking only for the same outcome pair.
            signature = json.dumps([canonical(left), canonical(right)], sort_keys=True)
            if signature not in minimized:
                if source_stream is None:
                    source_stream = stack.enter_context(JsonLines([sys.executable, "-I", str(ROOT / "original.py")]))
                    observed_hash = source_stream.call({"path": str((source_root / spec["source"]["path"]).resolve()),
                        "symbol": spec["source"]["symbol"], "invocation": spec.get("invocation", {})})
                    if not equal(observed_hash["sourceSha256"], digest):
                        raise RuntimeError("source changed during shrinking")
                    norn_stream = stack.enter_context(JsonLines(eval_command(spec, defs, norn)))
                def outcomes(value):
                    return source_stream.call(value), candidate(norn_stream.call({"slice": value, "event": {}}))
                def differs(value):
                    observed, expected = outcomes(value)
                    return not equal(observed, expected)
                # Retain the known counterexample even when the mismatch is rare.
                example = find(st.one_of(from_schema(schema), st.just(case)), differs,
                               settings=settings(max_examples=1000, database=None, deadline=None),
                               random=random.Random(spec["seed"]))
                observed, expected = outcomes(example)
                minimized[signature] = {"input": example, "original": observed, "norn": expected}
            divergences.append({"case": index, "input": case, "original": left, "norn": right,
                                "minimized": minimized[signature], "accepted": False})
    artifact_path = next(path for path in sorted(defs.glob("*.json"))
                         if any(equal(item.get("artifactId"), spec["artifactId"]) for item in
                                (read(path) if isinstance(read(path), list) else [read(path)])))
    artifact_hash = hashlib.sha256(artifact_path.read_bytes()).hexdigest()
    if not equal(context, admitted_context([spec], defs, norn)):
        raise ValueError("definition context changed during parity")
    report = {"admissionContext": context, "format": "norn.port-parity/0.1-draft", "unit": spec["id"], "originalSha256": digest,
              "artifactId": spec["artifactId"], "nornArtifactSha256": artifact_hash, "artifactHashMode": "exact-file-bytes",
              "parityManifestSha256": manifest_hash, "manifestHashMode": "exact-file-bytes",
              "generatorSeed": spec["seed"], "seedCases": len(seeds), "generatedCases": len(cases) - len(seeds),
              "testSeeds": seeds, "pythonVersion": sys.version,
              "generatorVersions": {name: importlib.metadata.version(name) for name in ["hypothesis", "hypothesis-jsonschema", "jsonschema"]},
              "nornBinarySha256": hashlib.sha256(Path(norn).read_bytes()).hexdigest(),
              "caseCount": len(cases), "agreementRate": (len(cases) - len(divergences)) / len(cases),
              "divergences": divergences, "inputDomain": schema, "seedEvidence": spec["seedEvidence"]}
    report_path = output_dir / (spec["reportName"] + ".json")
    write(report_path, report)
    report_path.with_suffix(".md").write_text(
        f"{spec['id']}: {len(cases) - len(divergences)}/{len(cases)} agreement "
        f"({report['agreementRate']:.2%}); {len(seeds)} test seeds, {len(cases) - len(seeds)} generated, "
        f"generator seed {spec['seed']}.\n\nSource SHA256: `{digest}`. "
        f"Artifact SHA256: `{artifact_hash}`. Manifest SHA256: `{manifest_hash}`. All divergences and minimized inputs: "
        f"[{report_path.name}]({report_path.name}). Draft evidence, not runtime installation.\n")
    return report


def artifact_hashes(defs):
    hashes = {}
    for path in sorted(defs.glob("*.json")):
        data = read(path)
        for artifact in data if isinstance(data, list) else [data]:
            artifact_id = artifact.get("artifactId")
            if artifact_id in hashes:
                raise ValueError(f"duplicate artifact id: {artifact_id}")
            hashes[artifact_id] = hashlib.sha256(path.read_bytes()).hexdigest()
    return hashes


def current_reports(unit, reports, hashes, source_root, manifest, manifest_hash, context=None):
    """Fail closed unless every current artifact has exactly one fresh report."""
    expected = set(unit["nornArtifactIds"])
    if not expected or not equal(len(reports), len(expected)):
        return False
    if not equal(sorted(report.get("artifactId", "") for report in reports), sorted(expected)):
        return False
    source = source_root / unit["source"]["path"]
    if not source.is_file():
        return False
    source_hash = hashlib.sha256(source.read_bytes()).hexdigest()
    specs = [spec for spec in manifest["units"] if equal(spec.get("id"), unit["id"])]
    if not equal(len(specs), len(expected)):
        return False
    if not equal(sorted(spec.get("artifactId", "") for spec in specs), sorted(expected)):
        return False
    if not all(equal(spec.get("source", {}).get(key), unit["source"].get(key))
               for spec in specs for key in ("path", "symbol")):
        return False
    return all((context is None or equal(report.get("admissionContext"), context))
               and equal(report.get("originalSha256"), source_hash)
               and equal(report.get("artifactHashMode"), "exact-file-bytes")
               and report.get("artifactId") in hashes
               and equal(report.get("nornArtifactSha256"), hashes[report["artifactId"]])
               and equal(report.get("manifestHashMode"), "exact-file-bytes")
               and equal(report.get("parityManifestSha256"), manifest_hash)
               for report in reports)


def scoreboard(inventory_path, defs, reports, output, authoring, source_root, manifest_path):
    manifest_bytes = manifest_path.read_bytes()
    manifest = json.loads(manifest_bytes)
    manifest_hash = hashlib.sha256(manifest_bytes).hexdigest()
    inventory = read(inventory_path)
    Draft202012Validator(read(ROOT / "inventory.schema.json")).validate(inventory)
    units = inventory["units"]
    if not equal(len({unit["id"] for unit in units}), len(units)):
        raise ValueError("inventory unit ids must be unique")
    report_list = [read(path) for path in reports.glob("*.json") if read(path).get("format", "").startswith("norn.port-parity/")]
    by_id = {}
    for report in report_list:
        by_id.setdefault(report["unit"], []).append(report)
    hashes = artifact_hashes(defs)
    translated = [unit for unit in units if equal(unit["status"], "translated")]
    measured = [unit for unit in translated
                if current_reports(unit, by_id.get(unit["id"], []), hashes, source_root, manifest, manifest_hash)]
    passed = sum(all(report.get("generatedCases", 0) >= 1000
                     and "divergences" in report
                     and all(d.get("accepted", False) and d.get("acceptanceReason")
                             for d in report["divergences"])
                     for report in by_id[unit["id"]]) for unit in measured)
    result = {"format": "norn.port-scoreboard/0.1", "app": inventory["app"],
              "sourceLoc": inventory["sourceLoc"], "translatedSourceLoc": sum(unit["loc"] for unit in translated),
              "nornDefinitionLoc": sum(len(path.read_text().splitlines()) for path in authoring.rglob("*.ts") if not path.name.endswith(".d.ts")),
              "nornAuthoringLoc": sum(len(path.read_text().splitlines()) for path in authoring.rglob("*.ts") if not path.name.endswith(".d.ts")),
              "irJsonLoc": sum(len(path.read_text().splitlines()) for path in defs.glob("*.json")),
              "irRuleJsonLoc": sum(len(path.read_text().splitlines()) for path in defs.glob("*.json") if equal(read(path).get("kind"), "rule")),
              "irOntologyJsonLoc": sum(len(path.read_text().splitlines()) for path in defs.glob("*.json") if not equal(read(path).get("kind"), "rule")),
              "unitsByStatus": dict(Counter(unit["status"] for unit in units)),
              "translatedUnits": len(translated), "parityPassedUnits": passed,
              "unmeasuredUnits": [unit["id"] for unit in translated if unit["id"] not in {item["id"] for item in measured}],
              "parityPassRate": passed / len(translated) if translated else None,
              "cases": sum(report["caseCount"] for report in report_list),
              "elapsedMinutes": inventory.get("elapsedMinutes"), "modelTokens": inventory.get("modelTokens")}
    write(output, result)
    Path(output).with_suffix(".md").write_text(
        f"{result['app']}: {result['sourceLoc']} source LOC / {result['nornDefinitionLoc']} TS authoring LOC / {result['irJsonLoc']} IR JSON LOC.\n\n"
        f"Units: {result['unitsByStatus']}. Parity: {passed}/{len(translated)} translated units; "
        f"{result['cases']} cases. Source LOC includes fallback code. TS LOC counts authoring modules and shared support; IR JSON is compile/import output and includes ontology.\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    parity = commands.add_parser("parity")
    parity.add_argument("--manifest", type=Path, required=True)
    parity.add_argument("--source-root", type=Path, required=True)
    parity.add_argument("--defs", type=Path, required=True)
    parity.add_argument("--norn", required=True)
    parity.add_argument("--reports", type=Path, required=True)
    scores = commands.add_parser("scoreboard")
    scores.add_argument("--manifest", type=Path, required=True)
    scores.add_argument("--source-root", type=Path, required=True)
    scores.add_argument("--inventory", type=Path, required=True)
    scores.add_argument("--defs", type=Path, required=True)
    scores.add_argument("--authoring", type=Path, required=True)
    scores.add_argument("--reports", type=Path, required=True)
    scores.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if equal(args.command, "scoreboard"):
        print(json.dumps(scoreboard(args.inventory, args.defs, args.reports, args.output, args.authoring, args.source_root, args.manifest)))
        return
    failed = False
    for spec in read(args.manifest)["units"]:
        if spec.get("cases", 1000) < 1000:
            raise ValueError("parity requires at least 1000 generated cases per unit")
        report = run_unit(spec, args.source_root, args.defs, args.norn, args.reports, args.manifest)
        print(f"{spec['id']}: {report['agreementRate']:.2%}", flush=True)
        failed |= bool(report["divergences"])
    if failed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
