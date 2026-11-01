"""Acceptance for the first live oracle checkpoint, never a parity claim."""
import json
import sys
from decimal import Decimal
from pathlib import Path


def check(dump):
    from rest import encode
    boundary = {'qty': Decimal('999999999999.999999'), 'items': [Decimal('0.000001')]}
    encoded = encode(boundary)
    assert json.loads(encoded, parse_float=Decimal) == boundary
    assert '"999999999999.999999"' not in encoded
    assert dump['format'] == 'norn.stock-dump/1' and dump['engine'] == 'erpnext'
    assert len(dump['cases']) == 1
    case = dump['cases'][0]
    assert case['case'] == 'receipt-delivery' and case['status'] == 'complete', case
    assert len(case['checkpoints']) == 2
    for cp, qty, value, leg_count in zip(case['checkpoints'], ('10', '7'), ('20', '14'), (2, 4)):
        assert cp['attempts'][0]['outcome'] == 'accepted', cp['attempts']
        assert all(d['status'] == 1 for d in cp['documents'])
        assert len(cp['gl']) == leg_count
        assert sum(Decimal(r['debit']) - Decimal(r['credit']) for r in cp['gl']) == 0
        stock = cp['stock'][-1]
        assert (stock['quantity'], stock['value'], stock['valuationRate']) == (qty, value, '2')
        assert stock['fifo'] == [[qty, '2']]
    final = case['checkpoints'][-1]
    assert len(final['stock']) == 2
    assert [(r['quantityDelta'], r['valueDelta']) for r in final['stock']] == [('10', '20'), ('-3', '-6')]
    assert {(r['requestId'], r['account'], r['debit'], r['credit']) for r in final['gl']} == {
        ('receive', 'stock-a', '20', '0'), ('receive', 'srbnb', '0', '20'),
        ('deliver', 'stock-a', '0', '6'), ('deliver', 'cogs', '6', '0')}


if __name__ == '__main__':
    check(json.loads(Path(sys.argv[1]).read_text()))
    print('Live receipt/delivery checkpoint passed: 2 stock entries, 4 balanced GL legs, Q=7, V=14')
