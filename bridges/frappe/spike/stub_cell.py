"""B0 spike only: a stand-in cell serving the host's object read route shapes.

`GET /v1/objects/{type}` (grid query page), `GET /v1/objects/{type}/{id}`
(one record), `POST /v1/actions/{action}` (records the call; the real host
does not serve actions yet, #805). Every request is logged with its decoded
principal so the transcript shows what reached the "cell".
"""

import base64
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

RECORDS = {
    "demo.Repair": {
        "R-001": {"title": "Resize ring", "customer": "Synthetic Customer A", "status": "draft", "estimate": "40.00"},
        "R-002": {"title": "Polish bracelet", "customer": "Synthetic Customer B", "status": "received", "estimate": "25.00"},
        "R-003": {"title": "Replace clasp", "customer": "Synthetic Customer A", "status": "repaired", "estimate": "15.50"},
    }
}
VERSIONS = {}


def principal(header):
    if not header or not header.startswith("Bearer "):
        return None
    try:
        payload = header.split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
        return f"{claims['sub']} role={claims['norn']['role']}"
    except Exception:
        return "unparsed"


def record(kind, key, values):
    return {"objectType": kind, "id": key, "scope": "org_demo/dev", "version": VERSIONS.get(key, 1),
            "values": values, "withheld": False, "withheldProperties": []}


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, document):
        body = json.dumps(document).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log(self, extra=""):
        print(f"STUB {self.command} {unquote(self.path)} as={principal(self.headers.get('authorization'))} {extra}", flush=True)

    def do_GET(self):
        url = urlsplit(self.path)
        query = {k: v[0] for k, v in parse_qs(url.query).items()}
        parts = [unquote(p) for p in url.path.split("/")[3:]]
        self.log()
        if not url.path.startswith("/v1/objects/") or not parts or parts[0] not in RECORDS:
            return self.reply(404, {"code": "route.not_found", "message": "no such route"})
        rows = RECORDS[parts[0]]
        if len(parts) == 2:
            if parts[1] not in rows:
                return self.reply(404, {"code": "object.not_found", "message": "no such object"})
            return self.reply(200, record(parts[0], parts[1], rows[parts[1]]))
        clauses = json.loads(query.get("filter", '{"all":[]}'))["all"]
        keep = [k for k, v in sorted(rows.items())
                if all((c["op"] == "eq" and v.get(c["property"]) == c["value"]) or
                       (c["op"] == "in" and v.get(c["property"]) in c["value"]) for c in clauses)]
        limit = int(query.get("limit", "50"))
        page = keep[:limit]
        return self.reply(200, {"objectType": parts[0], "records": [record(parts[0], k, rows[k]) for k in page],
                                "quarantineCount": 0, "query": {"sort": [], "filter": None}, "next": None,
                                "total": {"count": len(keep), "approximate": False}})

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        self.log(f"body={json.dumps(body, sort_keys=True)}")
        key = body.get("id")
        rows = RECORDS.get(body.get("objectType"), {})
        if key in rows and body.get("values"):
            rows[key].update({k: v for k, v in body["values"].items() if v is not None})
            VERSIONS[key] = VERSIONS.get(key, 1) + 1
        return self.reply(200, {"receipt": f"stub-receipt-{VERSIONS.get(key, 1)}", "commitSeq": VERSIONS.get(key, 1)})

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", int(sys.argv[1]) if len(sys.argv) > 1 else 8787), Handler).serve_forever()
