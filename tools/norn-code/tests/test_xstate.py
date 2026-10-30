from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from xstate import extract, authoring, Unsupported

PREFIX = "import { createMachine as machine } from 'xstate';\n"
CONFIG = "{id:'Order', initial:'draft', states:{draft:{on:{SUBMIT:{target:'posted',guard:'allowed'}}}, posted:{type:'final'}}}"


class XStateTests(unittest.TestCase):
    def report(self, config=CONFIG):
        return extract({'order.ts': (PREFIX + 'machine(' + config + ');').encode()}, 'synthetic')

    def test_states_events_guards_and_evidence(self):
        report = self.report()
        self.assertEqual(report['unknown'], [])
        fact = report['extracted'][0]
        self.assertEqual(fact['states'], ['draft', 'posted'])
        self.assertEqual(fact['initial'], 'draft')
        self.assertEqual(fact['terminal'], ['posted'])
        self.assertEqual(fact['transitions'][0], {'event': 'SUBMIT', 'source': 'draft', 'target': 'posted', 'guard': 'allowed', 'evidence': fact['evidence']})
        self.assertEqual(fact['evidence']['line'], 2)
        self.assertEqual(fact['evidence']['revision'], 'synthetic')
        with self.assertRaises(Unsupported):
            authoring(fact, {})
        source = authoring(fact, {'allowed': 'true'})
        self.assertIn('Norn.type({', source)
        self.assertIn('Norn.unsafeZen<boolean>("true")', source)
        self.assertNotIn('irVersion', source)

    def test_dynamic_or_behavioral_config_is_a_gap(self):
        for config in ['config', CONFIG.replace("guard:'allowed'", 'guard:()=>true'),
                       CONFIG.replace("posted:{type:'final'}", "posted:{entry:'notify'}"),
                       CONFIG.replace("target:'posted'", "target:'draft'"),
                       CONFIG.replace("initial:'draft'", "initial:'missing'"),
                       CONFIG.replace("target:'posted'", "target:'#other'"),
                       CONFIG.replace("guard:'allowed'", "guard:'allowed',actions:'notify'")]:
            report = self.report(config)
            self.assertEqual(report['extracted'], [], config)
            self.assertEqual(len(report['unknown']), 1, config)

    def test_unrelated_factory_is_not_xstate(self):
        report = extract({'x.ts': ('const machine = f; machine(' + CONFIG + ');').encode()})
        self.assertEqual(report['extracted'], [])

    def test_shadowed_factory_is_a_gap(self):
        report = extract({'x.ts': (PREFIX + 'function f(machine) { return machine(' + CONFIG + '); }').encode()})
        self.assertEqual(report['extracted'], [])
        self.assertEqual(len(report['unknown']), 1)

    def test_malformed_literals_are_gaps(self):
        for config in [CONFIG.replace("initial:'draft'", 'initial:{}'),
                       CONFIG.replace("type:'final'", 'type:{}'),
                       CONFIG.replace("guard:'allowed'", "guard:''")]:
            self.assertEqual(self.report(config)['extracted'], [])
            self.assertEqual(len(self.report(config)['unknown']), 1)

    def test_unsupported_factory_forms_are_not_silent(self):
        for source in ["import * as X from 'xstate'; X.createMachine(" + CONFIG + ")",
                       "const X = require('xstate'); X.createMachine(" + CONFIG + ")",
                       "import { setup } from 'xstate'; setup({}).createMachine(" + CONFIG + ")"]:
            result = extract({'x.ts': source.encode()})
            self.assertEqual(len(result['unknown']), 1)
        result = extract({'x.ts': (PREFIX + 'machine(' + CONFIG + '); X.createMachine(' + CONFIG + ');').encode()})
        self.assertEqual(len(result['extracted']), 1)
        self.assertEqual(len(result['unknown']), 1)

    def test_authoring_name_grammar_and_comments(self):
        for identity in ['order', 'Order_2']:
            self.assertEqual(self.report(CONFIG.replace("id:'Order'", "id:'" + identity + "'"))['extracted'], [])
        result = extract({'x.ts': (PREFIX + 'machine(/* note */' + CONFIG + ');').encode()})
        self.assertEqual(len(result['extracted']), 1)

    def test_escaped_event_key_is_decoded(self):
        result = self.report(CONFIG.replace('SUBMIT:', "'S\\x55BMIT':"))
        self.assertEqual(result['extracted'][0]['transitions'][0]['event'], 'SUBMIT')
