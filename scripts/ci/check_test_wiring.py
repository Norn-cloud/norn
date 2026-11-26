#!/usr/bin/env python3
"""Zero-orphan test wiring & fail-closed CI base guard (#1864).

Mechanically prevents test suites and gate scripts from silently going dark:
1. Every tracked Python test file (`*test_*.py`, `*_test.py`) must be executed by
   a real Python simple command in `scripts/ci/core.sh` or `.github/workflows/*.yml`
   (direct `python3 <path>` or `python3 -m unittest discover -s <dir>` whose `-p`
   pattern matches the filename), unless listed in `EXEMPT_PY_TESTS`.
2. Every tracked JS/TS test package (`*.test.ts`, `*.test.tsx`, `*.test.mjs`,
   `*.e2e.ts`) and every standalone `apps/norn-cell-do/*.test.mjs` file must be
   executed by an actual `bun test` or `playwright test` simple command inside
   a subshell/block targeting that package, with standalone filenames passed as
   arguments to that same `bun test` command.
3. No shrink-only or CI base script may fall back to `HEAD` (`:-HEAD`), which
   compares a commit against itself and silently passes.
"""

from __future__ import annotations

import fnmatch
import re
import shlex
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# Tracked Python tests that require optional local-only data-science packages
# (pandas/numpy) not installed on standard CI runners. Adding an entry requires
# review; never add a standard-library or repo-runnable test here.
EXEMPT_PY_TESTS: dict[str, str] = {
    "sdks/python/regression/test_cell_export.py": "requires optional pandas",
    "sdks/python/regression/test_suite.py": "requires optional numpy",
}

BASE_FALLBACK_HEAD_RE = re.compile(r"\$\{[A-Za-z_][A-Za-z0-9_]*:-HEAD\}")

BASE_CHECKED_SCRIPT_GLOBS = (
    "scripts/ci/core.sh",
    "crates/norn-skeleton-guard/scripts/*.sh",
    "crates/norn-checker/scripts/*.sh",
)

ENV_ASSIGN_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=.*$")
PYTHON_BIN_RE = re.compile(r"^(?:[\w./${}-]+/)?python3?(?:\.\d+)?$")
YAML_RUN_PREFIX_RE = re.compile(r"^(?:-\s+)?run:\s+")


def _strip_shell_comment(line: str) -> str:
    """Return `line` up to an unquoted `#` comment delimiter."""
    in_single = False
    in_double = False
    escaped = False
    for idx, ch in enumerate(line):
        if escaped:
            escaped = False
            continue
        if ch == "\\" and not in_single:
            escaped = True
            continue
        if ch == "'" and not in_double:
            in_single = not in_single
        elif ch == '"' and not in_single:
            in_double = not in_double
        elif ch == "#" and not in_single and not in_double:
            if idx == 0 or line[idx - 1].isspace():
                return line[:idx]
    return line


def _split_shell_segments(line: str) -> list[str]:
    """Split a non-comment shell line on unquoted control operators only."""
    segments: list[str] = []
    buf: list[str] = []
    in_single = False
    in_double = False
    escaped = False
    i = 0
    n = len(line)
    while i < n:
        ch = line[i]
        if escaped:
            buf.append(ch)
            escaped = False
            i += 1
            continue
        if ch == "\\" and not in_single:
            buf.append(ch)
            escaped = True
            i += 1
            continue
        if ch == "'" and not in_double:
            in_single = not in_single
            buf.append(ch)
            i += 1
            continue
        if ch == '"' and not in_single:
            in_double = not in_double
            buf.append(ch)
            i += 1
            continue
        if not in_single and not in_double:
            if ch in (";", "(", ")"):
                seg = "".join(buf).strip()
                if seg:
                    segments.append(seg)
                buf.clear()
                i += 1
                continue
            if ch in ("&", "|"):
                # Keep fd redirections (`2>&1`, `<&0`) attached to the same command.
                if ch == "&" and buf and buf[-1] in (">", "<"):
                    buf.append(ch)
                    i += 1
                    continue
                seg = "".join(buf).strip()
                if seg:
                    segments.append(seg)
                buf.clear()
                i += 2 if (i + 1 < n and line[i + 1] == ch) else 1
                continue
        buf.append(ch)
        i += 1
    seg = "".join(buf).strip()
    if seg:
        segments.append(seg)
    return segments


