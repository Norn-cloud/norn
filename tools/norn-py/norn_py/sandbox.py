"""Two-layer deterministic elaboration sandbox for `tools/norn-py` (Astra Ruling, 2026-11-23).

Layer 1 (AST pre-flight, zero execution):
- Enforces strict import allowlist (`__future__`, `typing`, `enum`, `dataclasses`, `decimal`, `norn_py`).
- Bans `eval`, `exec`, `compile`, `__import__`, `open`, `input`, `breakpoint`, `globals`, `locals`,
  `vars`, `getattr`/`setattr`/`delattr` bypasses, `global`/`nonlocal`, `async`/`await`, `yield`,
  `float` literals, and `try`/`except` blocks that could swallow tracing errors.
- Captures exact AST `(file, line, col)` source spans for every top-level definition.

Layer 2 (Isolated subprocess runtime):
- Runs in a fresh `python3 -I -B` process with `PYTHONHASHSEED=0`, `TZ=UTC`, `LC_ALL=C`.
- Installs `sys.addaudithook` failing closed on IO/network/subprocess/dynamic-code events.
- Enforces cooperative wall-time and source-size budgets.
"""

from __future__ import annotations

import ast
import json
import os
import subprocess
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from .canonical import EnvelopeOptions, JsonValue, SourceSpan, canonical_json
from .expr import NornTracingError
from .schema import collect_artifacts

ALLOWED_IMPORT_ROOTS: frozenset[str] = frozenset(
    {
        "__future__",
        "dataclasses",
        "decimal",
        "enum",
        "norn_py",
        "typing",
    }
)

FORBIDDEN_CALL_NAMES: frozenset[str] = frozenset(
    {
        "__import__",
        "breakpoint",
        "compile",
        "delattr",
        "eval",
        "exec",
        "float",
        "getattr",
        "globals",
        "input",
        "locals",
        "open",
        "setattr",
        "vars",
    }
)

MAX_SOURCE_BYTES = 1_048_576
MAX_AST_NODES = 100_000
DEFAULT_TIMEOUT_SECS = 15.0


@dataclass(frozen=True, slots=True)
class PreflightResult:
    """Validated AST and captured symbol -> source-span table."""

    tree: ast.Module
    spans: dict[str, SourceSpan]


