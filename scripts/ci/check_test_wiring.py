#!/usr/bin/env python3
"""Zero-orphan test wiring & fail-closed CI base guard (#1864).

Mechanically prevents test suites and gate scripts from silently going dark:
1. Every tracked Python test file (`*test_*.py`, `*_test.py`) must be executed by
   a real Python runner invocation in `scripts/ci/core.sh` or `.github/workflows/*.yml`
   (direct `python3 <path>` or `python3 -m unittest discover -s <dir>` whose `-p`
   pattern matches the filename), unless listed in `EXEMPT_PY_TESTS`.
2. Every tracked JS/TS test package (`*.test.ts`, `*.test.tsx`, `*.test.mjs`,
   `*.e2e.ts`) and every standalone `apps/norn-cell-do/*.test.mjs` file must be
   executed by an actual `bun test` or `playwright test` command in a non-comment
   subshell targeting that package.
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

PYTHON_CMD_RE = re.compile(r"(?:^|[\s;&|(\"'`])(?:[\w./${}-]*/)?python3?(?:\.\d+)?(?:\s|$)")
CD_PKG_RE = re.compile(r"(?:^|[\s;&|(])cd\s+([A-Za-z0-9_./-]+)(?:$|[\s;&|)])")
TEST_RUNNER_RE = re.compile(r"(?:^|[\s;&|(])(?:bun\s+test\b|bunx\s+playwright\s+test\b|playwright\s+test\b)")


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


def _active_lines(text: str) -> list[str]:
    result: list[str] = []
    for raw in text.splitlines():
        cleaned = _strip_shell_comment(raw).strip()
        if cleaned:
            result.append(cleaned)
    return result


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


def _extract_unittest_discover_targets(lines: list[str]) -> list[tuple[str, str]]:
    """Return `(start_dir, pattern)` pairs for non-comment `unittest discover` invocations."""
    targets: list[tuple[str, str]] = []
    for line in lines:
        if not PYTHON_CMD_RE.search(line) or "unittest" not in line or "discover" not in line:
            continue
        try:
            tokens = shlex.split(line)
        except ValueError:
            tokens = line.split()
        # Find `-m unittest discover` subsequence.
        for i in range(len(tokens) - 2):
            if tokens[i] == "-m" and tokens[i + 1] == "unittest" and tokens[i + 2] == "discover":
                start_dir: str | None = None
                pattern = "test*.py"
                j = i + 3
                while j < len(tokens):
                    tok = tokens[j]
                    if tok in ("&&", "||", ";", "|"):
                        break
                    if tok in ("-s", "--start-directory") and j + 1 < len(tokens):
                        start_dir = tokens[j + 1].rstrip("/")
                        j += 2
                        continue
                    if tok.startswith("--start-directory="):
                        start_dir = tok.split("=", 1)[1].rstrip("/")
                        j += 1
                        continue
                    if tok in ("-p", "--pattern") and j + 1 < len(tokens):
                        pattern = tokens[j + 1]
                        j += 2
                        continue
                    if tok.startswith("--pattern="):
                        pattern = tok.split("=", 1)[1]
                        j += 1
                        continue
                    j += 1
                if start_dir:
                    targets.append((start_dir, pattern))
    return targets


def check_py_test_wiring(py_tests: list[str], ci_text: str) -> list[str]:
    lines = _active_lines(ci_text)
    python_lines = [line for line in lines if PYTHON_CMD_RE.search(line)]
    discover_targets = _extract_unittest_discover_targets(python_lines)

    errors: list[str] = []
    for rel in sorted(py_tests):
        if rel in EXEMPT_PY_TESTS:
            continue
        p = Path(rel)
        parent = p.parent.as_posix()
        direct_re = re.compile(rf'(?:^|[\s"\']){re.escape(rel)}(?:$|[\s"\';&|)])')
        direct = any(direct_re.search(line) for line in python_lines)
        discovered = any(
            start_dir == parent and fnmatch.fnmatch(p.name, pattern)
            for start_dir, pattern in discover_targets
        )
        if not (direct or discovered):
            errors.append(
                f"unwired Python test file: {rel} has no active python runner invocation in scripts/ci/core.sh or .github/workflows/*.yml"
            )
    return errors


def _extract_pkg_blocks(ci_text: str) -> dict[str, list[str]]:
    """Map each `cd <pkg>` target to the active command lines inside its subshell/block."""
    blocks: dict[str, list[str]] = {}
    lines = ci_text.splitlines()
    idx = 0
    while idx < len(lines):
        cleaned = _strip_shell_comment(lines[idx]).strip()
        if not cleaned:
            idx += 1
            continue
        # Single-line subshell: `(cd pkg && ...)`
        if cleaned.startswith("(") and cleaned.endswith(")"):
            inner = cleaned[1:-1]
            m = CD_PKG_RE.search(inner)
            if m:
                pkg = m.group(1).rstrip("/")
                blocks.setdefault(pkg, []).append(inner)
            idx += 1
            continue
        # Multi-line subshell: `(` or `fn() (` ... `)`
        if cleaned == "(" or cleaned.endswith("() ("):
            body: list[str] = []
            idx += 1
            while idx < len(lines):
                sub = _strip_shell_comment(lines[idx]).strip()
                idx += 1
                if sub == ")":
                    break
                if sub:
                    body.append(sub)
            for b_line in body:
                m = CD_PKG_RE.search(b_line)
                if m:
                    pkg = m.group(1).rstrip("/")
                    blocks.setdefault(pkg, []).extend(body)
                    break
            continue
        # Workflow working-directory / inline cd on a single line.
        m = CD_PKG_RE.search(cleaned)
        if m:
            pkg = m.group(1).rstrip("/")
            blocks.setdefault(pkg, []).append(cleaned)
        idx += 1
    return blocks


def check_js_test_wiring(js_tests: list[str], ci_text: str) -> list[str]:
    blocks = _extract_pkg_blocks(ci_text)
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
        pkg_lines = blocks.get(pkg, [])
        if not any(TEST_RUNNER_RE.search(line) for line in pkg_lines):
            errors.append(
                f"unwired JS/TS test package: {pkg} has test files but no active `bun test` or `playwright test` invocation in scripts/ci/core.sh or .github/workflows/*.yml"
            )

    cell_do_lines = [line for line in blocks.get("apps/norn-cell-do", []) if TEST_RUNNER_RE.search(line)]
    for rel in standalone_mjs:
        name = Path(rel).name
        file_re = re.compile(rf'(?:^|[\s"\'])(?:\./)?{re.escape(name)}(?:$|[\s"\';&|)])')
        if not any(file_re.search(line) for line in cell_do_lines):
            errors.append(
                f"unwired standalone test file: {rel} is not executed by `bun test` in apps/norn-cell-do"
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
