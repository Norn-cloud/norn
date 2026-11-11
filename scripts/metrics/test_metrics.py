#!/usr/bin/env python3
"""Tests of the metrics record tools: schema, append-only store, push races, ratchet, views."""

from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import collect_ci  # noqa: E402
import ratchet  # noqa: E402
import record  # noqa: E402
import report  # noqa: E402

SHA = "6cbc5766c342dd26744106920bc3a38a3cacfe58"
GIT_ENV = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid", "GIT_COMMITTER_NAME": "t",
           "GIT_COMMITTER_EMAIL": "t@example.invalid", "GIT_CONFIG_GLOBAL": os.devnull,
           "GIT_CONFIG_SYSTEM": os.devnull, "METRICS_PUSH_TOKEN": ""}


def sample(**changes) -> dict:
    base = {
        "schema": 1, "id": "sim.cell.serial_s/test-1", "metric": "sim.cell.serial_s", "value": 34.7,
        "unit": "s", "statistic": "total",
        "commit": {"sha": SHA, "date": "2026-11-11T12:23:59Z", "ref": "PR #1223 head"},
        "measured_at": "2026-11-11T13:00:00Z", "recorded_at": "2026-11-11T15:00:00Z",
        "measured_by": {"name": "L-dst"}, "env": {"runner": "boat", "vm_type": "default", "vcpu": 4},
        "workload": {"threads": 1, "tests": 11}, "caveats": "serial, one run", "estimate": False,
        "source": {"url": "https://github.com/Norn-cloud/norn/pull/1223"},
    }
    base.update(changes)
    return base


INVALID = {
    "missing caveats": lambda r: r.pop("caveats"),
    "empty caveats": lambda r: r.update(caveats=""),
    "bad metric id": lambda r: r.update(metric="Sim Cell"),
    "single-segment metric": lambda r: r.update(metric="sim"),
    "unknown unit": lambda r: r.update(unit="minutes"),
    "unknown statistic": lambda r: r.update(statistic="p42"),
    "extra field": lambda r: r.update(note="x"),
    "string value": lambda r: r.update(value="34.7"),
    "boolean value": lambda r: r.update(value=True),
    "short sha": lambda r: r["commit"].update(sha="6cbc576"),
    "unknown runner": lambda r: r["env"].update(runner="laptop"),
    "zero vcpu": lambda r: r["env"].update(vcpu=0),
    "nested workload": lambda r: r.update(workload={"a": {"b": 1}}),
    "wrong schema version": lambda r: r.update(schema=2),
    "missing source url": lambda r: r.update(source={}),
}


class Schema(unittest.TestCase):
    def setUp(self):
        self.schema = record.load_schema()

    def test_sample_is_valid(self):
        self.assertEqual(record.record_errors(sample(), self.schema), [])

    def test_each_invalid_case_is_refused(self):
        for name, mutate in INVALID.items():
            candidate = copy.deepcopy(sample())
            mutate(candidate)
            with self.subTest(name):
                self.assertTrue(record.schema_errors(candidate, self.schema), name)

    def test_semantic_rules(self):
        self.assertTrue(record.record_errors(sample(range={"min": 1, "max": 2}), self.schema))
        self.assertTrue(record.record_errors(sample(commit={"sha": None, "date": "2026-11-11T00:00:00Z"}), self.schema))
        self.assertTrue(record.record_errors(sample(measured_at="2026-12-01T00:00:00Z"), self.schema))
        self.assertTrue(record.record_errors(sample(measured_at="2026-11-11"), self.schema))
        self.assertEqual(record.record_errors(sample(commit={"sha": None, "date": None}), self.schema), [])

    def test_unknown_keyword_is_refused(self):
        with self.assertRaises(record.Invalid):
            record.schema_errors({}, {"type": "object", "oneOf": []})

    def test_agrees_with_jsonschema(self):
        try:
            import jsonschema
        except ImportError:
            self.skipTest("jsonschema not installed (CI installs it)")
        validator = jsonschema.Draft202012Validator(self.schema)
        jsonschema.Draft202012Validator.check_schema(self.schema)
        cases = [("valid", sample())]
        for name, mutate in INVALID.items():
            candidate = copy.deepcopy(sample())
            mutate(candidate)
            cases.append((name, candidate))
        for name, candidate in cases:
            with self.subTest(name):
                self.assertEqual(validator.is_valid(candidate), not record.schema_errors(candidate, self.schema))


