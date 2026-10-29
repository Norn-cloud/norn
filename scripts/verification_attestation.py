#!/usr/bin/env python3
"""Shared evidence, selector, and tool identity helpers for verification attestations."""

from __future__ import annotations

import ast
import hashlib
import json
import os
import re
import shlex
import stat
import subprocess
from pathlib import Path
from typing import Iterable


IGNORED_EVIDENCE_DIRS = {".git", ".turbo", "__pycache__", "node_modules", "target"}
EXPECTED_HAX_COMMIT = "37ecb8240fc237377f63614622650190a45617ff"
EXPECTED_QUINT_SHA256 = "939b64095b706017f2f202c6f99c860c40be7c31bddc2b98557316e50f42cd7f"
EXPECTED_APALACHE_VERSION = "0.56.1"
EXPECTED_APALACHE_JAR_SHA256 = "4753c0ebb2cbb266e2c6ac19ab5ca3827d726cc80fd1fc5d7c1eeb64736cd60b"
EXPECTED_JAVA_SHA256 = "999c141a6e620c71aa4230ccb62341a89162f37818753ff006d766bf935dcbe1"


def relative(root: Path, path: Path) -> str:
    return path.resolve().relative_to(root.resolve()).as_posix()


def expand_evidence_root(root: Path, path: Path) -> set[str]:
    if path.is_file():
        return {relative(root, path)}
    if not path.is_dir():
        raise ValueError(f"evidence root is neither a file nor directory: {path}")
    expanded: set[str] = set()
    for candidate in path.rglob("*"):
        if any(part in IGNORED_EVIDENCE_DIRS for part in candidate.relative_to(path).parts):
            continue
        if candidate.is_file() and candidate.suffix != ".pyc":
            expanded.add(relative(root, candidate))
    return expanded


def expected_evidence_paths(
    root: Path,
    coverage: dict,
    coverage_path: Path,
) -> set[str]:
    """Return the exact repository/generated evidence closure selected by coverage."""

    paths = {
        relative(root, coverage_path),
        coverage["promotedGateManifest"],
    }
    for evidence_root in coverage["evidenceRoots"]:
        paths.update(expand_evidence_root(root, root / evidence_root))
    for subsystem in coverage["subsystems"]:
        for layer_name in ("runtime", "model", "proof"):
            layer = subsystem[layer_name]
            paths.update(layer["artifacts"])
            paths.update(layer.get("resultArtifacts", []))
            paths.update(layer.get("generatedArtifacts", []))
            correspondence = layer.get("correspondenceArtifact")
            if correspondence:
                paths.add(correspondence)
    return paths


def canonical_command(value: str) -> list[str]:
    """Canonicalize only ephemeral output paths, preserving semantic argv/env."""

    argv = shlex.split(value)
    canonical: list[str] = []
    index = 0
    while index < len(argv):
        if argv[index] in {"--out", "--target-dir"}:
            if index + 1 >= len(argv):
                raise ValueError(f"command option {argv[index]} has no value: {value}")
            index += 2
            continue
        canonical.append(argv[index])
        index += 1
    return canonical


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _run_bytes(*command: str, cwd: Path) -> bytes:
    return subprocess.run(
        command,
        cwd=cwd,
        check=True,
        capture_output=True,
    ).stdout


