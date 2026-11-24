/**
 * Norn Workforce, Organization & Time canonical ontology pack (#1591).
 *
 * Modular sibling to `norn.ontology-pack.canon` (commerce/finance REA canon) so
 * commerce/finance cell digests remain undisturbed while time, org, and HR
 * system packs (e.g. Frappe HR in #1592) project onto canonical workforce terms.
 */
import { Norn } from "@norn/authoring"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.workforce",
  title: "Norn workforce canon handles: time, organization, and HR primitives",
  version: "0.1.0",
  sources: [
    {
      sourceId: "source.w3c-org",
      releaseId: "0.8@2014-04-12",
      releasedAt: "2014-04-12",
      retrievedAt: "2026-11-23",
      canonicalUrl: "https://www.w3.org/TR/vocab-org/",
      license: "PDDL-1.0",
      redistribution: "reference_only",
      importMode: "map_only",
      files: [{ path: "https://www.w3.org/ns/org.ttl", sha256: "3e01f89a17475a88f8b1606ca0fae04800d34dbbf93adc6db12dc30929848729" }],
    },
    {
      sourceId: "source.w3c-time",
      releaseId: "2016@2017-04-06",
      releasedAt: "2017-04-06",
      retrievedAt: "2026-11-23",
      canonicalUrl: "https://www.w3.org/TR/owl-time/",
      license: "CC-BY-4.0",
      redistribution: "reference_only",
      importMode: "map_only",
      files: [{ path: "https://www.w3.org/2006/time.ttl", sha256: "251bd6970b0d7a5efa97ff681afe81a297df85deab3d860fa4f8eeba72d8f16c" }],
    },
  ],
  terms: [
    { termId: "norn.time.calendar", label: "Calendar", definition: "A pinned IANA timezone and DST resolution calendar governing local wall-clock schedules.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.time.calendar.exception", label: "Calendar exception", definition: "A public holiday, blackout date, or working-day override attached to a calendar.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.time.working.window", label: "Working window", definition: "A recurring local wall-clock operating window materialized to canonical UTC intervals.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.org.person", label: "Person", definition: "A natural person participating in organizational memberships and employment relationships (implements core.Agent).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.org.unit", label: "Organizational unit", definition: "A branch, department, workshop, or team within an organization (W3C ORG OrganizationalUnit, implements core.Agent).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.org.post", label: "Post", definition: "A budgeted role or position within an organizational unit that can be vacant or filled over time (W3C ORG Post).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.org.membership", label: "Membership", definition: "A time-bounded relationship binding a person to an organizational unit and optional post (W3C ORG Membership, implements core.AgentRole).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.employment", label: "Employment", definition: "Effective-dated employment terms governing an agent's work membership with an employer unit.", termKind: "class", status: "active", semanticAuthority: "semantic_only", parentTermId: "norn.org.membership" },
    { termId: "norn.hr.leave.balance", label: "Leave balance", definition: "A conserved stock of earned leave entitlement days held for an employee (implements core.Resource).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.quantity", label: "Leave quantity", definition: "An exact decimal quantity of leave days allocated, reserved, or consumed in UCUM days.", termKind: "property", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.hr.leave.balance" },
    { termId: "norn.hr.leave.request", label: "Leave request", definition: "A governed request by an employee to consume leave over a date interval.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.request.approve", label: "Approve leave request", definition: "Approve a pending leave request under independent manager authority and debit the conserved leave balance.", termKind: "action", status: "active", semanticAuthority: "semantic_only", domainTermId: "norn.hr.leave.request" },
    { termId: "norn.hr.shift.commitment", label: "Shift commitment", definition: "A promised labor shift assigned to an employee over a half-open UTC interval (implements core.Commitment).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.attendance.event", label: "Attendance event", definition: "An immutable posted work event fulfilling a shift commitment (implements core.EconomicEvent).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.leave.insufficient.balance", label: "Insufficient leave balance", definition: "A leave request exceeds the employee's remaining available leave balance.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.leave.overlap", label: "Overlapping leave request", definition: "A leave request overlaps an existing active or approved leave interval for the same employee.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.shift.overlap", label: "Overlapping shift commitment", definition: "Two scheduled shifts for the same employee overlap in time.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.shift.rest.violation", label: "Minimum rest violation", definition: "The gap between two consecutive shifts for an employee is below the governed statutory rest floor.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.self.approval", label: "Self-approval forbidden", definition: "A requester attempts to approve their own leave or roster exception.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
  ],
  mappings: [
    { mappingId: "map.norn.org.unit.w3c-org", termId: "norn.org.unit", sourceId: "source.w3c-org", externalIri: "https://www.w3.org/ns/org#OrganizationalUnit", relation: "close_match", justification: "W3C ORG org:OrganizationalUnit (canonical RDF IRI http://www.w3.org/ns/org#OrganizationalUnit; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/vocab-org/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.norn.org.post.w3c-org", termId: "norn.org.post", sourceId: "source.w3c-org", externalIri: "https://www.w3.org/ns/org#Post", relation: "close_match", justification: "W3C ORG org:Post (canonical RDF IRI http://www.w3.org/ns/org#Post; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/vocab-org/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.norn.org.membership.w3c-org", termId: "norn.org.membership", sourceId: "source.w3c-org", externalIri: "https://www.w3.org/ns/org#Membership", relation: "close_match", justification: "W3C ORG org:Membership (canonical RDF IRI http://www.w3.org/ns/org#Membership; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/vocab-org/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.norn.time.calendar.w3c-time", termId: "norn.time.calendar", sourceId: "source.w3c-time", externalIri: "https://www.w3.org/2006/time#TRS", relation: "related_match", justification: "OWL-Time time:TRS paired with pinned IANA tzdb 2025b policy (canonical RDF IRI http://www.w3.org/2006/time#TRS; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/owl-time/"], confidenceBasisPoints: 7500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
  ],
  shapes: [
    { shapeId: "shape.norn.time.calendar", termId: "norn.time.calendar", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.time.calendar.exception", termId: "norn.time.calendar.exception", closed: true, fields: [{ fieldId: "calendarDate", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.time.working.window", termId: "norn.time.working.window", closed: true, fields: [{ fieldId: "weekday", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.org.person", termId: "norn.org.person", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.org.unit", termId: "norn.org.unit", closed: true, fields: [{ fieldId: "name", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.org.post", termId: "norn.org.post", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    {
      shapeId: "shape.norn.org.membership",
      termId: "norn.org.membership",
      closed: true,
      fields: [
        { fieldId: "person", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.person" },
        { fieldId: "unit", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.unit" },
      ],
    },
    { shapeId: "shape.norn.hr.employment", termId: "norn.hr.employment", closed: true, fields: [{ fieldId: "person", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.person" }] },
    { shapeId: "shape.norn.hr.leave.balance", termId: "norn.hr.leave.balance", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.leave.request", termId: "norn.hr.leave.request", closed: true, fields: [{ fieldId: "days", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.shift.commitment", termId: "norn.hr.shift.commitment", closed: true, fields: [{ fieldId: "person", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.person" }] },
    { shapeId: "shape.norn.hr.attendance.event", termId: "norn.hr.attendance.event", closed: true, fields: [{ fieldId: "occurredAt", valueType: "date_time", minCount: 1, maxCount: 1 }] },
  ],
})