def _split_simple_commands(line: str) -> list[list[str]]:
    """Split one non-comment shell line into simple commands (`argv` lists)."""
    cleaned = _strip_shell_comment(line).strip()
    if not cleaned:
        return []
    cleaned = YAML_RUN_PREFIX_RE.sub("", cleaned)
    commands: list[list[str]] = []
    for seg in _split_shell_segments(cleaned):
        try:
            argv = shlex.split(seg, posix=True)
        except ValueError:
            continue
        if argv:
            commands.append(argv)
    return commands


def _strip_env_prefix(argv: list[str]) -> list[str]:
    """Strip leading `VAR=val` assignments and `env [VAR=val ...]` wrappers."""
    i = 0
    while i < len(argv) and ENV_ASSIGN_RE.match(argv[i]):
        i += 1
    if i < len(argv) and argv[i] == "env":
        i += 1
        while i < len(argv) and (argv[i].startswith("-") or ENV_ASSIGN_RE.match(argv[i])):
            i += 1
    return argv[i:]


def _git_ls_files(root: Path, *patterns: str) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "--", *patterns],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def _ci_corpus(root: Path) -> str:
    parts: list[str] = []
    core = root / "scripts/ci/core.sh"
    if core.is_file():
        parts.append(core.read_text(encoding="utf-8"))
    wf_dir = root / ".github/workflows"
    if wf_dir.is_dir():
        for wf in sorted(wf_dir.glob("*.yml")):
            parts.append(wf.read_text(encoding="utf-8"))
    return "\n".join(parts)


def _python_invocations(ci_text: str) -> tuple[set[str], list[tuple[str, str]]]:
    """Extract `(direct_scripts, discover_targets)` from real Python simple commands."""
    direct: set[str] = set()
    discover_targets: list[tuple[str, str]] = []

    for raw in ci_text.splitlines():
        for raw_cmd in _split_simple_commands(raw):
            argv = _strip_env_prefix(raw_cmd)
            if not argv or not PYTHON_BIN_RE.match(argv[0]):
                continue
            args = argv[1:]
            # Parse options up to `-m`, `-c`, or the script filename.
            i = 0
            while i < len(args):
                tok = args[i]
                if tok == "-c":
                    break
                if tok == "-m":
                    if i + 2 < len(args) and args[i + 1] == "unittest" and args[i + 2] == "discover":
                        start_dir: str | None = None
                        pattern = "test*.py"
                        j = i + 3
                        while j < len(args):
                            opt = args[j]
                            if opt in ("-s", "--start-directory") and j + 1 < len(args):
                                start_dir = args[j + 1].rstrip("/")
                                j += 2
                                continue
                            if opt.startswith("--start-directory="):
                                start_dir = opt.split("=", 1)[1].rstrip("/")
                                j += 1
                                continue
                            if opt in ("-p", "--pattern") and j + 1 < len(args):
                                pattern = args[j + 1]
                                j += 2
                                continue
                            if opt.startswith("--pattern="):
                                pattern = opt.split("=", 1)[1]
                                j += 1
                                continue
                            j += 1
                        if start_dir:
                            discover_targets.append((start_dir, pattern))
                    break
                if tok in ("-X", "-W"):
                    i += 2
                    continue
                if tok.startswith("-"):
                    i += 1
                    continue
                direct.add(tok.lstrip("./"))
                break

    return direct, discover_targets


def check_py_test_wiring(py_tests: list[str], ci_text: str) -> list[str]:
    direct, discover_targets = _python_invocations(ci_text)
    errors: list[str] = []
    for rel in sorted(py_tests):
        if rel in EXEMPT_PY_TESTS:
            continue
        p = Path(rel)
        parent = p.parent.as_posix()
        matched_direct = rel in direct
        matched_discover = any(
            start_dir == parent and fnmatch.fnmatch(p.name, pattern)
            for start_dir, pattern in discover_targets
        )
        if not (matched_direct or matched_discover):
            errors.append(
                f"unwired Python test file: {rel} has no active python runner invocation in scripts/ci/core.sh or .github/workflows/*.yml"
            )
    return errors


def _extract_pkg_commands(ci_text: str) -> dict[str, list[list[str]]]:
    """Map each `cd <pkg>` target to the simple commands executed in that subshell/block."""
    blocks: dict[str, list[list[str]]] = {}
    lines = ci_text.splitlines()
    idx = 0

    def record_cmds(cmds: list[list[str]]) -> None:
        cwd: str | None = None
        for raw_cmd in cmds:
            argv = _strip_env_prefix(raw_cmd)
            if not argv:
                continue
            if argv[0] == "cd" and len(argv) >= 2:
                cwd = argv[1].rstrip("/")
                continue
            if cwd is not None:
                blocks.setdefault(cwd, []).append(argv)

    while idx < len(lines):
        cleaned = _strip_shell_comment(lines[idx]).strip()
        if not cleaned:
            idx += 1
            continue
        # Multi-line subshell: `(` or `fn() (` ... `)`
        if cleaned == "(" or cleaned.endswith("() ("):
            sub_cmds: list[list[str]] = []
            idx += 1
            while idx < len(lines):
                sub = _strip_shell_comment(lines[idx]).strip()
                idx += 1
                if sub == ")":
                    break
                if sub:
                    sub_cmds.extend(_split_simple_commands(sub))
            record_cmds(sub_cmds)
            continue
        record_cmds(_split_simple_commands(cleaned))
        idx += 1

    return blocks


