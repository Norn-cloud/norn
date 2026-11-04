"""CP1-isolation: an allocated but stalled fork is deleted before returning."""
import os
from pathlib import Path
import tempfile
import threading
import socket
import unittest
from unittest.mock import patch
import boat


class CreationTimeout(unittest.TestCase):
    def test_stalled_creation_deletes_allocated_vm(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root / 'boat'
            fake.write_text('''#!/usr/bin/env python3
import json, os, sys, time, socket
with open(os.environ['CALLS'], 'a') as log:
    log.write(' '.join(sys.argv[1:]) + '\\n')
if sys.argv[1] == 'limits':
    print(json.dumps({'canStart': True, 'starts': {'day': {'remaining': 1}}}))
elif sys.argv[1] == 'new':
    print(json.dumps({'event': 'created', 'id': 'test-vm'}), flush=True)
    with socket.socket(socket.AF_UNIX) as signal:
        signal.connect(os.environ['READY'])
        signal.sendall(b'ready')
    time.sleep(60)
elif sys.argv[1] == 'info':
    print('not found (404)')
    sys.exit(1)
''')
            fake.chmod(0o700)
            args = ['boat.py', '--snapshot', 'synthetic', '--source', str(root/'source'),
                    '--traces', str(root/'traces'), '--output', str(root/'output')]
            server = socket.socket(socket.AF_UNIX)
            self.addCleanup(server.close)
            server.bind(str(root/'ready'))
            server.listen(1)
            server.settimeout(5)
            ready = threading.Event()
            def kill_after_signal(callback):
                try:
                    with server.accept()[0] as connection:
                        connection.settimeout(5)
                        if connection.recv(5) == b'ready':
                            ready.set()
                finally:
                    callback()
            env = {'PATH': str(root)+os.pathsep+os.environ['PATH'], 'CALLS': str(root/'calls'),
                   'READY': str(root/'ready')}
            with patch.dict(os.environ, env), patch('sys.argv', args), patch.object(
                    boat, 'Timer', side_effect=lambda _, callback: threading.Timer(0, kill_after_signal, args=(callback,))):
                with self.assertRaisesRegex(RuntimeError, 'timed out'):
                    boat.main()
            self.assertTrue(ready.is_set(), 'fake allocation signalled before timeout')
            calls = (root/'calls').read_text().splitlines()
            self.assertIn('delete test-vm --yes', calls)
            self.assertEqual(calls[-1], 'info test-vm --json')


if __name__ == '__main__':
    unittest.main()
