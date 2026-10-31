import json
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from contracts import project


class ContractTests(unittest.TestCase):
    def inputs(self):
        # Fresh synthetic scenario; no ERPNext test code, fixture or expected value.
        vocabulary = ['norn.finance.invoice.post', 'norn.finance.payment.post', 'norn.finance.invoice.amount.total',
                      'norn.finance.payment.amount', 'norn.finance.guard.period.closed']
        mapping = dict(status='proposed_v0', vocabulary=vocabulary, entry_points={'private_invoice': vocabulary[0], 'private_payment': vocabulary[1]},
                       properties={'["PrivateInvoice","amount"]': vocabulary[2], '["PrivatePayment","amount"]': vocabulary[3]},
                       guards={'private_predicate()': vocabulary[4]})
        def fact(handler, relation, entity=None, field=None, condition=None):
            return dict(handler=handler, relation=relation, entity=entity, field=field, condition=condition,
                        confidence='resolved', diagnostic=None, source={'digest': 'a' * 64})
        return dict(format='norn.code-ontology/1', facts=[
            fact('private_invoice', 'reads', 'PrivateInvoice', 'amount'),
            fact('private_invoice', 'writes', 'PrivateInvoice', 'amount'),
            fact('private_invoice', 'throws_if', condition='private_predicate()'),
            fact('private_payment', 'writes', 'PrivatePayment', 'amount'),
            fact('private_payment', 'calls', condition='private_unmapped')]), mapping

    def test_pair_effects_and_guards_clean_boundary(self):
        facts, mapping = self.inputs()
        report = project(facts, mapping)
        self.assertEqual(report['mapping_status'], 'proposed_v0')
        self.assertEqual(report['coverage'], {'entry_points': 2, 'with_mapped_facts': 2})
        invoice, payment = report['contracts']
        self.assertEqual(invoice['effects']['writes'], ['norn.finance.invoice.amount.total'])
        self.assertEqual(invoice['guards'], ['norn.finance.guard.period.closed'])
        self.assertEqual(payment['coverage'], {'source_facts': 2, 'mapped': 1, 'unknown': 1})
        encoded = json.dumps(report)
        for private in ['private_invoice', 'private_payment', 'private_predicate', 'PrivateInvoice', 'PrivatePayment']:
            self.assertNotIn(private, encoded)
        self.assertIn('ordering_not_established', invoice['unknown'])
        self.assertEqual(invoice['validation'], 'hypothesis_not_parity')

    def test_unresolved_is_never_promoted(self):
        facts, mapping = self.inputs()
        facts['facts'][0]['confidence'] = 'unresolved'
        result = project(facts, mapping)['contracts'][0]
        self.assertEqual(result['effects']['reads'], [])

    def test_unmapped_term_rejected(self):
        facts, mapping = self.inputs()
        mapping['guards']['private_predicate()'] = 'private.expression'
        with self.assertRaises(ValueError):
            project(facts, mapping)

    def test_sets_and_no_inferred_dataflow(self):
        facts, mapping = self.inputs()
        facts['facts'].append(facts['facts'][0])
        result = project(facts, mapping)['contracts'][0]
        self.assertEqual(result['effects']['reads'], ['norn.finance.invoice.amount.total'])
        for key in ['dependencies', 'ordering', 'atomicity', 'permission_coverage']:
            self.assertEqual(result[key], [])

    def test_mapped_external_call_and_vocabulary_gate(self):
        facts, mapping = self.inputs()
        mapping['vocabulary'].append('norn.finance.ledger.entry')
        mapping['calls'] = {'private_ledger_call': 'norn.finance.ledger.entry'}
        facts['facts'][-1]['target'] = 'private_ledger_call'
        result = project(facts, mapping)['contracts'][1]
        self.assertEqual(result['effects']['calls'], ['norn.finance.ledger.entry'])
        self.assertEqual(result['coverage']['mapped'], 2)
        mapping['calls']['private_ledger_call'] = 'unadmitted.term'
        with self.assertRaises(ValueError):
            project(facts, mapping)


    def test_v0_catalog_terms_and_unknown_evidence(self):
        catalog = json.loads(Path(__file__).resolve().parents[1].joinpath('canon_v0.json').read_text())
        self.assertEqual(catalog['status'], 'proposed_v0')
        self.assertEqual(catalog['evidence_level'], 'declared')
        by_id = {term['id']: term for term in catalog['terms']}
        self.assertTrue(by_id['norn.finance.invoice.amount.outstanding']['derived'])
        self.assertTrue(by_id['norn.finance.invoice.state']['derived'])
        self.assertTrue(by_id['norn.finance.payment.amount.unallocated']['derived'])
        self.assertEqual(by_id['norn.core.resource.quantity']['condition'], 'only_when_stock_moves')
        facts, mapping = self.inputs()
        mapping['vocabulary'] = list(by_id)
        result = project(facts, mapping)
        for contract in result['contracts']:
            self.assertEqual(contract['evidence_level'], 'analyzed')
            for dimension in ['dependencies', 'ordering', 'atomicity']:
                self.assertEqual(contract[dimension], [])
                self.assertIn(dimension + '_not_established', contract['unknown'])
        self.assertNotIn('provisional.', json.dumps(result))

    def test_uppercase_or_empty_term_segments_rejected(self):
        for term in ['norn.finance.Invoice.post', 'norn..invoice.post', 'norn.finance.invoice.POST']:
            facts, mapping = self.inputs()
            mapping['vocabulary'].append(term)
            with self.assertRaises(ValueError):
                project(facts, mapping)
