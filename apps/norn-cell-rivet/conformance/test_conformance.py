#!/usr/bin/env python3
"""Stub tests for the conformance runner's own safety rules, with no host:

    python3 -m unittest apps/norn-cell-rivet/conformance/test_conformance.py
"""
import importlib.util
from pathlib import Path
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

    def __init__(self):
        self.kills = self.starts = 0

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


class StateIsFreshAndReal(unittest.TestCase):
    """Host state and logs go to a fresh directory, never through a link."""

    def test_default_state_is_a_fresh_temp_directory(self):
        state = conformance.fresh_state(None)
        try:
            self.assertEqual(list(state.iterdir()), [])
        finally:
            state.rmdir()

    def test_unsafe_state_directories_are_refused(self):
        with tempfile.TemporaryDirectory() as root:
            target = Path(root) / "target"
            target.mkdir()
            linked = Path(root) / "linked-state"
            linked.symlink_to(target)
            busy = Path(root) / "busy"
            busy.mkdir()
            (busy / "runner-0.log").write_text("")
            for bad in (linked, busy):
                with self.assertRaises(SystemExit, msg=str(bad)):
                    conformance.fresh_state(bad)
            self.assertEqual(list(target.iterdir()), [], "nothing was written through the link")


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
