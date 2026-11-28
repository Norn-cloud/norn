#!/usr/bin/env python3
"""Unit tests for the stop-gap auto-extracted feature map (scripts/feature_map.py)."""

from __future__ import annotations

import io
import json
import os
import shutil
import subprocess
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

    def test_staleness_digest_detects_source_and_probe_changes(self):
        feat = _base_feature()
        _write_domain(self.tmp, [feat])
        digest_v1 = fm.compute_verification_digest(self.tmp, feat)
        self.assertTrue(digest_v1.startswith("sha256:"))

        # Record matching lastVerified -> fresh, omitted by `list --stale`.
        feat["lastVerified"] = {"commitSha": "abcdef1", "sourceDigest": digest_v1}
        _write_domain(self.tmp, [feat])
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = fm.main(["list", "--stale", "--json", "--root", str(self.tmp)])
        self.assertEqual(rc, 0)
        self.assertEqual(json.loads(buf.getvalue()), [])

        # Mutate ONLY the probe -> verification digest changes -> `list --stale` flags it!
        feat_probe_changed = {
            **feat,
            "probe": {
                **feat["probe"],
                "drive": "Run `norn check` and `GET /v1/items?orgId=o&environmentId=e`.",
            },
        }
        _write_domain(self.tmp, [feat_probe_changed])
        digest_probe = fm.compute_verification_digest(self.tmp, feat_probe_changed)
        self.assertNotEqual(digest_v1, digest_probe)
        buf_probe = io.StringIO()
        with redirect_stdout(buf_probe):
            rc = fm.main(["list", "--stale", "--json", "--root", str(self.tmp)])
        self.assertEqual(rc, 0)
        rows_probe = json.loads(buf_probe.getvalue())
        self.assertEqual(len(rows_probe), 1)
        self.assertEqual(rows_probe[0]["reason"], "source-changed")
        self.assertEqual(rows_probe[0]["liveSourceDigest"], digest_probe)

        # Restore original probe and mutate the source file -> digest changes -> `list --stale` flags it!
        _write_domain(self.tmp, [feat])
        src = self.tmp / "crates/norn-ir/src/lib.rs"
        src.write_text(src.read_text(encoding="utf-8") + "\n// edited\n", encoding="utf-8")
        digest_v2 = fm.compute_verification_digest(self.tmp, feat)
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

    def test_extractors_handle_trace_single_quotes_serde_cli_ts_and_duplicate_gates(self):
        # 1. HTTP OpenAPI: TRACE method must be extracted alongside all standard HTTP methods.
        openapi = self.tmp / "sdks/openapi/norn-api.openapi.json"
        doc = json.loads(openapi.read_text(encoding="utf-8"))
        doc["paths"]["/v1/items"]["trace"] = {"operationId": "trace_items"}
        openapi.write_text(json.dumps(doc), encoding="utf-8")
        self.assertIn("http:TRACE /v1/items", fm.extract_http_surfaces(self.tmp))

        # 2. Studio pages.ts: single-quoted routes must be extracted; dynamic templates must be refused.
        pages = self.tmp / "apps/studio/src/lib/pages.ts"
        pages.write_text(
            "export const PAGES = {\n  \"/\": \"/\",\n  '/single-quoted': '/single-quoted',\n} as const\n",
            encoding="utf-8",
        )
        self.assertEqual(fm.extract_studio_surfaces(self.tmp), {"studio:/", "studio:/single-quoted"})

        pages.write_text(
            "export const PAGES = {\n  [`/${dynamic}`]: '/bad',\n} as const\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(fm.FeatureMapError, r"dynamic template route key"):
            fm.extract_studio_surfaces(self.tmp)

        # 3. IR ArtifactKind: honor #[serde(rename_all = "...")] and per-variant #[serde(rename = "...")].
        ir_lib = self.tmp / "crates/norn-ir/src/lib.rs"
        ir_lib.write_text(
            """
#[derive(Debug)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    BusinessPack,
    #[serde(rename = "custom_override")]
    SourceSchema,
}
""",
            encoding="utf-8",
        )
        self.assertEqual(fm.extract_ir_surfaces(self.tmp), {"ir:business-pack", "ir:custom_override"})

        # 4. CLI: separate Rust `cli:norn <cmd>` vs TypeScript `cli:norn-ts <cmd>` (including overlapping commands).
        ts_cli = self.tmp / "tools/norn-ts/src/cli.ts"
        ts_cli.parent.mkdir(parents=True)
        ts_cli.write_text(
            """
if (process.argv[2] === "dev") {}
if (process.argv[2] === 'init') {}
if (process.argv[2] === "seed") {}
if (command !== "check" && command !== "compile") {}
""",
            encoding="utf-8",
        )
        cli_surfaces = fm.extract_cli_surfaces(self.tmp)
        self.assertIn("cli:norn check", cli_surfaces)
        self.assertTrue(
            {"cli:norn-ts dev", "cli:norn-ts init", "cli:norn-ts seed", "cli:norn-ts check", "cli:norn-ts compile"}
            .issubset(cli_surfaces)
        )
        self.assertNotIn("cli:norn dev", cli_surfaces)
        self.assertNotIn("cli:norn init", cli_surfaces)

        # 5. Promoted gates: refuse duplicate gate IDs explicitly.
        gates_file = self.tmp / "verification/coverage-v1/promoted-gates.json"
        gates_file.write_text(
            json.dumps(
                {
                    "gates": [
                        {"id": "demo-gate", "classifications": []},
                        {"id": "demo-gate", "classifications": []},
                    ]
                }
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(fm.FeatureMapError, r"duplicate promoted gate id\(s\): demo-gate"):
            fm.extract_gate_surfaces(self.tmp)

    def test_invariant_symbol_validation(self):
        # Populate Rust file with a top-level constant outside ArtifactKind, struct, and impl methods.
        (self.tmp / "crates/norn-ir/src/lib.rs").write_text(
            """
pub const SUPPORTED_IR_VERSION: &str = "0.1";

#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Entity,
    BusinessPack,
}

impl ArtifactKind {
    pub const DEFAULT_KIND: Self = Self::Entity;
    pub fn as_str(&self) -> &'static str {
        let _ = SUPPORTED_IR_VERSION;
        "entity"
    }
}

pub struct Envelope {
    pub kind: ArtifactKind,
}
""",
            encoding="utf-8",
        )
        (self.tmp / "docs/spec/core/01-values-identity.md").write_text(
            """# Spec

## Action Wire Contract

Action requests carry `idempotencyKey` in the JSON body.

```sh
# fake-code-heading
```
""",
            encoding="utf-8",
        )

        # Present symbols (top-level type, enum variant, impl method/const, struct field, real Markdown heading) pass.
        feat_ok = _base_feature(
            invariants=[
                "demo-gate",
                "checker:crates/norn-ir/src/lib.rs::ArtifactKind",
                "checker:crates/norn-ir/src/lib.rs::ArtifactKind::Entity",
                "checker:crates/norn-ir/src/lib.rs::ArtifactKind::as_str",
                "checker:crates/norn-ir/src/lib.rs::ArtifactKind::DEFAULT_KIND",
                "checker:crates/norn-ir/src/lib.rs::Envelope::kind",
                "law:docs/spec/core/01-values-identity.md#spec",
                "law:docs/spec/core/01-values-identity.md#action-wire-contract",
            ]
        )
        _write_domain(self.tmp, [feat_ok])
        fm.load_registry(self.tmp)

        # Missing `::` symbol fails Law 2.
        feat_bad_rust = _base_feature(
            invariants=["demo-gate", "theorem:crates/norn-ir/src/lib.rs::DoesNotExistSymbol"]
        )
        _write_domain(self.tmp, [feat_bad_rust])
        with self.assertRaisesRegex(fm.FeatureMapError, r"cited symbol 'DoesNotExistSymbol' not found"):
            fm.load_registry(self.tmp)

        # Top-level constant outside `ArtifactKind` MUST be refused for `ArtifactKind::SUPPORTED_IR_VERSION`.
        feat_bad_qualified = _base_feature(
            invariants=["demo-gate", "checker:crates/norn-ir/src/lib.rs::ArtifactKind::SUPPORTED_IR_VERSION"]
        )
        _write_domain(self.tmp, [feat_bad_qualified])
        with self.assertRaisesRegex(
            fm.FeatureMapError, r"cited symbol 'ArtifactKind::SUPPORTED_IR_VERSION' not found"
        ):
            fm.load_registry(self.tmp)

        # Missing `#` heading anchor fails Law 2.
        feat_bad_anchor = _base_feature(
            invariants=["demo-gate", "law:docs/spec/core/01-values-identity.md#missing-anchor"]
        )
        _write_domain(self.tmp, [feat_bad_anchor])
        with self.assertRaisesRegex(fm.FeatureMapError, r"cited symbol 'missing-anchor' not found"):
            fm.load_registry(self.tmp)

        # Body-only identifier `#idempotencyKey` (not an actual Markdown heading) MUST be refused.
        feat_body_only_anchor = _base_feature(
            invariants=["demo-gate", "law:docs/spec/core/01-values-identity.md#idempotencyKey"]
        )
        _write_domain(self.tmp, [feat_body_only_anchor])
        with self.assertRaisesRegex(fm.FeatureMapError, r"cited symbol 'idempotencyKey' not found"):
            fm.load_registry(self.tmp)

        # Heading-like comment inside fenced code block MUST also be refused.
        feat_fenced_anchor = _base_feature(
            invariants=["demo-gate", "law:docs/spec/core/01-values-identity.md#fake-code-heading"]
        )
        _write_domain(self.tmp, [feat_fenced_anchor])
        with self.assertRaisesRegex(fm.FeatureMapError, r"cited symbol 'fake-code-heading' not found"):
            fm.load_registry(self.tmp)

    def test_digest_tracks_gate_quint_models_invariant_files_and_ts_cli_sources(self):
        # 1. Promoted gate Quint model file (resolved from promoted-gates.json command + coverage manifest)
        quint_model = self.tmp / "verification/hosted-timers-v1/models/hosted-timers.qnt"
        quint_model.parent.mkdir(parents=True)
        quint_model.write_text("module hostedTimers { val x = 1 }\n", encoding="utf-8")

        manifest_model = self.tmp / "verification/hosted-timers-v1/models/approval-fencing.qnt"
        manifest_model.write_text("module approvalFencing { val y = 1 }\n", encoding="utf-8")

        (self.tmp / "verification/coverage-v1/promoted-gates.json").write_text(
            json.dumps(
                {
                    "gates": [
                        {
                            "id": "demo-gate",
                            "command": "quint verify verification/hosted-timers-v1/models/hosted-timers.qnt --backend apalache",
                            "classifications": [{"subsystem": "transition-authority", "layer": "model"}],
                        }
                    ]
                }
            ),
            encoding="utf-8",
        )
        cm = _sibling("coverage_manifest")
        cm.write(
            {
                "schemaVersion": "norn.verification.coverage.v1",
                "subsystems": [
                    {
                        "id": "transition-authority",
                        "model": {
                            "status": "bounded_model_checked",
                            "artifacts": [
                                "verification/hosted-timers-v1/models/hosted-timers.qnt",
                                "verification/hosted-timers-v1/models/approval-fencing.qnt",
                            ],
                            "commands": [],
                            "generatedArtifacts": [],
                            "resultArtifacts": [],
                        },
                        "obligationClaims": [],
                    }
                ],
            },
            self.tmp / "verification/coverage-v1/manifest",
        )

        # 2. Cited invariant file outside sourcePaths
        inv_file = self.tmp / "crates/norn-checker/src/table_analysis.rs"
        inv_file.parent.mkdir(parents=True)
        inv_file.write_text("pub fn analyze() {}\n", encoding="utf-8")

        # 3. TypeScript CLI source under tools/norn-ts/src
        ts_fmt = self.tmp / "tools/norn-ts/src/fmt.ts"
        ts_fmt.parent.mkdir(parents=True)
        ts_fmt.write_text("export function fmt() { return 0 }\n", encoding="utf-8")

        feat = _base_feature(
            sourcePaths=["crates/norn-ir/src/lib.rs", "tools/norn-ts/src"],
            invariants=["demo-gate", "checker:crates/norn-checker/src/table_analysis.rs"],
        )
        d0 = fm.compute_verification_digest(self.tmp, feat, use_git=False)

        # Editing the promoted gate's Quint model file MUST change the feature's digest.
        quint_model.write_text("module hostedTimers { val x = 2 }\n", encoding="utf-8")
        d_quint = fm.compute_verification_digest(self.tmp, feat, use_git=False)
        self.assertNotEqual(d0, d_quint)

        # Editing a coverage-manifest subsystem artifact for that gate MUST also change the digest.
        manifest_model.write_text("module approvalFencing { val y = 2 }\n", encoding="utf-8")
        d_manifest = fm.compute_verification_digest(self.tmp, feat, use_git=False)
        self.assertNotEqual(d_quint, d_manifest)

        # Editing the cited invariant file MUST change the digest.
        inv_file.write_text("pub fn analyze() { /* changed */ }\n", encoding="utf-8")
        d_inv = fm.compute_verification_digest(self.tmp, feat, use_git=False)
        self.assertNotEqual(d_manifest, d_inv)

        # Editing tools/norn-ts/src/fmt.ts MUST change the digest.
        ts_fmt.write_text("export function fmt() { return 1 }\n", encoding="utf-8")
        d_ts = fm.compute_verification_digest(self.tmp, feat, use_git=False)
        self.assertNotEqual(d_inv, d_ts)

        # Also verify against the real repository registry that core-approval-fencing-and-timers
        # includes hosted-timers.qnt and cli-authoring-compile-and-format includes tools/norn-ts/src/fmt.ts.
        domains, _, gate_meta = fm.load_registry(ROOT)
        coverage_subsystems = fm._load_coverage_subsystems(ROOT)
        approval_feat = next(
            f for f in domains["core-engine"]["features"] if f["id"] == "core-approval-fencing-and-timers"
        )
        approval_paths = fm._feature_digest_paths(
            ROOT, approval_feat, gate_meta=gate_meta, coverage_subsystems=coverage_subsystems
        )
        self.assertIn("verification/hosted-timers-v1/models/hosted-timers.qnt", approval_paths)

        cli_feat = next(
            f for f in domains["authoring-and-cli"]["features"] if f["id"] == "cli-authoring-compile-and-format"
        )
        cli_files = {
            p.relative_to(ROOT).as_posix()
            for sp in fm._feature_digest_paths(
                ROOT, cli_feat, gate_meta=gate_meta, coverage_subsystems=coverage_subsystems
            )
            for p in fm._files_under_path(ROOT, sp, True)
        }
        self.assertIn("tools/norn-ts/src/fmt.ts", cli_files)

    def test_git_commit_verification_untracked_exclusion_and_symlink_rejection(self):
        def git(*args: str) -> str:
            res = subprocess.run(
                ["git", "-C", str(self.tmp), *args],
                check=True,
                capture_output=True,
                text=True,
            )
            return res.stdout.strip()

        git("init", "-q")
        git("config", "user.email", "test@norn.local")
        git("config", "user.name", "Norn Test")

        (self.tmp / "crates/norn-ir/src/sub").mkdir(parents=True)
        (self.tmp / "crates/norn-ir/src/sub/tracked.rs").write_text("pub const X: u8 = 1;\n", encoding="utf-8")

        feat = _base_feature(sourcePaths=["crates/norn-ir/src"])
        _write_domain(self.tmp, [feat])
        git("add", ".")
        git("commit", "-q", "-m", "initial")
        head_sha = git("rev-parse", "HEAD")

        # Untracked scratch file under directory sourcePath must NOT change digest.
        digest_clean = fm.compute_verification_digest(self.tmp, feat, use_git=True)
        scratch = self.tmp / "crates/norn-ir/src/untracked_scratch.tmp"
        scratch.write_text("do not poison digest\n", encoding="utf-8")
        digest_with_scratch = fm.compute_verification_digest(self.tmp, feat, use_git=True)
        self.assertEqual(digest_clean, digest_with_scratch)
        scratch.unlink()

        # Valid commitSha + matching digest passes load_registry and reports fresh.
        feat["lastVerified"] = {"commitSha": head_sha, "sourceDigest": digest_clean}
        _write_domain(self.tmp, [feat])
        fm.load_registry(self.tmp)
        info_ok = fm.staleness_info(self.tmp, feat, use_git=True)
        self.assertFalse(info_ok["stale"])
        self.assertEqual(info_ok["reason"], "fresh")

        # Nonexistent commitSha is refused by load_registry AND marked stale by staleness_info.
        feat_bad_sha = {**feat, "lastVerified": {"commitSha": "deadbeef1234", "sourceDigest": digest_clean}}
        _write_domain(self.tmp, [feat_bad_sha])
        with self.assertRaisesRegex(fm.FeatureMapError, r"lastVerified\.commitSha 'deadbeef1234' does not exist"):
            fm.load_registry(self.tmp)
        info_bad = fm.staleness_info(self.tmp, feat_bad_sha, use_git=True)
        self.assertTrue(info_bad["stale"])
        self.assertEqual(info_bad["reason"], "missing-commit")

        # Symlink parent directory replacement must be refused in BOTH git and non-git hashing modes.
        outside = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, outside)
        (outside / "tracked.rs").write_text("pub const X: u8 = 99;\n", encoding="utf-8")
        sub_dir = self.tmp / "crates/norn-ir/src/sub"
        shutil.rmtree(sub_dir)
        os.symlink(outside, sub_dir)

        with self.assertRaisesRegex(fm.FeatureMapError, r"refusing symlink"):
            fm.compute_source_digest(self.tmp, ["crates/norn-ir/src"], use_git=True)
        with self.assertRaisesRegex(fm.FeatureMapError, r"refusing symlink"):
            fm.compute_source_digest(self.tmp, ["crates/norn-ir/src"], use_git=False)
        with self.assertRaisesRegex(fm.FeatureMapError, r"refusing symlink"):
            fm.compute_source_digest(self.tmp, ["crates/norn-ir/src/sub/tracked.rs"], use_git=False)

    def test_parent_root_flag_before_subcommand_is_honored(self):
        _write_domain(self.tmp, [_base_feature()])
        # Generated views do not exist yet in self.tmp, while ROOT passes check.
        # Passing `--root <self.tmp>` BEFORE `check` must inspect self.tmp (and fail Law 3), not fall back to ROOT.
        err = io.StringIO()
        with redirect_stderr(err):
            rc = fm.main(["--root", str(self.tmp), "check"])
        self.assertEqual(rc, 1)
        self.assertIn("[Law 3]", err.getvalue())

        # Writing via `--root <self.tmp> write` then checking via `--root <self.tmp> check` succeeds and cites #2005.
        with redirect_stdout(io.StringIO()):
            self.assertEqual(fm.main(["--root", str(self.tmp), "write"]), 0)
            self.assertEqual(fm.main(["--root", str(self.tmp), "check"]), 0)
        readme = (self.tmp / fm.GENERATED_DIR / "README.md").read_text(encoding="utf-8")
        card = (self.tmp / fm.GENERATED_DIR / "core-engine.md").read_text(encoding="utf-8")
        self.assertIn("#2005", readme)
        self.assertIn("#2005", card)

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
