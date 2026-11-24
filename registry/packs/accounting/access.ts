/**
 * Fail-closed action-level AccessIR policy for the Accounting Domain Pack.
 *
 * Compiles to `accounting.access.policy`, enforces explicit `(org, environment)`
 * scope and role separation across every accounting action, and requires
 * separation of duties on `accounting.journal.post` so an entry's creator
 * (`created_by_user_id`) cannot post their own journal entry.
 */
import { Norn } from "@norn/authoring"
import {
  CloseFiscalPeriod,
  FiscalPeriod,
  JournalEntry,
  LockFiscalPeriod,
  PostJournalEntry,
  ReverseJournalEntry,
} from "./ledger.ts"
import {
  Invoice,
  IssueInvoice,
  Payment,
  RecordPayment,
  VoidInvoice,
} from "./settlement.ts"

export const AccountingAccess = Norn.access({
  id: "accounting.access",
  title: "Accounting action authorization policy",
  version: "0.1.0",
  rules: [
    {
      ruleId: "post_journal_entry",
      actions: [PostJournalEntry],
      resourceType: JournalEntry,
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["senior_accountant", "controller", "owner"] } },
        {
          resource: {
            name: "created_by_user_id",
            op: "ne",
            value: { principalAttribute: "id" },
          },
        },
      ],
    },
    {
      ruleId: "reverse_journal_entry",
      actions: [ReverseJournalEntry],
      resourceType: JournalEntry,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["controller", "owner"] } }],
    },
    {
      ruleId: "close_fiscal_period",
      actions: [CloseFiscalPeriod],
      resourceType: FiscalPeriod,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["senior_accountant", "controller", "owner"] } }],
    },
    {
      ruleId: "lock_fiscal_period",
      actions: [LockFiscalPeriod],
      resourceType: FiscalPeriod,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["controller", "owner"] } }],
    },
    {
      ruleId: "issue_invoice",
      actions: [IssueInvoice],
      resourceType: Invoice,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["accountant", "senior_accountant", "controller", "owner"] } }],
    },
    {
      ruleId: "void_invoice",
      actions: [VoidInvoice],
      resourceType: Invoice,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["senior_accountant", "controller", "owner"] } }],
    },
    {
      ruleId: "record_payment",
      actions: [RecordPayment],
      resourceType: Payment,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["accountant", "senior_accountant", "controller", "owner"] } }],
    },
  ],
})
