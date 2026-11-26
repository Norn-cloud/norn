#!/usr/bin/env python3
"""owl_sources: a pinned fetch reads the pinned URL itself and refuses a redirect before any
digest is computed (#1537); `pin` follows an HTTPS redirect and pins its destination. The
network is a fake HTTPS transport: nothing leaves the process."""

import email.message
import io
import tempfile
import unittest
import urllib.request
import urllib.response
from pathlib import Path


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


owl_sources = _sibling("owl_sources")

PINNED = "https://example.test/onto/Pinned.rdf"
ELSEWHERE = "https://mirror.example.test/onto/Pinned.rdf"
BODY = b"<rdf:RDF/>"


class FakeHttps(urllib.request.HTTPSHandler):
    """Serves `routes`: a URL maps to (status, body, location)."""

    def __init__(self, routes):
        super().__init__()
        self.routes = routes
        self.opened = []

    def https_open(self, req):
        self.opened.append(req.full_url)
        status, body, location = self.routes[req.full_url]
        headers = email.message.Message()
        if location:
            headers["Location"] = location
        response = urllib.response.addinfourl(io.BytesIO(body), headers, req.full_url, status)
        response.msg = "Found" if location else "OK"
        return response


class PinnedFetch(unittest.TestCase):
    def transport(self, routes):
        fake = FakeHttps(routes)
        previous = getattr(owl_sources, "TRANSPORT", ())
        owl_sources.TRANSPORT = (fake,)
        self.addCleanup(setattr, owl_sources, "TRANSPORT", previous)
        # The process-wide opener too, so a fetch through `urlopen` meets the same fake.
        urllib.request.install_opener(urllib.request.build_opener(fake))
        self.addCleanup(urllib.request.install_opener, None)
        return fake

    def test_redirect_is_refused_before_the_digest(self):
        fake = self.transport({PINNED: (302, b"", ELSEWHERE), ELSEWHERE: (200, BODY, None)})
        with tempfile.TemporaryDirectory() as raw_dir:
            with self.assertRaises(SystemExit) as refused:
                owl_sources.acquire(PINNED, owl_sources.sha256(BODY), Path(raw_dir))
            self.assertIn("follows no redirect", str(refused.exception))
            self.assertEqual(list(Path(raw_dir).rglob("*.rdf")), [])
        self.assertEqual(fake.opened, [PINNED], "the destination is never fetched")

    def test_direct_pin_is_fetched(self):
        self.transport({PINNED: (200, BODY, None)})
        with tempfile.TemporaryDirectory() as raw_dir:
            raw = owl_sources.acquire(PINNED, owl_sources.sha256(BODY), Path(raw_dir))
        self.assertEqual(raw, BODY)

    def test_pin_follows_and_records_the_destination(self):
        self.transport({PINNED: (301, b"", ELSEWHERE), ELSEWHERE: (200, BODY, None)})
        raw, served = owl_sources.fetch_url(PINNED, attempts=1, follow=True)
        self.assertEqual((raw, served), (BODY, ELSEWHERE))

    def test_pin_refuses_a_redirect_off_https(self):
        self.transport({PINNED: (302, b"", "http://example.test/onto/Pinned.rdf")})
        with self.assertRaises(SystemExit):
            owl_sources.fetch_url(PINNED, attempts=1, follow=True)

    def test_local_path_refuses_non_https_and_symlinks(self):
        with tempfile.TemporaryDirectory() as raw_dir:
            root = Path(raw_dir)
            with self.assertRaises(SystemExit):
                owl_sources.local_path(root, "relative/foo.rdf")
            with self.assertRaises(SystemExit):
                owl_sources.local_path(root, "https://example.test/./foo.rdf")
            outside = Path(tempfile.mkdtemp())
            self.addCleanup(lambda: outside.rmdir() if outside.exists() else None)
            (root / "example.test").symlink_to(outside)
            with self.assertRaises(SystemExit):
                owl_sources.local_path(root, PINNED)

    def test_declares_licence_requires_ontology_closing_tag(self):
        raw = b"<dct:license>https://opensource.org/licenses/MIT</dct:license>"
        self.assertFalse(owl_sources.declares_licence(raw, "MIT"))
        valid = b"<owl:Ontology><dct:license>https://opensource.org/licenses/MIT</dct:license></owl:Ontology>"
        self.assertTrue(owl_sources.declares_licence(valid, "MIT"))


if __name__ == "__main__":
    unittest.main()
