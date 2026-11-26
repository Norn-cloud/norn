#!/usr/bin/env python3
"""Durability and cost evidence for Norn cells on a three-instance Rivet cluster (D131, #1130).

Brings the cluster up from apps/norn-cell-rivet/compose.yaml (three engines, the load balancer,
the bundled S3 bucket) with compose.meter.yaml in front of the bucket, and runs norn-cell-rivet
runners (a `conformance` build, NORN_CELL_RIVET) on this machine. Phases:

  kill     kill -9 loop: writers commit governed installs, each acknowledged id recorded; every
           round SIGKILLs a runner, an engine, both, or everything mid-commit, restarts it, and
           requires every acknowledged id present, no id that was never sent, and the ledger's
           hash chain verified from genesis on reopen (/__conformance/head).
  disk     loses one engine's disk while writers run (kill -9, volume deleted, restarted empty),
           then a second engine's after the first rebuilt, then a full restart; the same oracle.
  load     S3 requests made by 10,000 commits (and the offload after them).
  idle     S3 requests made by an idle cluster over --idle-seconds (3600 by default).
  latency  warm read and commit latency p50/p95/p99 through the router, and the storage floor (a
           one-row commit in a probe actor).
  rehearsal  cells are created and commit; the cluster crashes (a runner, one engine, everything)
           and restarts; every cell answers the same verified head and listing bytes, and commits
           again on its chain. Create and commit latency, recovery time per crash.
  storage  bytes per cell on the engines' log disks (all replicas) and in the bucket, after
           --storage-cells cells with --storage-commits commits each and the offload settled.

    NORN_CELL_RIVET=BIN durability.py --reference REF --phase kill,disk,load,latency --out OUT

REF is the native conformance reference (crates/norn-cell-do/tests/conformance_reference.rs):
the request shape and the stage lineage key come from it. Every row is a measurement or a
pass/fail with its evidence. Power loss is not modelled: kills are process kills, and the page
cache survives them.
"""
import argparse
import concurrent.futures
from contextlib import contextmanager
from datetime import datetime, timezone
import http.client
import json
import os
from pathlib import Path
import random
import re
import secrets
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

HERE = Path(__file__).resolve().parent
APP = HERE.parent
ADMIN = "Bearer fixture:admin@demo"
SCOPE = "orgId=org_demo&environmentId=analytics"
NOW_HEADER = "x-norn-conformance-now"
ENGINES = ["engine-1", "engine-2", "engine-3"]
WRITE_PROGRESS_SECONDS = 300
WRITE_BATCH_SECONDS = 900


def log(message):
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def request(base, method, path, query="", body=None, headers=None, timeout=60):
    url = f"{base}{path}" + (f"?{query}" if query else "")
    data = body.encode() if isinstance(body, str) else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("user-agent", "norn-durability/1")
    for name, value in (headers or {}).items():
        req.add_header(name, value)
    if data is not None:
        req.add_header("content-type", "application/json")
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            status, payload = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, payload = error.code, error.read()
    return status, payload, (time.perf_counter() - started) * 1000


def summary(values):
    ordered = sorted(values)
    if not ordered:
        return {}
    def pick(q):
        return ordered[min(len(ordered) - 1, int(q * len(ordered)))]
    return {"n": len(ordered), "p50": round(statistics.median(ordered), 2), "p90": round(pick(0.90), 2), "p95": round(pick(0.95), 2), "p99": round(pick(0.99), 2), "max": round(ordered[-1], 2)}


def run(argv, check=True, **kwargs):
    return subprocess.run(argv, check=check, capture_output=True, text=True, **kwargs)


