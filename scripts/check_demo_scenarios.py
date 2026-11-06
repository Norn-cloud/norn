#!/usr/bin/env python3
"""Require each native drop profile to select exactly its declared scenarios."""

import json
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def required_scenarios(document, profiles):
    """Require one explicit, nonempty set of binary/name pairs per drop profile."""
    if not isinstance(document, dict) or set(document) != set(profiles):
        raise ValueError("required scenario data must cover every drop-* profile exactly")
    result = {}
    for profile, binaries in document.items():
        if not isinstance(binaries, dict) or not binaries:
            raise ValueError(f"{profile}: required binaries must be nonempty")
        cases = set()
        for binary, names in binaries.items():
            if not isinstance(binary, str) or not binary or not isinstance(names, list) or not names:
                raise ValueError(f"{profile}: expected a binary ID and nonempty scenario list")
            for name in names:
                if not isinstance(name, str) or not name or (binary, name) in cases:
                    raise ValueError(f"{profile}: invalid or duplicate required scenario")
                cases.add((binary, name))
        result[profile] = cases
    return result


def selected_scenarios(listing):
    """Read nextest's actual selection and refuse ignored selected cases."""
    selected = set()
    for binary, suite in listing["rust-suites"].items():
        for name, case in suite["testcases"].items():
            status = case["filter-match"]["status"]
            if status == "mismatch":
                continue
            if status != "matches" or case["ignored"]:
                raise ValueError(f"{binary} {name}: unknown selection status or ignored scenario")
            selected.add((binary, name))
    return selected


def check_profile(profile, required, listing):
    """Fail on partial selection as well as an accidentally broadened filter."""
    selected = selected_scenarios(listing)
    missing, extra = required - selected, selected - required
    if missing or extra:
        details = []
        for label, cases in (("missing", missing), ("unexpected", extra)):
            if cases:
                names = ", ".join(f"{binary} {name}" for binary, name in sorted(cases)[:10])
                details.append(f"{label} ({len(cases)}): {names}")
        raise ValueError(f"{profile}: " + "; ".join(details))


def unique_object(pairs):
    """Keep duplicate JSON keys from silently replacing required scenarios."""
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def main():
    """List every configured drop through nextest and check its frozen inventory."""
    config = tomllib.loads((ROOT / ".config/nextest.toml").read_text())
    profiles = {name for name in config["profile"] if name.startswith("drop-")}
    if not profiles:
        raise ValueError("no drop-* profiles declared")
    data = json.loads((ROOT / ".config/drop-scenarios.json").read_text(), object_pairs_hook=unique_object)
    required = required_scenarios(data, profiles)
    for profile in sorted(profiles):
        raw = subprocess.check_output([
            "cargo", "nextest", "list", "--locked", "--workspace", "--all-targets",
            "--profile", profile, "--message-format", "json",
        ], cwd=ROOT, text=True)
        check_profile(profile, required[profile], json.loads(raw, object_pairs_hook=unique_object))
        print(f"{profile}: exactly {len(required[profile])} required scenarios selected", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"drop scenario coverage: {error}", file=sys.stderr)
        sys.exit(1)
