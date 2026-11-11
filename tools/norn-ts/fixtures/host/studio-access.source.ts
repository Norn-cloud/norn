import { Norn } from "@norn/authoring"

/**
 * Hosted Studio roles (#817): a viewer reads, a member also proposes, an admin
 * (or owner) also approves. Roles are the Norn roles the host's declared table
 * maps from an organization membership; nothing here trusts a client claim.
 */
export const StudioAccess = Norn.access({
  id: "studio.workspace.access",
  title: "Hosted Studio workspace roles",
  version: "1.0.0",
  rules: [
    {
      ruleId: "read",
      actions: ["studio.read"],
      resourceType: "studio.Workspace",
      scope: { dimensions: ["org"] },
      allOf: [{ role: { anyOf: ["viewer", "member", "admin", "owner"] } }],
    },
    {
      ruleId: "propose",
      actions: ["studio.propose"],
      resourceType: "studio.Workspace",
      scope: { dimensions: ["org"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }],
    },
    {
      ruleId: "approve",
      actions: ["studio.approve"],
      resourceType: "studio.Workspace",
      scope: { dimensions: ["org"] },
      allOf: [{ role: { anyOf: ["admin", "owner"] } }],
    },
  ],
})
