"""Controller entry point. Snapshot identity is supplied privately, never in artifacts."""
import argparse
import json
import pathlib
import subprocess
from threading import Timer
import hashlib


def boat(*args, timeout=900):
    return subprocess.check_output(['boat', *args], text=True, timeout=timeout)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--snapshot', required=True, help='Prebuilt ERPNext v16 Boat snapshot')
    parser.add_argument('--source', type=pathlib.Path, required=True, help='Committed source archive')
    parser.add_argument('--traces', type=pathlib.Path, required=True, help='Validated stock-trace/1 JSON array')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--acceptance', choices=('smoke', 'lifecycle'), default='smoke')
    args = parser.parse_args()
    limits = json.loads(boat('limits', '--json'))
    if not limits['canStart'] or limits['starts']['day']['remaining'] < 1:
        raise SystemExit('Boat start budget unavailable')
    vm, completed, dump_expected = None, False, False
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
python3 CHECKER /home/user/results.json
'''.replace('DIGEST', digest).replace('CHECKER', {'smoke': 'check_smoke.py',
            'lifecycle': 'lifecycle.py'}[args.acceptance])
        seconds = 1800 if args.acceptance == 'lifecycle' else 600
        dump_expected = True
        result = json.loads(boat('exec', vm, '--json', '--timeout', str(seconds), '--', 'bash', '-lc', command,
                                 timeout=seconds + 60))
        completed = bool(result.get('success'))
        if not completed:
            raise RuntimeError('Oracle batch incomplete; inspect retained dump when available')
    finally:
        if vm:
            try:
                if dump_expected:
                    try:
                        boat('scp', f'{vm}:/home/user/results.json', str(args.output), timeout=60)
                    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                        if completed:
                            raise
            finally:
                boat('delete', vm, '--yes')
                check = subprocess.run(['boat', 'info', vm, '--json'], capture_output=True, text=True, timeout=60)
                if check.returncode == 0 or not any(t in (check.stdout + check.stderr).lower()
                                                     for t in ('404', 'not_found', 'not found')):
                    raise RuntimeError('VM deletion was not verified')


if __name__ == '__main__':
    main()
