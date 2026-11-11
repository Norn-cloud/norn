"""B0 probe: does Frappe's own Workflow insert work on a Virtual DocType?

Run with the bench's Python from `sites/`. Rolls back whatever happens.
"""

import sys
import traceback

import frappe

frappe.init(sys.argv[1], sites_path=".")
frappe.connect()
try:
    frappe.get_doc(
        {
            "doctype": "Workflow",
            "workflow_name": "Probe lifecycle",
            "document_type": "Demo Repair",
            "workflow_state_field": "status",
            "is_active": 0,
            "states": [{"state": "draft", "doc_status": "0", "allow_edit": "Demo Clerk"}],
        }
    ).insert(ignore_permissions=True)
    print("native Workflow insert: OK")
except Exception:
    lines = traceback.format_exc().splitlines()
    print("native Workflow insert refused:")
    print("\n".join(line for line in lines if "workflow.py" in line or "Error" in line))
finally:
    frappe.db.rollback()
