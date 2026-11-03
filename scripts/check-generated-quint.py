#!/usr/bin/env python3
"""Execute declaration-generated jobs with tla-ts and an independent Apalache oracle."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import zipfile

from quint_apalache import HarnessError, kill_group, scrubbed_environment


def verdict(primary, oracle, returncode, timed_out, expected):
    """PROOF-QUINT-RUN-1: only complete, agreeing backend results qualify."""
    if timed_out or oracle.get("errors") != []:
        return "unknown"
    if returncode == 0 and primary.get("status") == "success" and primary.get("distinctStates") == expected and oracle.get("status") == "ok":
        return "ok"
    primary_shape = trace_shape(primary.get("violation", {}).get("trace"))
    if returncode == 12 and primary.get("status") == "invariant-violation" and primary_shape and oracle.get("status") == "violation" and primary_shape == trace_shape(oracle.get("trace")):
        return "violation"
    return "unknown"


def oracle_trace(trace):
    """Decode the pinned CLI's ITF scalar trace without numeric coercion."""
    if not isinstance(trace, dict):
        raise ValueError("missing Apalache ITF trace")
    variables, raw_states = trace.get("vars"), trace.get("states")
    if not isinstance(variables, list) or not variables or any(not isinstance(v, str) for v in variables) or len(set(variables)) != len(variables) or not isinstance(raw_states, list) or not raw_states:
        raise ValueError("malformed Apalache ITF trace")
    states = []
    for index, state in enumerate(raw_states):
        metadata = state.get("#meta") if isinstance(state, dict) else None
        if not isinstance(state, dict) or set(state) - {"#meta"} != set(variables) or not isinstance(metadata, dict) or type(metadata.get("index")) is not int or metadata["index"] != index:
            raise ValueError("malformed Apalache ITF state")
        values = {}
        for name in variables:
            value = state[name]
            if type(value) is bool:
                values[name] = value
            elif isinstance(value, dict) and set(value) == {"#bigint"} and isinstance(value["#bigint"], str):
                text = value["#bigint"]
                if len(text) > 11 or not re.fullmatch(r"0|-?[1-9][0-9]*", text) or not -2_147_483_648 <= int(text) <= 2_147_483_647:
                    raise ValueError("inexact or out-of-profile Apalache ITF integer")
                values[name] = int(text)
            else:
                raise ValueError("unsupported Apalache ITF scalar")
        states.append({"state": values})
    return states


def trace_shape(trace):
    if not isinstance(trace, list) or not trace:
        return None
    result = []
    for entry in trace:
        state = entry.get("state") if isinstance(entry, dict) else None
        if not isinstance(state, dict) or not state or type(state.get("present")) is not bool:
            return None
        if any(type(v) not in (int, bool) for v in state.values()):
            return None
        result.append((state["present"], sorted((k, type(v).__name__) for k, v in state.items())))
    return result


def oracle_verdict(returncode, stdout):
    outcomes = re.findall(r"^The outcome is: (\w+)\b", stdout, re.MULTILINE)
    if returncode == 0 and outcomes == ["NoError"]:
        return "ok"
    if returncode == 12 and outcomes == ["Error"] and re.search(r"State \d+: state invariant \d+ violated\.", stdout):
        return "violation"
    return "unknown"


def arithmetic_qualified(model):
    # Each expanded writer expression and law expression has at most 4096
    # scalar leaves. Laws can add derived post-state coefficients again.
    magnitudes = [abs(int(n)) for n in re.findall(r"(?<![A-Za-z_0-9])\d+", model)]
    return max(magnitudes, default=0) * 4096 * 4096 <= 2_147_483_647


def initializer_predicate(spec):
    """PROOF-QUINT-RUN-1: scoped equivalent of Quint 0.32.0 convertInit.

    Only the generated constant initializer is accepted. Other definitions,
    identifiers, types and effects pass unchanged to the pinned parser.
    """
    result = copy.deepcopy(spec)
    modules = result.get("modules", [])
    if len(modules) != 1 or modules[0].get("name") != "generated":
        raise ValueError("unsupported flattened Quint module")
    declarations = modules[0]["declarations"]
    names = [d.get("name") for d in declarations]
    if len(set(names)) != len(names) or any(d.get("kind") not in {"var", "def"} or d.get("qualifier") == "run" for d in declarations):
        raise ValueError("unsupported Quint declaration")
    definitions = {d["name"]: d for d in declarations if d["kind"] == "def"}
    init = definitions.get("init", {})
    alias = definitions.get("q::init", {})
    if init.get("qualifier") != "action" or alias.get("qualifier") != "action" or alias.get("expr", {}).get("kind") != "name" or alias["expr"].get("name") != "init":
        raise ValueError("unsupported Quint initializer alias")
    def visits(value):
        if isinstance(value, dict):
            yield value
            for child in value.values():
                yield from visits(child)
        elif isinstance(value, list):
            for child in value:
                yield from visits(child)
    for definition in declarations:
        if definition.get("name") != "q::init" and any(node.get("kind") == "name" and node.get("name") in {"init", "q::init"} for node in visits(definition.get("expr"))):
            raise ValueError("initializer reused outside q::init")
    expression = init.get("expr", {})
    if expression.get("kind") != "app" or expression.get("opcode") != "actionAll" or not expression.get("args"):
        raise ValueError("unsupported Quint initializer expression")
    assigned = []
    for assignment in expression["args"]:
        args = assignment.get("args", [])
        if assignment.get("kind") != "app" or assignment.get("opcode") != "assign" or len(args) != 2 or args[0].get("kind") != "name":
            raise ValueError("unsupported Quint initializer assignment")
        literal = args[1]
        if not ((literal.get("kind") == "int" and type(literal.get("value")) is int) or (literal.get("kind") == "bool" and type(literal.get("value")) is bool)):
            raise ValueError("nonconstant Quint initializer")
        assigned.append(args[0]["name"])
        assignment["opcode"] = "eq"
    variables = {d["name"] for d in declarations if d["kind"] == "var"}
    if len(set(assigned)) != len(assigned) or set(assigned) != variables:
        raise ValueError("Quint initializer must assign every state variable once")
    return result


