"""Oracle fixture: mutation, typed refusal, and an unused platform import."""
import frappe
import platform_package_that_must_never_be_stubbed


# A local parameter named doc must not select this unused module setup.
doc = 1 / 0


def bump(doc):
    if doc.count < 0:
        frappe.throw("count must be nonnegative")
    doc.count += 1
    return doc.count


def mutate_boolean(doc):
    doc.flag = int(doc.flag)
    doc.nested[0].flag = int(doc.nested[0].flag)
    return None


def mutate_number(data):
    data["flag"] = bool(data["flag"])
    data["nested"][0]["flag"] = bool(data["nested"][0]["flag"])
    return None
