import { Norn } from "@norn/authoring"

export const Privacy = Norn.markings({
  id: "pilot.privacy.markings",
  title: "Privacy catalog",
  version: "0.1.0",
  markings: [
    { id: "pii.contact", label: "Contact", severity: "moderate", category: "pii", mask: { kind: "redacted" } },
    { id: "pii.account", label: "Account", severity: "high", category: "pii", mask: { kind: "last-n", count: 4 } },
    { id: "pii.secret", label: "Secret", severity: "critical", category: "pii", mask: { kind: "null" } },
  ],
})
