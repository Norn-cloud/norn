/**
 * Fail-closed action-level AccessIR policy for HR historical import actions (#1594).
 *
 * `hr_import_worker` may quarantine nonconforming candidates during untrusted
 * extraction, but NEVER promotes, rejects, supersedes candidates, or resolves
 * findings; every promotion or finding disposition requires human HR authority.
 */
import { Norn } from "@norn/authoring"
import {
  DataQualityFinding,
  ExtractedCandidate,
  PromoteCandidate,
  QuarantineCandidate,
  RejectCandidate,
  RejectFinding,
  ResolveFinding,
  SupersedeCandidate,
  SupersedeFinding,
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
      ruleId: "decide_candidate",
      actions: [PromoteCandidate, RejectCandidate, SupersedeCandidate],
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
    {
      ruleId: "resolve_or_supersede_finding",
      actions: [ResolveFinding, SupersedeFinding, RejectFinding],
      resourceType: DataQualityFinding,
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
