"""Exact checkpoint parity; only evidence-bound reviewed dispositions close a diff."""
import argparse
from collections import defaultdict
from decimal import Decimal
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import re

DECISIONS = ('original-cost-returns', 'no-negative-stock', 'backdating-refused',
             'delivery-stock-cogs-only')
PUBLIC = re.compile(r'https://github.com/Norn-cloud/norn/(?:issues|pull)/[0-9]+(?:#[a-z0-9-]+)?\Z')


def exact(value, scale):
    if not isinstance(value, str) or not re.fullmatch(r'-?\d{1,18}(?:\.\d{1,12})?', value):
        raise ValueError('Expected exact decimal text')
    number = Decimal(value)
    if number != number.quantize(Decimal(1).scaleb(-scale)):
        raise ValueError('Amount exceeds comparison scale')
    return int(number.scaleb(scale))


def indexed(rows, key):
    result = {}
    for row in rows:
        identity = row[key]
        if identity in result:
            raise ValueError('Duplicate checkpoint/case identity')
        result[identity] = row
    return result


def view(cp):
    stock = sorted((r['requestId'], r['line'], r['warehouse'], bool(r['cancelled']),
                    *(exact(r[k], 6) for k in ('quantityDelta', 'valueDelta', 'quantity', 'value')))
                   for r in cp['stock'])
    books = defaultdict(lambda: [0, 0])
    for r in cp['gl']:
        key = (r['requestId'], r['account'], r['currency'], bool(r['cancelled']))
        books[key][0] += exact(r['debit'], 2)
        books[key][1] += exact(r['credit'], 2)
    return dict(stock=stock, gl=sorted((*k, *v) for k, v in books.items()),
                documents=sorted((r['requestId'], r['status']) for r in cp['documents']),
                outcomes=sorted((r['attempt'], r['requestId'], r['outcome'], r['reason'])
                                for r in cp['attempts']))


def refusal_unchanged(before, after):
    if not after['attempts'] or not all(a['outcome'] == 'refused' for a in after['attempts']):
        return True
    return (view(before)['stock'] == view(after)['stock'] and
            view(before)['gl'] == view(after)['gl'] and all(
                {r['id'] for r in before[k]} == {r['id'] for r in after[k]}
                for k in ('stock', 'gl')))


def fingerprint(case, method, step, field, left, right):
    data = json.dumps([case, method, step, field, left, right], sort_keys=True)
    return hashlib.sha256(data.encode()).hexdigest()


def reviewed(d):
    if not re.fullmatch(r'[A-Za-z0-9_-]+', d.get('reviewer', '')):
        return False
    if not PUBLIC.fullmatch(d.get('review', '')):
        return False
    bucket = d.get('bucket')
    if bucket == 'intentional-difference':
        return d.get('decision') in DECISIONS and bool(PUBLIC.fullmatch(d.get('decisionRecord', '')))
    if bucket in ('norn-bug', 'erpnext-bug'):
        fields = ['reproduction', 'receipt', 'issue']
        if bucket == 'erpnext-bug':
            if not re.fullmatch(r'[A-Z][A-Z0-9-]+', d.get('invariant', '')):
                return False
        return all(PUBLIC.fullmatch(d.get(k, '')) for k in fields)
    return False


