"""Correspondence for DIFF-EXACT, DIFF-REVIEW, DIFF-COVERAGE and DIFF-PUBLIC."""
from copy import deepcopy
import json
import unittest
from pathlib import Path
from compare import compare, markdown, exact, DECISIONS
from contract import unbound_norn


def dump(engine):
    return dict(json.loads(Path(__file__).with_name('comparison-fixture.json').read_text()), engine=engine)


class Contract(unittest.TestCase):
    def setUp(self):
        self.n, self.e = dump('norn'), dump('erpnext')
        self.trace = json.loads(Path(__file__).with_name('coverage-fixture.json').read_text())['trace']

    def report(self, dispositions=None):
        count = max(len(d["cases"][0]["checkpoints"]) for d in (self.n, self.e) if d["cases"])
        trace = dict(self.trace, id=self.n['cases'][0]['case'], steps=self.trace['steps'] * max(1, count))
        return compare(self.n, self.e, dispositions or [], [trace])

    def test_trace_coverage_and_unbound_adapter(self):
        traces = [self.trace]
        missing = unbound_norn(traces)
        self.assertEqual(missing['cases'][0]['reason'], 'not yet: #850')
        self.assertFalse(compare(missing, self.e, [], traces)['passed'])

    def test_stub_match_and_representation(self):
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '20.000000'
        self.assertTrue(self.report()['passed'])
        self.assertIn('| case-1 | fifo | 0 | matched |', markdown(self.report()))

    def test_earlier_divergence_survives_final_match(self):
        for d in (self.n, self.e):
            last = deepcopy(d['cases'][0]['checkpoints'][0])
            last['step'] = 1
            d['cases'][0]['checkpoints'].append(last)
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '20.000001'
        self.assertEqual(self.report()['counts']['untriaged'], 1)
        self.assertFalse(self.report()['passed'])

    def test_gl_gross_legs_not_final_net(self):
        leg = self.n['cases'][0]['checkpoints'][0]['gl'][0]
        leg.update(debit='20.01', credit='0.01')
        self.assertFalse(self.report()['passed'])
        self.assertRaises(ValueError, exact, '0.001', 2)

    def test_refusal_identity_even_if_both_engines_do_it(self):
        for change in ('replace', 'swap'):
            self.n, self.e = dump('norn'), dump('erpnext')
            for d in (self.n, self.e):
                before = d['cases'][0]['checkpoints'][0]
                before['gl'].append(dict(before['gl'][0], id='second', debit='1'))
                cp = deepcopy(before)
                cp['step'] = 1
                cp['attempts'][0].update(outcome='refused', reason='insufficient-stock')
                cp['gl'][0]['id'], cp['gl'][1]['id'] = (
                    ('replacement', 'second') if change == 'replace' else ('second', 'native-gl'))
                d['cases'][0]['checkpoints'].append(cp)
            self.assertFalse(self.report()['passed'])
            difference = self.report()['cases'][0]['checkpoints'][2]['differences'][0]
            evidence = 'https://github.com/Norn-cloud/norn/issues/215'
            reviewed = [dict(fingerprint=difference['fingerprint'], bucket='intentional-difference',
                reviewer='reviewer', review=evidence, decision=DECISIONS[0], decisionRecord=evidence)]
            self.assertTrue(self.report(reviewed)['passed'])
            for d in (self.n, self.e):
                d['cases'][0]['checkpoints'][1]['gl'][0]['id'] = 'different-row'
            self.assertRaises(ValueError, self.report, reviewed)

    def test_coverage_and_mixed_refusal_boundaries(self):
        for case in json.loads(Path(__file__).with_name('coverage-fixture.json').read_text())['cases']:
            with self.subTest(case=case['name']):
                report = compare(case['norn'], case['erpnext'], [], case['traces'])
                self.assertEqual(report['passed'], case['passed'])
                for key, value in case['counts'].items():
                    self.assertEqual(report['counts'][key], value)

    def test_exactly_one_reviewed_bucket_bound_to_values(self):
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '21'
        diff = self.report()['cases'][0]['checkpoints'][1]['differences'][0]
        evidence = 'https://github.com/Norn-cloud/norn/issues/214#issuecomment-6551626530'
        d = dict(fingerprint=diff['fingerprint'], bucket='intentional-difference', reviewer='reviewer',
                 review=evidence, decision=DECISIONS[0], decisionRecord=evidence)
        self.assertTrue(self.report([d])['passed'])
        self.assertRaises(ValueError, self.report, [d, d])
        d.update(bucket='unsupported')
        self.assertRaises(ValueError, self.report, [d])
        d.update(bucket='erpnext-bug', reproduction=evidence, receipt=evidence, issue=evidence)
        self.assertRaises(ValueError, self.report, [d])
        d['invariant'] = 'POST-VALUE'
        self.assertEqual(self.report([d])['counts']['openErpnextBugs'], 1)
        d['bucket'] = 'norn-bug'
        self.assertEqual(self.report([d])['counts']['openNornBugs'], 1)
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '22'
        self.assertRaises(ValueError, self.report, [d])

    def test_gl_dispositions_are_per_request_account(self):
        leg = self.n['cases'][0]['checkpoints'][0]['gl'][0]
        leg['debit'] = '21'
        self.n['cases'][0]['checkpoints'][0]['gl'].append(dict(leg, account='cogs'))
        report = self.report()
        self.assertEqual(report['counts']['untriaged'], 2)
        evidence = 'https://github.com/Norn-cloud/norn/issues/215'
        dispositions = [dict(fingerprint=d['fingerprint'], bucket='norn-bug', reviewer='reviewer',
            review=evidence, issue=evidence, reproduction=evidence, receipt=evidence)
            for d in report['cases'][0]['checkpoints'][1]['differences']]
        self.assertEqual(self.report(dispositions)['counts']['openNornBugs'], 1)

    def test_public_report_omits_untrusted_labels(self):
        for d in (self.n, self.e):
            d['cases'][0]['case'] = 'private-host.example'
            d['cases'][0]['checkpoints'][0]['gl'][0]['account'] = 'private-account'
        public = json.dumps(self.report()) + markdown(self.report())
        self.assertNotIn('private', public)
        self.assertNotIn('native-gl', public)
        self.assertEqual(len(self.report()['cases'][0]['checkpoints'][1]['rawGl']['norn']), 1)
