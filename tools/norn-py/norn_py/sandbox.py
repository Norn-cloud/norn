"""Two-layer deterministic elaboration sandbox for `tools/norn-py`.

Layer 1 (AST pre-flight, zero execution):
- Enforces strict module + symbol import allowlist (rejects relative imports, `*` imports,
  and non-allowlisted symbols such as `from typing import sys`).
- Bans ALL dunder attribute/name access (`__subclasses__`, `__class__`, `__globals__`,
  `__builtins__`, etc.), sensitive frame/code attributes, custom metaclasses, and dynamic
  builtins (`__import__`, `open`, `exec`, `eval`, `compile`, `getattr`, `setattr`, `delattr`,
  `globals`, `locals`, `vars`, `dir`, `type`, `breakpoint`, `input`, etc.).
- Bans imperative control flow (`if`, `if`-expr, `for`, `while`, comprehensions, `try`/`except`,
  `with`, `async`/`await`, `yield`, `global`/`nonlocal`, `match`, `raise`, `delete`, `float`).
- Validates every `FunctionDef` and `Lambda` callback AST BEFORE tracing.
- Captures exact AST `(file, line, col)` source spans for every top-level definition.

Layer 2 (Restricted isolated subprocess runtime):
- Runs in a fresh `python3 -s -S -B -P` subprocess with minimal clean environment
  (`LC_ALL=C`, `PYTHONHASHSEED=0`, `PYTHONPATH=<pkg>`, `TZ=UTC`; intentionally avoids `-I`
  because CPython ignores `PYTHONHASHSEED` under `-I`).
- Applies POSIX `resource` CPU, virtual-memory, and zero-file-write limits when available.
- Executes authored code against a strictly closed minimal `__builtins__` dict with closed
  module-proxy `__import__` and base-checked `__build_class__`.
- Installs `sys.addaudithook` failing closed on IO/network/process/dynamic-code events.

Trust boundary note (honest evidence label: `structural` / `bounded`):
This sandbox provides defense-in-depth for developer authoring workflows, NOT multi-tenant
OS isolation. Untrusted multi-tenant input MUST additionally run inside an OS-level
container / seccomp / microVM boundary.
"""

from __future__ import annotations

import ast
import builtins
import decimal
import enum
import json
import math
import os
import subprocess
import sys
import threading
import time
import types
import typing
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import IO, Any, Protocol

from .canonical import (
    EnvelopeOptions,
    JsonValue,
    SourceSpan,
    canonical_json,
    canonical_norn1_bytes,
    norn1_sha256_bytes,
)
from .expr import (
    _FORBIDDEN_INTERNAL_ATTRS,
    NornTracingError,
    read_regular_file_bounded,
    register_source,
    restore_source_entry,
    snapshot_source_entry,
    validate_callback_node,
)
from .schema import Concept, collect_artifacts, is_annotation_parse_active

# Closed per-module symbol allowlist: prevents transitive escapes like `typing.sys` or `enum.sys`.
# Note: `dataclasses` is intentionally excluded because `@dataclass` calls `exec()` to
# synthesize methods, which Layer 2's audit hook blocks; authored modules use `Concept`.
ALLOWED_IMPORT_SYMBOLS: Mapping[str, frozenset[str]] = {
    "__future__": frozenset({"annotations"}),
    "decimal": frozenset({"Decimal"}),
    "enum": frozenset({"Enum", "StrEnum", "auto"}),
    "typing": frozenset(
        {
            "Annotated",
            "ClassVar",
            "Final",
            "Literal",
            "Optional",
            "Protocol",
            "Union",
        }
    ),
}

ALLOWED_IMPORT_ROOTS: frozenset[str] = frozenset({*ALLOWED_IMPORT_SYMBOLS.keys(), "norn_py"})

