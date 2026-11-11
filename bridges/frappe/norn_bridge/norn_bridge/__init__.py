"""Norn bridge: one generic Frappe app, shipped once.

Frappe shows Norn-owned objects through generated Virtual DocTypes. Every
generated record (DocType, Workflow, Role, DocPerm) is data from a Norn
projection bundle; no Python is written per app. The cell decides every read
and write; this package only forwards identity and relays answers.
"""

__version__ = "0.0.1"

from norn_bridge import finder as _finder

_finder.install()
