import { gen } from "effect/Effect"
import * as Effects from "effect"
import { Norn as N } from "@norn/authoring"
import { chargeCard, loadInvoice } from "../flows/support/billing.source.ts"

export default N.flow({ id: "billing.sequence", version: "1.0.0" }, gen(function* () {
  const invoice = yield* N.perform("load", loadInvoice, { invoiceId: "inv-1" })
  const receipt = yield* N.perform("charge", chargeCard, { invoiceId: invoice.id })
  const same = yield* Effects.Effect.succeed(receipt.receiptId)
  return { invoice: invoice.id, receipt: same }
}))
