#!/usr/bin/env python3
"""A mutating HTTP proxy: the application-level half of the hostile network.

Toxiproxy (TCP latency, resets, timeouts) sits behind it; this proxy breaks
what TCP chaos cannot: pagination, rate limits, payload shape, and webhooks.

  route  --listen PORT --upstream http://host:port --kind erpnext|odoo|saleor
  chaos  (seeded, per route) from --config JSON:
    page_skip    p  serve the page after the one asked for (ERPNext limit_start, Odoo offset)
    page_repeat  p  serve the page before the one asked for
    storm_every  n  every n-th request starts a 429 storm of storm_len responses (Retry-After: 1)
    drop_field   p  delete one random key from one random record of a list response
    drift_after  n  after n requests, rename fields per `drift` ({"old": "new"}) in every response
  webhook relay  --listen PORT --upstream URL: delivers each webhook 1-3 times, holding some
                 back so they arrive out of order (dup_p, hold_p).

Every mutation is counted; GET /__chaos returns the counters as JSON.
Stdlib only, one thread per connection. Usage: hostile.py CONFIG_JSON
"""
from __future__ import annotations

import json
import random
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

COUNTS: dict[str, dict[str, int]] = {}
LOCK = threading.Lock()


def bump(route: str, key: str, n: int = 1) -> None:
    with LOCK:
        COUNTS.setdefault(route, {}).setdefault(key, 0)
        COUNTS[route][key] += n


def records_of(body):
    """The list of records in a response, per API: ERPNext {data:[...]}, Odoo {result:[...]}, Saleor edges."""
    if isinstance(body, dict):
        if isinstance(body.get("data"), list):
            return body["data"]
        if isinstance(body.get("result"), list):
            return body["result"]
        if isinstance(body.get("message"), list):
            return body["message"]
        if isinstance(body.get("data"), dict):
            for v in body["data"].values():
                if isinstance(v, dict) and isinstance(v.get("edges"), list):
                    return [e.get("node") for e in v["edges"] if isinstance(e, dict)]
    return []


def rename(obj, drift: dict):
    if isinstance(obj, dict):
        return {drift.get(k, k): rename(v, drift) for k, v in obj.items()}
    if isinstance(obj, list):
        return [rename(v, drift) for v in obj]
    return obj


