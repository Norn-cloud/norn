"""Pure-stdlib unit tests for authenticated forecast evaluation receipts."""

from __future__ import annotations

import copy
import sys
import unittest
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn_regression.receipt import (  # noqa: E402
    FORECAST_EVALUATOR_FUNCTION_ID,
    FORECAST_EVALUATOR_MANIFEST,
    FORECAST_EVALUATOR_MANIFEST_DIGEST,
    FORECAST_EVALUATOR_PROFILE_DIGEST,
    MAX_CANONICAL_NUMBER_BYTES,
    build_evaluation_invocation,
    build_evaluation_receipt,
    canonical_json_str,
    dataset_snapshot_blob,
    dependency_token_digest,
    evaluate_forecast_receipt,
    input_identity,
    invocation_digest,
    manifest_digest,
    norn1_json_digest,
    output_manifest_digest,
    parse_exact_decimal,
    receipt_digest,
    result_identity,
    verify_evaluator_artifact_pins,
)


@dataclass(frozen=True)
class _StubCheck:
    status: str


class ForecastReceiptTests(unittest.TestCase):
    def test_canonical_json_rules_exact_precision_and_expansion_limits(self):
        self.assertEqual(
            canonical_json_str({"b": 1, "a": Decimal("0.1200"), "z": Decimal("-0.0")}),
            '{"a":0.12,"b":1,"z":0}',
        )
        # Must not round >28-digit exact Decimals under default decimal context.
        wide = Decimal("12345678901234567890123456789")
        self.assertEqual(canonical_json_str(wide), "12345678901234567890123456789")
        wide_scaled = parse_exact_decimal("12345678901234567890123456789.123456789", max_scale=9)
        self.assertEqual(
            canonical_json_str(wide_scaled),
            "12345678901234567890123456789.123456789",
        )
        # NFC normalization of decomposed characters (`e` + combining acute)
        self.assertEqual(
            canonical_json_str({"caf\u0065\u0301": "caf\u0065\u0301"}),
            '{"caf\u00e9":"caf\u00e9"}',
        )
        with self.assertRaises(TypeError):
            canonical_json_str({"bad": 0.12})
        with self.assertRaises(ValueError):
            canonical_json_str({"caf\u0065\u0301": 1, "caf\u00e9": 2})
        with self.assertRaises(ValueError):
            parse_exact_decimal("0.1234567891", max_scale=9)
        with self.assertRaises(ValueError):
            canonical_json_str(Decimal(f"1e{MAX_CANONICAL_NUMBER_BYTES}"))

    def test_manifest_pins_real_on_disk_artifacts_and_is_hermetic(self):
        verify_evaluator_artifact_pins(Path(__file__).resolve().parent)
        manifest = FORECAST_EVALUATOR_MANIFEST
        self.assertEqual(manifest["functionId"], FORECAST_EVALUATOR_FUNCTION_ID)
        self.assertEqual(manifest["implementation"]["format"], "oci_image")
        self.assertTrue(
            manifest["implementation"]["locator"].endswith(
                f"@{manifest['implementation']['digest']}"
            )
        )
        self.assertEqual(manifest["cache"], "none")
        self.assertEqual(manifest["retention"], "ledger")
        self.assertEqual(manifest["capabilities"], [])
        self.assertEqual(manifest_digest(manifest), FORECAST_EVALUATOR_MANIFEST_DIGEST)
        self.assertRegex(FORECAST_EVALUATOR_MANIFEST_DIGEST, r"^sha256:[0-9a-f]{64}$")
        self.assertRegex(FORECAST_EVALUATOR_PROFILE_DIGEST, r"^sha256:[0-9a-f]{64}$")

        # Tampering with any manifest field changes its content digest.
        tampered = copy.deepcopy(manifest)
        tampered["version"] = 2
        self.assertNotEqual(manifest_digest(tampered), FORECAST_EVALUATOR_MANIFEST_DIGEST)

    def test_receipt_binds_manifest_snapshot_model_revision_scope_and_contract(self):
        rows_a = [
            {"id": "r2", "y": Decimal("12.5"), "x1": Decimal("1.0")},
            {"id": "r1", "y": Decimal("10.0"), "x1": Decimal("0.5")},
        ]
        rows_a_reordered = list(reversed(rows_a))
        rows_b = [
            {"id": "r1", "y": Decimal("10.0"), "x1": Decimal("0.5")},
            {"id": "r2", "y": Decimal("99.9"), "x1": Decimal("1.0")},
        ]
        blob_a = dataset_snapshot_blob(rows_a, ["y", "x1"], dataset_revision="c10")
        blob_a_reordered = dataset_snapshot_blob(rows_a_reordered, ["y", "x1"], dataset_revision="c10")
        blob_b = dataset_snapshot_blob(rows_b, ["y", "x1"], dataset_revision="c10")
        # Row-order independent for identical rows, distinct for altered data under same label.
        self.assertEqual(blob_a, blob_a_reordered)
        self.assertNotEqual(blob_a["digest"], blob_b["digest"])

        invocation = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            dataset_blob=blob_a,
            metric_name="wape",
            target="y",
            features=["x1"],
        )
        other_data_invocation = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            dataset_blob=blob_b,
            metric_name="wape",
            target="y",
            features=["x1"],
        )
        self.assertNotEqual(input_identity(invocation), input_identity(other_data_invocation))
        self.assertNotEqual(invocation_digest(invocation), invocation_digest(other_data_invocation))

        receipt = build_evaluation_receipt(
            invocation,
            evaluation_revision="c10",
            metric_name="wape",
            metric_value="0.12",
            contracts_passed=True,
            checks_passed=11,
            checks_failed=0,
        )
        self.assertEqual(receipt["manifest"], FORECAST_EVALUATOR_MANIFEST_DIGEST)
        self.assertEqual(receipt["executionProfile"], FORECAST_EVALUATOR_PROFILE_DIGEST)
        self.assertEqual(receipt["dispatch"], invocation_digest(invocation))
        self.assertEqual(receipt["inputIdentity"], input_identity(invocation))
        self.assertEqual(receipt["dependencyToken"], dependency_token_digest(invocation["dependency"]))
        summary = receipt["outcome"]["success"]["summary"]
        self.assertEqual(summary["evaluation_revision"], "c10")
        self.assertEqual(summary["metric_value"], Decimal("0.12"))
        self.assertTrue(summary["contracts_passed"])
        self.assertEqual(
            receipt["outcome"]["success"]["outputManifest"],
            output_manifest_digest([], summary),
        )

        # Two attempts with different resource metrics share the same result_identity
        # while producing distinct whole-receipt content digests.
        retry = build_evaluation_receipt(
            invocation,
            evaluation_revision="c10",
            metric_name="wape",
            metric_value=Decimal("0.1200"),
            contracts_passed=True,
            checks_passed=11,
            checks_failed=0,
            attempt=2,
            cpu_millis=42,
        )
        self.assertEqual(result_identity(receipt), result_identity(retry))
        self.assertNotEqual(receipt_digest(receipt), receipt_digest(retry))

        # Changing tenant scope changes dispatch & receipt digests.
        other_scope = build_evaluation_invocation(
            tenant="org_other/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            dataset_blob=blob_a,
            metric_name="wape",
            target="y",
            features=["x1"],
        )
        self.assertNotEqual(invocation_digest(invocation), invocation_digest(other_scope))
        self.assertRegex(norn1_json_digest({"ok": True}), r"^sha256:[0-9a-f]{64}$")

    def test_evaluate_forecast_receipt_default_r_squared_and_snapshot_verification(self):
        rows = [
            {"id": "r1", "y": Decimal("3.0"), "x1": Decimal("1.0")},
            {"id": "r2", "y": Decimal("5.0"), "x1": Decimal("2.0")},
        ]
        tampered_rows = [
            {"id": "r1", "y": Decimal("3.0"), "x1": Decimal("1.0")},
            {"id": "r2", "y": Decimal("9.0"), "x1": Decimal("2.0")},
        ]

        def stub_suite(_frame, _target, _features, order=None):
            return (
                [_StubCheck("pass"), _StubCheck("pass")],
                {"fitted": [], "residuals": [], "qq": {}, "n": 2, "r_squared": 0.94125, "coefficients": {}},
            )

        invocation, receipt, checks, plots = evaluate_forecast_receipt(
            rows,
            "y",
            ["x1"],
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-default",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            suite_runner=stub_suite,
        )
        self.assertEqual(len(checks), 2)
        self.assertEqual(plots["r_squared"], 0.94125)
        summary = receipt["outcome"]["success"]["summary"]
        self.assertEqual(summary["metric_name"], "r_squared")
        self.assertEqual(summary["metric_value"], Decimal("0.94125"))
        self.assertTrue(summary["contracts_passed"])
        self.assertEqual(invocation["blobs"]["dataset"], dataset_snapshot_blob(rows, ["y", "x1"], dataset_revision="c10"))

        # Re-evaluating against a dispatched invocation with different data refuses before emitting.
        with self.assertRaisesRegex(ValueError, "does not match dispatched blob pin"):
            evaluate_forecast_receipt(
                tampered_rows,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-default",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                dispatched_invocation=invocation,
                suite_runner=stub_suite,
            )


if __name__ == "__main__":
    unittest.main()
