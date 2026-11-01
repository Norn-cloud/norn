"""CP1-isolation: an allocated but stalled fork is deleted before returning."""
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch, MagicMock
from types import SimpleNamespace
import subprocess
import boat


class CreationTimeout(unittest.TestCase):
    def test_stalled_creation_deletes_allocated_vm(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / 'boat'
            fake.write_text('''#!/usr/bin/env python3
import json, os, sys, time
with open(os.environ['CALLS'], 'a') as log:
    log.write(' '.join(sys.argv[1:]) + '\\n')
if sys.argv[1] == 'limits':
    print(json.dumps({'canStart': True, 'starts': {'day': {'remaining': 1}}}))
elif sys.argv[1] == 'new':
    print(json.dumps({'event': 'created', 'id': 'test-vm'}), flush=True)
    time.sleep(60)
elif sys.argv[1] == 'info':
    print('not found (404)')
    sys.exit(1)
''')
            fake.chmod(0o700)
            args = ['boat.py', '--snapshot', 'synthetic', '--source', str(root/'source'),
                    '--traces', str(root/'traces'), '--output', str(root/'output')]
            env = {'PATH': str(root)+os.pathsep+os.environ['PATH'], 'CALLS': str(root/'calls')}
            with patch.dict(os.environ, env), patch('sys.argv', args), patch.object(
                    boat, 'Timer', side_effect=lambda _, callback: threading.Timer(0.2, callback)):
                with self.assertRaisesRegex(RuntimeError, 'timed out'):
                    boat.main()
            calls = (root/'calls').read_text().splitlines()
            self.assertFalse(any(c.startswith('scp ') for c in calls))
            self.assertIn('delete test-vm --yes', calls)
            self.assertEqual(calls[-1], 'info test-vm --json')

    def test_failed_acceptance_keeps_dump_before_deletion(self):
        calls = []
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'source').write_bytes(b'synthetic')
            def fake_boat(*args, **kwargs):
                calls.append(args)
                if args[0] == 'limits':
                    return '{"canStart":true,"starts":{"day":{"remaining":1}}}'
                if args[0] == 'exec':
                    raise subprocess.CalledProcessError(1, ['boat', 'exec'])
                if args[:2] == ('scp', 'test-vm:/home/user/results.json'):
                    Path(args[2]).write_text('normalized-dump')
                return ''
            proc = MagicMock()
            proc.stdout = ['{"id":"test-vm"}']
            proc.wait.return_value = 0
            args = ['boat.py', '--snapshot', 'synthetic', '--source', str(root/'source'),
                    '--traces', str(root/'traces'), '--output', str(root/'output')]
            with patch('sys.argv', args), patch.object(boat, 'boat', side_effect=fake_boat), patch.object(
                    boat.subprocess, 'Popen') as popen, patch.object(boat.subprocess, 'run', return_value=
                    SimpleNamespace(returncode=1, stdout='not found (404)', stderr='')):
                popen.return_value.__enter__.return_value = proc
                with self.assertRaises(subprocess.CalledProcessError):
                    boat.main()
            self.assertEqual((root/'output').read_text(), 'normalized-dump')
            download = next(i for i, c in enumerate(calls) if c[:2] == ('scp', 'test-vm:/home/user/results.json'))
            self.assertLess(download, next(i for i, c in enumerate(calls) if c[0] == 'delete'))


if __name__ == '__main__':
    unittest.main()
