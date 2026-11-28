#!/usr/bin/env python3
"""Unit tests for the stop-gap auto-extracted feature map (scripts/feature_map.py)."""

from __future__ import annotations

import io
import json
import shutil
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path


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


fm = _sibling("feature_map")
ROOT = Path(__file__).resolve().parent.parent


def _make_synthetic_repo(tmp: Path) -> None:
    """Create a minimal synthetic repo with one surface in every auto-extracted family."""
    (tmp / "registry/features").mkdir(parents=True)
    shutil.copy(ROOT / fm.SCHEMA_PATH, tmp / fm.SCHEMA_PATH)

    (tmp / "sdks/openapi").mkdir(parents=True)
    (tmp / "sdks/openapi/norn-api.openapi.json").write_text(
        json.dumps(
            {
                "openapi": "3.1.0",
                "paths": {
                    "/v1/items": {
                        "get": {"operationId": "list_items"},
                        "post": {"operationId": "create_item"},
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    (tmp / "crates/norn-cli/src").mkdir(parents=True)
    (tmp / "crates/norn-cli/src/main.rs").write_text(
        """
#[derive(Debug, Subcommand)]
enum Command {
    /// Check definitions.
    Check,
    /// Manage cells.
    Cell(cell::CellArgs),
}
""",
        encoding="utf-8",
    )
    (tmp / "crates/norn-cli/src/cell.rs").write_text(
        """
#[derive(Debug, Subcommand)]
enum CellCommand {
    Plan,
    Apply,
}
""",
        encoding="utf-8",
    )

    (tmp / "crates/norn-ir/src").mkdir(parents=True)
    (tmp / "crates/norn-ir/src/lib.rs").write_text(
        """
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Entity,
    BusinessPack,
}
""",
        encoding="utf-8",
    )

    (tmp / "apps/studio/src/lib").mkdir(parents=True)
    (tmp / "apps/studio/src/lib/pages.ts").write_text(
        """
export const PAGES = {
  "/": "/",
  "/items": "/items",
} as const
""",
        encoding="utf-8",
    )

    (tmp / "verification/coverage-v1").mkdir(parents=True)
    (tmp / "verification/coverage-v1/promoted-gates.json").write_text(
        json.dumps(
            {
                "gates": [
                    {
                        "id": "demo-gate",
                        "classifications": [{"subsystem": "core", "layer": "runtime"}],
                    }
                ]
            }
        ),
        encoding="utf-8",
    )

    (tmp / "docs/spec/core").mkdir(parents=True)
    (tmp / "docs/spec/core/01-values-identity.md").write_text("# Spec\n", encoding="utf-8")


def _base_feature(**overrides) -> dict:
    feat = {
        "id": "demo-feature",
        "title": "Demo feature",
        "domain": "core-engine",
        "specContracts": ["docs/spec/core/01-values-identity.md"],
        "status": "implemented",
        "evidenceTier": "runtime-enforced",
        "surfaces": [
            "http:GET /v1/items",
            "http:POST /v1/items",
            "cli:norn check",
            "cli:norn cell plan",
            "cli:norn cell apply",
            "ir:entity",
            "ir:business_pack",
            "studio:/",
            "studio:/items",
            "gate:demo-gate",
        ],
        "sourcePaths": ["crates/norn-ir/src/lib.rs"],
        "invariants": ["demo-gate"],
        "probe": {
            "drive": "Run `norn check` and `GET /v1/items`.",
            "observableEndState": "Exit code 0 and JSON list returned.",
        },
    }
    feat.update(overrides)
    return feat


def _write_domain(tmp: Path, features: list[dict], domain: str = "core-engine") -> None:
    doc = {
        "schemaVersion": 1,
        "stopGap": {
            "status": "stop-gap",
            "retirementTrigger": "Retire once TARGET Norn authoring hosts this ontology in a cell.",
        },
        "domain": domain,
        "title": "Core Engine",
        "summary": "Synthetic test domain.",
        "features": features,
    }
    (tmp / "registry/features" / f"{domain}.json").write_text(
        json.dumps(doc, indent=2) + "\n",
        encoding="utf-8",
    )


class FeatureMapTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        _make_synthetic_repo(self.tmp)

    def test_auto_extraction_reads_all_five_surface_families(self):
        extracted, gate_meta = fm.extract_all_surfaces(self.tmp)
        self.assertEqual(extracted["http"], {"http:GET /v1/items", "http:POST /v1/items"})
        self.assertEqual(
            extracted["cli"],
            {"cli:norn check", "cli:norn cell plan", "cli:norn cell apply"},
        )
        self.assertEqual(extracted["ir"], {"ir:entity", "ir:business_pack"})
        self.assertEqual(extracted["studio"], {"studio:/", "studio:/items"})
        self.assertEqual(extracted["gate"], {"gate:demo-gate"})
        self.assertIn("demo-gate", gate_meta)

    def test_law1_refuses_orphan_surface(self):
        feat = _base_feature(
            surfaces=[s for s in _base_feature()["surfaces"] if s != "http:POST /v1/items"]
        )
        _write_domain(self.tmp, [feat])
        with self.assertRaisesRegex(fm.FeatureMapError, r"\[Law 1\] orphan live surface.*http:POST /v1/items"):
            fm.load_registry(self.tmp)

    def test_law1_refuses_dangling_surface_claim(self):
        feat = _base_feature(surfaces=[*_base_feature()["surfaces"], "http:DELETE /v1/ghost"])
        _write_domain(self.tmp, [feat])
        with self.assertRaisesRegex(fm.FeatureMapError, r"\[Law 1\] claimed surface.*http:DELETE /v1/ghost"):
            fm.load_registry(self.tmp)

    def test_law2_refuses_unprobed_implemented_feature(self):
        feat = _base_feature(probe={"drive": "   ", "observableEndState": "ok"})
        _write_domain(self.tmp, [feat])
        with self.assertRaisesRegex(fm.FeatureMapError, r"\[Law 2\] implemented feature must have non-empty probe\.drive"):
            fm.load_registry(self.tmp)

        feat2 = _base_feature(probe={"drive": "run it", "observableEndState": ""})
        _write_domain(self.tmp, [feat2])
        with self.assertRaisesRegex(
            fm.FeatureMapError, r"\[Law 2\] implemented feature must have non-empty probe\.observableEndState"
        ):
            fm.load_registry(self.tmp)

    def test_law2_refuses_missing_paths_and_unknown_invariants(self):
        feat = _base_feature(sourcePaths=["crates/does-not-exist.rs"])
        _write_domain(self.tmp, [feat])
        with self.assertRaisesRegex(fm.FeatureMapError, r"\[Law 2\] sourcePath.*does not exist"):
            fm.load_registry(self.tmp)

        feat2 = _base_feature(specContracts=["docs/spec/core/99-missing.md"])
        _write_domain(self.tmp, [feat2])
        with self.assertRaisesRegex(fm.FeatureMapError, r"\[Law 2\] specContract.*does not exist"):
            fm.load_registry(self.tmp)

        feat3 = _base_feature(invariants=["no-such-promoted-gate"])
        _write_domain(self.tmp, [feat3])
        with self.assertRaisesRegex(fm.FeatureMapError, r"\[Law 2\] invariant 'no-such-promoted-gate'"):
            fm.load_registry(self.tmp)

    def test_law3_write_and_check_detects_staleness_and_stray_files(self):
        _write_domain(self.tmp, [_base_feature()])
        # Before write, check fails Law 3.
        err = io.StringIO()
        with redirect_stderr(err):
            self.assertEqual(fm.main(["check", "--root", str(self.tmp)]), 1)
        self.assertIn("[Law 3]", err.getvalue())

        # Write generates README.md and core-engine.md, then check passes.
        out = io.StringIO()
        with redirect_stdout(out):
            self.assertEqual(fm.main(["write", "--root", str(self.tmp)]), 0)
            self.assertEqual(fm.main(["check", "--root", str(self.tmp)]), 0)

        readme = self.tmp / fm.GENERATED_DIR / "README.md"
        self.assertTrue(readme.read_text(encoding="utf-8").startswith(fm.GENERATED_BANNER))

        # Tampering with a generated file fails Law 3.
        readme.write_text(readme.read_text(encoding="utf-8") + "\ntampered\n", encoding="utf-8")
        err2 = io.StringIO()
        with redirect_stderr(err2):
            self.assertEqual(fm.main(["check", "--root", str(self.tmp)]), 1)
        self.assertIn("[Law 3]", err2.getvalue())

        # Stray markdown file also fails Law 3, and write cleans it up.
        with redirect_stdout(io.StringIO()):
            self.assertEqual(fm.main(["write", "--root", str(self.tmp)]), 0)
        stray = self.tmp / fm.GENERATED_DIR / "stray.md"
        stray.write_text("# stray\n", encoding="utf-8")
        with redirect_stderr(io.StringIO()):
            self.assertEqual(fm.main(["check", "--root", str(self.tmp)]), 1)
        with redirect_stdout(io.StringIO()):
            self.assertEqual(fm.main(["write", "--root", str(self.tmp)]), 0)
        self.assertFalse(stray.exists())

    def test_staleness_digest_detects_source_changes(self):
        feat = _base_feature()
        _write_domain(self.tmp, [feat])
        digest_v1 = fm.compute_source_digest(self.tmp, feat["sourcePaths"])
        self.assertTrue(digest_v1.startswith("sha256:"))

        # Record matching lastVerified -> fresh, omitted by `list --stale`.
        feat["lastVerified"] = {"commitSha": "abcdef1", "sourceDigest": digest_v1}
        _write_domain(self.tmp, [feat])
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = fm.main(["list", "--stale", "--json", "--root", str(self.tmp)])
        self.assertEqual(rc, 0)
        self.assertEqual(json.loads(buf.getvalue()), [])

        # Mutate the source file -> digest changes -> `list --stale` flags it!
        src = self.tmp / "crates/norn-ir/src/lib.rs"
        src.write_text(src.read_text(encoding="utf-8") + "\n// edited\n", encoding="utf-8")
        digest_v2 = fm.compute_source_digest(self.tmp, feat["sourcePaths"])
        self.assertNotEqual(digest_v1, digest_v2)

        buf2 = io.StringIO()
        with redirect_stdout(buf2):
            rc = fm.main(["list", "--stale", "--json", "--root", str(self.tmp)])
        self.assertEqual(rc, 0)
        rows = json.loads(buf2.getvalue())
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["id"], "demo-feature")
        self.assertEqual(rows[0]["reason"], "source-changed")
        self.assertEqual(rows[0]["liveSourceDigest"], digest_v2)

    def test_show_command_renders_feature_details(self):
        _write_domain(self.tmp, [_base_feature()])
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = fm.main(["show", "demo-feature", "--root", str(self.tmp)])
        self.assertEqual(rc, 0)
        text = buf.getvalue()
        self.assertIn("demo-feature", text)
        self.assertIn("docs/spec/core/01-values-identity.md", text)
        self.assertIn("demo-gate", text)
        self.assertIn("http:GET /v1/items", text)

    def test_real_repository_passes_all_laws(self):
        out = io.StringIO()
        err = io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            rc = fm.main(["check", "--root", str(ROOT)])
        self.assertEqual(rc, 0, msg=err.getvalue())
        self.assertIn("all 3 laws pass", out.getvalue())


if __name__ == "__main__":
    unittest.main()
