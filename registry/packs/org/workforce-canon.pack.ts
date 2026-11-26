/**
 * Norn Workforce, Organization, Time & Full HR Canonical Ontology Pack v0.2 (#1591).
 *
 * Modular sibling to `norn.ontology-pack.canon` (commerce/finance REA canon) so
 * commerce/finance cell digests remain undisturbed while time, org, and full HR
 * system packs (e.g. Frappe HRMS in #1592, Soliman HR, Egypt/GCC/EU statutory packs)
 * project onto canonical workforce terms anchored in off-the-shelf standards:
 *   - W3C ORG 0.8 (OrganizationalUnit, Post, Membership)
 *   - W3C OWL-Time 2016 + IANA tzdb + ISO 8601 (Calendar, WorkingWindow, TimeInterval)
 *   - W3C Data Privacy Vocabulary (DPV) 2.1 (canonical HR PII markings)
 *   - HR Open Standards 4.5R (Worker, Timecard, Payroll, Recruiting)
 *   - ISO/IEC 15944-4 REA core (`core.Agent`, `core.Resource`, `core.Commitment`, `core.EconomicEvent`, `core.Claim`)
 *   - ISO 20022 `pain.001` / `camt.053` + `canon.Settlement` (payout disbursement)
 */
import { Norn } from "@norn/authoring"

