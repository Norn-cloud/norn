"""Tests for the experimental typed Python symbolic-tracing spike (`tools/norn-py`).

Verifies:
1. Byte-identical canonical JSON parity against the checked-in `registry/core/artifacts/core.Agent.interface.json`
   (compiled by `tools/norn-ts`), plus live `bun` parity when `tools/norn-ts/node_modules` is present.
2. Two-layer sandbox determinism (two fresh isolated subprocess runs emit identical bytes).
3. Hard Astra soundness refusals (`__bool__`, `__len__`, `__iter__`, `__contains__`, `__index__`,
   bare `float`, forbidden imports, `eval`/`open`, `try`/`except`, unknown fields).
4. Exact `.py:line` source spans captured from AST.
5. Full HR vertical pilot (`examples/hr_pilot.py`) elaborates cleanly with zero closures.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import unittest
from decimal import Decimal
from pathlib import Path
from typing import Any

from norn_py import (
    Concept,
    DeploymentSpec,
    Expr,
    NornFloatRefusalError,
    NornTracingError,
    PackManifest,
    SecretRef,
    Sym,
    TimestampTag,
    all_of,
    between,
    canonical_json,
    compose_packs,
    elaborate_file,
    fn_ref,
    interval_gap_seconds,
    intervals_overlap,
    preflight_source,
    verify_deterministic_elaboration,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
NORN_TS_ROOT = REPO_ROOT / "tools" / "norn-ts"
HR_PILOT = Path(__file__).resolve().parents[1] / "examples" / "hr_pilot.py"
CORE_AGENT_GOLDEN = REPO_ROOT / "registry" / "core" / "artifacts" / "core.Agent.interface.json"


class TestSoundnessRefusals(unittest.TestCase):
    def _sym(self) -> Sym[Any]:
        return Sym("self", ("hours", "name", "starts_at", "ends_at"), "hr.Shift")

    def test_bool_coercion_refused(self) -> None:
        s = self._sym()
        with self.assertRaises(NornTracingError) as ctx:
            if s.hours > Decimal("0.00"):
                pass
        self.assertEqual(ctx.exception.code, "NORN-PY-BOOL-COERCION")
        self.assertIn("all_of", str(ctx.exception))

    def test_python_and_or_chained_compare_refused(self) -> None:
        s = self._sym()
        with self.assertRaises(NornTracingError) as ctx:
            _ = (s.hours > 0) and (s.hours <= 12)
        self.assertEqual(ctx.exception.code, "NORN-PY-BOOL-COERCION")

        with self.assertRaises(NornTracingError) as ctx2:
            _ = 0 < s.hours <= 12
        self.assertEqual(ctx2.exception.code, "NORN-PY-BOOL-COERCION")

    def test_len_iter_contains_index_refused(self) -> None:
        s = self._sym()
        with self.assertRaises(NornTracingError) as c1:
            len(s.name)
        self.assertEqual(c1.exception.code, "NORN-PY-LEN-COERCION")

        with self.assertRaises(NornTracingError) as c2:
            iter(s.name)
        self.assertEqual(c2.exception.code, "NORN-PY-ITER-COERCION")

        with self.assertRaises(NornTracingError) as c3:
            _ = "a" in s.name
        self.assertEqual(c3.exception.code, "NORN-PY-CONTAINS-COERCION")

        with self.assertRaises(NornTracingError) as c4:
            range(s.hours)
        self.assertEqual(c4.exception.code, "NORN-PY-INDEX-COERCION")

    def test_float_refused_everywhere(self) -> None:
        s = self._sym()
        with self.assertRaises(NornFloatRefusalError):
            _ = s.hours > 8.5  # type: ignore[operator]
        with self.assertRaises(NornFloatRefusalError):
            canonical_json({"amount": 1.5})

    def test_unknown_field_refused(self) -> None:
        s = self._sym()
        with self.assertRaises(NornTracingError) as ctx:
            _ = s.nonexistent_field
        self.assertEqual(ctx.exception.code, "NORN-PY-UNKNOWN-FIELD")

    def test_temporal_and_between_combinators(self) -> None:
        s = self._sym()
        ts_start: Expr[TimestampTag] = Expr("self.starts_at", "ref")
        ts_end: Expr[TimestampTag] = Expr("self.ends_at", "ref")
        expr = all_of(
            between(s.hours, Decimal("1.00"), Decimal("12.00")),
            intervals_overlap(ts_start, ts_end, ts_start, ts_end),
            interval_gap_seconds(ts_start, ts_end, ts_start, ts_end) >= Decimal("39600"),
        )
        self.assertIn("intervals_overlap(timestamp(self.starts_at)", expr.zen)
        self.assertIn("interval_gap_seconds(timestamp(self.starts_at)", expr.zen)


class TestSandboxPreflight(unittest.TestCase):
    def test_forbids_os_import(self) -> None:
        with self.assertRaises(NornTracingError) as ctx:
            preflight_source("import os\n", "bad.py")
        self.assertEqual(ctx.exception.code, "NORN-PY-FORBIDDEN-IMPORT")

    def test_forbids_float_literal_statically(self) -> None:
        with self.assertRaises(NornTracingError) as ctx:
            preflight_source("x = 1.25\n", "bad.py")
        self.assertEqual(ctx.exception.code, "NORN-PY-FLOAT-LITERAL")

    def test_forbids_eval_open_try_async(self) -> None:
        for snippet, code in [
            ("eval('1+1')\n", "NORN-PY-FORBIDDEN-CALL"),
            ("open('/etc/passwd')\n", "NORN-PY-FORBIDDEN-CALL"),
            ("try:\n  x = 1\nexcept Exception:\n  pass\n", "NORN-PY-FORBIDDEN-SYNTAX"),
            ("async def f():\n  pass\n", "NORN-PY-FORBIDDEN-SYNTAX"),
        ]:
            with self.assertRaises(NornTracingError) as ctx:
                preflight_source(snippet, "bad.py")
            self.assertEqual(ctx.exception.code, code)


class TestHrPilotAndDeterminism(unittest.TestCase):
    def test_hr_pilot_deterministic_and_source_mapped(self) -> None:
        result = verify_deterministic_elaboration(
            HR_PILOT,
            scope={"environmentId": "dev", "orgId": "org_demo"},
            source_path="tools/norn-py/examples/hr_pilot.py",
        )
        ids = [a["artifactId"] for a in result.artifacts if isinstance(a, dict)]
        self.assertEqual(
            ids,
            [
                "hr.privacy.markings",
                "core.TimeInterval.interface",
                "hr.Employee.entity",
                "hr.LeaveRequest.entity",
                "hr.Shift.entity",
                "hr.shift.valid_window_and_hours.law",
            ],
        )
        self.assertIn("Employee", result.source_map)
        emp_line = result.source_map["Employee"]["line"]
        assert isinstance(emp_line, int)
        self.assertGreater(emp_line, 1)

        # Confirm PII markings lowered cleanly onto Employee properties.
        emp = result.artifacts[2]
        assert isinstance(emp, dict)
        payload = emp["payload"]
        assert isinstance(payload, dict)
        obj = payload["objectType"]
        assert isinstance(obj, dict)
        prop_list = obj["properties"]
        assert isinstance(prop_list, list)
        props: dict[str, dict[str, Any]] = {
            str(p["id"]): p for p in prop_list if isinstance(p, dict)
        }
        self.assertEqual(props["gender"]["markings"], ["pii.demographic"])
        self.assertEqual(props["national_id"]["markings"], ["pii.gov_id"])
        self.assertEqual(props["base_salary"]["markings"], ["pii.compensation"])


class TestByteIdenticalParityWithNornTs(unittest.TestCase):
    def test_byte_identical_parity_with_checked_in_core_agent_golden(self) -> None:
        """Exact byte-for-byte parity against registry/core/artifacts/core.Agent.interface.json."""
        expected_bytes = CORE_AGENT_GOLDEN.read_text(encoding="utf-8")

        with tempfile.TemporaryDirectory() as tmp:
            py_file = Path(tmp) / "core_agent.py"
            py_file.write_text(
                '''from __future__ import annotations

from enum import StrEnum
from typing import Annotated

from norn_py import Concept, ref_type


class AgentKind(StrEnum):
    PERSON = "person"
    ORGANIZATION = "organization"
    SYSTEM = "system"


class Agent(
    Concept,
    name="core.Agent",
    title="Agent",
    version="0.1.0",
    interface=True,
    role="thing",
    description=(
        "An actor that holds resources, takes part in economic events and "
        "makes commitments: a person, an organisation or a system (REA agent)."
    ),
):
    display_name: Annotated[str, ref_type("core.display_name")]
    kind: AgentKind
''',
                encoding="utf-8",
            )
            result = verify_deterministic_elaboration(
                py_file,
                scope={},
                source_path="registry/core/src/physics.ts",
                registry_package_path="registry/core",
            )
            self.assertEqual(len(result.artifacts), 1)
            self.assertEqual(
                canonical_json(result.artifacts[0]),
                expected_bytes,
                "Python elaboration MUST match registry/core/artifacts/core.Agent.interface.json byte-for-byte",
            )

    def test_live_parity_with_norn_ts_cli_when_installed(self) -> None:
        bun = shutil.which("bun")
        ts_installed = (NORN_TS_ROOT / "node_modules" / "typescript").exists()
        if bun is None or not ts_installed:
            self.skipTest("tools/norn-ts/node_modules not installed in this checkout")

        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            ts_file = tmp_dir / "parity.ts"
            ts_out = tmp_dir / "parity.ts.json"
            py_file = tmp_dir / "parity.py"

            ts_file.write_text(
                """import { Norn } from "@norn/authoring"