def compile_process(command, environment, work, deadline):
    """A bounded compiler invocation owns and cleans its whole process group."""
    process = subprocess.Popen(command, env=environment, cwd=work, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=max(0.001, deadline - time.monotonic()))
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    finally:
        kill_group(process)


def compile_tla(args, root, environment, work):
    """Avoid the pre-parser RPC stall; preserve one 120-second compile budget."""
    deadline = time.monotonic() + 120
    stages = []
    compiled = compile_process([
        str(root / "bin/quint"), "compile", str(args.model.resolve()), "--main", "generated",
        "--target", "json", "--flatten", "--step", args.step, "--invariant", args.invariant,
    ], environment, work, deadline)
    stages.append({"stage": "quint-json", "exitCode": compiled.returncode,
                   "stdoutBytes": len(compiled.stdout),
                   "stdoutSha256": hashlib.sha256(compiled.stdout).hexdigest(), "stderr": compiled.stderr.decode()})
    if compiled.returncode != 0:
        raise ValueError("Quint JSON compilation failed: " + compiled.stderr.decode())
    source = work / "generated.qnt.json"
    source.write_text(json.dumps(initializer_predicate(json.loads(compiled.stdout))))
    tla = work / "generated.tla"
    parsed = compile_process([
        str(root / "quint-home/apalache-dist-0.56.1/apalache/bin/apalache-mc"),
        "--out-dir=" + str(work / "parser-output"), "parse", "--output=" + str(tla), str(source),
    ], environment, work, deadline)
    stages.append({"stage": "apalache-cli", "exitCode": parsed.returncode,
                   "stdout": parsed.stdout.decode(), "stderr": parsed.stderr.decode()})
    if parsed.returncode != 0 or not tla.is_file():
        raise ValueError("Apalache CLI compilation failed: " + parsed.stdout.decode() + parsed.stderr.decode())
    return tla.read_bytes(), stages


def run(args):
    model = args.model.read_text()
    if not arithmetic_qualified(model):
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
        tla_bytes, compilation = compile_tla(args, root, environment, work)
        tla = work / "generated.tla"
        primary_path = work / "primary.json"
        subprocess.run([str(args.bun), "run", "tools/norn-proofs/check.ts", str(tla), str(primary_path), str(work), str(args.seed)],
                       env=environment, capture_output=True, check=True, timeout=120)
        primary_raw = primary_path.read_text()
        primary = json.loads(primary_raw)
        oracle_directory = work / "oracle-run"
        checked = compile_process([
            str(root / "quint-home/apalache-dist-0.56.1/apalache/bin/apalache-mc"),
            "--out-dir=" + str(work / "oracle-output"), "--run-dir=" + str(oracle_directory),
            "check", "--init=q_init", "--next=q_step", "--inv=q_inv", "--length=1", "--output-traces=true", str(tla),
        ], environment, work, time.monotonic() + 120)
        status = oracle_verdict(checked.returncode, checked.stdout.decode())
        trace_raw = (oracle_directory / "violation.itf.json").read_text() if status == "violation" else None
        oracle = {"status": status, "errors": [], "trace": oracle_trace(json.loads(trace_raw)) if trace_raw else None}
        oracle_raw = json.dumps({"stdout": checked.stdout.decode(), "stderr": checked.stderr.decode(), "traceRaw": trace_raw}, separators=(",", ":"))
        expected = 1 if args.step == "idle" else args.expected_states
        status = verdict(primary, oracle, checked.returncode, False, expected)
        return {"status": status, "errors": [], "reason": "backend parity" if status != "unknown" else "unqualified or disagreeing backend verdicts",
                "compilation": compilation, "primaryRaw": primary_raw, "oracleRaw": oracle_raw,
                "oracleRun": {"exitCode": checked.returncode, "timedOut": False},
                "tlaSha256": hashlib.sha256(tla_bytes).hexdigest(), "traceBackend": "apalache", "trace": oracle.get("trace") if status == "violation" else None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--tools", type=Path, required=True)
    parser.add_argument("--bun", type=Path, required=True)
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