def git(cwd, *args):
    return subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True,
                          env={**os.environ, **GIT_ENV}).stdout.strip()


class Store(unittest.TestCase):
    def setUp(self):
        self.env = {k: os.environ.get(k) for k in GIT_ENV}
        os.environ.update(GIT_ENV)
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.remote = root / "remote.git"
        subprocess.run(["git", "init", "--quiet", "--bare", str(self.remote)], check=True)
        self.a, self.b = root / "a", root / "b"
        for clone in (self.a, self.b):
            subprocess.run(["git", "clone", "--quiet", str(self.remote), str(clone)], check=True, capture_output=True)

    def tearDown(self):
        for key, value in self.env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        self.tmp.cleanup()

    def test_append_creates_branch_dedupes_and_verifies(self):
        commit, fresh = record.append(self.a, [sample()], views=report.render_files)
        self.assertEqual(len(fresh), 1)
        self.assertEqual(git(self.remote, "rev-parse", "refs/heads/metrics"), commit)
        files = git(self.remote, "ls-tree", "-r", "--name-only", "metrics").splitlines()
        self.assertEqual(sorted(files), ["README.md", "index.html", "metrics/2026-11.jsonl"])
        again, fresh = record.append(self.b, [sample()])
        self.assertIsNone(again)
        self.assertEqual(fresh, [])
        record.append(self.b, [sample(id="sim.cell.serial_s/test-2", value=33.4,
                                      recorded_at="2026-12-01T00:00:00Z")])
        self.assertEqual(record.verify_store(self.a, record.fetch_tip(self.a, "origin", "metrics")), 0)
        shard = git(self.remote, "show", "metrics:metrics/2026-11.jsonl")
        self.assertEqual(json.loads(shard)["value"], 34.7)

    def test_a_lost_race_is_rebuilt_on_the_new_tip_without_force(self):
        record.append(self.a, [sample()])
        stale = git(self.a, "rev-parse", "refs/remotes/origin/metrics")
        record.append(self.b, [sample(id="sim.cell.serial_s/test-2", value=33.4)])
        real = record.fetch_tip
        calls = []

        def first_stale(repo, remote, branch):
            calls.append(1)
            tip = real(repo, remote, branch)
            if len(calls) == 1:
                git(repo, "update-ref", f"refs/remotes/{remote}/{branch}", stale)
                return stale
            return tip

        record.fetch_tip = first_stale
        try:
            commit, fresh = record.append(self.a, [sample(id="sim.cell.serial_s/test-3", value=35.0)])
        finally:
            record.fetch_tip = real
        self.assertEqual(len(calls), 2)
        lines = git(self.remote, "show", "metrics:metrics/2026-11.jsonl").splitlines()
        self.assertEqual([json.loads(line)["value"] for line in lines], [34.7, 33.4, 35.0])
        self.assertEqual(git(self.remote, "rev-list", "--count", "metrics"), "3")

    def test_a_series_keeps_its_unit_and_statistic(self):
        record.append(self.a, [sample()])
        with self.assertRaises(record.Invalid):
            record.append(self.a, [sample(id="sim.cell.serial_s/test-2", unit="ms")])

    def test_invalid_record_is_refused_before_any_commit(self):
        with self.assertRaises(record.Invalid):
            record.append(self.a, [sample(caveats="")])
        self.assertIsNone(record.fetch_tip(self.a, "origin", "metrics"))

    def test_verify_catches_a_rewritten_line(self):
        record.append(self.a, [sample()])
        record.append(self.a, [sample(id="sim.cell.serial_s/test-2", value=33.4)])
        work = Path(self.tmp.name) / "w"
        subprocess.run(["git", "clone", "--quiet", "-b", "metrics", str(self.remote), str(work)], check=True,
                       capture_output=True)
        shard = work / "metrics/2026-11.jsonl"
        shard.write_text(shard.read_text().replace("34.7", "30.0"))
        git(work, "commit", "--quiet", "-am", "rewrite")
        with self.assertRaisesRegex(record.Invalid, "append-only"):
            record.verify_store(work, "HEAD")

    def test_verify_catches_a_misplaced_or_duplicate_record(self):
        record.append(self.a, [sample()])
        work = Path(self.tmp.name) / "w"
        subprocess.run(["git", "clone", "--quiet", "-b", "metrics", str(self.remote), str(work)], check=True,
                       capture_output=True)
        shard = work / "metrics/2026-11.jsonl"
        shard.write_text(shard.read_text() * 2)
        git(work, "commit", "--quiet", "-am", "duplicate")
        with self.assertRaisesRegex(record.Invalid, "duplicate id"):
            record.verify_store(work, "HEAD")

    def test_cli_add_builds_a_record_from_flags(self):
        git(self.a, "commit", "--quiet", "--allow-empty", "-m", "base")
        code = record.main(["add", "--repo", str(self.a), "--metric", "tests.rust.count", "--value", "3279",
                            "--unit", "count", "--statistic", "single", "--commit", "HEAD", "--by", "L-metrics",
                            "--runner", "local", "--workload", "method=grep", "--caveats", "static count",
                            "--source", "https://github.com/Norn-cloud/norn/pull/1", "--no-report"])
        self.assertEqual(code, 0)
        stored = json.loads(git(self.remote, "show", "metrics:metrics/" + record.now_utc()[:7] + ".jsonl"))
        self.assertEqual(stored["value"], 3279)
        self.assertEqual(stored["commit"]["sha"], git(self.a, "rev-parse", "HEAD"))
        self.assertEqual(stored["workload"], {"method": "grep"})
        self.assertTrue(stored["id"].startswith("tests.rust.count/"))