class Cluster:
    """Three engines, the load balancer and the metered bucket (compose), plus runner processes."""

    def __init__(self, state, binary, lineage_key, project, lb_port, runners=2):
        self.state, self.binary, self.lineage_key = state, binary, lineage_key
        self.project, self.lb_port = project, lb_port
        self.token = secrets.token_hex(24)
        self.ports = [self.free_port() for _ in range(runners)]
        self.children = [None] * runners
        self.env_file = state / "compose.env"
        self.env_file.write_text("\n".join([
            f"RIVET_ADMIN_TOKEN={self.token}",
            f"RIVET_LB_PORT={lb_port}",
            "S3_BUCKET=norn-dura",
            "S3_ENDPOINT=http://s3-meter:9000",
            "S3_REGION=us-east-1",
            "S3_ALLOW_HTTP=true",
            f"S3_ACCESS_KEY_ID=dura{secrets.token_hex(6)}",
            f"S3_SECRET_ACCESS_KEY={secrets.token_hex(20)}",
            # The compose runners are not started here; their required variables are placeholders.
            "NORN_HOST_ISSUER=unused", "NORN_HOST_JWKS_URL=unused", f"NORN_LINEAGE_KEY={lineage_key}",
            "",
        ]))
        self.meter_snapshot = []

    @staticmethod
    def free_port():
        import socket
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            return sock.getsockname()[1]

    def compose(self, *args, check=True):
        argv = ["sudo", "docker", "compose", "-p", self.project, "--env-file", str(self.env_file),
                "-f", str(APP / "compose.yaml"), "-f", str(HERE / "compose.meter.yaml"), "--profile", "s3", *args]
        return run(argv, check=check)

    def container(self, service):
        out = self.compose("ps", "-a", "-q", service, check=False).stdout.strip()
        return out.splitlines()[0] if out else None

    def require_unused_project(self):
        label = f"label=com.docker.compose.project={self.project}"
        for kind in ("container", "network", "volume"):
            args = ["-a"] if kind == "container" else []
            if run(["sudo", "docker", kind, "ls", "-q", *args, "--filter", label]).stdout.strip():
                raise RuntimeError(f"Compose project {self.project} already has a {kind}")

    def up(self):
        started = time.perf_counter()
        self.compose("up", "-d", "s3", "s3-bucket", "s3-meter", *ENGINES, "lb", "pool")
        self.wait_pool()
        for index in range(len(self.ports)):
            self.start_runner(index)
        for url in self.urls():
            self.wait_ready(url)
        log(f"cluster up in {time.perf_counter() - started:.1f} s")

    def wait_pool(self, seconds=300):
        stop = time.time() + seconds
        while time.time() < stop:
            done = self.compose("ps", "-a", "--format", "json", "pool", check=False).stdout.strip()
            entries = []
            if done.startswith("["):
                try:
                    parsed = json.loads(done)
                    if isinstance(parsed, list):
                        entries = parsed
                except ValueError:
                    pass
            else:
                for line in done.splitlines():
                    try:
                        entries.append(json.loads(line))
                    except ValueError:
                        pass
            for entry in entries:
                if not isinstance(entry, dict):
                    continue
                if entry.get("State") == "exited":
                    code = entry.get("ExitCode")
                    if code == 0:
                        return
                    raise RuntimeError(f"the pool job exited with code {code}: " + self.compose("logs", "--tail", "40", "pool", check=False).stdout[-2000:])
            time.sleep(2)
        raise RuntimeError("the pool job never finished: " + self.compose("logs", "--tail", "40", check=False).stdout[-2000:])

    def urls(self):
        return [f"http://127.0.0.1:{port}" for port in self.ports]

    def start_runner(self, index):
        env = {**os.environ, "RIVET_ENDPOINT": f"http://default:{self.token}@127.0.0.1:{self.lb_port}", "NORN_LISTEN": f"127.0.0.1:{self.ports[index]}",
               "NORN_STAGE": "test", "NORN_CONFORMANCE": "synthetic", "NORN_LINEAGE_KEY": self.lineage_key}
        with open(self.state / f"runner-{index}.log", "ab") as out:
            self.children[index] = subprocess.Popen([self.binary], env=env, stdout=out, stderr=out, start_new_session=True)

    def supervise(self):
        """Restart a runner process that exited on its own (none should; counted)."""
        for index, child in enumerate(self.children):
            if child is not None and child.poll() is not None:
                self.exited = getattr(self, "exited", 0) + 1
                log(f"runner {index} exited on its own (code {child.returncode}); restarting it")
                self.start_runner(index)

    def kill_runner(self, index, *, require_running=True):
        child = self.children[index]
        if child is not None and child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            if child.wait(timeout=30) != -signal.SIGKILL and require_running:
                raise RuntimeError(f"runner {index} did not exit from SIGKILL")
        elif require_running:
            raise RuntimeError(f"runner {index} was already stopped")
        self.children[index] = None

    def crash_and_restart(self, kind, engine=None):
        """Inject one crash kind (`runner`, `engine`, `engine+runner`, `everything`), wait 1 s, and restart."""
        if kind in ("runner", "engine+runner", "everything"):
            for index in range(len(self.children)):
                self.kill_runner(index)
        if kind in ("engine", "engine+runner"):
            self.kill_engine(engine)
        if kind == "everything":
            for service in ENGINES:
                self.kill_engine(service)
        killed_at = time.perf_counter()
        time.sleep(1)
        if kind in ("engine", "engine+runner"):
            self.start_engine(engine)
        if kind == "everything":
            for service in ENGINES:
                self.start_engine(service)
        if kind != "engine":
            for index in range(len(self.children)):
                self.start_runner(index)
        return killed_at

    def wait_ready(self, base, seconds=300):
        stop = time.time() + seconds
        while time.time() < stop:
            try:
                if request(base, "GET", "/__probe/identity", "object=ready", timeout=10)[0] == 200:
                    return
            except OSError:
                pass
            time.sleep(0.5)
        raise RuntimeError(f"{base} not ready in {seconds} s")

    def kill_engine(self, service):
        cid = self.container(service)
        if not cid:
            raise RuntimeError(f"engine {service} is absent")
        injected_at = datetime.now(timezone.utc)
        run(["sudo", "docker", "kill", "-s", "KILL", cid])
        state = json.loads(run(["sudo", "docker", "inspect", "-f", "{{json .State}}", cid]).stdout)
        if state["Running"] or state["ExitCode"] != 137 or state["OOMKilled"]:
            raise RuntimeError(f"engine {service} did not exit from injected SIGKILL")
        finished_at = datetime.fromisoformat(state["FinishedAt"].replace("Z", "+00:00"))
        if finished_at < injected_at:
            raise RuntimeError(f"engine {service} exited before SIGKILL was injected")

    def start_engine(self, service):
        self.compose("up", "-d", "--no-deps", service)

    def lose_disk(self, service):
        """The engine's disk is gone: kill -9, delete its container and volume, start it empty."""
        self.kill_engine(service)
        cid = self.container(service)
        if cid:
            run(["sudo", "docker", "rm", "-f", cid], check=False)
        volume = f"{self.project}_{service}-log"
        removed = run(["sudo", "docker", "volume", "rm", volume], check=False)
        if removed.returncode != 0:
            raise RuntimeError(f"volume {volume} not removed: {removed.stderr[-300:]}")
        self.start_engine(service)

    def down(self):
        for index in range(len(self.children)):
            self.kill_runner(index, require_running=False)
        self.compose("down", "-v", "--remove-orphans", check=False)

    def meter(self):
        """Every S3 request the engines made so far: (method, path, status) per line."""
        cid = self.container("s3-meter")
        if not cid:
            raise RuntimeError("the S3 meter container is absent")
        out = run(["sudo", "docker", "logs", cid])
        lines = (out.stdout + out.stderr).splitlines()
        current = [tuple(line.split(" ", 3)[1:4]) for line in lines if line.startswith("S3 ") and len(line.split(" ")) >= 4]
        if current[:len(self.meter_snapshot)] != self.meter_snapshot:
            raise RuntimeError("the S3 meter log lost its previous prefix")
        self.meter_snapshot = current
        return current


