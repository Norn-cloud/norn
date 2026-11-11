"""NornDocument: the one controller every generated Virtual DocType uses.

Reads go to the cell's object routes as the Frappe user's principal; the cell
applies grants and withholds values (the bridge shows what it is given).
Writes go to the cell's action route (#1085 §4), which the host does not
serve yet (#805), so every save surfaces the cell's refusal.
"""

from __future__ import annotations

import frappe
from frappe.model.document import Document

from norn_bridge import bundle, client, query


def _record(doctype: str, name: str) -> dict:
    entry = bundle.object_type(doctype)
    try:
        record = client.get_object(entry["objectType"], name)
    except client.Refused as refused:
        if refused.status == 404:
            raise frappe.DoesNotExistError(doctype, name) from None
        frappe.throw(refused.document.get("message") or str(refused), title="Norn")
    return query.to_row(doctype, entry, record)


class NornDocument(Document):
    def load_from_db(self):
        row = _record(self.doctype, self.name)
        super(Document, self).__init__(row)

    def db_insert(self, *args, **kwargs):
        self._write("create")

    def db_update(self, *args, **kwargs):
        self._write("update")

    def delete(self, *args, **kwargs):
        frappe.throw("Norn facts are never deleted; use a lifecycle action", title="Norn")

    def _write(self, mode: str):
        entry = bundle.object_type(self.doctype)
        action = None
        state_field = entry.get("stateField")
        before = self.get_doc_before_save()
        if state_field and before is not None and before.get(state_field) != self.get(state_field):
            action = entry.get("transitions", {}).get(
                f"{before.get(state_field)}->{self.get(state_field)}"
            )
        body = {
            "objectType": entry["objectType"],
            "id": self.name,
            "mode": mode,
            "action": action,
            "expectedVersion": self.get("_norn_version"),
            "values": {prop: self.get(f) for f, prop in entry["fields"].items()},
        }
        try:
            receipt = client.act(action or f"{entry['objectType']}.{mode}", body)
        except client.Refused as refused:
            frappe.throw(refused.document.get("message") or str(refused), title="Norn refused")
        self.flags.norn_receipt = receipt

    @staticmethod
    def get_list(*args, **kwargs):
        return query.get_list(kwargs)

    @staticmethod
    def get_count(*args, **kwargs):
        return query.get_count(kwargs)

    @staticmethod
    def get_stats(*args, **kwargs):
        return query.get_stats(kwargs)
