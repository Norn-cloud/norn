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

    def report(self, dispositions=None):
        count = max(len(d["cases"][0]["checkpoints"]) for d in (self.n, self.e) if d["cases"])
        return compare(self.n, self.e, dispositions or [], [dict(format="norn.stock-trace/1",
            id=self.n["cases"][0]["case"], method="fifo", steps=[{}] * max(1, count))])

    def test_trace_coverage_and_unbound_adapter(self):
        traces = [dict(format='norn.stock-trace/1', id='synthetic', method='fifo', steps=[{}, {}])]
        self.assertFalse(compare(self.n, self.e, [], traces)['passed'])
        missing = unbound_norn(traces)
        self.assertEqual(missing['cases'][0]['reason'], 'not yet: #694')
        self.assertFalse(compare(missing, self.e, [], traces)['passed'])
        self.n['cases'] = self.e['cases'] = []
        self.assertFalse(compare(self.n, self.e, [], traces)['passed'])

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
        with self.assertRaises(ValueError):
            exact('0.001', 2)

    def test_refusal_identity_even_if_both_engines_do_it(self):
        for d in (self.n, self.e):
            cp = deepcopy(d['cases'][0]['checkpoints'][0])
            cp['step'] = 1
            cp['attempts'][0].update(outcome='refused', reason='insufficient-stock')
            cp['gl'][0]['id'] = 'replacement'
            d['cases'][0]['checkpoints'].append(cp)
        self.assertFalse(self.report()['passed'])

    def test_missing_and_unsupported_fail(self):
        for field, value in [('checkpoints', []), ('status', 'unsupported')]:
            self.e = dump('erpnext')
            self.e['cases'][0][field] = value
            self.assertFalse(self.report()['passed'])
        self.e['cases'] = []
        self.assertEqual(self.report()['counts']['unsupported'], 1)

    def test_incomplete_checkpoint_and_refusal_after_missing_observation(self):
        del self.e['cases'][0]['checkpoints'][0]['gl']
        self.assertFalse(self.report()['passed'])
        self.e = dump('erpnext')
        for d in (self.n, self.e):
            d['cases'][0]['initial'] = dict(step=-1, status='unsupported')
            d['cases'][0]['checkpoints'][0]['attempts'][0]['outcome'] = 'refused'
        counts = self.report()['counts']
        self.assertEqual((counts['unsupported'], counts['untriaged']), (2, 0))

    def test_unavailable_snapshot_is_not_an_empty_observation(self):
        for dump in (self.n, self.e):
            dump['cases'][0]['checkpoints'][0]['status'] = 'unsupported'
        self.assertFalse(self.report()['passed'])

    def test_incomplete_prefix_is_still_compared(self):
        self.e['cases'][0]['status'] = 'unsupported'
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '21'
        report = self.report()
        self.assertEqual(report['counts']['unsupported'], 1)
        self.assertEqual(report['counts']['untriaged'], 1)
        self.assertFalse(report['passed'])

    def test_exactly_one_reviewed_bucket_bound_to_values(self):
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '21'
        diff = self.report()['cases'][0]['checkpoints'][1]['differences'][0]
        evidence = 'https://github.com/Norn-cloud/norn/issues/214#issuecomment-6551626530'
        d = dict(fingerprint=diff['fingerprint'], bucket='intentional-difference', reviewer='reviewer',
                 review=evidence, decision=DECISIONS[0], decisionRecord=evidence)
        self.assertTrue(self.report([d])['passed'])
        with self.assertRaises(ValueError):
            self.report([d, d])
        d.update(bucket='unsupported')
        with self.assertRaises(ValueError):
            self.report([d])
        d.update(bucket='erpnext-bug', reproduction=evidence, receipt=evidence, issue=evidence)
        with self.assertRaises(ValueError):
            self.report([d])
        d['invariant'] = 'POST-VALUE'
        self.assertEqual(self.report([d])['counts']['openErpnextBugs'], 1)
        d['bucket'] = 'norn-bug'
        self.assertEqual(self.report([d])['counts']['openNornBugs'], 1)
        self.n['cases'][0]['checkpoints'][0]['stock'][0]['value'] = '22'
        with self.assertRaises(ValueError):
            self.report([d])

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