def classify(requests):
    """Count all requests and estimate R2 operation counts for the observed responses."""
    counts, chargeable = {}, {}
    for method, path, status in requests:
        query = path.split("?", 1)[1] if "?" in path else ""
        key_part = path.split("?", 1)[0].strip("/").split("/", 1)
        is_bucket = len(key_part) < 2 or key_part[1] == ""
        if method == "GET" and is_bucket and ("list-type" in query or "prefix=" in query or "delimiter=" in query or query == "" or "continuation-token" in query):
            op = "ListObjects"
        elif method == "GET" and "uploads" in query:
            op = "ListMultipartUploads"
        elif method == "POST" and "uploads" in query:
            op = "CreateMultipartUpload"
        elif method == "PUT" and "partNumber" in query:
            op = "UploadPart"
        elif method == "POST" and "uploadId" in query:
            op = "CompleteMultipartUpload"
        elif method == "POST" and "delete" in query:
            op = "DeleteObjects"
        else:
            op = {"PUT": "PutObject", "GET": "GetObject", "HEAD": "HeadObject", "DELETE": "DeleteObject", "POST": "PostObject"}.get(method, method)
        counts[op] = counts.get(op, 0) + 1
        if status not in {"401", "403"}:
            chargeable[op] = chargeable.get(op, 0) + 1
    # Where the requests went: the object path with ids and sequence numbers folded.
    paths = {}
    for method, path, _status in requests:
        key = re.sub(r"[0-9a-f]{8}-[0-9a-f-]{27,}|\d{6,}", "*", path.split("?", 1)[0])
        key = f"{method} {'/'.join(key.split('/')[:6])}"
        paths[key] = paths.get(key, 0) + 1
    class_a = sum(n for op, n in chargeable.items() if op in {"ListObjects", "ListMultipartUploads", "CreateMultipartUpload", "UploadPart", "CompleteMultipartUpload", "PutObject", "PostObject"})
    class_b = sum(n for op, n in chargeable.items() if op in {"GetObject", "HeadObject"})
    return {"byPath": dict(sorted(paths.items(), key=lambda kv: -kv[1])[:15]), "byOperation": dict(sorted(counts.items())), "classA": class_a, "classB": class_b, "free": counts.get("DeleteObject", 0) + counts.get("DeleteObjects", 0), "total": len(requests)}


