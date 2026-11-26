/**
 * Frappe HRMS v16.60.0 -> Norn Workforce Canon v0.2 link (#1592).
 *
 * Maps all 34 core operational Frappe HRMS DocTypes across Shifts, Attendance,
 * Leaves, Payroll, Statutory Tax/Gratuity, Expenses/Advances, Recruitment,
 * Onboarding/Lifecycle, Performance, and Training onto `norn.ontology-pack.workforce` 0.2.0.
 */
import { Norn } from "@norn/authoring"

export const Pack = Norn.packLink({
  id: "norn.link.frappe-hrms.workforce",
  title: "Frappe HRMS v16.60.0 to Norn Workforce Canon v0.2",
  version: "0.1.0",
  fromPack: { packId: "norn.ontology-pack.frappe-hrms", version: "0.1.0" },
  toPack: { packId: "norn.ontology-pack.workforce", version: "0.2.0" },
  correspondences: [
    {
        "correspondenceId": "corr.frappe.hrms.shift.type",
        "fromTermId": "norn.frappe.hrms.shift.type",
        "relation": "subsumed_by",
        "toTermIds": [
            "norn.hr.roster.shift.template"
        ],
        "justification": "Frappe HRMS Shift Type maps cleanly onto Norn Workforce Canon `norn.hr.roster.shift.template`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 8500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.shift.assignment",
        "fromTermId": "norn.frappe.hrms.shift.assignment",
        "relation": "subsumed_by",
        "toTermIds": [
            "norn.hr.shift.commitment"
        ],
        "justification": "Frappe HRMS Shift Assignment maps cleanly onto Norn Workforce Canon `norn.hr.shift.commitment`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 8500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.attendance",
        "fromTermId": "norn.frappe.hrms.attendance",
        "relation": "subsumed_by",
        "toTermIds": [
            "norn.hr.attendance.event"
        ],
        "justification": "Frappe HRMS Attendance maps cleanly onto Norn Workforce Canon `norn.hr.attendance.event`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.employee.checkin",
        "fromTermId": "norn.frappe.hrms.employee.checkin",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.attendance.checkin"
        ],
        "justification": "Frappe HRMS Employee Checkin maps cleanly onto Norn Workforce Canon `norn.hr.attendance.checkin`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.attendance.request",
        "fromTermId": "norn.frappe.hrms.attendance.request",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.attendance.exception"
        ],
        "justification": "Frappe HRMS Attendance Request maps cleanly onto Norn Workforce Canon `norn.hr.attendance.exception`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 8500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.leave.type",
        "fromTermId": "norn.frappe.hrms.leave.type",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.type"
        ],
        "justification": "Frappe HRMS Leave Type maps cleanly onto Norn Workforce Canon `norn.hr.leave.type`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.leave.policy",
        "fromTermId": "norn.frappe.hrms.leave.policy",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.policy"
        ],
        "justification": "Frappe HRMS Leave Policy maps cleanly onto Norn Workforce Canon `norn.hr.leave.policy`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.leave.allocation",
        "fromTermId": "norn.frappe.hrms.leave.allocation",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.allocation"
        ],
        "justification": "Frappe HRMS Leave Allocation maps cleanly onto Norn Workforce Canon `norn.hr.leave.allocation`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.leave.ledger.entry",
        "fromTermId": "norn.frappe.hrms.leave.ledger.entry",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.ledger.entry"
        ],
        "justification": "Frappe HRMS Leave Ledger Entry maps cleanly onto Norn Workforce Canon `norn.hr.leave.ledger.entry`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.leave.application",
        "fromTermId": "norn.frappe.hrms.leave.application",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.request"
        ],
        "justification": "Frappe HRMS Leave Application maps cleanly onto Norn Workforce Canon `norn.hr.leave.request`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.compensatory.leave.request",
        "fromTermId": "norn.frappe.hrms.compensatory.leave.request",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.comp.off"
        ],
        "justification": "Frappe HRMS Compensatory Leave Request maps cleanly onto Norn Workforce Canon `norn.hr.leave.comp.off`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.leave.encashment",
        "fromTermId": "norn.frappe.hrms.leave.encashment",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.leave.encashment"
        ],
        "justification": "Frappe HRMS Leave Encashment maps cleanly onto Norn Workforce Canon `norn.hr.leave.encashment`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.salary.component",
        "fromTermId": "norn.frappe.hrms.salary.component",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.payroll.component"
        ],
        "justification": "Frappe HRMS Salary Component maps cleanly onto Norn Workforce Canon `norn.hr.payroll.component`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.salary.structure",
        "fromTermId": "norn.frappe.hrms.salary.structure",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.payroll.structure"
        ],
        "justification": "Frappe HRMS Salary Structure maps cleanly onto Norn Workforce Canon `norn.hr.payroll.structure`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.salary.structure.assignment",
        "fromTermId": "norn.frappe.hrms.salary.structure.assignment",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.payroll.assignment"
        ],
        "justification": "Frappe HRMS Salary Structure Assignment maps cleanly onto Norn Workforce Canon `norn.hr.payroll.assignment`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.payroll.entry",
        "fromTermId": "norn.frappe.hrms.payroll.entry",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.payroll.run"
        ],
        "justification": "Frappe HRMS Payroll Entry maps cleanly onto Norn Workforce Canon `norn.hr.payroll.run`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.salary.slip",
        "fromTermId": "norn.frappe.hrms.salary.slip",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.payroll.payslip"
        ],
        "justification": "Frappe HRMS Salary Slip maps cleanly onto Norn Workforce Canon `norn.hr.payroll.payslip`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.additional.salary",
        "fromTermId": "norn.frappe.hrms.additional.salary",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.payroll.adjustment"
        ],
        "justification": "Frappe HRMS Additional Salary maps cleanly onto Norn Workforce Canon `norn.hr.payroll.adjustment`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.income.tax.slab",
        "fromTermId": "norn.frappe.hrms.income.tax.slab",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.statutory.tax.slab"
        ],
        "justification": "Frappe HRMS Income Tax Slab maps cleanly onto Norn Workforce Canon `norn.hr.statutory.tax.slab`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.gratuity",
        "fromTermId": "norn.frappe.hrms.gratuity",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.statutory.gratuity"
        ],
        "justification": "Frappe HRMS Gratuity maps cleanly onto Norn Workforce Canon `norn.hr.statutory.gratuity`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.expense.claim",
        "fromTermId": "norn.frappe.hrms.expense.claim",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.expense.claim"
        ],
        "justification": "Frappe HRMS Expense Claim maps cleanly onto Norn Workforce Canon `norn.hr.expense.claim`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.employee.advance",
        "fromTermId": "norn.frappe.hrms.employee.advance",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.expense.advance"
        ],
        "justification": "Frappe HRMS Employee Advance maps cleanly onto Norn Workforce Canon `norn.hr.expense.advance`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.job.opening",
        "fromTermId": "norn.frappe.hrms.job.opening",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.talent.job.opening"
        ],
        "justification": "Frappe HRMS Job Opening maps cleanly onto Norn Workforce Canon `norn.hr.talent.job.opening`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.job.applicant",
        "fromTermId": "norn.frappe.hrms.job.applicant",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.talent.applicant"
        ],
        "justification": "Frappe HRMS Job Applicant maps cleanly onto Norn Workforce Canon `norn.hr.talent.applicant`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.interview",
        "fromTermId": "norn.frappe.hrms.interview",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.talent.interview"
        ],
        "justification": "Frappe HRMS Interview maps cleanly onto Norn Workforce Canon `norn.hr.talent.interview`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.job.offer",
        "fromTermId": "norn.frappe.hrms.job.offer",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.talent.job.offer"
        ],
        "justification": "Frappe HRMS Job Offer maps cleanly onto Norn Workforce Canon `norn.hr.talent.job.offer`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.employee.onboarding",
        "fromTermId": "norn.frappe.hrms.employee.onboarding",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.talent.onboarding"
        ],
        "justification": "Frappe HRMS Employee Onboarding maps cleanly onto Norn Workforce Canon `norn.hr.talent.onboarding`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.employee.transfer",
        "fromTermId": "norn.frappe.hrms.employee.transfer",
        "relation": "subsumed_by",
        "toTermIds": [
            "norn.hr.talent.employment.change"
        ],
        "justification": "Frappe HRMS Employee Transfer maps cleanly onto Norn Workforce Canon `norn.hr.talent.employment.change`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 8500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.employee.promotion",
        "fromTermId": "norn.frappe.hrms.employee.promotion",
        "relation": "subsumed_by",
        "toTermIds": [
            "norn.hr.talent.employment.change"
        ],
        "justification": "Frappe HRMS Employee Promotion maps cleanly onto Norn Workforce Canon `norn.hr.talent.employment.change`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 8500,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.employee.separation",
        "fromTermId": "norn.frappe.hrms.employee.separation",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.talent.separation"
        ],
        "justification": "Frappe HRMS Employee Separation maps cleanly onto Norn Workforce Canon `norn.hr.talent.separation`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.appraisal",
        "fromTermId": "norn.frappe.hrms.appraisal",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.performance.appraisal"
        ],
        "justification": "Frappe HRMS Appraisal maps cleanly onto Norn Workforce Canon `norn.hr.performance.appraisal`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.goal",
        "fromTermId": "norn.frappe.hrms.goal",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.performance.goal"
        ],
        "justification": "Frappe HRMS Goal maps cleanly onto Norn Workforce Canon `norn.hr.performance.goal`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.training.event",
        "fromTermId": "norn.frappe.hrms.training.event",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.training.event"
        ],
        "justification": "Frappe HRMS Training Event maps cleanly onto Norn Workforce Canon `norn.hr.training.event`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    },
    {
        "correspondenceId": "corr.frappe.hrms.training.result",
        "fromTermId": "norn.frappe.hrms.training.result",
        "relation": "equivalent",
        "toTermIds": [
            "norn.hr.training.result"
        ],
        "justification": "Frappe HRMS Training Result maps cleanly onto Norn Workforce Canon `norn.hr.training.result`.",
        "evidenceRefs": [
            "source.frappe-hrms"
        ],
        "confidenceBasisPoints": 9000,
        "reviewedBy": "hr-vertical (agent, proposed)",
        "reviewedAt": "2026-11-26T00:00:00Z"
    }
],
  residuals: [],
})
