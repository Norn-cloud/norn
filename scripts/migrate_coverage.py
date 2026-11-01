#!/usr/bin/env python3
"""Migrate a pre-split PR during `git merge origin/main` (run on Boat).

Usage: python3 /path/to/migrate_coverage.py [--root CHECKOUT] [--main REF]
Requires an active merge, HEAD the PR, MERGE_HEAD the post-#132 main.
Only coverage and per-artifact provenance conflicts are resolved/staged.
Metadata conflicts, concurrent scalar edits, list reordering and unknown list
identities are refused. No commit, push, or merge of unrelated files occurs.
Validation failure leaves the migration available for inspection; exit nonzero.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

MONOLITH = 'verification/coverage-v1/coverage.json'
MANIFEST = 'verification/coverage-v1/manifest'
RECORDS = '.github/ci/provenance/artifacts/'
MISSING = object()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def parse(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f'duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def validate_members(value, path):
    if isinstance(value, dict):
        for key, item in value.items():
            validate_members(item, f'{path}/{key}')
    elif isinstance(value, list):
        if all(isinstance(item, str) for item in value):
            require(len(value) == len(set(value)), f'{path}: duplicate list members')
        for item in value:
            validate_members(item, path)


def merge(base, own, current, path):
    for value in (base, own, current):
        validate_members(value, path)
    return _merge(base, own, current, path)


def _merge(base, own, current, path):
    if own == base or own == current:
        return current
    if current == base:
        return own
    if all(isinstance(x, dict) for x in (base, own, current)):
        result = dict(current)
        for key in dict.fromkeys([*base, *own]):
            value = _merge(base.get(key, MISSING), own.get(key, MISSING),
                          current.get(key, MISSING), f'{path}/{key}')
            if value is MISSING:
                result.pop(key, None)
            else:
                result[key] = value
        return result
    if all(isinstance(x, list) for x in (base, own, current)):
        if all(isinstance(v, str) for items in (base, own, current) for v in items):
            for items in (own, current):
                require([x for x in items if x in base] == [x for x in base if x in items],
                        f'{path}: concurrent list reorder')
            return [x for x in current if x not in base or x in own] + [
                x for x in own if x not in base and x not in current]
        if all(isinstance(v, dict) and isinstance(v.get('id'), str)
               for items in (base, own, current) for v in items):
            def keyed(items):
                result = {v['id']: v for v in items}
                require(len(result) == len(items), f'{path}: duplicate ids')
                return result
            merged = _merge(keyed(base), keyed(own), keyed(current), path)
            return list(merged.values())
    raise ValueError(f'{path}: ambiguous concurrent change (resolve manually)')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path.cwd())
    parser.add_argument('--main', default='origin/main')
    args = parser.parse_args()
    root = args.root.resolve()

    def git(*args, check=True):
        result = subprocess.run(['git', '-C', str(root), *args],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
        require(not check or result.returncode == 0, result.stderr.decode().strip())
        return result.stdout.decode()

    def document(ref, path):
        return parse(git('show', f'{ref}:{path}'))

    head = git('rev-parse', 'HEAD').strip()
    incoming = git('rev-parse', 'MERGE_HEAD').strip()
    require(incoming == git('rev-parse', args.main).strip(), 'MERGE_HEAD must equal --main')
    bases = git('merge-base', '--all', head, incoming).splitlines()
    require(len(bases) == 1, 'multiple merge bases are ambiguous')
    base = bases[0]
    conflicts = set(git('diff', '--name-only', '--diff-filter=U', '-z').strip('\0').split('\0')) - {''}
    require(MONOLITH in conflicts, 'expected unresolved coverage.json modify/delete conflict')
    require(not git('ls-tree', incoming, MONOLITH).strip(), 'incoming main still has the monolith')
    unknown = {p for p in conflicts if p != MONOLITH and not p.startswith(RECORDS)}
    require(not unknown, f'unrelated conflicts require manual resolution first: {sorted(unknown)}')
    old, own = document(base, MONOLITH), document(head, MONOLITH)
    module_path = root / 'scripts/coverage_manifest.py'
    spec = importlib.util.spec_from_file_location('coverage_manifest', module_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    current = module.load(root / MANIFEST)
    # Reparse the layout to reject duplicate JSON keys before its normal reader.
    for file in module.files(root / MANIFEST):
        parse(file.read_bytes())
    for value in (old, own, current):
        ids = [x['id'] for x in value['subsystems']]
        require(len(ids) == len(set(ids)), 'duplicate subsystem ids')
    merged = merge(old, own, current, 'coverage')
    planned = {}

    def plan_json(path, value):
        planned[path] = (json.dumps(value, indent=2) + '\n').encode()

    before = {s['id']: s for s in current['subsystems']}
    after = {s['id']: s for s in merged['subsystems']}
    for identity in before.keys() | after.keys():
        require(re.fullmatch(r'[a-z0-9][a-z0-9-]*', identity), f'invalid subsystem id {identity}')
        path = f'{MANIFEST}/subsystems/{identity}.json'
        if identity not in after:
            planned[path] = None
        elif before.get(identity) != after[identity]:
            plan_json(path, {k: v for k, v in after[identity].items() if k != 'id'})
    index = {k: v for k, v in merged.items() if k != 'subsystems'}
    if index != {k: v for k, v in current.items() if k != 'subsystems'}:
        plan_json(f'{MANIFEST}/index.json', index)
    planned[MONOLITH] = None

    # The repo's provenance check has no fix mode. Its digest is SHA-256 of
    # raw file bytes. Resolve metadata independently, never choose a licence.
    tracked = set(git('ls-files', '-z').strip('\0').split('\0'))
    record_paths = {p for p in tracked if p.startswith(RECORDS)}
    mono_record = RECORDS + MONOLITH + '.json'
    template = document(incoming, RECORDS + MANIFEST + '/index.json.json')
    for path in list(planned):
        record_paths.add(RECORDS + path + '.json')
    changed = set(git('diff', '--name-only', head, '-z').strip('\0').split('\0'))
    changed.update(git('diff', '--name-only', incoming, '-z').strip('\0').split('\0'))
    changed.update(planned)
    for record in sorted(record_paths):
        path = record[len(RECORDS):-5]
        if record == mono_record or planned.get(path, MISSING) is None:
            planned[record] = None
            continue
        if record in conflicts:
            versions = []
            for stage in (1, 2, 3):
                data = git('show', f':{stage}:{record}', check=False)
                versions.append(parse(data) if data else MISSING)
            versions = [({k: v for k, v in value.items() if k != 'sha256'}
                         if isinstance(value, dict) else value) for value in versions]
            metadata = merge(*versions, record)
            require(metadata is not MISSING, f'{record}: record deletion conflicts with live file')
        elif (root / record).exists():
            metadata = parse((root / record).read_bytes())
        else:
            require(path in planned and path.startswith(MANIFEST + '/subsystems/'),
                    f'{record}: missing provenance; enroll manually')
            metadata = copy.deepcopy(template)
        if record not in conflicts and path not in changed:
            continue
        content = planned.get(path, MISSING)
        if content is MISSING:
            require((root / path).is_file() and not (root / path).is_symlink(),
                    f'{path}: absent or nonregular provenance target')
            content = (root / path).read_bytes()
        metadata['sha256'] = hashlib.sha256(content).hexdigest()
        plan_json(record, metadata)

    for path, data in planned.items():
        target = root / path
        require(not target.is_symlink(), f'{path}: refusing symlink')
        if data is None:
            target.unlink(missing_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
    git('add', '-A', '--', *sorted(p for p in planned if p in tracked or (root / p).exists()))
    require(not git('diff', '--name-only', '--diff-filter=U').strip(), 'unresolved conflicts remain')
    print(f'Migrated PR {head[:12]} against {incoming[:12]}; subsystems: '
          f'{sorted(k for k in before.keys() | after.keys() if before.get(k) != after.get(k))}', flush=True)
    for command in ([sys.executable, 'scripts/proof_enrollment.py'],
                    [sys.executable, 'verification/coverage-v1/validate.py'],
                    ['cargo', 'xtask', 'ci', 'provenance']):
        print('+ ' + ' '.join(command), flush=True)
        subprocess.run(command, cwd=root, check=True, timeout=1800)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        sys.exit(f'migration failed: {error}')
