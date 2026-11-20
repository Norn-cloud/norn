#!/usr/bin/env python3
"""Host conformance suite for the Norn cell Worker (D122, #1130, #786).

Runs the same request script on a Workers host and compares it with the
native reference (crates/norn-cell-do/tests/conformance_reference.rs), then
checks the Durable Object properties the cell relies on: transaction
rollback, kills at sampled SQL boundaries inside a commit's transaction (each
boundary proven reached out of band before the kill; no marker, no kill),
acknowledged commits surviving a kill, no interleaving inside a synchronous
turn, one live instance per id, platform alarm probes, SQLite behaviour and
limits, the clock inside a synchronous call, waitUntil background work after
the reply, live WebSockets (and whether the object hibernated), and backup and
restore.

Every row is pass, fail, not tested (the run could not show it) or measured
(an observation, not a claim). A row passes only when what it names was shown.

    conformance.py --reference REF --host celld  --project DIR [--out OUT]
    conformance.py --reference REF --host rivet  --project DIR [--out OUT]
    NORN_CELL_RIVET=BIN conformance.py --reference REF --host rivet-native [--out OUT]
    conformance.py --reference REF --host url --url https://cell.example [--out OUT]

`celld` and `rivet` start and stop the host themselves (state in a fresh temp directory, or
an empty real --state outside the project); `url` runs only the tests that need no host control. DIR is a
built conformance project (see apps/norn-cell-do/README.md); it lives outside
the checkout. Pins: celld 0.6.3, open-orange 1.0.3 (installed from
conformance/package.json + bun.lock) with Rivet Engine 3.1.2-rc.1
(`rivetdev/engine` by digest).
"""
import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import ssl
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

ENGINE_IMAGE = "rivetdev/engine@sha256:6e585002b700dd4d3630808eb67884aa15d1a602d83781984edac47023b6d40f"
# Rivet Engine 3.1.1 (`rivetdev/engine:3.1.1`), the release the native host's RivetKit pins.
NATIVE_ENGINE_IMAGE = "rivetdev/engine@sha256:55cdfe9a92c0ee2f7d3ce5f388dab2ef8e0be1bab8e3f1adb7ba972f862e25d4"
ORANGE_VERSION = "1.0.3"
# The locked, test-only OpenOrange install (bun install --frozen-lockfile in this directory).
LOCKED_ORANGE = Path(__file__).resolve().parent / "node_modules" / ".bin" / "orange"
NOW_HEADER = "x-norn-conformance-now"
HOLD_HEADER = "x-norn-conformance-hold"
MARKER_HEADER = "x-norn-conformance-marker"
MARKER_LINE = "norn-conformance-hold-reached"
REFERENCE_FORMAT = "norn/host-conformance-reference@2"
SCRIPT_STEPS = 12
ADMIN = "Bearer fixture:admin@demo"
ANALYTICS = "orgId=org_demo&environmentId=analytics"
# A descriptive client name: workers.dev refuses urllib's default User-Agent (error 1010).
USER_AGENT = "norn-conformance/1"


# ------------------------------------------------------------------ http


def request(base, method, path, query="", body=None, headers=None, timeout=60):
    url = f"{base}{path}" + (f"?{query}" if query else "")
    data = body.encode() if isinstance(body, str) and method != "GET" else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("user-agent", USER_AGENT)
    for name, value in (headers or {}).items():
        req.add_header(name, value)
    if data is not None:
        req.add_header("content-type", "application/json")
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            status, payload, got = response.status, response.read(), dict(response.headers)
    except urllib.error.HTTPError as error:
        status, payload, got = error.code, error.read(), dict(error.headers)
    return status, payload, (time.perf_counter() - started) * 1000, got


def probe(base, route, object_name="probe", timeout=60, **params):
    query = urllib.parse.urlencode({"object": object_name, **{k: str(v) for k, v in params.items()}})
    # A host re-placing the object after a crash answers 503/504 from its gateway until it is back;
    # recovery time is measured elsewhere, so a probe waits it out (up to two minutes).
    stop_at = time.time() + 120
    while True:
        status, body, _, _ = request(base, "GET", f"/__probe/{route}", query, timeout=timeout)
        if status not in (503, 504) or time.time() > stop_at:
            break
        time.sleep(1)
    try:
        document = json.loads(body)
    except ValueError:
        return status, {"raw": body[:300].decode(errors="replace")}
    return status, document if isinstance(document, dict) else {"raw": document}


def probed(base, route, *keys, **kwargs):
    """A probe that must answer 200 with a JSON object carrying `keys` and no error, or raise."""
    status, body = probe(base, route, **kwargs)
    missing = [key for key in keys if key not in body]
    if status != 200 or "error" in body or missing:
        raise RuntimeError(f"probe {route} answered {status} (missing {missing}): {str(body.get('error') or body)[:200]}")
    return body


def at(query, obj):
    return query if obj is None else f"{query}&conformanceObject={obj}"


def head(base, query=ANALYTICS, timeout=120, obj=None):
    """The cell's verified head as the exact string the Rust cell serialized, its document, and the time taken."""
    query = at(query, obj)
    status, body, ms, _ = request(base, "GET", "/__conformance/head", query, timeout=timeout)
    if status != 200:
        raise RuntimeError(f"head: {status} {body[:300]!r}")
    document = json.loads(body)
    if not isinstance(document.get("headJson"), str):
        raise RuntimeError(f"head: no headJson string in {body[:300]!r}")
    return document["headJson"], document, ms


def recovered_head(base, obj=None, deadline_s=300):
    """The head once the cell answers again after a crash, and how long that took (ms).

    A host may refuse while it re-places the cell (Rivet: 504 until the dead runner's epoch is
    declared lost); only an answer counts, and the wait is reported as recovery time.
    """
    started = time.perf_counter()
    stop_at = time.time() + deadline_s
    while True:
        try:
            value, document, _ = head(base, obj=obj, timeout=120)
            return value, (time.perf_counter() - started) * 1000
        except (RuntimeError, OSError) as error:
            if time.time() > stop_at:
                raise RuntimeError(f"no recovery within {deadline_s} s: {error}") from error
            time.sleep(1)


def scripted(base, step, now_ms, hold=None, timeout=120, obj=None, marker=None):
    headers = {"authorization": step["authorization"], NOW_HEADER: str(now_ms)}
    if hold:
        headers[HOLD_HEADER] = hold
    if marker:
        headers[MARKER_HEADER] = marker
    return request(base, step["method"], step["path"], at(step["query"], obj), step["body"] or None, headers, timeout)


def wait_ready(base, deadline_s, child=None):
    stop_at = time.time() + deadline_s
    while time.time() < stop_at:
        if child is not None and child.poll() is not None:
            raise RuntimeError("host exited during startup")
        try:
            status, _, _, _ = request(base, "GET", "/__probe/identity", "object=ready", timeout=10)
            if status == 200:
                return
        except OSError:
            pass
        time.sleep(0.25)
    raise RuntimeError(f"host not ready within {deadline_s} s")


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def pss_kib(pids):
    """Proportional set size of `pids`, KiB (smaps_rollup; root-owned processes read via sudo)."""
    total = 0
    for pid in pids:
        path = f"/proc/{pid}/smaps_rollup"
        try:
            text = Path(path).read_text()
        except PermissionError:
            text = subprocess.run(["sudo", "-n", "cat", path], capture_output=True, text=True).stdout
        except OSError:
            continue
        for line in text.splitlines():
            if line.startswith("Pss:"):
                total += int(line.split()[1])
    return total