class Writers:
    """Writers, each on its own cell, committing governed installs. Every acknowledged id is
    appended to a file and fsynced before it counts; an unanswered commit is in flight (it may
    land, it is never required)."""

    def __init__(self, cluster, reference, state, count=4, tag="w"):
        self.cluster, self.state = cluster, state
        self.step = reference["bench"]
        self.now = reference["nowMs"]
        self.cells = [f"dura-{tag}{i}-{secrets.token_hex(3)}" for i in range(count)]
        self.acked = {cell: [] for cell in self.cells}
        self.inflight = {cell: set() for cell in self.cells}
        self.sent = {cell: set() for cell in self.cells}
        self.ack_path = state / f"acks-{tag}.jsonl"
        self.lock = threading.Condition()
        self.stop = threading.Event()
        self.errors = 0

    def install(self, cell, ident, base):
        body = self.step["body"].replace("studio.dashboard.bench", ident)
        query = f"{SCOPE}&conformanceObject={cell}"
        return request(base, "POST", self.step["path"], query, body, {"authorization": ADMIN, NOW_HEADER: str(self.now)}, timeout=120)

    def open_cells(self):
        for cell in self.cells:
            head(self.cluster.urls()[0], cell, timeout=300)

    def loop(self, index, cell, limit, ack_log):
        seq = 0
        while not self.stop.is_set():
            if limit is not None and len(self.acked[cell]) >= limit:
                return
            ident = f"studio.dashboard.dura-{cell}-{seq}"
            seq += 1
            base = self.cluster.urls()[(index + seq) % len(self.cluster.urls())]
            with self.lock:
                self.sent[cell].add(ident)
                self.inflight[cell].add(ident)
            try:
                status, _, ms = self.install(cell, ident, base)
            except (OSError, http.client.HTTPException):
                status, ms = None, None
            with self.lock:
                if status == 200:
                    ack_log.write(json.dumps({"cell": cell, "id": ident, "ms": ms}) + "\n")
                    ack_log.flush()
                    os.fsync(ack_log.fileno())
                    self.inflight[cell].discard(ident)
                    self.acked[cell].append(ident)
                    self.lock.notify_all()
                else:
                    self.errors += 1
            if status != 200:
                self.stop.wait(0.5)

    @contextmanager
    def running(self, limit=None):
        with self.ack_path.open("a") as ack_log, concurrent.futures.ThreadPoolExecutor(max_workers=len(self.cells)) as pool:
            self.futures = [pool.submit(self.loop, i, cell, limit, ack_log) for i, cell in enumerate(self.cells)]
            try:
                yield
            finally:
                self.stop.set()
            for future in self.futures:
                future.result()

    def commit_batch(self, limit):
        if limit <= 0:
            raise ValueError("each writer needs a positive target")
        with self.running(limit):
            for future in concurrent.futures.as_completed(self.futures, timeout=WRITE_BATCH_SECONDS):
                future.result()

    def counts(self):
        with self.lock:
            return {cell: len(ids) for cell, ids in self.acked.items()}

    def wait_for_progress(self, before):
        with self.lock:
            deadline = time.monotonic() + WRITE_PROGRESS_SECONDS
            while pending := [cell for cell, count in before.items() if len(self.acked[cell]) <= count]:
                for future in self.futures:
                    if future.done():
                        future.result()
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"no acknowledged write from {', '.join(pending)}")
                self.lock.wait(min(1, remaining))

    def total(self):
        return sum(self.counts().values())


