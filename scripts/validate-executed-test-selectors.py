#!/usr/bin/env python3
"""Bind every promoted `test:` selector to an observed passing test log line."""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

import coverage_manifest


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_GATES = ROOT / "verification/coverage-v1/promoted-gates.json"
DEFAULT_LOG_ROOT = ROOT / "target/verification-results/logs"


class SelectorEvidenceError(ValueError):
    pass


ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")


def strip_ansi(text: str) -> str:
    """Remove terminal colour/style sequences (CI runs cargo with CARGO_TERM_COLOR=always)."""
    return ANSI_ESCAPE.sub("", text)


def load_json(path: Path) -> dict:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SelectorEvidenceError(f"{path}: root must be an object")
    return value


@dataclass(frozen=True)
class ObservedTest:
    qualified_name: str
    leaf_name: str
    source_hint: str
    binary_hint: str


def rust_passes(log: str) -> list[ObservedTest]:
    observed: list[ObservedTest] = []
    source_hint = ""
    binary_hint = ""
    log = strip_ansi(log)
    for line in log.splitlines():
        runner = re.match(r"^\s*Running (?:unittests )?([^ ]+) \(([^)]+)\)", line)
        if runner is not None:
            source_hint = runner.group(1)
            binary_hint = Path(runner.group(2)).name
            continue
        passed = re.match(r"^test ([^\s]+)(?: - should panic)? \.\.\. ok$", line)
        if passed is None:
            continue
        qualified = passed.group(1)
        observed.append(
            ObservedTest(
                qualified,
                qualified.rsplit("::", 1)[-1],
                source_hint,
                binary_hint,
            )
        )
    expected_count = sum(
        int(match.group(1))
        for match in re.finditer(
            r"test result: ok\. ([0-9]+) passed", log, re.MULTILINE
        )
    )
    if len(observed) != expected_count:
        raise SelectorEvidenceError(
            "Rust passing-line count does not match cargo's passing-test summaries: "
            f"lines={len(observed)}, summaries={expected_count}"
        )
    return observed


def artifact_matches(artifact: str, test: ObservedTest) -> bool:
    parts = Path(artifact).parts
    if len(parts) < 3 or parts[0] != "crates":
        return False
    crate_binary = parts[1].replace("-", "_")
    boundary = next(
        (index for index, part in enumerate(parts) if part in {"src", "tests"}),
        None,
    )
    if boundary is None:
        return False
    source_relative = Path(*parts[boundary:]).as_posix()
    if source_relative.startswith("tests/"):
        expected_binary = Path(source_relative).stem
        return (
            test.source_hint == source_relative
            and test.binary_hint.startswith(f"{expected_binary}-")
        )
    if source_relative == "src/lib.rs":
        return (
            test.source_hint == source_relative
            and test.binary_hint.startswith(f"{crate_binary}-")
        )
    if not source_relative.startswith("src/") or not source_relative.endswith(".rs"):
        return False
    module = source_relative.removeprefix("src/").removesuffix(".rs")
    if module.endswith("/mod"):
        module = module.removesuffix("/mod")
    module_prefix = module.replace("/", "::") + "::"
    return (
        test.source_hint == "src/lib.rs"
        and test.binary_hint.startswith(f"{crate_binary}-")
        and test.qualified_name.startswith(module_prefix)
    )


def selector_observed(
    artifact: str, selector: str, observed: list[ObservedTest]
) -> bool:
    return any(
        selector in {test.qualified_name, test.leaf_name}
        and artifact_matches(artifact, test)
        for test in observed
    )


def run_canaries() -> None:
    rust = rust_passes(
        "     Running unittests src/lib.rs (target/debug/deps/canary)\n"
        "test storage::tests::exact_named_case ... ok\n"
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured\n"
    )
    rust = [
        ObservedTest(
            test.qualified_name,
            test.leaf_name,
            test.source_hint,
            "norn_engine-canary",
        )
        for test in rust
    ]
    if not selector_observed(
        "crates/norn-engine/src/storage.rs",
        "exact_named_case",
        rust,
    ) or selector_observed(
        "crates/norn-engine/src/deleted.rs", "exact_named_case", rust
    ):
        raise SelectorEvidenceError("Rust selector parser canary failed")
    coloured = rust_passes(
        "\x1b[1m\x1b[92m     Running\x1b[0m tests/canary_bin.rs (target/debug/deps/canary_bin-0123abcd)\n"
        "test coloured_header_case ... ok\n"
        "test result: \x1b[32mok\x1b[0m. 1 passed; 0 failed; 0 ignored; 0 measured\n"
    )
    if not selector_observed(
        "crates/norn-engine/tests/canary_bin.rs",
        "coloured_header_case",
        coloured,
    ):
        raise SelectorEvidenceError(
            "Rust selector parser did not attribute a test under an ANSI-coloured Running header"
        )
    wrong_crate = [
        ObservedTest(
            test.qualified_name,
            test.leaf_name,
            test.source_hint,
            "attacker_crate-canary",
        )
        for test in rust
    ]
    if selector_observed(
        "crates/norn-engine/src/storage.rs",
        "exact_named_case",
        wrong_crate,
    ):
        raise SelectorEvidenceError("Rust selector accepted a same-named foreign crate test")
