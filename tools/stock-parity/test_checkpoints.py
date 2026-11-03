"""CP1-FIFO-seven: recorded synthetic observations are not a live parity pass."""
from copy import deepcopy
import json
from pathlib import Path
import unittest
from check_smoke import check_fifo


class FrozenCorpus(unittest.TestCase):
    def setUp(self):
        self.dump = json.loads(Path(__file__).with_name('fifo-oracle.fixture.json').read_text())

    def test_recorded_seven_checkpoint_oracle(self):
        check_fifo(self.dump)

    def test_missing_case_or_initial_and_cross_layer_drift_refuse(self):
        bad = deepcopy(self.dump)
        bad['cases'] = bad['cases'][1:]
        with self.assertRaises(AssertionError):
            check_fifo(bad)
        for name in ('cross-layer-fifo', 'receipt-delivery'):
            bad = deepcopy(self.dump)
            next(c for c in bad['cases'] if c['case'] == name)['initial']['stock'] = [{}]
            with self.assertRaises(AssertionError):
                check_fifo(bad)
        bad = deepcopy(self.dump)
        next(c for c in bad['cases'] if c['case'] == 'cross-layer-fifo')['checkpoints'][-1]['stock'][-1]['value'] = '21'
        with self.assertRaises(AssertionError):
            check_fifo(bad)

    def test_wrong_intermediate_queue_receipt_account_or_history_refuse(self):
        for change in ('queue', 'account', 'history'):
            bad = deepcopy(self.dump)
            case = next(c for c in bad['cases'] if c['case'] == 'cross-layer-fifo')
            if change == 'queue':
                case['checkpoints'][1]['stock'][-1]['fifo'] = [['5', '16']]
            elif change == 'account':
                next(r for r in case['checkpoints'][1]['gl'] if r['requestId'] == 'receive-two' and r['credit'] == '60')['account'] = 'stock-a'
            else:
                case['checkpoints'][2]['stock'][0]['value'] = '19'
            with self.assertRaises(AssertionError):
                check_fifo(bad)
