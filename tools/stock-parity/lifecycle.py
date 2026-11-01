"""Synthetic CP2 witnesses, ordered moving-average, transfer, return, cancellation."""
from copy import deepcopy
import json
from pathlib import Path
import sys


def traces():
    base = json.loads(Path(__file__).with_name('smoke.json').read_text())[0]
    cases = []
    for method in ('moving-average', 'fifo'):
        case = deepcopy(base)
        case.update(id='lifecycle-' + method, method=method)
        steps = case['steps']
        steps[1]['command']['effectiveAt'] = '2026-01-02T12:02:00Z'
        steps.insert(1, dict(kind='serial', command=dict(requestId='receive-second',
            effectiveAt='2026-01-02T12:01:00Z', operation=dict(kind='receipt', warehouse='a',
            quantity='10', rate='4')), expected=dict(kind='accepted')))
        operations = [('transfer', dict(kind='transfer', **{'from': 'a', 'to': 'b'}, quantity='2')),
                      ('return', dict(kind='return', delivery='deliver', quantity='1')),
                      ('cancel-return', dict(kind='cancel', target='return')),
                      ('cancel-deliver', dict(kind='cancel', target='deliver')),
                      ('cancel-transfer', dict(kind='cancel', target='transfer')),
                      ('cancel-second', dict(kind='cancel', target='receive-second')),
                      ('cancel-receive', dict(kind='cancel', target='receive'))]
        for minute, (key, op) in enumerate(operations, 3):
            steps.append(dict(kind='serial', command=dict(requestId=key,
                effectiveAt=f'2026-01-02T12:{minute:02}:00Z', operation=op), expected=dict(kind='accepted')))
        cases.append(case)
    return cases


def check(dump):
    from decimal import Decimal as D
    assert len(dump['cases']) == 2
    for case, method in zip(dump['cases'], ('moving-average', 'fifo')):
        assert case['method'] == method and case['status'] == 'complete', case['status']
        assert len(case['checkpoints']) == 10
        delivered, returned = ('51', '54') if method == 'moving-average' else ('54', '56')
        expected = [('10','20'), ('20','60'), ('17',delivered), ('17',delivered),
                    ('18',returned), ('17',delivered), ('20','60'), ('20','60'), ('10','20'), ('0','0')]
        for cp, (qty, value) in zip(case['checkpoints'], expected):
            assert cp['attempts'][0]['outcome'] == 'accepted', cp['attempts']
            active = [r for r in cp['stock'] if not r['cancelled']]
            assert sum(D(r['quantityDelta']) for r in active) == D(qty), cp['step']
            assert sum(D(r['valueDelta']) for r in active) == D(value), cp['step']
            assert sum(D(r['debit']) - D(r['credit']) for r in cp['gl']) == 0
        transfer = case['checkpoints'][3]
        moved = [r for r in transfer['stock'] if r['requestId'] == 'transfer']
        cost = '6' if method == 'moving-average' else '4'
        assert {(r['warehouse'], r['quantityDelta'], r['valueDelta']) for r in moved} == {
            ('a', '-2', '-' + cost), ('b', '2', cost)}
        legs = [r for r in transfer['gl'] if r['requestId'] == 'transfer']
        assert {(r['account'], r['debit'], r['credit']) for r in legs} == {
            ('stock-a', '0', cost), ('stock-b', cost, '0')}
        returned = case['checkpoints'][4]
        unit_cost = '3' if method == 'moving-average' else '2'
        assert {(r['account'], r['debit'], r['credit']) for r in returned['gl']
                if r['requestId'] == 'return'} == {('stock-a', unit_cost, '0'), ('cogs', '0', unit_cost)}
        final_gl = case['checkpoints'][-1]['gl']
        for account in {r['account'] for r in final_gl}:
            assert sum(D(r['debit']) - D(r['credit']) for r in final_gl if r['account'] == account) == 0
        assert all(d['status'] == 2 for d in case['checkpoints'][-1]['documents'])


if __name__ == '__main__':
    if len(sys.argv) == 1:
        print(json.dumps(traces()))
    else:
        check(json.loads(Path(sys.argv[1]).read_text()))
        print('CP2 lifecycle: both methods, 20 checkpoints, transfer/return/reverse accepted')
