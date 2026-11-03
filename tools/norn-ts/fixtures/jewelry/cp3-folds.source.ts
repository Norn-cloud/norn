import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Holding } from "../stock/stock.source"

export const MetalHolding = Norn.type({ name: "jewelry.FoldHolding", title: "Measured metal holding", version: "1.0.0", extends: Holding,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: Schema.Struct({ gross_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), purity: Norn.decimal(0, { unit: { system: "ucum", code: "[ppth]" } }), valuation_currency: Schema.Literal("USD"), book_value: Norn.decimal(2, { currency: "valuation_currency" }) }),
})
const ReceiptItem = Schema.Struct({ resource_code: Schema.String, gross: Schema.Finite, purity: Schema.Int, value: Schema.Finite, unit: Schema.Literal("g"), currency: Schema.Literal("USD") })
const ReceiptState = Schema.Struct({ fine: Schema.Finite, gross: Schema.Finite, value: Schema.Finite, eligible: Schema.Boolean, sources: Schema.Array(Schema.String) })
export const ReceiptStep = Norn.decision({ id: "jewelry.fold.receipt_step", title: "Normalize receipt", version: "1.0.0", contract: "sub_decision", scopeRequired: ["orgId", "environmentId"], input: Schema.Struct({ accumulator: ReceiptState, item: ReceiptItem, index: Schema.Int }), output: ReceiptState }, input => ({
  fine: input.accumulator.fine + input.item.gross * input.item.purity / 1000, gross: input.accumulator.gross + input.item.gross, value: input.accumulator.value + input.item.value,
  eligible: input.accumulator.eligible && input.item.gross > 0 && input.item.purity > 0 && input.item.purity <= 1000 && input.item.value >= 0,
  sources: input.accumulator.sources.concat([input.item.resource_code]),
}))
const HoldingSnapshot = Schema.Struct({ resource_code: Schema.String, quantity: Schema.Finite, unit: Schema.Literal("g"), currency: Schema.Null, valuation_method: Schema.Literals(["fifo", "moving_average"]), gross_quantity: Schema.Finite, purity: Schema.Int, valuation_currency: Schema.Literal("USD"), book_value: Schema.Finite })
const HoldingItem = Schema.Struct({ resource_code: Schema.String, quantity: Schema.Finite, gross: Schema.Finite, purity: Schema.Int, value: Schema.Finite, unit: Schema.Literal("g"), currency: Schema.Literal("USD") })
const Allocation = Schema.Struct({ resource_code: Schema.String, quantity: Schema.Finite, gross: Schema.Finite, value: Schema.Finite, residual_quantity: Schema.Finite, residual_value: Schema.Finite })
const AllocationState = Schema.Struct({ demand: Schema.Finite, fine: Schema.Finite, gross: Schema.Finite, value: Schema.Finite, residual: Schema.Finite, residual_value: Schema.Finite, eligible: Schema.Boolean, allocations: Schema.Array(Allocation) })
export const AllocateStep = Norn.decision({ id: "jewelry.fold.allocate_step", title: "Allocate original holding cost", version: "1.0.0", contract: "sub_decision", scopeRequired: ["orgId", "environmentId"], input: Schema.Struct({ accumulator: AllocationState, item: HoldingItem, index: Schema.Int }), output: AllocationState }, input => {
  const taken = input.item.quantity > 0 ? (input.accumulator.demand < input.item.quantity ? input.accumulator.demand : input.item.quantity) : 0
  const cost = input.item.quantity > 0 ? input.item.value * taken / input.item.quantity : 0
  const gross = input.item.quantity > 0 ? input.item.gross * taken / input.item.quantity : 0
  return { demand: input.accumulator.demand - taken, fine: input.accumulator.fine + taken, gross: input.accumulator.gross + gross, value: input.accumulator.value + cost,
    residual: input.accumulator.residual + input.item.quantity - taken, residual_value: input.accumulator.residual_value + input.item.value - cost,
    eligible: input.accumulator.eligible && input.accumulator.demand >= 0 && input.item.quantity >= 0 && input.item.purity >= 0 && input.item.purity <= 1000 && input.item.quantity === input.item.gross * input.item.purity / 1000 && (input.item.quantity > 0 || (input.item.value === 0 && input.item.gross === 0)) && input.item.gross >= input.item.quantity && input.item.value >= 0,
    allocations: input.accumulator.allocations.concat([{ resource_code: input.item.resource_code, quantity: taken, gross, value: cost, residual_quantity: input.item.quantity - taken, residual_value: input.item.value - cost }]),
  }
})
const Product = Schema.Struct({ resource_code: Schema.String, gross: Schema.Finite, purity: Schema.Int, kind: Schema.Literals(["output", "loss", "byproduct"]), unit: Schema.Literal("g"), currency: Schema.Literal("USD") })
const ProductAllocation = Schema.Struct({ resource_code: Schema.String, kind: Schema.String, fine: Schema.Finite, gross: Schema.Finite, value: Schema.Finite })
const MeltState = Schema.Struct({ funded_fine: Schema.Finite, funded_value: Schema.Finite, remaining_fine: Schema.Finite, remaining_gross: Schema.Finite, remaining_value: Schema.Finite, loss: Schema.Finite, loss_count: Schema.Int, output_count: Schema.Int, threshold: Schema.Finite, needs_approval: Schema.Boolean, eligible: Schema.Boolean, allocations: Schema.Array(ProductAllocation) })
export const MeltStep = Norn.decision({ id: "jewelry.fold.melt_step", title: "Allocate funded conversion", version: "1.0.0", contract: "sub_decision", scopeRequired: ["orgId", "environmentId"], input: Schema.Struct({ accumulator: MeltState, item: Product, index: Schema.Int }), output: MeltState }, input => {
  const fine = input.item.gross * input.item.purity / 1000
  const loss = input.accumulator.loss + (input.item.kind === "loss" ? fine : 0)
  const cost = input.accumulator.funded_fine > 0 ? input.accumulator.funded_value * fine / input.accumulator.funded_fine : 0
  return { funded_fine: input.accumulator.funded_fine, funded_value: input.accumulator.funded_value,
    remaining_fine: input.accumulator.remaining_fine - fine, remaining_gross: input.accumulator.remaining_gross - input.item.gross, remaining_value: input.accumulator.remaining_value - cost,
    loss, needs_approval: input.accumulator.funded_fine > 0 ? loss / input.accumulator.funded_fine > input.accumulator.threshold : false, loss_count: input.accumulator.loss_count + (input.item.kind === "loss" ? 1 : 0), output_count: input.accumulator.output_count + (input.item.kind === "output" ? 1 : 0), threshold: input.accumulator.threshold,
    eligible: input.accumulator.eligible && input.item.gross >= 0 && input.item.purity >= 0 && input.item.purity <= 1000 && input.accumulator.threshold >= 0 && input.accumulator.threshold <= 1,
    allocations: input.accumulator.allocations.concat([{ resource_code: input.item.resource_code, kind: input.item.kind, fine, gross: input.item.gross, value: cost }]),
  }
})
export const Plan = Norn.decision({ id: "jewelry.fold.plan", title: "Sell, scrap and melt allocation plan", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({ slice: Schema.Struct({ opening: HoldingSnapshot, scrap: ReceiptItem, products: Schema.Array(Product), sell_quantity: Schema.Finite, sale_price: Schema.Finite, threshold: Schema.Finite }), event: Schema.Struct({ kind: Schema.String }) }),
  output: Schema.Struct({ facts: Schema.Array(Schema.Struct({ op: Schema.Literal("delete"), objectType: Schema.Literal("jewelry.FoldHolding"), key: Schema.String })), commands: Schema.Array(Schema.Struct({ actionId: Schema.String, complete: Schema.Boolean, needs_approval: Schema.Boolean, sale_price: Schema.Finite, sale: AllocationState, receipt: ReceiptState, funding: AllocationState, melt: MeltState })), timers: Schema.Array(Schema.Struct({})) }),
}, input => {
  const sale = Norn.orderedFold(AllocateStep, { items: [{ resource_code: input.slice.opening.resource_code, quantity: input.slice.opening.quantity, gross: input.slice.opening.gross_quantity, purity: input.slice.opening.purity, value: input.slice.opening.book_value, unit: input.slice.opening.unit, currency: input.slice.opening.valuation_currency }], initial: { demand: input.slice.sell_quantity, fine: 0, gross: 0, value: 0, residual: 0, residual_value: 0, eligible: true, allocations: [] } }, { identity: "resource_code", order: [{ property: "resource_code", kind: "string" }], maxItems: 64, maxAccumulatorBytes: 65536, arithmetic: { scale: 6, rounding: "refuse" } })
  const receipt = Norn.orderedFold(ReceiptStep, { items: [input.slice.scrap], initial: { fine: 0, gross: 0, value: 0, eligible: true, sources: [] } }, { identity: "resource_code", order: [{ property: "resource_code", kind: "string" }], maxItems: 64, maxAccumulatorBytes: 65536, arithmetic: { scale: 6, rounding: "refuse" } })
  const funding = Norn.orderedFold(AllocateStep, { items: [
    { resource_code: input.slice.opening.resource_code, quantity: sale.residual, gross: input.slice.opening.gross_quantity - sale.gross, purity: input.slice.opening.purity, value: sale.residual_value, unit: "g", currency: "USD" },
    { resource_code: input.slice.scrap.resource_code, quantity: receipt.fine, gross: receipt.gross, purity: input.slice.scrap.purity, value: receipt.value, unit: "g", currency: "USD" },
  ], initial: { demand: sale.residual + receipt.fine, fine: 0, gross: 0, value: 0, residual: 0, residual_value: 0, eligible: true, allocations: [] } }, { identity: "resource_code", order: [{ property: "resource_code", kind: "string" }], maxItems: 64, maxAccumulatorBytes: 65536, arithmetic: { scale: 6, rounding: "refuse" } })
  const melt = Norn.orderedFold(MeltStep, { items: input.slice.products, initial: { funded_fine: funding.fine, funded_value: funding.value, remaining_fine: funding.fine, remaining_gross: funding.gross, remaining_value: funding.value, loss: 0, loss_count: 0, output_count: 0, threshold: input.slice.threshold, needs_approval: false, eligible: true, allocations: [] } }, { identity: "resource_code", order: [{ property: "resource_code", kind: "string" }], maxItems: 64, maxAccumulatorBytes: 65536, arithmetic: { scale: 6, rounding: "refuse" } })
  return { facts: [], timers: [], commands: [{ actionId: "cp3_plan", sale, receipt, funding, melt, sale_price: input.slice.sale_price,
    complete: sale.eligible && sale.demand === 0 && input.slice.sell_quantity > 0 && input.slice.sale_price >= 0 && receipt.eligible && funding.eligible && funding.demand === 0 && melt.eligible && melt.funded_fine > 0 && melt.remaining_fine === 0 && melt.remaining_gross === 0 && melt.remaining_value === 0 && melt.loss_count > 0 && melt.output_count > 0,
    needs_approval: melt.needs_approval,
  }] }
})