class Ratchet(unittest.TestCase):
    def test_judge(self):
        self.assertEqual(ratchet.judge([10, 10], "lower", 10)["status"], "insufficient")
        self.assertEqual(ratchet.judge([10, 10, 10, 10.5], "lower", 10)["status"], "within")
        self.assertEqual(ratchet.judge([10, 10, 10, 12], "lower", 10)["status"], "regressed")
        self.assertEqual(ratchet.judge([10, 10, 10, 8], "lower", 10)["status"], "improved")
        self.assertEqual(ratchet.judge([10, 10, 10, 8], "higher", 10)["status"], "regressed")
        # The baseline is the best window median ever seen, so a slow drift still trips it.
        self.assertEqual(ratchet.judge([10, 10, 10, 10.6, 11.2, 11.8], "lower", 10)["status"], "regressed")
        # One lucky outlier does not set the bar.
        self.assertEqual(ratchet.judge([10, 2, 10, 10, 10.5], "lower", 10)["status"], "within")

    def test_only_boat_non_estimates_count(self):
        rows = [sample(id=f"s/{i}", value=v, measured_at=f"2026-11-0{i}T00:00:00Z",
                       recorded_at="2026-11-11T00:00:00Z") for i, v in enumerate([10, 10, 10, 20], 1)]
        rows.append(sample(id="s/9", value=99, measured_at="2026-11-09T00:00:00Z",
                           env={"runner": "github-hosted"}))
        rows.append(sample(id="s/8", value=1, measured_at="2026-11-08T00:00:00Z", estimate=True))
        config = {"metrics": {"sim.cell.serial_s": {"better": "lower", "noise_pct": 10}}}
        [verdict] = ratchet.evaluate(rows, config)
        self.assertEqual((verdict["status"], verdict["current"]), ("regressed", 20))

    def test_shipped_config_is_well_formed(self):
        config = json.loads((HERE / "ratchet.json").read_text())
        self.assertEqual(config["runner"], "boat")
        for metric, rule in config["metrics"].items():
            self.assertRegex(metric, record.load_schema()["properties"]["metric"]["pattern"])
            self.assertIn(rule["better"], ("lower", "higher"))
            self.assertGreater(rule["noise_pct"], 0)
            self.assertTrue(rule["why"])


class Views(unittest.TestCase):
    def test_render(self):
        rows = [sample(), sample(id="x/2", value=33.4, measured_at="2026-11-12T00:00:00Z",
                                 recorded_at="2026-11-12T00:00:00Z")]
        files = report.render_files(rows)
        readme = files["README.md"].decode()
        self.assertIn("`sim.cell.serial_s` | 33.4 |", readme)
        self.assertIn("<svg", files["index.html"].decode())
        self.assertEqual(report.sparkline([1, 2, 3]), "▁▅█")