def make_route(name: str, cfg: dict):
    rng = random.Random(cfg.get("seed", 7))
    state = {"n": 0, "storm": 0}
    upstream = cfg["upstream"].rstrip("/")
    kind = cfg["kind"]

    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):  # quiet
            pass

        def _reply(self, code: int, body: bytes, headers: dict) -> None:
            self.send_response(code)
            for k, v in headers.items():
                if k.lower() not in ("content-length", "transfer-encoding", "connection", "content-encoding"):
                    self.send_header(k, v)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _handle(self) -> None:
            if self.path == "/__chaos":
                self._reply(200, json.dumps(COUNTS).encode(), {"Content-Type": "application/json"})
                return
            length = int(self.headers.get("Content-Length") or 0)
            data = self.rfile.read(length) if length else None
            with LOCK:
                state["n"] += 1
                n = state["n"]
                if state["storm"] > 0:
                    state["storm"] -= 1
                    storming = True
                elif cfg.get("storm_every") and n % cfg["storm_every"] == 0:
                    state["storm"] = cfg.get("storm_len", 5) - 1
                    storming = True
                else:
                    storming = False
            if storming:
                bump(name, "429")
                self._reply(429, b'{"error":"rate limited"}', {"Retry-After": "1", "Content-Type": "application/json"})
                return
            path = self.path
            # pagination chaos rewrites the request
            if kind == "erpnext" and "limit_start=" in path:
                u = urllib.parse.urlsplit(path)
                q = dict(urllib.parse.parse_qsl(u.query))
                size = int(q.get("limit_page_length", 20) or 20)
                start = int(q.get("limit_start", 0))
                r = rng.random()
                if r < cfg.get("page_skip", 0):
                    q["limit_start"] = str(start + size)
                    bump(name, "page_skip")
                elif r < cfg.get("page_skip", 0) + cfg.get("page_repeat", 0) and start >= size:
                    q["limit_start"] = str(start - size)
                    bump(name, "page_repeat")
                path = urllib.parse.urlunsplit((u.scheme, u.netloc, u.path, urllib.parse.urlencode(q), u.fragment))
            if kind == "odoo" and data and b'"search_read"' in data:
                body = json.loads(data)
                kw = body.get("params", {}).get("kwargs", {})
                if "offset" in kw and kw.get("limit"):
                    r = rng.random()
                    if r < cfg.get("page_skip", 0):
                        kw["offset"] += kw["limit"]
                        bump(name, "page_skip")
                    elif r < cfg.get("page_skip", 0) + cfg.get("page_repeat", 0) and kw["offset"] >= kw["limit"]:
                        kw["offset"] -= kw["limit"]
                        bump(name, "page_repeat")
                    data = json.dumps(body).encode()
            req = urllib.request.Request(upstream + path, data=data, method=self.command,
                                         headers={k: v for k, v in self.headers.items() if k.lower() not in ("host", "content-length", "accept-encoding")})
            try:
                with urllib.request.urlopen(req, timeout=60) as resp:
                    code, rbody, rh = resp.status, resp.read(), dict(resp.headers)
            except urllib.error.HTTPError as e:
                code, rbody, rh = e.code, e.read(), dict(e.headers)
            except Exception as e:  # upstream reset by toxiproxy
                bump(name, "upstream_error")
                self._reply(502, json.dumps({"error": str(e)}).encode(), {"Content-Type": "application/json"})
                return
            if code == 200 and "json" in rh.get("Content-Type", ""):
                try:
                    body = json.loads(rbody)
                except ValueError:
                    body = None
                if body is not None:
                    recs = [r for r in records_of(body) if isinstance(r, dict) and r]
                    if recs and rng.random() < cfg.get("drop_field", 0):
                        rec = rng.choice(recs)
                        key = rng.choice(sorted(rec))
                        del rec[key]
                        bump(name, "drop_field")
                    if cfg.get("drift_after") and n > cfg["drift_after"] and cfg.get("drift"):
                        body = rename(body, cfg["drift"])
                        bump(name, "drift")
                    rbody = json.dumps(body).encode()
            bump(name, "forwarded")
            self._reply(code, rbody, rh)

        do_GET = do_POST = do_PUT = do_DELETE = _handle

    return H


def make_relay(name: str, cfg: dict):
    rng = random.Random(cfg.get("seed", 11))
    held: list[tuple[bytes, dict]] = []

    def deliver(body: bytes, headers: dict) -> None:
        req = urllib.request.Request(cfg["upstream"], data=body, method="POST", headers=headers)
        try:
            urllib.request.urlopen(req, timeout=30).read()
            bump(name, "delivered")
        except Exception:
            bump(name, "delivery_failed")

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
            hdr = {k: v for k, v in self.headers.items() if k.lower().startswith(("saleor", "content-type", "x-"))}
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()
            bump(name, "received")
            if rng.random() < cfg.get("hold_p", 0.2):
                held.append((body, hdr))  # released after the next one: out of order
                bump(name, "held")
                return
            copies = 1 + (rng.random() < cfg.get("dup_p", 0.3)) + (rng.random() < cfg.get("dup_p", 0.3) / 3)
            for _ in range(copies):
                deliver(body, hdr)
            bump(name, "duplicates", copies - 1)
            while held:
                deliver(*held.pop(0))

    return H


def main() -> None:
    cfg = json.loads(open(sys.argv[1]).read())
    servers = []
    for name, r in cfg["routes"].items():
        handler = make_relay(name, r) if r["kind"] == "relay" else make_route(name, r)
        # the webhook relay must be reachable from the Saleor worker container
        s = ThreadingHTTPServer(("0.0.0.0" if r["kind"] == "relay" else "127.0.0.1", r["listen"]), handler)
        threading.Thread(target=s.serve_forever, daemon=True).start()
        servers.append(s)
    print(json.dumps({"listening": {n: r["listen"] for n, r in cfg["routes"].items()}}), flush=True)
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
