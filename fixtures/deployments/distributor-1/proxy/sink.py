#!/usr/bin/env python3
"""Webhook sink: appends every delivered Saleor webhook body, as one JSON line,
to OUT. Duplicates and out-of-order deliveries are kept: they are the input.
Usage: sink.py PORT OUT_JSONL"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

OUT = sys.argv[2]


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        try:
            doc = json.loads(body)
        except ValueError:
            doc = {"raw": body[:200].decode(errors="replace")}
        with open(OUT, "a") as f:
            f.write(json.dumps(doc) + "\n")
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
