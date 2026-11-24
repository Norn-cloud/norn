"""Norn `perform` / `FunctionManifest` HTTP worker for `norn.solver.rostering@0.1.0`.

Unmarked Slice Execution Contract:
  In the HR domain pack (`registry/packs/hr/rostering.ts`), `SolveRosterFn` declares the
  cell-level handle `{ rosterId, unitId, preferenceProfileId }`. Before invoking this HTTP
  worker, the cell host adapter resolves those handles, enforces `V2Markings` / `AccessIR`
  stripping, and submits the resulting unmarked operational slice (`SolveRequest` JSON
  containing ONLY opaque identifiers, exact integer/decimal quantities, and pinned
  `iana_timezone`) to `POST /v1/functions/norn.solver.rostering/invoke`.

Endpoints (stdlib `http.server.ThreadingHTTPServer`, bounded concurrency & timeouts):
- `GET /health` — liveness + operational metadata (`source_digest`, `manifest_digest`, `endpoints`).
- `GET /manifest` — strict canonical `FunctionManifest` JSON matching
  `crates/norn-ir/src/function.rs` (`#[serde(deny_unknown_fields)]`, exact 16 fields,
  zero extra keys; `implementation.format == "host"` pinned by source-tree digest).
- `POST /v1/functions/norn.solver.rostering/invoke` — duplicate-key-proof strict UTF-8
  JSON boundary -> cardinality bounds -> deterministic OR-Tools CP-SAT -> independent
  hard-law post-verifier -> full-payload canonical receipt.
"""

from __future__ import annotations

import argparse
import hashlib
import platform
import sys
import threading
from decimal import DecimalException
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.metadata import PackageNotFoundError
from importlib.metadata import version as pkg_version
from pathlib import Path
from typing import Any

from .boundary import (
    ModelInvalidError,
    PostVerificationError,
    SolverContractError,
    canonical_json,
    domain_sha256_digest,
    parse_strict_json,
    validate_invoke_payload,
)
from .models import (
    CONTRACT_ID,
    FUNCTION_ID,
    FUNCTION_VERSION,
    PreferenceProfileInput,
    SolveRequest,
)
from .solver import compare_what_if, solve_roster

FUNCTION_ABI_V1 = "norn.function-abi/1"
MAX_INPUT_BYTES = 4 * 1024 * 1024  # 4 MiB hard ingress cap
MAX_RESPONSE_BYTES = 8 * 1024 * 1024  # 8 MiB hard egress cap
MAX_OUTPUT_BYTES = MAX_RESPONSE_BYTES
SOCKET_READ_TIMEOUT_SECONDS = 10.0
DEFAULT_MAX_CONCURRENT_SOLVES = 4
DEFAULT_ACQUIRE_TIMEOUT_SECONDS = 0.05

_MANIFEST_DOMAIN = b"norn.function/manifest/v1\0"
_DEPENDENCY_CLOSURE_DOMAIN = b"norn.function/dependency-closure/v1\0"
_SOURCE_TREE_DOMAIN = b"norn.function/source-tree/v1\0"

_BAD_REQUEST_CODES: frozenset[str] = frozenset(
    {
        "NORN-SOLVER-DECIMAL",
        "NORN-SOLVER-DUPLICATE-KEY",
        "NORN-SOLVER-JSON",
    }
)


def package_source_digest() -> str:
    """Deterministic source-tree digest (HOST-TREE-1 shape) of `norn_rostering_solver/`."""
    pkg_dir = Path(__file__).resolve().parent
    entries: list[dict[str, object]] = []
    for path in sorted(pkg_dir.rglob("*.py")):
        rel = path.relative_to(pkg_dir).as_posix()
        if any(part.startswith(".") or part == "__pycache__" for part in path.relative_to(pkg_dir).parts):
            continue
        raw = path.read_bytes()
        entries.append(
            {
                "path": rel,
                "sha256": hashlib.sha256(raw).hexdigest(),
                "size": len(raw),
            }
        )
    entries.sort(key=lambda e: str(e["path"]).encode("utf-8"))
    return domain_sha256_digest(_SOURCE_TREE_DOMAIN, entries)


