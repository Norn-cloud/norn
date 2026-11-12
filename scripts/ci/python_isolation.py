#!/usr/bin/env python3
"""Base-owned Python isolation policy over a checkout read as data (#513).

Python puts the script's directory (or, for `-c` and `-`, the working directory) first on
`sys.path`, so a file such as `scripts/ci/argparse.py` would replace the standard library
inside a gated script. The trusted lanes start Python with `-I`, the gated shell scripts
export `PYTHONSAFEPATH=1`, and gated Python loads its siblings by explicit path. This check
keeps all of that from regressing:

- every `python`/`python3` a workflow runs carries `-I`;
- each gated shell script starts with the `PYTHONSAFEPATH` export and the assertion that it
  took effect, and contains nothing that drops it (`env -i`/`-u`, `sudo`, `--pure`,
  `exec -c`, unsetting or reassigning it, or `python -E`);
- no tracked file anywhere is importable under a standard-library module name;
- a gated Python file never touches `sys.path`, imports only the standard library or the
  pinned third-party packages, and every file it loads by an explicit literal path
  (`_sibling`, `spec_from_file_location`, `run_path`, `ROOT / "....py"`) is a gate file.

Loads through computed paths are not followed; gated files use literal ones.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parents[2]
GATE_SOURCE = "crates/norn-skeleton-guard/src/ci/mod.rs"
SHELL_GATES = ("scripts/ci/core.sh", "scripts/verify-promoted-subsystems.sh")
SAFE_EXPORT = "export PYTHONSAFEPATH=1"
SAFE_ASSERT = ("python3 -c 'import sys; sys.exit(0 if sys.flags.safe_path else 1)'"
               ' || { echo "PYTHONSAFEPATH not honoured" >&2; exit 1; }')
# Third-party packages gated Python may import; the CI venv pins their versions (core.sh).
THIRD_PARTY = frozenset({"jsonschema", "yaml"})
IMPORTABLE_SUFFIXES = frozenset({"py", "pyc", "pyo", "pyw", "pyd", "so"})
ENV_DROPS = (
    (re.compile(r"\benv\s+(?:-\w*[iu]\b|--ignore-environment\b|--unset\b)"), "`env -i`/`env -u`"),
    (re.compile(r"\bsudo\b"), "`sudo`"),
    (re.compile(r"--pure\b"), "`--pure`"),
    (re.compile(r"\bexec\s+-\w*c"), "`exec -c`"),
    (re.compile(r"\bunset\b[^#\n]*\bPYTHONSAFEPATH\b|export\s+-n\s+PYTHONSAFEPATH\b"), "unsetting PYTHONSAFEPATH"),
    (re.compile(r"\bPYTHONSAFEPATH=(?!1(?:\s|$))"), "reassigning PYTHONSAFEPATH"),
    (re.compile(r"(?:^|[\s;&|(\"'])(?:[\w./${}-]*/)?python3?(?:\.\d+)?\"?\s+-[A-Za-z]*E"), "`python -E`"),
)
WORKFLOW_PYTHON = re.compile(r"(?:^|[\s;&|(\"'`])((?:[\w./${}-]*/)?python3?(?:\.\d+)?)\"?[ \t]+(\S+)")


def tracked(root: Path) -> list[str]:
    out = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True)
    return [path for path in out.stdout.decode().split("\0") if path]


def gates(gate_source: str) -> tuple[set[str], tuple[str, ...]]:
    """`GATE_FILES` and `GATE_DIRECTORIES` as the base's guard declares them."""
    def block(name: str) -> list[str]:
        return re.findall(r'"([^"]+)"', gate_source.split(f"const {name}:", 1)[1].split("];", 1)[0])
    return set(block("GATE_FILES")), tuple(block("GATE_DIRECTORIES"))


def is_gated(path: str, files: set[str], directories: tuple[str, ...]) -> bool:
    return path in files or path.startswith(directories) or path.startswith(".github/workflows/")


def stdlib_shadows(paths: list[str]) -> list[str]:
    names = set(sys.stdlib_module_names)
    errors = []
    for path in paths:
        pure = PurePosixPath(path)
        stem, _, suffix = pure.name.partition(".")
        if stem in names and suffix.rsplit(".", 1)[-1] in IMPORTABLE_SUFFIXES:
            errors.append(f"{path}: importable as standard-library module `{stem}`")
        if pure.name.startswith("__init__.") and pure.parent.name in names:
            errors.append(f"{path}: makes `{pure.parent}` a package named like standard-library module `{pure.parent.name}`")
    return errors


def workflow_errors(path: str, text: str) -> list[str]:
    errors = []
    for number, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        for match in WORKFLOW_PYTHON.finditer(line):
            if match.group(2) != "-I":
                errors.append(f"{path}:{number}: `{match.group(1)}` runs without `-I`")
    return errors


