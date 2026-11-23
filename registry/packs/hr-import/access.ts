/**
 * Fail-closed action-level AccessIR policy for HR historical import actions (#1594).
 *
 * `hr_import_worker` may quarantine nonconforming candidates during untrusted
 * extraction, but NEVER promotes or rejects candidates into authoritative HR
 * state; promotion and rejection require human HR review authority.
 */
import { Norn } from "@norn/authoring"
import {
  ExtractedCandidate,
  PromoteCandidate,
  QuarantineCandidate,
  RejectCandidate,
} from "./hr_import.ts"

export const HrImportAccess = Norn.access({
  id: "hr_import.access",
  title: "HR historical import action authorization policy",
  version: "0.1.0",
  rules: [
    {
      ruleId: "quarantine_candidate",
      actions: [QuarantineCandidate],
      resourceType: ExtractedCandidate,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        {
          role: {
            anyOf: ["hr_import_worker", "hr_manager", "hr_admin", "owner"],
          },
        },
      ],
    },
    {
      ruleId: "promote_or_reject_candidate",
      actions: [PromoteCandidate, RejectCandidate],
      resourceType: ExtractedCandidate,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        {
          role: {
            anyOf: ["hr_manager", "hr_admin", "owner"],
          },
        },
      ],
    },
  ],
})
