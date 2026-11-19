#!/usr/bin/env python3
"""Stub tests for the conformance runner's own safety rules, with no host:

    python3 -m unittest apps/norn-cell-do/conformance/test_conformance.py
"""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("conformance", HERE / "conformance.py")
conformance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(conformance)

OPENED = '{"seq":0}'


class StubHost:
    name = "stub"
    controls = True
    runners = 1

    def __init__(self, project=None, state=None):
        self.project, self.state = project, state
        self.kills = self.starts = 0
        StubHost.last = self

    def urls(self):
        return ["http://stub.invalid"]

    def logs(self):
        return []  # no host log: no marker can ever be read

    def start(self):
        self.starts += 1

    def kill(self):
        self.kills += 1

    stop = kill


def reference(steps=3):
    step = {"method": "POST", "path": "/v1/x", "query": "", "body": "", "authorization": "a"}
    return {
        "nowMs": 0,
        "lineageKey": "k",
        "kill": {
            "script": [step] * steps,
            "native": {"openedJson": OPENED, "steps": [{"headJson": f'{{"seq":{i + 1}}}', "status": 200, "body": "ok"} for i in range(steps)]},
        },
    }


class LocalRunLeavesTheProjectAlone(unittest.TestCase):
    """S1: a local run writes the opt-in config only in a disposable copy."""

    def test_source_wrangler_json_is_byte_identical(self):
        self.run_local(symlinked=False)

    def test_a_symlinked_wrangler_json_is_never_written_through(self):
        self.run_local(symlinked=True)

    def run_local(self, symlinked):
        with tempfile.TemporaryDirectory() as root:
            project = Path(root) / "project"
            project.mkdir()
            original = (HERE / "wrangler.json").read_bytes()
            deployable = Path(root) / "deployable.json"
            deployable.write_bytes(original)
            if symlinked:
                (project / "wrangler.json").symlink_to(deployable)
            else:
                (project / "wrangler.json").write_bytes(original)
            ref = Path(root) / "reference.json"
            ref.write_text(json.dumps({**reference(), "format": conformance.REFERENCE_FORMAT}))
            argv = ["conformance.py", "--reference", str(ref), "--host", "celld", "--project", str(project), "--state", str(Path(root) / "state"), "--only", "none", "--quick"]
            seen = {}

            def host(path, state):
                seen["config"] = json.loads((path / "wrangler.json").read_text())
                seen["copy"] = path
                return StubHost(path, state)

            with mock.patch.object(conformance, "CelldHost", host), mock.patch.object(sys, "argv", argv):
                with self.assertRaises(SystemExit) as exited:
                    conformance.main()
            self.assertEqual(exited.exception.code, 0)
            self.assertEqual((project / "wrangler.json").read_bytes(), original)
            self.assertEqual(deployable.read_bytes(), original, "the file a symlink points at is untouched")
            self.assertNotEqual(seen["copy"].resolve(), project.resolve())
            self.assertEqual(seen["config"]["vars"]["NORN_CONFORMANCE"], "synthetic")
            self.assertNotIn("workers_dev", seen["config"])
            self.assertFalse(seen["copy"].exists(), "the disposable copy is removed afterwards")


class MarkerlessResetNeverKills(unittest.TestCase):
    """S2: a reset connection without a marker is not tested, never a kill or a restart."""

    def sweep(self, wedged):
        host = StubHost()
        suite = conformance.Suite(host, reference(), quick=True)
        cell = {"head": OPENED, "commits": 0, "resets": 0}

        def head(base, obj=None, timeout=120, **_):
            if wedged and cell["resets"]:
                raise ConnectionResetError("reset by peer")
            return cell["head"], {}, 1.0

        def scripted(base, step, now_ms, hold=None, obj=None, marker=None, **_):
            if hold:
                cell["resets"] += 1
                raise ConnectionResetError("reset by peer")
            if obj and obj.startswith("kill-") and not obj.startswith("kill-dry-"):
                cell["commits"] += 1
                cell["head"] = f'{{"seq":{cell["commits"]}}}'
            return 200, b"ok", 1.0, {"x-norn-conformance-statements": "4"}

        with mock.patch.object(conformance, "head", head), mock.patch.object(conformance, "scripted", scripted):
            suite.run("kills", suite.kill_points, True)
        return host, suite.results[-1]

    def test_wedged_cell_ends_the_sweep_untested(self):
        host, row = self.sweep(wedged=True)
        self.assertEqual((host.kills, host.starts), (0, 0))
        self.assertEqual(row["status"], "not tested", row)

    def test_answering_cell_reports_not_tested(self):
        host, row = self.sweep(wedged=False)
        self.assertEqual((host.kills, host.starts), (0, 0))
        self.assertEqual(row["status"], "not tested", row)


class WaitUntilNeedsPendingWork(unittest.TestCase):
    """S4: the row passes only when the reply arrived with background work still pending."""

    def row(self, written):
        suite = conformance.Suite(StubHost(), reference())

        def probed(base, route, *keys, **_):
            if route == "wait-until":
                return {"started": 15, "written": written}
            return {"count": 15, "pairs": 15}

        with mock.patch.object(conformance, "probed", probed), mock.patch.object(conformance.time, "sleep", lambda _: None):
            suite.run("waitUntil", suite.wait_until)
        return suite.results[-1]

    def test_reply_after_all_writes_does_not_pass(self):
        self.assertNotEqual(self.row(15)["status"], "pass")

    def test_reply_while_pending_passes(self):
        self.assertEqual(self.row(0)["status"], "pass")


if __name__ == "__main__":
    unittest.main()