def _ortools_version() -> str:
    try:
        return pkg_version("ortools")
    except PackageNotFoundError:
        return "unknown"


def _input_schema_doc() -> dict[str, object]:
    return {
        "id": "norn.solver.rostering.input",
        "version": 1,
        "contract": CONTRACT_ID,
        "execution_contract": "unmarked_slice",
        "cell_adapter_note": (
            "Cell host adapter resolves SolveRosterFn handle (rosterId, unitId, preferenceProfileId) "
            "into this unmarked SolveRequest slice before invoking the worker."
        ),
        "required": ["roster_id", "unit_id", "profile", "employees", "shifts"],
        "optional": ["iana_timezone", "allow_cross_branch_transfers", "authorized_transfer_employee_ids"],
        "envelope": {
            "direct": "SolveRequest",
            "perform": {"input": "SolveRequest", "alt_profile": "optional PreferenceProfileInput"},
        },
        "refusals": [
            "NORN-SOLVER-DECIMAL",
            "NORN-SOLVER-DUPLICATE-KEY",
            "NORN-SOLVER-FLOAT-FORBIDDEN",
            "NORN-SOLVER-INTERVAL",
            "NORN-SOLVER-JSON",
            "NORN-SOLVER-LIMIT-EXCEEDED",
            "NORN-SOLVER-PII-FORBIDDEN",
            "NORN-SOLVER-SCHEMA",
            "NORN-SOLVER-UNKNOWN-FIELD",
        ],
    }


def _output_schema_doc() -> dict[str, object]:
    return {
        "id": "norn.solver.rostering.output",
        "version": 1,
        "contract": CONTRACT_ID,
        "perform_summary": {
            "status": "optimal | feasible | infeasible | unknown",
            "assignmentCount": "int",
            "solverDigest": "sha256:<64 hex>",
        },
        "fields": [
            "alternative_repairs",
            "assignment_count",
            "assignments",
            "backend",
            "contract",
            "evidence_label",
            "function_id",
            "input_digest",
            "metrics",
            "perform_summary",
            "profile_id",
            "repair_optimality",
            "repair_suggestions",
            "roster_id",
            "solver_receipt_digest",
            "status",
            "unsat_summary",
            "verification",
            "version",
        ],
    }


def _error_schema_doc() -> dict[str, object]:
    return {
        "id": "norn.solver.rostering.error",
        "version": 1,
        "codes": [
            "NORN-SOLVER-BUSY",
            "NORN-SOLVER-DECIMAL",
            "NORN-SOLVER-DUPLICATE-KEY",
            "NORN-SOLVER-FLOAT-FORBIDDEN",
            "NORN-SOLVER-INTERVAL",
            "NORN-SOLVER-JSON",
            "NORN-SOLVER-LIMIT-EXCEEDED",
            "NORN-SOLVER-MODEL-INVALID",
            "NORN-SOLVER-NOT-FOUND",
            "NORN-SOLVER-PAYLOAD-TOO-LARGE",
            "NORN-SOLVER-PII-FORBIDDEN",
            "NORN-SOLVER-POSTVERIFY-FAILED",
            "NORN-SOLVER-RESPONSE-TOO-LARGE",
            "NORN-SOLVER-SCHEMA",
            "NORN-SOLVER-TIMEOUT",
            "NORN-SOLVER-UNKNOWN-FIELD",
        ],
    }