# Sandbox-internal / host runner functions are never exposed to authored modules.
_DISALLOWED_NORN_PY_AUTHORING_EXPORTS: frozenset[str] = frozenset(
    {
        "collect_artifacts",
        "elaborate_file",
        "preflight_source",
        "register_source",
        "resolve_annotation",
        "sandbox_subprocess_cmd",
        "sandbox_subprocess_env",
        "trace_bool_callback",
        "validate_callback_ast",
        "verify_deterministic_elaboration",
    }
)

FORBIDDEN_NAMES: frozenset[str] = frozenset(
    {
        "__build_class__",
        "__builtins__",
        "__import__",
        "__loader__",
        "__spec__",
        "breakpoint",
        "classmethod",
        "compile",
        "delattr",
        "dir",
        "eval",
        "exec",
        "exit",
        "float",
        "getattr",
        "globals",
        "help",
        "input",
        "locals",
        "memoryview",
        "object",
        "open",
        "property",
        "quit",
        "setattr",
        "staticmethod",
        "super",
        "type",
        "vars",
    }
)

_FORBIDDEN_ATTRS: frozenset[str] = _FORBIDDEN_INTERNAL_ATTRS | frozenset({"format", "format_map"})

MAX_SOURCE_BYTES = 1_048_576
MAX_AST_NODES = 100_000
MAX_SUBPROCESS_OUTPUT_BYTES = 16 * 1024 * 1024
MAX_SUBPROCESS_STDERR_BYTES = 256 * 1024
DEFAULT_TIMEOUT_SECS = 15.0
DEFAULT_MEMORY_LIMIT_BYTES = 1024 * 1024 * 1024  # 1 GiB virtual address space cap


@dataclass(frozen=True, slots=True)
class PreflightResult:
    """Validated AST and captured symbol -> source-span table."""

    tree: ast.Module
    spans: dict[str, SourceSpan]


def _allowed_norn_py_symbols() -> frozenset[str]:
    import norn_py

    return frozenset(name for name in norn_py.__all__ if name not in _DISALLOWED_NORN_PY_AUTHORING_EXPORTS)


