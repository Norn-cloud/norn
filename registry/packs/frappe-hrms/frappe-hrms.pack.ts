/**
 * Frappe HRMS v16.60.0 system pack (map-only, reference-only, GPL-3.0-only, #1592).
 *
 * Pins the exact upstream DocType schema files of `frappe/hrms` without copying
 * any GPL code, providing a machine-checked parity checklist against
 * `norn.ontology-pack.workforce` v0.2.
 */
import { Norn } from "@norn/authoring"

export const Pack = Norn.ontologyPack({
  id: "norn.ontology-pack.frappe-hrms",
  title: "Frappe HRMS v16.60.0 system pack (map-only)",
  version: "0.1.0",
  sources: [
    {
      sourceId: "source.frappe-hrms",
      releaseId: "version-16@1b2d7e034e0de8e30a9e513541d7741710587ad5",
      releasedAt: "2026-11-17",
      retrievedAt: "2026-11-26",
      canonicalUrl: "https://github.com/frappe/hrms/tree/1b2d7e034e0de8e30a9e513541d7741710587ad5",
      license: "GPL-3.0-only",
      redistribution: "reference_only",
      importMode: "map_only",
      importer: { name: "norn.importers.frappe", version: "0.1.0" },
      files: [
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/license.txt",
                "sha256": "f333043685c88280b1a0a41b4f8e2eacb02079f0bfca4d467e52c8834c658cea"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/shift_type/shift_type.json",
                "sha256": "4af18694c98cd04da647a5fd2eff2f46d171ca8f55a22078058eeb124459555f"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/shift_assignment/shift_assignment.json",
                "sha256": "16545511be592adf4d1fc08b6142b1b9a5bf945e0c18e51a1a0f6cffbaf46359"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/attendance/attendance.json",
                "sha256": "f0c0d68b0842a00cd87cb88e40a5b6f024e397674bd0768d39b64d90109da3c2"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_checkin/employee_checkin.json",
                "sha256": "4b0e47b0aa8a5ab3b1821814d2085caad95513c69c00b68da81f2ae47f1a9df4"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/attendance_request/attendance_request.json",
                "sha256": "8c4c0563b4b06a4f5d271a1054893ed67d18d3744579d57932edbc94a3cb8d18"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_type/leave_type.json",
                "sha256": "931d8260a00ea585c6c71318e23607d2062dd5c33d59c32846d386e230431f4d"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_policy/leave_policy.json",
                "sha256": "9127866939210f1497bf17f7797d19526aa91e4d62a7c1041d4cc70366d2226b"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_allocation/leave_allocation.json",
                "sha256": "754feb486e8e36cb83b6cf82c9263cbb8ecce2747d7b57ef00d8da86b2cd08a6"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_ledger_entry/leave_ledger_entry.json",
                "sha256": "4f4c5f2255ee7e2cbb9c5a32f743a9edf564245e53cbc863cdc8450d8cfb7b4e"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_application/leave_application.json",
                "sha256": "f7a4fb7c74c4b734777ce81962b3ca3cafe7691f34245098d481dfdb7d5b14b7"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/compensatory_leave_request/compensatory_leave_request.json",
                "sha256": "202850b3391046e3d9ffc6dc8b3b1d05d3722bfcd987ff0e38544094902ea916"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_encashment/leave_encashment.json",
                "sha256": "e2a29826fdfb68132f707a2ba3bfe976b97746875f1073cd59374042dd05426e"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_component/salary_component.json",
                "sha256": "88dc5429f35fa057d1e56f32c60c33033e7fcab922669fb8a029c7a6d81cac95"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_structure/salary_structure.json",
                "sha256": "b657dbf58069fa213ce5a33c5b0b8d056b618598e2908696fc49b46720384184"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_structure_assignment/salary_structure_assignment.json",
                "sha256": "8222037587736271aa6ee9f90cd03f9ce06c2373fb2180ae334d6f4d025bdef5"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/payroll_entry/payroll_entry.json",
                "sha256": "8f30568a6b41e0ae085f440dbc1ed2d6fc3a066152a51d3efb1ab1cf6b9b07ac"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_slip/salary_slip.json",
                "sha256": "9b1812e99a28f5c51e268bb7fa3e3f287ad6a3c40230bc81ae4b55a8b208668d"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/additional_salary/additional_salary.json",
                "sha256": "dc295e9b919062f9f3ae6700a52edf9f53c0a69bb6c840b39e3d093165916890"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/income_tax_slab/income_tax_slab.json",
                "sha256": "e528a215a3773eab2699e0ff75a42c2112d2f6a5e1c2a4a1e9c435050b60dac5"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/gratuity/gratuity.json",
                "sha256": "60f0e5c3c7e0a5b7c72a028c47ce77cdc328dcbf7f2dad92a6a860612ae82bb6"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/expense_claim/expense_claim.json",
                "sha256": "db60cac06528e99ec4aa4f12237bf198f44bd23840182f1ebc853513ae52e226"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_advance/employee_advance.json",
                "sha256": "ea5c0c391c1288e0406ed778e4cb6164fa66b67ec93563be2e921f17631a07dd"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_opening/job_opening.json",
                "sha256": "e7b9f67b71be234e6e46a257850bd2a5070c03581cd79347397164f3f6fca533"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_applicant/job_applicant.json",
                "sha256": "95e6f069fc6ec363c04bb2b394ab2ddbab975ad7669937f2e9114cff26fd7898"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/interview/interview.json",
                "sha256": "1956d2d0095a573dd83574a68d967b9ade345ad5f136d0c3302b491bbd8991ff"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_offer/job_offer.json",
                "sha256": "dd924aedf4d1efe1b81441963ee7503e4b39edea649f68bfd2753687d9aad1ee"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_onboarding/employee_onboarding.json",
                "sha256": "8695826fa007ea016915c9d59352e89d0b551be5271784ea21c5eec19264b5d5"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_transfer/employee_transfer.json",
                "sha256": "44cd339fb132a591ede55499bd74a1f0be76e35a6100c09d6b9e80b0f5acd96f"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_promotion/employee_promotion.json",
                "sha256": "827fc7630a185556cf825885f2c5a4d890f37287b86181ccefdbda8b7055c06c"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_separation/employee_separation.json",
                "sha256": "3cbbbcac1cc7302b9df4bf3e7b6971d07c93c96953149aa8c2a36fa4e94669bb"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/appraisal/appraisal.json",
                "sha256": "93f31f63fa8ad7eebe101cd03cc659d6a7f02b8a51c2675b978f688f4f2513c8"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/goal/goal.json",
                "sha256": "7b1d9f91c9081807fb553a5a56305ede0ccfb2138669162fcdfbde4ad6cebf5a"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/training_event/training_event.json",
                "sha256": "1dc1e5f18aa4718a8c256ec3953e9bd777c008707335329c160d56231b0d4ef2"
        },
        {
                "path": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/training_result/training_result.json",
                "sha256": "d53db07ff1ff0af60203474da3cd65bad00876795e339ffba318bfdadd0909ce"
        }
],
    },
  ],
  terms: [
    {
        "termId": "norn.frappe.hrms.shift.type",
        "label": "Shift Type",
        "definition": "Frappe HRMS Shift Type DocType defining recurring shift start/end times and grace rules.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.shift.assignment",
        "label": "Shift Assignment",
        "definition": "Frappe HRMS Shift Assignment DocType assigning an employee to a shift type over a date range.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.attendance",
        "label": "Attendance",
        "definition": "Frappe HRMS Attendance DocType recording an employee's posted daily attendance status.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.employee.checkin",
        "label": "Employee Checkin",
        "definition": "Frappe HRMS Employee Checkin DocType recording raw IN/OUT device/kiosk logs.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.attendance.request",
        "label": "Attendance Request",
        "definition": "Frappe HRMS Attendance Request DocType requesting manual attendance correction or WFH.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.leave.type",
        "label": "Leave Type",
        "definition": "Frappe HRMS Leave Type DocType defining leave category flags (carry-forward, encashment, LWP).",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.leave.policy",
        "label": "Leave Policy",
        "definition": "Frappe HRMS Leave Policy DocType grouping annual leave entitlement allocations.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.leave.allocation",
        "label": "Leave Allocation",
        "definition": "Frappe HRMS Leave Allocation DocType granting leave days to an employee for a period.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.leave.ledger.entry",
        "label": "Leave Ledger Entry",
        "definition": "Frappe HRMS Leave Ledger Entry DocType recording immutable leave balance credits and debits.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.leave.application",
        "label": "Leave Application",
        "definition": "Frappe HRMS Leave Application DocType submitting an employee leave request for approval.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.compensatory.leave.request",
        "label": "Compensatory Leave Request",
        "definition": "Frappe HRMS Compensatory Leave Request DocType claiming comp-off for holiday work.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.leave.encashment",
        "label": "Leave Encashment",
        "definition": "Frappe HRMS Leave Encashment DocType converting unused leave balance into pay.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.salary.component",
        "label": "Salary Component",
        "definition": "Frappe HRMS Salary Component DocType defining earning/deduction pay components.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.salary.structure",
        "label": "Salary Structure",
        "definition": "Frappe HRMS Salary Structure DocType composing pay components and formulas.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.salary.structure.assignment",
        "label": "Salary Structure Assignment",
        "definition": "Frappe HRMS Salary Structure Assignment DocType assigning a structure and base salary effective from a date.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.payroll.entry",
        "label": "Payroll Entry",
        "definition": "Frappe HRMS Payroll Entry DocType orchestrating batch payslip generation and bank disbursement.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.salary.slip",
        "label": "Salary Slip",
        "definition": "Frappe HRMS Salary Slip DocType representing an employee's itemized pay statement.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.additional.salary",
        "label": "Additional Salary",
        "definition": "Frappe HRMS Additional Salary DocType applying one-off or recurring bonus/deduction adjustments.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.income.tax.slab",
        "label": "Income Tax Slab",
        "definition": "Frappe HRMS Income Tax Slab DocType defining progressive statutory tax brackets.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.gratuity",
        "label": "Gratuity",
        "definition": "Frappe HRMS Gratuity DocType calculating statutory end-of-service indemnity.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.expense.claim",
        "label": "Expense Claim",
        "definition": "Frappe HRMS Expense Claim DocType submitting itemized employee expenses for reimbursement.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.employee.advance",
        "label": "Employee Advance",
        "definition": "Frappe HRMS Employee Advance DocType issuing cash advances against upcoming expenses or salary.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.job.opening",
        "label": "Job Opening",
        "definition": "Frappe HRMS Job Opening DocType publishing an open budgeted position.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.job.applicant",
        "label": "Job Applicant",
        "definition": "Frappe HRMS Job Applicant DocType tracking a candidate applying to a Job Opening.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.interview",
        "label": "Interview",
        "definition": "Frappe HRMS Interview DocType scheduling and scoring candidate interviews.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.job.offer",
        "label": "Job Offer",
        "definition": "Frappe HRMS Job Offer DocType issuing formal employment offer terms to an applicant.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.employee.onboarding",
        "label": "Employee Onboarding",
        "definition": "Frappe HRMS Employee Onboarding DocType executing new-hire onboarding tasks.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.employee.transfer",
        "label": "Employee Transfer",
        "definition": "Frappe HRMS Employee Transfer DocType transferring an employee across departments/branches.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.employee.promotion",
        "label": "Employee Promotion",
        "definition": "Frappe HRMS Employee Promotion DocType recording designation/grade promotion changes.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.employee.separation",
        "label": "Employee Separation",
        "definition": "Frappe HRMS Employee Separation DocType executing employee exit and offboarding tasks.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.appraisal",
        "label": "Appraisal",
        "definition": "Frappe HRMS Appraisal DocType recording periodic employee performance evaluations.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.goal",
        "label": "Goal",
        "definition": "Frappe HRMS Goal DocType tracking employee objectives and KRA progress.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.training.event",
        "label": "Training Event",
        "definition": "Frappe HRMS Training Event DocType scheduling employee training programs.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    },
    {
        "termId": "norn.frappe.hrms.training.result",
        "label": "Training Result",
        "definition": "Frappe HRMS Training Result DocType recording employee training outcomes.",
        "termKind": "class",
        "status": "active",
        "semanticAuthority": "semantic_only"
    }
],
  mappings: [
      {
          "mappingId": "map.norn.frappe.hrms.shift.type",
          "termId": "norn.frappe.hrms.shift.type",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/shift_type/shift_type.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/shift_type/shift_type.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.shift.assignment",
          "termId": "norn.frappe.hrms.shift.assignment",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/shift_assignment/shift_assignment.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/shift_assignment/shift_assignment.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.attendance",
          "termId": "norn.frappe.hrms.attendance",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/attendance/attendance.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/attendance/attendance.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.employee.checkin",
          "termId": "norn.frappe.hrms.employee.checkin",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_checkin/employee_checkin.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_checkin/employee_checkin.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.attendance.request",
          "termId": "norn.frappe.hrms.attendance.request",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/attendance_request/attendance_request.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/attendance_request/attendance_request.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.leave.type",
          "termId": "norn.frappe.hrms.leave.type",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_type/leave_type.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_type/leave_type.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.leave.policy",
          "termId": "norn.frappe.hrms.leave.policy",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_policy/leave_policy.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_policy/leave_policy.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.leave.allocation",
          "termId": "norn.frappe.hrms.leave.allocation",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_allocation/leave_allocation.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_allocation/leave_allocation.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.leave.ledger.entry",
          "termId": "norn.frappe.hrms.leave.ledger.entry",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_ledger_entry/leave_ledger_entry.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_ledger_entry/leave_ledger_entry.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.leave.application",
          "termId": "norn.frappe.hrms.leave.application",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_application/leave_application.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_application/leave_application.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.compensatory.leave.request",
          "termId": "norn.frappe.hrms.compensatory.leave.request",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/compensatory_leave_request/compensatory_leave_request.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/compensatory_leave_request/compensatory_leave_request.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.leave.encashment",
          "termId": "norn.frappe.hrms.leave.encashment",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_encashment/leave_encashment.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/leave_encashment/leave_encashment.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.salary.component",
          "termId": "norn.frappe.hrms.salary.component",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_component/salary_component.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_component/salary_component.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.salary.structure",
          "termId": "norn.frappe.hrms.salary.structure",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_structure/salary_structure.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_structure/salary_structure.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.salary.structure.assignment",
          "termId": "norn.frappe.hrms.salary.structure.assignment",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_structure_assignment/salary_structure_assignment.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_structure_assignment/salary_structure_assignment.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.payroll.entry",
          "termId": "norn.frappe.hrms.payroll.entry",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/payroll_entry/payroll_entry.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/payroll_entry/payroll_entry.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.salary.slip",
          "termId": "norn.frappe.hrms.salary.slip",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_slip/salary_slip.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/salary_slip/salary_slip.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.additional.salary",
          "termId": "norn.frappe.hrms.additional.salary",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/additional_salary/additional_salary.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/additional_salary/additional_salary.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.income.tax.slab",
          "termId": "norn.frappe.hrms.income.tax.slab",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/income_tax_slab/income_tax_slab.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/income_tax_slab/income_tax_slab.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.gratuity",
          "termId": "norn.frappe.hrms.gratuity",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/gratuity/gratuity.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/payroll/doctype/gratuity/gratuity.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.expense.claim",
          "termId": "norn.frappe.hrms.expense.claim",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/expense_claim/expense_claim.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/expense_claim/expense_claim.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.employee.advance",
          "termId": "norn.frappe.hrms.employee.advance",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_advance/employee_advance.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_advance/employee_advance.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.job.opening",
          "termId": "norn.frappe.hrms.job.opening",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_opening/job_opening.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_opening/job_opening.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.job.applicant",
          "termId": "norn.frappe.hrms.job.applicant",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_applicant/job_applicant.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_applicant/job_applicant.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.interview",
          "termId": "norn.frappe.hrms.interview",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/interview/interview.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/interview/interview.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.job.offer",
          "termId": "norn.frappe.hrms.job.offer",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_offer/job_offer.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/job_offer/job_offer.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.employee.onboarding",
          "termId": "norn.frappe.hrms.employee.onboarding",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_onboarding/employee_onboarding.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_onboarding/employee_onboarding.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.employee.transfer",
          "termId": "norn.frappe.hrms.employee.transfer",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_transfer/employee_transfer.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_transfer/employee_transfer.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.employee.promotion",
          "termId": "norn.frappe.hrms.employee.promotion",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_promotion/employee_promotion.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_promotion/employee_promotion.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.employee.separation",
          "termId": "norn.frappe.hrms.employee.separation",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_separation/employee_separation.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/employee_separation/employee_separation.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.appraisal",
          "termId": "norn.frappe.hrms.appraisal",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/appraisal/appraisal.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/appraisal/appraisal.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.goal",
          "termId": "norn.frappe.hrms.goal",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/goal/goal.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/goal/goal.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.training.event",
          "termId": "norn.frappe.hrms.training.event",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/training_event/training_event.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/training_event/training_event.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      },
      {
          "mappingId": "map.norn.frappe.hrms.training.result",
          "termId": "norn.frappe.hrms.training.result",
          "sourceId": "source.frappe-hrms",
          "externalIri": "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/training_result/training_result.json",
          "relation": "close_match",
          "justification": "The term names this Frappe HRMS DocType of the pinned release.",
          "evidenceRefs": [
              "https://raw.githubusercontent.com/frappe/hrms/1b2d7e034e0de8e30a9e513541d7741710587ad5/hrms/hr/doctype/training_result/training_result.json"
          ],
          "confidenceBasisPoints": 9000,
          "reviewedBy": "hr-vertical (agent, proposed)",
          "reviewedAt": "2026-11-26T00:00:00Z"
      }
  ],
  shapes: [
    {
        "shapeId": "shape.norn.frappe.hrms.shift.type",
        "termId": "norn.frappe.hrms.shift.type",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.shift.assignment",
        "termId": "norn.frappe.hrms.shift.assignment",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.attendance",
        "termId": "norn.frappe.hrms.attendance",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.employee.checkin",
        "termId": "norn.frappe.hrms.employee.checkin",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.attendance.request",
        "termId": "norn.frappe.hrms.attendance.request",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.leave.type",
        "termId": "norn.frappe.hrms.leave.type",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.leave.policy",
        "termId": "norn.frappe.hrms.leave.policy",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.leave.allocation",
        "termId": "norn.frappe.hrms.leave.allocation",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.leave.ledger.entry",
        "termId": "norn.frappe.hrms.leave.ledger.entry",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.leave.application",
        "termId": "norn.frappe.hrms.leave.application",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.compensatory.leave.request",
        "termId": "norn.frappe.hrms.compensatory.leave.request",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.leave.encashment",
        "termId": "norn.frappe.hrms.leave.encashment",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.salary.component",
        "termId": "norn.frappe.hrms.salary.component",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.salary.structure",
        "termId": "norn.frappe.hrms.salary.structure",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.salary.structure.assignment",
        "termId": "norn.frappe.hrms.salary.structure.assignment",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.payroll.entry",
        "termId": "norn.frappe.hrms.payroll.entry",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.salary.slip",
        "termId": "norn.frappe.hrms.salary.slip",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.additional.salary",
        "termId": "norn.frappe.hrms.additional.salary",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.income.tax.slab",
        "termId": "norn.frappe.hrms.income.tax.slab",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.gratuity",
        "termId": "norn.frappe.hrms.gratuity",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.expense.claim",
        "termId": "norn.frappe.hrms.expense.claim",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.employee.advance",
        "termId": "norn.frappe.hrms.employee.advance",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.job.opening",
        "termId": "norn.frappe.hrms.job.opening",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.job.applicant",
        "termId": "norn.frappe.hrms.job.applicant",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.interview",
        "termId": "norn.frappe.hrms.interview",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.job.offer",
        "termId": "norn.frappe.hrms.job.offer",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.employee.onboarding",
        "termId": "norn.frappe.hrms.employee.onboarding",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.employee.transfer",
        "termId": "norn.frappe.hrms.employee.transfer",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.employee.promotion",
        "termId": "norn.frappe.hrms.employee.promotion",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.employee.separation",
        "termId": "norn.frappe.hrms.employee.separation",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.appraisal",
        "termId": "norn.frappe.hrms.appraisal",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.goal",
        "termId": "norn.frappe.hrms.goal",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.training.event",
        "termId": "norn.frappe.hrms.training.event",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    },
    {
        "shapeId": "shape.norn.frappe.hrms.training.result",
        "termId": "norn.frappe.hrms.training.result",
        "closed": true,
        "fields": [
            {
                "fieldId": "name",
                "valueType": "string",
                "minCount": 1,
                "maxCount": 1
            }
        ]
    }
],
})
