import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from recognizer import analyze, collect


class RecognizerTests(unittest.TestCase):
    def test_six_goldens(self):
        cases = json.loads(Path(__file__).with_name('fingerprints.json').read_text())
        for case in cases:
            with self.subTest(framework=case['framework']):
                data = case['source'].encode()
                report = analyze({case['file']: data}, 'synthetic-v1')
                self.assertEqual(report['recognized'], [{
                    'framework': case['framework'], 'confidence': case['confidence'], 'signal': case['signal'],
                    'evidence': {'file': case['file'], 'line': case['line'], 'revision': 'synthetic-v1',
                                 'source_hash': hashlib.sha256(data).hexdigest(),
                                 'level': 'declared' if case['signal'].startswith('config:') else 'analyzed'}}])
                self.assertEqual(report['unknown'], [])

    def test_manifests(self):
        sources = {'package.json': b'{"dependencies":{"xstate":"^5","effect":"4","@temporalio/workflow":"1"}}',
                   'pyproject.toml': b'[project]\ndependencies = ["frappe>=15", "odoo==18"]\n'}
        report = analyze(sources)
        self.assertEqual({m['framework'] for m in report['recognized']}, {'xstate','effect','temporal','frappe','odoo'})
        self.assertTrue(all(m['confidence'] == .98 for m in report['recognized']))
        self.assertEqual(report, analyze(dict(reversed(list(sources.items())))))

    def test_comments_strings_and_similar_packages_are_not_imports(self):
        for source in [b'// import { x } from "xstate";', b'const s = "import x from xstate";',
                       b'import x from "xstate-other";', b'/* require("effect") */']:
            self.assertEqual(analyze({'x.ts': source})['recognized'], [])

    def test_invalid_source_and_unrecognized_are_reported(self):
        report = analyze({'x.ts': b'import {', 'other.py': b'x = 3', 'package.json': b'{broken'})
        self.assertEqual(len(report['unknown']), 3)
        self.assertEqual(report['recognized'], [])

    def test_shopify_config_does_not_claim_graphql(self):
        report = analyze({'shopify.app.toml': b'name = "synthetic"'})
        self.assertEqual(report['recognized'], [])
        self.assertEqual(report['unknown'][0]['reason'], 'shopify_config_without_client_evidence')

    def test_commonjs_and_reexports(self):
        report = analyze({'x.js': b'const x = require("xstate");\nexport { Effect } from "effect";'})
        self.assertEqual({m['framework'] for m in report['recognized']}, {'xstate', 'effect'})

    def test_collect_rejects_links_and_skips_builds(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'node_modules').mkdir()
            (root / 'node_modules/x.ts').write_text('invalid')
            (root / 'x.py').write_text('import frappe')
            self.assertEqual(list(collect(root)), ['x.py'])
            (root / 'link.py').symlink_to(root / 'x.py')
            with self.assertRaises(ValueError):
                collect(root)

class DecoratorTests(unittest.TestCase):
    def test_bound_decorator(self):
        report = analyze({'x.py': b'from odoo import api as a\n@a.constrains("amount")\ndef check(): pass\n'})
        self.assertEqual(report['recognized'][1]['signal'], 'decorator:odoo.api.constrains')
        self.assertEqual(report['recognized'][1]['evidence']['line'], 2)


class RobustnessTests(unittest.TestCase):
    def test_relative_import_is_local(self):
        self.assertEqual(analyze({'x.py': b'from .odoo import api\n@api.constrains("x")\ndef f(): pass'})['recognized'], [])

    def test_deep_expression_does_not_stop_scan(self):
        report = analyze({'x.ts': b'const x = ' + b'a + ' * 2000 + b'a; import "effect";'})
        self.assertEqual(report['recognized'][0]['framework'], 'effect')

    def test_escaped_import_does_not_hide_later_import(self):
        report = analyze({'x.ts': b'import "\\x66s"; import "effect";'})
        self.assertEqual(report['recognized'][0]['framework'], 'effect')

class ReviewRegressions(unittest.TestCase):
    def test_type_only_declarations_do_not_claim_runtime_use(self):
        for source in ['import type { State } from "xstate";', 'export type { Effect } from "effect";', 'import { type State } from "xstate";']:
            self.assertEqual(analyze({'x.ts': source.encode()})['recognized'], [])
        self.assertEqual(analyze({'x.ts': b'export { Effect } from "effect";'})['recognized'][0]['framework'], 'effect')

    def test_nested_import_does_not_change_outer_decorator(self):
        source = b'from odoo import api\ndef unrelated():\n from other import api\n@api.depends("x")\ndef check(): pass\n'
        self.assertTrue(any(m['signal']=='decorator:odoo.api.depends' for m in analyze({'x.py':source})['recognized']))
        source = b'from odoo import api\napi = object()\n@api.depends("x")\ndef check(): pass\n'
        self.assertFalse(any(m['signal'].startswith('decorator:') for m in analyze({'x.py':source})['recognized']))

    def test_configuration_line_evidence(self):
        sources = {'hooks.py':b'# header\n\napp_name = "x"\n', 'package.json': b'{\n"dependencies": {\n"effect": "4"\n}}', 'pyproject.toml':b'[project]\nname="x"\ndependencies=[\n"frappe>=15"\n]\n'}
        by_path = {m['evidence']['file']:m['evidence']['line'] for m in analyze(sources)['recognized']}
        self.assertEqual(by_path, {'hooks.py':3,'package.json':3,'pyproject.toml':4})


if __name__ == '__main__':
    unittest.main()