def _check_import_module_and_names(
    module_name: str,
    imported_names: list[str] | None,
    span: SourceSpan,
) -> None:
    if module_name not in ALLOWED_IMPORT_ROOTS:
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-IMPORT",
            f"import {module_name!r} is forbidden in Norn authoring modules "
            f"(allowed: {sorted(ALLOWED_IMPORT_ROOTS)})",
            span,
        )
    allowed_syms = (
        _allowed_norn_py_symbols() if module_name == "norn_py" else ALLOWED_IMPORT_SYMBOLS[module_name]
    )
    if imported_names is not None:
        for sym_name in imported_names:
            if sym_name == "*":
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-IMPORT",
                    f"wildcard `from {module_name} import *` is forbidden in Norn authoring modules",
                    span,
                )
            if sym_name not in allowed_syms:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-IMPORT",
                    f"symbol {sym_name!r} cannot be imported from {module_name!r} in Norn authoring modules",
                    span,
                )


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
                _check_import_module_and_names(alias.name, None, span)
                if alias.asname is not None and (alias.asname.startswith("__") or alias.asname in FORBIDDEN_NAMES):
                    raise NornTracingError(
                        "NORN-PY-FORBIDDEN-NAME",
                        f"import alias {alias.asname!r} is forbidden in Norn authoring modules",
                        span,
                    )
        elif isinstance(node, ast.ImportFrom):
            if node.level > 0 or node.module is None:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-IMPORT",
                    "relative imports are not admitted in Norn authoring modules",
                    span,
                )
            _check_import_module_and_names(node.module, [a.name for a in node.names], span)
            for alias in node.names:
                if alias.asname is not None and (alias.asname.startswith("__") or alias.asname in FORBIDDEN_NAMES):
                    raise NornTracingError(
                        "NORN-PY-FORBIDDEN-NAME",
                        f"import alias {alias.asname!r} is forbidden in Norn authoring modules",
                        span,
                    )
        elif isinstance(node, ast.arg):
            if node.arg.startswith("__") or node.arg in FORBIDDEN_NAMES:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-NAME",
                    f"parameter name {node.arg!r} is forbidden in Norn authoring modules",
                    span,
                )
        elif isinstance(node, ast.Constant) and isinstance(node.value, float):
            raise NornTracingError(
                "NORN-PY-FLOAT-LITERAL",
                f"float literal {node.value!r} is refused (VAL-EXACT-1); use Decimal({str(node.value)!r})",
                span,
            )
        elif isinstance(node, ast.Attribute):
            if node.attr.startswith("__") or node.attr in _FORBIDDEN_ATTRS:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-ATTR",
                    f"attribute access .{node.attr} is forbidden in Norn authoring modules",
                    span,
                )
        elif isinstance(node, ast.Name):
            if node.id.startswith("__") or node.id in FORBIDDEN_NAMES:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-CALL" if node.id in FORBIDDEN_NAMES else "NORN-PY-FORBIDDEN-NAME",
                    f"reference to {node.id!r} is forbidden in Norn authoring modules",
                    span,
                )
        elif isinstance(node, ast.ClassDef):
            if node.name.startswith("__") or node.name in FORBIDDEN_NAMES:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-SYNTAX",
                    f"class name {node.name!r} is forbidden in Norn authoring modules",
                    span,
                )
            for kw in node.keywords:
                if kw.arg == "metaclass":
                    raise NornTracingError(
                        "NORN-PY-FORBIDDEN-SYNTAX",
                        "custom `metaclass=` is forbidden in Norn authoring modules",
                        span,
                    )
            for item in node.body:
                if isinstance(item, ast.FunctionDef) and item.name.startswith("__"):
                    raise NornTracingError(
                        "NORN-PY-FORBIDDEN-SYNTAX",
                        f"dunder method {item.name!r} is forbidden in Norn authoring classes",
                        SourceSpan(filename, item.lineno, item.col_offset),
                    )
        elif isinstance(node, (ast.FunctionDef, ast.Lambda)):
            if isinstance(node, ast.FunctionDef) and (node.name.startswith("__") or node.name in FORBIDDEN_NAMES):
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-SYNTAX",
                    f"function name {node.name!r} is forbidden in Norn authoring modules",
                    span,
                )
            validate_callback_node(node, filename)
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
        elif isinstance(node, (ast.With, ast.For, ast.While, ast.Raise, ast.Delete, ast.Assert, ast.Match)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-SYNTAX",
                f"`{type(node).__name__}` is forbidden in declarative Norn authoring modules",
                span,
            )
        elif isinstance(node, (ast.If, ast.IfExp)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "Python `if`/`else` is forbidden in declarative Norn authoring modules; "
                "use `when(cond, yes, no)` / `if_else(cond, yes, no)` inside expressions",
                span,
            )
        elif isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "comprehensions and generator expressions are forbidden in declarative Norn authoring modules",
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


# ------------------------------------------------------------------ Layer 2 restricted runtime


def _build_closed_module_proxies() -> dict[str, types.SimpleNamespace]:
    """Build closed read-only-style module proxies exposing ONLY allowlisted symbols."""
    import __future__ as future_mod

    import norn_py

    source_modules: dict[str, object] = {
        "__future__": future_mod,
        "decimal": decimal,
        "enum": enum,
        "typing": typing,
    }
    proxies: dict[str, types.SimpleNamespace] = {}
    for mod_name, sym_names in ALLOWED_IMPORT_SYMBOLS.items():
        mod = source_modules[mod_name]
        proxies[mod_name] = types.SimpleNamespace(
            **{sym: getattr(mod, sym) for sym in sorted(sym_names)}
        )
    proxies["norn_py"] = types.SimpleNamespace(
        **{sym: getattr(norn_py, sym) for sym in sorted(_allowed_norn_py_symbols())}
    )
    return proxies