def build_function_manifest() -> dict[str, object]:
    """Build the strict canonical `FunctionManifest` for `norn.solver.rostering`.

    Emits ONLY the exact 16 camelCase fields accepted by `FunctionManifest`
    (`crates/norn-ir/src/function.rs`, `#[serde(deny_unknown_fields)]`):
    uses `implementation.format = "host"` pinned by the Python package `source_digest`
    and `determinism.effectful` (never faking an OCI image or hermetic container claim).
    """
    source_digest = package_source_digest()
    ortools_ver = _ortools_version()
    py_ver = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"

    in_doc = _input_schema_doc()
    out_doc = _output_schema_doc()
    err_doc = _error_schema_doc()

    # Dependency closure entries MUST be strictly sorted by `name` (Core 07).
    dep_entries: list[dict[str, object]] = [
        {
            "digest": domain_sha256_digest(
                _DEPENDENCY_CLOSURE_DOMAIN,
                {"name": "ortools", "version": ortools_ver},
            ),
            "kind": "library",
            "name": "ortools",
        },
        {
            "digest": domain_sha256_digest(
                _DEPENDENCY_CLOSURE_DOMAIN,
                {"name": "python-stdlib", "version": py_ver},
            ),
            "kind": "library",
            "name": "python-stdlib",
        },
    ]
    dep_closure_digest = domain_sha256_digest(_DEPENDENCY_CLOSURE_DOMAIN, dep_entries)

    runtime_digest = domain_sha256_digest(
        b"norn.function/runtime/v1\0",
        {
            "id": "norn-python-cpsat-worker",
            "ortools": ortools_ver,
            "python": py_ver,
            "seed": 0,
            "source_digest": source_digest,
            "workers": 1,
        },
    )

    arch_raw = platform.machine().lower()
    arch = "amd64" if arch_raw in ("x86_64", "amd64") else ("arm64" if arch_raw in ("aarch64", "arm64") else arch_raw)

    return {
        "abi": FUNCTION_ABI_V1,
        "bounds": {
            "maxCpuMillis": 30_000,
            "maxDecodedBytes": 16 * 1024 * 1024,
            "maxInputBytes": MAX_INPUT_BYTES,
            "maxMemoryBytes": 512 * 1024 * 1024,
            "maxOutputBytes": MAX_RESPONSE_BYTES,
            "maxWallMillis": 30_000,
        },
        "cache": "none",
        "capabilities": [],
        "dependencyClosure": {
            "digest": dep_closure_digest,
            "entries": dep_entries,
        },
        "determinism": {
            "effectful": {
                "maxAttempts": 1,
                "providerIdempotency": True,
            }
        },
        "errorSchema": {
            "digest": domain_sha256_digest(b"norn.schema/v1\0", err_doc),
            "id": "norn.solver.rostering.error",
            "version": 1,
        },
        "functionId": FUNCTION_ID,
        "implementation": {
            "digest": source_digest,
            "entryPoint": "norn_rostering_solver.worker:handle_invoke_payload",
            "format": "host",
            "locator": f"norn-rostering-solver/{FUNCTION_VERSION}@{source_digest}",
        },
        "inputSchema": {
            "digest": domain_sha256_digest(b"norn.schema/v1\0", in_doc),
            "id": "norn.solver.rostering.input",
            "version": 1,
        },
        "outputSchema": {
            "digest": domain_sha256_digest(b"norn.schema/v1\0", out_doc),
            "id": "norn.solver.rostering.output",
            "version": 1,
        },
        "platform": {
            "arch": arch,
            "os": platform.system().lower(),
        },
        "retention": "ledger",
        "runtime": {
            "digest": runtime_digest,
            "id": "norn-python-cpsat-worker",
            "version": py_ver,
        },
        "steps": [],
        "version": 1,
    }


def compute_manifest_digest(manifest: dict[str, object] | None = None) -> str:
    """Compute `FunctionManifest::content_digest` (`MANIFEST_DOMAIN || canonical_json`)."""
    doc = build_function_manifest() if manifest is None else manifest
    return domain_sha256_digest(_MANIFEST_DOMAIN, doc)


