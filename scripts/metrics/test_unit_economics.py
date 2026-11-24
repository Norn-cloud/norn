#!/usr/bin/env python3
"""Tests for unit_economics.py, pricing.json, probe_studio_bundle.py, and probe_binary_sizes.py."""

from __future__ import annotations

import contextlib
import csv
import gzip
import io
import json
import re
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


boat_nightly = _sibling("boat_nightly")
probe_binary = _sibling("probe_binary_sizes")
probe_studio = _sibling("probe_studio_bundle")
record = _sibling("record")
run_probes = _sibling("run_probes")
unit_econ = _sibling("unit_economics")

SHA = "6a19ebdf68898bbb51cd40c93af35ea3cf426906"


def _rec(metric: str, value: float, unit: str, statistic: str = "mean", *,
         workload: dict | None = None, measured_at: str = "2026-11-24T12:00:00Z",
         recorded_at: str = "2026-11-24T12:05:00Z", runner: str = "boat",
         supersedes: str | None = None, record_id: str | None = None) -> dict:
    item = {
        "schema": 1,
        "id": record_id or f"{metric}/{measured_at}",
        "metric": metric,
        "value": value,
        "unit": unit,
        "statistic": statistic,
        "commit": {"sha": SHA, "date": "2026-11-24T11:00:00Z", "ref": "main"},
        "measured_at": measured_at,
        "recorded_at": recorded_at,
        "measured_by": {"name": "metrics-nightly-boat/test"},
        "env": {"runner": runner, "vm_type": "default", "vcpu": 4},
        "workload": workload or {},
        "caveats": "synthetic unit test record",
        "estimate": False,
        "source": {"url": f"https://github.com/Norn-cloud/norn/commit/{SHA}"},
    }
    if supersedes:
        item["supersedes"] = supersedes
    return item


class PricingConfig(unittest.TestCase):
    def test_pricing_json_is_dated_and_sourced(self):
        pricing = unit_econ.load_pricing()
        self.assertEqual(pricing["schema"], 1)
        self.assertEqual(pricing["currency"], "USD")
        date_re = re.compile(r"^\d{4}-\d{2}-\d{2}$")

        cpx = pricing["compute"]["hetzner_cpx62"]
        self.assertEqual(cpx["vcpu"], 16)
        self.assertEqual(cpx["ram_gib"], 30.0)
        self.assertEqual(cpx["nvme_gb"], 610.0)
        self.assertGreater(cpx["monthly_usd"], 0)
        self.assertTrue(cpx["source"])
        self.assertRegex(cpx["as_of"], date_re)

        for section in ("storage", "bandwidth", "ci", "utilization", "workload_assumptions"):
            self.assertIn(section, pricing)
            for key, entry in pricing[section].items():
                with self.subTest(f"{section}.{key}"):
                    self.assertIn("value", entry)
                    self.assertTrue(entry.get("source"))
                    self.assertRegex(entry.get("as_of", ""), date_re)


