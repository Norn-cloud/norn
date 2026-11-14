"""The code-graph pin refuses every binary that is not the pinned release's.

No real binary is needed: a stand-in shell script plays one and leaves a
marker if it is ever executed. The stand-in's digest is not the pinned one, so
removing any digest check makes one of these tests fail.
"""
import hashlib
import io
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import codegraph

STAND_IN = """#!/bin/sh
: > "$MARKER"
case "$1" in
  --version) echo "codegraph 0.54.0" ;;
  init) ;;
  export) shift; while [ "$1" != "-o" ]; do shift; done; echo '{"nodes": [], "links": []}' > "$2" ;;
esac
"""


class PinTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / "stand-in"
        self.binary.write_text(STAND_IN)
        self.binary.chmod(0o755)
        self.marker = self.root / "EXECUTED"
        self.env = mock.patch.dict(os.environ, {"MARKER": str(self.marker)})
        self.env.start()
        self.addCleanup(self.env.stop)
        os.environ.pop("CI", None)

    def graph(self, allow_unpinned=False):
        scratch = self.root / "scratch"
        return codegraph.CodeGraphRust(self.binary, scratch, allow_unpinned).extract({"a.py": b"x = 1\n"})

    def test_a_binary_with_another_digest_never_runs(self):
        with self.assertRaises(codegraph.UnpinnedBinary):
            self.graph()
        self.assertFalse(self.marker.exists(), "the version probe ran before the digest check")

    def test_the_explicit_override_runs_it_with_a_warning_but_never_in_ci(self):
        # Positive control: the stand-in works, so the refusals above are real.
        self.assertEqual(self.graph(allow_unpinned=True), {"nodes": [], "links": []})
        self.assertTrue(self.marker.exists())
        self.marker.unlink()
        with mock.patch.dict(os.environ, {"CI": "true"}):
            with self.assertRaises(codegraph.UnpinnedBinary):
                self.graph(allow_unpinned=True)
        self.assertFalse(self.marker.exists())

    def test_a_cached_file_with_another_digest_is_refused_before_any_download(self):
        cache = self.root / "cache"
        cache.mkdir()
        (cache / "codegraph").write_bytes(self.binary.read_bytes())
        with mock.patch.object(codegraph.urllib.request, "urlopen", side_effect=AssertionError("downloaded")):
            with self.assertRaises(codegraph.UnpinnedBinary):
                codegraph.fetch(cache)

    def test_an_archive_with_the_pinned_digest_but_another_binary_is_refused(self):
        # The archive digest is checked first; a matching archive whose member
        # is not the pinned binary must still be refused.
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w:gz") as archive:
            data = self.binary.read_bytes()
            member = tarfile.TarInfo("codegraph")
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
        payload = raw.getvalue()
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = payload
        cache = self.root / "fresh"
        with mock.patch.object(codegraph, "ARCHIVE_SHA256", hashlib.sha256(payload).hexdigest()), \
                mock.patch.object(codegraph.urllib.request, "urlopen", return_value=response):
            with self.assertRaises(codegraph.UnpinnedBinary):
                codegraph.fetch(cache)
        self.assertFalse((cache / "codegraph").exists())


if __name__ == "__main__":
    unittest.main()
