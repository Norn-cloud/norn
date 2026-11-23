"""Norn `perform` / `FunctionManifest` HTTP worker for `norn.solver.rostering@0.1.0`.

Uses Python stdlib `http.server.ThreadingHTTPServer` (zero web-framework dependencies):
- `GET /health`
- `GET /manifest` — canonical `FunctionManifest` for `norn.solver.rostering@0.1.0`
  aligned with `registry/packs/hr/rostering.ts` (`SolveRosterFn`) and Core 07 (`FunctionManifest`).
- `POST /v1/functions/norn.solver.rostering/invoke` — strict JSON boundary ->
  deterministic OR-Tools CP-SAT -> independent hard-law post-verifier -> canonical receipt.
"""

from __future__ import annotations

import argparse
import hashlib
import platform
import sys
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.metadata import PackageNotFoundError
from importlib.metadata import version as pkg_version
from pathlib import Path
from typing import Any

from .boundary import (
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
MAX_OUTPUT_BYTES = 8 * 1024 * 1024

_MANIFEST_DOMAIN = b"norn.function/manifest/v1\0"
_DEPENDENCY_CLOSURE_DOMAIN = b"norn.function/dependency-closure/v1\0"
_SOURCE_TREE_DOMAIN = b"norn.function/source-tree/v1\0"


def _package_source_digest() -> str:
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
        "required": ["roster_id", "unit_id", "profile", "employees", "shifts"],
        "envelope": {
            "direct": "SolveRequest",
            "perform": {"input": "SolveRequest", "alt_profile": "optional PreferenceProfileInput"},
        },
        "perform_handle_input": {
            "rosterId": "string",
            "unitId": "string",
            "preferenceProfileId": "string",
        },
        "refusals": [
            "NORN-SOLVER-PII-FORBIDDEN",
            "NORN-SOLVER-UNKNOWN-FIELD",
            "NORN-SOLVER-FLOAT-FORBIDDEN",
            "NORN-SOLVER-SCHEMA",
        ],
    }


def _output_schema_doc() -> dict[str, object]:
    return {
        "id": "norn.solver.rostering.output",
        "version": 1,
        "contract": CONTRACT_ID,
        "perform_summary": {
            "status": "optimal | feasible | infeasible",
            "assignmentCount": "int",
            "solverDigest": "sha256:<64 hex>",
        },
        "fields": [
            "contract",
            "roster_id",
            "profile_id",
            "status",
            "backend",
            "assignments",
            "assignment_count",
            "metrics",
            "repair_suggestions",
            "alternative_repairs",
            "unsat_summary",
            "verification",
            "input_digest",
            "solver_receipt_digest",
            "perform_summary",
        ],
    }


def _error_schema_doc() -> dict[str, object]:
    return {
        "id": "norn.solver.rostering.error",
        "version": 1,
        "codes": [
            "NORN-SOLVER-FLOAT-FORBIDDEN",
            "NORN-SOLVER-INTERVAL",
            "NORN-SOLVER-JSON",
            "NORN-SOLVER-PAYLOAD-TOO-LARGE",
            "NORN-SOLVER-PII-FORBIDDEN",
            "NORN-SOLVER-POSTVERIFY-FAILED",
            "NORN-SOLVER-SCHEMA",
            "NORN-SOLVER-UNKNOWN-FIELD",
        ],
    }