class UnitEconomics(unittest.TestCase):
    def setUp(self):
        self.pricing = unit_econ.load_pricing()

    def _by_id(self, report: dict) -> dict[str, dict]:
        return {row["id"]: row for row in report["metrics"]}

    def test_empty_records_never_fabricates_inputs(self):
        report = unit_econ.compute_unit_economics([], self.pricing, source="empty")
        self.assertEqual(report["evidence"], "derived-estimate")
        for row in report["metrics"]:
            with self.subTest(row["id"]):
                self.assertIsNone(row["value"])
                self.assertTrue(row["missing_inputs"])
                self.assertEqual(row["evidence"], "derived-estimate")
                self.assertTrue(row["formula"])

    def test_full_metrics_calculation_and_formats(self):
        records = [
            _rec("cell.memory.rss_per_cell.bytes", 4 * 1024 * 1024, "bytes"),
            _rec("cell.commit.throughput.cmd_s", 500.0, "per_s"),
            _rec("cell.commit.size.bytes", 2048.0, "bytes"),
            _rec("decision.eval.throughput.ops_s", 25000.0, "per_s"),
            _rec("lake.parquet.commit_size.bytes", 128.0, "bytes"),
            _rec("lake.ingest.throughput.rows_s", 500000.0, "per_s", workload={"threads": 4}),
            _rec("studio.bundle.js.gzip_bytes", 350000, "bytes", "single"),
            _rec("studio.bundle.css.gzip_bytes", 50000, "bytes", "single"),
            _rec("ci.core.run.s", 1800.0, "s", "single", runner="github-hosted"),
        ]
        report = unit_econ.compute_unit_economics(records, self.pricing, source="synthetic")
        by_id = self._by_id(report)

        for metric_id in (
            "tenants_per_cpx62_by_ram",
            "tenants_per_cpx62_by_cpu",
            "tenants_per_cpx62_by_disk",
            "binding_constraint",
            "usd_per_active_tenant_month_compute",
            "usd_per_1m_commits_compute",
            "usd_per_1m_decisions_compute",
            "usd_per_1m_commits_hot_cell_month",
            "usd_per_1m_commits_cold_lake_month",
            "hot_to_cold_cost_ratio",
            "usd_per_100k_cmd_tenant_month_storage",
            "usd_per_1m_rows_landed",
            "usd_per_10k_studio_loads",
            "usd_per_pr_ci_estimate",
        ):
            self.assertIn(metric_id, by_id)
            self.assertEqual(by_id[metric_id]["missing_inputs"], [], metric_id)
            self.assertIsNotNone(by_id[metric_id]["value"], metric_id)
            self.assertEqual(by_id[metric_id]["evidence"], "derived-estimate")

        # Exact math checks:
        # RAM: floor((30 GiB * 0.70) / 4 MiB) = floor(21 * 1024 / 4) = 5376
        self.assertEqual(by_id["tenants_per_cpx62_by_ram"]["value"], 5376)
        # Disk: floor((610e9 * 0.75) / (2048 * 100000 * 12)) = floor(457.5e9 / 2.4576e9) = 186
        self.assertEqual(by_id["tenants_per_cpx62_by_disk"]["value"], 186)
        self.assertEqual(by_id["binding_constraint"]["value"], "disk")

        # Hot storage: (2048 * 1e6 / 1e9) * 3 * 0.052 = 0.319488
        self.assertAlmostEqual(by_id["usd_per_1m_commits_hot_cell_month"]["value"], 0.319488, places=5)
        # Cold storage: (128 * 1e6 / 1e9) * 0.015 = 0.00192
        self.assertAlmostEqual(by_id["usd_per_1m_commits_cold_lake_month"]["value"], 0.00192, places=6)
        # Ratio: 0.319488 / 0.00192 = 166.4
        self.assertAlmostEqual(by_id["hot_to_cold_cost_ratio"]["value"], 166.4, places=1)
        # 100k commits storage: 0.1 * 0.319488 = 0.0319488
        self.assertAlmostEqual(by_id["usd_per_100k_cmd_tenant_month_storage"]["value"], 0.0319488, places=6)

        # Studio loads: ((350000 + 50000) * 10000 / 1e9) * 0.085 = 4.0 * 0.085 = 0.34
        self.assertAlmostEqual(by_id["usd_per_10k_studio_loads"]["value"], 0.34, places=5)
        # CI: (1800 / 3600) * 3.0 * 0.037 = 0.0555
        self.assertAlmostEqual(by_id["usd_per_pr_ci_estimate"]["value"], 0.0555, places=5)

        # JSON / CSV / Markdown round-trips
        parsed_json = json.loads(unit_econ.format_json(report))
        self.assertEqual(len(parsed_json["metrics"]), len(report["metrics"]))

        csv_rows = list(csv.DictReader(io.StringIO(unit_econ.format_csv(report))))
        self.assertEqual(len(csv_rows), len(report["metrics"]))
        self.assertEqual(csv_rows[0]["evidence"], "derived-estimate")
        self.assertTrue(csv_rows[0]["input_measured_at"])

        md = unit_econ.format_markdown(report)
        self.assertIn("# Norn Unit-Economics Estimate", md)
        self.assertIn("`tenants_per_cpx62_by_ram`", md)
        self.assertIn("`derived-estimate`", md)

    def test_binding_constraint_switches_to_ram_and_cpu(self):
        # Huge RAM per cell -> RAM binds
        ram_bound = unit_econ.compute_unit_economics(
            [
                _rec("cell.memory.rss_per_cell.bytes", 512 * 1024 * 1024, "bytes"),
                _rec("cell.commit.throughput.cmd_s", 5000.0, "per_s"),
                _rec("cell.commit.size.bytes", 64.0, "bytes"),
            ],
            self.pricing,
        )
        self.assertEqual(self._by_id(ram_bound)["binding_constraint"]["value"], "ram")

        # Very slow commit throughput -> CPU binds
        cpu_bound = unit_econ.compute_unit_economics(
            [
                _rec("cell.memory.rss_per_cell.bytes", 1024 * 1024, "bytes"),
                _rec("cell.commit.throughput.cmd_s", 1.0, "per_s"),
                _rec("cell.commit.size.bytes", 64.0, "bytes"),
            ],
            self.pricing,
        )
        self.assertEqual(self._by_id(cpu_bound)["binding_constraint"]["value"], "cpu")

    def test_fallback_metrics_and_supersedes_honesty(self):
        bad = _rec("ci.core.run.s", 99999.0, "s", "single", record_id="ci/bad",
                   recorded_at="2026-11-24T12:00:00Z")
        good = _rec("ci.core.run.s", 3600.0, "s", "single", record_id="ci/good",
                    recorded_at="2026-11-24T12:01:00Z", supersedes="ci/bad")
        records = [
            bad,
            good,
            _rec("host.celld.pss_per_cell.bytes", 2_569_011, "bytes"),
            _rec("host.celld.commit_one_row.ms", 5.0, "ms", "p50"),
            _rec("analytics.duckdb.load_parquet_1m.s", 2.0, "s", "single",
                 workload={"rows": 1_000_000, "threads": 4}),
        ]
        report = unit_econ.compute_unit_economics(records, self.pricing)
        by_id = self._by_id(report)

        # Superseded bad value must be ignored
        self.assertAlmostEqual(by_id["usd_per_pr_ci_estimate"]["value"], 3.0 * 0.037, places=5)
        # Fallback metric names are recorded verbatim for auditability
        self.assertEqual(by_id["tenants_per_cpx62_by_ram"]["inputs"][0]["metric"], "host.celld.pss_per_cell.bytes")
        self.assertEqual(by_id["tenants_per_cpx62_by_cpu"]["inputs"][0]["metric"], "host.celld.commit_one_row.ms")
        self.assertEqual(by_id["usd_per_1m_rows_landed"]["inputs"][0]["metric"], "analytics.duckdb.load_parquet_1m.s")
        # Missing disk/cold-lake/studio/decision remain honestly null
        self.assertIsNone(by_id["binding_constraint"]["value"])
        self.assertEqual(by_id["binding_constraint"]["missing_inputs"], ["cell.commit.size.bytes"])
        self.assertIsNone(by_id["usd_per_10k_studio_loads"]["value"])
        self.assertIsNone(by_id["usd_per_1m_decisions_compute"]["value"])

    def test_cli_reads_jsonl_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            jsonl = Path(tmp) / "m.jsonl"
            out = Path(tmp) / "out.csv"
            jsonl.write_text(json.dumps(_rec("ci.core.run.s", 3600.0, "s", "single")) + "\n", encoding="utf-8")
            code = unit_econ.main(["--jsonl", str(jsonl), "--format", "csv", "--out", str(out)])
            self.assertEqual(code, 0)
            rows = list(csv.DictReader(out.read_text(encoding="utf-8").splitlines()))
            ci = next(r for r in rows if r["id"] == "usd_per_pr_ci_estimate")
            self.assertAlmostEqual(float(ci["value"]), 0.111, places=5)


