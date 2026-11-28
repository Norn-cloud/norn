#!/usr/bin/env python3
"""bundle_durations: the duration table from synthetic bundle logs, no `gh` needed.

- a bundle's checker WASM package is built (and charged) once, by its first WASM-package task:
  a later `phase/browser-1`, `phase/browser-2` or `phase/integration-wasm` reuses it and logs no size report, and a
  later `phase/integration-authoring` keeps its whole time;
- log stamps (`...Z`) are read as UTC whatever the reader's local zone.
"""

import calendar
import os
import re
import time
import unittest


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

bd = _sibling("bundle_durations")

START = calendar.timegm((2026, 11, 20, 10, 0, 0))


def stamp(at):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(at)) + ".1234567Z"


def bundle(*tasks):
    """A bundle log: each task is (name, seconds, {marker line: seconds after its start})."""
    lines, at = [], START
    for name, seconds, markers in tasks:
        lines.append(f"{stamp(at)} ##[group]{name}")
        lines.extend(f"{stamp(at + offset)} {line}" for line, offset in sorted(markers.items(), key=lambda item: item[1]))
        lines.append(f"{stamp(at + seconds)} ##[notice]task={name} result=success seconds={seconds}")
        at += seconds
    return "\n".join(lines) + "\n"


TEST_BUILT = "    Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 40s"
WASM_BUILT = "@norn/checker-wasm 0.1.0 size report (wasm-release, wasm-opt -Oz):"
TESTS = bundle(("test/heavy-1", 130, {TEST_BUILT: 110}), ("test/heavy-2", 40, {TEST_BUILT: 10}))


def table(*bundles):
    original = bd.bundle_logs
    bd.bundle_logs = lambda run: [TESTS, *bundles]
    try:
        return bd.table([1])
    finally:
        bd.bundle_logs = original


class WasmPackageReuse(unittest.TestCase):
    def test_a_second_package_task_reuses_the_build(self):
        result = table(bundle(("phase/browser-1", 350, {WASM_BUILT: 300}), ("phase/integration-wasm", 50, {})))
        self.assertEqual(result["wasm_pkg_seconds"], 300)
        self.assertEqual(result["task_seconds"]["phase/browser-1"], 50)
        self.assertEqual(result["task_seconds"]["phase/integration-wasm"], 50)
        self.assertEqual(result["test_build_seconds"], 100)
        self.assertEqual(result["task_seconds"]["test/heavy-1"], 30)

    def test_legacy_unsharded_browser_first_nets_wasm_and_later_task_reuses_it(self):
        result = table(bundle(("phase/browser", 350, {WASM_BUILT: 300}), ("phase/integration-wasm", 50, {})))
        self.assertEqual(result["wasm_pkg_seconds"], 300)
        self.assertEqual(result["task_seconds"]["phase/browser"], 50)
        self.assertEqual(result["task_seconds"]["phase/integration-wasm"], 50)

    def test_authoring_after_a_built_package_keeps_its_whole_time(self):
        result = table(bundle(("phase/integration-wasm", 380, {WASM_BUILT: 300}),
                              ("phase/integration-authoring", 500, {})))
        self.assertEqual(result["task_seconds"]["phase/integration-wasm"], 80)
        self.assertEqual(result["task_seconds"]["phase/integration-authoring"], 500)

    def test_authoring_first_is_charged_the_median_build_and_the_next_task_reuses_it(self):
        result = table(bundle(("phase/integration-authoring", 600, {}), ("phase/browser-2", 60, {})),
                       bundle(("phase/integration-wasm", 380, {WASM_BUILT: 300})))
        self.assertEqual(result["task_seconds"]["phase/integration-authoring"], 300)
        self.assertEqual(result["task_seconds"]["phase/browser-2"], 60)
        self.assertEqual(result["task_seconds"]["phase/integration-wasm"], 80)

    def test_the_first_package_task_must_log_its_build(self):
        with self.assertRaises(SystemExit):
            table(bundle(("phase/browser-1", 350, {}), ("phase/integration-wasm", 50, {WASM_BUILT: 30})))

    def test_guards_cold_is_netted_by_median_test_build(self):
        result = table(bundle(("phase/integration-wasm", 380, {WASM_BUILT: 300}), ("phase/guards", 340, {})))
        self.assertEqual(result["test_build_seconds"], 100)
        self.assertEqual(result["task_seconds"]["phase/guards"], 240)

    def test_guards_after_test_shard_keeps_its_warm_time(self):
        result = table(
            bundle(("test/rest-1", 210, {TEST_BUILT: 110}), ("phase/guards", 240, {})),
            bundle(("phase/integration-wasm", 380, {WASM_BUILT: 300})),
        )
        self.assertEqual(result["test_build_seconds"], 100)
        self.assertEqual(result["task_seconds"]["phase/guards"], 240)

    def test_guards_after_pruned_test_shard_is_netted_as_cold(self):
        pruned_log = (
            bundle(("test/rest-1", 210, {TEST_BUILT: 110}))
            + f"{stamp(START + 211)} ##[notice]pruned target before phase/guards; 40000000 KiB free\n"
            + bundle(("phase/guards", 340, {}))
        )
        result = table(pruned_log, bundle(("phase/integration-wasm", 380, {WASM_BUILT: 300})))
        self.assertEqual(result["test_build_seconds"], 100)
        self.assertEqual(result["task_seconds"]["phase/guards"], 240)


class UtcStamps(unittest.TestCase):
    def setUp(self):
        self.zone = os.environ.get("TZ")

    def tearDown(self):
        if self.zone is None:
            os.environ.pop("TZ", None)
        else:
            os.environ["TZ"] = self.zone
        time.tzset()

    def parsed(self, zone, log):
        os.environ["TZ"] = zone
        time.tzset()
        return bd.parse(log)

    def test_stamps_are_utc_in_any_local_zone(self):
        log = bundle(("phase/doc", 60, {"documenting": 15}))
        for zone in ("UTC0", "EST5", "XXX-3"):
            [(task, seconds, lines)] = self.parsed(zone, log)
            self.assertEqual((task, seconds), ("phase/doc", 60))
            self.assertAlmostEqual(lines[0][0], START + 0.1234567, places=4, msg=zone)
            self.assertAlmostEqual(bd.elapsed(lines, re.compile("^documenting$")), 15, places=4, msg=zone)

    def test_elapsed_spans_a_daylight_saving_change(self):
        # 00:30Z to 02:30Z on 2026-03-29: two hours, though London's clocks jump an hour at 01:00Z.
        start = calendar.timegm((2026, 3, 29, 0, 30, 0))
        log = (f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(start))} ##[group]phase/doc\n"
               f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(start + 7200))} documented\n"
               f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(start + 7201))} ##[notice]task=phase/doc result=success seconds=7201\n")
        [(_, _, lines)] = self.parsed("GMT0BST,M3.5.0/1,M10.5.0", log)
        self.assertEqual(bd.elapsed(lines, re.compile("^documented$")), 7200)


if __name__ == "__main__":
    unittest.main()