def compare(norn, erpnext, dispositions):
    for dump, engine in ((norn, 'norn'), (erpnext, 'erpnext')):
        if dump['format'] != 'norn.stock-dump/1' or dump['engine'] != engine:
            raise ValueError('Wrong dump format/engine')
    decisions = indexed(dispositions, 'fingerprint')
    if not all(reviewed(d) for d in dispositions):
        raise ValueError('Disposition requires one reviewed bucket and public evidence')
    sides = [indexed(d['cases'], 'case') for d in (norn, erpnext)]
    rows, used = [], set()
    counts = dict(matched=0, intentionallyDifferent=0, unsupported=0, untriaged=0,
                  openNornBugs=0, openErpnextBugs=0)
    aliases = {}
    def alias(kind, value):
        key = (kind, value)
        if key not in aliases:
            aliases[key] = f'{kind}-{1 + sum(k[0] == kind for k in aliases)}'
        return aliases[key]
    def legs(cp):
        return [dict(id=alias('gl', r['id']), request=alias('request', r['requestId']), account=alias('account', r['account']),
                     currency=alias('currency', r['currency']), debit=exact(r['debit'], 2),
                     credit=exact(r['credit'], 2), cancelled=bool(r['cancelled'])) for r in cp['gl']]
    for name in sorted(set(sides[0]) | set(sides[1])):
        cases = [s.get(name) for s in sides]
        row = dict(case=alias('case', name), checkpoints=[], status='matched',
                   method=next((c['method'] for c in cases if c and c['method'] in ('fifo', 'moving-average')), 'unknown'))
        rows.append(row)
        if any(c is None or 'initial' not in c for c in cases):
            row['status'] = 'unsupported'
            counts['unsupported'] += 1
            continue
        if any(c['status'] != 'complete' or not c['checkpoints'] for c in cases):
            row['status'] = 'unsupported'
            counts['unsupported'] += 1
        if cases[0]['method'] not in ('fifo', 'moving-average') or cases[0]['method'] != cases[1]['method']:
            raise ValueError('Case method mismatch')
        checkpoints = [indexed([c['initial'], *c['checkpoints']], 'step') for c in cases]
        if any(sorted(c) != list(range(-1, len(c)-1)) for c in checkpoints):
            raise ValueError('Checkpoint sequence must start at -1 without gaps')
        for step in sorted(set(checkpoints[0]) | set(checkpoints[1])):
            pair = [c.get(step) for c in checkpoints]
            entry = dict(step=step, differences=[])
            row['checkpoints'].append(entry)
            if any(c is None or c.get('status') == 'unsupported' for c in pair):
                entry['status'] = row['status'] = 'unsupported'
                counts['unsupported'] += 1
                continue
            entry['rawGl'] = dict(norn=legs(pair[0]), erpnext=legs(pair[1]))
            entry['balances'] = {}
            for engine, cp in zip(('norn', 'erpnext'), pair):
                balances = defaultdict(lambda: [0, 0])
                for r in cp['stock']:
                    if not r['cancelled']:
                        amounts = balances[alias('warehouse', r['warehouse'])]
                        amounts[0] += exact(r['quantityDelta'], 6)
                        amounts[1] += exact(r['valueDelta'], 6)
                entry['balances'][engine] = dict(balances)
            views = [view(c) for c in pair]
            if step >= 0:
                for i, engine in enumerate(('norn', 'erpnext')):
                    views[i]['refusalPreserved'] = refusal_unchanged(checkpoints[i][step-1], pair[i])
            def sanitized(value):
                if isinstance(value, str):
                    return alias('symbol', value)
                if isinstance(value, (list, tuple)):
                    return [sanitized(v) for v in value]
                return value
            entry['observations'] = [{k: sanitized(v) for k, v in view.items()} for view in views]
            for category in sorted(set(views[0]) | set(views[1])):
                a, b = views[0].get(category), views[1].get(category)
                def groups(rows):
                    result = defaultdict(list)
                    width = {'stock': 4, 'gl': 4, 'documents': 1, 'outcomes': 2}[category]
                    for row in rows:
                        key = hashlib.sha256(json.dumps(row[:width]).encode()).hexdigest()
                        result[key].append(row)
                    return result
                left, right = (groups(a), groups(b)) if isinstance(a, list) else ({'': a}, {'': b})
                for key in sorted(set(left) | set(right)):
                    field, a, b = category + (':' + key if key else ''), left.get(key), right.get(key)
                    if a == b and (category != 'refusalPreserved' or a is True):
                        continue
                    digest = fingerprint(name, cases[0]['method'], step, field, a, b)
                    d = decisions.get(digest)
                    bucket = d['bucket'] if d else 'untriaged'
                    entry['differences'].append(dict(fingerprint=digest, field=field, bucket=bucket,
                        norn=sanitized(a), erpnext=sanitized(b)))
                    if d:
                        used.add(digest)
                        entry['differences'][-1]['evidence'] = {k: d[k] for k in ('bucket', 'reviewer', 'review', 'decision', 'decisionRecord',
                            'issue', 'reproduction', 'receipt', 'invariant') if k in d}
                    counts[{'norn-bug': 'openNornBugs', 'erpnext-bug': 'openErpnextBugs',
                            'intentional-difference': 'intentionallyDifferent', 'untriaged': 'untriaged'}[bucket]] += 1
            entry['status'] = ('matched' if not entry['differences'] else
                               'intentionally different' if all(d['bucket'] == 'intentional-difference'
                               for d in entry['differences']) else 'divergent')
            if entry['status'] == 'matched':
                counts['matched'] += 1
            elif row['status'] != 'unsupported':
                row['status'] = ('divergent' if entry['status'] == 'divergent' or row['status'] == 'divergent'
                                 else 'intentionally different')
    if used != set(decisions):
        raise ValueError('Stale disposition does not match an observed divergence')
    for bucket, key in (('norn-bug', 'openNornBugs'), ('erpnext-bug', 'openErpnextBugs')):
        counts[key] = len({d['issue'] for d in dispositions if d['bucket'] == bucket})
    passed = bool(rows) and not any(counts[k] for k in ('unsupported', 'untriaged', 'openNornBugs', 'openErpnextBugs'))
    return dict(format='norn.stock-parity/1', passed=passed, counts=counts,
                decisions=list(DECISIONS), scales=dict(stock=6, gl=2), cases=rows)


def amount(value, scale):
    return format(Decimal(value).scaleb(-scale), 'f')


def rate(quantity, value):
    if not quantity:
        return '—'
    ratio = Fraction(value, quantity)
    denominator = ratio.denominator
    for factor in (2, 5):
        while denominator % factor == 0:
            denominator //= factor
    return (format(Decimal(ratio.numerator) / Decimal(ratio.denominator), 'f')
            if denominator == 1 else str(ratio))


