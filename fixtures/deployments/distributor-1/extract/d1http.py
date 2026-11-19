"""Small stdlib HTTP client shared by the seeders and extractors: JSON, cookies,
bounded retries on 429/5xx/resets, and counters for the gap report."""
from __future__ import annotations

import http.cookiejar
import json
import time
import urllib.error
import urllib.request

STATS: dict[str, int] = {}


def bump(key: str, n: int = 1) -> None:
    STATS[key] = STATS.get(key, 0) + n


class Client:
    def __init__(self, base: str, name: str, headers: dict | None = None, retries: int = 8):
        self.base, self.name, self.retries = base.rstrip("/"), name, retries
        self.headers = {"Content-Type": "application/json", "Accept": "application/json", **(headers or {})}
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def call(self, method: str, path: str, body=None, raw: bool = False, headers: dict | None = None):
        data = None if body is None else (body if isinstance(body, bytes) else json.dumps(body).encode())
        delay = 0.25
        for attempt in range(self.retries + 1):
            # ERPNext doctypes have spaces ("Sales Order"); urllib refuses a raw space in a URL
            req = urllib.request.Request(self.base + path.replace(" ", "%20"), data=data, method=method, headers={**self.headers, **(headers or {})})
            try:
                with self.opener.open(req, timeout=120) as r:
                    out = r.read()
                    bump(f"{self.name}.ok")
                    return out if raw else (json.loads(out) if out else None)
            except urllib.error.HTTPError as e:
                payload = e.read()
                if e.code in (429, 502, 503, 504) and attempt < self.retries:
                    bump(f"{self.name}.retry_{e.code}")
                    time.sleep(float(e.headers.get("Retry-After") or delay))
                    delay = min(delay * 2, 8)
                    continue
                bump(f"{self.name}.http_{e.code}")
                raise RuntimeError(f"{self.name} {method} {path[:120]} -> {e.code}: {payload[:400]!r}") from None
            except (urllib.error.URLError, ConnectionError, TimeoutError, OSError) as e:
                if attempt < self.retries:
                    bump(f"{self.name}.retry_conn")
                    time.sleep(delay)
                    delay = min(delay * 2, 8)
                    continue
                bump(f"{self.name}.conn_failed")
                raise RuntimeError(f"{self.name} {method} {path[:120]}: {e}") from None
        raise AssertionError("unreachable")

    def get(self, path, **k):
        return self.call("GET", path, **k)

    def post(self, path, body=None, **k):
        return self.call("POST", path, body, **k)

    def put(self, path, body=None, **k):
        return self.call("PUT", path, body, **k)
