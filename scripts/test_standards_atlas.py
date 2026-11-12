#!/usr/bin/env python3
"""Regressions for the standards atlas validator and its coverage table."""

import json
import shutil
import tempfile
import unittest
from pathlib import Path


def _sibling(name, directory=None):
    """Load sibling module `name` by explicit path, never through sys.path: gated scripts
    run Python with PYTHONSAFEPATH or -I, so the script directory is not on it."""
    import importlib.util
    import pathlib
    import sys

    if name in sys.modules:
        return sys.modules[name]
    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {name} from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module

atlas = _sibling("standards_atlas")

ROOT = Path(__file__).resolve().parent.parent


def entry(entry_id="alpha-standard", **changes):
    base = {
        "id": entry_id,
        "name": "Alpha standard",
        "publisher": "Alpha body",
        "kind": "standard",
        "domains": ["finance"],
        "version": "1.0",
        "versionDate": "2026-01-01",
        "versionBasis": "Read from the publisher page.",
        "licence": {
            "spdx": "Apache-2.0",
            "text": "Apache License 2.0",
            "status": "verified",
            "basis": "LICENSE file read.",
            "termsUrl": "https://example.org/licence",
            "checked": "2026-11-11",
        },
        "reuse": "code-reusable",
        "formats": ["XSD"],
        "machineReadable": "yes",
        "artifactUrl": "https://example.org/alpha.xsd",
        "evidenceUrls": ["https://example.org/alpha"],
        "norn": {
            "overlap": "currency amounts",
            "concepts": ["core.Agent"],
            "irTargets": [],
            "proposedConcepts": ["Currency"],
            "mapping": "none",
            "mappingEvidence": [],
            "importerIssue": None,
        },
        "priority": {"sapParity": "P0", "reason": "Test."},
        "origin": "atlas-2026-11-11",
    }
    for key, value in changes.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            base[key] = {**base[key], **value}
        else:
            base[key] = value
    return base


class AtlasTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        (self.tmp / "registry/standards/atlas").mkdir(parents=True)
        shutil.copy(ROOT / atlas.SCHEMA, self.tmp / atlas.SCHEMA)
        artifacts = self.tmp / "registry/core/artifacts"
        artifacts.mkdir(parents=True)
        (artifacts / "core.Agent.interface.json").write_text(json.dumps({"artifactId": "core.Agent.interface"}))
        (self.tmp / "evidence.txt").write_text("evidence")

    def write(self, *entries, domain="finance", name=None):
        document = {"atlasVersion": 1, "domain": domain, "entries": list(entries)}
        path = self.tmp / "registry/standards/atlas" / f"{name or domain}.json"
        path.write_text(json.dumps(document))

    def refuses(self, pattern, *entries, **kwargs):
        self.write(*entries, **kwargs)
        with self.assertRaisesRegex(atlas.AtlasError, pattern):
            atlas.load(self.tmp)

    def test_valid_atlas_loads_and_renders(self):
        self.write(entry())
        loaded = atlas.load(self.tmp)
        self.assertIn("| finance | 1 | 0 | 0% | 0 | 1 | 1 |", atlas.render(loaded))

    def test_repository_atlas_is_valid_and_coverage_is_current(self):
        self.assertEqual(atlas.main(["--check"]), 0)

    def test_schema_rejects_unknown_fields_and_values(self):
        self.refuses("unknown field", entry(extra=1))
        self.refuses("not one of", entry(reuse="maybe"))
        self.refuses("unknown field customer1", entry(priority={"customer1": "P0"}))
        self.refuses("does not match", entry("alpha-standard\n"))
        self.refuses("importerIssue: matches no allowed form", entry(norn={"mapping": "planned", "importerIssue": "Norn-cloud/norn-platform#1"}))
        self.refuses("missing priority", {k: v for k, v in entry().items() if k != "priority"})

    def test_file_name_and_primary_domain_must_agree(self):
        self.refuses("named finance.json", entry(), name="payments")
        self.refuses("belongs in payments.json", entry(domains=["payments", "finance"]))

    def test_duplicate_and_unsorted_ids_refuse(self):
        self.refuses("sorted by id", entry("b-entry"), entry("a-entry"))
        self.write(entry())
        self.refuses("also used", entry(domains=["payments"]), domain="payments")

    def test_concepts_must_name_registry_artifacts(self):
        self.refuses("not a registry artifact", entry(norn={"concepts": ["core.Invoice"]}))

    def test_planned_mapping_needs_an_issue(self):
        self.refuses("importerIssue", entry(norn={"mapping": "planned"}))

    def test_mapped_entries_need_existing_evidence_and_a_target(self):
        self.refuses("cites mappingEvidence", entry(norn={"mapping": "done"}))
        self.refuses("does not exist", entry(norn={"mapping": "partial", "mappingEvidence": ["missing.txt"]}))
        self.refuses("does not exist", entry(norn={"mapping": "partial", "mappingEvidence": ["../evidence.txt"]}))
        self.refuses(
            "concept or an IR target",
            entry(norn={"mapping": "done", "concepts": [], "mappingEvidence": ["evidence.txt"]}),
        )
        self.refuses("only for partial or done", entry(norn={"mappingEvidence": ["evidence.txt"]}))
        self.write(entry(norn={"mapping": "done", "mappingEvidence": ["evidence.txt"]}))
        atlas.load(self.tmp)

    def test_licence_rules(self):
        self.refuses(
            "NOASSERTION cannot be a verified",
            entry(reuse="vocabulary-only", licence={"spdx": "NOASSERTION"}),
        )
        self.refuses("code-reusable needs", entry(licence={"status": "unverified"}))
        self.refuses(
            "cites termsUrl",
            entry(reuse="vocabulary-only", licence={"termsUrl": None}, evidenceUrls=[]),
        )

    def test_free_text_cannot_cite_private_or_bare_issues(self):
        self.refuses("issue reference #1301", entry(notes="Search returned #1301 historical."))
        self.refuses("names a private repository", entry(notes="see Norn-cloud/norn-platform#7"))
        self.refuses("issue reference #12", entry(priority={"reason": "Per (#12)."}))
        for text in ("norn#1301", "GH#1301", "platform#7", "secret-repo#7", "Other-org/norn#7"):
            self.refuses("issue reference", entry(notes=f"see {text} for details"))
        self.refuses(
            "issue URL outside Norn-cloud/norn",
            entry(notes="see https://github.com/Norn-cloud/secret-repo/issues/1301"),
        )
        self.refuses(
            "issue URL outside Norn-cloud/norn",
            entry(evidenceUrls=["https://github.com/other/repo/pull/3"]),
        )
        self.write(
            entry(
                notes="Tracked in Norn-cloud/norn#12 (and Norn-cloud/norn#13); see "
                "https://w3.org/time#2016, https://example.org/spec_(2026)#123 and "
                "https://github.com/Norn-cloud/norn/issues/12."
            )
        )
        atlas.load(self.tmp)

    def test_machine_readable_needs_an_artifact(self):
        self.refuses("artifactUrl", entry(artifactUrl=None))

    def test_next_mappings_rank_priority_then_planned_and_skip_reference_only(self):
        rows = [
            entry("c-later", priority={"sapParity": "P2"}),
            entry("b-planned", norn={"mapping": "planned", "importerIssue": "Norn-cloud/norn#1"}),
            entry("a-none"),
            entry("d-reference", reuse="reference-only", priority={"sapParity": "P0"}),
            entry("e-watch", priority={"sapParity": "watch"}),
        ]
        ranked = [row["id"] for row in atlas.next_mappings({"finance": rows})]
        self.assertEqual(ranked, ["b-planned", "a-none", "c-later"])


if __name__ == "__main__":
    unittest.main()
