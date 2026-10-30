"""Extract literal, flat XState machines to facts and typed Norn authoring.

No callbacks execute. Unsupported syntax rejects the whole machine. Named
guards stay unresolved facts until explicitly mapped to Norn predicates.
"""
import argparse
from dataclasses import asdict, dataclass
import json
from pathlib import Path
import re
from recognizer import collect, evidence
from syntax import parse, walk, string, spelling


class Unsupported(ValueError):
    pass


def literal(node):
    if node is None:
        raise Unsupported('missing literal')
    if node.type == 'string':
        value = string(node)
        if value is None:
            raise Unsupported('escaped non-JSON string')
        return value
    if node.type == 'object':
        result = {}
        for pair in node.named_children:
            if pair.type == 'comment':
                continue
            if pair.type != 'pair':
                raise Unsupported('computed config, spread or method')
            key_node = pair.child_by_field_name('key')
            key = string(key_node) if key_node.type == 'string' else spelling(key_node)
            if key_node.type not in {'string', 'property_identifier'} or key in result:
                raise Unsupported('computed or duplicate key')
            result[key] = literal(pair.child_by_field_name('value'))
        return result
    raise Unsupported('non-literal config: ' + node.type)


@dataclass(frozen=True)
class Transition:
    event: str
    source: str
    target: str
    guard: str | None
    evidence: dict


@dataclass(frozen=True)
class Lifecycle:
    id: str
    initial: str
    states: list[str]
    terminal: list[str]
    transitions: list[Transition]
    evidence: dict


def machine(config, source):
    if set(config) - {'id', 'initial', 'states'}:
        raise Unsupported('machine behavior beyond flat states')
    states = config.get('states')
    if not isinstance(states, dict) or not states or config.get('initial') not in states:
        raise Unsupported('missing states or invalid initial')
    identity = config.get('id')
    if not isinstance(identity, str) or not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*', identity):
        raise Unsupported('machine needs an authoring-safe id')
    terminal, transitions = [], []
    for state, spec in states.items():
        if not isinstance(spec, dict) or set(spec) - {'type', 'on'}:
            raise Unsupported('nested state or executable state behavior')
        if spec.get('type') not in {None, 'final'}:
            raise Unsupported('non-atomic state')
        events = spec.get('on', {})
        if not isinstance(events, dict):
            raise Unsupported('dynamic events')
        if spec.get('type') == 'final':
            if events:
                raise Unsupported('final state has transitions')
            terminal.append(state)
        for event, transition in events.items():
            if event == '*':
                raise Unsupported('wildcard event')
            transition = {'target': transition} if isinstance(transition, str) else transition
            if not isinstance(transition, dict) or set(transition) - {'target', 'guard', 'cond'}:
                raise Unsupported('transition behavior or ordered alternatives')
            guard = transition.get('guard', transition.get('cond'))
            if ('guard' in transition and 'cond' in transition) or (guard is not None and not isinstance(guard, str)):
                raise Unsupported('non-symbolic guard')
            target = transition.get('target')
            if not isinstance(target, str) or target not in states or target == state:
                raise Unsupported('nonlocal, missing or self target')
            transitions.append(Transition(event, state, target, guard, source))
    return Lifecycle(identity, config['initial'], list(states), terminal, transitions, source)


def extract(sources, revision='working-tree'):
    facts, unknown = [], []
    for path, data in sorted(sources.items()):
        if Path(path).suffix not in {'.ts', '.tsx', '.js', '.jsx', '.mjs'}:
            continue
        tree = parse(data, path.endswith(('.tsx', '.jsx')))
        bindings = set()
        for node in tree.named_children:
            if node.type == 'import_statement' and string(node.child_by_field_name('source')) == 'xstate':
                for spec in walk(node):
                    if spec.type == 'import_specifier' and spelling(spec.child_by_field_name('name')) == 'createMachine':
                        bindings.add(spelling(spec.child_by_field_name('alias') or spec.child_by_field_name('name')))
        if not bindings:
            continue
        # A compiler semantic pass is not wired yet. Refuse ambiguous bindings.
        ambiguous = any(n.type == 'identifier' and spelling(n) in bindings and n.parent.type not in {'import_specifier', 'call_expression'} for n in walk(tree))
        for node in walk(tree):
            if node.type != 'call_expression' or spelling(node.child_by_field_name('function')) not in bindings:
                continue
            source = asdict(evidence(path, data, revision, node.start_point.row + 1))
            try:
                if tree.has_error or ambiguous:
                    raise Unsupported('syntax error or ambiguous factory binding')
                args = node.child_by_field_name('arguments').named_children
                if len(args) != 1:
                    raise Unsupported('implementations argument needs semantic analysis')
                facts.append(asdict(machine(literal(args[0]), source)))
            except Unsupported as error:
                unknown.append({'reason': str(error), 'evidence': source})
    return {'schema_version': 'norn-code-lifecycle/1', 'extracted': facts, 'unknown': unknown,
            'coverage': {'extracted': len(facts), 'unknown': len(unknown)},
            'evidence_level': 'analyzed', 'parity': 'unverified'}


def authoring(fact, predicates):
    """Emit source for the existing compiler, never raw IR or unchecked guards."""
    quote = json.dumps
    rows = []
    for i, transition in enumerate(fact['transitions']):
        guard = transition['guard']
        if guard and guard not in predicates:
            raise Unsupported('unmapped guard: ' + guard)
        guards = ', guards: [Norn.unsafeZen<boolean>(' + quote(predicates[guard]) + ')]' if guard else ''
        rows.append('{ id: ' + quote('event_' + str(i)) + ', description: ' + quote(transition['event']) + ', from: [' + quote(transition['source']) + '], to: ' + quote(transition['target']) + guards + ' }')
    return '\n'.join([
        'import { Schema } from "effect"', 'import { Norn } from "@norn/authoring"',
        'export const Machine = Norn.type({',
        '  name: ' + quote('code.' + fact['id']) + ', title: ' + quote(fact['id']) + ', version: "0.1.0",',
        '  properties: Schema.Struct({ status: Schema.Literals(' + quote(fact['states']) + ') }),',
        '  defaults: { status: ' + quote(fact['initial']) + ' },',
        '  lifecycles: [{ property: "status", initial: [' + quote(fact['initial']) + '], terminal: ' + quote(fact['terminal']) + ', transitions: [' + ', '.join(rows) + '] }],',
        '})', ''])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--revision', default='working-tree')
    parser.add_argument('--predicates', type=Path, help='JSON guard name to reviewed ZEN predicate')
    parser.add_argument('--authoring', action='store_true', help='emit one machine as typed TS source')
    args = parser.parse_args()
    report = extract(collect(args.root), args.revision)
    if args.authoring:
        if report['unknown'] or len(report['extracted']) != 1:
            parser.error('authoring requires exactly one fully extracted machine')
        predicates = json.loads(args.predicates.read_text()) if args.predicates else {}
        print(authoring(report['extracted'][0], predicates))
    else:
        print(json.dumps(report, sort_keys=True, indent=2))
