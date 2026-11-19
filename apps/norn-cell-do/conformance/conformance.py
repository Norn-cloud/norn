#!/usr/bin/env python3
"""Host conformance suite for the Norn cell Worker (D122, #1130, #786).

Runs the same request script on a Workers host and compares it with the
native reference (crates/norn-cell-do/tests/conformance_reference.rs), then
checks the Durable Object properties the cell relies on: transaction
rollback, kills at every storage boundary of a commit, replies only after
durability, no interleaving inside a synchronous turn, one live instance per
id, alarms, SQLite behaviour and limits, the clock inside a synchronous call,
waitUntil, live WebSockets across hibernation, and backup and restore.

    conformance.py --reference REF --host celld  --project DIR [--out OUT]
    conformance.py --reference REF --host rivet  --project DIR [--out OUT]
    conformance.py --reference REF --host url --url https://cell.example [--out OUT]

`celld` and `rivet` start and stop the host themselves (state under
--state); `url` runs only the tests that need no host control. DIR is a
built conformance project (see apps/norn-cell-do/README.md); it lives outside
the checkout. Pins: celld 0.6.3, open-orange 1.0.3 with Rivet Engine
3.1.2-rc.1 (`rivetdev/engine` by digest).
"""
import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

ENGINE_IMAGE = "rivetdev/engine@sha256:6e585002b700dd4d3630808eb67884aa15d1a602d83781984edac47023b6d40f"
NOW_HEADER = "x-norn-conformance-now"
HOLD_HEADER = "x-norn-conformance-hold"
ADMIN = "Bearer fixture:admin@demo"
ANALYTICS = "orgId=org_demo&environmentId=analytics"


# ------------------------------------------------------------------ http


def request(base, method, path, query="", body=None, headers=None, timeout=60):
    url = f"{base}{path}" + (f"?{query}" if query else "")
    data = body.encode() if isinstance(body, str) and method != "GET" else None
    req = urllib.request.Request(url, data=data, method=method)
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
    return status, payload.decode(errors="replace"), (time.perf_counter() - started) * 1000, got


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
        return status, json.loads(body)
    except ValueError:
        return status, {"raw": body[:300]}


def at(query, obj):
    return query if obj is None else f"{query}&conformanceObject={obj}"


def head(base, query=ANALYTICS, timeout=120, obj=None):
    query = at(query, obj)
    status, body, ms, _ = request(base, "GET", "/__conformance/head", query, timeout=timeout)
    if status != 200:
        raise RuntimeError(f"head: {status} {body[:300]}")
    document = json.loads(body)
    return document["head"], document, ms


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


