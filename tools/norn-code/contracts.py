"""Clean-room boundary over norn.code-ontology/1 facts (#3470).

Analyst-side mapping is private input, not a shipped translation of source.
Only explicitly supplied canon terms leave this boundary. Effects are sets;
no source AST, handler identity, condition text or call graph is exported.
Facts do NOT establish dependency, ordering, atomicity or permission coverage.
Those dimensions remain unknown until stronger analysis is supplied.
"""
import argparse
from dataclasses import asdict, dataclass
import json
from pathlib import Path
import re


@dataclass(frozen=True)
class Contract:
    entry_point: str
    effects: dict[str, list[str]]
    dependencies: list
    guards: list[str]
    ordering: list
    atomicity: list
    invariants: list
    permission_coverage: list
    evidence_level: str
    validation: str
    coverage: dict[str, int]
    unknown: list[str]
    source_hashes: list[str]


def _project_group(report, mapping, entries):
    if report.get('format') != 'norn.code-ontology/1':
        raise ValueError('expected norn.code-ontology/1')
    vocabulary = set(mapping['vocabulary'])
    if not vocabulary or any(not isinstance(t, str) or not re.fullmatch(r'[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+', t) for t in vocabulary):
        raise ValueError('canon vocabulary must contain qualified terms')
    def canon(value):
        if value not in vocabulary:
            raise ValueError('mapping target is outside admitted canon vocabulary')
        return value
    if len(set(entries.values())) != len(entries):
        raise ValueError('entry point mappings must be one to one')
    result = []
    for handler, target in sorted(entries.items(), key=lambda item: item[1]):
        canon(target)
        effects = {k: set() for k in ('reads', 'writes', 'calls', 'emits')}
        guards, hashes = set(), set()
        facts = [fact for fact in report['facts'] if fact['handler'] == handler]
        mapped = 0
        for fact in facts:
            digest = fact.get('source', {}).get('digest') or fact.get('source', {}).get('sha256')
            if digest:
                if not re.fullmatch('[a-f0-9]{64}', digest):
                    raise ValueError('invalid source digest')
                hashes.add(digest)
            if fact.get('confidence') != 'resolved' or fact.get('diagnostic'):
                continue
            relation = fact['relation']
            if relation in {'reads', 'writes'}:
                key = json.dumps([fact.get('entity'), fact.get('field')], separators=(',', ':'))
                value = mapping.get('properties', {}).get(key)
                if value:
                    effects[relation].add(canon(value))
                    mapped += 1
            elif relation == 'calls':
                value = mapping.get('calls', {}).get(fact.get('target'))
                if value:
                    effects['calls'].add(canon(value))
                    mapped += 1
            elif relation == 'throws_if':
                # Exact private mapping, never heuristic rewording of a guard.
                value = mapping.get('guards', {}).get(fact.get('condition'))
                if value:
                    guards.add(canon(value))
                    mapped += 1
        unknown = ['dependencies_not_established', 'ordering_not_established', 'atomicity_not_established',
                   'invariants_not_established', 'permission_coverage_not_established', 'events_not_established']
        if mapped != len(facts):
            unknown.append('unmapped_source_facts')
        if not facts:
            unknown.append('entry_point_has_no_source_facts')
        if not hashes:
            unknown.append('source_digest_not_supplied')
        result.append(asdict(Contract(target, {k: sorted(v) for k, v in effects.items()}, [], sorted(guards), [], [], [], [],
                                      'analyzed', 'hypothesis_not_parity', {'source_facts': len(facts), 'mapped': mapped,
                                      'unknown': len(facts) - mapped}, unknown, sorted(hashes))))
    return {'schema_version': 'norn-code-contract/1', 'contracts': result,
            'mapping_status': mapping.get('status', 'analyst_supplied'),
            'coverage': {'entry_points': len(result), 'with_mapped_facts': sum(c['coverage']['mapped'] > 0 for c in result)},
            'gap': 'typed JSON only; code-fact ontology ingestion/NornQL not implemented'}


def project(report, mapping):
    result = _project_group(report, mapping, mapping.get('entry_points', {}))
    entries = mapping.get('validation_entry_points', {})
    validators = _project_group(report, mapping, entries)['contracts']
    by_subject = {subject: handler for handler, subject in entries.items()}
    known = {h['id'] for h in report.get('handlers', [])}
    facts = report['facts']
    for contract in validators:
        subject = contract.pop('entry_point')
        root = by_subject[subject]
        contract.update(subject=subject, kind='validation')
        seen, pending = {root}, [root]
        while pending:
            handler = pending.pop()
            for fact in facts:
                target = fact.get('target')
                if fact['handler'] == handler and fact['relation'] == 'calls' and fact.get('confidence') == 'resolved' and not fact.get('diagnostic') and target in known and target not in seen:
                    seen.add(target)
                    pending.append(target)
        refusals = [f for f in facts if f['handler'] in seen - {root} and f['relation'] == 'throws_if']
        candidates, mapped = set(), 0
        for fact in refusals:
            predicate = mapping.get('guards', {}).get(fact.get('condition'))
            if predicate and fact.get('confidence') == 'resolved' and not fact.get('diagnostic'):
                if predicate not in mapping['vocabulary']:
                    raise ValueError('guard candidate is outside supplied canon vocabulary')
                candidates.add(predicate)
                mapped += 1
        # A static call edge establishes a candidate, not applicability on all
        # paths. Never lift a helper's refusal into the direct guard set.
        contract['guard_candidates'] = [dict(predicate=p, evidence_level='analyzed', applicability='unknown') for p in sorted(candidates)]
        contract['guard_candidate_coverage'] = dict(source_refusals=len(refusals), mapped=mapped, unknown=len(refusals) - mapped)
        contract['unknown'].append('helper_guard_applicability_not_established')
    result['validations'] = validators
    result['coverage']['validation_entry_points'] = len(validators)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('facts', type=Path)
    parser.add_argument('mapping', type=Path, help='private analyst mapping to admitted canon terms')
    args = parser.parse_args()
    print(json.dumps(project(json.loads(args.facts.read_text()), json.loads(args.mapping.read_text())), sort_keys=True, indent=2))
