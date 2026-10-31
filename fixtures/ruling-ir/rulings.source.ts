import { Norn } from "@norn/authoring"

// norn#59: SalesOrder `invoiced` is derived from invoice facts.
const order = Norn.pin("commerce.SalesOrder.entity", "0.1.0")

export const invoicedIsDerived = Norn.claim({
  id: "commerce.sales_order_invoiced_is_derived.claim",
  title: "A sales order's invoiced state is derived",
  version: "0.1.0",
  statement: Norn.statement.derivedState(
    Norn.meta.state(order, "commerce.SalesOrder", "status", "invoiced"),
    [Norn.meta.link(order, "commerce.SalesOrder", "lines")],
  ),
  evidence: [
    Norn.evidence.upstream(
      "erpnext",
      "v16.0.0",
      "erpnext/selling/doctype/sales_order/sales_order.py#set_status",
      "structural",
      "status follows per_billed, computed from Sales Invoice items",
    ),
  ],
})

// A cross-system conflict: the ERP maps the gross total, the storefront the net.
const erp = Norn.pin("shop.erp.mapping", "0.1.0")
const storefront = Norn.pin("shop.storefront.mapping", "0.1.0")

export const totalsDiverge = Norn.claim({
  id: "shop.order_total_divergence.claim",
  title: "ERP and storefront disagree on an order's total",
  version: "0.1.0",
  statement: Norn.statement.divergence(
    Norn.meta.property(Norn.pin("shop.Order.entity", "0.1.0"), "shop.Order", "total"),
    Norn.observe(erp, "order", { name: "SO-1", grand_total: 114 }),
    Norn.observe(storefront, "order", { id: "1001", subtotal_price: 100, total_price: 114 }),
  ),
  evidence: [Norn.evidence.upstream("storefront", "2026-10", "admin/api/orders.json#subtotal_price", "sampled")],
})

export const totalIsGross = Norn.ruling({
  id: "shop.order_total_is_gross.ruling",
  title: "An order's total is tax-inclusive",
  version: "0.1.0",
  resolves: [Norn.pin("shop.order_total_divergence.claim", "0.1.0")],
  outcome: Norn.outcome.choose(Norn.meta.artifact(Norn.pin("shop.storefront.mapping", "0.2.0"))),
  affects: [Norn.meta.artifact(Norn.pin("shop.storefront.mapping", "0.2.0"))],
  decidedBy: { principal: "owner", authority: "owner" },
  decidedAt: "2026-10-31",
  alternatives: [
    { outcome: Norn.outcome.reject(), note: "keep both totals" },
    {
      outcome: Norn.outcome.parameterise(
        Norn.meta.artifact(Norn.pin("shop.order_total_basis.setting", "0.1.0")),
        "gross",
        ["tenant", "jurisdiction"],
      ),
    },
    {
      outcome: Norn.outcome.scoped([
        { scope: { tenantId: "eu" }, definition: Norn.meta.artifact(Norn.pin("shop.storefront.mapping", "0.2.0")) },
        { scope: { tenantId: "us" }, definition: Norn.meta.artifact(Norn.pin("shop.storefront.mapping", "0.1.0")) },
      ]),
    },
    { outcome: Norn.outcome.deferUntil("2026-12-01"), note: "wait for the storefront tax API" },
  ],
})
