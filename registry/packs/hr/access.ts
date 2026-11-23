/**
 * Fail-closed action-level AccessIR policy for all HR domain actions (#1593).
 *
 * Compiles to `hr.access.policy` (referenced by `access: { artifactId: "hr.access.policy", version: "0.1.0" }`
 * on every HR action, matching `registry/packs/ml/ml.ts`).
 */
import { Norn } from "@norn/authoring"
import {
  ApproveLeave,
  CancelLeave,
  LeaveRequest,
  RejectLeave,
  SubmitLeave,
} from "./people_leave.ts"
import {
  ApproveAttendanceRequest,
  AttendanceEvent,
  AttendanceRequest,
  ProposeRoster,
  PublishRoster,
  RecordAttendance,
  Roster,
  SubmitAttendanceRequest,
} from "./rostering.ts"

export const HrAccess = Norn.access({
  id: "hr.access",
  title: "HR action authorization policy",
  version: "0.1.0",
  rules: [
    {
      ruleId: "submit_or_cancel_leave",
      actions: [SubmitLeave, CancelLeave],
      resourceType: LeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["employee", "branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "decide_leave",
      actions: [ApproveLeave, RejectLeave],
      resourceType: LeaveRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
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
      ruleId: "record_attendance",
      actions: [RecordAttendance],
      resourceType: AttendanceEvent,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "submit_attendance_request",
      actions: [SubmitAttendanceRequest],
      resourceType: AttendanceRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["employee", "branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
    {
      ruleId: "approve_attendance_request",
      actions: [ApproveAttendanceRequest],
      resourceType: AttendanceRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["branch_manager", "hr_manager", "hr_admin", "owner"] } }],
    },
  ],
})
