"""Cell.export against a local stand-in for the `:export` route: Parquet with nulls kept, the
snapshot header, and the fallback signal when the host has no analytical engine."""

from __future__ import annotations

import io
import sys
import threading
import unittest
from decimal import Decimal
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn_client import AuthenticatedClient  # noqa: E402
from norn_regression.cell import Cell  # noqa: E402

TABLE = pa.table({
    "id": ["s1", "s2", "s3"], "version": pa.array([1, 1, 2], pa.uint64()),
    "x": pa.array([Decimal("1.5"), None, Decimal("2.25")], pa.decimal128(18, 4)),
    "y": pa.array([Decimal("3.0"), Decimal("4.0"), None], pa.decimal128(18, 4)),
})


class Handler(BaseHTTPRequestHandler):
    seen: list = []

    def do_GET(self):  # noqa: N802
        url = urlparse(self.path)
        Handler.seen.append((url.path, parse_qs(url.query), self.headers.get("x-api-key")))
        if url.path == "/v1/objects/lab.Sample:export":
            sink = io.BytesIO()
            pq.write_table(TABLE, sink)
            body = sink.getvalue()
            self.send_response(200)
            self.send_header("Norn-Snapshot", "c42")
            self.send_header("Norn-Withheld-Rows", "1")
        elif url.path == "/v1/objects/lab.Plain:export":
            # A host without the engine reads the path as an object route.
            body = b'{"code":"request.invalid","explanation":"unknown query parameter columns"}'
            self.send_response(400)
        elif url.path == "/v1/objects/lab.Bad:export":
            # The engine exists and refuses this request: not a reason to take the slow path.
            body = b'{"code":"request.invalid","explanation":"column nope is not declared"}'
            self.send_response(400)
        elif url.path == "/v1/objects/lab.Secret:export":
            body = b'{"code":"scope.denied"}'
            self.send_response(403)
        else:
            body = b'{"code":"not_found"}'
            self.send_response(404)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Export(unittest.TestCase):
    def setUp(self):
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.cell = Cell(AuthenticatedClient(base_url=url, token="key-1", prefix="", auth_header_name="x-api-key"), "o1", "e1")
        Handler.seen.clear()

    def tearDown(self):
        self.cell.client.get_httpx_client().close()
        self.server.shutdown()
        self.server.server_close()

    def test_parquet_rows_keep_nulls_and_carry_the_snapshot(self):
        frame = self.cell.export("lab.Sample", ["x", "y"])
        self.assertEqual(list(frame["id"]), ["s1", "s2", "s3"])
        self.assertTrue(pd.isna(frame.loc[1, "x"]) and pd.isna(frame.loc[2, "y"]))
        self.assertEqual(pd.to_numeric(frame["x"]).dropna().tolist(), [1.5, 2.25])
        self.assertEqual((frame.attrs["snapshot"], frame.attrs["withheld"]), ("c42", 1))
        path, query, key = Handler.seen[0]
        self.assertEqual((path, query["columns"], query["format"], query["orgId"], key), ("/v1/objects/lab.Sample:export", ["x,y"], ["parquet"], ["o1"], "key-1"))

    def test_no_engine_means_fall_back(self):
        self.assertIsNone(self.cell.export("lab.Other", ["x"]))
        self.assertIsNone(self.cell.export("lab.Plain", ["x"]))

    def test_a_refused_caller_is_not_a_fall_back(self):
        from norn_regression.cell import Refused
        with self.assertRaisesRegex(Refused, "403"):
            self.cell.export("lab.Secret", ["x"])
        with self.assertRaisesRegex(Refused, "column nope is not declared"):
            self.cell.export("lab.Bad", ["nope"])

    def test_the_read_path_is_recorded(self):
        self.assertEqual(self.cell.export("lab.Sample", ["x"]).attrs["read_path"], "export")


if __name__ == "__main__":
    unittest.main()