def head(base, cell, timeout=120):
    status, body, ms = request(base, "GET", "/__conformance/head", f"{SCOPE}&conformanceObject={cell}", timeout=timeout)
    if status != 200:
        raise RuntimeError(f"head {cell}: {status} {body[:200]!r}")
    return json.loads(body), ms


def recovered(cluster, cells, seconds=600):
    """Every cell answers its verified head again (the open verifies the hash chain from genesis)."""
    started = time.perf_counter()
    stop = time.time() + seconds
    pending = set(cells)
    while pending and time.time() < stop:
        cluster.supervise()
        for cell in list(pending):
            try:
                head(cluster.urls()[0], cell, timeout=60)
                pending.discard(cell)
            except (RuntimeError, OSError, ValueError):
                pass
        if pending:
            time.sleep(1)
    if pending:
        raise RuntimeError(f"no recovery within {seconds} s for {sorted(pending)}")
    return (time.perf_counter() - started) * 1000


def list_dashboard_pages(base, cell):
    """Every page of dashboard definitions from `cell`, as raw bytes and decoded JSON."""
    pages, cursor = [], None
    while True:
        query = f"{SCOPE}&conformanceObject={cell}&kind=dashboard&limit=100" + (f"&cursor={urllib.parse.quote(cursor)}" if cursor else "")
        status, body, _ = request(base, "GET", "/v1/definitions:list", query, headers={"authorization": ADMIN}, timeout=120)
        if status != 200:
            raise RuntimeError(f"definitions {cell}: {status} {body[:200]!r}")
        parsed = json.loads(body)
        pages.append((body, parsed))
        cursor = parsed.get("cursor")
        if not cursor:
            return pages


def installed(base, cell):
    """Every dashboard id the cell lists, page by page."""
    ids = set()
    for _, page in list_dashboard_pages(base, cell):
        ids.update(item["id"] for item in page.get("items", []) if item.get("id", "").startswith("studio.dashboard.dura-"))
    return ids


def verify(cluster, writers):
    """Every acknowledged id is installed, every installed id was sent, the chain verifies."""
    lost, phantom, landed = [], [], 0
    for cell in writers.cells:
        # Acknowledged before the listing starts (each must be listed); sent by the time it ends
        # (nothing else may be listed). Writers may still be committing meanwhile.
        with writers.lock:
            acked = set(writers.acked[cell])
        head(cluster.urls()[0], cell, timeout=300)
        present = installed(cluster.urls()[0], cell)
        with writers.lock:
            sent, inflight = set(writers.sent[cell]), set(writers.inflight[cell])
        lost += sorted(acked - present)
        phantom += sorted(present - sent)
        landed += len(present & inflight)
    return lost, phantom, landed


def phase_kill(cluster, reference, state, rounds):
    if rounds <= 0:
        raise ValueError("the kill phase needs at least one round")
    writers = Writers(cluster, reference, state, tag="k")
    writers.open_cells()
    with writers.running():
        kinds = ["runner", "engine", "engine+runner", "everything"]
        rows, failures, recoveries = [], [], []
        writers.wait_for_progress(dict.fromkeys(writers.cells, 0))
        for round_index in range(rounds):
            time.sleep(random.uniform(6, 14))
            kind = kinds[round_index % len(kinds)]
            engine = random.choice(ENGINES)
            before = writers.total()
            killed_at = cluster.crash_and_restart(kind, engine)
            waited = recovered(cluster, writers.cells)
            recoveries.append({"kind": kind, "ms": round((time.perf_counter() - killed_at) * 1000)})
            writers.wait_for_progress(writers.counts())
            lost, phantom, landed = verify(cluster, writers)
            rows.append({"round": round_index + 1, "killed": kind if "engine" not in kind or kind == "everything" else f"{kind} ({engine})", "ackedSoFar": writers.total(), "ackedThisRound": writers.total() - before, "lost": len(lost), "phantom": len(phantom), "inFlightLanded": landed, "recoveryMs": round(waited)})
            log(f"kill round {round_index + 1}/{rounds}: {rows[-1]}")
            if lost or phantom:
                failures.append({"round": round_index + 1, "lost": lost[:10], "phantom": phantom[:10]})
    lost, phantom, landed = verify(cluster, writers)
    by_kind = {}
    for row in recoveries:
        by_kind.setdefault(row["kind"], []).append(row["ms"])
    return {
        "status": "pass" if not failures and not lost and not phantom else "fail",
        "rounds": rows,
        "acked": writers.total(),
        "commitErrorsDuringOutages": writers.errors,
        "finalLost": len(lost), "finalPhantom": len(phantom), "inFlightLanded": landed,
        "recoveryMsByKind": {kind: summary(values) for kind, values in by_kind.items()},
        "runnersExitedOnTheirOwn": getattr(cluster, "exited", 0),
        "failures": failures,
    }


