// T7-TS-2 (#2690): Norn.type with links (relations), Norn.action (upsert and a
// declared transition, with a compensation), and Norn.access. Golden: crm.ir.json.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Account = Norn.type({
  name: "crm.Account",
  title: "Account",
  version: "0.1.0",
  properties: Schema.Struct({ account_id: Schema.String }),
  identity: [{ name: "primary", properties: ["account_id"] }],
})

export const Contact = Norn.type({
  name: "crm.Contact",
  title: "Contact",
  version: "0.1.0",
  description: "A person at an account.",
  properties: Schema.Struct({
    contact_id: Schema.String,
    email: Schema.NullOr(Schema.String),
    status: Schema.Literals(["active", "archived"]),
  }),
  links: [
    {
      id: "account",
      description: "The account the contact works for.",
      target: Account,
      cardinality: { to: "one", from: "many" },
      inverse: "contacts",
    },
  ],
  identity: [{ name: "primary", properties: ["contact_id"] }],
  defaults: { status: "active" },
  lifecycles: [
    {
      property: "status",
      initial: ["active"],
      transitions: [
        { id: "archive", from: ["active"], to: "archived" },
        { id: "restore", from: ["archived"], to: "active" },
      ],
    },
  ],
})

const ContactRef = Schema.Struct({ contact_id: Schema.String })

export const UpsertContact = Norn.action({
  id: "crm.contact.upsert",
  title: "Upsert contact",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({ contact_id: Schema.String, email: Schema.optionalKey(Schema.String) }),
  output: ContactRef,
  writes: [
    { type: Contact, change: "created" },
    { type: Contact, change: "modified" },
  ],
  effect: { kind: "upsert_input_object", type: Contact, objectIdField: "contact_id" },
})

export const RestoreContact = Norn.action({
  id: "crm.contact.restore",
  title: "Restore contact",
  version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: false,
  ledgered: true,
  input: ContactRef,
  output: ContactRef,
  writes: [{ type: Contact, change: "modified" }],
  effect: { kind: "fire_transition", type: Contact, objectIdField: "contact_id", lifecycle: "status", transition: "restore" },
})

export const ArchiveContact = Norn.action({
  id: "crm.contact.archive",
  title: "Archive contact",
  version: "0.1.0",
  description: "Archive a contact; restoring it compensates.",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: false,
  ledgered: true,
  ledgerCategory: "crm.contact",
  input: ContactRef,
  output: ContactRef,
  preconditions: [Norn.unsafeZen<boolean>("len(contact_id) > 0")],
  writes: [{ type: Contact, change: "modified" }],
  effect: { kind: "fire_transition", type: Contact, objectIdField: "contact_id", lifecycle: "status", transition: "archive" },
  compensation: RestoreContact,
})

export const CrmAccess = Norn.access({
  id: "crm.contact.access",
  title: "Contact access",
  version: "0.1.0",
  rules: [
    {
      ruleId: "staff_manage_contacts",
      actions: [UpsertContact, ArchiveContact, RestoreContact],
      resourceType: Contact,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["Staff"] } }],
    },
    {
      ruleId: "owners_read_contacts",
      actions: ["norn.read"],
      resourceType: Contact,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["Staff"] } }, { rel: { from: "resource", role: "account" } }],
    },
  ],
})