def tree(pid):
    """`pid` and its descendants."""
    found, frontier = [], [pid]
    while frontier:
        current = frontier.pop()
        found.append(current)
        try:
            children = Path(f"/proc/{current}/task/{current}/children").read_text().split()
        except OSError:
            children = []
        frontier.extend(int(child) for child in children)
    return found


def cpu_ticks(pids):
    total = 0
    for pid in pids:
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            pass
    return total


def kill_group(child):
    if child is None or child.poll() is not None:
        return
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    child.wait(timeout=30)


# ------------------------------------------------------------------ hosts


class UrlHost:
    """Any deployed host, by URL: no lifecycle control."""

    name = "url"
    controls = False
    runners = 1

    def __init__(self, url):
        self.url = url.rstrip("/")

    def urls(self):
        return [self.url]

    def logs(self):
        return []

    def start(self):
        wait_ready(self.url, 180)

    def stop(self):
        pass


class CelldHost:
    """`celld dev` (0.6.3) over a state directory, killed by process group."""

    name = "celld"
    controls = True
    runners = 1

    def __init__(self, project, state):
        self.project, self.state = project, state
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.child = None
        version = subprocess.check_output(["celld", "--version"], text=True).strip()
        assert version == "celld 0.6.3", version
        self.version = version

    def urls(self):
        return [self.url]

    def logs(self):
        return [self.state / "celld.log"]

    def start(self):
        started = time.perf_counter()
        log = open(self.state / "celld.log", "ab")
        # --no-watch: a file written under the project must not rebuild and restart the host mid-test;
        # --logs: the Worker's console (the hold markers) reaches the log the runner reads.
        self.child = subprocess.Popen(["celld", "dev", "--port", str(self.port), "--no-watch", "--logs"], cwd=self.project, stdout=log, stderr=log, start_new_session=True)
        wait_ready(self.url, 120, self.child)
        return (time.perf_counter() - started) * 1000

    def kill(self):
        kill_group(self.child)

    stop = kill

    def pids(self):
        return tree(self.child.pid) if self.child else []

    def data(self):
        return [self.project / ".celld"]


def orange_version(binary):
    """`binary`'s version, refused unless it is exactly the pinned open-orange release.

    The binary must report exactly one version, ORANGE_VERSION; when it is the locked install's
    shim, the installed package's own manifest must say the same.
    """
    out = subprocess.check_output([binary, "--version"], text=True, stderr=subprocess.STDOUT).strip()
    versions = re.findall(r"\b\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?\b", out)
    if versions != [ORANGE_VERSION]:
        raise SystemExit(f"{binary} reports {out!r}; the suite runs only open-orange {ORANGE_VERSION} (bun install --frozen-lockfile in {LOCKED_ORANGE.parents[2]})")
    manifest = Path(binary).resolve().parent.parent / "package.json"
    if manifest.is_file():
        package = json.loads(manifest.read_text())
        if package.get("name") != "open-orange" or package.get("version") != ORANGE_VERSION:
            raise SystemExit(f"{binary} belongs to {package.get('name')} {package.get('version')}, not open-orange {ORANGE_VERSION}")
    return f"open-orange {ORANGE_VERSION}"


class RivetHost:
    """A self-hosted Rivet Engine (Docker, pinned digest) with `orange serve` runners connecting out to it."""

    name = "rivet"
    controls = True

    def __init__(self, project, state, runners=2):
        self.project, self.state = project, state
        self.runners = runners
        self.token = secrets.token_hex(24)
        self.ports = [free_port() for _ in range(runners)]
        self.children = [None] * runners
        self.container = f"norn-conformance-engine-{os.getpid()}"
        self.orange = os.environ.get("ORANGE", str(LOCKED_ORANGE))
        self.version = orange_version(self.orange)
        config = {
            "storage": {"log_directory": "/var/lib/rivet/log", "auto_provision_partitions": 1, "balancing": {"enabled": False}},
            "object_storage": {"primary": {"file_system": {"path": "/var/lib/rivet/objects"}}},
        }
        (state / "engine").mkdir(parents=True, exist_ok=True)
        (state / "engine" / "config.json").write_text(json.dumps(config))

    def urls(self):
        return [f"http://127.0.0.1:{port}" for port in self.ports]

    def logs(self):
        return [self.state / f"runner-{index}.log" for index in range(self.runners)]

    def start_engine(self):
        subprocess.run(["docker", "rm", "-f", self.container], capture_output=True)
        engine = self.state / "engine"
        subprocess.run([
            "docker", "run", "-d", "--name", self.container, "--network", "host",
            "-e", f"RIVET__AUTH__ADMIN_TOKEN={self.token}",
            "-v", f"{engine / 'config.json'}:/etc/rivet/config.json:ro",
            "-v", f"{engine / 'log'}:/var/lib/rivet/log",
            "-v", f"{engine / 'objects'}:/var/lib/rivet/objects",
            getattr(self, "image", ENGINE_IMAGE),
        ], check=True, capture_output=True)
        stop_at = time.time() + 120
        while time.time() < stop_at:
            try:
                status, _, _, _ = request("http://127.0.0.1:6420", "GET", "/health", timeout=5)
                if status == 200:
                    break
            except OSError:
                pass
            time.sleep(0.5)
        else:
            raise RuntimeError("engine not healthy")
        # /health answers before storage routing is up; the pool write is retried until it is.
        stop_at = time.time() + 120
        while True:
            status, body, _, _ = request("http://127.0.0.1:6420", "PUT", "/pools/default", "namespace=default", "{}", {"authorization": f"Bearer {self.token}"})
            if status < 300:
                break
            if time.time() > stop_at or b"retry" not in body:
                raise RuntimeError(f"pool: {status} {body[:200]}")
            time.sleep(0.5)

    def start_runner(self, index):
        env = {**os.environ, "RIVET_ENDPOINT": f"http://default:{self.token}@127.0.0.1:6420"}
        log = open(self.state / f"runner-{index}.log", "ab")
        self.children[index] = subprocess.Popen([self.orange, "serve", "--port", str(self.ports[index]), "--ip", "127.0.0.1"], cwd=self.project, env=env, stdout=log, stderr=log, start_new_session=True)

    def start(self):
        started = time.perf_counter()
        self.start_engine()
        for index in range(self.runners):
            self.start_runner(index)
        for index, url in enumerate(self.urls()):
            wait_ready(url, 180, self.children[index])
        return (time.perf_counter() - started) * 1000

    def kill_runner(self, index):
        kill_group(self.children[index])
        self.children[index] = None

    def kill(self):
        for index in range(self.runners):
            self.kill_runner(index)
        subprocess.run(["docker", "kill", self.container], capture_output=True)

    def stop(self):
        self.kill()
        subprocess.run(["docker", "rm", "-f", self.container], capture_output=True)

    def engine_pid(self):
        out = subprocess.run(["docker", "inspect", "-f", "{{.State.Pid}}", self.container], capture_output=True, text=True).stdout.strip()
        return int(out) if out.isdigit() and out != "0" else None

    def pids(self):
        pids = [pid for child in self.children if child is not None for pid in tree(child.pid)]
        engine = self.engine_pid()
        return pids + (tree(engine) if engine else [])

    def data(self):
        return [self.state / "engine" / "log", self.state / "engine" / "objects"]


