#!/usr/bin/env python3
"""Tests for unit_economics.py, pricing.json, probe_studio_bundle.py, and probe_binary_sizes.py."""

from __future__ import annotations

import contextlib
import copy
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
unit_econ = _sibling("unit_economics")

SHA = "6a19ebdf68898bbb51cd40c93af35ea3cf426906"


def _rec(metric: str, value: float, unit: str, statistic: str = "single", *,
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
    def test_pricing_json_pins_hetzner_eu_tariff_and_sources(self):
        pricing = unit_econ.load_pricing()
        self.assertEqual(pricing["schema"], 1)
        self.assertEqual(pricing["currency"], "USD")
        date_re = re.compile(r"^\d{4}-\d{2}-\d{2}$")

        cpx = pricing["compute"]["hetzner_cpx62"]
        self.assertEqual(cpx["vcpu"], 16)
        self.assertEqual(cpx["ram_gib"], 30.0)
        self.assertEqual(cpx["nvme_gb"], 610.0)
        self.assertEqual(cpx["region"], "EU (NBG1)")
        self.assertAlmostEqual(cpx["monthly_eur"], 130.49, places=2)
        self.assertAlmostEqual(cpx["eur_to_usd"], 1.1724, places=4)
        self.assertAlmostEqual(cpx["monthly_usd"], round(cpx["monthly_eur"] * cpx["eur_to_usd"], 2), places=2)
        self.assertAlmostEqual(cpx["monthly_usd"], 152.99, places=2)
        self.assertIn("excl. VAT, incl. IPv4", cpx["source"])
        self.assertIn("https://www.hetzner.com/cloud/regular-performance/", cpx["source"])
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
        self.assertEqual(report["historical_proxies"], [])
        for row in report["metrics"]:
            with self.subTest(row["id"]):
                self.assertIsNone(row["value"])
                self.assertTrue(row["missing_inputs"])
                self.assertEqual(row["evidence"], "derived-estimate")
                self.assertTrue(row["formula"])
                self.assertTrue(row["pricing_as_of"])

    def test_full_metrics_calculation_and_formats(self):
        records = [
            _rec("cell.memory.rss_per_cell.bytes", 4 * 1024 * 1024, "bytes"),
            _rec("cell.commit.throughput.cmd_s", 500.0, "per_s"),
            _rec("cell.commit.size.bytes", 2048.0, "bytes"),
            _rec("decision.eval.throughput.ops_s", 25000.0, "per_s"),
            _rec("lake.parquet.commit_size.bytes", 128.0, "bytes"),
            _rec("lake.ingest.throughput.rows_s", 500000.0, "per_s", workload={"threads": 4}),
            _rec("studio.bundle.initial_js.gzip_bytes", 350000, "bytes"),
            _rec("studio.bundle.initial_css.gzip_bytes", 50000, "bytes"),
            _rec("ci.core.run.s", 1800.0, "s", runner="github-hosted"),
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

        # RAM: floor((30 GiB * 0.70) / 4 MiB) = 5376
        self.assertEqual(by_id["tenants_per_cpx62_by_ram"]["value"], 5376)
        # Disk: floor((610e9 * 0.75) / (2048 * 100000 * 12)) = 186
        self.assertEqual(by_id["tenants_per_cpx62_by_disk"]["value"], 186)
        self.assertEqual(by_id["binding_constraint"]["value"], "disk")
        # Compute per tenant-month uses $152.99 * 3 / 5376
        self.assertAlmostEqual(
            by_id["usd_per_active_tenant_month_compute"]["value"],
            (152.99 * 3) / 5376,
            places=5,
        )

        # Hot storage: (2048 * 1e6 / 1e9) * 3 * 0.052 = 0.319488
        self.assertAlmostEqual(by_id["usd_per_1m_commits_hot_cell_month"]["value"], 0.319488, places=5)
        # Cold storage: (128 * 1e6 / 1e9) * 0.015 = 0.00192
        self.assertAlmostEqual(by_id["usd_per_1m_commits_cold_lake_month"]["value"], 0.00192, places=6)
        self.assertAlmostEqual(by_id["hot_to_cold_cost_ratio"]["value"], 166.4, places=1)
        self.assertAlmostEqual(by_id["usd_per_100k_cmd_tenant_month_storage"]["value"], 0.0319488, places=6)

        # Studio initial cold loads: ((350000 + 50000) * 10000 / 1e9) * 0.085 = 0.34
        self.assertAlmostEqual(by_id["usd_per_10k_studio_loads"]["value"], 0.34, places=5)
        # CI: (1800 / 3600) * 3.0 * 0.037 = 0.0555
        self.assertAlmostEqual(by_id["usd_per_pr_ci_estimate"]["value"], 0.0555, places=5)

        parsed_json = json.loads(unit_econ.format_json(report))
        self.assertEqual(len(parsed_json["metrics"]), len(report["metrics"]))

        csv_rows = list(csv.DictReader(io.StringIO(unit_econ.format_csv(report))))
        self.assertEqual(len(csv_rows), len(report["metrics"]))
        self.assertEqual(csv_rows[0]["evidence"], "derived-estimate")
        self.assertTrue(csv_rows[0]["input_measured_at"])

        md = unit_econ.format_markdown(report)
        self.assertIn("# Norn Unit-Economics Estimate", md)
        self.assertIn("`tenants_per_cpx62_by_ram`", md)
        self.assertIn("$152.99/mo", md)

    def test_binding_constraint_switches_to_ram_and_cpu(self):
        ram_bound = unit_econ.compute_unit_economics(
            [
                _rec("cell.memory.rss_per_cell.bytes", 512 * 1024 * 1024, "bytes"),
                _rec("cell.commit.throughput.cmd_s", 5000.0, "per_s"),
                _rec("cell.commit.size.bytes", 64.0, "bytes"),
            ],
            self.pricing,
        )
        self.assertEqual(self._by_id(ram_bound)["binding_constraint"]["value"], "ram")

        cpu_bound = unit_econ.compute_unit_economics(
            [
                _rec("cell.memory.rss_per_cell.bytes", 1024 * 1024, "bytes"),
                _rec("cell.commit.throughput.cmd_s", 1.0, "per_s"),
                _rec("cell.commit.size.bytes", 64.0, "bytes"),
            ],
            self.pricing,
        )
        self.assertEqual(self._by_id(cpu_bound)["binding_constraint"]["value"], "cpu")

    def test_retired_celld_never_populates_live_rivet_rows(self):
        bad = _rec("ci.core.run.s", 99999.0, "s", record_id="ci/bad", recorded_at="2026-11-24T12:00:00Z")
        good = _rec("ci.core.run.s", 3600.0, "s", record_id="ci/good",
                    recorded_at="2026-11-24T12:01:00Z", supersedes="ci/bad")
        records = [
            bad,
            good,
            _rec("host.celld.pss_per_cell.bytes", 2_569_011, "bytes", "mean"),
            _rec("host.celld.commit_one_row.ms", 4.92, "ms", "p50"),
            _rec("analytics.duckdb.load_parquet_1m.s", 2.0, "s", workload={"rows": 1_000_000, "threads": 4}),
        ]
        report = unit_econ.compute_unit_economics(records, self.pricing)
        by_id = self._by_id(report)

        # Superseded bad value must be ignored
        self.assertAlmostEqual(by_id["usd_per_pr_ci_estimate"]["value"], 3.0 * 0.037, places=5)
        # Live Rivet compute/RAM rows MUST stay null when only retired celld series exist
        for live_id in (
            "tenants_per_cpx62_by_ram",
            "tenants_per_cpx62_by_cpu",
            "usd_per_active_tenant_month_compute",
            "usd_per_1m_commits_compute",
            "binding_constraint",
        ):
            self.assertIsNone(by_id[live_id]["value"], live_id)
            self.assertTrue(by_id[live_id]["missing_inputs"], live_id)

        # Historical proxy appears strictly in its own section with explicit labeling in JSON, CSV, and MD
        self.assertEqual(len(report["historical_proxies"]), 1)
        proxy = report["historical_proxies"][0]
        self.assertEqual(proxy["id"], "retired_celld_reference_tenants_per_cpx62_by_ram")
        self.assertEqual(proxy["category"], "historical_proxy")
        self.assertEqual(proxy["evidence"], "historical-proxy-retired-celld")
        self.assertEqual(proxy["value"], 8777)

        csv_rows = list(csv.DictReader(io.StringIO(unit_econ.format_csv(report))))
        proxy_csv = next(r for r in csv_rows if r["id"] == proxy["id"])
        self.assertEqual(proxy_csv["category"], "historical_proxy")
        self.assertEqual(proxy_csv["evidence"], "historical-proxy-retired-celld")

        md = unit_econ.format_markdown(report)
        self.assertIn("## Historical Proxy Reference (Retired Hosts", md)
        self.assertIn("`retired_celld_reference_tenants_per_cpx62_by_ram`", md)

    def test_zero_css_is_valid_and_invalid_divisors_are_guarded(self):
        # Legitimate 0 initial CSS bytes must compute cleanly, NOT be treated as missing
        report = unit_econ.compute_unit_economics(
            [
                _rec("studio.bundle.initial_js.gzip_bytes", 200_000, "bytes"),
                _rec("studio.bundle.initial_css.gzip_bytes", 0, "bytes"),
                # Missing threads on lake ingest must NOT silently default to 1
                _rec("lake.ingest.throughput.rows_s", 500_000, "per_s", workload={}),
                # Zero rows on duckdb fallback must NOT crash or fabricate 1,000,000
                _rec("analytics.duckdb.load_parquet_1m.s", 1.5, "s", workload={"rows": 0, "threads": 4}),
            ],
            self.pricing,
        )
        by_id = self._by_id(report)
        self.assertAlmostEqual(by_id["usd_per_10k_studio_loads"]["value"], 0.17, places=5)
        self.assertEqual(by_id["usd_per_10k_studio_loads"]["missing_inputs"], [])
        self.assertIsNone(by_id["usd_per_1m_rows_landed"]["value"])
        self.assertTrue(by_id["usd_per_1m_rows_landed"]["missing_inputs"])

        # Zero vCPU or zero hours_per_month in pricing raises ValueError instead of ZeroDivisionError
        bad_vcpu = copy.deepcopy(self.pricing)
        bad_vcpu["compute"]["hetzner_cpx62"]["vcpu"] = 0
        with self.assertRaises(ValueError):
            unit_econ.compute_unit_economics([], bad_vcpu)

        bad_hours = copy.deepcopy(self.pricing)
        bad_hours["hours_per_month"] = 0
        with self.assertRaises(ValueError):
            unit_econ.compute_unit_economics([], bad_hours)

    def test_per_row_pricing_as_of_tracks_only_used_assumptions(self):
        custom = copy.deepcopy(self.pricing)
        custom["compute"]["hetzner_cpx62"]["as_of"] = "2026-09-01"
        custom["storage"]["cloudflare_r2_storage_usd_per_gb_month"]["as_of"] = "2026-08-15"
        custom["ci"]["boat_vm_usd_per_hour"]["as_of"] = "2026-10-10"
        custom["ci"]["effective_parallel_vms"]["as_of"] = "2026-10-10"
        custom["bandwidth"]["cdn_egress_usd_per_gb"]["as_of"] = "2026-11-24"
        custom["workload_assumptions"]["active_tenant_commits_per_month"]["as_of"] = "2026-07-01"
        custom["workload_assumptions"]["hot_cell_retention_months"]["as_of"] = "2026-06-15"

        report = unit_econ.compute_unit_economics(
            [
                _rec("lake.parquet.commit_size.bytes", 128, "bytes"),
                _rec("ci.core.run.s", 3600, "s"),
                _rec("studio.bundle.initial_js.gzip_bytes", 100_000, "bytes"),
                _rec("studio.bundle.initial_css.gzip_bytes", 10_000, "bytes"),
            ],
            custom,
        )
        by_id = self._by_id(report)
        self.assertEqual(by_id["usd_per_1m_commits_cold_lake_month"]["pricing_as_of"], "2026-08-15")
        self.assertEqual(by_id["usd_per_pr_ci_estimate"]["pricing_as_of"], "2026-10-10")
        self.assertEqual(by_id["usd_per_10k_studio_loads"]["pricing_as_of"], "2026-11-24")
        # Re-dated workload assumptions propagate to binding_constraint and usd_per_active_tenant_month_compute
        self.assertIn("2026-07-01", by_id["usd_per_active_tenant_month_compute"]["pricing_as_of"])
        self.assertNotIn("2026-06-15", by_id["usd_per_active_tenant_month_compute"]["pricing_as_of"])
        self.assertIn("2026-07-01", by_id["binding_constraint"]["pricing_as_of"])
        self.assertIn("2026-06-15", by_id["binding_constraint"]["pricing_as_of"])

        csv_rows = {r["id"]: r for r in csv.DictReader(io.StringIO(unit_econ.format_csv(report)))}
        self.assertEqual(csv_rows["usd_per_1m_commits_cold_lake_month"]["pricing_as_of"], "2026-08-15")
        self.assertEqual(csv_rows["usd_per_pr_ci_estimate"]["pricing_as_of"], "2026-10-10")

    def test_zero_cold_storage_rate_reports_undefined_ratio_and_dynamic_ha(self):
        custom = copy.deepcopy(self.pricing)
        custom["storage"]["cloudflare_r2_storage_usd_per_gb_month"]["value"] = 0.0
        custom["utilization"]["ha_replication_factor"]["value"] = 5
        report = unit_econ.compute_unit_economics(
            [
                _rec("cell.commit.size.bytes", 2048.0, "bytes"),
                _rec("lake.parquet.commit_size.bytes", 128.0, "bytes"),
            ],
            custom,
        )
        by_id = self._by_id(report)
        ratio = by_id["hot_to_cold_cost_ratio"]
        self.assertIsNone(ratio["value"])
        self.assertEqual(ratio["missing_inputs"], [])
        self.assertEqual(ratio["notes"], "cold storage rate is $0; ratio undefined")
        self.assertIn("5x HA", by_id["usd_per_100k_cmd_tenant_month_storage"]["notes"])

        md = unit_econ.format_markdown(report)
        ratio_line = next(line for line in md.splitlines() if "`hot_to_cold_cost_ratio`" in line)
        self.assertIn("null (undefined)", ratio_line)
        self.assertNotIn("null (missing inputs)", ratio_line)
        self.assertIn("cold storage rate is $0; ratio undefined", ratio_line)

    def test_cli_reads_jsonl_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            jsonl = Path(tmp) / "m.jsonl"
            out = Path(tmp) / "out.csv"
            jsonl.write_text(json.dumps(_rec("ci.core.run.s", 3600.0, "s")) + "\n", encoding="utf-8")
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

    def test_studio_bundle_probe_measurements_entry_assets_and_schema(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp) / "dist"
            (dist / "assets").mkdir(parents=True)
            (dist / "widgets").mkdir(parents=True)
            entry_js = b"console.log('entry');\n" * 150
            preload_js = b"export const shared = 42;\n" * 60
            lazy_js = b"export const lazy = 99;\n" * 250
            widget_js = b"export const widget = 1;\n" * 40
            entry_css = b"body { color: black; }\n" * 50
            extra_css = b".lazy { color: red; }\n" * 30
            html_doc = (
                b'<!doctype html><html><head>'
                b'<link rel="stylesheet" href="/assets/index-abc.css">'
                b'<link rel="modulepreload" href="/assets/vendor-123.js">'
                b'<script type="module" crossorigin src="/assets/index-abc.js"></script>'
                b'</head><body></body></html>'
            )
            (dist / "assets" / "index-abc.js").write_bytes(entry_js)
            (dist / "assets" / "vendor-123.js").write_bytes(preload_js)
            (dist / "assets" / "lazy-chunk.js").write_bytes(lazy_js)
            (dist / "widgets" / "chat.mjs").write_bytes(widget_js)
            (dist / "assets" / "index-abc.css").write_bytes(entry_css)
            (dist / "assets" / "lazy.css").write_bytes(extra_css)
            (dist / "index.html").write_bytes(html_doc)

            items = probe_studio.measure_dist(dist)
            by_metric = {item["metric"]: item for item in items}
            self.assertEqual(
                set(by_metric),
                {
                    "studio.bundle.js.raw_bytes",
                    "studio.bundle.js.gzip_bytes",
                    "studio.bundle.initial_js.gzip_bytes",
                    "studio.bundle.initial_css.gzip_bytes",
                    "studio.bundle.js.largest_chunk_raw_bytes",
                    "studio.bundle.js.largest_chunk_gzip_bytes",
                    "studio.bundle.css.gzip_bytes",
                    "studio.bundle.total_dist_bytes",
                },
            )
            self.assertEqual(
                by_metric["studio.bundle.initial_js.gzip_bytes"]["value"],
                len(gzip.compress(entry_js, compresslevel=9, mtime=0))
                + len(gzip.compress(preload_js, compresslevel=9, mtime=0)),
            )
            self.assertEqual(
                by_metric["studio.bundle.initial_css.gzip_bytes"]["value"],
                len(gzip.compress(entry_css, compresslevel=9, mtime=0)),
            )
            self.assertGreater(
                by_metric["studio.bundle.js.gzip_bytes"]["value"],
                by_metric["studio.bundle.initial_js.gzip_bytes"]["value"],
            )
            self.assertEqual(by_metric["studio.bundle.js.largest_chunk_raw_bytes"]["value"], len(lazy_js))
            self._validate_metric_lines("studio-bundle-size", items)

            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(probe_studio.main(["--dist", str(dist)]), 0)
            self.assertEqual(len([l for l in out.getvalue().splitlines() if l.startswith("METRIC ")]), 8)

    def test_studio_bundle_probe_fails_without_js_or_index(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp) / "dist"
            dist.mkdir()
            (dist / "index.html").write_bytes(b"<html></html>")
            with self.assertRaises(RuntimeError):
                probe_studio.measure_dist(dist)

    def test_studio_bundle_probe_refuses_symlinked_entry_asset(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = Path(tmp) / "dist"
            (dist / "assets").mkdir(parents=True)
            real_js = dist / "assets" / "real.js"
            real_js.write_bytes(b"console.log('ok');\n")
            link_js = dist / "assets" / "entry.js"
            link_js.symlink_to(real_js)
            (dist / "index.html").write_bytes(b'<script type="module" src="/assets/entry.js"></script>')
            with self.assertRaisesRegex(RuntimeError, "symlink"):
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
        # Verify binary probe always installs wasm32 target idempotently and resolves CARGO_TARGET_DIR
        bin_probe = by_name["binary-and-wasm-sizes"]
        self.assertIn("rustup target add wasm32-unknown-unknown", bin_probe["prepare"])
        self.assertNotIn("rustc --print target-libdir", bin_probe["prepare"])
        self.assertIn("--target-dir", bin_probe["run"])


if __name__ == "__main__":
    unittest.main()