def preflight_source(source: str, filename: str) -> PreflightResult:
    """Layer 1: statically inspect Python AST before any authored code runs."""
    if len(source.encode("utf-8")) > MAX_SOURCE_BYTES:
        raise NornTracingError(
            "NORN-PY-UNBOUNDED",
            f"source exceeds {MAX_SOURCE_BYTES} byte limit",
            SourceSpan(filename, 1, 0),
        )

    try:
        tree = ast.parse(source, filename=filename)
    except SyntaxError as exc:
        raise NornTracingError(
            "NORN-PY-SYNTAX",
            str(exc.msg),
            SourceSpan(filename, exc.lineno or 1, exc.offset or 0),
        ) from exc

    spans: dict[str, SourceSpan] = {}
    node_count = 0

    for node in ast.walk(tree):
        node_count += 1
        if node_count > MAX_AST_NODES:
            raise NornTracingError(
                "NORN-PY-UNBOUNDED",
                f"source exceeds {MAX_AST_NODES} AST node limit",
                SourceSpan(filename, getattr(node, "lineno", 1), getattr(node, "col_offset", 0)),
            )

        span = SourceSpan(filename, getattr(node, "lineno", 1), getattr(node, "col_offset", 0))

        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".", 1)[0]
                if root not in ALLOWED_IMPORT_ROOTS:
                    raise NornTracingError(
                        "NORN-PY-FORBIDDEN-IMPORT",
                        f"import {alias.name!r} is forbidden in Norn authoring modules "
                        f"(allowed: {sorted(ALLOWED_IMPORT_ROOTS)})",
                        span,
                    )
        elif isinstance(node, ast.ImportFrom):
            if node.level > 0 or node.module is None:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-IMPORT",
                    "relative imports are not admitted in the experimental Python spike",
                    span,
                )
            root = node.module.split(".", 1)[0]
            if root not in ALLOWED_IMPORT_ROOTS:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-IMPORT",
                    f"from {node.module!r} import ... is forbidden in Norn authoring modules "
                    f"(allowed: {sorted(ALLOWED_IMPORT_ROOTS)})",
                    span,
                )
        elif isinstance(node, ast.Constant) and isinstance(node.value, float):
            raise NornTracingError(
                "NORN-PY-FLOAT-LITERAL",
                f"float literal {node.value!r} is refused (VAL-EXACT-1); use Decimal({str(node.value)!r})",
                span,
            )
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            if node.func.id in FORBIDDEN_CALL_NAMES:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-CALL",
                    f"call to {node.func.id}() is forbidden in Norn authoring modules",
                    span,
                )
        elif isinstance(node, (ast.Global, ast.Nonlocal)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-SYNTAX",
                "`global`/`nonlocal` mutation is forbidden in Norn authoring modules",
                span,
            )
        elif isinstance(node, (ast.AsyncFunctionDef, ast.AsyncFor, ast.AsyncWith, ast.Await)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-SYNTAX",
                "`async`/`await` is forbidden in declarative Norn authoring modules",
                span,
            )
        elif isinstance(node, (ast.Yield, ast.YieldFrom)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-SYNTAX",
                "`yield` generators are forbidden in declarative Norn authoring modules",
                span,
            )
        elif isinstance(node, (ast.Try, ast.TryStar)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-SYNTAX",
                "`try`/`except` is forbidden in declarative Norn authoring modules "
                "(prevents swallowing symbolic-tracing refusals)",
                span,
            )

    for stmt in tree.body:
        span = SourceSpan(filename, stmt.lineno, stmt.col_offset)
        if isinstance(stmt, (ast.ClassDef, ast.FunctionDef)):
            spans[stmt.name] = span
        elif isinstance(stmt, ast.Assign):
            for target in stmt.targets:
                if isinstance(target, ast.Name):
                    spans[target.id] = span
        elif isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name):
            spans[stmt.target.id] = span

    return PreflightResult(tree=tree, spans=spans)


# ------------------------------------------------------------------ Layer 2 runtime hook


_FORBIDDEN_AUDIT_PREFIXES: tuple[str, ...] = (
    "open",
    "os.system",
    "os.popen",
    "os.posix_spawn",
    "os.fork",
    "os.exec",
    "subprocess.",
    "socket.",
    "http.",
    "urllib.",
    "ctypes.",
    "shutil.",
    "pathlib.",
    "tempfile.",
)


def _install_audit_hook() -> None:
    def _hook(event: str, _args: tuple[object, ...]) -> None:
        for prefix in _FORBIDDEN_AUDIT_PREFIXES:
            if event == prefix or event.startswith(prefix):
                raise PermissionError(f"[NORN-PY-SANDBOX] audit event {event!r} blocked during elaboration")

    sys.addaudithook(_hook)


def _elaborate_in_current_process(
    source: str,
    *,
    filename: str,
    options: EnvelopeOptions,
    enable_audit_hook: bool = False,
) -> dict[str, object]:
    preflight = preflight_source(source, filename)
    code = compile(preflight.tree, filename, "exec")
    ns: dict[str, object] = {"__name__": "__norn_authoring__", "__file__": filename}
    if enable_audit_hook:
        # Pre-warm stdlib codecs and allowed pure modules before locking down IO.
        _ = "".encode("utf-16-be")
        import dataclasses  # noqa: F401
        import decimal  # noqa: F401
        import enum  # noqa: F401
        import typing  # noqa: F401

        _install_audit_hook()
    exec(code, ns, ns)  # noqa: S102 - gated by Layer 1 AST preflight + Layer 2 audit hook
    artifacts = collect_artifacts(ns, options)
    source_map = {
        name: {"column": sp.column, "file": sp.file, "line": sp.line}
        for name, sp in sorted(preflight.spans.items())
    }
    return {
        "artifacts": artifacts,
        "sourceMap": source_map,
    }


