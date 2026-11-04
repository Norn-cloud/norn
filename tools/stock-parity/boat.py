"""Controller entry point. Snapshot identity is supplied privately, never in artifacts."""
import argparse
import json
import pathlib
import subprocess
from threading import Timer
import hashlib


def boat(*args, timeout=900):
    return subprocess.check_output(['boat', *args], text=True, timeout=timeout)


def cleanup(vm):
    try:
        boat('delete', vm, '--yes')
    except (OSError, subprocess.SubprocessError):
        pass
    try:
        check = subprocess.run(['boat', 'info', vm, '--json'], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError('VM deletion was not verified') from error
    if check.returncode == 0 or not any(t in (check.stdout + check.stderr).lower()
                                       for t in ('404', 'not_found', 'not found')):
        raise RuntimeError('VM deletion was not verified')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--snapshot', required=True, help='Prebuilt ERPNext v16 Boat snapshot')
    parser.add_argument('--source', type=pathlib.Path, required=True, help='Committed source archive')
    parser.add_argument('--traces', type=pathlib.Path, required=True, help='Validated stock-trace/1 JSON array')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    limits = json.loads(boat('limits', '--json'))
    if not limits['canStart'] or limits['starts']['day']['remaining'] < 1:
        raise SystemExit('Boat start budget unavailable')
    vm = None
    try:
        # Stream creation events so cleanup also covers a failure after allocation.
        with subprocess.Popen(['boat', 'new', '--json', '--from', args.snapshot, '--type', 'default',
                               '--ttl', '7200', '--no-env', '--no-monitoring', '--fail-fast'],
                              stdout=subprocess.PIPE, text=True) as proc:
            deadline = Timer(300, proc.kill)
            deadline.start()
            try:
                for line in proc.stdout:
                    event = json.loads(line)
                    if event.get('id'):
                        vm = event['id']
                if proc.wait() or not vm:
                    raise RuntimeError('Boat fork failed or timed out')
            finally:
                deadline.cancel()
        for src, dest in ((args.source, 'source.tar'), (args.traces, 'traces.json')):
            boat('scp', str(src), f'{vm}:/home/user/{dest}')
        digest = hashlib.sha256(args.source.read_bytes()).hexdigest()
        command = '''set -eu
echo 'DIGEST  /home/user/source.tar' | sha256sum --check --status
mkdir /home/user/parity-source
python3 - <<'PY'
import tarfile
from pathlib import PurePosixPath
with tarfile.open('/home/user/source.tar') as t:
    for m in t.getmembers():
        p = PurePosixPath(m.name)
        assert not p.is_absolute() and '..' not in p.parts and (m.isfile() or m.isdir())
    t.extractall('/home/user/parity-source', filter='data')
PY
bash /home/user/parity-source/tools/stock-parity/boot.sh
cd /home/user/parity-source/tools/stock-parity
python3 run.py /home/user/traces.json /home/user/results.json
'''.replace('DIGEST', digest)
        result = json.loads(boat('exec', vm, '--json', '--timeout', '1800', '--', 'bash', '-lc', command, timeout=1860))
        if not result.get('success'):
            raise RuntimeError('Oracle batch incomplete; inspect disposable VM logs before cleanup')
        boat('scp', f'{vm}:/home/user/results.json', str(args.output))
    finally:
        if vm:
            cleanup(vm)


if __name__ == '__main__':
    main()
