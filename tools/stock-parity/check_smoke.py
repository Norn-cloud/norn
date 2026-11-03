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


def check_fifo(dump):
    assert len(dump['cases']) == 2
    cases = {c['case']: c for c in dump['cases']}
    assert set(cases) == {'receipt-delivery', 'cross-layer-fifo'}
    check(dict(dump, cases=[cases['receipt-delivery']]))
    for case in cases.values():
        assert case['initial'] == dict(step=-1, attempts=[], stock=[], gl=[], documents=[])
    case = cases['cross-layer-fifo']
    assert case['status'] == 'complete' and len(case['checkpoints']) == 3
    fields = ('requestId', 'line', 'warehouse', 'quantityDelta', 'valueDelta',
              'quantity', 'value', 'incomingRate', 'valuationRate', 'cancelled', 'fifo')
    stock = [
        ('receive', 'receive:1', 'a', '2', '20', '2', '20', '10', '10', False, [['2', '10']]),
        ('receive-two', 'receive-two:1', 'a', '3', '60', '5', '80', '20', '16', False, [['2', '10'], ['3', '20']]),
        ('deliver', 'deliver:1', 'a', '-4', '-60', '1', '20', '15', '20', False, [['1', '20']]),
    ]
    gl = [
        ('receive', 'stock-a', 'USD', '20', '0', False), ('receive', 'srbnb', 'USD', '0', '20', False),
        ('receive-two', 'stock-a', 'USD', '60', '0', False), ('receive-two', 'srbnb', 'USD', '0', '60', False),
        ('deliver', 'cogs', 'USD', '60', '0', False), ('deliver', 'stock-a', 'USD', '0', '60', False),
    ]
    for index, cp in enumerate(case['checkpoints']):
        assert cp['step'] == index and cp['attempts'][0]['outcome'] == 'accepted'
        assert [(r['requestId'], r['status']) for r in cp['documents']] == [(r[0], 1) for r in stock[:index+1]]
        assert [tuple(r[k] for k in fields) for r in cp['stock']] == stock[:index+1]
        assert len(cp['gl']) == 2 * (index + 1)
        assert {tuple(r[k] for k in ('requestId', 'account', 'currency', 'debit', 'credit', 'cancelled'))
                for r in cp['gl']} == set(gl[:2 * (index + 1)])
        assert sum(Decimal(r['debit']) - Decimal(r['credit']) for r in cp['gl']) == 0



if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('dump', type=Path)
    parser.add_argument('--fifo', action='store_true', help='Require the frozen seven-checkpoint corpus')
    args = parser.parse_args()
    (check_fifo if args.fifo else check)(json.loads(args.dump.read_text()))
    print('Dump checkpoint assertions passed; no Norn parity claimed')
