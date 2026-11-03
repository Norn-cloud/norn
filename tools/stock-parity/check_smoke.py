"""Exact frozen oracle checkpoint assertions, never a new live parity claim."""
import json
from decimal import Decimal
from pathlib import Path


def normalized(value):
    identities = {}
    def visit(value, kind=None):
        if isinstance(value, list):
            return [visit(row, kind) for row in value]
        if not isinstance(value, dict):
            return value
        result = {}
        for key, item in value.items():
            if (kind in ('stock', 'gl') and key == 'id') or (kind == 'attempts' and key == 'nativeId'):
                assert isinstance(item, str) and item, 'Native identity must be nonempty text'
                identity = (kind, item)
                if identity not in identities:
                    identities[identity] = f'{kind}-{1 + sum(k[0] == kind for k in identities)}'
                result[key] = identities[identity]
            else:
                result[key] = visit(item, key)
        return result
    return json.dumps(visit(value), sort_keys=True)


def check_cases(dump, names):
    from rest import encode
    boundary = {'qty': Decimal('999999999999.999999'), 'items': [Decimal('0.000001')]}
    encoded = encode(boundary)
    assert json.loads(encoded, parse_float=Decimal) == boundary
    assert '"999999999999.999999"' not in encoded
    assert dump['format'] == 'norn.stock-dump/1' and dump['engine'] == 'erpnext'
    actual = {case['case']: case for case in dump['cases']}
    assert len(dump['cases']) == len(names) and set(actual) == set(names)
    expected = {case['case']: case for case in json.loads(
        Path(__file__).with_name('fifo-oracle.fixture.json').read_text())['cases']}
    for name in names:
        assert normalized(actual[name]) == normalized(expected[name]), name


def check(dump):
    check_cases(dump, ('receipt-delivery',))


def check_fifo(dump):
    check_cases(dump, ('receipt-delivery', 'cross-layer-fifo'))


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('dump', type=Path)
    parser.add_argument('--fifo', action='store_true', help='Require the frozen seven-checkpoint corpus')
    args = parser.parse_args()
    (check_fifo if args.fifo else check)(json.loads(args.dump.read_text()))
    print('Dump checkpoint assertions passed; no Norn parity claimed')