def phase_disk(cluster, reference, state, rebuild_wait):
    writers = Writers(cluster, reference, state, tag="d")
    writers.open_cells()
    with writers.running():
        steps = []

        def check(label):
            lost, phantom, landed = verify(cluster, writers)
            steps.append({"step": label, "acked": writers.total(), "lost": len(lost), "phantom": len(phantom), "inFlightLanded": landed, "lostIds": lost[:10]})
            log(f"disk: {steps[-1]}")

        time.sleep(20)
        writers.wait_for_progress(dict.fromkeys(writers.cells, 0))
        check("before any loss")
        cluster.lose_disk("engine-2")
        time.sleep(30)
        recovered(cluster, writers.cells)
        writers.wait_for_progress(writers.counts())
        check("engine-2's disk lost and restarted empty, writes resumed")
        time.sleep(rebuild_wait)
        check(f"after {rebuild_wait} s for replicas to rebuild")
        cluster.lose_disk("engine-3")
        time.sleep(30)
        recovered(cluster, writers.cells)
        writers.wait_for_progress(writers.counts())
        check("then engine-3's disk lost and restarted empty, writes resumed")
    cluster.crash_and_restart("everything")
    recovered(cluster, writers.cells)
    check("after kill -9 of everything and a restart")
    failed = any(step["lost"] or step["phantom"] for step in steps)
    return {"status": "fail" if failed else "pass", "steps": steps, "commitErrors": writers.errors}