def _make_safe_import(proxies: Mapping[str, types.SimpleNamespace]) -> Any:
    def _safe_import(
        name: str,
        _globals: Mapping[str, object] | None = None,
        _locals: Mapping[str, object] | None = None,
        fromlist: Sequence[str] = (),
        level: int = 0,
    ) -> types.SimpleNamespace:
        if level != 0:
            raise NornTracingError("NORN-PY-FORBIDDEN-IMPORT", "relative imports are forbidden in sandbox")
        proxy = proxies.get(name)
        if proxy is None:
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-IMPORT",
                f"import {name!r} blocked by sandbox import hook (allowed: {sorted(proxies.keys())})",
            )
        for item in fromlist or ():
            if not hasattr(proxy, item):
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-IMPORT",
                    f"symbol {item!r} from {name!r} blocked by sandbox import hook",
                )
        return proxy

    return _safe_import


def _safe_build_class(func: Any, class_name: str, /, *bases: type[Any], **kwds: object) -> type[Any]:
    if "metaclass" in kwds:
        raise NornTracingError("NORN-PY-FORBIDDEN-SYNTAX", "custom metaclass is forbidden in sandbox")
    if class_name.startswith("__"):
        raise NornTracingError("NORN-PY-FORBIDDEN-SYNTAX", f"dunder class name {class_name!r} is forbidden")
    for base in bases:
        base_obj: object = base
        is_allowed_base = (
            base_obj is Protocol
            or (isinstance(base, type) and issubclass(base, (Concept, StrEnum)))
        )
        if not is_allowed_base:
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-SYNTAX",
                f"authored class {class_name!r} may only inherit from Concept, StrEnum, or Protocol (got {base!r})",
            )
    return builtins.__build_class__(func, class_name, *bases, **kwds)  # type: ignore[no-any-return]


def _make_restricted_builtins() -> dict[str, object]:
    proxies = _build_closed_module_proxies()
    return {
        "False": False,
        "None": None,
        "True": True,
        "__build_class__": _safe_build_class,
        "__import__": _make_safe_import(proxies),
        "bool": bool,
        "dict": dict,
        "frozenset": frozenset,
        "int": int,
        "list": list,
        "str": str,
        "tuple": tuple,
    }


_FORBIDDEN_AUDIT_PREFIXES: tuple[str, ...] = (
    "code.__new__",
    "compile",
    "ctypes.",
    "fcntl.",
    "ftplib.",
    "function.__new__",
    "gc.",
    "http.",
    "imaplib.",
    "marshal.",
    "mmap.",
    "open",
    "os.",
    "pathlib.",
    "pickle.",
    "poplib.",
    "posix.",
    "shutil.",
    "signal.",
    "smtplib.",
    "socket.",
    "sqlite3.",
    "ssl.",
    "subprocess.",
    "sys.addaudithook",
    "sys.setprofile",
    "sys.settrace",
    "telnetlib.",
    "tempfile.",
    "urllib.",
    "webbrowser.",
    "winreg.",
)


def _install_audit_hook(allowed_code: types.CodeType) -> None:
    def _hook(event: str, args: tuple[object, ...]) -> None:
        if event == "exec":
            if not args or args[0] is not allowed_code:
                raise PermissionError("[NORN-PY-SANDBOX] dynamic exec blocked during elaboration")
            return
        if event == "compile":
            # Permit AST-only parsing of field annotations strictly inside `resolve_annotation`,
            # never arbitrary `compile(..., "<norn-annotation>", ...)` from authored code.
            if (
                is_annotation_parse_active()
                and len(args) >= 2
                and args[1] == "<norn-annotation>"
                and isinstance(args[0], (str, bytes))
            ):
                return
            raise PermissionError("[NORN-PY-SANDBOX] dynamic compile blocked during elaboration")
        if event == "import":
            mod_name = args[0] if args else ""
            if not isinstance(mod_name, str) or mod_name not in ALLOWED_IMPORT_ROOTS:
                raise PermissionError(f"[NORN-PY-SANDBOX] import {mod_name!r} blocked during elaboration")
            return
        for prefix in _FORBIDDEN_AUDIT_PREFIXES:
            if event == prefix or event.startswith(prefix):
                raise PermissionError(f"[NORN-PY-SANDBOX] audit event {event!r} blocked during elaboration")

    sys.addaudithook(_hook)


