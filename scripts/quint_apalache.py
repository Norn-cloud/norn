#!/usr/bin/env python3
"""Run one `quint verify --backend apalache` check against a harness-owned server.

Policy (see verification/README.md, "Quint/Apalache execution policy"):

* Every model check gets its own pinned Apalache server on a freshly selected
  dual-stack loopback port. The harness starts it, waits for it to accept
  connections, points Quint at it with `--server-endpoint`, and terminates the
  whole server process group afterwards, waiting until the port is released.
  One server serves one check: sequential checks against a shared, reused
  Apalache 0.56.1 server were observed to hang with the request accepted but
  never started (#2830), so the server is deliberately not shared.
* Every attempt has a wall-clock timeout well below the CI job timeout. On
  timeout the Quint process group is killed and the check fails; a timeout of
  a check that the server actually began is never retried.
* A check-start watchdog: if the server has not begun any check
  CHECK_START_WATCHDOG_SECONDS after Quint started, and Quint printed
  nothing, the request was lost before model checking (the stall class behind
  the 44-minute hang in #2795/#2830). The attempt is killed as a server stall.
* A lost-response watchdog: if the server logged its outcome for the check
  but Quint has not exited RESPONSE_GRACE_SECONDS later, the gRPC response
  was lost. A lost NoError is retried; a lost non-NoError outcome is final.
* Verdict first: after every attempt, the server-logged outcome and Quint's
  `--out` status are read before any retry decision (waiting for the outcome
  of a check the server began). A non-NoError server outcome, a Quint
  "violation" status (even with `errors: []`), any diagnostics, or a
  counterexample marker is final, whatever transport-error text Quint printed.
* At most one retry, and only when neither the server nor Quint produced a
  verdict: a server startup/connection/deadline error, a server stall, a lost
  NoError response, or a missing/diagnostic-free `--out` result that the
  server attests had no verdict. A startup/connection retry reuses the same
  (now warmed) server when it is still alive and has not run a check; any
  other retry uses a fresh server.
* Both attempts are logged, with the classification that caused the retry.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from typing import Callable, Sequence

APALACHE_VERSION = "0.56.1"
DEFAULT_ATTEMPT_TIMEOUT_SECONDS = 300
SERVER_READY_TIMEOUT_SECONDS = 120
# If the server has not begun a check this long after Quint connected, and
# Quint printed nothing, the request was lost before model checking started.
CHECK_START_WATCHDOG_SECONDS = 60
# Once the server logs its outcome for this check, Quint must receive the
# response and exit within this grace; otherwise the response was lost.
RESPONSE_GRACE_SECONDS = 60
PORT_RELEASE_TIMEOUT_SECONDS = 20
MAX_ATTEMPTS = 2
MAX_SERVER_STARTS = 3

# Apalache's launcher accepts environment overrides that can replace the
# attested JAR/JVM behavior or alter checker configuration.
SCRUBBED_ENVIRONMENT = (
    "APALACHE_JAR",
    "CONFIG_FILE",
    "JVM_ARGS",
    "JVM_GC_ARGS",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
    "OUT_DIR",
    "RUN_DIR",
    "SMT_ENCODING",
    "WRITE_INTERMEDIATE",
)

# Markers of a real model-checking verdict. Their presence makes a result final.
VERDICT_MARKERS = ("[violation]", "found a counterexample")

OUTCOME_PATTERN = re.compile(r"The outcome is: (\w+)")

# Quint/gRPC client errors that mean the checker never delivered a verdict.
# Quint 0.32.0 queries the gRPC reflection endpoint with a hard-coded 5 s
# deadline; a cold JVM on a loaded 4-vCPU runner can miss it (#2830).
SERVER_ERROR_PATTERN = re.compile(
    r"Error querying reflection endpoint"
    r"|DEADLINE_EXCEEDED"
    r"|Timed out waiting for Apalache server"
    r"|\b14 UNAVAILABLE\b"
    r"|\b12 UNIMPLEMENTED\b"
    r"|No connection established"
    r"|Failed to launch Apalache server"
    r"|Apalache gRPC endpoint is corrupted"
    r"|BindException|Address already in use"
)


class HarnessError(RuntimeError):
    """The harness could not provide a server; no check was attempted."""


@dataclass
class Attempt:
    number: int
    port: int
    returncode: int | None
    timed_out: bool
    stalled: bool
    # Server-side outcome ("NoError", "Error", ...) when the server finished but
    # Quint never received the response; None otherwise.
    undelivered_outcome: str | None
    seconds: float
    output: str
    # Server-side evidence attributed to this attempt: whether the server began
    # a check, and every outcome it logged ("NoError", "Error", ...).
    check_started: bool = False
    server_outcomes: list[str] = field(default_factory=list)
    classification: str = ""


@dataclass
class QuintRun:
    returncode: int
    output: str
    attempts: list[Attempt] = field(default_factory=list)

    @property
    def timed_out(self) -> bool:
        return bool(self.attempts) and self.attempts[-1].timed_out


def repo_root() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parent.parent


def default_quint_home() -> pathlib.Path:
    tool_root = os.environ.get(
        "NORN_VERIFICATION_TOOL_ROOT", str(repo_root() / "target/verification-tools")
    )
    return pathlib.Path(os.environ.get("QUINT_HOME", str(pathlib.Path(tool_root) / "quint-home")))


def apalache_launcher(quint_home: pathlib.Path) -> pathlib.Path:
    return quint_home / f"apalache-dist-{APALACHE_VERSION}" / "apalache" / "bin" / "apalache-mc"


def scrubbed_environment(base: dict[str, str] | None = None) -> dict[str, str]:
    environment = dict(os.environ if base is None else base)
    for name in SCRUBBED_ENVIRONMENT:
        environment.pop(name, None)
    return environment


def select_port() -> int:
    """Pick a loopback port that is free on both IPv4 and IPv6."""

    for _ in range(64):
        ipv4 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        ipv6 = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
        try:
            ipv4.bind(("127.0.0.1", 0))
            port = ipv4.getsockname()[1]
            ipv6.bind(("::1", port))
            return port
        except OSError:
            continue
        finally:
            ipv4.close()
            ipv6.close()
    raise HarnessError("could not select an unused dual-stack loopback port")


def port_released(port: int) -> bool:
    sockets = []
    try:
        for family, address in (
            (socket.AF_INET, ("127.0.0.1", port)),
            (socket.AF_INET6, ("::1", port)),
        ):
            sock = socket.socket(family, socket.SOCK_STREAM)
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(address)
            sock.listen(1)
            sockets.append(sock)
        return True
    except OSError:
        return False
    finally:
        for sock in sockets:
            sock.close()


def port_accepts(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1.0):
            return True
    except OSError:
        return False


def kill_group(process: subprocess.Popen, grace_seconds: float = 10.0) -> None:
    """Terminate a process group we started (start_new_session=True)."""

    if process.poll() is not None:
        # The leader exited; still reap any orphans left in its group.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


class ApalacheServer:
    """One pinned Apalache server owned by the harness for one check."""

    def __init__(self, quint_home: pathlib.Path, environment: dict[str, str], log: Callable[[str], None]):
        self.launcher = apalache_launcher(quint_home)
        if not os.access(self.launcher, os.X_OK):
            raise HarnessError(f"missing pinned Apalache launcher: {self.launcher}")
        self.environment = environment
        self.log = log
        self.port = 0
        self.process: subprocess.Popen | None = None
        self.workdir: tempfile.TemporaryDirectory | None = None
        self.log_path: pathlib.Path | None = None
        self._scan_offset = 0
        self._scan_partial = ""
        self._checks_started = 0
        self._outcomes: list[str] = []

    def start(self) -> None:
        last_error = ""
        for start in range(1, MAX_SERVER_STARTS + 1):
            self.workdir = tempfile.TemporaryDirectory(prefix="norn-apalache-")
            self.log_path = pathlib.Path(self.workdir.name) / "server.log"
            self._scan_offset = 0
            self._scan_partial = ""
            self._checks_started = 0
            self._outcomes = []
            self.port = select_port()
            began = time.monotonic()
            with self.log_path.open("wb") as server_log:
                self.process = subprocess.Popen(
                    [str(self.launcher), "server", f"--port={self.port}"],
                    cwd=self.workdir.name,
                    env=self.environment,
                    stdin=subprocess.DEVNULL,
                    stdout=server_log,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
            deadline = began + SERVER_READY_TIMEOUT_SECONDS
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    last_error = f"server exited with code {self.process.returncode} before accepting connections"
                    break
                if port_accepts(self.port):
                    self.log(
                        f"[apalache] server {APALACHE_VERSION} ready on 127.0.0.1:{self.port} "
                        f"after {time.monotonic() - began:.1f}s (start {start})"
                    )
                    return
                time.sleep(0.2)
            else:
                last_error = f"server did not accept connections within {SERVER_READY_TIMEOUT_SECONDS}s"
            self.log(f"[apalache] start {start} on port {self.port} failed: {last_error}")
            self.log(self.log_tail())
            self.stop()
        raise HarnessError(f"Apalache server could not be started: {last_error}")

    def alive(self) -> bool:
        return self.process is not None and self.process.poll() is None

    def _scan(self) -> None:
        """Incrementally scan new server-log lines for check starts and outcomes."""

        if self.log_path is None or not self.log_path.exists():
            return
        with self.log_path.open("rb") as handle:
            handle.seek(self._scan_offset)
            chunk = handle.read()
        self._scan_offset += len(chunk)
        text = self._scan_partial + chunk.decode("utf-8", errors="replace")
        lines = text.split("\n")
        self._scan_partial = lines.pop()
        for line in lines:
            if "PASS #0" in line:
                self._checks_started += 1
            match = OUTCOME_PATTERN.search(line)
            if match:
                self._outcomes.append(match.group(1))

    def outcomes_logged(self) -> list[str]:
        """Outcomes the server has logged, one per finished check."""

        self._scan()
        return list(self._outcomes)

    def checks_logged(self) -> int:
        """Number of checks the server has begun (one SANY parse per check)."""

        self._scan()
        return self._checks_started

    def log_tail(self, lines: int = 40) -> str:
        if self.log_path is None or not self.log_path.exists():
            return "(no server log)"
        text = self.log_path.read_text(encoding="utf-8", errors="replace").splitlines()
        return "\n".join(["--- apalache server log tail ---", *text[-lines:]])

    def stop(self) -> None:
        if self.process is not None:
            kill_group(self.process)
            self.process = None
        if self.port:
            deadline = time.monotonic() + PORT_RELEASE_TIMEOUT_SECONDS
            while not port_released(self.port):
                if time.monotonic() >= deadline:
                    raise HarnessError(
                        f"Apalache server did not release port {self.port} within "
                        f"{PORT_RELEASE_TIMEOUT_SECONDS}s"
                    )
                time.sleep(0.1)
            self.port = 0
        if self.workdir is not None:
            self.workdir.cleanup()
            self.workdir = None


def out_path(argv: Sequence[str], cwd: pathlib.Path) -> pathlib.Path | None:
    for index, argument in enumerate(argv):
        if argument == "--out" and index + 1 < len(argv):
            candidate = pathlib.Path(argv[index + 1])
            return candidate if candidate.is_absolute() else cwd / candidate
        if argument.startswith("--out="):
            candidate = pathlib.Path(argument.split("=", 1)[1])
            return candidate if candidate.is_absolute() else cwd / candidate
    return None


def read_result(path: pathlib.Path | None) -> dict | None:
    if path is None or not path.is_file() or path.stat().st_size == 0:
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return None
    return payload if isinstance(payload, dict) else None


RETRYABLE = frozenset({"server", "server-stall", "lost-response", "no-result"})


def classify(attempt: Attempt, result_path: pathlib.Path | None) -> str:
    """Classify an attempt from its exit, output, `--out` result, and server log."""

    return classify_evidence(attempt, read_result(result_path), expects_result=result_path is not None)


def classify_evidence(attempt: Attempt, result: dict | None, *, expects_result: bool) -> str:
    """Return success, verdict, timeout, failure, or a RETRYABLE class.

    Verdict evidence is checked first and always wins over transport-error
    text, so a retry can never replace a verdict with a later pass:

    1. any server-logged outcome other than NoError for this attempt;
    2. a Quint `--out` status of "violation" (even with `errors: []`), any
       non-empty `errors`, or a counterexample/violation marker in the output;
    3. only then: timeouts (final) and the no-verdict retry classes.

    A non-ok Quint status without diagnostics (e.g. `failure` with
    `errors: []`) is retryable only when the server attests there was no
    verdict: it logged NoError for this attempt or never began a check.
    """

    outcomes = list(attempt.server_outcomes)
    if attempt.undelivered_outcome is not None and attempt.undelivered_outcome not in outcomes:
        outcomes.append(attempt.undelivered_outcome)
    if any(outcome != "NoError" for outcome in outcomes):
        return "verdict"
    server_attests_no_verdict = bool(outcomes) or not attempt.check_started

    status = result.get("status") if result is not None else None
    errors = result.get("errors") if result is not None else None
    if status == "violation" or errors:
        return "verdict"
    if any(marker in attempt.output for marker in VERDICT_MARKERS):
        return "verdict"
    if result is not None and status not in (None, "ok") and not server_attests_no_verdict:
        return "verdict"

    if attempt.stalled:
        return "server-stall"
    if attempt.undelivered_outcome is not None:
        # Only a NoError outcome reaches here; the server finished cleanly but
        # Quint never received the response.
        return "lost-response"
    if attempt.timed_out:
        return "timeout"
    if status == "ok" and errors == [] and attempt.returncode == 0:
        return "success"
    if attempt.returncode == 0:
        # A clean client exit without a successful --out result is not evidence.
        return "no-result" if expects_result else "success"
    if SERVER_ERROR_PATTERN.search(attempt.output):
        return "server"
    if expects_result and (result is None or (status not in (None, "ok") and errors == [])):
        # No result, or a failure response without any diagnostic, while the
        # server attests no verdict for this attempt.
        return "no-result"
    return "failure"


def run_attempt(
    number: int,
    argv: Sequence[str],
    server: ApalacheServer,
    cwd: pathlib.Path,
    environment: dict[str, str],
    timeout_seconds: float,
    emit: Callable[[str], None],
) -> Attempt:
    command = [*argv, "--server-endpoint", f"127.0.0.1:{server.port}"]
    checks_before = server.checks_logged()
    outcomes_before = len(server.outcomes_logged())
    outcome_seen_at: float | None = None
    undelivered_outcome: str | None = None
    began = time.monotonic()
    chunks: list[str] = []
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        start_new_session=True,
    )

    def pump() -> None:
        assert process.stdout is not None
        for line in process.stdout:
            chunks.append(line)
            emit(line.rstrip("\n"))

    reader = threading.Thread(target=pump, daemon=True)
    reader.start()
    timed_out = False
    stalled = False
    while process.poll() is None:
        elapsed = time.monotonic() - began
        if elapsed >= timeout_seconds:
            timed_out = True
            break
        if (
            elapsed >= CHECK_START_WATCHDOG_SECONDS
            and server.checks_logged() == checks_before
            and not chunks
        ):
            # Quint has been connected for the whole watchdog window, printed
            # nothing, and the server never began checking: the request was
            # lost before any model checking happened.
            stalled = True
            break
        outcomes = server.outcomes_logged()
        if len(outcomes) > outcomes_before:
            if outcome_seen_at is None:
                outcome_seen_at = time.monotonic()
            elif time.monotonic() - outcome_seen_at >= RESPONSE_GRACE_SECONDS:
                # The server finished this check but its gRPC response never
                # reached Quint (observed on CI: the write was still pending
                # when the connection closed minutes later).
                undelivered_outcome = outcomes[outcomes_before]
                emit(
                    f"[lost-response] server outcome {undelivered_outcome} was not delivered "
                    f"to Quint within {RESPONSE_GRACE_SECONDS}s"
                )
                break
        time.sleep(0.5)
    kill_group(process)
    reader.join(timeout=10)
    check_started = server.checks_logged() > checks_before
    outcomes = server.outcomes_logged()[outcomes_before:]
    if check_started and not outcomes and not timed_out and server.alive():
        # Quint is gone (success, transport error, or no result) but the server
        # began this check and has not logged its outcome yet. A retry must not
        # race a verdict the server is still computing: wait for the outcome
        # within the remaining attempt budget. No outcome in time is a timeout.
        emit("[harness] waiting for the server outcome of the check it began")
        while not outcomes:
            if time.monotonic() - began >= timeout_seconds or not server.alive():
                timed_out = server.alive()
                break
            time.sleep(0.5)
            outcomes = server.outcomes_logged()[outcomes_before:]
    return Attempt(
        number=number,
        port=server.port,
        returncode=None if (timed_out or stalled or undelivered_outcome) else process.returncode,
        timed_out=timed_out,
        stalled=stalled,
        undelivered_outcome=undelivered_outcome,
        seconds=time.monotonic() - began,
        output="".join(chunks),
        check_started=check_started,
        server_outcomes=outcomes,
    )


def run_quint_apalache(
    argv: Sequence[str],
    *,
    cwd: pathlib.Path | str | None = None,
    environment: dict[str, str] | None = None,
    timeout_seconds: float = DEFAULT_ATTEMPT_TIMEOUT_SECONDS,
    quint_home: pathlib.Path | None = None,
    result_root: pathlib.Path | None = None,
    label: str = "quint",
    emit: Callable[[str], None] | None = None,
) -> QuintRun:
    """Run `argv` (a `quint verify ... --backend apalache` command) under the policy."""

    argv = list(argv)
    if len(argv) < 2 or pathlib.Path(argv[0]).name != "quint" or argv[1] != "verify":
        raise HarnessError(f"not a quint verify command: {argv!r}")
    if any(a == "--server-endpoint" or a.startswith("--server-endpoint=") for a in argv):
        raise HarnessError("the harness owns --server-endpoint; do not pass it")
    work = pathlib.Path(cwd) if cwd is not None else pathlib.Path.cwd()
    environment = scrubbed_environment(environment)
    home = quint_home or pathlib.Path(environment.get("QUINT_HOME", str(default_quint_home())))
    environment["QUINT_HOME"] = str(home)
    result_path = out_path(argv, work)
    if result_path is not None and result_root is not None:
        resolved = pathlib.Path(os.path.abspath(result_path))
        try:
            resolved.relative_to(result_root.resolve())
            resolved.parent.resolve().relative_to(result_root.resolve())
        except ValueError as error:
            raise HarnessError(f"refusing Quint --out outside {result_root}: {resolved}") from error
    output_lines: list[str] = []

    def log(line: str) -> None:
        output_lines.append(line)
        (emit or print)(line)

    def clear_result() -> None:
        if result_path is None:
            return
        if result_path.is_dir() and not result_path.is_symlink():
            raise HarnessError(f"refusing to clear Quint result directory: {result_path}")
        result_path.unlink(missing_ok=True)

    attempts: list[Attempt] = []
    server = ApalacheServer(home, environment, log)
    try:
        clear_result()
        server.start()
        for number in range(1, MAX_ATTEMPTS + 1):
            log(f"[attempt {number}/{MAX_ATTEMPTS}] {label} on 127.0.0.1:{server.port} (timeout {timeout_seconds:.0f}s)")
            attempt = run_attempt(number, argv, server, work, environment, timeout_seconds, log)
            attempt.classification = classify(attempt, result_path)
            attempts.append(attempt)
            log(
                f"[attempt {number}/{MAX_ATTEMPTS}] {label}: {attempt.classification} "
                f"(exit={attempt.returncode}, {attempt.seconds:.1f}s)"
            )
            if attempt.classification not in RETRYABLE and attempt.classification != "timeout":
                break
            if attempt.classification == "timeout":
                log(f"[timeout] {label} exceeded {timeout_seconds:.0f}s; timeouts are not retried")
                log(server.log_tail())
                break
            if number == MAX_ATTEMPTS:
                log(server.log_tail())
                break
            # Retryable: no verdict was produced.
            log(f"[retry] {label} retrying once after a '{attempt.classification}' result without a verdict")
            clear_result()
            reuse = attempt.classification == "server" and server.alive() and server.checks_logged() == 0
            if not reuse:
                server.stop()
                server.start()
            else:
                log(f"[retry] reusing warmed server on 127.0.0.1:{server.port}")
    finally:
        server.stop()

    last = attempts[-1]
    if last.classification == "success":
        returncode = 0
    elif last.timed_out:
        returncode = 124
    elif last.returncode not in (None, 0):
        returncode = last.returncode
    else:
        returncode = 1
    return QuintRun(returncode=returncode, output="\n".join(output_lines), attempts=attempts)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--timeout", type=float, default=DEFAULT_ATTEMPT_TIMEOUT_SECONDS,
                        help="per-attempt wall-clock timeout in seconds")
    parser.add_argument("--label", default="quint")
    parser.add_argument("--result-root", type=pathlib.Path,
                        help="refuse a --out path outside this directory")
    parser.add_argument("command", nargs=argparse.REMAINDER,
                        help="-- quint verify <model> --backend apalache ...")
    arguments = parser.parse_args()
    command = arguments.command
    if command and command[0] == "--":
        command = command[1:]
    try:
        run = run_quint_apalache(
            command,
            timeout_seconds=arguments.timeout,
            result_root=arguments.result_root,
            label=arguments.label,
            emit=lambda line: print(line, flush=True),
        )
    except HarnessError as error:
        print(f"[harness] {error}", file=sys.stderr, flush=True)
        return 125
    return run.returncode


if __name__ == "__main__":
    raise SystemExit(main())
