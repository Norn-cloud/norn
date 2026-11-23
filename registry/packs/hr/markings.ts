/**
 * HR privacy markings & fail-closed reveal policy (#1593).
 *
 * PII fields are marked at declaration time so managers and the shift-rostering
 * solver worker receive only the unmarked operational slice, while HR/payroll
 * roles reveal sensitive fields only under audited reveal receipts (`HR-PI-01`).
 */
import { Norn } from "@norn/authoring"

export const HrMarkings = Norn.markings({
  id: "hr.privacy.markings",
  title: "HR privacy and PII classification markings",
  version: "0.1.0",
  markings: [
    {
      id: "pii.demographic",
      label: "Demographic PII",
      severity: "high",
      category: "pii",
      mask: { kind: "redacted" },
    },
    {
      id: "pii.gov_id",
      label: "Government ID",
      severity: "critical",
      category: "pii",
      mask: { kind: "last-n", count: 4 },
    },
    {
      id: "pii.compensation",
      label: "Compensation",
      severity: "high",
      category: "pii",
      mask: { kind: "redacted" },
    },
    {
      id: "pii.medical",
      label: "Medical note",
      severity: "critical",
      category: "pii",
      mask: { kind: "redacted" },
    },
    {
      id: "pii.raw_chat_media",
      label: "Raw chat or schedule media",
      severity: "critical",
      category: "pii",
      mask: { kind: "redacted" },
    },
  ],
})

export const HrRevealPolicy = Norn.access({
  id: "hr.reveal",
  title: "Who may reveal marked HR personal data",
  version: "0.1.0",
  rules: [
    {
      ruleId: "hr_admin_reveal_identity_and_medical",
      actions: ["norn.reveal"],
      resourceType: "norn.Marking",
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["hr_admin", "owner"] } },
        { marking: { anyOf: ["pii.demographic", "pii.gov_id", "pii.medical", "pii.raw_chat_media"] } },
      ],
    },
    {
      ruleId: "payroll_reveal_compensation",
      actions: ["norn.reveal"],
      resourceType: "norn.Marking",
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["payroll_officer", "hr_admin", "owner"] } },
        { marking: { anyOf: ["pii.compensation"] } },
      ],
    },
  ],
})