class RivetNativeHost(RivetHost):
    """The native Rivet host (crates/norn-cell-rivet, D131): a self-hosted Rivet Engine 3.1.1 (Docker,
    pinned digest) with `norn-cell-rivet` runner processes, RivetKit's Rust SDK with the cell's SQLite
    in process. The binary is a `conformance` build, named by NORN_CELL_RIVET."""

    name = "rivet-native"

    def __init__(self, state, lineage_key, runners=2):
        self.project, self.state = None, state
        self.runners = runners
        self.lineage_key = lineage_key
        self.token = secrets.token_hex(24)
        self.ports = [free_port() for _ in range(runners)]
        self.children = [None] * runners
        self.container = f"norn-conformance-engine-{os.getpid()}"
        self.binary = os.environ.get("NORN_CELL_RIVET")
        if not self.binary or not Path(self.binary).is_file():
            raise SystemExit("NORN_CELL_RIVET names the norn-cell-rivet binary (cargo build --features conformance in crates/norn-cell-rivet)")
        self.image = NATIVE_ENGINE_IMAGE
        self.version = f"norn-cell-rivet (rivetkit 3.1.1) on {NATIVE_ENGINE_IMAGE.split('@')[1][:19]}…"
        config = {
            "storage": {"log_directory": "/var/lib/rivet/log", "auto_provision_partitions": 1, "balancing": {"enabled": False}},
            "object_storage": {"primary": {"file_system": {"path": "/var/lib/rivet/objects"}}},
        }
        (state / "engine").mkdir(parents=True, exist_ok=True)
        (state / "engine" / "config.json").write_text(json.dumps(config))

    def start_runner(self, index):
        env = {
            **os.environ,
            "RIVET_ENDPOINT": f"http://default:{self.token}@127.0.0.1:6420",
            "NORN_LISTEN": f"127.0.0.1:{self.ports[index]}",
            "NORN_STAGE": "test",
            "NORN_CONFORMANCE": "synthetic",
            "NORN_LINEAGE_KEY": self.lineage_key,
        }
        log = open(self.state / f"runner-{index}.log", "ab")
        self.children[index] = subprocess.Popen([self.binary], env=env, stdout=log, stderr=log, start_new_session=True)


# ------------------------------------------------------------------ tests