class NewProbes(unittest.TestCase):
    def _validate_metric_lines(self, probe_name: str, items: list[dict]) -> None:
        lines = 'ENV {"vcpu": 4, "cpu_model": "test"}\n'
        for item in items:
            payload = {
                "probe": probe_name,
                "measured_at": "2026-11-24T12:00:00Z",
                "caveats": "unit test probe output",
                **item,
            }
            lines += f"METRIC {json.dumps(payload, sort_keys=True)}\n"
        rows, notes = boat_nightly.records_from(lines, SHA, "2026-11-24T11:00:00Z", "bx_test1234", "snap", "default")
        self.assertEqual(notes, [])
        self.assertEqual(len(rows), len(items))
        schema = record.load_schema()
        for row in rows:
            completed = record.complete(row)
            self.assertEqual(record.record_errors(completed, schema), [], row["metric"])

    def test_studio_bundle_probe_measurements_and_schema(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp) / "dist"
            (dist / "assets").mkdir(parents=True)
            (dist / "widgets").mkdir(parents=True)
            js_a = b"console.log('hello');\n" * 200
            js_b = b"export const x = 1;\n" * 40
            css = b"body { color: black; }\n" * 50
            html_doc = b"<!doctype html><html></html>"
            (dist / "assets" / "index-abc.js").write_bytes(js_a)
            (dist / "widgets" / "chat.mjs").write_bytes(js_b)
            (dist / "assets" / "style.css").write_bytes(css)
            (dist / "index.html").write_bytes(html_doc)

            items = probe_studio.measure_dist(dist)
            by_metric = {item["metric"]: item for item in items}
            self.assertEqual(
                set(by_metric),
                {
                    "studio.bundle.js.raw_bytes",
                    "studio.bundle.js.gzip_bytes",
                    "studio.bundle.js.largest_chunk_raw_bytes",
                    "studio.bundle.js.largest_chunk_gzip_bytes",
                    "studio.bundle.css.gzip_bytes",
                    "studio.bundle.total_dist_bytes",
                },
            )
            self.assertEqual(by_metric["studio.bundle.js.raw_bytes"]["value"], len(js_a) + len(js_b))
            self.assertEqual(by_metric["studio.bundle.js.largest_chunk_raw_bytes"]["value"], len(js_a))
            self.assertEqual(
                by_metric["studio.bundle.js.gzip_bytes"]["value"],
                len(gzip.compress(js_a, compresslevel=9, mtime=0))
                + len(gzip.compress(js_b, compresslevel=9, mtime=0)),
            )
            self.assertEqual(
                by_metric["studio.bundle.css.gzip_bytes"]["value"],
                len(gzip.compress(css, compresslevel=9, mtime=0)),
            )
            self.assertEqual(
                by_metric["studio.bundle.total_dist_bytes"]["value"],
                len(js_a) + len(js_b) + len(css) + len(html_doc),
            )
            self._validate_metric_lines("studio-bundle-size", items)

            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(probe_studio.main(["--dist", str(dist)]), 0)
            self.assertEqual(len([l for l in out.getvalue().splitlines() if l.startswith("METRIC ")]), 6)

    def test_studio_bundle_probe_fails_without_js(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp) / "dist"
            dist.mkdir()
            (dist / "index.html").write_bytes(b"<html></html>")
            with self.assertRaises(RuntimeError):
                probe_studio.measure_dist(dist)

    def test_binary_sizes_probe_measurements_and_schema(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "target"
            rivet = target / "rivet"
            (target / "release").mkdir(parents=True)
            (rivet / "release").mkdir(parents=True)
            (target / "wasm32-unknown-unknown" / "wasm-release").mkdir(parents=True)

            cli_bytes = b"\x7fELF" + b"A" * 1000
            rivet_bytes = b"\x7fELF" + b"B" * 2000
            checker_bytes = b"\x00asm" + b"C" * 3000
            cell_do_bytes = b"\x00asm" + b"D" * 4000
            (target / "release" / "norn").write_bytes(cli_bytes)
            (rivet / "release" / "norn-cell-rivet").write_bytes(rivet_bytes)
            (target / "wasm32-unknown-unknown" / "wasm-release" / "norn_checker_wasm.wasm").write_bytes(checker_bytes)
            (target / "wasm32-unknown-unknown" / "wasm-release" / "norn_cell_do.wasm").write_bytes(cell_do_bytes)

            items = probe_binary.measure_artifacts(target, rivet)
            by_metric = {item["metric"]: item for item in items}
            self.assertEqual(
                set(by_metric),
                {
                    "binary.norn_cli.bytes",
                    "binary.norn_cell_rivet.bytes",
                    "binary.norn_checker_wasm.bytes",
                    "binary.norn_checker_wasm.gzip_bytes",
                    "binary.norn_cell_do_wasm.bytes",
                    "binary.norn_cell_do_wasm.gzip_bytes",
                },
            )
            self.assertEqual(by_metric["binary.norn_cli.bytes"]["value"], len(cli_bytes))
            self.assertEqual(by_metric["binary.norn_cell_rivet.bytes"]["value"], len(rivet_bytes))
            self.assertEqual(by_metric["binary.norn_checker_wasm.bytes"]["value"], len(checker_bytes))
            self.assertEqual(
                by_metric["binary.norn_checker_wasm.gzip_bytes"]["value"],
                len(gzip.compress(checker_bytes, compresslevel=9, mtime=0)),
            )
            self.assertEqual(by_metric["binary.norn_cell_do_wasm.bytes"]["value"], len(cell_do_bytes))
            self.assertEqual(
                by_metric["binary.norn_cell_do_wasm.gzip_bytes"]["value"],
                len(gzip.compress(cell_do_bytes, compresslevel=9, mtime=0)),
            )
            self._validate_metric_lines("binary-and-wasm-sizes", items)

    def test_binary_sizes_probe_fails_on_missing_or_empty(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "target"
            with self.assertRaises(RuntimeError):
                probe_binary.measure_artifacts(target, target / "rivet")

    def test_probes_json_registers_new_probes_and_requires_exist(self):
        probes = json.loads((HERE / "probes.json").read_text(encoding="utf-8"))["probes"]
        by_name = {probe["name"]: probe for probe in probes}
        for name in ("studio-bundle-size", "binary-and-wasm-sizes", "engine-core-metrics"):
            self.assertIn(name, by_name)
            for req in by_name[name]["requires"]:
                self.assertTrue((REPO / req).exists(), f"{name} missing required path {req}")


if __name__ == "__main__":
    unittest.main()
