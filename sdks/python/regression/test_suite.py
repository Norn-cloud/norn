"""The assumption suite against data with known properties (seeded, no cell)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

import numpy as np
import pandas as pd

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn_regression.suite import assumption_suite  # noqa: E402
from synthetic import dataset  # noqa: E402


def by_test(checks, test, subject=None):
    found = [c for c in checks if c.test.startswith(test) and (subject is None or c.subject == subject)]
    assert len(found) == 1, (test, subject, found)
    return found[0]


def clean(n=1000, seed=7):
    rng = np.random.default_rng(seed)
    x1, x2 = rng.normal(0, 1, n), rng.normal(0, 1, n)
    y = 1.5 + 2.0 * x1 - 1.0 * x2 + rng.normal(0, 1, n)
    return pd.DataFrame({"id": [f"r{i:05d}" for i in range(n)], "x1": x1, "x2": x2, "y": y})


class Suite(unittest.TestCase):
    def test_well_behaved_data_passes_the_core_checks(self):
        checks, plots = assumption_suite(clean(), "y", ["x1", "x2"])
        for test in ("Breusch-Pagan", "White", "Ramsey RESET", "Jarque-Bera", "Shapiro-Wilk"):
            self.assertEqual(by_test(checks, test).status, "pass", test)
        for feature in ("x1", "x2"):
            self.assertEqual(by_test(checks, "variance inflation factor", feature).status, "pass")
        self.assertEqual(by_test(checks, "condition number").status, "pass")
        self.assertAlmostEqual(plots["coefficients"]["x1"], 2.0, delta=0.1)

    def test_heteroscedastic_data_fails_breusch_pagan(self):
        frame = dataset(n=1500)
        checks, _ = assumption_suite(frame, "y", ["x1", "x2"])
        bp = by_test(checks, "Breusch-Pagan")
        self.assertEqual(bp.status, "fail", bp)
        self.assertLess(float(bp.p_value), 0.01)
        self.assertIn("robust", bp.remediation)

    def test_collinear_features_show_high_vif(self):
        frame = dataset(n=1500)
        checks, _ = assumption_suite(frame, "y", ["x1", "x2", "x3"])
        for feature in ("x1", "x2", "x3"):
            vif = by_test(checks, "variance inflation factor", feature)
            self.assertEqual(vif.status, "fail", vif)
            self.assertGreater(float(vif.statistic), 10)
        self.assertEqual(by_test(checks, "condition number").status, "fail")

    def test_a_curved_mean_fails_reset(self):
        rng = np.random.default_rng(11)
        x = rng.uniform(-3, 3, 800)
        frame = pd.DataFrame({"id": [f"r{i}" for i in range(800)], "x": x, "y": x**2 + rng.normal(0, 0.5, 800)})
        self.assertEqual(by_test(assumption_suite(frame, "y", ["x"])[0], "Ramsey RESET").status, "fail")

    def test_skewed_errors_fail_normality(self):
        rng = np.random.default_rng(5)
        x = rng.normal(0, 1, 1000)
        frame = pd.DataFrame({"id": range(1000), "x": x, "y": x + rng.exponential(2.0, 1000)})
        checks, _ = assumption_suite(frame, "y", ["x"])
        self.assertEqual(by_test(checks, "Jarque-Bera").status, "fail")
        self.assertEqual(by_test(checks, "Shapiro-Wilk").status, "fail")

    def test_serial_correlation_fails_durbin_watson_only_with_an_order(self):
        frame = dataset(n=1500)
        checks, _ = assumption_suite(frame, "y", ["x1", "x2"], order="t")
        dw = by_test(checks, "Durbin-Watson")
        self.assertEqual(dw.status, "fail", dw)
        self.assertLess(float(dw.statistic), 1.0)
        unordered, _ = assumption_suite(frame, "y", ["x1", "x2"])
        self.assertEqual(by_test(unordered, "Durbin-Watson").status, "not_applicable")

    def test_leakage_constant_features_and_missingness(self):
        frame = clean(600)
        frame["leak"] = frame["y"]
        frame["near"] = frame["y"] * 1.0001 + 0.001
        frame["const"] = 3.0
        frame.loc[frame.index[:200], "x2"] = np.nan  # a third missing
        checks, plots = assumption_suite(frame, "y", ["x1", "x2", "leak", "near", "const"])
        self.assertEqual(by_test(checks, "target leakage", "leak").statistic, "identical to the target")
        self.assertEqual(by_test(checks, "target leakage", "leak").status, "fail")
        self.assertEqual(by_test(checks, "target leakage", "near").status, "fail")
        self.assertEqual(by_test(checks, "target leakage", "x1").status, "pass")
        self.assertEqual(by_test(checks, "constant feature", "const").status, "fail")
        self.assertEqual(by_test(checks, "share of missing values", "x2").status, "fail")
        self.assertEqual(by_test(checks, "share of missing values", "x1").status, "pass")
        self.assertEqual(plots["n"], 400)

    def test_the_same_rows_in_any_order_give_identical_checks(self):
        frame = dataset(n=1500)
        first, _ = assumption_suite(frame, "y", ["x1", "x2", "x3"], order="t")
        again, _ = assumption_suite(frame.sample(frac=1, random_state=3), "y", ["x1", "x2", "x3"], order="t")
        self.assertEqual([c.as_dict() for c in first], [c.as_dict() for c in again])

    def test_every_check_names_its_threshold_source_and_a_hint(self):
        checks, _ = assumption_suite(dataset(n=1500), "y", ["x1", "x2", "x3"], order="t")
        for check in checks:
            self.assertIn(check.status, {"pass", "warn", "fail", "not_applicable"})
            for value in (check.threshold, check.threshold_source, check.remediation, check.statistic):
                self.assertTrue(value, check)

    def test_text_columns_from_an_uploaded_csv_are_read_as_numbers(self):
        frame = dataset(n=1500)
        text = frame.astype({column: str for column in ("x1", "x2", "x3", "y", "t")})
        first, _ = assumption_suite(frame, "y", ["x1", "x2"], order="t")
        again, _ = assumption_suite(text, "y", ["x1", "x2"], order="t")
        self.assertEqual([c.as_dict() for c in first], [c.as_dict() for c in again])

    def test_bad_requests_are_refused(self):
        with self.assertRaisesRegex(ValueError, "both the target and a feature"):
            assumption_suite(clean(), "y", ["y", "x1"])
        with self.assertRaisesRegex(ValueError, "at least one feature"):
            assumption_suite(clean(), "y", [])


if __name__ == "__main__":
    unittest.main()
