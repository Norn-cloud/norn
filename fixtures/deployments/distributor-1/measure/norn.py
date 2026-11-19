"""A thin client for a running `norn dev` host: reads .norn/dev.json and the
tokens, and wraps the routes the deployment test uses."""
from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


class Norn:
    def __init__(self, project: Path, reviewer: bool = False):
        d = project / ".norn"
        desc = json.loads((d / "dev.json").read_text())
        self.base = (desc.get("url") or f"http://127.0.0.1:{desc.get('port')}").rstrip("/")
        self.token = (d / ("dev.reviewer.token" if reviewer else "dev.token")).read_text().strip()
        manifest = json.loads((project / "norn.json").read_text())
        self.scope = {"orgId": manifest["scope"]["orgId"], "environmentId": manifest["scope"]["environmentId"]}

    def call(self, method: str, path: str, body=None, query: dict | None = None, timeout: float = 600):
        q = urllib.parse.urlencode({**self.scope, **(query or {})})
        url = f"{self.base}{path}{'&' if '?' in path else '?'}{q}"
        req = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(), method=method,
                                     headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json", "Accept": "application/json"})
        t = time.perf_counter()
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                raw = r.read()
                return r.status, (json.loads(raw) if raw else None), time.perf_counter() - t
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw), time.perf_counter() - t
            except ValueError:
                return e.code, {"raw": raw[:500].decode(errors="replace")}, time.perf_counter() - t

    def objects(self, type_: str, limit: int = 500, max_pages: int = 400) -> tuple[list, int]:
        """Every readable object of a type, following whatever cursor the page names."""
        out, cursor, code = [], None, 0
        for _ in range(max_pages):
            code, body, _ = self.call("GET", f"/v1/objects/{type_}", query={"limit": limit, **({"cursor": cursor} if cursor else {})})
            if code != 200 or not isinstance(body, dict):
                return out, code
            items = body.get("objects") or body.get("items") or body.get("data") or []
            out += items
            cursor = body.get("nextCursor") or body.get("next_cursor") or body.get("cursor")
            if not cursor or not items:
                break
        return out, code


def rss_kb(pattern: str = "norn-dev-host") -> int:
    """Resident memory of the dev host process (Linux /proc)."""
    best = 0
    for p in Path("/proc").iterdir():
        if not p.name.isdigit():
            continue
        try:
            if pattern in (p / "cmdline").read_bytes().decode(errors="ignore"):
                for line in (p / "status").read_text().splitlines():
                    if line.startswith("VmRSS:"):
                        best = max(best, int(line.split()[1]))
        except OSError:
            continue
    return best