def markdown(report):
    lines = ['# Checkpoint parity (bounded corpus)', '',
             'Sampled checkpoint evidence; this report does not prove equivalence.', '',
             '| Case | Method | Checkpoint | Result |', '|---|---|---|---|']
    for case in report['cases']:
        for cp in case['checkpoints']:
            lines.append(f"| {case['case']} | {case['method']} | {cp['step']} | {cp['status']} |")
        if not case['checkpoints']:
            lines.append(f"| {case['case']} | {case['method']} | — | {case['status']} |")
    for case in report['cases']:
        lines.extend(['', f"## {case['case']} · {case['method']}", '', f"Case coverage: {case['status']}."])
        for cp in case['checkpoints']:
            lines.extend(['', f"### Checkpoint {cp['step']} · {cp['status']}", ''])
            if cp['status'] == 'unsupported':
                lines.append('Bucket: unsupported. A checkpoint is missing or its snapshot is unavailable; no empty state is inferred.')
                continue
            if not cp['differences']:
                lines.append('Bucket: matched. Exact stock, gross GL, documents and outcomes agree; refusal preservation holds where applicable.')
            for diff in cp['differences']:
                evidence = diff.get('evidence', {})
                reason = evidence.get('decision', evidence.get('invariant', ''))
                lines.append(f"- Bucket: {diff['bucket']}. Field: `{diff['field']}`. "
                             + (f"Reason: {reason}. " if reason else
                                'Reason: reviewed bug evidence. ' if evidence else 'Reason: no reviewed disposition. ')
                             + f"Fingerprint: `{diff['fingerprint']}`.")
                for key in ('review', 'decisionRecord', 'issue', 'reproduction', 'receipt'):
                    if key in evidence:
                        lines.append(f"  [{key}]({evidence[key]})")
            lines.extend(['', 'Warehouse balances (active stock deltas; exact amounts). Rate = value / quantity; fractions remain exact.', '',
                          '| Warehouse | Norn quantity | ERPNext quantity | Norn value | ERPNext value | Norn rate | ERPNext rate |',
                          '|---|---:|---:|---:|---:|---:|---:|'])
            balances = cp['balances']
            warehouses = sorted(set(balances['norn']) | set(balances['erpnext']))
            for warehouse in warehouses:
                pairs = [balances[e].get(warehouse, [0, 0]) for e in ('norn', 'erpnext')]
                q, v = zip(*pairs)
                lines.append(f"| {warehouse} | {amount(q[0], 6)} | {amount(q[1], 6)} | "
                             f"{amount(v[0], 6)} | {amount(v[1], 6)} | {rate(*pairs[0])} | {rate(*pairs[1])} |")
            if not warehouses:
                lines.append('| No active stock | 0 | 0 | 0 | 0 | — | — |')
            lines.extend(['', 'Raw GL legs by account (gross amounts; cancelled legs remain visible).', '',
                          '| Account | Request | Currency | Cancelled | Leg | Norn debit | ERPNext debit | Norn credit | ERPNext credit |',
                          '|---|---|---|---|---:|---:|---:|---:|---:|'])
            groups = {}
            for engine in ('norn', 'erpnext'):
                groups[engine] = defaultdict(list)
                for leg in cp['rawGl'][engine]:
                    key = tuple(leg[k] for k in ('account', 'request', 'currency', 'cancelled'))
                    groups[engine][key].append((leg['debit'], leg['credit']))
            for key in sorted(set(groups['norn']) | set(groups['erpnext'])):
                sides = [sorted(groups[e][key]) for e in ('norn', 'erpnext')]
                for i in range(max(map(len, sides))):
                    legs = [side[i] if i < len(side) else None for side in sides]
                    cells = [amount(leg[j], 2) if leg else '—' for j in (0, 1) for leg in legs]
                    lines.append('| ' + ' | '.join([*map(str, key), str(i + 1), *cells]) + ' |')
            if not any(groups.values()):
                lines.append('| No GL legs | — | — | — | — | — | — | — | — |')
    lines.extend(['', '## Summary', '', f"Gate: {'pass' if report['passed'] else 'fail'}.", '',
                  'Counts retain gate units: matched checkpoints, intentional differing fields, unsupported cases/checkpoints, untriaged fields and unique open bug issues.', '',
                  '| Count | Value |', '|---|---:|',
                  *[f'| {k} | {v} |' for k, v in report['counts'].items()]])
    return '\n'.join(lines) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    for name in ('norn', 'erpnext', 'dispositions', 'output'):
        parser.add_argument('--' + name, required=True, type=Path)
    args = parser.parse_args()
    report = compare(*(json.loads(p.read_text()) for p in (args.norn, args.erpnext, args.dispositions)))
    args.output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    args.output.with_suffix('.md').write_text(markdown(report))
    raise SystemExit(0 if report['passed'] else 1)
