#!/usr/bin/env python3
"""Regression suite for scripts/check-proof-holes.py (#1864)."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

_SPEC = importlib.util.spec_from_file_location(
    "check_proof_holes", Path(__file__).with_name("check-proof-holes.py")
)
assert _SPEC is not None and _SPEC.loader is not None
holes = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(holes)


def _valid_assumptions() -> dict:
    return {
        "schemaVersion": "norn.verification.trusted-assumptions.v1",
        "assumptions": [
            {
                "id": "TA-1",
                "owner": "verification",
                "boundary": "Lean kernel",
                "justification": "trusted computing base",
                "blastRadius": "all Lean proofs",
                "falsificationTest": "scripts/test_check_proof_holes.py",
            }
        ],
    }


def _valid_coverage(
    *,
    lean_artifacts: list[str] | None = None,
    lean_commands: list[str] | None = None,
    fstar_artifacts: list[str] | None = None,
    proof_scripts: list[str] | None = None,
    composite: dict[str, str] | None = None,
) -> dict:
    lean_artifacts = lean_artifacts if lean_artifacts is not None else ["verification/demo/Ok.lean"]
    lean_commands = (
        lean_commands
        if lean_commands is not None
        else [f"lean -DwarningAsError=true {path}" for path in lean_artifacts]
    )
    fstar_artifacts = fstar_artifacts if fstar_artifacts is not None else []
    proof_scripts = proof_scripts if proof_scripts is not None else ["scripts/verify-demo.sh"]
    return {
        "leanCompositeGates": composite if composite is not None else {},
        "subsystems": [
            {
                "id": "demo",
                "runtime": {"artifacts": [], "commands": []},
                "model": {"artifacts": [], "commands": []},
                "proof": {
                    "artifacts": [*lean_artifacts, *fstar_artifacts],
                    "commands": [*lean_commands, *proof_scripts],
                },
            }
        ],
    }


class ProofHoleLexerTests(unittest.TestCase):
    def test_builtin_lexer_canaries_pass(self) -> None:
        holes.run_lexer_canaries()

    def test_lean_line_numbers_preserved_across_multiline_comments_and_strings(self) -> None:
        source = (
            "/- block comment\n"
            "   /- nested sorry -/\n"
            "   still comment -/\n"
            'def ok := "line 4\n'
            '  sorry on line 5"\n'
            "theorem bad_one : True := by sorry\n"
            "-- line 7 axiom\n"
            "axiom bad_two : False\n"
        )
        self.assertEqual(holes.lean_proof_hole_locations(source), ["6", "8"])

    def test_unterminated_comment_string_or_guillemet_fails_closed(self) -> None:
        for bad in (
            "/- unterminated comment\n",
            "/- outer /- inner -/\n",
            'def s := "unterminated\n',
            'def s := s!"unterminated {1 + 2\n',
            "def g := «unterminated\n",
        ):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                holes.lean_proof_hole_locations(bad)

    def test_nested_fstar_comments_and_admission_options(self) -> None:
        clean = (
            "(* outer (* admit *) outer *)\n"
            '// #set-options "--admit_smt_queries true"\n'
            'let msg = "unsafe_coerce"\n'
            "let ok = 1\n"
        )
        stripped = holes.strip_comments_and_strings(
            clean, line_marker="//", block=("(*", "*)")
        )
        self.assertIsNone(holes.FSTAR_PROOF_HOLE.search(stripped))
        self.assertFalse(holes.fstar_admission_option_present(clean))
        self.assertTrue(
            holes.fstar_admission_option_present('#push-options "--admit_smt_queries true"\n')
        )
        self.assertTrue(holes.extraction_admission_present("fstar.exe --LAX Foo.fst"))


class ProofHoleMainTests(unittest.TestCase):
    def _run_main(
        self,
        root: Path,
        coverage: dict,
        assumptions: dict,
        argv: list[str] | None = None,
    ) -> int:
        assumptions_path = root / "verification/coverage-v1/trusted-assumptions.json"
        assumptions_path.parent.mkdir(parents=True, exist_ok=True)
        assumptions_path.write_text(json.dumps(assumptions), encoding="utf-8")
        fstar_root = root / "verification/stripmine-2026-08-22/proofs/fstar"
        fstar_root.mkdir(parents=True, exist_ok=True)
        with (
            mock.patch.object(holes, "ROOT", root.resolve()),
            mock.patch.object(holes, "ASSUMPTIONS", assumptions_path),
            mock.patch.object(holes, "LEAN_ROOT", root / "verification"),
            mock.patch.object(holes, "FSTAR_ROOT", fstar_root),
            mock.patch.object(holes.coverage_manifest, "load", return_value=coverage),
            mock.patch.object(sys, "argv", ["check-proof-holes.py", *(argv or [])]),
        ):
            return holes.main()

    def _seed_valid_workspace(self, root: Path) -> None:
        lean_file = root / "verification/demo/Ok.lean"
        lean_file.parent.mkdir(parents=True, exist_ok=True)
        lean_file.write_text("theorem ok : True := by exact True.intro\n", encoding="utf-8")
        script = root / "scripts/verify-demo.sh"
        script.parent.mkdir(parents=True, exist_ok=True)
        script.write_text("#!/usr/bin/env bash\nlean -DwarningAsError=true verification/demo/Ok.lean\n", encoding="utf-8")

    def test_valid_workspace_and_composite_gate_pass(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._seed_valid_workspace(root)
            cov = _valid_coverage(
                lean_commands=[],
                composite={"verification/demo/Ok.lean": "scripts/verify-demo.sh"},
            )
            self.assertEqual(self._run_main(root, cov, _valid_assumptions()), 0)

    def test_rejects_undeclared_lean_file_and_lean_hole(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._seed_valid_workspace(root)
            extra = root / "verification/demo/Extra.lean"
            extra.write_text("theorem x : True := True.intro\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "promoted Lean inventory mismatch"):
                self._run_main(root, _valid_coverage(), _valid_assumptions())
            extra.unlink()

            (root / "verification/demo/Ok.lean").write_text(
                "theorem bad : False := by sorry\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, "forbidden Lean proof hole"):
                self._run_main(root, _valid_coverage(), _valid_assumptions())

    def test_rejects_missing_warning_as_error_gate_and_bad_composite_gate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._seed_valid_workspace(root)
            with self.assertRaisesRegex(ValueError, "no exact warning-as-error gate"):
                self._run_main(root, _valid_coverage(lean_commands=[]), _valid_assumptions())

            (root / "scripts/verify-demo.sh").write_text(
                "#!/usr/bin/env bash\nlean verification/demo/Ok.lean\n", encoding="utf-8"
            )
            cov = _valid_coverage(
                lean_commands=[],
                composite={"verification/demo/Ok.lean": "scripts/verify-demo.sh"},
            )
            with self.assertRaisesRegex(ValueError, "warning-as-error"):
                self._run_main(root, cov, _valid_assumptions())

    def test_rejects_bad_assumptions_schema_and_duplicates(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._seed_valid_workspace(root)
            bad_version = {**_valid_assumptions(), "schemaVersion": "wrong"}
            with self.assertRaisesRegex(ValueError, "wrong schemaVersion"):
                self._run_main(root, _valid_coverage(), bad_version)

            dup = _valid_assumptions()
            dup["assumptions"].append(dict(dup["assumptions"][0]))
            with self.assertRaisesRegex(ValueError, "duplicate trusted assumption id"):
                self._run_main(root, _valid_coverage(), dup)

    def test_rejects_fstar_generated_holes_outside_root_and_script_admission(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, tempfile.TemporaryDirectory() as outside:
            root = Path(tmp)
            self._seed_valid_workspace(root)
            gen = root / "generated.fst"
            gen.write_text("let bad = admit ()\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "forbidden F\\* proof hole"):
                self._run_main(
                    root, _valid_coverage(), _valid_assumptions(), ["--fstar-generated", str(gen)]
                )

            external = Path(outside) / "ext.fst"
            external.write_text("let ok = ()\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "existing workspace file"):
                self._run_main(
                    root, _valid_coverage(), _valid_assumptions(), ["--fstar-generated", str(external)]
                )

            (root / "scripts/verify-demo.sh").write_text(
                "#!/usr/bin/env bash\nfstar.exe --lax Foo.fst\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, "proof admission flag present"):
                self._run_main(root, _valid_coverage(), _valid_assumptions())


if __name__ == "__main__":
    unittest.main()