def collect_git_identity(root: Path) -> dict[str, object]:
    """Bind HEAD plus every tracked/untracked byte represented by this workspace."""

    commit = run("git", "rev-parse", "HEAD", cwd=root)
    commit_tree = run("git", "rev-parse", "HEAD^{tree}", cwd=root)
    status = _run_bytes(
        "git",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        cwd=root,
    )
    diff = _run_bytes(
        "git",
        "diff",
        "--no-ext-diff",
        "--binary",
        "--full-index",
        "--no-textconv",
        "HEAD",
        "--",
        cwd=root,
    )
    untracked = sorted(
        value
        for value in _run_bytes(
            "git", "ls-files", "--others", "--exclude-standard", "-z", cwd=root
        ).split(b"\0")
        if value
    )

    digest = hashlib.sha256()
    digest.update(b"norn.workspace-state.v1\0")
    digest.update(commit.encode("ascii"))
    digest.update(b"\0")
    digest.update(commit_tree.encode("ascii"))
    digest.update(b"\0status\0")
    digest.update(status)
    digest.update(b"\0diff\0")
    digest.update(diff)
    digest.update(b"\0untracked\0")
    for raw_path in untracked:
        path_string = os.fsdecode(raw_path)
        relative_path = Path(path_string)
        if relative_path.is_absolute() or ".." in relative_path.parts:
            raise ValueError(f"unsafe untracked Git path: {path_string!r}")
        path = root / relative_path
        metadata = path.lstat()
        digest.update(raw_path)
        digest.update(b"\0")
        digest.update(f"{stat.S_IFMT(metadata.st_mode):o}".encode("ascii"))
        digest.update(b"\0")
        if stat.S_ISREG(metadata.st_mode):
            with path.open("rb") as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(chunk)
        elif stat.S_ISLNK(metadata.st_mode):
            digest.update(os.fsencode(os.readlink(path)))
        else:
            raise ValueError(f"unsupported untracked Git file type: {path_string!r}")
        digest.update(b"\0")
    return {
        "commit": commit,
        "commitTree": commit_tree,
        "workspaceDirty": bool(status),
        "workspaceStatePolicy": "git-tracked-and-untracked-nonignored-v1",
        "workspaceStateSha256": digest.hexdigest(),
    }


def strip_comments_and_strings(
    source: str,
    *,
    line_marker: str,
    block: tuple[str, str],
) -> str:
    """Remove nested block comments, line comments, and double-quoted strings."""

    output: list[str] = []
    index = 0
    depth = 0
    in_string = False
    while index < len(source):
        if depth:
            if source.startswith(block[0], index):
                depth += 1
                output.extend(" " * len(block[0]))
                index += len(block[0])
            elif source.startswith(block[1], index):
                depth -= 1
                output.extend(" " * len(block[1]))
                index += len(block[1])
            else:
                output.append("\n" if source[index] == "\n" else " ")
                index += 1
            continue
        if in_string:
            if source[index] == "\\":
                width = min(2, len(source) - index)
                output.extend(" " * width)
                index += width
            elif source[index] == '"':
                output.append(" ")
                index += 1
                in_string = False
            else:
                output.append("\n" if source[index] == "\n" else " ")
                index += 1
            continue
        if source.startswith(line_marker, index):
            newline = source.find("\n", index)
            if newline < 0:
                output.extend(" " * (len(source) - index))
                break
            output.extend(" " * (newline - index))
            index = newline
        elif source.startswith(block[0], index):
            output.extend(" " * len(block[0]))
            index += len(block[0])
            depth = 1
        elif source[index] == '"':
            output.append(" ")
            index += 1
            in_string = True
        else:
            output.append(source[index])
            index += 1
    if depth:
        raise ValueError("unterminated block comment in verification source")
    if in_string:
        raise ValueError("unterminated string in verification source")
    return "".join(output)


def lean_declarations(source: str) -> tuple[set[str], set[str]]:
    """Return theorem/lemma names and def names, including active namespaces."""

    code = strip_comments_and_strings(
        source,
        line_marker="--",
        block=("/-", "-/"),
    )
    theorems: set[str] = set()
    definitions: set[str] = set()
    namespace = ""
    namespace_stack: list[str] = []
    declaration = re.compile(
        r"^\s*(?:(?:private|protected|noncomputable)\s+)*"
        r"(theorem|lemma|def)\s+([A-Za-z_][A-Za-z0-9_'.]*)\b"
    )
    for line in code.splitlines():
        opened = re.match(r"^\s*namespace\s+([A-Za-z_][A-Za-z0-9_'.]*)\s*$", line)
        if opened:
            namespace_stack.append(namespace)
            name = opened.group(1)
            namespace = f"{namespace}.{name}" if namespace else name
            continue
        if re.match(r"^\s*end(?:\s+[A-Za-z_][A-Za-z0-9_'.]*)?\s*$", line):
            if namespace_stack:
                namespace = namespace_stack.pop()
            continue
        found = declaration.match(line)
        if not found:
            continue
        kind, name = found.groups()
        # Lean prefixes the active namespace onto dotted names too:
        # `namespace A` + `def B.c` declares `A.B.c`. Only `_root_.` escapes it.
        if name.startswith("_root_."):
            name = name.removeprefix("_root_.")
            qualified = name
        else:
            qualified = f"{namespace}.{name}" if namespace else name
        target = theorems if kind in {"theorem", "lemma"} else definitions
        target.update({name, qualified})
    return theorems, definitions