def _compute_clamped_rlimit(
    current_soft: int,
    current_hard: int,
    desired: int,
    rlim_infinity: int,
) -> tuple[int, int]:
    """Clamp `desired` so we never raise an inherited soft or hard rlimit."""
    if desired < 0:
        raise ValueError(f"rlimit desired value must be non-negative, got {desired}")
    new_hard = desired if current_hard == rlim_infinity else min(current_hard, desired)
    new_soft = new_hard if current_soft == rlim_infinity else min(current_soft, new_hard)
    return new_soft, new_hard


def _apply_posix_resource_limits(cpu_seconds: int, memory_bytes: int) -> None:
    """Apply fail-closed POSIX CPU, address-space, and zero-file-size limits inside the child process."""
    if os.name != "posix":
        return

    import resource

    def _cap(limit_const: int, target: int) -> None:
        cur_soft, cur_hard = resource.getrlimit(limit_const)
        new_soft, new_hard = _compute_clamped_rlimit(cur_soft, cur_hard, target, resource.RLIM_INFINITY)
        resource.setrlimit(limit_const, (new_soft, new_hard))

    if cpu_seconds < 1:
        raise ValueError(f"cpu_seconds must be >= 1, got {cpu_seconds}")
    if memory_bytes < 1:
        raise ValueError(f"memory_bytes must be >= 1, got {memory_bytes}")

    _cap(resource.RLIMIT_CPU, cpu_seconds)
    _cap(resource.RLIMIT_FSIZE, 0)
    if hasattr(resource, "RLIMIT_AS"):
        _cap(resource.RLIMIT_AS, memory_bytes)


def _read_source_bounded(
    file_path: Path,
    rel_source_path: str,
    *,
    max_bytes: int = MAX_SOURCE_BYTES,
) -> str:
    """Read at most `max_bytes` bytes from a regular file without following final symlinks or blocking on FIFOs."""
    return read_regular_file_bounded(file_path, rel_source_path, max_bytes=max_bytes)


def _elaborate_in_current_process(
    source: str,
    *,
    filename: str,
    options: EnvelopeOptions,
    enable_audit_hook: bool = False,
) -> dict[str, object]:
    preflight = preflight_source(source, filename)
    prior_source = snapshot_source_entry(filename)
    register_source(filename, source, preflight.tree)
    try:
        code = compile(preflight.tree, filename, "exec")
        restricted_builtins = _make_restricted_builtins()
        ns: dict[str, object] = {
            "__builtins__": restricted_builtins,
            "__file__": filename,
            "__name__": "__norn_authoring__",
        }
        if enable_audit_hook:
            # Pre-warm stdlib codecs and Unicode tables before locking down IO.
            _ = "".encode("utf-16-be")
            _install_audit_hook(code)
        exec(code, ns, ns)  # noqa: S102 - gated by Layer 1 AST preflight, closed builtins, and Layer 2 audit hook
        artifacts = collect_artifacts(ns, options)
        source_map = {
            name: {"column": sp.column, "file": sp.file, "line": sp.line}
            for name, sp in sorted(preflight.spans.items())
        }
        return {
            "artifacts": artifacts,
            "sourceMap": source_map,
        }
    finally:
        restore_source_entry(filename, prior_source)


@dataclass(frozen=True, slots=True)
class ElaborationResult:
    """Canonical elaboration result plus Norn1 compact bytes/hash and AST source map."""

    artifacts: list[JsonValue]
    canonical_text: str
    norn1_bytes: bytes
    norn1_sha256: str
    source_map: dict[str, dict[str, object]]