class CollectCi(unittest.TestCase):
    def test_run_records(self):
        run = {"id": 7, "run_attempt": 1, "head_sha": SHA, "head_commit": {"timestamp": "2026-11-11T12:23:59Z"},
               "html_url": "https://github.com/Norn-cloud/norn/actions/runs/7", "event": "push",
               "conclusion": "success"}
        jobs = [{"id": 1, "name": "bundle (b0)", "conclusion": "success", "started_at": "2026-11-11T13:00:00Z",
                 "completed_at": "2026-11-11T13:10:00Z", "html_url": "https://github.com/x/job/1"},
                {"id": 2, "name": "plan", "conclusion": "success", "started_at": "2026-11-11T12:59:00Z",
                 "completed_at": "2026-11-11T12:59:30Z", "html_url": "https://github.com/x/job/2"}]
        notices = {1: ["task=test/rest-1 result=success seconds=1166", "task=phase/clippy result=failure seconds=9",
                       "99% - 194 hits, 1 miss, 0 errors"]}
        rows = [record.complete(r, "2026-11-11T15:00:00Z") for r in collect_ci.run_records(run, jobs, notices, HERE)]
        self.assertEqual([r["metric"] for r in rows], ["ci.core.task.test.rest-1.s", "ci.core.run.s"])
        self.assertEqual([r["value"] for r in rows], [1166, 660])
        self.assertEqual(rows[0]["id"], "ci.core/7/1/test/rest-1")
        schema = record.load_schema()
        for row in rows:
            self.assertEqual(record.record_errors(row, schema), [])


class Probes(unittest.TestCase):
    def test_probe_output_becomes_valid_records(self):
        import boat_nightly
        import run_probes
        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "present").write_text("")
            probes = [
                {"name": "absent", "requires": ["missing"], "run": "true", "caveats": "c"},
                {"name": "gates", "requires": ["present"], "run": "echo '[pass] Kani.Ledger/A (12s)'; echo "
                 "'METRIC {\"metric\": \"fork.cow.create.f20.p50\", \"value\": 0.4, \"unit\": \"ms\", "
                 "\"statistic\": \"p50\", \"workload\": {\"forks\": 20}}'",
                 "timed": {"metric": "proof.gate.all.s", "unit": "s", "statistic": "single"},
                 "parse": {"regex": r"^\[pass\] (?P<name>\S+) \((?P<value>\d+)s\)$", "metric": "proof.gate.{name}.s",
                           "unit": "s", "statistic": "single"},
                 "workload": {"bench": "b"}, "caveats": "one run"},
                {"name": "broken", "requires": [], "run": "exit 3", "caveats": "c"},
            ]
            out = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                results = [run_probes.run_probe(probe, Path(root)) for probe in probes]
        self.assertEqual(results, [True, True, False])
        lines = out.getvalue()
        self.assertIn("SKIP absent missing missing", lines)
        self.assertIn("FAIL broken run exited 3", lines)
        lines = 'ENV {"vcpu": 4, "cpu_model": "x"}\n' + lines

        class Args:
            snapshot, type = "snap", "default"
        rows, notes = boat_nightly.records_from(lines, SHA, "2026-11-11T12:23:59Z", "bx_abcdefgh", Args)
        rows.append(boat_nightly.starts_record({"starts": {"day": {"used": 410, "limit": 840}}}, "2026-11-11T15:28:26Z"))
        self.assertEqual(len(notes), 2)
        self.assertEqual(sorted(r["metric"] for r in rows), ["boat.starts.used_24h", "fork.cow.create.f20.p50",
                                                            "proof.gate.all.s", "proof.gate.kani_ledger_a.s"])
        schema = record.load_schema()
        for row in rows:
            row = record.complete(row, "2026-11-11T16:00:00Z")
            self.assertEqual(record.record_errors(row, schema), [], row["metric"])
        fork = next(r for r in rows if r["metric"].startswith("fork."))
        self.assertEqual(fork["workload"], {"bench": "b", "forks": 20, "boat_snapshot": "snap", "boat_sandbox": "bx_abcdefgh"})
        self.assertEqual((fork["env"]["runner"], fork["env"]["vcpu"]), ("boat", 4))


if __name__ == "__main__":
    unittest.main()
