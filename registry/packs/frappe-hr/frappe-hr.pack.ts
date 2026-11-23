/**
 * Frappe HR clean-room ontology pack (map-only, reference-only) (#1592).
 *
 * Provenance rule (`CONTRIBUTING.md`, `AGENTS.md`): Frappe HR is GPL-3.0-only.
 * Zero DocType JSON schemas or Python controller code are copied here; this
 * file declares original semantic-only handles pinned to an immutable release
 * commit for interoperability projection onto Norn's canon and black-box
 * behavioral oracle comparison.
 */
import { Norn } from "@norn/authoring"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.frappe-hr",
  title: "Frappe HR v15 system pack (map-only, clean-room)",
  version: "0.1.0",
  sources: [
    {
      sourceId: "source.frappe-hr",
      releaseId: "version-15@46b389047bb12fb9c86c1300c3d7bfd5f897f171",
      releasedAt: "2026-11-23",
      retrievedAt: "2026-11-23",
      canonicalUrl: "https://github.com/frappe/hrms/tree/46b389047bb12fb9c86c1300c3d7bfd5f897f171",
      license: "GPL-3.0-only",
      redistribution: "reference_only",
      importMode: "map_only",
    },
  ],
  terms: [
    // Classes
    { termId: "norn.frappe-hr.employee", label: "Employee", definition: "Frappe HR employee master record combining person identity and active employment.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.department", label: "Department", definition: "Frappe HR hierarchical organizational department.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.designation", label: "Designation", definition: "Frappe HR job title or post designation.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.holiday.list", label: "Holiday List", definition: "Frappe HR calendar of public holidays and weekly off days.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.holiday.list.holiday", label: "Holiday", definition: "One holiday or weekly-off date row inside a Holiday List.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.leave.allocation", label: "Leave Allocation", definition: "Frappe HR period entitlement of leave days allocated to an employee.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.leave.application", label: "Leave Application", definition: "Frappe HR employee request for leave over a date interval.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.shift.type", label: "Shift Type", definition: "Frappe HR recurring local wall-clock shift window definition.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.shift.assignment", label: "Shift Assignment", definition: "Frappe HR scheduled shift assigned to an employee over a date range.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.attendance", label: "Attendance", definition: "Frappe HR posted daily attendance record for an employee.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.attendance.request", label: "Attendance Request", definition: "Frappe HR employee request to regularize or submit missed attendance.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    // Properties
    { termId: "norn.frappe-hr.employee.department", label: "department (employee)", definition: "Department linked to an Employee record.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.employee" },
    { termId: "norn.frappe-hr.employee.designation", label: "designation (employee)", definition: "Designation linked to an Employee record.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.employee" },
    { termId: "norn.frappe-hr.employee.date.of.joining", label: "date_of_joining (employee)", definition: "Employment start date recorded on the Employee master.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.employee" },
    { termId: "norn.frappe-hr.leave.allocation.new.leaves.allocated", label: "new_leaves_allocated (leave.allocation)", definition: "Number of leave days newly granted in a Leave Allocation.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.leave.allocation" },
    { termId: "norn.frappe-hr.leave.application.from.date", label: "from_date (leave.application)", definition: "Start date of a Leave Application.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.leave.application" },
    { termId: "norn.frappe-hr.leave.application.to.date", label: "to_date (leave.application)", definition: "End date of a Leave Application.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.leave.application" },
    { termId: "norn.frappe-hr.leave.application.total.leave.days", label: "total_leave_days (leave.application)", definition: "Calculated leave days excluding non-working holidays.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.leave.application" },
    { termId: "norn.frappe-hr.shift.assignment.start.date", label: "start_date (shift.assignment)", definition: "First effective date of a Shift Assignment.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.shift.assignment" },
    { termId: "norn.frappe-hr.shift.assignment.end.date", label: "end_date (shift.assignment)", definition: "Optional last effective date of a Shift Assignment.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.shift.assignment" },
    { termId: "norn.frappe-hr.attendance.status", label: "status (attendance)", definition: "Daily attendance classification (Present, Absent, On Leave, Half Day, Work From Home).", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.attendance" },
    // Actions
    { termId: "norn.frappe-hr.leave.application.submit", label: "Submit Leave Application", definition: "Submit and approve a Leave Application, posting a leave ledger debit.", termKind: "action", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.leave.application" },
    { termId: "norn.frappe-hr.leave.application.cancel", label: "Cancel Leave Application", definition: "Cancel a submitted Leave Application and reverse its leave ledger entry.", termKind: "action", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.leave.application" },
    { termId: "norn.frappe-hr.attendance.cancel.amend", label: "Cancel and amend Attendance", definition: "Cancel a submitted Attendance document and amend a replacement copy in place of an immutable reversing event.", termKind: "action", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.frappe-hr.attendance" },
    // Guards / predicates
    { termId: "norn.frappe-hr.guard.leave.insufficient.balance", label: "Insufficient leave balance", definition: "Refuses a Leave Application when requested days exceed available balance (unless negative balance is allowed).", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.guard.leave.overlap", label: "Overlapping leave application", definition: "Refuses overlapping Leave Applications for the same employee.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.frappe-hr.guard.shift.overlap", label: "Overlapping shift assignment", definition: "Refuses overlapping Shift Assignments for the same employee.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
  ],
  mappings: [
    { mappingId: "map.frappe-hr.employee", termId: "norn.frappe-hr.employee", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/employee", relation: "close_match", justification: "Public Frappe HR Employee concept.", evidenceRefs: ["https://docs.frappe.io/hr/employee"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.department", termId: "norn.frappe-hr.department", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/department", relation: "close_match", justification: "Public Frappe HR Department concept.", evidenceRefs: ["https://docs.frappe.io/hr/department"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.designation", termId: "norn.frappe-hr.designation", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/designation", relation: "close_match", justification: "Public Frappe HR Designation concept.", evidenceRefs: ["https://docs.frappe.io/hr/designation"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.holiday.list", termId: "norn.frappe-hr.holiday.list", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/holiday-list", relation: "close_match", justification: "Public Frappe HR Holiday List concept.", evidenceRefs: ["https://docs.frappe.io/hr/holiday-list"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.leave.allocation", termId: "norn.frappe-hr.leave.allocation", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/leave-allocation", relation: "close_match", justification: "Public Frappe HR Leave Allocation concept.", evidenceRefs: ["https://docs.frappe.io/hr/leave-allocation"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.leave.application", termId: "norn.frappe-hr.leave.application", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/leave-application", relation: "close_match", justification: "Public Frappe HR Leave Application concept.", evidenceRefs: ["https://docs.frappe.io/hr/leave-application"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.shift.type", termId: "norn.frappe-hr.shift.type", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/shift-type", relation: "close_match", justification: "Public Frappe HR Shift Type concept.", evidenceRefs: ["https://docs.frappe.io/hr/shift-type"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.shift.assignment", termId: "norn.frappe-hr.shift.assignment", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/shift-assignment", relation: "close_match", justification: "Public Frappe HR Shift Assignment concept.", evidenceRefs: ["https://docs.frappe.io/hr/shift-assignment"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.attendance", termId: "norn.frappe-hr.attendance", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/attendance", relation: "close_match", justification: "Public Frappe HR Attendance concept.", evidenceRefs: ["https://docs.frappe.io/hr/attendance"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.frappe-hr.attendance.request", termId: "norn.frappe-hr.attendance.request", sourceId: "source.frappe-hr", externalIri: "https://docs.frappe.io/hr/attendance-request", relation: "close_match", justification: "Public Frappe HR Attendance Request concept.", evidenceRefs: ["https://docs.frappe.io/hr/attendance-request"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
  ],
  shapes: [
    { shapeId: "shape.frappe-hr.employee", termId: "norn.frappe-hr.employee", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.department", termId: "norn.frappe-hr.department", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.designation", termId: "norn.frappe-hr.designation", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.holiday.list", termId: "norn.frappe-hr.holiday.list", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.holiday.list.holiday", termId: "norn.frappe-hr.holiday.list.holiday", closed: true, fields: [{ fieldId: "holidayDate", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.leave.allocation", termId: "norn.frappe-hr.leave.allocation", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.leave.application", termId: "norn.frappe-hr.leave.application", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.shift.type", termId: "norn.frappe-hr.shift.type", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.shift.assignment", termId: "norn.frappe-hr.shift.assignment", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.attendance", termId: "norn.frappe-hr.attendance", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.frappe-hr.attendance.request", termId: "norn.frappe-hr.attendance.request", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
  ],
})
