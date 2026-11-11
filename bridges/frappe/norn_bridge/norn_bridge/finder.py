"""One generic import finder for generated DocType controllers.

Frappe imports a standard DocType's controller from
`norn_bridge.norn_generated.doctype.<slug>.<slug>`. This finder synthesizes
that package and module for each DocType in the site's installed bundle: the
module holds one class, named as Frappe expects, subclassing NornDocument. A
DocType that is not in the bundle does not import, so `bench migrate`'s
orphan sweep removes it like any deleted DocType.
"""

from __future__ import annotations

import importlib.abc
import importlib.machinery
import sys

PREFIX = "norn_bridge.norn_generated.doctype."


def _scrub(name: str) -> str:
    return name.replace(" ", "_").replace("-", "_").lower()


def _doctypes() -> dict[str, str]:
    """slug -> DocType name, for the current site's installed bundle."""
    from norn_bridge import bundle

    try:
        document = bundle.load("installed") or {}
    except Exception:
        return {}
    return {_scrub(name): name for name in document.get("objectTypes", {})}


class _Loader(importlib.abc.Loader):
    def __init__(self, doctype: str | None):
        self.doctype = doctype

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        if self.doctype is None:  # the per-DocType package
            module.__path__ = []
            return
        from norn_bridge.document import NornDocument

        classname = self.doctype.replace(" ", "").replace("-", "")
        cls = type(classname, (NornDocument,), {"__module__": module.__name__})
        setattr(module, classname, cls)


class GeneratedControllers(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if not fullname.startswith(PREFIX):
            return None
        parts = fullname[len(PREFIX) :].split(".")
        doctypes = _doctypes()
        if parts[0] not in doctypes:
            return None
        if len(parts) == 1:
            return importlib.machinery.ModuleSpec(fullname, _Loader(None), is_package=True)
        if len(parts) == 2 and parts[1] == parts[0]:
            return importlib.machinery.ModuleSpec(fullname, _Loader(doctypes[parts[0]]))
        return None


def install() -> None:
    if not any(isinstance(f, GeneratedControllers) for f in sys.meta_path):
        sys.meta_path.insert(0, GeneratedControllers())