def handle_invoke_payload(
    raw_bytes: bytes,
    *,
    max_response_bytes: int = MAX_RESPONSE_BYTES,
) -> tuple[int, dict[str, object]]:
    """Process one `/v1/functions/norn.solver.rostering/invoke` request body deterministically."""
    if len(raw_bytes) > MAX_INPUT_BYTES:
        return (
            int(HTTPStatus.REQUEST_ENTITY_TOO_LARGE),
            {
                "code": "NORN-SOLVER-PAYLOAD-TOO-LARGE",
                "error": f"payload size {len(raw_bytes)} exceeds max_input_bytes ({MAX_INPUT_BYTES})",
                "status": "refused",
            },
        )

    try:
        parsed = parse_strict_json(raw_bytes)
        raw_request, request_path, invocation_id, raw_alt_profile = validate_invoke_payload(parsed)

        request = SolveRequest.from_dict(raw_request, request_path)
        alt_profile = (
            PreferenceProfileInput.from_dict(raw_alt_profile, "$.alt_profile")
            if raw_alt_profile is not None
            else None
        )

        response = solve_roster(request)
        payload: dict[str, object] = response.to_dict()
        if invocation_id is not None:
            payload["invocation_id"] = invocation_id
        if alt_profile is not None:
            payload["what_if"] = compare_what_if(request, alt_profile).to_dict()

        encoded_len = len((canonical_json(payload) + "\n").encode("utf-8"))
        if encoded_len > max_response_bytes:
            return (
                int(HTTPStatus.INTERNAL_SERVER_ERROR),
                {
                    "code": "NORN-SOLVER-RESPONSE-TOO-LARGE",
                    "error": f"response size {encoded_len} exceeds max_response_bytes ({max_response_bytes})",
                    "status": "refused",
                },
            )

        return int(HTTPStatus.OK), payload
    except PostVerificationError as exc:
        return (
            int(HTTPStatus.INTERNAL_SERVER_ERROR),
            {
                "code": exc.code,
                "error": exc.message,
                "status": "verification_failed",
                "violations": list(exc.violations),
            },
        )
    except ModelInvalidError as exc:
        return (
            int(HTTPStatus.INTERNAL_SERVER_ERROR),
            {
                "code": exc.code,
                "error": exc.message,
                "status": "error",
            },
        )
    except SolverContractError as exc:
        http_status = HTTPStatus.BAD_REQUEST if exc.code in _BAD_REQUEST_CODES else HTTPStatus.UNPROCESSABLE_ENTITY
        return (
            int(http_status),
            {
                "code": exc.code,
                "error": exc.message,
                "status": "refused",
            },
        )
    except UnicodeDecodeError as exc:
        return (
            int(HTTPStatus.BAD_REQUEST),
            {
                "code": "NORN-SOLVER-JSON",
                "error": f"invalid UTF-8 payload: {exc}",
                "status": "refused",
            },
        )
    except DecimalException as exc:
        return (
            int(HTTPStatus.BAD_REQUEST),
            {
                "code": "NORN-SOLVER-DECIMAL",
                "error": f"invalid decimal value: {exc}",
                "status": "refused",
            },
        )


class RosteringWorkerServer(ThreadingHTTPServer):
    """Bounded `ThreadingHTTPServer` with socket timeout, concurrency semaphore, and response byte cap."""

    daemon_threads = True
    allow_reuse_address = True

    def __init__(
        self,
        server_address: tuple[str, int],
        *,
        max_concurrent_solves: int = DEFAULT_MAX_CONCURRENT_SOLVES,
        socket_timeout: float = SOCKET_READ_TIMEOUT_SECONDS,
        max_response_bytes: int = MAX_RESPONSE_BYTES,
        acquire_timeout: float = DEFAULT_ACQUIRE_TIMEOUT_SECONDS,
    ) -> None:
        if max_concurrent_solves < 1:
            raise ValueError("max_concurrent_solves must be >= 1")
        if socket_timeout <= 0:
            raise ValueError("socket_timeout must be > 0")
        if max_response_bytes < 256:
            raise ValueError("max_response_bytes must be >= 256")
        self.socket_timeout = socket_timeout
        self.max_response_bytes = max_response_bytes
        self.acquire_timeout = acquire_timeout
        self.concurrency_semaphore = threading.BoundedSemaphore(max_concurrent_solves)
        super().__init__(server_address, RosteringWorkerHandler)


