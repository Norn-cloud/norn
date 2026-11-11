"""DocType Actions for Norn actions (#1085 §3.3), with no Python per action.

A generated DocType Action row names `norn_bridge.act.<slug>`. PEP 562
`__getattr__` synthesizes one whitelisted function per slug the installed
bundle declares and refuses every other name.
"""

from __future__ import annotations

import frappe

_made: dict[str, object] = {}


def __getattr__(slug: str):
    if slug.startswith("__"):
        raise AttributeError(slug)
    from norn_bridge import bundle

    actions = (bundle.load() or {}).get("actions", {})
    if slug not in actions:
        raise AttributeError(f"norn_bridge.act has no action {slug!r} in the installed bundle")
    if slug not in _made:
        spec = actions[slug]

        def run(doc=None, **_ignored):
            from norn_bridge import client

            document = frappe.parse_json(doc) if doc else {}
            try:
                return client.act(spec["action"], {"objectType": spec["objectType"], "id": document.get("name")})
            except client.Refused as refused:
                frappe.throw(refused.document.get("message") or str(refused), title="Norn refused")

        run.__name__ = slug
        run.__qualname__ = slug
        run.__module__ = __name__
        _made[slug] = frappe.whitelist()(run)
    return _made[slug]
