#!/usr/bin/env python3
"""Merge origin/main; resolve only mechanically reproducible conflicts.

Metadata is immutable, manifest entries retain their identity, and unresolved
source conflicts never receive a fresh provenance hash. Review and commit the
result explicitly; failures leave the merge in progress for inspection/abort.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

PROVENANCE = '.github/ci/provenance/artifacts/'
COVERAGE = 'verification/coverage-v1/manifest/subsystems/'


def git(*args, check=True):
    return subprocess.run(['git', *args], check=check, capture_output=True).stdout


def conflicts():
    return set(git('diff', '--name-only', '--diff-filter=U', '-z').decode().split('\0')) - {''}


def stages(path):
    rows = git('ls-files', '-u', '-z', '--', path).decode().split('\0')
    if any(row and row.split()[0] != '100644' for row in rows):
        raise ValueError('not a regular file')
    return [json.loads(git('show', f':{stage}:{path}')) for stage in (1, 2, 3)]


def manifest(base, ours, theirs):
    """Union lists by stable identity; refuse divergent edits of one entry."""
    if isinstance(ours, list) and isinstance(theirs, list) and isinstance(base, list):
        def indexed(values):
            result = {}
            for value in values:
                if isinstance(value, dict):
                    key = next((k for k in ('id', 'artifact', 'path') if k in value), None)
                    if key is None:
                        raise ValueError('object entry has no stable identity')
                    identity = json.dumps([key, value[key]], sort_keys=True)
                else:
                    identity = json.dumps(value, sort_keys=True)
                if identity in result and result[identity] != value:
                    raise ValueError(f'duplicate identity: {identity}')
                result[identity] = value
            return result
        b, o, t = map(indexed, (base, ours, theirs))
        result = {}
        for key in o.keys() | t.keys():
            if key in o and key in t:
                if o[key] != t[key] and (key not in b or (o[key] != b[key] and t[key] != b[key])):
                    raise ValueError(f'divergent entry: {key}')
                result[key] = t[key] if key in b and o[key] == b[key] else o[key]
            else:
                value = o.get(key, t.get(key))
                if key in b and value != b[key]:
                    raise ValueError(f'delete/edit entry: {key}')
                result[key] = value
        return [result[key] for key in sorted(result)]
    if all(isinstance(value, dict) for value in (base, ours, theirs)):
        if base.keys() != ours.keys() or base.keys() != theirs.keys():
            raise ValueError('manifest schema changed')
        return {key: manifest(base[key], ours[key], theirs[key]) for key in ours}
    if ours == theirs or theirs == base:
        return ours
    if ours == base:
        return theirs
    raise ValueError('divergent scalar')


def provenance(path, base, ours, theirs, unresolved):
    metadata = [{k: v for k, v in value.items() if k != 'sha256'} for value in (base, ours, theirs)]
    if any(value.keys() != metadata[0].keys() for value in metadata):
        raise ValueError('provenance schema changed')
    merged = {}
    for key, original in metadata[0].items():
        left, right = metadata[1][key], metadata[2][key]
        if left != right and left != original and right != original:
            raise ValueError(f'provenance metadata conflict: {key}')
        merged[key] = right if left == original else left
    source = Path(path[len(PROVENANCE):-5])
    if source.as_posix() in unresolved or source.is_symlink() or not source.is_file():
        raise ValueError(f'source unresolved or not regular: {source}')
    return {'sha256': hashlib.sha256(source.read_bytes()).hexdigest(), **merged}


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')
    git('add', '--', path)


def main():
    root = git('rev-parse', '--show-toplevel').decode().strip()
    import os
    os.chdir(root)
    if git('status', '--porcelain'):
        raise ValueError('start with a clean working tree and index')
    merged = subprocess.run(['git', 'merge', '--no-commit', '--no-ff', 'origin/main'])
    pending = conflicts()
    if merged.returncode and not pending:
        return merged.returncode
    failed = set()
    for path in sorted(pending, key=lambda p: (p.startswith(PROVENANCE), p)):
        try:
            if path.startswith(PROVENANCE) and path.endswith('.json'):
                write(path, provenance(path, *stages(path), conflicts()))
            elif path.startswith(COVERAGE) and path.endswith('.json') and '/' not in path[len(COVERAGE):]:
                write(path, manifest(*stages(path)))
            elif path == 'Cargo.lock':
                git('checkout', '--ours', '--', path)
                subprocess.run(['cargo', 'update', '-w', '--offline'], check=True)
                git('add', '--', path)
                record = PROVENANCE + path + '.json'
                if record not in pending:
                    value = json.loads(Path(record).read_text())
                    write(record, provenance(record, value, value, value, conflicts()))
        except (ValueError, OSError, subprocess.CalledProcessError) as error:
            failed.add(path)
            print(f'{path}: {error}', file=sys.stderr)
    remaining = conflicts() | failed
    if remaining:
        print('Unresolved conflicts:\n' + '\n'.join(sorted(remaining)), file=sys.stderr)
        return 1
    print('Mechanical merge ready; inspect the staged diff and commit.')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
