#!/usr/bin/env python3
"""install.sh against a local release directory: what it installs and when it refuses.

    python3 -I scripts/release/test_install.py

A fake `uname` and `sysctl` stand in for the platform, and a local HTTP server for the
release, so the cases run anywhere that has sh and curl.
"""

import hashlib
import http.server
import io
import os
import subprocess
import tarfile
import tempfile
import threading
import unittest
from pathlib import Path

INSTALL = Path(__file__).resolve().parent / "install.sh"


def archive(script: str) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        data = script.encode()
        info = tarfile.TarInfo("norn")
        info.size, info.mode = len(data), 0o755
        tar.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class Release(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


class InstallTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.release = self.tmp / "release"
        self.release.mkdir()
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        handler = lambda *a, **k: Release(*a, directory=str(self.release), **k)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.shutdown)
        self.addCleanup(self.server.server_close)

    def publish(self, asset: str, body: bytes, sum_of: bytes | None = None):
        (self.release / asset).write_bytes(body)
        digest = hashlib.sha256(body if sum_of is None else sum_of).hexdigest()
        with open(self.release / "SHA256SUMS", "a") as sums:
            sums.write(f"{digest}  {asset}\n")

    def platform(self, system: str, machine: str, translated: str = "0"):
        for name, output in (("uname", None), ("sysctl", translated)):
            script = self.bin / name
            if output is None:
                script.write_text(
                    f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; *) exit 1;; esac\n'
                )
            else:
                script.write_text(f"#!/bin/sh\necho {output}\n")
            script.chmod(0o755)

    def run_install(self):
        env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "HOME": str(self.tmp / "home"),
            "NORN_BASE_URL": f"http://127.0.0.1:{self.server.server_port}",
            "NORN_INSTALL_DIR": str(self.tmp / "install"),
        }
        return subprocess.run(["sh", str(INSTALL)], env=env, capture_output=True, text=True)

    @property
    def installed(self):
        return (self.tmp / "install" / "norn").exists()

    def test_installs_the_binary_for_this_platform(self):
        self.platform("Linux", "aarch64")
        self.publish("norn-checker-wasm.tar.gz", archive("#!/bin/sh\n"))
        self.publish("norn-dev-host-linux-arm64.tar.gz", archive("#!/bin/sh\n"))
        self.publish("norn-linux-arm64.tar.gz", archive("#!/bin/sh\necho norn-arm64 1.2.3\n"))
        self.publish("norn-linux-x64.tar.gz", archive("#!/bin/sh\necho norn-x64\n"))
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("installed norn-arm64 1.2.3", result.stdout)
        self.assertIn("add it to your PATH", result.stdout)

    def test_rosetta_shell_gets_the_arm64_binary(self):
        self.platform("Darwin", "x86_64", translated="1")
        self.publish("norn-macos-arm64.tar.gz", archive("#!/bin/sh\necho norn-macos-arm64\n"))
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("norn-macos-arm64", result.stdout)
        self.assertIn("experimental", result.stderr)

    def test_a_checksum_mismatch_installs_nothing(self):
        self.platform("Linux", "x86_64")
        self.publish("norn-linux-x64.tar.gz", archive("#!/bin/sh\necho evil\n"), sum_of=b"another file")
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertFalse(self.installed)

    def test_a_missing_binary_for_the_platform_names_it(self):
        self.platform("Linux", "x86_64")
        self.publish("norn-macos-arm64.tar.gz", archive("#!/bin/sh\n"))
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no binary for linux-x64", result.stderr)
        self.assertFalse(self.installed)

    def test_an_unsupported_cpu_is_refused_before_any_download(self):
        self.platform("Linux", "riscv64")
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported CPU riscv64", result.stderr)


if __name__ == "__main__":
    unittest.main()