class RosteringWorkerHandler(BaseHTTPRequestHandler):
    """HTTP handler for `norn.solver.rostering@0.1.0`."""

    server_version = f"norn-rostering-worker/{FUNCTION_VERSION}"
    server: RosteringWorkerServer

    def setup(self) -> None:
        timeout = getattr(self.server, "socket_timeout", SOCKET_READ_TIMEOUT_SECONDS)
        self.request.settimeout(timeout)
        super().setup()

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
        # Suppress noisy stderr access logs during embedded/test runs.
        return

    def _send_canonical_json(
        self,
        status_code: int,
        payload: dict[str, object],
        *,
        extra_headers: dict[str, str] | None = None,
    ) -> None:
        max_resp = getattr(self.server, "max_response_bytes", MAX_RESPONSE_BYTES)
        body = (canonical_json(payload) + "\n").encode("utf-8")
        if len(body) > max_resp:
            status_code = int(HTTPStatus.INTERNAL_SERVER_ERROR)
            body = (
                canonical_json(
                    {
                        "code": "NORN-SOLVER-RESPONSE-TOO-LARGE",
                        "error": f"response size {len(body)} exceeds max_response_bytes ({max_resp})",
                        "status": "refused",
                    }
                )
                + "\n"
            ).encode("utf-8")
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Norn-Contract", CONTRACT_ID)
        if extra_headers:
            for h_key, h_val in sorted(extra_headers.items()):
                self.send_header(h_key, h_val)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        path = self.path.split("?", 1)[0]
        if path == "/health":
            manifest = build_function_manifest()
            self._send_canonical_json(
                int(HTTPStatus.OK),
                {
                    "backend": "ortools_cpsat",
                    "contract": CONTRACT_ID,
                    "endpoints": {
                        "health": "/health",
                        "invoke": f"/v1/functions/{FUNCTION_ID}/invoke",
                        "manifest": "/manifest",
                    },
                    "execution_contract": "unmarked_slice",
                    "functionId": FUNCTION_ID,
                    "manifest_digest": compute_manifest_digest(manifest),
                    "source_digest": package_source_digest(),
                    "status": "ok",
                    "version": FUNCTION_VERSION,
                },
            )
            return
        if path == "/manifest":
            manifest = build_function_manifest()
            self._send_canonical_json(
                int(HTTPStatus.OK),
                manifest,
                extra_headers={"X-Norn-Manifest-Digest": compute_manifest_digest(manifest)},
            )
            return
        self._send_canonical_json(
            int(HTTPStatus.NOT_FOUND),
            {"code": "NORN-SOLVER-NOT-FOUND", "error": f"unknown route {path!r}", "status": "refused"},
        )

    def do_POST(self) -> None:
        path = self.path.split("?", 1)[0]
        if path != f"/v1/functions/{FUNCTION_ID}/invoke":
            self._send_canonical_json(
                int(HTTPStatus.NOT_FOUND),
                {"code": "NORN-SOLVER-NOT-FOUND", "error": f"unknown route {path!r}", "status": "refused"},
            )
            return

        content_len_raw = self.headers.get("Content-Length")
        if content_len_raw is None:
            self._send_canonical_json(
                int(HTTPStatus.LENGTH_REQUIRED),
                {"code": "NORN-SOLVER-SCHEMA", "error": "Content-Length header is required", "status": "refused"},
            )
            return
        try:
            content_len = int(content_len_raw)
        except ValueError:
            self._send_canonical_json(
                int(HTTPStatus.BAD_REQUEST),
                {"code": "NORN-SOLVER-SCHEMA", "error": "invalid Content-Length header", "status": "refused"},
            )
            return
        if content_len < 0 or content_len > MAX_INPUT_BYTES:
            self._send_canonical_json(
                int(HTTPStatus.REQUEST_ENTITY_TOO_LARGE),
                {
                    "code": "NORN-SOLVER-PAYLOAD-TOO-LARGE",
                    "error": f"payload size {content_len} exceeds max_input_bytes ({MAX_INPUT_BYTES})",
                    "status": "refused",
                },
            )
            return

        try:
            raw_bytes = self.rfile.read(content_len)
        except TimeoutError:
            self._send_canonical_json(
                int(HTTPStatus.REQUEST_TIMEOUT),
                {
                    "code": "NORN-SOLVER-TIMEOUT",
                    "error": "socket read timed out while receiving request body",
                    "status": "refused",
                },
            )
            return

        if len(raw_bytes) != content_len:
            self._send_canonical_json(
                int(HTTPStatus.BAD_REQUEST),
                {
                    "code": "NORN-SOLVER-JSON",
                    "error": f"truncated request body: expected {content_len} bytes, got {len(raw_bytes)}",
                    "status": "refused",
                },
            )
            return

        sem = self.server.concurrency_semaphore
        acquire_timeout = self.server.acquire_timeout
        acquired = sem.acquire(blocking=True, timeout=acquire_timeout) if acquire_timeout > 0 else sem.acquire(blocking=False)
        if not acquired:
            self._send_canonical_json(
                int(HTTPStatus.SERVICE_UNAVAILABLE),
                {
                    "code": "NORN-SOLVER-BUSY",
                    "error": "solver concurrency limit reached; retry later",
                    "status": "refused",
                },
            )
            return

        try:
            status_code, payload = handle_invoke_payload(
                raw_bytes,
                max_response_bytes=self.server.max_response_bytes,
            )
        finally:
            sem.release()

        self._send_canonical_json(status_code, payload)