def shell_errors(path: str, text: str) -> list[str]:
    lines = text.splitlines()
    statements = [line for line in lines[1:] if line.strip() and not line.lstrip().startswith("#")]
    errors = []
    if statements[:2] != [SAFE_EXPORT, SAFE_ASSERT]:
        errors.append(f"{path}: must start with `{SAFE_EXPORT}` and the assertion that it took effect")
    for number, line in enumerate(lines, 1):
        if line.lstrip().startswith("#"):
            continue
        for pattern, label in ENV_DROPS:
            if pattern.search(line):
                errors.append(f"{path}:{number}: {label} would drop PYTHONSAFEPATH")
    return errors


def loaded_paths(path: str, tree: ast.AST) -> list[tuple[str, str | None]]:
    """Each explicit literal load in a gated file: (description, repository path or None)."""
    here = PurePosixPath(path).parent
    loads: list[tuple[str, str | None]] = []

    def literal(node: ast.AST) -> str | None:
        return node.value if isinstance(node, ast.Constant) and isinstance(node.value, str) else None

    for node in ast.walk(tree):
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
            value = literal(node.right)
            if value and value.endswith(".py"):
                loads.append((value, value))
        if not isinstance(node, ast.Call):
            continue
        name = node.func.attr if isinstance(node.func, ast.Attribute) else getattr(node.func, "id", "")
        if name == "_sibling" and node.args and literal(node.args[0]):
            module = f"{literal(node.args[0])}.py"
            if len(node.args) == 1 and not node.keywords:
                loads.append((module, str(here / module)))
            elif (len(node.args) == 2 and isinstance(node.args[1], ast.BinOp)
                  and literal(node.args[1].right) == "scripts"):
                loads.append((module, f"scripts/{module}"))
            else:
                loads.append((f"_sibling({module}) from a computed directory", None))
        elif name == "with_name" and node.args and (literal(node.args[0]) or "").endswith(".py"):
            loads.append((literal(node.args[0]), str(here / literal(node.args[0]))))
    return loads


def gated_python_errors(root: Path, path: str, files: set[str], directories: tuple[str, ...],
                        present: set[str]) -> list[str]:
    try:
        tree = ast.parse((root / path).read_text(encoding="utf-8"), filename=path)
    except (OSError, SyntaxError, UnicodeDecodeError) as error:
        return [f"{path}: gated Python does not parse: {error}"]
    allowed = set(sys.stdlib_module_names) | THIRD_PARTY
    errors = []
    for node in ast.walk(tree):
        if (isinstance(node, ast.Attribute) and node.attr == "path"
                and isinstance(node.value, ast.Name) and node.value.id == "sys"):
            errors.append(f"{path}:{node.lineno}: gated Python must not use `sys.path`")
        elif isinstance(node, ast.ImportFrom) and node.module == "sys" and any(a.name == "path" for a in node.names):
            errors.append(f"{path}:{node.lineno}: gated Python must not use `sys.path`")
        if isinstance(node, ast.Import):
            modules = [alias.name for alias in node.names]
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                errors.append(f"{path}:{node.lineno}: relative import in gated Python")
            modules = [node.module or ""]
        else:
            continue
        for module in modules:
            if module.split(".", 1)[0] not in allowed:
                errors.append(f"{path}:{node.lineno}: `{module}` is neither standard library nor pinned "
                              "third-party; load gated siblings by explicit path")
    for label, target in loaded_paths(path, tree):
        if target is None:
            errors.append(f"{path}: {label} cannot be checked")
        elif target != path and target in present and not is_gated(target, files, directories):
            errors.append(f"{path}: loads {target}, which is not a gate file")
        elif target != path and target not in present and (root / target).exists():
            errors.append(f"{path}: loads {target}, which is untracked")
    return errors


def check(root: Path, gate_source: str) -> list[str]:
    paths = tracked(root)
    present = set(paths)
    files, directories = gates(gate_source)
    errors = stdlib_shadows(paths)
    for path in paths:
        if path.startswith(".github/workflows/") and path.endswith((".yml", ".yaml")):
            errors += workflow_errors(path, (root / path).read_text(encoding="utf-8"))
    for path in SHELL_GATES:
        errors += shell_errors(path, (root / path).read_text(encoding="utf-8")) if path in present \
            else [f"{path}: gated shell script is missing"]
    for path in sorted(present):
        if path.endswith(".py") and is_gated(path, files, directories) and not path.startswith(".github/"):
            errors += gated_python_errors(root, path, files, directories, present)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=str(REPO), help="the checkout checked as data (default: this one)")
    args = parser.parse_args()
    # The gate list is always this checkout's: in the trusted lane, the base's.
    errors = check(Path(args.root).resolve(), (REPO / GATE_SOURCE).read_text(encoding="utf-8"))
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
