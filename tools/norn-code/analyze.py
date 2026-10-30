"""Combined framework recognition / extraction coverage; typed JSON only."""
import argparse
import json
from pathlib import Path
from recognizer import analyze, collect
from xstate import extract


def report(sources, revision='working-tree'):
    result = analyze(sources, revision)
    lifecycles = extract(sources, revision)
    result['extracted'] = lifecycles['extracted']
    result['unknown'].extend(lifecycles['unknown'])
    for match in result['recognized']:
        if match['framework'] != 'xstate':
            result['unknown'].append({'reason': 'framework_extractor_not_implemented',
                                      'framework': match['framework'], 'evidence': match['evidence']})
    result['coverage']['extracted'] = len(lifecycles['extracted'])
    result['coverage']['extraction_gaps'] = len(lifecycles['unknown'])
    result['coverage']['denominator'] = 'collected Python/JS/TS sources and supported manifests/configs'
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--revision', default='working-tree')
    args = parser.parse_args()
    print(json.dumps(report(collect(args.root), args.revision), indent=2, sort_keys=True))
