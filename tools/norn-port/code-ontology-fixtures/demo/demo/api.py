import frappe

@frappe.whitelist()
def update(name):
    doc = frappe.get_doc("Invoice", name)
    doc.status = "Paid"
    total = doc.total
    frappe.db.set_value("Invoice", name, "status", "Paid")
    frappe.db.get_value("Invoice", name, "total")
    frappe.get_all("Invoice", fields=["total", "status"])
    frappe.db.get_list("Invoice", fields=["status"])
    frappe.new_doc("Invoice")
    helper()
    return total

def helper():
    return None

def on_update(doc, method=None):
    if doc.total < 0:
        frappe.throw("Negative")
    doc.status = "Updated"

def daily():
    frappe.get_list("Invoice", fields=["status"])

def permission(user):
    return "owner = " + user

def has_permission(doc, user=None):
    return doc.status == "Paid"

def dynamic(doctype, fields):
    doc = frappe.get_doc(doctype)
    doc.total = 1
    frappe.get_all("Invoice", fields=fields)
    frappe.db.get_value("Invoice", "x", "sum(total) as total")
    frappe.db.get_value("External", "x", "missing")
    doc = {}
    return doc.total

def branch(flag):
    doc = frappe.get_doc("Invoice", "x")
    if flag:
        doc = {}
    doc.status = "uncertain"
    def nested():
        doc.status = "closure not inferred"
    return nested

def loop(documents):
    doc = frappe.get_doc("Invoice", "x")
    for doc in documents:
        doc.total = 4
    doc.total = 5

def same_line():
    helper(); external()

def comprehensions(name, others):
    doc = frappe.get_doc("Invoice", name)
    values = [doc.status for doc in others]
    unique = {doc.status for doc in others}
    mapping = {doc.status: doc.total for doc in others}
    lazy = (doc.status for doc in others)
    nested = [[doc.status for doc in others] for row in others]
    still_typed = doc.status
    return values, unique, mapping, lazy, nested, still_typed

def alias_invalidation(flag):
    invoice = frappe.get_doc("Invoice", "x")
    before = invoice.total
    if flag:
        invoice = {}
    return invoice.total

def alias_loop(others):
    invoice = frappe.get_doc("Invoice", "x")
    for invoice in others:
        invoice.total = 1
    return invoice.total

def exception_alias():
    doc = frappe.get_doc("Invoice", "x")
    try:
        external()
    except Exception as doc:
        return doc.total

def shared_wildcard(doc):
    return doc.total

def shared_unknown(doc):
    return doc.total

def match_captures(value):
    invoice = frappe.get_doc("Invoice", "x")
    match value:
        case {"invoice": invoice}:
            first = invoice.total
    invoice = frappe.get_doc("Invoice", "x")
    match value:
        case [*invoice]:
            second = invoice.total
    invoice = frappe.get_doc("Invoice", "x")
    match value:
        case {**invoice}:
            third = invoice.total
    return invoice.total