export const Privacy = Norn.markings({
  id: "pilot.privacy.markings",
  title: "Privacy catalog",
  version: "0.1.0",
  markings: [
    { id: "pii.contact", label: "Contact", severity: "moderate", category: "pii", mask: { kind: "redacted" } },
    { id: "pii.account", label: "Account", severity: "high", category: "pii", mask: { kind: "last-n", count: 4 } },
    { id: "pii.secret", label: "Secret", severity: "critical", category: "pii", mask: { kind: "null" } },
  ],
})
""",
                encoding="utf-8",
            )

            py_file.write_text(
                """from __future__ import annotations

from norn_py import MarkingDecl, MarkingsCatalog, mask_last_n, mask_null, mask_redacted

Privacy = MarkingsCatalog(
    id="pilot.privacy.markings",
    title="Privacy catalog",
    version="0.1.0",
    markings=(
        MarkingDecl(id="pii.contact", label="Contact", severity="moderate", category="pii", mask=mask_redacted()),
        MarkingDecl(id="pii.account", label="Account", severity="high", category="pii", mask=mask_last_n(4)),
        MarkingDecl(id="pii.secret", label="Secret", severity="critical", category="pii", mask=mask_null()),
    ),
)
""",
                encoding="utf-8",
            )

            proc = subprocess.run(
                [bun, "src/cli.ts", "compile", str(ts_file), "--org", "org_demo", "--env", "dev", "--out", str(ts_out)],
                cwd=NORN_TS_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr or proc.stdout)
            ts_artifacts = json.loads(ts_out.read_text(encoding="utf-8"))
            ts_source_path = ts_artifacts[0]["provenance"]["sourcePath"]

            py_result = elaborate_file(
                py_file,
                scope={"environmentId": "dev", "orgId": "org_demo"},
                source_path=ts_source_path,
            )
            self.assertEqual(py_result.canonical_text, canonical_json(ts_artifacts))


class TestPackAndDeploymentComposition(unittest.TestCase):
    def test_pack_composition_and_deployment_spec(self) -> None:
        class _Person(Concept, name="hr.TestPerson", title="Test person", version="0.1.0"):
            id: str

        class _EgyptProfile(Concept, name="loc_eg.TestProfile", title="Egypt profile", version="0.1.0"):
            id: str

        hr_pack = PackManifest(
            id="norn.pack.hr",
            version="0.1.0",
            category="domain",
            title="HR Pack",
            entities=(_Person,),
            functions=(
                fn_ref(
                    id="norn.solver.rostering",
                    version="0.1.0",
                    input_type="SolveRequest",
                    output_type="SolveResponse",
                ),
            ),
        )
        loc_eg = PackManifest(
            id="norn.pack.loc-eg",
            version="0.1.0",
            category="localization",
            title="Egypt Localization",
            requires=("norn.pack.hr",),
            entities=(_EgyptProfile,),
        )
        composed = compose_packs(hr_pack, loc_eg)
        self.assertEqual(composed["entities"], ["hr.TestPerson", "loc_eg.TestProfile"])
        self.assertEqual(composed["functions"], ["norn.solver.rostering@0.1.0"])

        dep = DeploymentSpec(
            id="dep_demo_cairo",
            org_id="org_demo",
            environment_id="prod",
            packs=(hr_pack, loc_eg),
            worker_endpoints={"norn.solver.rostering@0.1.0": "https://solver.example.internal/solve"},
            secret_refs=(SecretRef(name="WHATSAPP_VERIFY_TOKEN"),),
            initial_settings={"weekly_overtime_budget_egp": Decimal("1500.00"), "avoid_split_shifts": True},
        )
        wire = dep.to_ir()
        self.assertEqual(wire["initialSettings"], {"avoid_split_shifts": True, "weekly_overtime_budget_egp": "1500.00"})
        self.assertTrue(canonical_json(wire))

    def test_pack_missing_dep_and_raw_secret_refused(self) -> None:
        orphan = PackManifest(
            id="norn.pack.loc-eg",
            version="0.1.0",
            category="localization",
            title="Orphan Egypt Pack",
            requires=("norn.pack.hr",),
        )
        with self.assertRaises(NornTracingError) as ctx:
            compose_packs(orphan)
        self.assertEqual(ctx.exception.code, "NORN-PY-PACK-MISSING-DEP")

        base = PackManifest(id="norn.pack.hr", version="0.1.0", category="domain", title="HR")
        with self.assertRaises(NornTracingError) as sec_ctx:
            DeploymentSpec(
                id="dep_bad",
                org_id="org_demo",
                environment_id="dev",
                packs=(base,),
                initial_settings={"whatsapp_api_token": "raw-secret-never-allowed"},
            )
        self.assertEqual(sec_ctx.exception.code, "NORN-PY-RAW-SECRET")

        with self.assertRaises(NornFloatRefusalError):
            DeploymentSpec(
                id="dep_float",
                org_id="org_demo",
                environment_id="dev",
                packs=(base,),
                initial_settings={"budget": 12.5},  # type: ignore[dict-item]
            )


if __name__ == "__main__":
    unittest.main()