def _is_js_test_command(argv: list[str]) -> bool:
    if len(argv) >= 2 and argv[0] == "bun" and argv[1] == "test":
        return True
    if len(argv) >= 3 and argv[0] == "bunx" and argv[1] == "playwright" and argv[2] == "test":
        return True
    if len(argv) >= 2 and argv[0] == "playwright" and argv[1] == "test":
        return True
    return False


def _bun_test_file_args(argv: list[str]) -> set[str]:
    """Return normalized file arguments passed to a single `bun test ...` command."""
    if not (len(argv) >= 2 and argv[0] == "bun" and argv[1] == "test"):
        return set()
    files: set[str] = set()
    i = 2
    while i < len(argv):
        tok = argv[i]
        if tok in ("--timeout", "--reporter", "--reporter-outfile", "--test-name-pattern", "--shard"):
            i += 2
            continue
        if tok.startswith("-"):
            i += 1
            continue
        files.add(Path(tok).name)
        i += 1
    return files


def check_js_test_wiring(js_tests: list[str], ci_text: str) -> list[str]:
    pkg_cmds = _extract_pkg_commands(ci_text)
    errors: list[str] = []
    packages: set[str] = set()
    standalone_mjs: list[str] = []
    for rel in sorted(js_tests):
        parts = Path(rel).parts
        if len(parts) >= 2:
            packages.add(f"{parts[0]}/{parts[1]}")
        if rel.startswith("apps/norn-cell-do/") and rel.endswith(".test.mjs"):
            standalone_mjs.append(rel)

    for pkg in sorted(packages):
        cmds = pkg_cmds.get(pkg, [])
        if not any(_is_js_test_command(argv) for argv in cmds):
            errors.append(
                f"unwired JS/TS test package: {pkg} has test files but no active `bun test` or `playwright test` command in scripts/ci/core.sh or .github/workflows/*.yml"
            )

    cell_do_tested: set[str] = set()
    for argv in pkg_cmds.get("apps/norn-cell-do", []):
        cell_do_tested.update(_bun_test_file_args(argv))
    for rel in standalone_mjs:
        if Path(rel).name not in cell_do_tested:
            errors.append(
                f"unwired standalone test file: {rel} is not passed to `bun test` in apps/norn-cell-do"
            )
    return errors


def check_no_head_base_fallback(scripts: dict[str, str]) -> list[str]:
    errors: list[str] = []
    for rel, text in sorted(scripts.items()):
        for lineno, line in enumerate(text.splitlines(), start=1):
            cleaned = _strip_shell_comment(line)
            if BASE_FALLBACK_HEAD_RE.search(cleaned):
                errors.append(
                    f"{rel}:{lineno}: fail-open :-HEAD fallback compares HEAD against itself; use origin/main locally and require NORN_CI_BASE in CI"
                )
    return errors


def check_repo(root: Path = REPO) -> list[str]:
    ci_text = _ci_corpus(root)
    py_tests = _git_ls_files(root, "*test_*.py", "*_test.py")
    js_tests = _git_ls_files(root, "*.test.ts", "*.test.tsx", "*.test.mjs", "*.e2e.ts")
    scripts: dict[str, str] = {}
    for pattern in BASE_CHECKED_SCRIPT_GLOBS:
        for path in sorted(root.glob(pattern)):
            if path.is_file():
                scripts[path.relative_to(root).as_posix()] = path.read_text(encoding="utf-8")

    errors: list[str] = []
    errors.extend(check_py_test_wiring(py_tests, ci_text))
    errors.extend(check_js_test_wiring(js_tests, ci_text))
    errors.extend(check_no_head_base_fallback(scripts))
    return errors


def main() -> int:
    errors = check_repo(REPO)
    if errors:
        for error in errors:
            print(f"check_test_wiring: {error}", file=sys.stderr)
        return 1
    print("check_test_wiring: all tracked Python/JS test suites are wired and no :-HEAD base fallbacks exist")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
