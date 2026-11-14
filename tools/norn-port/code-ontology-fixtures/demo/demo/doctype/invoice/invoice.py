import frappe
from frappe.model.document import Document

class Invoice(Document):
    def validate(self):
        if self.total < 0:
            frappe.throw("Negative total")
        self.status = "Checked"
        self.total += 1
