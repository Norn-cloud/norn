import { Norn } from "@norn/authoring"

export const ReferencedClassifications = Norn.markings({
  id: "demo.erpnext.classifications.entity", title: "Demo admission classifications", version: "0.1.0",
  markings: [
    { id: "pii.contact", label: "Contact", severity: "critical", category: "pii.contact", mask: { kind: "redacted" } },
    { id: "pii.identity", label: "Identity", severity: "critical", category: "pii.identity", mask: { kind: "redacted" } },
    { id: "pii.financial", label: "Financial", severity: "critical", category: "pii.financial", mask: { kind: "redacted" } },
    { id: "confidential.finance", label: "Confidential finance", severity: "critical", category: "confidential.finance", mask: { kind: "redacted" } },
    { id: "pii.health", label: "Health", severity: "critical", category: "pii.health", mask: { kind: "redacted" } },
  ],
})
