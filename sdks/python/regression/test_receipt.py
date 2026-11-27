"""Pure-stdlib unit and CLI integration tests for authenticated forecast evaluation receipts."""

from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
import unittest
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
    decode_dataset_snapshot,
    dependency_token_digest,
    encode_dataset_snapshot,
    encode_model_artifact,
    evaluate_forecast_receipt,
    input_identity,
    invocation_digest,
    manifest_digest,
    model_artifact_blob,
    norn1_json_digest,
    output_manifest_digest,
    parse_exact_decimal,
    receipt_digest,
    result_identity,
    verify_evaluator_artifact_pins,
)


def _sample_model_bytes(*, intercept: str = "1", slope: str = "2", max_wape: str = "0.05", min_pred: str = "0") -> bytes:
    return encode_model_artifact(
        model_id="supply.demand_forecast",
        target="y",
        features=["x1"],
        intercept=intercept,
        coefficients={"x1": slope},
        contracts={"maxWape": max_wape, "minPrediction": min_pred},
    )


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
        # Python ints > 4,300 digits must succeed up to Norn's 1,048,576-byte limit.
        big_int = 10**5000 + 7
        big_json = canonical_json_str(big_int)
        self.assertEqual(len(big_json), 5001)
        self.assertTrue(big_json.startswith("1000") and big_json.endswith("0007"))
        self.assertEqual(canonical_json_str(parse_exact_decimal(big_int, max_scale=0)), big_json)
        with self.assertRaises(ValueError):
            canonical_json_str(1 << (MAX_CANONICAL_NUMBER_BYTES * 4))
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

    def test_manifest_pins_all_dockerfile_evaluator_inputs_and_is_hermetic(self):
        verify_evaluator_artifact_pins(Path(__file__).resolve().parent)
        manifest = FORECAST_EVALUATOR_MANIFEST
        self.assertEqual(manifest["functionId"], FORECAST_EVALUATOR_FUNCTION_ID)
        self.assertEqual(manifest["implementation"]["format"], "oci_image")
        self.assertEqual(
            manifest["implementation"]["entryPoint"],
            "python -m norn_regression.evaluator_cli",
        )
        self.assertIsNone(manifest["implementation"]["locator"])
        closure_names = [entry["name"] for entry in manifest["dependencyClosure"]["entries"]]
        self.assertEqual(closure_names, sorted(closure_names))
        self.assertEqual(
            closure_names,
            [
                "base-python-3.11-slim",
                "norn-evaluator-cli",
                "norn-evaluator-dockerfile",
                "norn-evaluator-init",
                "norn-evaluator-receipt",
            ],
        )
        self.assertEqual(manifest["cache"], "none")
        self.assertEqual(manifest["retention"], "ledger")
        self.assertEqual(manifest["capabilities"], [])
        self.assertEqual(manifest_digest(manifest), FORECAST_EVALUATOR_MANIFEST_DIGEST)
        self.assertRegex(FORECAST_EVALUATOR_MANIFEST_DIGEST, r"^sha256:[0-9a-f]{64}$")
        self.assertRegex(FORECAST_EVALUATOR_PROFILE_DIGEST, r"^sha256:[0-9a-f]{64}$")

        tampered = copy.deepcopy(manifest)
        tampered["version"] = 2
        self.assertNotEqual(manifest_digest(tampered), FORECAST_EVALUATOR_MANIFEST_DIGEST)

    def test_canonical_snapshot_format_nfc_decimals_and_type_distinction(self):
        # Distinct runtime types (`1` vs `1.0` vs `"1"`) produce distinct snapshot digests.
        int_rows = [
            {"id": "r1", "y": Decimal("3.0"), "x1": Decimal("1.0"), "seq": 1},
            {"id": "r2", "y": Decimal("5.0"), "x1": Decimal("2.0"), "seq": "1"},
            {"id": "r3", "y": Decimal("7.0"), "x1": Decimal("3.0"), "seq": "z"},
        ]
        float_rows = [
            {"id": "r1", "y": Decimal("3.0"), "x1": Decimal("1.0"), "seq": 1.0},
            {"id": "r2", "y": Decimal("5.0"), "x1": Decimal("2.0"), "seq": "1"},
            {"id": "r3", "y": Decimal("7.0"), "x1": Decimal("3.0"), "seq": "z"},
        ]
        self.assertNotEqual(
            dataset_snapshot_blob(int_rows, ["y", "x1", "seq"], dataset_revision="c10")["digest"],
            dataset_snapshot_blob(float_rows, ["y", "x1", "seq"], dataset_revision="c10")["digest"],
        )

        # Canonically equivalent NFC strings and decimal spellings encode and decode identically.
        nfd_rows = [{"id": "caf\u0065\u0301", "y": Decimal("3.00"), "x1": Decimal("1.000"), "seq": "caf\u0065\u0301"}]
        nfc_rows = [{"id": "caf\u00e9", "y": Decimal("3"), "x1": Decimal("1"), "seq": "caf\u00e9"}]
        snap_nfd = encode_dataset_snapshot(nfd_rows, ["y", "x1", "seq"])
        snap_nfc = encode_dataset_snapshot(nfc_rows, ["y", "x1", "seq"])
        self.assertEqual(snap_nfd, snap_nfc)
        cols, decoded_nfd = decode_dataset_snapshot(snap_nfd)
        _, decoded_nfc = decode_dataset_snapshot(snap_nfc)
        self.assertEqual(cols, ["id", "y", "x1", "seq"])
        self.assertEqual(decoded_nfd, decoded_nfc)
        self.assertEqual(decoded_nfd[0]["seq"], "caf\u00e9")
        self.assertEqual(str(decoded_nfd[0]["y"]), "3")

        # Unambiguous column grammar rejects commas and whitespace.
        with self.assertRaisesRegex(ValueError, "must match"):
            encode_dataset_snapshot([{"a,b": 1, "y": 2}], ["y", "a,b"])

        # Raw snapshot decoder refuses non-canonical whitespace or decomposed UTF-8 bytes.
        non_canonical = b" " + snap_nfc
        with self.assertRaisesRegex(ValueError, "not Norn1 canonical"):
            decode_dataset_snapshot(non_canonical)
        decomposed_raw = snap_nfc.decode("utf-8").replace("caf\u00e9", "caf\u0065\u0301").encode("utf-8")
        with self.assertRaisesRegex(ValueError, "not Norn1 canonical"):
            decode_dataset_snapshot(decomposed_raw)

    def test_receipt_binds_manifest_dataset_model_revision_scope_and_contract(self):
        rows_a = [
            {"id": "r2", "y": Decimal("5.0"), "x1": Decimal("2.0")},
            {"id": "r1", "y": Decimal("3.0"), "x1": Decimal("1.0")},
        ]
        rows_b = [
            {"id": "r1", "y": Decimal("3.0"), "x1": Decimal("1.0")},
            {"id": "r2", "y": Decimal("99.9"), "x1": Decimal("2.0")},
        ]
        model_a = _sample_model_bytes(intercept="1", slope="2")
        model_b = _sample_model_bytes(intercept="1", slope="5")

        blob_a = dataset_snapshot_blob(rows_a, ["y", "x1"], dataset_revision="c10")
        blob_a_rev = dataset_snapshot_blob(list(reversed(rows_a)), ["y", "x1"], dataset_revision="c10")
        blob_b = dataset_snapshot_blob(rows_b, ["y", "x1"], dataset_revision="c10")
        mblob_a = model_artifact_blob(model_a, version_id="v1")
        mblob_b = model_artifact_blob(model_b, version_id="v1")

        self.assertEqual(blob_a, blob_a_rev)
        self.assertNotEqual(blob_a["digest"], blob_b["digest"])
        self.assertNotEqual(mblob_a["digest"], mblob_b["digest"])

        invocation = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            dataset_blob=blob_a,
            model_blob=mblob_a,
            metric_name="wape",
            target="y",
            features=["x1"],
        )
        other_data_inv = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            dataset_blob=blob_b,
            model_blob=mblob_a,
            metric_name="wape",
            target="y",
            features=["x1"],
        )
        other_model_inv = build_evaluation_invocation(
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-v1",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            dataset_blob=blob_a,
            model_blob=mblob_b,
            metric_name="wape",
            target="y",
            features=["x1"],
        )
        self.assertNotEqual(input_identity(invocation), input_identity(other_data_inv))
        self.assertNotEqual(input_identity(invocation), input_identity(other_model_inv))
        self.assertNotEqual(invocation_digest(invocation), invocation_digest(other_model_inv))

        receipt = build_evaluation_receipt(
            invocation,
            evaluation_revision="c10",
            metric_name="wape",
            metric_value="0.12",
            contracts_passed=True,
            checks_passed=2,
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

        retry = build_evaluation_receipt(
            invocation,
            evaluation_revision="c10",
            metric_name="wape",
            metric_value=Decimal("0.1200"),
            contracts_passed=True,
            checks_passed=2,
            checks_failed=0,
            attempt=2,
            cpu_millis=42,
        )
        self.assertEqual(result_identity(receipt), result_identity(retry))
        self.assertNotEqual(receipt_digest(receipt), receipt_digest(retry))
        self.assertRegex(norn1_json_digest({"ok": True}), r"^sha256:[0-9a-f]{64}$")

    def test_evaluate_scores_registered_model_artifact_and_cli_roundtrip(self):
        rows = [
            {"id": "r1", "y": Decimal("3"), "x1": Decimal("1"), "seq": 1},
            {"id": "r2", "y": Decimal("5"), "x1": Decimal("2"), "seq": 2},
            {"id": "r3", "y": Decimal("7"), "x1": Decimal("3"), "seq": 3},
        ]
        snap_bytes = encode_dataset_snapshot(rows, ["y", "x1", "seq"])
        good_model = _sample_model_bytes(intercept="1", slope="2", max_wape="0.05", min_pred="0")
        # Broken model predicts negative/inaccurate outputs on the SAME clean dataset!
        bad_model = _sample_model_bytes(intercept="-10", slope="0", max_wape="0.05", min_pred="0")

        invocation, good_receipt, good_checks, _ = evaluate_forecast_receipt(
            snap_bytes,
            good_model,
            "y",
            ["x1"],
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-good",
            version_id="v1",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="wape",
            order="seq",
        )
        good_summary = good_receipt["outcome"]["success"]["summary"]
        self.assertEqual(good_summary["metric_value"], Decimal(0))
        self.assertEqual(
            good_summary["evaluated_artifact_sha256"],
            invocation["blobs"]["model"]["digest"].removeprefix("sha256:"),
        )
        self.assertEqual(good_summary["evaluated_artifact_size"], len(good_model))
        self.assertTrue(good_summary["contracts_passed"])
        self.assertEqual(len(good_checks), 2)

        # Bad registered model artifact MUST fail contracts_passed even on clean data.
        _, bad_receipt, bad_checks, _ = evaluate_forecast_receipt(
            snap_bytes,
            bad_model,
            "y",
            ["x1"],
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-eval-bad",
            version_id="v2",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="wape",
            order="seq",
        )
        bad_summary = bad_receipt["outcome"]["success"]["summary"]
        self.assertFalse(bad_summary["contracts_passed"])
        self.assertTrue(any(c.status == "fail" for c in bad_checks))

        # Swapping model artifact bytes against a dispatched invocation refuses before scoring.
        with self.assertRaisesRegex(ValueError, "dispatched model blob pin"):
            evaluate_forecast_receipt(
                snap_bytes,
                bad_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=invocation,
            )

        # Dropping `order` or relabeling `metric_name` or tampering `blobs.dataset.version` refuses.
        with self.assertRaisesRegex(ValueError, "does not match dispatched"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order=None,
                dispatched_invocation=invocation,
            )
        with self.assertRaisesRegex(ValueError, "evaluation setting 'metric_name'"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="mae",
                order="seq",
                dispatched_invocation=invocation,
            )
        tampered_ver = copy.deepcopy(invocation)
        tampered_ver["blobs"]["dataset"]["version"] = "c999"
        with self.assertRaisesRegex(ValueError, "blobs.dataset.version"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=tampered_ver,
            )

        # Real CLI entrypoint (`python -m norn_regression.evaluator_cli`) matches API byte-for-byte.
        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            inv_path = tmp_dir / "invocation.json"
            snap_path = tmp_dir / "snapshot.norn1.json"
            model_path = tmp_dir / "model.norn1.json"
            inv_path.write_text(canonical_json_str(invocation), encoding="utf-8")
            snap_path.write_bytes(snap_bytes)
            model_path.write_bytes(good_model)
            proc = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "norn_regression.evaluator_cli",
                    "--invocation",
                    str(inv_path),
                    "--snapshot",
                    str(snap_path),
                    "--model",
                    str(model_path),
                ],
                cwd=str(Path(__file__).resolve().parent),
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(proc.stdout.strip(), canonical_json_str(good_receipt))

        # Exact rational arithmetic avoids ambient 28-digit Decimal catastrophic cancellation:
        # 1e28 + 1 - 1e28 == 1 exactly, so MAE == 0 passes maxMae=0.
        cancel_model = encode_model_artifact(
            model_id="supply.demand_forecast",
            target="y",
            features=["x1", "x2"],
            intercept="1",
            coefficients={"x1": "1", "x2": "-1"},
            contracts={"maxMae": "0"},
        )
        cancel_rows = [{"id": "c1", "y": Decimal("1"), "x1": Decimal("1e28"), "x2": Decimal("1e28")}]
        _, cancel_receipt, _, _ = evaluate_forecast_receipt(
            cancel_rows,
            cancel_model,
            "y",
            ["x1", "x2"],
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-cancel",
            version_id="v-cancel",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="mae",
        )
        cancel_summary = cancel_receipt["outcome"]["success"]["summary"]
        self.assertEqual(cancel_summary["metric_value"], Decimal(0))
        self.assertTrue(cancel_summary["contracts_passed"])

        # Missing explicit `inputs.order`, wrong world, authority kind, or generation refuse.
        missing_order = copy.deepcopy(invocation)
        del missing_order["inputs"]["order"]
        with self.assertRaisesRegex(ValueError, "inputs must declare exactly"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order=None,
                dispatched_invocation=missing_order,
            )
        wrong_world = copy.deepcopy(invocation)
        wrong_world["world"] = "fork"
        with self.assertRaisesRegex(ValueError, "world must be 'main'"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=wrong_world,
            )
        wrong_gen = copy.deepcopy(invocation)
        wrong_gen["run"]["generation"] = 2
        with self.assertRaisesRegex(ValueError, "run does not match"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=wrong_gen,
            )
        wrong_auth_kind = copy.deepcopy(invocation)
        wrong_auth_kind["authority"]["kind"] = "user"
        with self.assertRaisesRegex(ValueError, "authority does not match"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=wrong_auth_kind,
            )

        # Extra nested keys and float generation (1.0) in dispatched invocation must refuse.
        float_gen = copy.deepcopy(invocation)
        float_gen["run"]["generation"] = 1.0
        with self.assertRaisesRegex(ValueError, "run does not match"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=float_gen,
            )
        extra_run = copy.deepcopy(invocation)
        extra_run["run"]["extra"] = "bad"
        with self.assertRaisesRegex(ValueError, "run does not match"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=extra_run,
            )
        extra_auth = copy.deepcopy(invocation)
        extra_auth["authority"]["extra"] = "bad"
        with self.assertRaisesRegex(ValueError, "authority does not match"):
            evaluate_forecast_receipt(
                snap_bytes,
                good_model,
                "y",
                ["x1"],
                tenant="org_demo/dev",
                authority_id="svc:evaluator",
                command_id="cmd-eval-good",
                version_id="v1",
                model_id="supply.demand_forecast",
                evaluation_run="run-eval",
                dataset_revision="c10",
                metric_name="wape",
                order="seq",
                dispatched_invocation=extra_auth,
            )

        # >4,300-digit values in a canonical snapshot score and format contracts without str(int) errors.
        huge_val = Decimal("1" + "0" * 4499 + "7")
        huge_rows = [{"id": "h1", "y": huge_val, "x1": huge_val}]
        huge_model = encode_model_artifact(
            model_id="supply.demand_forecast",
            target="y",
            features=["x1"],
            intercept="0",
            coefficients={"x1": "1"},
            contracts={"minPrediction": "0", "maxMae": "0"},
        )
        _, huge_receipt, huge_checks, _ = evaluate_forecast_receipt(
            huge_rows,
            huge_model,
            "y",
            ["x1"],
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-huge",
            version_id="v-huge",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="mae",
        )
        self.assertTrue(huge_receipt["outcome"]["success"]["summary"]["contracts_passed"])
        self.assertEqual(len(huge_checks), 2)

        # Contract diagnostic formatting must never abort a valid score even when prediction
        # width exceeds Python's 4,300-digit limit or MAX_CANONICAL_NUMBER_BYTES.
        from fractions import Fraction
        from norn_regression.receipt import _format_fraction_brief

        huge_brief = _format_fraction_brief(Fraction(1 << (MAX_CANONICAL_NUMBER_BYTES * 4), 1))
        self.assertIn("<exact-fraction num_bits=", huge_brief)
        neg_huge_brief = _format_fraction_brief(Fraction(-(1 << (MAX_CANONICAL_NUMBER_BYTES * 4)), 1))
        self.assertTrue(neg_huge_brief.startswith("-<exact-fraction num_bits="))

        wide_val = Decimal("1e5000")
        wide_model = encode_model_artifact(
            model_id="supply.demand_forecast",
            target="y",
            features=["x1"],
            intercept="0",
            coefficients={"x1": "10"},
            contracts={"minPrediction": "0", "maxWape": "9"},
        )
        _, wide_receipt, wide_checks, _ = evaluate_forecast_receipt(
            [{"id": "w1", "y": wide_val, "x1": wide_val}],
            wide_model,
            "y",
            ["x1"],
            tenant="org_demo/dev",
            authority_id="svc:evaluator",
            command_id="cmd-wide-pred",
            version_id="v-wide",
            model_id="supply.demand_forecast",
            evaluation_run="run-eval",
            dataset_revision="c10",
            metric_name="wape",
        )
        wide_summary = wide_receipt["outcome"]["success"]["summary"]
        self.assertEqual(wide_summary["metric_value"], Decimal(9))
        self.assertTrue(wide_summary["contracts_passed"])
        self.assertIn("<exact-fraction", wide_checks[0].detail)

    def test_dockerfile_verifier_rejects_recipe_drift(self):
        src_root = Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            pkg = tmp_root / "norn_regression"
            pkg.mkdir()
            for name in ("__init__.py", "receipt.py", "evaluator_cli.py", "evaluator_pins.json"):
                (pkg / name).write_bytes((src_root / "norn_regression" / name).read_bytes())
            orig_df = (src_root / "Dockerfile.evaluator").read_text(encoding="utf-8")

            for drifted in (
                orig_df.replace("535\n", "535 AS stage\n", 1),
                orig_df + "COPY\t/etc/passwd\t/app/leak\n",
                orig_df.replace("RUN useradd --system --uid 10001 evaluator", "RUN useradd --system --uid 0 evaluator", 1),
                orig_df.replace("USER evaluator", "USER root", 1),
                orig_df.replace("WORKDIR /app", "WORKDIR /tmp", 1),
                orig_df.replace("TZ=UTC", "TZ=America/New_York", 1),
            ):
                (tmp_root / "Dockerfile.evaluator").write_text(drifted, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "recipe mismatch"):
                    verify_evaluator_artifact_pins(tmp_root)

            # Restore valid Dockerfile; missing or altered build-context evaluator_pins.json must fail.
            (tmp_root / "Dockerfile.evaluator").write_text(orig_df, encoding="utf-8")
            (pkg / "evaluator_pins.json").write_text("{}\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "missing or differs from trusted"):
                verify_evaluator_artifact_pins(tmp_root)
            (pkg / "evaluator_pins.json").unlink()
            with self.assertRaisesRegex(ValueError, "missing or differs from trusted"):
                verify_evaluator_artifact_pins(tmp_root)


if __name__ == "__main__":
    unittest.main()
