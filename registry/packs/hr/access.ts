/**
 * Fail-closed action-level AccessIR policy for all HR domain actions (#1593).
 *
 * Compiles to `hr.access.policy` (referenced by `access: { artifactId: "hr.access.policy", version: "0.1.0" }`
 * on every HR action). Enforces explicit `(org, environment)` scope, role gates,
 * self-service ownership predicates (`requester_user_id == principal.id` and
 * `applicant_user_id == principal.id` for `employee` role), and separation-of-duties
 * predicates (`requester_user_id != principal.id` and `prepared_by_user_id != principal.id`)
 * so no manager or payroll officer can approve their own requests or payroll runs.
 */
import { Norn } from "@norn/authoring"
import {
  ApproveCompensatoryLeave,
  ApproveLeave,
  ApproveLeaveEncashment,
  CancelLeave,
  CompensatoryLeaveRequest,
  ConsumeLeaveHold,
  LeaveEncashment,
  LeaveHold,
  LeaveRequest,
  RejectCompensatoryLeave,
  RejectLeave,
  ReleaseLeaveHold,
  SubmitCompensatoryLeave,
  SubmitLeave,
} from "./people_leave.ts"
import {
  ApproveAttendanceRequest,
  AttendanceEvent,
  AttendanceRequest,
  ProposeRoster,
  PublishRoster,
  RecordAttendance,
  RejectAttendanceRequest,
  ReverseAttendance,
  Roster,
  SubmitAttendanceRequest,
} from "./rostering.ts"
import {
  ApproveEmployeeAdvance,
  ApproveExpenseClaim,
  ApprovePayroll,
  CalculatePayroll,
  DisburseEmployeeAdvance,
  EmployeeAdvance,
  ExpenseClaim,
  PayrollRun,
  PostPayroll,
  RejectExpenseClaim,
  SubmitExpenseClaim,
} from "./payroll_expenses.ts"
import {
  AcceptJobOffer,
  Appraisal,
  CompleteOnboarding,
  FinalizeAppraisal,
  JobOffer,
  OnboardingChecklist,
  SendJobOffer,
} from "./talent.ts"

export const HrAccess = Norn.access({
  id: "hr.access",
  title: "HR action authorization policy",
  version: "0.1.0",
  rules: [
    // ---------------------------------------------------------------- Leave & Holds
    {
      ruleId: "employee_submit_or_cancel_own_leave",
      actions: [SubmitLeave, CancelLeave],
      resourceType: LeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["employee"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "eq",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "manager_submit_or_cancel_leave",
      actions: [SubmitLeave, CancelLeave],
      resourceType: LeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "decide_leave_independent_approver",
      actions: [ApproveLeave, RejectLeave],
      resourceType: LeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "manage_leave_hold",
      actions: [ConsumeLeaveHold, ReleaseLeaveHold],
      resourceType: LeaveHold,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "employee_submit_own_compensatory_leave",
      actions: [SubmitCompensatoryLeave],
      resourceType: CompensatoryLeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["employee"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "eq",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "manager_submit_compensatory_leave",
      actions: [SubmitCompensatoryLeave],
      resourceType: CompensatoryLeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "decide_compensatory_leave_independent_approver",
      actions: [ApproveCompensatoryLeave, RejectCompensatoryLeave],
      resourceType: CompensatoryLeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "approve_leave_encashment_independent_approver",
      actions: [ApproveLeaveEncashment],
      resourceType: LeaveEncashment,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["hr_manager", "payroll_officer", "hr_admin", "owner"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },

    // ---------------------------------------------------------------- Rostering & Attendance
    {
      ruleId: "propose_roster",
      actions: [ProposeRoster],
      resourceType: Roster,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["hr_solver_worker", "branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "publish_roster",
      actions: [PublishRoster],
      resourceType: Roster,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "record_or_reverse_attendance",
      actions: [RecordAttendance, ReverseAttendance],
      resourceType: AttendanceEvent,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "employee_submit_own_attendance_request",
      actions: [SubmitAttendanceRequest],
      resourceType: AttendanceRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["employee"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "eq",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "manager_submit_attendance_request",
      actions: [SubmitAttendanceRequest],
      resourceType: AttendanceRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "decide_attendance_request_independent_approver",
      actions: [ApproveAttendanceRequest, RejectAttendanceRequest],
      resourceType: AttendanceRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },

    // ---------------------------------------------------------------- Payroll, Expenses & Advances
    {
      ruleId: "calculate_payroll",
      actions: [CalculatePayroll],
      resourceType: PayrollRun,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["payroll_officer", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "approve_payroll_independent_approver",
      actions: [ApprovePayroll],
      resourceType: PayrollRun,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["hr_manager", "hr_admin", "owner"] } },
        {
          resource: {
            name: "prepared_by_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "post_payroll",
      actions: [PostPayroll],
      resourceType: PayrollRun,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["payroll_officer", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "employee_submit_own_expense_claim",
      actions: [SubmitExpenseClaim],
      resourceType: ExpenseClaim,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["employee"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "eq",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "manager_submit_expense_claim",
      actions: [SubmitExpenseClaim],
      resourceType: ExpenseClaim,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "payroll_officer", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "decide_expense_claim_independent_approver",
      actions: [ApproveExpenseClaim, RejectExpenseClaim],
      resourceType: ExpenseClaim,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["branch_manager", "hr_manager", "payroll_officer", "hr_admin", "owner"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "approve_employee_advance_independent_approver",
      actions: [ApproveEmployeeAdvance],
      resourceType: EmployeeAdvance,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["branch_manager", "hr_manager", "payroll_officer", "hr_admin", "owner"] } },
        {
          resource: {
            name: "requester_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "disburse_employee_advance",
      actions: [DisburseEmployeeAdvance],
      resourceType: EmployeeAdvance,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["payroll_officer", "hr_admin", "owner"] } }],
    },

    // ---------------------------------------------------------------- Recruitment, Onboarding & Performance
    {
      ruleId: "send_job_offer",
      actions: [SendJobOffer],
      resourceType: JobOffer,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "applicant_accept_own_job_offer",
      actions: [AcceptJobOffer],
      resourceType: JobOffer,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["employee"] } },
        {
          resource: {
            name: "applicant_user_id",
            op: "eq",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "hr_accept_job_offer",
      actions: [AcceptJobOffer],
      resourceType: JobOffer,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "complete_onboarding",
      actions: [CompleteOnboarding],
      resourceType: OnboardingChecklist,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "finalize_appraisal",
      actions: [FinalizeAppraisal],
      resourceType: Appraisal,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
  ],
})