def build_function_manifest() -> dict[str, object]:
    """Build the canonical `FunctionManifest` for `norn.solver.rostering@0.1.0`."""
    impl_digest = _package_source_digest()
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
        {"id": "python-worker", "ortools": ortools_ver, "python": py_ver, "workers": 1, "seed": 0},
    )

    arch_raw = platform.machine().lower()
    arch = "amd64" if arch_raw in ("x86_64", "amd64") else ("arm64" if arch_raw in ("aarch64", "arm64") else arch_raw)

    manifest_body: dict[str, object] = {
        "abi": FUNCTION_ABI_V1,
        "bounds": {
            "maxCpuMillis": 30_000,
            "maxDecodedBytes": 16 * 1024 * 1024,
            "maxInputBytes": MAX_INPUT_BYTES,
            "maxMemoryBytes": 512 * 1024 * 1024,
            "maxOutputBytes": MAX_OUTPUT_BYTES,
            "maxWallMillis": 30_000,
        },
        "cache": "none",
        "capabilities": [],
        "contractId": CONTRACT_ID,
        "dependencyClosure": {
            "digest": dep_closure_digest,
            "entries": dep_entries,
        },
        "determinism": {
            "hermetic_container": {
                "credentials": False,
                "goldenRuns": [],
                "isolatedScratch": True,
                "locale": "C.UTF-8",
                "network": False,
                "qualified": False,
                "timezone": "UTC",
            }
        },
        "endpoints": {
            "health": "/health",
            "invoke": f"/v1/functions/{FUNCTION_ID}/invoke",
            "manifest": "/manifest",
        },
        "errorSchema": {
            "digest": domain_sha256_digest(b"norn.schema/v1\0", err_doc),
            "id": "norn.solver.rostering.error",
            "version": 1,
        },
        "functionId": FUNCTION_ID,
        "implementation": {
            "digest": impl_digest,
            "entryPoint": "norn_rostering_solver.worker:handle_invoke_payload",
            "format": "oci_image",
            "locator": f"norn-rostering-solver/{FUNCTION_VERSION}@{impl_digest}",
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
        "semver": FUNCTION_VERSION,
        "steps": [],
        "version": 1,
    }
    manifest_digest = domain_sha256_digest(_MANIFEST_DOMAIN, manifest_body)
    return {
        **manifest_body,
        "manifestDigest": manifest_digest,
    }


def handle_invoke_payload(raw_bytes: bytes) -> tuple[int, dict[str, object]]:
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
    except SolverContractError as exc:
        http_status = HTTPStatus.BAD_REQUEST if exc.code == "NORN-SOLVER-JSON" else HTTPStatus.UNPROCESSABLE_ENTITY
        return (
            int(http_status),
            {
                "code": exc.code,
                "error": exc.message,
                "status": "refused",
            },
        )


class RosteringWorkerHandler(BaseHTTPRequestHandler):
    """HTTP handler for `norn.solver.rostering@0.1.0`."""

    server_version = f"norn-rostering-worker/{FUNCTION_VERSION}"

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
        # Suppress noisy stderr access logs during embedded/test runs.
        return

    def _send_canonical_json(self, status_code: int, payload: dict[str, object]) -> None:
        body = (canonical_json(payload) + "\n").encode("utf-8")
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Norn-Contract", CONTRACT_ID)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        path = self.path.split("?", 1)[0]
        if path == "/health":
            self._send_canonical_json(
                int(HTTPStatus.OK),
                {
                    "backend": "ortools_cpsat",
                    "contract": CONTRACT_ID,
                    "functionId": FUNCTION_ID,
                    "status": "ok",
                    "version": FUNCTION_VERSION,
                },
            )
            return
        if path == "/manifest":
            self._send_canonical_json(int(HTTPStatus.OK), build_function_manifest())
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

        raw_bytes = self.rfile.read(content_len)
        status_code, payload = handle_invoke_payload(raw_bytes)
        self._send_canonical_json(status_code, payload)


def create_worker_server(host: str = "127.0.0.1", port: int = 8091) -> ThreadingHTTPServer:
    """Create a configured `ThreadingHTTPServer` bound to `(host, port)` (port=0 picks an ephemeral port)."""
    return ThreadingHTTPServer((host, port), RosteringWorkerHandler)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run the Norn OR-Tools CP-SAT rostering worker HTTP server.")
    parser.add_argument("--host", default="127.0.0.1", help="Bind host (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=8091, help="Bind port (default: 8091)")
    args = parser.parse_args(argv)

    server = create_worker_server(args.host, args.port)
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
