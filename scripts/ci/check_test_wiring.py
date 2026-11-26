#!/usr/bin/env python3
"""Zero-orphan test wiring & fail-closed CI base guard (#1864).

Mechanically prevents test suites and gate scripts from silently going dark:
1. Every tracked Python test file (`*test_*.py`, `*_test.py`) must be invoked by
   `scripts/ci/core.sh` or a workflow under `.github/workflows/`, unless listed in
   `EXEMPT_PY_TESTS` (tests that require non-CI optional packages).
2. Every tracked JS/TS test package (`*.test.ts`, `*.test.tsx`, `*.test.mjs`,
   `*.e2e.ts`) and every standalone `apps/norn-cell-do/*.test.mjs` file must be
   wired into `scripts/ci/core.sh` or `.github/workflows/`.
3. No shrink-only or CI base script may fall back to `HEAD` (`:-HEAD`), which
   compares a commit against itself and silently passes.
"""

from __future__ import annotations

import re
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


def check_py_test_wiring(py_tests: list[str], ci_text: str) -> list[str]:
    errors: list[str] = []
    for rel in sorted(py_tests):
        if rel in EXEMPT_PY_TESTS:
            continue
        p = Path(rel)
        # Accept explicit file invocation, or unittest discovery targeting the
        # test's parent directory (e.g. `-s tools/norn-py/tests`).
        parent = p.parent.as_posix()
        referenced = (
            rel in ci_text
            or f"-s {parent}" in ci_text
            or f'-s "{parent}"' in ci_text
        )
        if not referenced:
            errors.append(
                f"unwired Python test file: {rel} is not referenced in scripts/ci/core.sh or .github/workflows/*.yml"
            )
    return errors


def check_js_test_wiring(js_tests: list[str], ci_text: str) -> list[str]:
    errors: list[str] = []
    packages: set[str] = set()
    standalone_mjs: list[str] = []
    for rel in sorted(js_tests):
        parts = Path(rel).parts
        if len(parts) >= 2:
            pkg = f"{parts[0]}/{parts[1]}"
            packages.add(pkg)
        if rel.startswith("apps/norn-cell-do/") and rel.endswith(".test.mjs"):
            standalone_mjs.append(rel)

    for pkg in sorted(packages):
        # Package must be entered (`cd <pkg>`) or invoked by path in CI.
        if f"cd {pkg}" not in ci_text and f"{pkg}/" not in ci_text:
            errors.append(
                f"unwired JS/TS test package: {pkg} has test files but is not run in scripts/ci/core.sh or .github/workflows/*.yml"
            )

    for rel in standalone_mjs:
        name = Path(rel).name
        if rel not in ci_text and f"./{name}" not in ci_text:
            errors.append(
                f"unwired standalone test file: {rel} is not listed in apps/norn-cell-do bun test commands"
            )
    return errors


def check_no_head_base_fallback(scripts: dict[str, str]) -> list[str]:
    errors: list[str] = []
    for rel, text in sorted(scripts.items()):
        for lineno, line in enumerate(text.splitlines(), start=1):
            stripped = line.lstrip()
            if stripped.startswith("#"):
                continue
            if BASE_FALLBACK_HEAD_RE.search(line):
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
