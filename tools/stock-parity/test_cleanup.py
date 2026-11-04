"""CP1-isolation: verification remains authoritative after a delete error."""
import subprocess
import unittest
from unittest.mock import patch
import boat


class Cleanup(unittest.TestCase):
    def test_delete_error_still_verifies_absence(self):
        absent = subprocess.CompletedProcess([], 1, '', 'not found (404)')
        with patch.object(boat, 'boat', side_effect=OSError('delete transport')), patch.object(
                boat.subprocess, 'run', return_value=absent) as verify:
            boat.cleanup('synthetic-vm')
        self.assertEqual(verify.call_args.args[0], ['boat', 'info', 'synthetic-vm', '--json'])

    def test_existing_vm_and_unavailable_verification_fail(self):
        with patch.object(boat, 'boat'), patch.object(boat.subprocess, 'run', return_value=
                subprocess.CompletedProcess([], 0, '{}', '')):
            with self.assertRaisesRegex(RuntimeError, 'not verified'):
                boat.cleanup('synthetic-vm')
        with patch.object(boat, 'boat'), patch.object(boat.subprocess, 'run', side_effect=
                subprocess.TimeoutExpired('boat info', 60)):
            with self.assertRaisesRegex(RuntimeError, 'not verified'):
                boat.cleanup('synthetic-vm')