def phase_load(cluster, reference, state, commits, settle):
    writers = Writers(cluster, reference, state, count=10, tag="l")
    writers.open_cells()
    time.sleep(settle)
    before = cluster.meter()
    started = time.perf_counter()
    writers.commit_batch(commits // len(writers.cells))
    elapsed = time.perf_counter() - started
    after_commits = cluster.meter()
    time.sleep(settle)
    after_settle = cluster.meter()
    lost, phantom, _ = verify(cluster, writers)
    return {
        "status": "fail" if lost or phantom else "pass",
        "commits": writers.total(), "cells": len(writers.cells), "seconds": round(elapsed, 1), "commitsPerSecond": round(writers.total() / elapsed, 1),
        "s3DuringCommits": classify(after_commits[len(before):]),
        "s3IncludingOffload": classify(after_settle[len(before):]),
        "settleSeconds": settle, "lost": len(lost), "phantom": len(phantom),
    }


def phase_idle(cluster, reference, state, seconds, settle):
    if seconds <= 0:
        raise ValueError("the idle phase needs a positive duration")
    writers = Writers(cluster, reference, state, count=10, tag="i")
    writers.open_cells()
    writers.commit_batch(1)
    log(f"idle: {len(writers.cells)} cells committed once; settling {settle} s")
    time.sleep(settle)
    before = cluster.meter()
    time.sleep(seconds)
    window = cluster.meter()[len(before):]
    counts = classify(window)
    per_month = 30 * 24 * 3600 / seconds
    return {"seconds": seconds, "cells": len(writers.cells), "s3": counts, "classAPerMonth": round(counts["classA"] * per_month), "classBPerMonth": round(counts["classB"] * per_month)}


def evidence(base, cell):
    """What a cell answers, as bytes: its verified head (the hash chain over every commit and
    receipt, as the Rust cell serialized it) and its full definition listing."""
    doc, _ = head(base, cell, timeout=300)
    pages = [raw for raw, _ in list_dashboard_pages(base, cell)]
    return doc["headJson"], b"\n".join(pages)


def phase_rehearsal(cluster, reference, state, commits):
    """The alpha's life on the cluster: cells are created, commit, the cluster crashes (a runner;
    one engine; everything at once), restarts, and every cell answers again with the same head
    and listing bytes, then commits on the chain it had."""
    if commits <= 0:
        raise ValueError("the rehearsal needs at least one commit per cell")
    writers = Writers(cluster, reference, state, count=3, tag="r")
    base = cluster.urls()[0]
    created = []
    for cell in writers.cells:
        started = time.perf_counter()
        head(base, cell, timeout=300)
        created.append((time.perf_counter() - started) * 1000)
    latencies = []
    for cell in writers.cells:
        for i in range(commits):
            status, body, ms = writers.install(cell, f"studio.dashboard.dura-{cell}-{i}", base)
            if status != 200:
                raise RuntimeError(f"commit {cell}/{i}: {status} {body[:200]!r}")
            latencies.append(ms)
    rounds, failures = [], []
    for kind in ("runner", "engine", "everything"):
        before = {cell: evidence(base, cell) for cell in writers.cells}
        engine = random.choice(ENGINES)
        killed = cluster.crash_and_restart(kind, engine)
        recovered(cluster, writers.cells)
        recovery = (time.perf_counter() - killed) * 1000
        after = {cell: evidence(cluster.urls()[1], cell) for cell in writers.cells}
        same = [cell for cell in writers.cells if after[cell] == before[cell]]
        # The recovered cell keeps committing on the chain it had.
        resumed = 0
        for cell in writers.cells:
            status, _, _ = writers.install(cell, f"studio.dashboard.dura-{cell}-after-{kind}", base)
            seq_before = json.loads(before[cell][0])["seq"]
            seq_after = json.loads(evidence(base, cell)[0])["seq"]
            resumed += status == 200 and seq_after == seq_before + 1
        row = {"crash": kind if kind != "engine" else f"engine ({engine})", "recoveryMs": round(recovery), "identical": f"{len(same)}/{len(writers.cells)}", "resumed": f"{resumed}/{len(writers.cells)}"}
        rounds.append(row)
        log(f"rehearsal: {row}")
        if len(same) != len(writers.cells) or resumed != len(writers.cells):
            failures.append(row)
    return {"status": "fail" if failures else "pass", "cells": len(writers.cells), "commitsPerCell": commits,
            "createMs": summary(created), "commitMs": summary(latencies), "rounds": rounds}


def volume_bytes(cluster, volume):
    path = run(["sudo", "docker", "volume", "inspect", "-f", "{{.Mountpoint}}", f"{cluster.project}_{volume}"]).stdout.strip()
    return int(run(["sudo", "du", "-sb", path]).stdout.split()[0])


def phase_storage(cluster, reference, state, cells, commits, settle):
    """Bytes per cell: each engine's log disk and the bucket, before and after `cells` cells with
    `commits` governed installs each, once offload has settled."""
    if cells <= 0:
        raise ValueError("the storage phase needs at least one cell")
    volumes = [f"{service}-log" for service in ENGINES] + ["s3-data"]
    time.sleep(settle)
    before = {volume: volume_bytes(cluster, volume) for volume in volumes}
    writers = Writers(cluster, reference, state, count=cells, tag="s")
    writers.open_cells()
    writers.commit_batch(commits)
    time.sleep(settle)
    after = {volume: volume_bytes(cluster, volume) for volume in volumes}
    grown = {volume: after[volume] - before[volume] for volume in volumes}
    logs = sum(grown[f"{service}-log"] for service in ENGINES)
    return {"cells": cells, "commitsPerCell": commits, "acked": writers.total(), "settleSeconds": settle,
            "bytesGrown": grown, "logBytesPerCellAllReplicas": round(logs / cells), "bucketBytesPerCell": round(grown["s3-data"] / cells),
            "bytesPerCommitAllReplicas": round(logs / max(1, writers.total()))}


def phase_latency(cluster, reference, state, samples):
    if samples <= 0:
        raise ValueError("the latency phase needs at least one sample")
    writers = Writers(cluster, reference, state, count=1, tag="t")
    writers.open_cells()
    cell = writers.cells[0]
    base = cluster.urls()[0]
    for i in range(30):
        writers.install(cell, f"studio.dashboard.dura-warm-{i}", base)
        request(base, "GET", "/v1/definitions/studio.dashboard.dura-warm-0", f"{SCOPE}&conformanceObject={cell}", headers={"authorization": ADMIN})
    commits, reads, floor, small = [], [], [], []
    for i in range(samples):
        status, _, ms = writers.install(cell, f"studio.dashboard.dura-lat-{i}", base)
        if status == 200:
            commits.append(ms)
        status, _, ms = request(base, "GET", "/v1/definitions/studio.dashboard.dura-warm-0", f"{SCOPE}&conformanceObject={cell}", headers={"authorization": ADMIN})
        if status == 200:
            reads.append(ms)
        status, _, ms = request(base, "GET", "/__probe/turn", "object=floor&n=1&ms=0&tag=floor")
        if status == 200:
            floor.append(ms)
        status, _, ms = request(base, "GET", "/__probe/count", "object=floor&tag=floor")
        if status == 200:
            small.append(ms)
    complete = all(len(values) == samples for values in (commits, reads, floor, small))
    return {"status": "pass" if complete else "fail", "samples": samples, "commitMs": summary(commits),
            "readMs": summary(reads), "oneRowCommitMs": summary(floor), "oneRowReadMs": summary(small)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--phase", default="kill,disk,load,latency")
    parser.add_argument("--rounds", type=int, default=20)
    parser.add_argument("--commits", type=int, default=10_000)
    parser.add_argument("--idle-seconds", type=int, default=3600)
    parser.add_argument("--settle", type=int, default=180)
    parser.add_argument("--rebuild-wait", type=int, default=240)
    parser.add_argument("--samples", type=int, default=300)
    parser.add_argument("--rehearsal-commits", type=int, default=20, help="commits per cell before the rehearsal's crashes")
    parser.add_argument("--storage-cells", type=int, default=20)
    parser.add_argument("--storage-commits", type=int, default=50, help="commits per cell in the storage phase")
    parser.add_argument("--state", type=Path)
    parser.add_argument("--project", default="norn-dura", help="Compose project name prefix; each run gets its own suffix")
    parser.add_argument("--lb-port", type=int, default=6420, help="loopback port of the engines' load balancer")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    binary = os.environ.get("NORN_CELL_RIVET")
    if not binary or not Path(binary).is_file():
        sys.exit("NORN_CELL_RIVET names a norn-cell-rivet binary built with --features conformance")
    reference = json.loads(args.reference.read_text())
    state = args.state or Path(tempfile.mkdtemp(prefix="norn-dura-"))
    state.mkdir(parents=True, exist_ok=True)
    project = f"{args.project}-{secrets.token_hex(6)}"
    cluster = Cluster(state, binary, reference["lineageKey"], project, args.lb_port)
    cluster.require_unused_project()
    results = {"engine": "rivetdev/engine 3.1.1 x3", "phases": {}}
    phases = {
        "kill": lambda: phase_kill(cluster, reference, state, args.rounds),
        "disk": lambda: phase_disk(cluster, reference, state, args.rebuild_wait),
        "load": lambda: phase_load(cluster, reference, state, args.commits, args.settle),
        "idle": lambda: phase_idle(cluster, reference, state, args.idle_seconds, args.settle),
        "latency": lambda: phase_latency(cluster, reference, state, args.samples),
        "rehearsal": lambda: phase_rehearsal(cluster, reference, state, args.rehearsal_commits),
        "storage": lambda: phase_storage(cluster, reference, state, args.storage_cells, args.storage_commits, args.settle),
    }
    selected = [name.strip() for name in args.phase.split(",") if name.strip()]
    unknown = [name for name in selected if name not in phases]
    if not selected or unknown:
        parser.error(f"unknown phase(s): {', '.join(unknown) if unknown else '<empty>'}; valid: {', '.join(phases)}")
    if "idle" in selected and args.idle_seconds <= 0:
        parser.error("--idle-seconds must be positive")
    if "storage" in selected and args.storage_cells <= 0:
        parser.error("--storage-cells must be positive")
    try:
        cluster.up()
        for name in selected:
            log(f"phase {name}")
            try:
                results["phases"][name] = phases[name]()
            except Exception as error:  # noqa: BLE001 - every failure is a row
                results["phases"][name] = {"status": "fail", "error": f"{type(error).__name__}: {error}"[:2000]}
            log(f"{name}: {json.dumps(results['phases'][name])[:1500]}")
            if args.out:
                args.out.write_text(json.dumps(results, indent=2))
    finally:
        cluster.down()
    print(json.dumps(results, indent=2))
    failed = any(phase.get("status") == "fail" for phase in results["phases"].values())
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
