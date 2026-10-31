"""Generate private test-time ERPNext facts with the existing #3470 adapter.

Run in a disposable sandbox. Upstream checkout, graph and fact outputs must
stay outside the Norn repo. Stdout contains hashes and coverage only.
"""
import argparse
from collections import Counter
import hashlib
import importlib
import json
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--port-tool', type=Path, required=True)
    parser.add_argument('--codegraph', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mapping', type=Path, help='private canon mapping (lane G proposed v0 handles)')
    args = parser.parse_args()
    revision = subprocess.check_output(['git', '-C', str(args.root), 'rev-parse', 'HEAD'], text=True).strip()
    if revision != args.revision:
        raise ValueError('ERPNext pin mismatch')
    if subprocess.check_output(['git', '-C', str(args.root), 'status', '--porcelain'], text=True).strip():
        raise ValueError('dirty upstream checkout')
    args.output.mkdir(parents=True, exist_ok=True)
    sys.path.insert(0, str(args.port_tool.resolve()))
    adapter = importlib.import_module('code_ontology')
    graph_module = importlib.import_module('codegraph')
    sources = adapter.collect([args.root])
    graph = graph_module.CodeGraphRust(args.codegraph, args.output / 'scratch').extract(sources)
    facts = adapter.FrappeRules(sources, graph, revision).report()
    (args.output / 'facts.json').write_text(json.dumps(facts, sort_keys=True))
    if args.mapping:
        from contracts import project
        contracts = project(facts, json.loads(args.mapping.read_text()))
        contracts['source_revision'] = revision
        (args.output / 'contracts.json').write_text(json.dumps(contracts, sort_keys=True))
    (args.output / 'graph.json').write_text(json.dumps(graph, sort_keys=True))
    hashes = {path: hashlib.sha256(raw).hexdigest() for path, raw in sources.items()}
    (args.output / 'source-hashes.json').write_text(json.dumps(hashes, sort_keys=True))
    # The graph is examined before assuming it carries data flow or ordering.
    types = Counter(str(edge.get('kind') or edge.get('type') or edge.get('relation') or 'unspecified') for edge in graph['links'])
    selected = [h for h in facts['handlers'] if any(name in h['source']['path'] for name in ('/sales_invoice/', '/payment_entry/')) and h['symbol'].split('.')[-1] in {'on_submit', 'validate'}]
    counts = {}
    for handler in selected:
        rows = [f for f in facts['facts'] if f['handler'] == handler['id']]
        counts[handler['symbol']] = dict(Counter(f['relation'] for f in rows))
    print(json.dumps({'revision': revision, 'source_files': len(sources), 'source_hashes': hashes,
                      'facts': len(facts['facts']), 'graph_nodes': len(graph['nodes']),
                      'graph_edges': len(graph['links']), 'graph_edge_types': dict(types),
                      'entry_point_fact_counts': counts}, sort_keys=True))


if __name__ == '__main__':
    main()