def scripted(base, step, now_ms, hold=None, timeout=120, obj=None):
    headers = {"authorization": step["authorization"], NOW_HEADER: str(now_ms)}
    if hold:
        headers[HOLD_HEADER] = hold
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

    def start(self):
        started = time.perf_counter()
        log = open(self.state / "celld.log", "ab")
        self.child = subprocess.Popen(["celld", "dev", "--port", str(self.port)], cwd=self.project, stdout=log, stderr=log, start_new_session=True)
        wait_ready(self.url, 120, self.child)
        return (time.perf_counter() - started) * 1000

    def kill(self):
        kill_group(self.child)

    stop = kill

    def pids(self):
        return tree(self.child.pid) if self.child else []

    def data(self):
        return [self.project / ".celld"]


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
        self.orange = os.environ.get("ORANGE", "orange")
        self.version = subprocess.check_output([self.orange, "--version"], text=True).strip()
        config = {
            "storage": {"log_directory": "/var/lib/rivet/log", "auto_provision_partitions": 1, "balancing": {"enabled": False}},
            "object_storage": {"primary": {"file_system": {"path": "/var/lib/rivet/objects"}}},
        }
        (state / "engine").mkdir(parents=True, exist_ok=True)
        (state / "engine" / "config.json").write_text(json.dumps(config))

    def urls(self):
        return [f"http://127.0.0.1:{port}" for port in self.ports]

    def start_engine(self):
        subprocess.run(["docker", "rm", "-f", self.container], capture_output=True)
        engine = self.state / "engine"
        subprocess.run([
            "docker", "run", "-d", "--name", self.container, "--network", "host",
            "-e", f"RIVET__AUTH__ADMIN_TOKEN={self.token}",
            "-v", f"{engine / 'config.json'}:/etc/rivet/config.json:ro",
            "-v", f"{engine / 'log'}:/var/lib/rivet/log",
            "-v", f"{engine / 'objects'}:/var/lib/rivet/objects",
            ENGINE_IMAGE,
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
            if time.time() > stop_at or "retry" not in body:
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
            return self.record(name, "skip", "needs host lifecycle control")
        try:
            status, detail = fn()
        except Exception as error:  # noqa: BLE001 - every failure is a row in the table
            status, detail = "fail", f"{type(error).__name__}: {error}"[:600]
        self.record(name, status, detail)

    def restart(self):
        self.host.kill()
        return self.host.start()

    # -- differential

    def differential(self):
        native = self.ref["native"]
        opened, _, _ = head(self.base)
        diffs = []
        if opened != native["opened"]:
            diffs.append(f"opened {opened} != {native['opened']}")
        statements = []
        for index, (step, want) in enumerate(zip(self.ref["script"], native["steps"])):
            status, body, _, got = scripted(self.base, step, self.now)
            statements.append(int(header(got, "x-norn-conformance-statements") or 0))
            now_head, _, _ = head(self.base)
            if status != want["status"]:
                diffs.append(f"step {index} status {status} != {want['status']}: {body[:200]}")
            elif body != want["body"]:
                diffs.append(f"step {index} body differs (sha256 {sha(body)} != {sha(want['body'])})")
            if now_head != want["head"]:
                diffs.append(f"step {index} head {now_head} != {want['head']}")
        self.statements = max(statements) if statements else 0
        final = native["steps"][-1]["head"]
        if diffs:
            return "fail", "; ".join(diffs[:6])
        return "pass", f"{len(native['steps'])} steps byte-identical (status, body, verified head); final head seq={final['seq']} commits={final['commits']} hash={final['hash'][:16]}…; ≤{self.statements} SQL statements per commit"

    # -- transactions

    def tx_rollback(self):
        status, adapter = probe(self.base, "adapter")
        _, thrown = probe(self.base, "tx-throw")
        ok = status == 200 and adapter.get("result", "").endswith(":pass") and thrown.get("rolledBack") and thrown.get("rethrown")
        return ("pass" if ok else "fail"), f"adapter={adapter.get('result') or adapter.get('error')}; thrown string rolled back={thrown.get('rolledBack')} rethrown unchanged={thrown.get('rethrown')}"

    def tx_nested(self):
        _, adapter = probe(self.base, "adapter")
        _, nested = probe(self.base, "tx-nested")
        ok = "nested" in adapter.get("result", "")
        return ("pass" if ok else "fail"), f"Norn refuses nesting before the platform (adapter={adapter.get('result') or adapter.get('error')}); platform nesting: inner error {nested.get('inner')!r}, outer kept={nested.get('outerKept')}"

    # -- kills

    def kill_points(self):
        steps = self.ref["kill"]["script"]
        native = self.ref["kill"]["native"]
        obj = f"kill-{secrets.token_hex(4)}"
        opened, _, _ = head(self.base, obj=obj, timeout=300)
        if opened != native["opened"]:
            return "fail", f"a fresh cell opened at {opened}, native {native['opened']}"
        # Size the sweep by the kill request itself: a dry run on its own fresh cell.
        dry = f"kill-dry-{secrets.token_hex(4)}"
        head(self.base, obj=dry, timeout=300)  # provisioning is not part of the count
        _, _, _, got = scripted(self.base, steps[0], self.now, obj=dry, timeout=300)
        n = max(int(header(got, "x-norn-conformance-statements") or 1), 1)
        spread = sorted({1, 2, 3, max(1, n // 4), max(1, n // 2), max(1, (3 * n) // 4), max(1, n - 1), n})
        if self.quick:
            spread = [1, n]
        points = [f"stmt:{k}" for k in spread] + ["turn-end", "post-turn"]
        rows, failures, reopen_ms = [], [], []
        before = native["opened"]
        for index, point in enumerate(points):
            step, want = steps[index], native["steps"][index]
            # The request spins at the point; the reply must not arrive before the kill.
            replied = []

            def send():
                try:
                    status, _, _, got = scripted(self.base, step, self.now, hold=point, timeout=600, obj=obj)
                    replied.append((status, header(got, "x-norn-conformance-hold-reached")))
                except OSError as error:
                    replied.append((f"error: {type(error).__name__}", None))

            with concurrent.futures.ThreadPoolExecutor(1) as pool:
                future = pool.submit(send)
                time.sleep(2.0)
                early = list(replied)
                self.host.kill()
                future.result(timeout=120)
            self.host.start()
            after_kill, waited = recovered_head(self.base, obj=obj)
            reopen_ms.append(waited)
            landed = after_kill == want["head"]
            if not landed and after_kill != before:
                failures.append(f"{point}: head after kill {after_kill} is neither {before} nor {want['head']}")
            if early and early[0][1] == "false":
                failures.append(f"{point}: harness error, the request never reached the hold")
                before = want["head"] if landed else before
                continue
            if early:
                failures.append(f"{point}: a reply ({early[0][0]}) left before the kill")
            if point.startswith("stmt") and landed:
                failures.append(f"{point}: a write inside an open transaction survived the kill")
            if not landed:
                status, body, _, _ = scripted(self.base, step, self.now, obj=obj)
                retried, _, _ = head(self.base, obj=obj)
                if (status, body) != (want["status"], want["body"]) or retried != want["head"]:
                    failures.append(f"{point}: retry differs from native ({status})")
            rows.append(f"{point}={'landed' if landed else 'rolled back'}")
            before = want["head"]
        # Acknowledged then killed: an acknowledged commit must survive.
        ack_step, ack_want = steps[len(points)], native["steps"][len(points)]
        status, _, _, _ = scripted(self.base, ack_step, self.now, obj=obj)
        self.host.kill()
        self.host.start()
        survived, waited = recovered_head(self.base, obj=obj)
        reopen_ms.append(waited)
        if status != 200 or survived != ack_want["head"]:
            failures.append(f"ack-then-kill: status {status}, head {survived} != {ack_want['head']}")
        self.measurements["cell_recovery_after_kill_ms"] = summary(reopen_ms)
        detail = f"{len(points)} kill points over ≤{n} statements + ack-then-kill: " + ", ".join(rows) + f"; the cell answered again p50 {statistics.median(reopen_ms):.0f} ms after the host restarted"
        return ("fail", "; ".join(failures)[:900]) if failures else ("pass", detail)

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
        before, _, _ = head(self.base)
        with concurrent.futures.ThreadPoolExecutor(len(steps)) as pool:
            statuses = list(pool.map(lambda step: scripted(self.base, step, self.now)[0], steps))
        after, _, _ = head(self.base)
        ok = all(status == 200 for status in statuses) and after["commits"] - before["commits"] == len(steps)
        return ("pass" if ok else "fail"), f"{len(steps)} concurrent installs: statuses {sorted(statuses)}; commits +{after['commits'] - before['commits']}; ledger verifies"

    # -- one live instance

    def single_instance(self):
        urls = self.host.urls()
        name = f"single-{secrets.token_hex(4)}"
        with concurrent.futures.ThreadPoolExecutor(16) as pool:
            replies = list(pool.map(lambda i: probe(urls[i % len(urls)], "identity", object_name=name)[1], range(64)))
        instances = {reply.get("instance") for reply in replies}
        writes = max(reply.get("writes", 0) for reply in replies)
        ok = len(instances) == 1 and writes == 64
        detail = f"64 concurrent requests through {len(urls)} runner(s): {len(instances)} instance(s), {writes} serialized writes"
        if self.host.controls:
            self.restart()
            wait_for(lambda: probe(urls[0], "identity", object_name=name)[0] == 200, 300)
            after = {probe(url, "identity", object_name=name)[1].get("instance") for url in urls for _ in range(4)}
            ok = ok and len(after) == 1
            detail += f"; after restart {len(after)} instance(s)"
        return ("pass" if ok else "fail"), detail

    def zombie_frozen(self):
        """Rivet only: the runner holding a cell is frozen (SIGSTOP) mid-commit until the engine loses it
        and serves the cell elsewhere; a write lands there; then the frozen runner thaws and finishes
        its commit. That commit must not land (the outcome must be `y` alone) and must not be acknowledged."""
        if self.host.runners < 2:
            return "skip", "needs two runners"
        urls = self.host.urls()
        z = self.ref["zombie"]
        obj = f"frozen-{secrets.token_hex(4)}"
        before, ident, _ = head(urls[0], obj=obj, timeout=300)
        held = []

        def send():
            try:
                held.append(scripted(urls[0], z["x"], self.now, hold="stmt:3:5000", timeout=900, obj=obj)[:2])
            except OSError as error:
                held.append((f"error {type(error).__name__}", ""))

        notes = []
        with concurrent.futures.ThreadPoolExecutor(1) as pool:
            future = pool.submit(send)
            time.sleep(2)
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
                    if status == 200 and json.loads(body).get("instance") != ident.get("instance"):
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
        outcome = {json.dumps(heads["y"], sort_keys=True): "fenced (y only)", json.dumps(heads["xy"], sort_keys=True): "x then y", json.dumps(heads["yx"], sort_keys=True): "SPLIT BRAIN (y then x)"}.get(json.dumps(final, sort_keys=True), f"unknown head {final}")
        detail = f"runner {owner} frozen mid-commit; cell served by a new instance on runner {other} after {moved_s:.0f} s (head {'unchanged' if moved['head'] == before else moved['head']}; earlier answers {sorted(set(notes))}); y → {y_status}; thawed x → {held_status}; outcome: {outcome}"
        ok = moved["head"] == before and y_status == 200 and outcome.startswith("fenced") and held_status != 200
        return ("pass" if ok else "fail"), detail

    def zombie(self):
        """Rivet only: the runner holding a cell stalls mid-commit; the cell moves; the stalled instance must not commit."""
        if self.host.runners < 2:
            return "skip", "needs two runners"
        urls = self.host.urls()
        z = self.ref["zombie"]
        obj = f"zombie-{secrets.token_hex(4)}"
        before, ident, _ = head(urls[0], obj=obj, timeout=300)
        held = []

        def send():
            try:
                held.append(scripted(urls[0], z["x"], self.now, hold="stmt:3:40000", timeout=600, obj=obj)[:2])
            except OSError as error:
                held.append((f"error {type(error).__name__}", ""))

        with concurrent.futures.ThreadPoolExecutor(1) as pool:
            future = pool.submit(send)
            time.sleep(2)
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
        outcome = {json.dumps(heads["xy"], sort_keys=True): "serialized (x then y)", json.dumps(heads["y"], sort_keys=True): "fenced (y only)", json.dumps(heads["yx"], sort_keys=True): "SPLIT BRAIN (y then x)"}.get(json.dumps(final, sort_keys=True), f"unknown head {final}")
        detail = f"owner runner {owner} (CPU ticks {spent}) spun 40 s inside the transaction; write via runner {other} → {status} after {moved_ms:.0f} ms (instance {'moved' if got.get('x-norn-conformance-instance') != ident.get('instance') else 'same'}); held request → {held_status}; outcome: {outcome}"
        if status != 200:
            return "fail", detail + f"; {body[:200]}"
        if outcome.startswith("fenced") and held_status == 200:
            return "fail", detail + "; the held request was acknowledged but its write is not in the ledger"
        return ("pass" if outcome.startswith(("serialized", "fenced")) else "fail"), detail

    # -- alarms

    def alarms(self):
        notes, ok = [], True
        name = f"alarm-{secrets.token_hex(4)}"
        probe(self.base, "alarm-set", object_name=name, **{"in": 1500})
        fired = wait_for(lambda: probe(self.base, "alarm-state", object_name=name)[1].get("fired"), 30)
        ok &= bool(fired)
        notes.append(f"fires: {len(fired or [])}")
        name = f"alarm-retry-{secrets.token_hex(4)}"
        probe(self.base, "alarm-set", object_name=name, fail=1, **{"in": 1000})
        fired = wait_for(lambda: probe(self.base, "alarm-state", object_name=name)[1].get("fired"), 90)
        ok &= bool(fired)
        notes.append(f"retried after a failing handler: {fired[0][1] if fired else 'never'}")
        _, rolled = probe(self.base, "alarm-rollback", object_name=f"alarm-rb-{secrets.token_hex(4)}")
        ok &= rolled.get("alarm") is None and rolled.get("logged") == 0
        notes.append(f"rolled back with its transaction: alarm={rolled.get('alarm')} rows={rolled.get('logged')}")
        if self.host.controls:
            name = f"alarm-restart-{secrets.token_hex(4)}"
            probe(self.base, "alarm-set", object_name=name, **{"in": 6000})
            self.host.kill()
            killed_at = time.time()
            self.host.start()
            fired = wait_for(lambda: probe(self.base, "alarm-state", object_name=name)[1].get("fired"), 120)
            ok &= bool(fired)
            notes.append(f"survives a kill: {'fired' if fired else 'lost'} ({time.time() - killed_at:.0f} s after the kill)")
        return ("pass" if ok else "fail"), "; ".join(notes)

    # -- sqlite

    def sqlite(self):
        status, info = probe(self.base, "sqlite")
        if status != 200:
            return "fail", f"probe failed: {info.get('error') or info}"
        expected = ["integer", "text", "real", "integer", "blob", "text"]
        types = info.get("affinity", {}).get("types")
        sizes = {}
        for size in (1 << 20, 16 << 20, 130 << 20):
            _, result = probe(self.base, "size", object_name="sqlite-size", bytes=size, timeout=300)
            sizes[f"{size >> 20}MiB"] = "ok" if result.get("accepted") else result.get("error", "?")[:80]
        self.measurements["sqlite"] = {**info, "sizes": sizes}
        ok = types == expected and info.get("affinity", {}).get("maxInt") == "9223372036854775807" and info.get("variables100") is True
        return ("pass" if ok else "fail"), f"SQLite {info.get('version')}; affinity {types}; 1000 bindings={info.get('variables1000')}; 100 kB statement={info.get('statement100k')}; json={info.get('json')}; STRICT={info.get('strict')}; one value per commit: {sizes}"

    def clock(self):
        _, info = probe(self.base, "clock", ms=50)
        frozen = info.get("frozen")
        return "pass", ("frozen inside a synchronous call" if frozen else f"advances inside a synchronous call (+{info.get('dateAdvancedMs')} ms over a 50 ms spin): a workerd deviation; safe for Norn, which reads the clock once per call and passes it into the cell")

    def wait_until(self):
        name = f"bg-{secrets.token_hex(4)}"
        probe(self.base, "wait-until", object_name=name, n=15, every=1000)
        time.sleep(22)
        _, counts = probe(self.base, "pairs", object_name=name)
        ok = counts.get("count") == 15 and counts.get("pairs") == 15
        return ("pass" if ok else "fail"), f"15 background turns over 15 s with no traffic (idle eviction after 10 s on OpenOrange): {counts.get('count')} ran, each atomic ({counts.get('pairs')} pairs)"

    # -- websockets

    def websocket(self):
        url = urllib.parse.urlparse(self.base)
        ws = WebSocket(url.hostname, url.port, f"/v1/subscribe?{ANALYTICS}", {"authorization": ADMIN, "sec-websocket-protocol": "norn.live.v1"})
        ws.send(json.dumps({"type": "subscribe", "id": "list", "path": f"/v1/definitions:list?kind=dashboard&{ANALYTICS}"}))
        first = ws.receive(30)
        idle = 15
        time.sleep(idle)
        step = self.ref["live"]
        started = time.perf_counter()
        status, _, _, _ = scripted(self.base, step, self.now)
        second = ws.receive(30)
        latency = (time.perf_counter() - started) * 1000
        ws.close()
        anonymous = request(self.base, "GET", "/v1/subscribe", ANALYTICS, headers={"upgrade": "websocket", "connection": "upgrade", "sec-websocket-key": base64.b64encode(os.urandom(16)).decode(), "sec-websocket-version": "13"})[0]
        ok = first.get("type") == "snapshot" and status == 200 and second.get("type") == "snapshot" and second.get("digest") != first.get("digest") and anonymous == 401
        self.measurements["live_commit_to_snapshot_ms"] = round(latency, 1)
        return ("pass" if ok else "fail"), f"snapshot, {idle} s idle (past OpenOrange's 10 s instance drop), commit → new snapshot in {latency:.0f} ms; anonymous upgrade → {anonymous}"

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
            return "pass", f"commit p50 {self.measurements['commit_ms']['p50']} ms"
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
        return ("pass" if errors == 0 else "fail"), f"{cells} cells opened and committed, {errors} errors"


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
    pick = lambda q: ordered[min(len(ordered) - 1, int(round(q * (len(ordered) - 1))))]
    return {"n": len(ordered), "p50": round(pick(0.5), 1), "p99": round(pick(0.99), 1), "max": round(ordered[-1], 1)}


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()[:16]


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
    """A minimal RFC 6455 client: handshake, masked text frames out, text frames in."""

    def __init__(self, host, port, path, headers):
        self.sock = socket.create_connection((host, port), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        lines = [f"GET {path} HTTP/1.1", f"Host: {host}:{port}", "Upgrade: websocket", "Connection: Upgrade", f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13"]
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


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--host", required=True, choices=["celld", "rivet", "url"])
    parser.add_argument("--project", type=Path)
    parser.add_argument("--state", type=Path)
    parser.add_argument("--url")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--cells", type=int, default=100)
    parser.add_argument("--quick", action="store_true", help="two kill points, no measurements")
    parser.add_argument("--only", help="comma-separated test names")
    args = parser.parse_args()
    reference = json.loads(args.reference.read_text())
    assert reference["format"] == "norn/host-conformance-reference@1", reference["format"]
    if args.host == "url":
        host = UrlHost(args.url)
    else:
        project = args.project.resolve()
        config = json.loads((project / "wrangler.json").read_text())
        config.setdefault("vars", {})["NORN_LINEAGE_KEY"] = reference["lineageKey"]
        (project / "wrangler.json").write_text(json.dumps(config))
        state = (args.state or project / ".conformance").resolve()
        state.mkdir(parents=True, exist_ok=True)
        host = CelldHost(project, state) if args.host == "celld" else RivetHost(project, state)
    suite = Suite(host, reference, quick=args.quick)
    started_ms = host.start()
    suite.measurements["first_start_ms"] = round(started_ms or 0, 1)
    tests = [
        ("differential ledger heads", suite.differential, False),
        ("transactionSync rollback on throw", suite.tx_rollback, False),
        ("nested transaction refused", suite.tx_nested, False),
        ("kill at every commit boundary", suite.kill_points, True),
        ("no interleaving inside a synchronous turn", suite.interleaving, False),
        ("concurrent commits serialize", suite.concurrent_commits, False),
        ("one live instance per id", suite.single_instance, False),
        ("no split brain when a runner stalls mid-commit", suite.zombie, True),
        ("no split brain when a frozen runner thaws after its cell moved", suite.zombie_frozen, True),
        ("alarms: at-least-once, restart, rollback", suite.alarms, False),
        ("SQLite version, affinity, limits", suite.sqlite, False),
        ("clock inside a synchronous call", suite.clock, False),
        ("waitUntil across idle eviction", suite.wait_until, False),
        ("WebSocket subscribe across hibernation", suite.websocket, False),
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