def create_worker_server(
    host: str = "127.0.0.1",
    port: int = 8091,
    *,
    max_concurrent_solves: int = DEFAULT_MAX_CONCURRENT_SOLVES,
    socket_timeout: float = SOCKET_READ_TIMEOUT_SECONDS,
    max_response_bytes: int = MAX_RESPONSE_BYTES,
    acquire_timeout: float = DEFAULT_ACQUIRE_TIMEOUT_SECONDS,
) -> RosteringWorkerServer:
    """Create a configured bounded `RosteringWorkerServer` bound to `(host, port)` (port=0 picks an ephemeral port)."""
    return RosteringWorkerServer(
        (host, port),
        max_concurrent_solves=max_concurrent_solves,
        socket_timeout=socket_timeout,
        max_response_bytes=max_response_bytes,
        acquire_timeout=acquire_timeout,
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run the Norn OR-Tools CP-SAT rostering worker HTTP server.")
    parser.add_argument("--host", default="127.0.0.1", help="Bind host (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=8091, help="Bind port (default: 8091)")
    parser.add_argument(
        "--max-concurrent",
        type=int,
        default=DEFAULT_MAX_CONCURRENT_SOLVES,
        help=f"Maximum concurrent solves (default: {DEFAULT_MAX_CONCURRENT_SOLVES})",
    )
    parser.add_argument(
        "--socket-timeout",
        type=float,
        default=SOCKET_READ_TIMEOUT_SECONDS,
        help=f"Socket read timeout in seconds (default: {SOCKET_READ_TIMEOUT_SECONDS})",
    )
    args = parser.parse_args(argv)

    server = create_worker_server(
        args.host,
        args.port,
        max_concurrent_solves=args.max_concurrent,
        socket_timeout=args.socket_timeout,
    )
    bound_host, bound_port = server.server_address[:2]
    sys.stdout.write(
        canonical_json(
            {
                "contract": CONTRACT_ID,
                "event": "listening",
                "host": str(bound_host),
                "port": int(bound_port),
            }
        )
        + "\n"
    )
    sys.stdout.flush()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
