"""Pure-stdlib unit tests for authenticated forecast evaluation receipts."""

from __future__ import annotations

import copy
import sys
import unittest
from decimal import Decimal
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn_regression.receipt import (  # noqa: E402
    FORECAST_EVALUATOR_FUNCTION_ID,
    FORECAST_EVALUATOR_MANIFEST,
    FORECAST_EVALUATOR_MANIFEST_DIGEST,
    FORECAST_EVALUATOR_PROFILE_DIGEST,
    build_evaluation_invocation,
    build_evaluation_receipt,
    canonical_json_str,
    dependency_token_digest,
    input_identity,
    invocation_digest,
    manifest_digest,
    norn1_json_digest,
    output_manifest_digest,
    parse_exact_decimal,
    receipt_digest,
    result_identity,
)


class ForecastReceiptTests(unittest.TestCase):
    def test_canonical_json_rules_and_float_refusal(self):
        self.assertEqual(
            canonical_json_str({"b": 1, "a": Decimal("0.1200"), "z": Decimal("-0.0")}),
            '{"a":0.12,"b":1,"z":0}',
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

    def test_manifest_is_hermetic_and_self_consistent(self):
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

    def test_receipt_binds_manifest_model_revision_scope_and_contract(self):
        invocation = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="wape",
            target="issued_qty",
            features=["lead_time_days", "promotion"],
        )
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

        # Changing tenant scope or dataset revision changes dispatch & receipt digests.
        other_scope = build_evaluation_invocation(
            tenant="org_other/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="wape",
            target="issued_qty",
            features=["lead_time_days", "promotion"],
        )
        self.assertNotEqual(invocation_digest(invocation), invocation_digest(other_scope))

        other_rev = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c11",
            metric_name="wape",
            target="issued_qty",
            features=["lead_time_days", "promotion"],
        )
        self.assertNotEqual(input_identity(invocation), input_identity(other_rev))
        self.assertRegex(norn1_json_digest({"ok": True}), r"^sha256:[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()
