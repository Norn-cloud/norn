#!/usr/bin/env python3
"""Exact executable-source enrollment, independent of coverage's source inventory.

Source ownership does not claim a theorem for every function. Existing coverage
claims and their limitations remain authoritative about what has been proved.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path

import coverage_manifest

ROOT = Path(__file__).resolve().parents[1]
EXECUTABLE_SUFFIXES = {
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".cjs", ".mjs", ".py", ".pyw",
    ".sh", ".bash", ".zsh", ".fish", ".ps1", ".bat", ".cmd",
    ".lean", ".qnt", ".fst", ".fsti", ".sql", ".wat", ".tla", ".dl",
    ".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".hxx",
    ".go", ".rb", ".php", ".pl", ".pm", ".lua", ".java", ".kt", ".kts",
    ".scala", ".swift", ".cs", ".fs", ".fsx", ".vb", ".m", ".mm",
    ".hs", ".lhs", ".ml", ".mli", ".ex", ".exs", ".erl", ".hrl",
    ".clj", ".cljs", ".cljc", ".r", ".R", ".s", ".S", ".asm",
    ".vue", ".svelte", ".rego", ".cedar", ".proto",
}
HELD_OBLIGATIONS = {
    "accounting-pack": {"INV-ACC-JOURNAL-BALANCED", "INV-ACC-POSTED-IMMUTABLE", "INV-ACC-PERIOD-LOCK", "INV-ACC-RECONCILIATION"},
    "inventory-authority": {"INV-INV-QUANTITY-CONSERVATION", "INV-INV-RESERVATION", "INV-INV-TRACKING-UNIQUE"},
}
HELD_GATES = {
    "accounting-pack": {"accounting-pack-runtime", "accounting-pack-lean", "accounting-authority-lean", "accounting-pack-quint-typecheck", "accounting-pack-quint"},
    "inventory-authority": {"inventory-authority-runtime", "inventory-authority-lean", "inventory-authority-quint-typecheck", "inventory-authority-quint"},
}


def repository_files(root: Path, directory: str):
    """Enumerate candidate bytes, excluding unshipped dependency/build caches."""
    probe = subprocess.run(["git", "-C", str(root), "rev-parse", "--show-toplevel"], capture_output=True, text=True)
    if probe.returncode == 0 and Path(probe.stdout.strip()).resolve() == root.resolve():
        result = subprocess.run(["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", directory], capture_output=True, check=True)
        return [root / name.decode() for name in result.stdout.split(b"\0") if name]
    return (root / directory).rglob("*")


def discover_sources(root: Path) -> set[str]:
    """Walk executable inputs on disk, including newly added modules/packages."""
    sources = set()
    for path in repository_files(root, "."):
        if not path.is_file():
            continue
        if path.is_symlink():
            raise ValueError(f"source is a symlink: {path.relative_to(root)}")
        with path.open("rb") as source:
            shebang = source.read(2) == b"#!"
        executable = (
            path.suffix in EXECUTABLE_SUFFIXES
            or path.name in {"Cargo.toml", "package.json", "pyproject.toml"}
            or bool(path.stat().st_mode & 0o111)
            or shebang
        )
        if path.suffix == ".json":
            try:
                artifact = json.loads(path.read_text(encoding="utf-8"))
            except (ValueError, UnicodeDecodeError):
                continue
            executable = executable or (isinstance(artifact, dict) and "irVersion" in artifact)
        if not executable:
            continue
        relative = path.relative_to(root)
        if path.is_symlink():
            raise ValueError(f"executable source is a symlink: {relative}")
        sources.add(relative.as_posix())
    return sources


def held_domains(root: Path) -> set[str]:
    """Recognize held authorities/laws even when renamed or outside source roots."""
    found = set()
    for directory in (".",):
        for path in repository_files(root, directory):
            if not path.is_file():
                continue
            name = path.stem.lower().replace("_", "-")
            if name in {"accounting", "accounting-pack", "accounting-authority", "accountingauthority", "accountingpack", "conservedquantity", "accounting-rules"}:
                found.add("accounting-pack")
            if name in {"inventory", "inventory-authority", "inventoryauthority"}:
                found.add("inventory-authority")
            if path.suffix in {".rs", ".lean", ".qnt", ".fst", ".fsti", ".ts"}:
                source = path.read_text(encoding="utf-8")
                if re.search(r"\b(?:struct|enum|namespace|module|class)\s+(?:Accounting(?:State|Pack|Authority)|ConservedQuantity)\b", source):
                    found.add("accounting-pack")
                if re.search(r"\b(?:struct|enum|namespace|module|class)\s+Inventory(?:State|Authority)\b", source):
                    found.add("inventory-authority")
            if path.suffix == ".json":
                try:
                    artifact = json.loads(path.read_text(encoding="utf-8"))
                except (ValueError, UnicodeDecodeError):
                    continue
                if isinstance(artifact, dict) and str(artifact.get("kind", "")).lower() == "law":
                    identity = str(artifact.get("artifactId", "")).lower()
                    payload = artifact.get("payload", {})
                    domain = payload.get("domain") if isinstance(payload, dict) else None
                    for token, owner in (("accounting", "accounting-pack"), ("inventory", "inventory-authority")):
                        if domain == token or token in identity:
                            found.add(owner)
    return found


def validate(root: Path, coverage: dict, gates: dict) -> set[str]:
    subsystems = {item["id"]: item for item in coverage["subsystems"]}
    actual = discover_sources(root)
    enrolled = set()
    for identity, subsystem in subsystems.items():
        files = subsystem.get("sourceFiles", [])
        if not files or len(files) != len(set(files)):
            raise ValueError(f"{identity}: source enrollment must be nonempty and unique")
        if enrolled.intersection(files):
            raise ValueError(f"{identity}: executable source has multiple enrollment owners")
        enrolled.update(files)
    if actual != enrolled:
        raise ValueError(f"exact source enrollment mismatch: unregistered={sorted(actual - enrolled)}, absent={sorted(enrolled - actual)}")
    present = held_domains(root)
    for identity in HELD_OBLIGATIONS:
        if (identity in subsystems) != (identity in present):
            raise ValueError(f"{identity}: enrollment must match independently discovered authority/law presence")
        if identity not in present:
            continue
        subsystem = subsystems[identity]
        gate_ids = {gate["id"] for gate in gates["gates"] if any(c["subsystem"] == identity for c in gate["classifications"])}
        if gate_ids != HELD_GATES[identity]:
            raise ValueError(f"{identity}: full domain runtime/Lean/Quint gate enrollment required")
        required = HELD_OBLIGATIONS[identity] | ({"INV-QL-ACCOUNT-GRAPH"} if identity == "accounting-pack" else set())
        if set(subsystem["obligations"]) != required:
            raise ValueError(f"{identity}: full domain obligation enrollment required")
        for layer in ("runtime", "model", "proof"):
            entries = [gate for gate in gates["gates"] if {"subsystem": identity, "layer": layer} in gate["classifications"]]
            if not entries or not subsystem[layer]["artifacts"] or not subsystem[layer]["commands"]:
                raise ValueError(f"{identity}: full {layer} enrollment required")
            for artifact in subsystem[layer]["artifacts"]:
                if not (root / artifact).is_file():
                    raise ValueError(f"{identity}: missing {layer} artifact {artifact}")
    return present


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="candidate checked only as data by this validator")
    root = parser.parse_args().root.resolve()
    coverage = coverage_manifest.load(root / "verification/coverage-v1/manifest")
    gates = json.loads((root / "verification/coverage-v1/promoted-gates.json").read_text())
    present = validate(root, coverage, gates)
    print(f"exact source enrollment: {len(discover_sources(root))} files, {len(coverage['subsystems'])} subsystems; held domains present: {sorted(present)}")


if __name__ == "__main__":
    main()
