"""Contract/golden checks run with the real pinned CodeGraph substrate."""
import json
import os
from pathlib import Path
import tempfile
import unittest

from codegraph import CodeGraphRust
from code_ontology import analyze, collect

ROOT = Path(__file__).parent


class CodeOntologyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.sources = collect([ROOT / "code-ontology-fixtures/demo"])
        cls.substrate = CodeGraphRust(Path(os.environ["CODEGRAPH_BIN"]), Path(cls.temp.name))
        cls.report = analyze(cls.sources, cls.substrate, "fixture")

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def test_golden_and_repeatability(self):
        expected = json.loads((ROOT / "code-ontology-fixtures/golden.json").read_text())
        self.assertEqual(self.report, expected)
        self.assertEqual(self.report, analyze(dict(reversed(list(self.sources.items()))), self.substrate, "fixture"))

    def test_all_relations(self):
        self.assertEqual({f["relation"] for f in self.report["facts"]},
                         {"reads", "writes", "triggers_on", "calls", "schedules", "throws_if", "exposes_api"})

    def test_no_guessed_receivers_or_fake_scripts(self):
        facts = self.report["facts"]
        self.assertFalse(any(f["entity"] == "Fake" for f in facts))
        uncertain = [f for f in facts if f["field"] and f["handler"].endswith((":dynamic", ":branch", ".nested"))]
        self.assertTrue(uncertain)
        self.assertTrue(all(f["confidence"] == "unresolved" for f in uncertain))
        self.assertTrue(all((f["confidence"] == "unresolved") == (f["diagnostic"] is not None) for f in facts))

    def test_hook_receiver_and_api(self):
        facts = self.report["facts"]
        self.assertTrue(any(f["handler"].endswith(":on_update") and f["field"] == "total" and f["confidence"] == "resolved" for f in facts))
        self.assertTrue(any(f["relation"] == "exposes_api" and f["target"] == "demo.api.update" for f in facts))
        self.assertTrue(any(f["relation"] == "calls" and f["target"].endswith(":helper") and f["confidence"] == "resolved" for f in facts))

    def test_compound_receivers_and_same_line_calls(self):
        facts = self.report["facts"]
        self.assertTrue(all(f["confidence"] == "unresolved" for f in facts
                            if f["handler"].endswith(":loop") and f["field"]))
        self.assertTrue(any(f["handler"].endswith(":same_line") and f["target"] == "external"
                            and f["diagnostic"] == "unresolved_call" for f in facts))
        self.assertTrue(any(f["diagnostic"] == "dynamic_client_handlers" for f in facts))

    def test_comprehension_scopes_do_not_inherit_receiver_type(self):
        facts = [f for f in self.report["facts"] if f["handler"].endswith(":comprehensions") and f["field"]]
        source = self.sources["demo/demo/api.py"].decode().splitlines()
        inner = [f for f in facts if " for " in source[f["source"]["line"] - 1]]
        outer = [f for f in facts if "still_typed =" in source[f["source"]["line"] - 1]]
        self.assertEqual(len(inner), 6)
        self.assertTrue(all(f["diagnostic"] == "untyped_document" and f["entity"] is None for f in inner))
        self.assertEqual(len(outer), 1)
        self.assertEqual((outer[0]["entity"], outer[0]["confidence"]), ("Invoice", "resolved"))

    def test_client_nested_setters_and_updates(self):
        facts = [f for f in self.report["facts"] if ".scope_checks@" in f["handler"] and f["field"]]
        source = self.sources["demo/demo/invoice.js"].decode().splitlines()
        nested = [f for f in facts if "function inner" in source[f["source"]["line"] - 1] or "const arrow" in source[f["source"]["line"] - 1]]
        self.assertEqual(len(nested), 2)
        self.assertTrue(all(f["diagnostic"] == "untyped_document" and f["entity"] is None for f in nested))
        outer = [f for f in facts if '"outer"' in source[f["source"]["line"] - 1]]
        self.assertEqual(len(outer), 1)
        self.assertEqual(outer[0]["confidence"], "resolved")
        for operator in ("++", "--"):
            updates = [f for f in facts if operator in source[f["source"]["line"] - 1]]
            self.assertEqual({f["relation"] for f in updates}, {"reads", "writes"})
            self.assertTrue(all(f["confidence"] == "resolved" for f in updates))

    def test_invalidated_alias_retains_unresolved_field_gap(self):
        facts = [f for f in self.report["facts"] if f["handler"].endswith(":alias_invalidation") and f["field"] == "total"]
        self.assertEqual(len(facts), 2)
        self.assertEqual([f["confidence"] for f in sorted(facts, key=lambda f: f["source"]["line"])],
                         ["resolved", "unresolved"])
        self.assertTrue(any(f["diagnostic"] == "untyped_document" for f in facts))

    def test_catch_parameter_shadows_form_receiver(self):
        facts = [f for f in self.report["facts"] if ".catch_scope@" in f["handler"] and f["field"]]
        source = self.sources["demo/demo/invoice.js"].decode().splitlines()
        caught = [f for f in facts if "caught" in source[f["source"]["line"] - 1] or "console.log(frm.doc.total)" in source[f["source"]["line"] - 1]]
        self.assertEqual(len(caught), 2)
        self.assertTrue(all(f["diagnostic"] == "untyped_document" and f["entity"] is None for f in caught))
        after = [f for f in facts if "after catch" in source[f["source"]["line"] - 1]]
        self.assertEqual(len(after), 1)
        self.assertEqual(after[0]["confidence"], "resolved")

    def test_compound_alias_and_shared_hook_gaps(self):
        facts = self.report["facts"]
        for symbol, count in (("alias_loop", 2), ("exception_alias", 1), ("shared_wildcard", 1), ("shared_unknown", 1), ("match_captures", 4)):
            fields = [f for f in facts if f["handler"].endswith(":" + symbol) and f["field"] == "total"]
            self.assertEqual(len(fields), count, symbol)
            self.assertTrue(all(f["entity"] is None and f["diagnostic"] == "untyped_document" for f in fields), symbol)

    def test_source_untouched(self):
        self.assertEqual(self.sources, collect([ROOT / "code-ontology-fixtures/demo"]))
        self.assertFalse((ROOT / "code-ontology-fixtures/demo/.codegraph").exists())

    def test_coverage_denominator(self):
        edges = [f for f in self.report["facts"] if f["relation"] in {"reads", "writes", "triggers_on"}]
        coverage = self.report["coverage"]["demo"]
        self.assertEqual(coverage["entityEdges"], len(edges))
        self.assertEqual(coverage["resolvedEntityEdges"], sum(f["confidence"] == "resolved" for f in edges))


if __name__ == "__main__":
    unittest.main()