def fstar_declarations(source: str) -> tuple[set[str], set[str]]:
    """Return F* Lemma-bearing `let` names and all `let` predicate names."""

    code = strip_comments_and_strings(
        source,
        line_marker="//",
        block=("(*", "*)"),
    )
    module_match = re.search(r"(?m)^\s*module\s+([A-Za-z_][A-Za-z0-9_'.]*)\s*$", code)
    module = module_match.group(1) if module_match else ""
    predicates: set[str] = set()
    theorems: set[str] = set()
    chunks = re.finditer(
        r"(?ms)^\s*let\s+(?:rec\s+)?([A-Za-z_][A-Za-z0-9_']*)\b"
        r"(.*?)(?=^\s*(?:let|type|module|open|class|instance)\b|\Z)",
        code,
    )
    for chunk in chunks:
        name, body = chunk.groups()
        names = {name, f"{module}.{name}"} if module else {name}
        predicates.update(names)
        if re.search(r"\:\s*Lemma\b", body):
            theorems.update(names)
    return theorems, predicates


def quint_definitions(source: str) -> set[str]:
    code = strip_comments_and_strings(
        source,
        line_marker="//",
        block=("/*", "*/"),
    )
    return set(
        re.findall(
            r"(?m)^\s*(?:pure\s+)?def\s+([A-Za-z_][A-Za-z0-9_']*)\b",
            code,
        )
    )


def _json_contains_exact_value(value: object, expected: str) -> bool:
    if isinstance(value, dict):
        return expected in value or any(
            _json_contains_exact_value(child, expected) for child in value.values()
        )
    if isinstance(value, list):
        return any(_json_contains_exact_value(child, expected) for child in value)
    return value == expected


def _typescript_test_is_declared(source: str, expected: str) -> bool:
    """Recognize an exact test/it description while ignoring commented calls."""

    # Reuse the lexer to erase comments and strings, then locate live test-call
    # prefixes. The description is read from the original source at that exact
    # offset, so a comment or an unrelated string cannot satisfy the selector.
    erased = strip_comments_and_strings(
        source,
        line_marker="//",
        block=("/*", "*/"),
    )
    for call in re.finditer(r"\b(?:test|it)(?:\.[A-Za-z_][A-Za-z0-9_]*)?\s*\(", erased):
        index = call.end()
        while index < len(source) and source[index].isspace():
            index += 1
        if index >= len(source) or source[index] not in {'"', "'", "`"}:
            continue
        quote = source[index]
        index += 1
        value: list[str] = []
        while index < len(source):
            if source[index] == "\\" and index + 1 < len(source):
                value.extend(source[index : index + 2])
                index += 2
                continue
            if source[index] == quote:
                if "".join(value) == expected:
                    return True
                break
            value.append(source[index])
            index += 1
    return False