@dataclass(frozen=True, slots=True)
class ElaborationResult:
    """Canonical elaboration result plus AST source map."""

    artifacts: list[JsonValue]
    canonical_text: str
    source_map: dict[str, dict[str, object]]


def elaborate_file(
    path: str | Path,
    *,
    scope: Mapping[str, str],
    source_path: str | None = None,
    registry_package_path: str | None = None,
    timeout_secs: float = DEFAULT_TIMEOUT_SECS,
) -> ElaborationResult:
    """Elaborate an authored `.py` file inside an isolated deterministic subprocess."""
    file_path = Path(path).resolve()
    source = file_path.read_text(encoding="utf-8")
    rel_source_path = source_path if source_path is not None else str(path)

    # Run Layer 1 preflight locally first for instant crisp diagnostics.
    preflight_source(source, rel_source_path)

    package_root = Path(__file__).resolve().parent.parent
    payload = json.dumps(
        {
            "filename": rel_source_path,
            "registryPackagePath": registry_package_path,
            "scope": dict(scope),
            "source": source,
        }
    )

    env = {
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", ""),
        "PYTHONHASHSEED": "0",
        "PYTHONPATH": str(package_root),
        "TZ": "UTC",
    }

    runner = (
        "import json, sys\n"
        f"sys.path.insert(0, {str(package_root)!r})\n"
        "from norn_py.canonical import EnvelopeOptions, canonical_json\n"
        "from norn_py.sandbox import _elaborate_in_current_process\n"
        "req = json.loads(sys.stdin.read())\n"
        "opts = EnvelopeOptions(\n"
        "    scope=req['scope'],\n"
        "    source_path=req['filename'],\n"
        "    registry_package_path=req['registryPackagePath'],\n"
        ")\n"
        "res = _elaborate_in_current_process(\n"
        "    req['source'],\n"
        "    filename=req['filename'],\n"
        "    options=opts,\n"
        "    enable_audit_hook=True,\n"
        ")\n"
        "sys.stdout.write(canonical_json(res))\n"
    )

    proc = subprocess.run(
        [sys.executable, "-I", "-B", "-c", runner],
        input=payload,
        capture_output=True,
        text=True,
        env=env,
        timeout=timeout_secs,
        check=False,
    )
    if proc.returncode != 0:
        detail = proc.stderr.strip() or proc.stdout.strip() or f"exit code {proc.returncode}"
        raise NornTracingError("NORN-PY-ELABORATION", detail, SourceSpan(rel_source_path, 1, 0))

    parsed = json.loads(proc.stdout)
    artifacts: list[JsonValue] = parsed["artifacts"]
    source_map: dict[str, dict[str, object]] = parsed["sourceMap"]
    return ElaborationResult(
        artifacts=artifacts,
        canonical_text=canonical_json(artifacts),
        source_map=source_map,
    )


def verify_deterministic_elaboration(
    path: str | Path,
    *,
    scope: Mapping[str, str],
    source_path: str | None = None,
    registry_package_path: str | None = None,
) -> ElaborationResult:
    """Astra determinism gate: two fresh isolated runs MUST emit byte-identical canonical JSON."""
    first = elaborate_file(
        path,
        scope=scope,
        source_path=source_path,
        registry_package_path=registry_package_path,
    )
    second = elaborate_file(
        path,
        scope=scope,
        source_path=source_path,
        registry_package_path=registry_package_path,
    )
    if first.canonical_text != second.canonical_text:
        raise NornTracingError(
            "NORN-PY-NONDETERMINISTIC",
            "two fresh isolated elaborations emitted different canonical JSON bytes",
            SourceSpan(source_path or str(path), 1, 0),
        )
    return first