def sandbox_subprocess_cmd(script: str) -> list[str]:
    """Subprocess invocation flags: isolated from site/user-site/CWD while HONORING `PYTHONHASHSEED=0`."""
    # NOTE: `-I` is intentionally NOT used because CPython ignores PYTHONHASHSEED under `-I`.
    return [sys.executable, "-s", "-S", "-B", "-P", "-c", script]


def sandbox_subprocess_env(package_root: Path) -> dict[str, str]:
    """Minimal clean environment for deterministic sandboxed subprocess execution."""
    return {
        "LC_ALL": "C",
        "PYTHONHASHSEED": "0",
        "PYTHONPATH": str(package_root),
        "TZ": "UTC",
    }


def _run_bounded_subprocess(
    cmd: list[str],
    *,
    payload_bytes: bytes,
    env: Mapping[str, str],
    timeout_secs: float,
    max_stdout_bytes: int,
    max_stderr_bytes: int,
    rel_source_path: str,
) -> tuple[int, bytes, bytes]:
    """Run isolated subprocess with bounded stdout/stderr pipes and strict monotonic timeout."""
    span = SourceSpan(rel_source_path, 1, 0)
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=dict(env),
        bufsize=0,
    )
    assert proc.stdin is not None
    assert proc.stdout is not None
    assert proc.stderr is not None

    stdout_chunks: list[bytes] = []
    stderr_chunks: list[bytes] = []
    stdout_overflow = threading.Event()

    def _read_pipe(
        stream: IO[bytes],
        out: list[bytes],
        limit: int,
        overflow_event: threading.Event | None,
    ) -> None:
        total = 0
        try:
            while True:
                chunk = stream.read(65_536)
                if not chunk:
                    break
                if total <= limit:
                    remaining = (limit + 1) - total
                    out.append(chunk[:remaining])
                    total += min(len(chunk), remaining)
                    if total > limit and overflow_event is not None:
                        overflow_event.set()
                        try:
                            proc.kill()
                        except OSError:
                            pass
                        break
        except OSError:
            pass
        finally:
            try:
                stream.close()
            except OSError:
                pass

    def _write_stdin(stream: IO[bytes]) -> None:
        try:
            stream.write(payload_bytes)
            stream.flush()
        except OSError:
            pass
        finally:
            try:
                stream.close()
            except OSError:
                pass

    t_out = threading.Thread(
        target=_read_pipe,
        args=(proc.stdout, stdout_chunks, max_stdout_bytes, stdout_overflow),
        daemon=True,
    )
    t_err = threading.Thread(
        target=_read_pipe,
        args=(proc.stderr, stderr_chunks, max_stderr_bytes, None),
        daemon=True,
    )
    t_in = threading.Thread(target=_write_stdin, args=(proc.stdin,), daemon=True)
    t_out.start()
    t_err.start()
    t_in.start()

    deadline = time.monotonic() + timeout_secs
    try:
        remaining = max(0.0, deadline - time.monotonic())
        try:
            returncode = proc.wait(timeout=remaining)
        except subprocess.TimeoutExpired as exc:
            proc.kill()
            proc.wait()
            raise NornTracingError(
                "NORN-PY-TIMEOUT",
                f"elaboration exceeded {timeout_secs}s timeout",
                span,
            ) from exc
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        t_out.join(timeout=1.0)
        t_err.join(timeout=1.0)
        t_in.join(timeout=1.0)

    if stdout_overflow.is_set():
        raise NornTracingError(
            "NORN-PY-UNBOUNDED",
            f"elaboration output exceeded {max_stdout_bytes} byte limit",
            span,
        )
    return returncode, b"".join(stdout_chunks), b"".join(stderr_chunks)


