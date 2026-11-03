#!/usr/bin/env python3
"""Execute declaration-generated jobs with tla-ts and an independent Apalache oracle."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import zipfile

from quint_apalache import ApalacheServer, HarnessError, run_quint_apalache, scrubbed_environment


def run(args):
    model = args.model.read_text()
    magnitudes = [abs(int(n)) for n in re.findall(r"(?<![A-Za-z_0-9])\d+", model)]
    if max(magnitudes, default=0) * 4096 > 2_147_483_647:
        return {"status": "unknown", "reason": "tla-ts integer arithmetic exceeds qualified profile"}
    root = args.tools.resolve()
    environment = scrubbed_environment(os.environ.copy())
    environment["QUINT_HOME"] = str(root / "quint-home")
    environment["PATH"] = str(root / "bin") + os.pathsep + environment["PATH"]
    jar = root / "quint-home/apalache-dist-0.56.1/apalache/lib/apalache.jar"
    if hashlib.sha256(jar.read_bytes()).hexdigest() != "4753c0ebb2cbb266e2c6ac19ab5ca3827d726cc80fd1fc5d7c1eeb64736cd60b":
        raise ValueError("Apalache compiler digest mismatch")
    with tempfile.TemporaryDirectory(prefix="norn-generated-quint-") as temporary:
        work = Path(temporary)
        with zipfile.ZipFile(jar) as archive:
            for name in ["Apalache", "Variants"]:
                (work / f"{name}.tla").write_bytes(archive.read(f"tla2sany/StandardModules/{name}.tla"))
        compilation = []
        for attempt in range(2):
            server = ApalacheServer(root / "quint-home", environment, lambda _: None)
            try:
                server.start()
                compiled = subprocess.run([
                    str(root / "bin/quint"), "compile", str(args.model), "--main", "generated",
                    "--target", "tlaplus", "--step", args.step, "--invariant", args.invariant,
                    "--out", str(work / "compile.json"), "--server-endpoint", f"127.0.0.1:{server.port}",
                ], env=environment, capture_output=True, timeout=120)
            finally:
                server.stop()
            compilation.append({"exitCode": compiled.returncode, "stdout": compiled.stdout.decode(), "stderr": compiled.stderr.decode()})
            if compiled.returncode == 0:
                break
            if attempt == 1:
                return {"status": "unknown", "reason": "Quint compilation failed", "compilation": compilation}
        tla = work / "generated.tla"
        tla.write_bytes(compiled.stdout)
        primary_path = work / "primary.json"
        subprocess.run(["bun", "run", "tools/norn-proofs/check.ts", str(tla), str(primary_path), str(work), str(args.seed)],
                       env=environment, capture_output=True, check=True, timeout=120)
        primary_raw = primary_path.read_text()
        primary = json.loads(primary_raw)
        oracle_path = work / "oracle.json"
        oracle_run = run_quint_apalache([
            str(root / "bin/quint"), "verify", str(args.model), "--main", "generated", "--backend", "apalache",
            "--step", args.step, "--invariants", args.invariant, "--max-steps", "1", "--out", str(oracle_path),
        ], environment=environment, timeout_seconds=120, result_root=work, emit=lambda line: print(line, file=sys.stderr, flush=True))
        oracle_raw = oracle_path.read_text()
        oracle = json.loads(oracle_raw)
        clean = oracle.get("errors") == []
        status = "unknown"
        expected = 1 if args.step == "idle" else args.expected_states
        if primary.get("status") == "success" and primary.get("distinctStates") == expected and clean and oracle.get("status") == "ok" and oracle_run.returncode == 0:
            status = "ok"
        if primary.get("status") == "invariant-violation" and primary.get("violation", {}).get("trace") and clean and oracle.get("status") == "violation" and oracle.get("trace"):
            status = "violation"
        return {"status": status, "errors": [], "reason": "backend parity" if status != "unknown" else "unqualified or disagreeing backend verdicts",
                "compilation": compilation, "primaryRaw": primary_raw, "oracleRaw": oracle_raw,
                "tlaSha256": hashlib.sha256(compiled.stdout).hexdigest(), "trace": primary.get("violation", {}).get("trace")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--tools", type=Path, required=True)
    parser.add_argument("--step", choices=["idle", "step"], required=True)
    parser.add_argument("--invariant", choices=["law", "unreachable"], required=True)
    parser.add_argument("--seed", type=int, required=True)
    parser.add_argument("--expected-states", type=int, required=True)
    args = parser.parse_args()
    try:
        result = run(args)
    except (OSError, ValueError, subprocess.SubprocessError, RuntimeError, HarnessError) as error:
        result = {"status": "unknown", "errors": [], "reason": str(error)}
    args.out.write_text(json.dumps(result, separators=(",", ":")))


if __name__ == "__main__":
    main()
