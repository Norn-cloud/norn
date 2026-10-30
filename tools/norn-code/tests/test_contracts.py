import json
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from contracts import project


class ContractTests(unittest.TestCase):
    def inputs(self):
        # Fresh synthetic scenario; no ERPNext test code, fixture or expected value.
        vocabulary = ['finance.invoice.post', 'finance.payment.post', 'finance.invoice.amount',
                      'finance.payment.amount', 'finance.period.closed']
        mapping = dict(vocabulary=vocabulary, entry_points={'private_invoice': vocabulary[0], 'private_payment': vocabulary[1]},
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
        self.assertEqual(report['coverage'], {'entry_points': 2, 'with_mapped_facts': 2})
        invoice, payment = report['contracts']
        self.assertEqual(invoice['effects']['writes'], ['finance.invoice.amount'])
        self.assertEqual(invoice['guards'], ['finance.period.closed'])
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
        self.assertEqual(result['effects']['reads'], ['finance.invoice.amount'])
        for key in ['dependencies', 'ordering', 'atomicity', 'permission_coverage']:
            self.assertEqual(result[key], [])