class Suite:
    def __init__(self, host, reference, quick=False):
        self.host, self.ref, self.quick = host, reference, quick
        self.base = host.urls()[0]
        self.results, self.measurements = [], {}
        self.now = reference["nowMs"]

    def record(self, name, status, detail):
        print(f"[{self.host.name}] {name}: {status} - {detail}", flush=True)
        self.results.append({"test": name, "status": status, "detail": detail})

    def run(self, name, fn, needs_control=False):
        if needs_control and not self.host.controls:
            return self.record(name, "not tested", "needs host lifecycle control")
        try:
            status, detail = fn()
        except Exception as error:  # noqa: BLE001 - every failure is a row in the table
            status, detail = "fail", f"{type(error).__name__}: {error}"[:600]
            if self.host.controls:
                # A test that failed between a kill and a restart must not fail every row after it.
                try:
                    self.restart()
                except Exception as restart_error:  # noqa: BLE001
                    detail += f"; the host did not restart: {restart_error}"[:300]
        self.record(name, status, detail)

    def restart(self):
        self.host.kill()
        return self.host.start()

    # -- differential

    def differential(self):
        native = self.ref["native"]
        script = self.ref["script"]
        if len(script) != SCRIPT_STEPS or len(native["steps"]) != SCRIPT_STEPS:
            return "fail", f"the reference must hold exactly {SCRIPT_STEPS} steps per side: script {len(script)}, native {len(native['steps'])}"
        opened, _, _ = head(self.base)
        diffs = []
        if opened.encode() != native["openedJson"].encode():
            diffs.append(f"opened {opened} != {native['openedJson']}")
        statements, ran = [], 0
        for index, (step, want) in enumerate(zip(script, native["steps"], strict=True)):
            status, body, _, got = scripted(self.base, step, self.now)
            statements.append(int(header(got, "x-norn-conformance-statements") or 0))
            now_head, _, _ = head(self.base)
            ran += 1
            if status != want["status"]:
                diffs.append(f"step {index} status {status} != {want['status']}: {body[:200]!r}")
            elif body != want["body"].encode():
                diffs.append(f"step {index} body differs (sha256 {sha(body)} != {sha(want['body'].encode())})")
            if now_head.encode() != want["headJson"].encode():
                diffs.append(f"step {index} head {now_head} != {want['headJson']}")
        if ran != SCRIPT_STEPS:
            diffs.append(f"ran {ran} of {SCRIPT_STEPS} steps")
        self.statements = max(statements) if statements else 0
        final = native["steps"][-1]["head"]
        if diffs:
            return "fail", "; ".join(diffs[:6])
        return "pass", f"{ran}/{SCRIPT_STEPS} steps byte-identical (status, raw body bytes, Rust-serialized verified head); final head seq={final['seq']} commits={final['commits']} hash={final['hash'][:16]}…; ≤{self.statements} SQL statements per commit"

    # -- transactions

    def tx_rollback(self):
        adapter = probed(self.base, "adapter", "result")
        thrown = probed(self.base, "tx-throw", "rolledBack", "rethrown")
        ok = str(adapter["result"]).endswith(":pass") and thrown["rolledBack"] is True and thrown["rethrown"] is True
        return ("pass" if ok else "fail"), f"adapter={adapter['result']}; thrown string rolled back={thrown['rolledBack']} rethrown unchanged={thrown['rethrown']}"

    def tx_nested(self):
        adapter = probed(self.base, "adapter", "result")
        nested = probed(self.base, "tx-nested", "inner", "outerKept")
        ok = str(adapter["result"]).endswith(":pass") and "nested" in str(adapter["result"])
        return ("pass" if ok else "fail"), f"Norn refuses nesting before the platform (adapter={adapter['result']}); platform nesting (measured): inner error {nested['inner']!r}, outer kept={nested['outerKept']}"

    # -- kills

    def wait_marker(self, token, replied, seconds=60):
        """Whether the host printed `token`'s hold marker (read from its own log) before any reply left."""
        line = f"{MARKER_LINE} {token}".encode()
        stop_at = time.time() + seconds
        while time.time() < stop_at and not replied:
            for path in self.host.logs():
                try:
                    if line in path.read_bytes():
                        return True
                except OSError:
                    pass
            time.sleep(0.2)
        return False

    def kill_points(self):
        """Kills at sampled SQL boundaries inside a commit's transaction: each boundary is proven
        reached (the host printed the request's marker from inside the write transaction while the
        object spins there) before the host is killed. The commit must then be either wholly absent
        or wholly present, never partial, and a retry must match native. Without a marker there is
        no kill: the boundary is not tested."""
        steps = self.ref["kill"]["script"]
        native = self.ref["kill"]["native"]
        obj = f"kill-{secrets.token_hex(4)}"
        opened, _, _ = head(self.base, obj=obj, timeout=300)
        if opened != native["openedJson"]:
            return "fail", f"a fresh cell opened at {opened}, native {native['openedJson']}"
        # Size the sweep by the kill request itself: a dry run on its own fresh cell.
        dry = f"kill-dry-{secrets.token_hex(4)}"
        head(self.base, obj=dry, timeout=300)  # provisioning is not part of the count
        status, _, _, got = scripted(self.base, steps[0], self.now, obj=dry, timeout=300)
        counted = header(got, "x-norn-conformance-statements")
        if status != 200 or not (counted or "").isdigit() or int(counted) < 1:
            return "fail", f"the dry run answered {status} with statement count {counted!r}"
        n = int(counted)
        spread = sorted({1, 2, 3, max(1, n // 4), max(1, n // 2), max(1, (3 * n) // 4), max(1, n - 1), n})
        if self.quick:
            spread = [1, n]
        points = [f"stmt:{k}" for k in spread] + ["turn-end"]
        if len(points) > len(steps):
            return "fail", f"{len(points)} kill points but only {len(steps)} kill steps in the reference"
        rows, failures, untested, reopen_ms = [], [], [], []
        before = native["openedJson"]
        wedged = None
        for index, point in enumerate(points):
            if wedged:
                untested.append(point)
                rows.append(f"{point}=not reached, not killed (the cell stopped answering after {wedged})")
                continue
            step, want = steps[index], native["steps"][index]
            token = secrets.token_hex(16)
            replied = []

            def send():
                try:
                    status, _, _, got = scripted(self.base, step, self.now, hold=point, timeout=600, obj=obj, marker=token)
                    replied.append((status, header(got, "x-norn-conformance-hold-reached")))
                except OSError as error:
                    replied.append((f"error: {type(error).__name__}", None))

            with concurrent.futures.ThreadPoolExecutor(1) as pool:
                future = pool.submit(send)
                reached = self.wait_marker(token, replied)
                if reached:
                    early = list(replied)
                    self.host.kill()
                # Unreached, the request is never killed: it answers on its own (or times out).
                future.result(timeout=120 if reached else 900)
            if reached:
                self.host.start()
                after_kill, waited = recovered_head(self.base, obj=obj)
                reopen_ms.append(waited)
            else:
                # No marker, no kill, and no restart either (a reset connection included): the
                # boundary is not tested. A cell that no longer answers ends the sweep untested.
                untested.append(point)
                try:
                    after_kill, _, _ = head(self.base, obj=obj)
                except (RuntimeError, OSError, ValueError) as error:
                    wedged = point
                    rows.append(f"{point}=not reached, not killed (reply {replied[0][0] if replied else None}; then head: {type(error).__name__})")
                    continue
            landed = after_kill == want["headJson"]
            if not landed and after_kill != before:
                failures.append(f"{point}: head {after_kill} is neither {before} nor {want['headJson']} (partial)")
            if reached and early:
                failures.append(f"{point}: a reply ({early[0][0]}, hold reached={early[0][1]}) left while the object was held")
            if reached and landed:
                failures.append(f"{point}: a write inside an open transaction survived the kill")
            if not landed:
                try:
                    status, body, _, _ = scripted(self.base, step, self.now, obj=obj)
                    retried, _, _ = head(self.base, obj=obj)
                except (RuntimeError, OSError, ValueError) as error:
                    if reached:
                        raise
                    wedged = point
                    rows.append(f"{point}=not reached, not killed (then the retry: {type(error).__name__})")
                    continue
                if (status, body) != (want["status"], want["body"].encode()) or retried != want["headJson"]:
                    failures.append(f"{point}: retry differs from native ({status})")
            rows.append(f"{point}={'not reached, not killed (reply ' + str(replied[0][0] if replied else None) + ')' if not reached else 'landed' if landed else 'rolled back'}")
            before = want["headJson"]
        detail = f"{len(points)} sampled boundaries of ≤{n} SQL statements, each killed only after its out-of-band marker: " + ", ".join(rows)
        if reopen_ms:
            self.measurements["cell_recovery_after_kill_ms"] = summary(reopen_ms)
            detail += f"; the cell answered again p50 {statistics.median(reopen_ms):.0f} ms after the host restarted"
        if failures:
            return "fail", "; ".join(failures)[:900]
        if untested:
            if wedged:
                detail += f"; the cell stopped answering after {wedged}, so the sweep ended there without a kill"
            return "not tested", f"the host never showed reaching {', '.join(untested)} (no marker in its log), so those boundaries were not killed; " + detail
        return "pass", detail

    def ack_kill(self):
        """An acknowledged commit survives a kill of the host right after its reply."""
        steps = self.ref["kill"]["script"]
        native = self.ref["kill"]["native"]
        obj = f"ack-{secrets.token_hex(4)}"
        opened, _, _ = head(self.base, obj=obj, timeout=300)
        if opened != native["openedJson"]:
            return "fail", f"a fresh cell opened at {opened}, native {native['openedJson']}"
        want = native["steps"][0]
        status, body, _, _ = scripted(self.base, steps[0], self.now, obj=obj)
        if (status, body) != (want["status"], want["body"].encode()):
            return "fail", f"the commit answered {status}, not native's {want['status']}"
        self.host.kill()
        self.host.start()
        survived, waited = recovered_head(self.base, obj=obj)
        if survived != want["headJson"]:
            return "fail", f"acknowledged commit lost: head {survived} != {want['headJson']}"
        return "pass", f"acknowledged install (200) then the host was killed: the head after restart is the acknowledged one (answered again after {waited:.0f} ms)"

    # -- interleaving

    def interleaving(self):
        tag = f"turn-{secrets.token_hex(4)}"
        seen = []

        def writer():
            return probe(self.base, "turn", n=50, ms=400, tag=tag)[0]

        def reader():
            out = []
            for _ in range(20):
                status, body = probe(self.base, "count", tag=tag)
                out.append(body.get("count") if status == 200 else f"{status}: {str(body.get('error') or body)[:120]}")
            return out

        with concurrent.futures.ThreadPoolExecutor(6) as pool:
            writes = [pool.submit(writer) for _ in range(3)]
            reads = [pool.submit(reader) for _ in range(3)]
            for future in reads:
                seen.extend(future.result())
            statuses = [future.result() for future in writes]
        errors = [count for count in seen if not isinstance(count, int)]
        partial = [count for count in seen if isinstance(count, int) and count % 50 != 0]
        ok = not partial and not errors and statuses == [200, 200, 200]
        observed = sorted({count for count in seen if isinstance(count, int)})
        return ("pass" if ok else "fail"), f"{len(seen)} concurrent reads during three 50-row synchronous turns: observed {observed}; partial={partial[:5]}; writer statuses {statuses}; errors={errors[:3]}"

    def concurrent_commits(self):
        steps = self.ref["concurrent"]
        _, before, _ = head(self.base)
        with concurrent.futures.ThreadPoolExecutor(len(steps)) as pool:
            statuses = list(pool.map(lambda step: scripted(self.base, step, self.now)[0], steps))
        _, after, _ = head(self.base)
        added = after["head"]["commits"] - before["head"]["commits"]
        ok = all(status == 200 for status in statuses) and added == len(steps)
        return ("pass" if ok else "fail"), f"{len(steps)} concurrent installs: statuses {sorted(statuses)}; commits +{added}; ledger verifies"

    # -- one live instance

    def single_instance(self):
        urls = self.host.urls()
        name = f"single-{secrets.token_hex(4)}"
        with concurrent.futures.ThreadPoolExecutor(16) as pool:
            replies = list(pool.map(lambda i: probed(urls[i % len(urls)], "identity", "instance", "writes", object_name=name), range(64)))
        instances = {reply["instance"] for reply in replies}
        writes = max(reply["writes"] for reply in replies)
        ok = len(instances) == 1 and writes == 64
        detail = f"64 concurrent requests through {len(urls)} runner(s): {len(instances)} instance(s), {writes} serialized writes"
        if self.host.controls:
            self.restart()
            wait_for(lambda: probe(urls[0], "identity", object_name=name)[0] == 200, 300)
            after = {probed(url, "identity", "instance", object_name=name)["instance"] for url in urls for _ in range(4)}
            ok = ok and len(after) == 1
            detail += f"; after restart {len(after)} instance(s)"
        return ("pass" if ok else "fail"), detail

    def zombie_frozen(self):
        """Rivet only: the runner holding a cell is frozen (SIGSTOP) mid-commit until the engine loses it
        and serves the cell elsewhere; a write lands there; then the frozen runner thaws and finishes
        its commit. That commit must not land (the outcome must be `y` alone) and must not be acknowledged."""
        if self.host.runners < 2:
            return "not tested", "needs two runners"
        urls = self.host.urls()
        z = self.ref["zombie"]
        obj = f"frozen-{secrets.token_hex(4)}"
        before, ident, _ = head(urls[0], obj=obj, timeout=300)
        held = []
        token = secrets.token_hex(16)

        def send():
            try:
                held.append(scripted(urls[0], z["x"], self.now, hold="stmt:3:5000", timeout=900, obj=obj, marker=token)[:2])
            except OSError as error:
                held.append((f"error {type(error).__name__}", ""))

        notes = []
        with concurrent.futures.ThreadPoolExecutor(1) as pool:
            future = pool.submit(send)
            if not self.wait_marker(token, held):
                future.result(timeout=900)
                return "not tested", f"the held write never showed reaching its hold (no marker; reply {held[:1]}), so no runner was frozen mid-commit"
            time.sleep(1)
            first = [cpu_ticks(tree(child.pid)) for child in self.host.children]
            time.sleep(1)
            spent = [cpu_ticks(tree(child.pid)) - was for child, was in zip(self.host.children, first)]
            owner = spent.index(max(spent))
            other = 1 - owner
            group = self.host.children[owner].pid
            os.killpg(group, signal.SIGSTOP)
            frozen_at = time.time()
            try:
                # Until the cell answers from a new instance on the other runner (or 180 s pass).
                moved = None
                while time.time() - frozen_at < 180:
                    status, body, _, _ = request(urls[other], "GET", "/__conformance/head", at(ANALYTICS, obj), timeout=60)
                    if status == 200 and json.loads(body).get("instance") != ident.get("instance") and isinstance(json.loads(body).get("headJson"), str):
                        moved = json.loads(body)
                        break
                    notes.append(status)
                    time.sleep(1)
                moved_s = time.time() - frozen_at
                if moved is None:
                    return "fail", f"the cell never moved off frozen runner {owner} in 180 s (answers {sorted(set(notes))})"
                y_status, y_body, _, _ = scripted(urls[other], z["y"], self.now, timeout=300, obj=obj)
            finally:
                os.killpg(group, signal.SIGCONT)
            future.result(timeout=900)
        time.sleep(5)
        final, _ = recovered_head(urls[other], obj=obj)
        held_status = held[0][0] if held else None
        heads = z["heads"]
        outcome = {heads["y"]: "fenced (y only)", heads["xy"]: "x then y", heads["yx"]: "SPLIT BRAIN (y then x)"}.get(final, f"unknown head {final}")
        detail = f"runner {owner} frozen mid-commit; cell served by a new instance on runner {other} after {moved_s:.0f} s (head {'unchanged' if moved['headJson'] == before else moved['headJson']}; earlier answers {sorted(set(notes))}); y → {y_status}; thawed x → {held_status}; outcome: {outcome}"
        ok = moved["headJson"] == before and y_status == 200 and outcome.startswith("fenced") and held_status != 200
        return ("pass" if ok else "fail"), detail

    def zombie(self):
        """Rivet only: the runner holding a cell stalls mid-commit; the cell moves; the stalled instance must not commit."""
        if self.host.runners < 2:
            return "not tested", "needs two runners"
        urls = self.host.urls()
        z = self.ref["zombie"]
        obj = f"zombie-{secrets.token_hex(4)}"
        before, ident, _ = head(urls[0], obj=obj, timeout=300)
        held = []
        token = secrets.token_hex(16)

        def send():
            try:
                held.append(scripted(urls[0], z["x"], self.now, hold="stmt:3:40000", timeout=600, obj=obj, marker=token)[:2])
            except OSError as error:
                held.append((f"error {type(error).__name__}", ""))

        with concurrent.futures.ThreadPoolExecutor(1) as pool:
            future = pool.submit(send)
            if not self.wait_marker(token, held):
                future.result(timeout=600)
                return "not tested", f"the held write never showed reaching its hold (no marker; reply {held[:1]}), so no runner stalled mid-commit"
            time.sleep(1)
            # The runner hosting the cell is the one whose thread spins at the hold.
            first = [cpu_ticks(tree(child.pid)) for child in self.host.children]
            time.sleep(1.5)
            spent = [cpu_ticks(tree(child.pid)) - was for child, was in zip(self.host.children, first)]
            owner = spent.index(max(spent))
            other = 1 - owner
            moved_at = time.time()
            status, body, _, got = scripted(urls[other], z["y"], self.now, timeout=300, obj=obj)
            moved_ms = (time.time() - moved_at) * 1000
            future.result(timeout=600)
        time.sleep(3)
        final, final_doc, _ = head(urls[other], obj=obj, timeout=300)
        held_status = held[0][0] if held else None
        heads = z["heads"]
        outcome = {heads["xy"]: "serialized (x then y)", heads["y"]: "fenced (y only)", heads["yx"]: "SPLIT BRAIN (y then x)"}.get(final, f"unknown head {final}")
        detail = f"owner runner {owner} (CPU ticks {spent}) spun 40 s inside the transaction; write via runner {other} → {status} after {moved_ms:.0f} ms (instance {'moved' if got.get('x-norn-conformance-instance') != ident.get('instance') else 'same'}); held request → {held_status}; outcome: {outcome}"
        if status != 200:
            return "fail", detail + f"; {body[:200]!r}"
        if outcome.startswith("fenced") and held_status == 200:
            return "fail", detail + "; the held request was acknowledged but its write is not in the ledger"
        return ("pass" if outcome.startswith(("serialized", "fenced")) else "fail"), detail

    # -- alarms (platform probes; the cell's own timers are not in this suite yet)

    def fired(self, name, seconds):
        """The alarm-state probe once it records a successful firing (or the last state after `seconds`)."""
        last = {}

        def check():
            nonlocal last
            last = probed(self.base, "alarm-state", "fired", "failed", object_name=name)
            return last if last["fired"] else None

        return wait_for(check, seconds) or last

    def alarms(self):
        notes, ok = [], True
        name = f"alarm-{secrets.token_hex(4)}"
        set_at = probed(self.base, "alarm-set", "at", object_name=name, **{"in": 1500})["at"]
        self.fired(name, 30)
        time.sleep(3)  # a duplicate delivery would land by now
        state = probed(self.base, "alarm-state", "fired", "failed", object_name=name)
        fires = len(state["fired"]) >= 1 and not state["failed"] and state["fired"][0][0] >= set_at
        ok &= fires
        notes.append(f"fires at or after its time: {len(state['fired'])} firing(s) (at-least-once), {len(state['failed'])} failure(s)")

        # Retry: the handler fails its first delivery. Passing needs a successful re-delivery AND evidence
        # that a failed delivery preceded it (a failure row that survived, or the runtime's own retry count).
        name = f"alarm-retry-{secrets.token_hex(4)}"
        probed(self.base, "alarm-set", "at", object_name=name, fail=1, **{"in": 1000})
        state = self.fired(name, 90)
        success = json.loads(state["fired"][0][1]) if state.get("fired") else None
        evidence = []
        if state.get("failed"):
            evidence.append(f"{len(state['failed'])} failed delivery row(s)")
        if success is not None and isinstance(success.get("retryCount"), int) and success["retryCount"] >= 1:
            evidence.append(f"retryCount={success['retryCount']}")
        if success is not None and success.get("isRetry") is True:
            evidence.append("isRetry=true")
        retried = success is not None and bool(evidence)
        ok &= retried
        notes.append(f"failing handler re-fired: {'yes (' + ', '.join(evidence) + ')' if retried else 'no' if success is None else 'fired, but nothing shows a failed delivery preceded it'}")

        rolled = probed(self.base, "alarm-rollback", "alarm", "logged", object_name=f"alarm-rb-{secrets.token_hex(4)}")
        ok &= rolled["alarm"] is None and rolled["logged"] == 0
        notes.append(f"rolled back with its transaction: alarm={rolled['alarm']} rows={rolled['logged']}")
        if self.host.controls:
            name = f"alarm-restart-{secrets.token_hex(4)}"
            probed(self.base, "alarm-set", "at", object_name=name, **{"in": 6000})
            self.host.kill()
            restarted_ms = time.time() * 1000
            self.host.start()
            state = self.fired(name, 120)
            # Only a firing recorded after the restart shows the alarm survived the kill.
            fired = state.get("fired") or []
            after = [row for row in fired if isinstance(row[0], (int, float)) and row[0] >= restarted_ms]
            ok &= bool(after)
            notes.append(f"survives a kill: {'fired after the restart' if after else 'no firing after the restart'} ({len(fired)} firing(s), {time.time() - restarted_ms / 1000:.0f} s after the restart)")
        else:
            notes.append("survives a kill: not tested (no host control)")
        return ("pass" if ok else "fail"), "; ".join(notes)

    # -- sqlite

    def sqlite(self):
        info = probed(self.base, "sqlite", "affinity", "variables100", "variables1000", "statement100k")
        expected = ["integer", "text", "real", "integer", "blob", "text"]
        types = info["affinity"].get("types")
        # One value per commit. The Workers platform guarantees values up to 2 MB, and the cell's
        # provisioning bound is 1 MiB, so 1 MiB must be stored exactly; larger sizes are measured.
        # Every reply must be well formed: accepted with the exact length stored, or a refusal.
        sizes, malformed = {}, []
        for size in (1 << 20, 16 << 20, 130 << 20):
            # A refused value is a well-formed answer here (accepted=false with the platform's error).
            status, result = probe(self.base, "size", object_name="sqlite-size", bytes=size, timeout=300)
            if status != 200 or result.get("bytes") != size:
                malformed.append(f"{size >> 20} MiB: {status} {str(result)[:120]}")
            elif result.get("accepted") is True and result.get("stored") == size:
                sizes[size] = "stored"
            elif result.get("accepted") is False and isinstance(result.get("error"), str):
                sizes[size] = f"refused ({result['error'][:60]})"
            else:
                malformed.append(f"{size >> 20} MiB: {result}")
        self.measurements["sqlite"] = {**info, "sizes": {f"{size >> 20}MiB": v for size, v in sizes.items()}}
        checks = {
            "affinity": types == expected,
            "64-bit integers": info["affinity"].get("maxInt") == "9223372036854775807",
            "100 bindings": info["variables100"] is True,
            "1 MiB value stored exactly": sizes.get(1 << 20) == "stored",
            "well-formed size replies": not malformed,
        }
        failed = [name for name, passed in checks.items() if not passed]
        measured = {f"{size >> 20} MiB": v for size, v in sizes.items() if size != 1 << 20}
        detail = f"asserted: {', '.join(checks)}{' — failed: ' + ', '.join(failed) if failed else ''}{'; ' + '; '.join(malformed) if malformed else ''}; measured: SQLite {info.get('version')}, 1000 bindings={info['variables1000']}, 100 kB statement={info['statement100k']}, json={info.get('json')}, STRICT={info.get('strict')}, larger values {measured}"
        return ("fail" if failed else "pass"), detail

    def clock(self):
        info = probed(self.base, "clock", "frozen", "dateAdvancedMs", "perfAdvancedMs", ms=50)
        if not isinstance(info["frozen"], bool) or not isinstance(info["dateAdvancedMs"], (int, float)):
            return "fail", f"malformed clock probe: {info}"
        self.measurements["clock_in_sync_call"] = {"frozen": info["frozen"], "dateAdvancedMs": info["dateAdvancedMs"], "perfAdvancedMs": info["perfAdvancedMs"]}
        observed = "frozen inside a synchronous call (workerd behaviour)" if info["frozen"] else f"advances inside a synchronous call (+{info['dateAdvancedMs']} ms over a 50 ms spin; a workerd deviation)"
        return "measured", observed + "; Norn reads the clock once per call and passes it into the cell, so either is safe"

    def wait_until(self):
        """Background work registered with waitUntil completes after the reply, with no traffic meanwhile.
        It passes only with evidence that the work was still pending when the reply arrived (the reply
        counts fewer writes than scheduled, and came back before the first was due) and finished after.
        Idle eviction is not claimed: whether the object was replaced meanwhile is only reported."""
        name = f"bg-{secrets.token_hex(4)}"
        n, every = 15, 1000
        probed(self.base, "pairs", "count", object_name=name)  # place the object first: its start is not the reply's time
        sent = time.perf_counter()
        started = probed(self.base, "wait-until", "started", "written", object_name=name, n=n, every=every)
        reply_ms = (time.perf_counter() - sent) * 1000
        time.sleep(22)
        counts = probed(self.base, "pairs", "count", "pairs", object_name=name)
        written = started["written"]
        same = started.get("instance") == counts.get("instance")
        detail = f"{n} background turns, one per {every} ms, scheduled before the reply; the reply came after {reply_ms:.0f} ms counting {written} written; after 22 s with no traffic {counts['count']} ran, each atomic ({counts['pairs']} pairs); the {'same instance answered afterwards' if same else 'object was replaced afterwards (new instance)'} (measured, not a claim about eviction)"
        if not isinstance(written, int) or written >= n or reply_ms >= every:
            return "not tested", "the reply did not show background work still pending when it arrived, so work after the reply was not shown; " + detail
        if counts["count"] != n or counts["pairs"] != n:
            return "fail", detail
        return "pass", detail

    # -- websockets

    def websocket(self):
        """A live subscription keeps answering after 15 s idle. Whether the object hibernated meanwhile
        is its own row: shown only when a new instance answered the commit that the socket saw."""
        ws = WebSocket(self.base, f"/v1/subscribe?{ANALYTICS}", {"authorization": ADMIN, "sec-websocket-protocol": "norn.live.v1"})
        hibernation = ("not tested", "the live subscription did not complete")
        try:
            ws.send(json.dumps({"type": "subscribe", "id": "list", "path": f"/v1/definitions:list?kind=dashboard&{ANALYTICS}"}))
            first = ws.receive(30)
            _, subscribed, _ = head(self.base)
            idle = 15
            time.sleep(idle)
            step = self.ref["live"]
            started = time.perf_counter()
            status, _, _, got = scripted(self.base, step, self.now)
            second = ws.receive(30)
            latency = (time.perf_counter() - started) * 1000
        finally:
            ws.close()
        anonymous = request(self.base, "GET", "/v1/subscribe", ANALYTICS, headers={"upgrade": "websocket", "connection": "upgrade", "sec-websocket-key": base64.b64encode(os.urandom(16)).decode(), "sec-websocket-version": "13"})[0]
        ok = first.get("type") == "snapshot" and status == 200 and second.get("type") == "snapshot" and second.get("digest") != first.get("digest") and anonymous == 401
        self.measurements["live_commit_to_snapshot_ms"] = round(latency, 1)
        before, after = subscribed.get("instance"), header(got, "x-norn-conformance-instance")
        if ok and before and after and before != after:
            hibernation = ("pass", f"the object was rebuilt while idle (instance {before[:8]}… → {after[:8]}…) and the same socket received the new snapshot")
        elif ok and before and after:
            hibernation = ("not tested", f"the socket survived {idle} s idle, but the same instance ({before[:8]}…) answered the commit: the object never hibernated, so hibernation is not shown")
        elif ok:
            hibernation = ("not tested", "the instance was not reported")
        self.hibernation = hibernation
        return ("pass" if ok else "fail"), f"snapshot, {idle} s idle, commit → new snapshot on the same socket in {latency:.0f} ms; anonymous upgrade → {anonymous}"

    def websocket_hibernation(self):
        return getattr(self, "hibernation", ("not tested", "the live subscription row did not run"))

    # -- backup and restore

    def backup_restore(self):
        reference, _, _ = head(self.base)
        reads = [scripted(self.base, step, self.now)[:2] for step in self.ref["reads"]]
        self.host.stop()
        backup = self.host.state / "backup"
        shutil.rmtree(backup, ignore_errors=True)
        for path in self.host.data():
            sudo(["cp", "-a", str(path), str(backup / path.name)], mkdir=backup)
            sudo(["rm", "-rf", str(path)])
        if hasattr(self.host, "start_engine"):
            # The restore lands on a fresh engine: nothing but the copied data carries over.
            pass
        for path in self.host.data():
            sudo(["cp", "-a", str(backup / path.name), str(path)])
        self.host.start()
        restored, _ = recovered_head(self.base)
        again = [scripted(self.base, step, self.now)[:2] for step in self.ref["reads"]]
        ok = restored == reference and again == reads
        return ("pass" if ok else "fail"), f"stopped, copied {', '.join(p.name for p in self.host.data())}, wiped, restored, restarted: head {'equal' if restored == reference else f'{restored} != {reference}'}; {len(reads)} reads {'byte-identical' if again == reads else 'differ'}"

    # -- measurements

    def measure(self, cells):
        # Commit latency on one warm cell: 100 sequential governed installs.
        bench = self.ref["bench"]
        latencies = []
        for index in range(100):
            step = dict(bench, body=bench["body"].replace("studio.dashboard.bench", f"studio.dashboard.bench-{index}"))
            latencies.append(scripted(self.base, step, self.now, obj="latency")[2])
        self.measurements["commit_ms"] = summary(latencies)
        reads = [head(self.base)[2] for _ in range(50)]
        self.measurements["head_read_ms"] = summary(reads)
        # Cold open after idle: the analytics cell, after 15 s without traffic.
        time.sleep(15)
        _, _, cold = head(self.base, timeout=300)
        self.measurements["first_request_after_15s_idle_ms"] = round(cold, 1)
        if not self.host.controls:
            return "measured", f"commit p50 {self.measurements['commit_ms']['p50']} ms"
        baseline = pss_kib(self.host.pids())
        opened = []
        for count in (1, 20, 50, cells):
            with concurrent.futures.ThreadPoolExecutor(10) as pool:
                opened.extend(pool.map(lambda i: bench_head(self.host.urls()[i % len(self.host.urls())], i)[2], range(len(opened), count)))
            self.measurements[f"pss_mib_{count}_cells"] = round(pss_kib(self.host.pids()) / 1024, 1)
        self.measurements["pss_mib_baseline"] = round(baseline / 1024, 1)
        self.measurements["mib_per_cell"] = round((pss_kib(self.host.pids()) - baseline) / 1024 / cells, 2)
        self.measurements["cell_open_ms"] = summary(opened)
        # Load: every cell commits once, twenty at a time.
        load = []
        errors = 0
        started = time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(20) as pool:
            for status, ms in pool.map(lambda i: bench_commit(self.host.urls()[i % len(self.host.urls())], i, self.ref, self.now), range(cells)):
                load.append(ms)
                errors += status != 200
        self.measurements[f"commit_ms_{cells}_cells_concurrency_20"] = summary(load)
        self.measurements[f"commits_per_s_{cells}_cells"] = round(cells / (time.perf_counter() - started), 1)
        self.measurements[f"errors_{cells}_cells"] = errors
        self.measurements["host_start_ms"] = summary([self.restart() for _ in range(3)])
        self.measurements["cell_recovery_after_restart_ms"] = recovered_head(self.base)[1]
        return ("measured" if errors == 0 else "fail"), f"{cells} cells opened and committed, {errors} errors"


def bench_head(base, index):
    return head(base, timeout=600, obj=f"bench-{index}")


def bench_commit(base, index, ref, now):
    status, _, ms, _ = scripted(base, ref["bench"], now, timeout=600, obj=f"bench-{index}")
    return status, ms


def header(headers, name):
    return next((value for key, value in headers.items() if key.lower() == name), None)


def summary(values):
    ordered = sorted(values)
    if not ordered:
        return {}

    def pick(q):
        return ordered[min(len(ordered) - 1, int(round(q * (len(ordered) - 1))))]

    return {"n": len(ordered), "p50": round(pick(0.5), 1), "p99": round(pick(0.99), 1), "max": round(ordered[-1], 1)}


def sha(data):
    """The first 16 hex digits of `data`'s sha256 (raw bytes, hashed as they are)."""
    return hashlib.sha256(data).hexdigest()[:16]


def sudo(argv, mkdir=None):
    if mkdir is not None:
        mkdir.mkdir(parents=True, exist_ok=True)
    if subprocess.run(argv, capture_output=True).returncode != 0:
        subprocess.run(["sudo", "-n", *argv], check=True, capture_output=True)


def wait_for(check, seconds):
    stop_at = time.time() + seconds
    while time.time() < stop_at:
        value = check()
        if value:
            return value
        time.sleep(1)
    return None


class WebSocket:
    """A minimal RFC 6455 client: handshake, masked text frames out, text frames in.

    `base` is the host's http(s) URL: http opens plain TCP (ws), https opens TLS with certificate
    and hostname verification (wss); the port defaults by scheme.
    """

    def __init__(self, base, path, headers):
        url = urllib.parse.urlsplit(base)
        if url.scheme not in ("http", "https"):
            raise ValueError(f"unsupported scheme {url.scheme!r}")
        secure = url.scheme == "https"
        default = 443 if secure else 80
        port = url.port or default
        sock = socket.create_connection((url.hostname, port), timeout=30)
        if secure:
            # create_default_context verifies the certificate chain and the hostname; TLS 1.2 at least.
            context = ssl.create_default_context()
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            sock = context.wrap_socket(sock, server_hostname=url.hostname)
        self.sock = sock
        key = base64.b64encode(os.urandom(16)).decode()
        authority = url.hostname if port == default else f"{url.hostname}:{port}"
        if ":" in url.hostname:
            authority = f"[{url.hostname}]" + ("" if port == default else f":{port}")
        lines = [f"GET {path} HTTP/1.1", f"Host: {authority}", "Upgrade: websocket", "Connection: Upgrade", f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13", f"User-Agent: {USER_AGENT}"]
        lines += [f"{name}: {value}" for name, value in headers.items()]
        self.sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode())
        response = b""
        while b"\r\n\r\n" not in response:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("socket closed during handshake")
            response += chunk
        status = response.split(b"\r\n", 1)[0]
        if b" 101 " not in status:
            raise RuntimeError(f"upgrade refused: {status!r}")
        self.buffer = response.split(b"\r\n\r\n", 1)[1]

    def send(self, text):
        payload, mask = text.encode(), os.urandom(4)
        header = bytes([0x81])
        length = len(payload)
        header += bytes([0x80 | length]) if length < 126 else bytes([0x80 | 126]) + length.to_bytes(2, "big")
        self.sock.sendall(header + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def read(self, count):
        while len(self.buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("socket closed")
            self.buffer += chunk
        out, self.buffer = self.buffer[:count], self.buffer[count:]
        return out

    def receive(self, timeout):
        self.sock.settimeout(timeout)
        while True:
            first, second = self.read(2)
            length = second & 0x7F
            if length == 126:
                length = int.from_bytes(self.read(2), "big")
            elif length == 127:
                length = int.from_bytes(self.read(8), "big")
            payload = self.read(length)
            opcode = first & 0x0F
            if opcode == 0x8:
                raise RuntimeError(f"socket closed by the cell: {payload[:2].hex()} {payload[2:].decode(errors='replace')}")
            if opcode == 0x1:
                return json.loads(payload)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


# Host state directories (celld, OpenOrange/Rivet, Wrangler) and the other config spellings celld would
# prefer over wrangler.json: none of these is copied, so a host starts from fresh state and reads only the
# copy's own wrangler.json.
LOCAL_ONLY = {".celld", ".open-orange", ".wrangler", "wrangler.jsonc", "wrangler.toml"}


def fresh_state(requested, source):
    if requested is None:
        return Path(tempfile.mkdtemp(prefix="norn-conformance-state-"))
    if requested.is_symlink():
        sys.exit(f"--state {requested} is a symlink; give a real directory outside the project")
    state = requested.resolve()
    if state == source or source in state.parents:
        sys.exit(f"--state {requested} is inside the project")
    state.mkdir(parents=True, exist_ok=True)
    if any(state.iterdir()):
        sys.exit(f"--state {requested} is not empty; give a fresh directory")
    return state


def local_copy(source, lineage_key):
    """A fresh copy of the built project, opted in for a local host. Only the copy's wrangler.json is
    written: the supplied project (which may be deployed as is) keeps its disabled config byte for byte.
    The caller removes the copy's parent directory afterwards."""
    project = Path(tempfile.mkdtemp(prefix="norn-conformance-")) / source.name
    # A copied `.celld` (often a symlink to the project's own state) would make celld open the original
    # project's objects and runtime: leave it out so the host starts from fresh state in the copy.
    shutil.copytree(source, project, symlinks=True, ignore=lambda folder, names: [n for n in names if n in LOCAL_ONLY] if Path(folder) == Path(source) else [])
    # A top-level link back into the supplied project would let a host write through it: materialize it.
    root = Path(source).resolve()
    for entry in project.iterdir():
        if entry.is_symlink() and (entry.resolve() == root or root in entry.resolve().parents):
            target = entry.resolve()
            entry.unlink()
            if target.is_dir():
                shutil.copytree(target, entry, symlinks=True)
            elif target.exists():
                shutil.copy2(target, entry)
    config = json.loads((project / "wrangler.json").read_text())
    # The local test host opts the conformance entry in explicitly (see conformance/worker.mjs).
    config.setdefault("vars", {}).update({"NORN_LINEAGE_KEY": lineage_key, "NORN_STAGE": "test", "NORN_CONFORMANCE": "synthetic"})
    # The public-URL switches (off in the checked-in config) mean nothing on a local host, and celld refuses them.
    for key in ("workers_dev", "preview_urls", "routes"):
        config.pop(key, None)
    # The copy keeps symlinks, so the copy's wrangler.json may point at the deployable original:
    # replace the link (or file) with a regular file instead of writing through it.
    target = project / "wrangler.json"
    target.unlink()
    target.write_text(json.dumps(config))
    return project


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--host", required=True, choices=["celld", "rivet", "rivet-native", "url"])
    parser.add_argument("--project", type=Path)
    parser.add_argument("--state", type=Path)
    parser.add_argument("--url")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--cells", type=int, default=100)
    parser.add_argument("--quick", action="store_true", help="two kill points, no measurements")
    parser.add_argument("--only", help="comma-separated test names")
    args = parser.parse_args()
    reference = json.loads(args.reference.read_text())
    if reference.get("format") != REFERENCE_FORMAT:
        raise SystemExit(f"reference format {reference.get('format')!r}, expected {REFERENCE_FORMAT} (rerun conformance_reference)")
    cleanup = []
    try:
        run(args, reference, cleanup)
    finally:
        for step in cleanup:
            step()


def run(args, reference, cleanup):
    if args.host == "url":
        host = UrlHost(args.url)
    elif args.host == "rivet-native":
        # No Worker project: the runner is the norn-cell-rivet binary.
        state = fresh_state(args.state, Path(tempfile.gettempdir()) / "norn-conformance-no-project")
        if args.state is None:
            cleanup.append(lambda: shutil.rmtree(state, ignore_errors=True))
        host = RivetNativeHost(state, reference["lineageKey"])
    else:
        source = args.project.resolve()
        # State and logs never reach the project: by default a fresh temp directory; a supplied --state
        # must be an empty, real (not symlinked) directory outside the project.
        state = fresh_state(args.state, source)
        if args.state is None:
            cleanup.append(lambda: shutil.rmtree(state, ignore_errors=True))
        # The host runs a disposable copy: the supplied project's wrangler.json is never written.
        project = local_copy(source, reference["lineageKey"])
        cleanup.append(lambda: shutil.rmtree(project.parent, ignore_errors=True))
        host = CelldHost(project, state) if args.host == "celld" else RivetHost(project, state)
    suite = Suite(host, reference, quick=args.quick)
    started_ms = host.start()
    suite.measurements["first_start_ms"] = round(started_ms or 0, 1)
    tests = [
        ("differential ledger heads", suite.differential, False),
        ("transactionSync rollback on throw", suite.tx_rollback, False),
        ("nested transaction refused", suite.tx_nested, False),
        ("kills at sampled SQL boundaries inside a commit leave no partial write", suite.kill_points, True),
        ("acknowledged commit survives a kill", suite.ack_kill, True),
        ("no interleaving inside a synchronous turn", suite.interleaving, False),
        ("concurrent commits serialize", suite.concurrent_commits, False),
        ("one live instance per id", suite.single_instance, False),
        ("no split brain when a runner stalls mid-commit", suite.zombie, True),
        ("no split brain when a frozen runner thaws after its cell moved", suite.zombie_frozen, True),
        ("alarms (platform probes): fire, re-fire after a failing handler, rollback, kill", suite.alarms, False),
        ("SQLite version, affinity, limits", suite.sqlite, False),
        ("clock inside a synchronous call (measurement)", suite.clock, False),
        ("waitUntil background work completes after the reply", suite.wait_until, False),
        ("WebSocket live subscription across 15 s idle", suite.websocket, False),
        ("WebSocket survives the object hibernating", suite.websocket_hibernation, False),
        ("backup and restore in place", suite.backup_restore, True),
    ]
    only = set(args.only.split(",")) if args.only else None
    try:
        for name, fn, control in tests:
            if only is None or fn.__name__ in only:
                suite.run(name, fn, control)
        if not args.quick and (only is None or "measure" in only):
            suite.run(f"measurements ({args.cells} cells)", lambda: suite.measure(args.cells))
    finally:
        host.stop()
    document = {"host": host.name, "version": getattr(host, "version", None), "results": suite.results, "measurements": suite.measurements}
    print(json.dumps(document, indent=2))
    if args.out:
        args.out.write_text(json.dumps(document, indent=2))
    sys.exit(1 if any(row["status"] == "fail" for row in suite.results) else 0)


if __name__ == "__main__":
    main()