def test_evidence(coverage: dict) -> Iterable[tuple[str, str, str, str, str]]:
    for subsystem in coverage["subsystems"]:
        for claim in subsystem["obligationClaims"]:
            for layer in ("runtime", "model", "proof"):
                for evidence in claim["evidence"][layer]:
                    for selector in evidence["selectors"]:
                        if selector.startswith("test:"):
                            yield (
                                subsystem["id"],
                                claim["id"],
                                evidence["artifact"],
                                evidence["command"],
                                selector.split(":", 1)[1],
                            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--coverage", type=Path, default=coverage_manifest.COVERAGE)
    parser.add_argument("--gates", type=Path, default=DEFAULT_GATES)
    parser.add_argument("--log-root", type=Path, default=DEFAULT_LOG_ROOT)
    parser.add_argument("--profile", choices=("fast", "full"), default="full")
    args = parser.parse_args()

    run_canaries()
    coverage = coverage_manifest.load(args.coverage)
    gate_manifest = load_json(args.gates)
    all_gates_by_command: dict[str, list[dict]] = defaultdict(list)
    gates_by_command: dict[str, list[dict]] = defaultdict(list)
    for gate in gate_manifest["gates"]:
        if gate["evidenceKind"] != "rust-tests":
            continue
        all_gates_by_command[gate["command"]].append(gate)
        if gate["profile"] == args.profile:
            gates_by_command[gate["command"]].append(gate)

    parsed_logs: dict[tuple[str, str], list[ObservedTest]] = {}
    checked = 0
    failures: list[str] = []
    required_selectors_by_gate: dict[str, int] = defaultdict(int)
    for subsystem_id, claim_id, artifact, command, selector in test_evidence(coverage):
        candidates = gates_by_command.get(command, [])
        if not candidates:
            # A declared selector may be owned solely by the other profile.
            # It is validated when that profile runs, not treated as evidence
            # for this profile's registered gates.
            if not all_gates_by_command.get(command):
                failures.append(
                    f"{subsystem_id}.{claim_id}: no Rust gate owns {command!r}"
                )
            continue

        observed = False
        missing_logs: list[str] = []
        for gate in candidates:
            required_selectors_by_gate[gate["id"]] += 1
            log_path = args.log_root / f"{gate['id']}.log"
            if not log_path.is_file():
                missing_logs.append(log_path.as_posix())
                continue
            cache_key = (gate["id"], gate["evidenceKind"])
            if cache_key not in parsed_logs:
                log = log_path.read_text(encoding="utf-8", errors="strict")
                parsed_logs[cache_key] = rust_passes(log)
            if selector_observed(
                artifact, selector, parsed_logs[cache_key]
            ):
                observed = True
                break
        if not observed:
            detail = (
                f"missing logs: {', '.join(missing_logs)}"
                if missing_logs
                else "selector was not an exact passing test from its cited artifact in any owning gate log"
            )
            failures.append(
                f"{subsystem_id}.{claim_id} test:{selector}: {detail}; command={command!r}"
            )
            continue
        checked += 1

    if args.profile == "fast":
        for gates in gates_by_command.values():
            for gate in gates:
                if required_selectors_by_gate[gate["id"]] == 0:
                    failures.append(
                        f"{gate['id']}: registered fast {gate['evidenceKind']} gate has no "
                        "declared test: selector"
                    )

    if failures:
        raise SelectorEvidenceError("\n".join(failures))
    print(
        f"bound {checked} {args.profile} promoted test selectors to observed passing gate log lines"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, KeyError, TypeError, json.JSONDecodeError, SelectorEvidenceError) as error:
        print(f"executed-selector validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
