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