def selector_is_declared(path: Path, selector: str) -> bool:
    """Require proof/model selectors to name declarations, not comments or strings."""

    selector_kind, selector_name = selector.split(":", 1)
    if path.suffix == ".llbc":
        # LLBC is binary. Bind its predicate selector through Aeneas' generated
        # translation manifest rather than accepting an uninspectable substring.
        manifest_path = path.parent / "lean/translation.json"
        if selector_kind != "predicate" or not manifest_path.is_file():
            return False
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        return any(
            function.get("rust_name") == selector_name
            for function in manifest.get("functions", [])
            if isinstance(function, dict)
        )
    source = path.read_text(encoding="utf-8", errors="strict")
    if path.suffix == ".lean":
        theorems, definitions = lean_declarations(source)
        if selector_kind == "theorem":
            return selector_name in theorems
        if selector_kind == "predicate":
            return selector_name in definitions
    elif path.suffix == ".fst":
        theorems, predicates = fstar_declarations(source)
        if selector_kind == "theorem":
            return selector_name in theorems
        if selector_kind == "predicate":
            return selector_name in predicates
    elif path.suffix == ".qnt" and selector_kind == "invariant":
        return selector_name in quint_definitions(source)
    elif path.suffix == ".rs" and selector_kind in {"test", "predicate", "harness"}:
        code = strip_comments_and_strings(source, line_marker="//", block=("/*", "*/"))
        return re.search(rf"\bfn\s+{re.escape(selector_name)}\b", code) is not None
    elif path.suffix == ".py" and selector_kind == "checker":
        tree = ast.parse(source, filename=str(path))
        return any(
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name == selector_name
            for node in ast.walk(tree)
        )
    elif path.suffix == ".sh" and selector_kind == "checker":
        code = strip_comments_and_strings(source, line_marker="#", block=("\0", "\0"))
        return re.search(rf"(?m)^\s*{re.escape(selector_name)}=", code) is not None
    elif path.suffix == ".ts" and selector_kind == "test":
        return _typescript_test_is_declared(source, selector_name)
    elif path.suffix == ".json" and selector_kind in {"fixture", "predicate"}:
        return (
            selector_kind == "fixture" and selector_name == path.stem
        ) or _json_contains_exact_value(json.loads(source), selector_name)
    return False


def run(
    *command: str,
    cwd: Path,
    extra_env: dict[str, str] | None = None,
    remove_env: Iterable[str] = (),
) -> str:
    environment = os.environ.copy()
    for name in remove_env:
        environment.pop(name, None)
    if extra_env:
        environment.update(extra_env)
    completed = subprocess.run(
        command,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
        env=environment,
    )
    return "\n".join(
        part.strip() for part in (completed.stdout, completed.stderr) if part.strip()
    )


def require_clean_source_tree(path: Path, *, label: str) -> None:
    if not (path / ".git").is_dir():
        raise ValueError(f"pinned {label} source tree is missing: {path}")
    dirty = run(
        "git",
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        cwd=path,
    )
    if dirty:
        raise ValueError(f"pinned {label} source tree is dirty")