const DPV_REVIEWED_AT = "2026-11-26T00:00:00Z"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.workforce",
  title: "Norn workforce canon handles: time, organization, full HR, payroll, talent, and W3C DPV PII primitives",
  version: "0.2.0",
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
    {
      sourceId: "source.w3c-dpv",
      releaseId: "2.1@2025-03-16",
      releasedAt: "2025-03-16",
      retrievedAt: "2026-11-26",
      canonicalUrl: "https://w3id.org/dpv/2.1/",
      license: "W3C-20150513",
      redistribution: "reference_only",
      importMode: "map_only",
      files: [
        { path: "https://raw.githubusercontent.com/w3c-cg/dpv/master/2.1/dpv/dpv.ttl", sha256: "bb264ef1e753e789fefb22b67d51a1dcd37879a00649644bf246830aa431c9a3" },
        { path: "https://raw.githubusercontent.com/w3c-cg/dpv/master/2.1/pd/pd.ttl", sha256: "f4a5c99cdb6f0830960b343e1b88de3cd05f8ff54e222fb1d08d83f493cc2306" },
      ],
    },
    {
      sourceId: "source.hr-open-standards",
      releaseId: "4.5.0",
      releasedAt: "2025-05-09",
      retrievedAt: "2026-11-26",
      canonicalUrl: "https://www.hropenstandards.org/documentation",
      license: "LicenseRef-HR-Open-Standards-License-Agreement",
      redistribution: "reference_only",
      importMode: "map_only",
      files: [
        { path: "https://hropentech.org/4.5R/schema/hros-timecard-dev/json/TimecardType.json", sha256: "4f6c5b667d2012207e3784e3c5c1419e775e2891f797d73f6fb8c92dcfe738c2" },
        { path: "https://hropentech.org/4.5R/schema/hros-payroll-dev/json/PayrollType.json", sha256: "a24053cfa46b790c0163c722140ffb1e43bb526e36c012584b009a57af58997d" },
        { path: "https://hropentech.org/4.5R/schema/hros-recruiting-dev/json/PositionOpeningType.json", sha256: "4474c71210bdc014ec983163141ff4f870de784e48e159a8517d17c40a631b3b" },
        { path: "https://hropentech.org/4.5R/schema/hros-common-dev/json/work/WorkerType.json", sha256: "a8272ca777993045dd19c848ddac38188e7cea0b667407814cc1571d87e69ebf" },
      ],
    },
  ],
  terms: [
    // --- v0.1 Foundation (Time, W3C ORG, Core HR — preserved verbatim) ---
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
    { termId: "norn.hr.guard.self.approval", label: "Self-approval forbidden", definition: "A requester attempts to approve their own leave, payroll, expense, or roster exception.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 1: Payroll, Compensation & ISO 20022 Disbursement ---
    { termId: "norn.hr.payroll.component", label: "Pay component", definition: "A typed earning, deduction, tax withholding, or employer statutory contribution rule (HR Open Payroll).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.structure", label: "Compensation structure", definition: "A named template composing pay components and calculation formulas for an employee grade or cohort.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.assignment", label: "Compensation assignment", definition: "Effective-dated assignment binding an employee to a compensation structure and base pay rate.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.period", label: "Payroll period", definition: "A closed accounting and pay calendar interval governing payslip accrual and cutoffs.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.run", label: "Payroll run", definition: "A governed batch cycle calculating, approving under SoD, and disbursing payslips for a payroll period.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.payslip", label: "Payslip", definition: "An employee pay statement for one payroll period (implements canon.ItemizedDocument; HR Open Payroll).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.payslip.line", label: "Payslip line", definition: "One itemized earning, deduction, tax, or benefit line on a payslip (implements canon.LineItem).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.adjustment", label: "Off-cycle payroll adjustment", definition: "An approved bonus, penalty, allowance, or salary advance adjustment applied to a payroll period.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.payroll.disbursement", label: "Payout disbursement", definition: "A receipt-authenticated salary or claim transfer instruction settling a payslip (implements canon.Settlement; ISO 20022 pain.001 / camt.053).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 2: Leave Policy & Conserved Accrual Ledgers ---
    { termId: "norn.hr.leave.type", label: "Leave type", definition: "A governed category of leave (annual, sick, unpaid, maternity, compensatory) with carry-forward and encashment flags.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.policy", label: "Leave policy", definition: "Statutory and company entitlement rules assigning annual leave allocations by tenure or grade.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.allocation", label: "Leave allocation", definition: "A periodic grant crediting a conserved leave balance for an employee over a leave period.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.ledger.entry", label: "Leave ledger entry", definition: "An immutable conserved credit or debit posting on an employee leave balance (implements core.LedgerEffect).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.comp.off", label: "Compensatory leave request", definition: "A request converting verified holiday or rest-day work into a conserved leave balance credit.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.leave.encashment", label: "Leave encashment", definition: "A governed conversion of unused encashable leave balance into a payroll earning adjustment.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 3: Rostering, Shift Templates & Attendance Intake ---
    { termId: "norn.hr.roster.shift.template", label: "Shift template", definition: "A reusable local wall-clock shift definition with optional overnight end-day offset (extends norn.time.working.window).", termKind: "class", status: "active", semanticAuthority: "semantic_only", parentTermId: "norn.time.working.window" },
    { termId: "norn.hr.roster.schedule", label: "Roster schedule", definition: "A multi-day branch schedule lifecycle combining solver proposals (FunctionReceipt) and manager overrides.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.roster.constraint", label: "Scheduling constraint", definition: "A declarative hard or soft rostering constraint (coverage floor, safe departure, availability, pairing, rest floor).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.attendance.checkin", label: "Check-in observation", definition: "A raw kiosk, biometric, or mobile punch observation prior to reconciliation into a posted attendance event.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.attendance.exception", label: "Attendance exception", definition: "A governed correction or excuse request for missed punches, lateness, or work-from-home days.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 4: Expenses, Advances & Employee Loans ---
    { termId: "norn.hr.expense.advance", label: "Employee advance", definition: "Cash advanced to an employee pending reconciliation against expense claims or payroll recovery (implements core.Claim).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.expense.claim", label: "Expense claim", definition: "An employee reimbursement claim backed by receipts and independent manager approval (implements canon.ItemizedDocument).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.expense.claim.line", label: "Expense claim line", definition: "One categorized receipt-backed expense item on an expense claim (implements canon.LineItem).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.loan.account", label: "Employee loan", definition: "A conserved principal balance lent to an employee with scheduled payroll withholding (implements core.Claim).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.loan.repayment", label: "Loan repayment schedule", definition: "Scheduled installment postings reducing an employee loan principal via payroll deduction or direct settlement.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 5: Talent Lifecycle (Recruiting -> Onboarding -> Separation) ---
    { termId: "norn.hr.talent.job.opening", label: "Job opening", definition: "An approved vacancy recruiting against a budgeted org.Post (HR Open PositionOpening).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.applicant", label: "Job applicant", definition: "A candidate applying to one or more job openings with PII-classified contact and CV records.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.interview", label: "Interview", definition: "A scheduled structured evaluation event between interviewers and a job applicant.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.job.offer", label: "Job offer", definition: "A formal conditional employment and compensation offer issued to a selected applicant.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.onboarding", label: "Employee onboarding", definition: "A governed checklist plan transitioning an accepted offer into an active org.Membership.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.employment.change", label: "Employment change", definition: "An effective-dated transfer, branch reassignment, or promotion updating org.Membership and compensation.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.separation", label: "Employee separation", definition: "An offboarding and contract termination workflow closing active memberships and access.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.talent.final.settlement", label: "Full and final settlement", definition: "Terminal payout combining prorated salary, statutory gratuity, leave encashment, and loan recovery.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 6: Skills, Performance & Training (ESCO / ISCO-08 / O*NET / ISO 30414) ---
    { termId: "norn.hr.skill", label: "Skill or qualification", definition: "A canonical occupation skill, license, or safety certification (aligns with ESCO / ISCO-08 / O*NET).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.employee.skill", label: "Employee skill attestation", definition: "An attested proficiency or time-bounded certification held by an employee (used by skill-qualified rostering).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.performance.goal", label: "Performance goal", definition: "A measurable key result area or objective assigned to an employee for an appraisal cycle.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.performance.appraisal", label: "Performance appraisal", definition: "A periodic performance review evaluating goals and competencies (ISO 30414 human capital reporting).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.training.event", label: "Training event", definition: "A scheduled learning or compliance training session for employees.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.training.result", label: "Training result", definition: "An employee completion record and assessment outcome for a training event.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Slice 7: Statutory / Jurisdiction Overlay & W3C DPV 2.1 HR PII Markings ---
    { termId: "norn.hr.statutory.tax.slab", label: "Income tax slab", definition: "Jurisdiction-scoped progressive income tax brackets and exemption rules (ISO 3166-1 country overlay).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.statutory.gratuity", label: "Statutory gratuity", definition: "End-of-service indemnity calculation governed by national labor statute.", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.marking.pii.compensation", label: "HR PII: Compensation", definition: "Canonical data-classification marking for salary, bonus, bank account, and payslip amounts (W3C DPV 2.1 dpv-pd:Salary).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.marking.pii.gov.id", label: "HR PII: Government ID", definition: "Canonical marking for national ID, passport, tax ID, and social insurance numbers (W3C DPV 2.1 dpv-pd:OfficialID).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.marking.pii.demographic", label: "HR PII: Demographic", definition: "Canonical marking for personal phone, home address, gender, and date of birth (W3C DPV 2.1 dpv-pd:Demographic).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.marking.pii.biometric", label: "HR PII: Biometric", definition: "Canonical marking for kiosk selfie hashes, face/fingerprint templates (W3C DPV 2.1 dpv-pd:Biometric).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.marking.pii.medical", label: "HR PII: Medical and sick leave", definition: "Canonical marking for sick-leave certificates and occupational health records (W3C DPV 2.1 dpv-pd:Health).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.marking.pii.performance", label: "HR PII: Performance and disciplinary", definition: "Canonical marking for appraisal scores and disciplinary records (W3C DPV 2.1 dpv-pd:JobPerformance).", termKind: "class", status: "active", semanticAuthority: "semantic_only" },

    // --- Additional Canonical HR Guards ---
    { termId: "norn.hr.guard.payroll.unbalanced", label: "Payslip arithmetic conservation violated", definition: "Payslip net pay does not equal gross earnings minus lawful deductions.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.payout.unauthenticated", label: "Unauthenticated payout settlement", definition: "A payout settlement transition lacks a verified FunctionReceipt from a pinned payment performer.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.negative.net.pay", label: "Negative net pay forbidden", definition: "Payroll deductions exceed gross earnings without clamping net pay to zero and carrying forward debt.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
    { termId: "norn.hr.guard.safe.departure.violation", label: "Safe departure cutoff exceeded", definition: "A scheduled shift finishes past an employee's active safe-departure local time ceiling.", termKind: "predicate", status: "active", semanticAuthority: "semantic_only" },
  ],
  mappings: [
    // W3C ORG & OWL-Time (preserved from v0.1)
    { mappingId: "map.norn.org.unit.w3c-org", termId: "norn.org.unit", sourceId: "source.w3c-org", externalIri: "https://www.w3.org/ns/org#OrganizationalUnit", relation: "close_match", justification: "W3C ORG org:OrganizationalUnit (canonical RDF IRI http://www.w3.org/ns/org#OrganizationalUnit; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/vocab-org/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.norn.org.post.w3c-org", termId: "norn.org.post", sourceId: "source.w3c-org", externalIri: "https://www.w3.org/ns/org#Post", relation: "close_match", justification: "W3C ORG org:Post (canonical RDF IRI http://www.w3.org/ns/org#Post; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/vocab-org/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.norn.org.membership.w3c-org", termId: "norn.org.membership", sourceId: "source.w3c-org", externalIri: "https://www.w3.org/ns/org#Membership", relation: "close_match", justification: "W3C ORG org:Membership (canonical RDF IRI http://www.w3.org/ns/org#Membership; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/vocab-org/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },
    { mappingId: "map.norn.time.calendar.w3c-time", termId: "norn.time.calendar", sourceId: "source.w3c-time", externalIri: "https://www.w3.org/2006/time#TRS", relation: "related_match", justification: "OWL-Time time:TRS paired with pinned IANA tzdb policy (canonical RDF IRI http://www.w3.org/2006/time#TRS; HTTPS form required by Norn ontology_pack checker).", evidenceRefs: ["https://www.w3.org/TR/owl-time/"], confidenceBasisPoints: 7500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: "2026-11-23T00:00:00Z" },

    // W3C Data Privacy Vocabulary (DPV) 2.1 — canonical HR PII markings
    { mappingId: "map.norn.hr.pii.compensation.dpv", termId: "norn.hr.marking.pii.compensation", sourceId: "source.w3c-dpv", externalIri: "https://w3id.org/dpv/pd#Salary", relation: "close_match", justification: "W3C DPV 2.1 Personal Data extension class dpv-pd:Salary covers salary, compensation, and pay amounts.", evidenceRefs: ["https://w3id.org/dpv/2.1/pd/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.pii.gov.id.dpv", termId: "norn.hr.marking.pii.gov.id", sourceId: "source.w3c-dpv", externalIri: "https://w3id.org/dpv/pd#OfficialID", relation: "exact_match", justification: "W3C DPV 2.1 dpv-pd:OfficialID covers national IDs, passports, and government-issued identifiers.", evidenceRefs: ["https://w3id.org/dpv/2.1/pd/"], confidenceBasisPoints: 9500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.pii.demographic.dpv", termId: "norn.hr.marking.pii.demographic", sourceId: "source.w3c-dpv", externalIri: "https://w3id.org/dpv/pd#Demographic", relation: "exact_match", justification: "W3C DPV 2.1 dpv-pd:Demographic covers demographic attributes such as gender, birth date, and contact metadata.", evidenceRefs: ["https://w3id.org/dpv/2.1/pd/"], confidenceBasisPoints: 9500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.pii.biometric.dpv", termId: "norn.hr.marking.pii.biometric", sourceId: "source.w3c-dpv", externalIri: "https://w3id.org/dpv/pd#Biometric", relation: "exact_match", justification: "W3C DPV 2.1 dpv-pd:Biometric covers biometric templates and kiosk verification photos.", evidenceRefs: ["https://w3id.org/dpv/2.1/pd/"], confidenceBasisPoints: 9500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.pii.medical.dpv", termId: "norn.hr.marking.pii.medical", sourceId: "source.w3c-dpv", externalIri: "https://w3id.org/dpv/pd#Health", relation: "close_match", justification: "W3C DPV 2.1 dpv-pd:Health covers employee sick leave documentation and medical fitness records.", evidenceRefs: ["https://w3id.org/dpv/2.1/pd/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.pii.performance.dpv", termId: "norn.hr.marking.pii.performance", sourceId: "source.w3c-dpv", externalIri: "https://w3id.org/dpv/pd#PerformanceAtWork", relation: "close_match", justification: "W3C DPV 2.1 dpv-pd:PerformanceAtWork covers employee appraisals, ratings, and disciplinary records.", evidenceRefs: ["https://w3id.org/dpv/2.1/pd/"], confidenceBasisPoints: 9000, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },

    // HR Open Standards 4.5R
    { mappingId: "map.norn.hr.employment.hropen", termId: "norn.hr.employment", sourceId: "source.hr-open-standards", externalIri: "https://hropentech.org/4.5R/schema/hros-common-dev/json/work/WorkerType.json", relation: "close_match", justification: "HR Open 4.5R WorkerType captures employment relationship, assignment, and status metadata.", evidenceRefs: ["https://www.hropenstandards.org/documentation"], confidenceBasisPoints: 8500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.attendance.event.hropen", termId: "norn.hr.attendance.event", sourceId: "source.hr-open-standards", externalIri: "https://hropentech.org/4.5R/schema/hros-timecard-dev/json/TimecardType.json", relation: "close_match", justification: "HR Open 4.5R TimecardType captures worked time intervals, allowances, and shift reconciliation.", evidenceRefs: ["https://www.hropenstandards.org/documentation"], confidenceBasisPoints: 8500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.payroll.payslip.hropen", termId: "norn.hr.payroll.payslip", sourceId: "source.hr-open-standards", externalIri: "https://hropentech.org/4.5R/schema/hros-payroll-dev/json/PayrollType.json", relation: "close_match", justification: "HR Open 4.5R PayrollType captures payroll earnings, deductions, withholdings, and net pay instructions.", evidenceRefs: ["https://www.hropenstandards.org/documentation"], confidenceBasisPoints: 8500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
    { mappingId: "map.norn.hr.talent.job.opening.hropen", termId: "norn.hr.talent.job.opening", sourceId: "source.hr-open-standards", externalIri: "https://hropentech.org/4.5R/schema/hros-recruiting-dev/json/PositionOpeningType.json", relation: "close_match", justification: "HR Open 4.5R PositionOpeningType captures job requisitions, posting profiles, and position requirements.", evidenceRefs: ["https://www.hropenstandards.org/documentation"], confidenceBasisPoints: 8500, reviewedBy: "hr-vertical (agent, proposed)", reviewedAt: DPV_REVIEWED_AT },
  ],
  shapes: [
    // v0.1 shapes (preserved verbatim)
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

    // v0.2 shapes (key operational aggregates)
    {
      shapeId: "shape.norn.hr.payroll.payslip",
      termId: "norn.hr.payroll.payslip",
      closed: true,
      fields: [
        { fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" },
        { fieldId: "period", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.payroll.period" },
        { fieldId: "netPay", valueType: "decimal", minCount: 1, maxCount: 1 },
      ],
    },
    {
      shapeId: "shape.norn.hr.payroll.disbursement",
      termId: "norn.hr.payroll.disbursement",
      closed: true,
      fields: [
        { fieldId: "payslip", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.payroll.payslip" },
        { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 },
        { fieldId: "currency", valueType: "string", minCount: 1, maxCount: 1 },
      ],
    },
    {
      shapeId: "shape.norn.hr.leave.ledger.entry",
      termId: "norn.hr.leave.ledger.entry",
      closed: true,
      fields: [
        { fieldId: "balance", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.leave.balance" },
        { fieldId: "deltaDays", valueType: "decimal", minCount: 1, maxCount: 1 },
      ],
    },
    {
      shapeId: "shape.norn.hr.roster.schedule",
      termId: "norn.hr.roster.schedule",
      closed: true,
      fields: [
        { fieldId: "unit", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.unit" },
        { fieldId: "startDate", valueType: "string", minCount: 1, maxCount: 1 },
      ],
    },
    {
      shapeId: "shape.norn.hr.expense.claim",
      termId: "norn.hr.expense.claim",
      closed: true,
      fields: [
        { fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" },
        { fieldId: "totalAmount", valueType: "decimal", minCount: 1, maxCount: 1 },
      ],
    },
    {
      shapeId: "shape.norn.hr.talent.job.opening",
      termId: "norn.hr.talent.job.opening",
      closed: true,
      fields: [
        { fieldId: "post", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.post" },
        { fieldId: "title", valueType: "string", minCount: 1, maxCount: 1 },
      ],
    },
    // Remaining v0.2 closed promotion shapes (one per active class term)
    { shapeId: "shape.norn.hr.payroll.component", termId: "norn.hr.payroll.component", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.payroll.structure", termId: "norn.hr.payroll.structure", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.payroll.assignment", termId: "norn.hr.payroll.assignment", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.payroll.period", termId: "norn.hr.payroll.period", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.payroll.run", termId: "norn.hr.payroll.run", closed: true, fields: [{ fieldId: "period", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.payroll.period" }] },
    { shapeId: "shape.norn.hr.payroll.payslip.line", termId: "norn.hr.payroll.payslip.line", closed: true, fields: [{ fieldId: "component", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.payroll.component" }, { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.payroll.adjustment", termId: "norn.hr.payroll.adjustment", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.leave.type", termId: "norn.hr.leave.type", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.leave.policy", termId: "norn.hr.leave.policy", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.leave.allocation", termId: "norn.hr.leave.allocation", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "days", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.leave.comp.off", termId: "norn.hr.leave.comp.off", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "days", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.leave.encashment", termId: "norn.hr.leave.encashment", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "days", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.roster.shift.template", termId: "norn.hr.roster.shift.template", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.roster.constraint", termId: "norn.hr.roster.constraint", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.attendance.checkin", termId: "norn.hr.attendance.checkin", closed: true, fields: [{ fieldId: "occurredAt", valueType: "date_time", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.attendance.exception", termId: "norn.hr.attendance.exception", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.expense.advance", termId: "norn.hr.expense.advance", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.expense.claim.line", termId: "norn.hr.expense.claim.line", closed: true, fields: [{ fieldId: "claim", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.expense.claim" }, { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.loan.account", termId: "norn.hr.loan.account", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "principal", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.loan.repayment", termId: "norn.hr.loan.repayment", closed: true, fields: [{ fieldId: "loan", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.loan.account" }, { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.talent.applicant", termId: "norn.hr.talent.applicant", closed: true, fields: [{ fieldId: "person", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.person" }] },
    { shapeId: "shape.norn.hr.talent.interview", termId: "norn.hr.talent.interview", closed: true, fields: [{ fieldId: "applicant", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.talent.applicant" }] },
    { shapeId: "shape.norn.hr.talent.job.offer", termId: "norn.hr.talent.job.offer", closed: true, fields: [{ fieldId: "applicant", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.talent.applicant" }] },
    { shapeId: "shape.norn.hr.talent.onboarding", termId: "norn.hr.talent.onboarding", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.talent.employment.change", termId: "norn.hr.talent.employment.change", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.talent.separation", termId: "norn.hr.talent.separation", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.talent.final.settlement", termId: "norn.hr.talent.final.settlement", closed: true, fields: [{ fieldId: "separation", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.talent.separation" }, { fieldId: "netAmount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.skill", termId: "norn.hr.skill", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.employee.skill", termId: "norn.hr.employee.skill", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "skill", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.skill" }] },
    { shapeId: "shape.norn.hr.performance.goal", termId: "norn.hr.performance.goal", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.performance.appraisal", termId: "norn.hr.performance.appraisal", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.training.event", termId: "norn.hr.training.event", closed: true, fields: [{ fieldId: "title", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.training.result", termId: "norn.hr.training.result", closed: true, fields: [{ fieldId: "event", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.training.event" }, { fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }] },
    { shapeId: "shape.norn.hr.statutory.tax.slab", termId: "norn.hr.statutory.tax.slab", closed: true, fields: [{ fieldId: "code", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.statutory.gratuity", termId: "norn.hr.statutory.gratuity", closed: true, fields: [{ fieldId: "employment", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.hr.employment" }, { fieldId: "amount", valueType: "decimal", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.marking.pii.compensation", termId: "norn.hr.marking.pii.compensation", closed: true, fields: [{ fieldId: "marking", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.marking.pii.gov.id", termId: "norn.hr.marking.pii.gov.id", closed: true, fields: [{ fieldId: "marking", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.marking.pii.demographic", termId: "norn.hr.marking.pii.demographic", closed: true, fields: [{ fieldId: "marking", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.marking.pii.biometric", termId: "norn.hr.marking.pii.biometric", closed: true, fields: [{ fieldId: "marking", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.marking.pii.medical", termId: "norn.hr.marking.pii.medical", closed: true, fields: [{ fieldId: "marking", valueType: "string", minCount: 1, maxCount: 1 }] },
    { shapeId: "shape.norn.hr.marking.pii.performance", termId: "norn.hr.marking.pii.performance", closed: true, fields: [{ fieldId: "marking", valueType: "string", minCount: 1, maxCount: 1 }] },
  ],
})