def elaborate_file(
    path: str | Path,
    *,
    scope: Mapping[str, str],
    source_path: str | None = None,
    registry_package_path: str | None = None,
    timeout_secs: float = DEFAULT_TIMEOUT_SECS,
    memory_limit_bytes: int = DEFAULT_MEMORY_LIMIT_BYTES,
    max_output_bytes: int = MAX_SUBPROCESS_OUTPUT_BYTES,
) -> ElaborationResult:
    """Elaborate an authored `.py` file inside an isolated deterministic subprocess."""
    file_path = Path(path)
    rel_source_path = source_path if source_path is not None else str(path)
    span = SourceSpan(rel_source_path, 1, 0)
    source = _read_source_bounded(file_path, rel_source_path)

    # Run Layer 1 preflight locally first for instant crisp diagnostics.
    preflight_source(source, rel_source_path)

    package_root = Path(__file__).resolve().parent.parent
    cpu_secs = max(1, int(math.ceil(timeout_secs)))
    payload = json.dumps(
        {
            "cpuSecs": cpu_secs,
            "filename": rel_source_path,
            "memoryBytes": memory_limit_bytes,
            "registryPackagePath": registry_package_path,
            "scope": dict(scope),
            "source": source,
        }
    )

    runner = (
        "import json, sys\n"
        "from norn_py.canonical import EnvelopeOptions, canonical_json\n"
        "from norn_py.sandbox import _apply_posix_resource_limits, _elaborate_in_current_process\n"
        "req = json.loads(sys.stdin.read())\n"
        "_apply_posix_resource_limits(int(req['cpuSecs']), int(req['memoryBytes']))\n"
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

    returncode, raw_stdout, raw_stderr = _run_bounded_subprocess(
        sandbox_subprocess_cmd(runner),
        payload_bytes=payload.encode("utf-8"),
        env=sandbox_subprocess_env(package_root),
        timeout_secs=timeout_secs,
        max_stdout_bytes=max_output_bytes,
        max_stderr_bytes=MAX_SUBPROCESS_STDERR_BYTES,
        rel_source_path=rel_source_path,
    )
    if returncode != 0:
        stderr_text = raw_stderr.decode("utf-8", errors="replace").strip()
        stdout_text = raw_stdout.decode("utf-8", errors="replace").strip()
        detail = stderr_text or stdout_text or f"exit code {returncode}"
        raise NornTracingError("NORN-PY-ELABORATION", detail, span)

    try:
        parsed = json.loads(raw_stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise NornTracingError(
            "NORN-PY-ELABORATION",
            f"sandbox subprocess emitted malformed JSON output: {exc}",
            span,
        ) from exc

    if (
        not isinstance(parsed, dict)
        or not isinstance(parsed.get("artifacts"), list)
        or not isinstance(parsed.get("sourceMap"), dict)
    ):
        raise NornTracingError(
            "NORN-PY-ELABORATION",
            "sandbox subprocess emitted invalid elaboration envelope",
            span,
        )

    artifacts: list[JsonValue] = parsed["artifacts"]
    source_map: dict[str, dict[str, object]] = parsed["sourceMap"]
    norn1_bytes = canonical_norn1_bytes(artifacts)
    return ElaborationResult(
        artifacts=artifacts,
        canonical_text=canonical_json(artifacts),
        norn1_bytes=norn1_bytes,
        norn1_sha256=norn1_sha256_bytes(norn1_bytes),
        source_map=source_map,
    )


def verify_deterministic_elaboration(
    path: str | Path,
    *,
    scope: Mapping[str, str],
    source_path: str | None = None,
    registry_package_path: str | None = None,
) -> ElaborationResult:
    """Determinism gate: two fresh isolated runs MUST emit byte-identical Norn1 bytes and hashes."""
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
    if first.norn1_bytes != second.norn1_bytes or first.canonical_text != second.canonical_text:
        raise NornTracingError(
            "NORN-PY-NONDETERMINISTIC",
            "two fresh isolated elaborations emitted different canonical JSON bytes",
            SourceSpan(source_path or str(path), 1, 0),
        )
    return first
