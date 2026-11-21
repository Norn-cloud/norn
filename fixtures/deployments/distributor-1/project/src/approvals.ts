// Approval of large credit requests. Norn.approval is not on main yet (it lands
// with the purchase-requests builder loop, #1469), so run.sh compiles this
// module on its own and records "approvals: not authorable" when it fails.
import { Norn } from "@norn/authoring"
import { CreditRequest, SubmitCredit } from "./distributor.ts"

/** Over 100,000 EGP a credit limit is signed off by finance before it is submitted. */
export const LargeCredit = Norn.approval({
  id: "distributor.large_credit",
  action: SubmitCredit,
  on: CreditRequest,
  when: { property: "new_limit", op: "gt", value: "100000" },
  decider: { roles: ["admin", "owner"] },
  question: "Raise this customer's credit limit above 100,000 EGP?",
  risk: "medium",
})