def collect_tool_versions(root: Path) -> dict[str, str]:
    """Recompute every deterministic tool identity recorded in the result."""

    ontology_tool_root = root / "target/ontology-tools"
    ontology_python = ontology_tool_root / "venv/bin/python"
    if not ontology_python.is_file():
        raise ValueError("pinned ontology Python environment is missing")
    tool_root = root / "target/verification-tools"
    rustup_home = tool_root / "rustup"
    extraction_rustc = (
        rustup_home
        / "toolchains/nightly-2025-11-08-x86_64-unknown-linux-gnu/bin/rustc"
    )
    hax_source = tool_root / "sources/hax"
    if not rustup_home.is_dir() or not extraction_rustc.is_file():
        raise ValueError("pinned verification Rust toolchain is missing")
    require_clean_source_tree(hax_source, label="hax")
    hax_commit = run("git", "rev-parse", "HEAD", cwd=hax_source)
    if hax_commit != EXPECTED_HAX_COMMIT:
        raise ValueError(f"pinned hax source is at unexpected commit: {hax_commit}")
    extraction_env = {
        "OPAMROOT": str(tool_root / "opam-root"),
        "OPAMSWITCH": "norn-verification-5.4.1",
    }
    pinned_bin = tool_root / "bin"
    quint_binary = (pinned_bin / "quint").resolve()
    java_binary = (pinned_bin / "java").resolve()
    apalache_root = (
        tool_root
        / f"quint-home/apalache-dist-{EXPECTED_APALACHE_VERSION}/apalache"
    )
    apalache_launcher = apalache_root / "bin/apalache-mc"
    apalache_jar = apalache_root / "lib/apalache.jar"
    pinned_hashes = {
        "Quint": (quint_binary, EXPECTED_QUINT_SHA256),
        "Apalache": (apalache_jar, EXPECTED_APALACHE_JAR_SHA256),
        "Java": (java_binary, EXPECTED_JAVA_SHA256),
    }
    for label, (path, expected_hash) in pinned_hashes.items():
        if not path.is_file():
            raise ValueError(f"pinned {label} executable/artifact is missing: {path}")
        actual_hash = sha256_file(path)
        if actual_hash != expected_hash:
            raise ValueError(
                f"pinned {label} executable/artifact digest mismatch: {actual_hash}"
            )
    apalache_version = run(
        str(apalache_launcher),
        "version",
        cwd=root,
        extra_env={
            "PATH": f"{pinned_bin}:{os.environ.get('PATH', '')}",
            "APALACHE_JAR": str(apalache_jar),
        },
        remove_env=(
            "JVM_ARGS",
            "JVM_GC_ARGS",
            "JAVA_TOOL_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "_JAVA_OPTIONS",
            "CONFIG_FILE",
            "OUT_DIR",
            "RUN_DIR",
            "SMT_ENCODING",
            "WRITE_INTERMEDIATE",
        ),
    )
    if apalache_version != EXPECTED_APALACHE_VERSION:
        raise ValueError(f"pinned Apalache version mismatch: {apalache_version}")
    return {
        "rustc": run("rustc", "--version", cwd=root),
        "cargo": run("cargo", "--version", cwd=root),
        "bun": run("bun", "--version", cwd=root),
        "python": run("python3", "--version", cwd=root),
        "jsonschema": run(
            "python3",
            "-c",
            "from importlib.metadata import version; print(version('jsonschema'))",
            cwd=root,
        ),
        "lean": run(
            str(pinned_bin / "lean"),
            "--version",
            cwd=root,
            extra_env={"ELAN_HOME": str(tool_root / "elan")},
        ),
        "quint": run(str(pinned_bin / "quint"), "--version", cwd=root),
        "quintBinarySha256": sha256_file(quint_binary),
        "apalache": apalache_version,
        "apalacheJarSha256": sha256_file(apalache_jar),
        "kani": run(
            str(pinned_bin / "kani"),
            "--version",
            cwd=root,
            extra_env={"KANI_HOME": str(tool_root / "kani-home")},
        ),
        "z3": run(str(pinned_bin / "z3"), "--version", cwd=root),
        "java": run(str(pinned_bin / "java"), "-version", cwd=root),
        "javaBinarySha256": sha256_file(java_binary),
        "owlrl": run(
            str(ontology_python),
            "-c",
            "from importlib.metadata import version; print(version('owlrl'))",
            cwd=root,
        ),
        "pyshacl": run(
            str(ontology_python),
            "-c",
            "from importlib.metadata import version; print(version('pyshacl'))",
            cwd=root,
        ),
        "rdflib": run(
            str(ontology_python),
            "-c",
            "from importlib.metadata import version; print(version('rdflib'))",
            cwd=root,
        ),
        "fstar": run(str(pinned_bin / "fstar.exe"), "--version", cwd=root),
        "cargoHax": run(str(pinned_bin / "cargo-hax"), "--version", cwd=root),
        "haxSourceCommit": hax_commit,
        "haxSourceTree": run("git", "rev-parse", "HEAD^{tree}", cwd=hax_source),
        "haxRustc": run(str(extraction_rustc), "--version", cwd=root),
        "opamSwitchState": run(
            str(tool_root / "bin/opam"),
            "list",
            "--installed",
            "--columns=name,version",
            "--short",
            cwd=root,
            extra_env=extraction_env,
        ),
        "opamRepository": run(
            str(tool_root / "bin/opam"),
            "repository",
            "list",
            "--all-switches",
            cwd=root,
            extra_env=extraction_env,
        ),
    }


def ensure_files_exist(root: Path, paths: Iterable[str]) -> None:
    for path in paths:
        if not (root / path).is_file():
            raise ValueError(f"declared evidence artifact is missing: {path}")
