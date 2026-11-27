# Projection report

Generated from the committed ontology links. Every source term is accounted for once.

## `norn.link.erpnext.canon`

`norn.ontology-pack.erpnext` → `norn.ontology-pack.canon`

### Maps

- `norn.erpnext.customer` subsumed_by `norn.core.agent`
- `norn.erpnext.delivery.note` subsumed_by `norn.core.event.transfer`
- `norn.erpnext.guard.accounting.period.closed` equivalent `norn.finance.guard.period.closed`
- `norn.erpnext.guard.customer.disabled` subsumed_by `norn.finance.guard.party.disabled`
- `norn.erpnext.guard.journal.unbalanced` subsumed_by `norn.finance.guard.entry.unbalanced`
- `norn.erpnext.guard.supplier.on.hold` subsumed_by `norn.finance.guard.party.disabled`
- `norn.erpnext.item` subsumed_by `norn.core.resource.type`
- `norn.erpnext.journal.entry` subsumed_by `norn.finance.accounting.transaction`
- `norn.erpnext.payment.entry.cancel` subsumes `norn.finance.payment.reverse`
- `norn.erpnext.payment.entry.currency` subsumes `norn.finance.payment.currency`
- `norn.erpnext.payment.entry.paid.amount` subsumes `norn.finance.payment.amount`
- `norn.erpnext.payment.entry.party` subsumes `norn.finance.payment.party`
- `norn.erpnext.payment.entry.payment.type` subsumes `norn.finance.payment.direction`
- `norn.erpnext.payment.entry.posting.date` subsumes `norn.finance.payment.date.posting`
- `norn.erpnext.payment.entry.reference` subsumes `norn.finance.payment.allocation`
- `norn.erpnext.payment.entry.reference.allocated.amount` subsumes `norn.finance.payment.allocation.amount`
- `norn.erpnext.payment.entry.reference.reference.name` subsumes `norn.finance.payment.allocation.invoice`
- `norn.erpnext.payment.entry.submit` subsumes `norn.finance.payment.post`
- `norn.erpnext.payment.entry.unallocated.amount` subsumes `norn.finance.payment.amount.unallocated`
- `norn.erpnext.sales.invoice` subsumed_by `norn.finance.invoice`
- `norn.erpnext.sales.invoice.cancel` subsumed_by `norn.finance.invoice.reverse`
- `norn.erpnext.sales.invoice.currency` subsumed_by `norn.finance.invoice.currency`
- `norn.erpnext.sales.invoice.customer` subsumed_by `norn.finance.invoice.party`
- `norn.erpnext.sales.invoice.due.date` subsumed_by `norn.finance.invoice.date.due`
- `norn.erpnext.sales.invoice.grand.total` subsumed_by `norn.finance.invoice.amount.total`
- `norn.erpnext.sales.invoice.is.return` derived `norn.finance.invoice.amount.total`
- `norn.erpnext.sales.invoice.item` subsumed_by `norn.finance.invoice.line`
- `norn.erpnext.sales.invoice.item.amount` subsumed_by `norn.finance.invoice.line.amount`
- `norn.erpnext.sales.invoice.item.item.code` subsumed_by `norn.finance.invoice.line.resource`
- `norn.erpnext.sales.invoice.item.qty` subsumed_by `norn.finance.invoice.line.quantity`
- `norn.erpnext.sales.invoice.net.total` subsumed_by `norn.finance.invoice.amount.net`
- `norn.erpnext.sales.invoice.outstanding.amount` derived `norn.finance.invoice.amount.total`, `norn.finance.payment.allocation.amount`
- `norn.erpnext.sales.invoice.posting.date` subsumed_by `norn.finance.invoice.date.posting`
- `norn.erpnext.sales.invoice.submit` subsumed_by `norn.finance.invoice.post`
- `norn.erpnext.sales.invoice.total.taxes` subsumed_by `norn.finance.invoice.amount.tax`
- `norn.erpnext.sales.order` subsumed_by `norn.commerce.order`
- `norn.erpnext.sales.order.customer` subsumed_by `norn.commerce.order.party`
- `norn.erpnext.sales.order.grand.total` subsumed_by `norn.commerce.order.amount.total`
- `norn.erpnext.stock.entry` subsumed_by `norn.core.event`
- `norn.erpnext.supplier` subsumed_by `norn.core.agent`

### Residual (system-specific)

- `norn.erpnext.customer.customer.group`
- `norn.erpnext.delivery.note.is.return`
- `norn.erpnext.item.has.variants`
- `norn.erpnext.journal.entry.voucher.type`
- `norn.erpnext.sales.invoice.is.pos`
- `norn.erpnext.sales.invoice.update.stock`
- `norn.erpnext.stock.entry.purpose`
- `norn.erpnext.supplier.supplier.group`

### Missing from the canon

- `norn.erpnext.customer.customer.type`
- `norn.erpnext.delivery.note.customer`
- `norn.erpnext.item.stock.uom`
- `norn.erpnext.item.valuation.method`
- `norn.erpnext.journal.entry.total.credit`
- `norn.erpnext.journal.entry.total.debit`
- `norn.erpnext.payment.entry`
- `norn.erpnext.sales.invoice.status`
- `norn.erpnext.sales.order.per.billed`
- `norn.erpnext.sales.order.per.delivered`
- `norn.erpnext.sales.order.status`

### Non-conforming source behaviour (breaks a canon law; flagged, never mapped)

None.

## `norn.link.frappe-hrms.workforce`

`norn.ontology-pack.frappe-hrms` → `norn.ontology-pack.workforce`

### Maps

- `norn.frappe.hrms.additional.salary` equivalent `norn.hr.payroll.adjustment`
- `norn.frappe.hrms.appraisal` equivalent `norn.hr.performance.appraisal`
- `norn.frappe.hrms.attendance` subsumes `norn.hr.attendance.event`
- `norn.frappe.hrms.attendance.request` equivalent `norn.hr.attendance.exception`
- `norn.frappe.hrms.compensatory.leave.request` equivalent `norn.hr.leave.comp.off`
- `norn.frappe.hrms.employee.advance` equivalent `norn.hr.expense.advance`
- `norn.frappe.hrms.employee.checkin` equivalent `norn.hr.attendance.checkin`
- `norn.frappe.hrms.employee.onboarding` equivalent `norn.hr.talent.onboarding`
- `norn.frappe.hrms.employee.promotion` subsumed_by `norn.hr.talent.employment.change`
- `norn.frappe.hrms.employee.separation` equivalent `norn.hr.talent.separation`
- `norn.frappe.hrms.employee.transfer` subsumed_by `norn.hr.talent.employment.change`
- `norn.frappe.hrms.expense.claim` equivalent `norn.hr.expense.claim`
- `norn.frappe.hrms.goal` equivalent `norn.hr.performance.goal`
- `norn.frappe.hrms.gratuity` equivalent `norn.hr.statutory.gratuity`
- `norn.frappe.hrms.income.tax.slab` equivalent `norn.hr.statutory.tax.slab`
- `norn.frappe.hrms.interview` equivalent `norn.hr.talent.interview`
- `norn.frappe.hrms.job.applicant` equivalent `norn.hr.talent.applicant`
- `norn.frappe.hrms.job.offer` equivalent `norn.hr.talent.job.offer`
- `norn.frappe.hrms.job.opening` equivalent `norn.hr.talent.job.opening`
- `norn.frappe.hrms.leave.allocation` equivalent `norn.hr.leave.allocation`
- `norn.frappe.hrms.leave.application` equivalent `norn.hr.leave.request`
- `norn.frappe.hrms.leave.encashment` equivalent `norn.hr.leave.encashment`
- `norn.frappe.hrms.leave.ledger.entry` equivalent `norn.hr.leave.ledger.entry`
- `norn.frappe.hrms.leave.policy` equivalent `norn.hr.leave.policy`
- `norn.frappe.hrms.leave.type` equivalent `norn.hr.leave.type`
- `norn.frappe.hrms.payroll.entry` equivalent `norn.hr.payroll.run`
- `norn.frappe.hrms.salary.component` equivalent `norn.hr.payroll.component`
- `norn.frappe.hrms.salary.slip` equivalent `norn.hr.payroll.payslip`
- `norn.frappe.hrms.salary.structure` equivalent `norn.hr.payroll.structure`
- `norn.frappe.hrms.salary.structure.assignment` equivalent `norn.hr.payroll.assignment`
- `norn.frappe.hrms.shift.assignment` subsumed_by `norn.hr.shift.commitment`
- `norn.frappe.hrms.shift.type` subsumed_by `norn.hr.roster.shift.template`
- `norn.frappe.hrms.training.event` equivalent `norn.hr.training.event`
- `norn.frappe.hrms.training.result` equivalent `norn.hr.training.result`

### Residual (system-specific)

None.

### Missing from the canon

None.

### Non-conforming source behaviour (breaks a canon law; flagged, never mapped)

None.

## `norn.link.odoo.canon`

`norn.ontology-pack.odoo` → `norn.ontology-pack.canon`

### Maps

- `norn.odoo.account.move` subsumes `norn.finance.invoice`
- `norn.odoo.account.move.action.post` subsumes `norn.finance.invoice.post`
- `norn.odoo.account.move.amount.residual` subsumes `norn.finance.invoice.amount.outstanding`
- `norn.odoo.account.move.amount.tax` subsumes `norn.finance.invoice.amount.tax`
- `norn.odoo.account.move.amount.total` subsumes `norn.finance.invoice.amount.total`
- `norn.odoo.account.move.amount.untaxed` subsumes `norn.finance.invoice.amount.net`
- `norn.odoo.account.move.currency.id` subsumes `norn.finance.invoice.currency`
- `norn.odoo.account.move.date` subsumes `norn.finance.invoice.date.posting`
- `norn.odoo.account.move.gives.rise.to` subsumes `norn.finance.invoice.gives.rise.to`
- `norn.odoo.account.move.invoice.date.due` subsumes `norn.finance.invoice.date.due`
- `norn.odoo.account.move.line` equivalent `norn.finance.ledger.entry`
- `norn.odoo.account.move.partner.id` subsumes `norn.finance.invoice.party`
- `norn.odoo.account.move.posting` subsumed_by `norn.finance.accounting.transaction`
- `norn.odoo.account.move.reverse` subsumes `norn.finance.invoice.reverse`
- `norn.odoo.account.payment.action.post` subsumes `norn.finance.payment.post`
- `norn.odoo.account.payment.amount` subsumes `norn.finance.payment.amount`
- `norn.odoo.account.payment.date` subsumes `norn.finance.payment.date.posting`
- `norn.odoo.account.payment.partner.id` subsumes `norn.finance.payment.party`
- `norn.odoo.account.payment.payment.type` subsumes `norn.finance.payment.direction`
- `norn.odoo.product.product` subsumed_by `norn.core.resource.type`
- `norn.odoo.res.partner` subsumes `norn.core.agent`
- `norn.odoo.sale.order` subsumes `norn.commerce.order`
- `norn.odoo.sale.order.amount.total` subsumes `norn.commerce.order.amount.total`
- `norn.odoo.sale.order.partner.id` subsumes `norn.commerce.order.party`
- `norn.odoo.stock.move` subsumed_by `norn.core.event`
- `norn.odoo.stock.move.product.uom.qty` subsumed_by `norn.core.resource.quantity`
- `norn.odoo.stock.picking` subsumed_by `norn.core.event.transfer`

### Residual (system-specific)

- `norn.odoo.account.move.move.type`
- `norn.odoo.res.partner.type`
- `norn.odoo.stock.picking.picking.type.id`

### Missing from the canon

- `norn.odoo.account.move.invoice.date`
- `norn.odoo.account.move.line.credit`
- `norn.odoo.account.move.line.debit`
- `norn.odoo.account.move.payment.state`
- `norn.odoo.account.move.state`
- `norn.odoo.account.payment`
- `norn.odoo.product.product.default.code`
- `norn.odoo.res.partner.is.company`
- `norn.odoo.sale.order.invoice.status`
- `norn.odoo.sale.order.state`
- `norn.odoo.stock.picking.partner.id`

### Non-conforming source behaviour (breaks a canon law; flagged, never mapped)

- `norn.odoo.account.move.button.draft`

## `norn.link.shopify.canon`

`norn.ontology-pack.shopify` → `norn.ontology-pack.canon`

### Maps

- `norn.shopify.customer` subsumed_by `norn.core.agent`
- `norn.shopify.fulfillment` subsumed_by `norn.core.event.transfer`
- `norn.shopify.order` subsumed_by `norn.commerce.order`
- `norn.shopify.order.customer` subsumed_by `norn.commerce.order.party`
- `norn.shopify.order.gives.rise.to` subsumed_by `norn.core.commitment.gives.rise.to`
- `norn.shopify.order.receivable` subsumed_by `norn.core.claim`
- `norn.shopify.order.receivable.currency.code` subsumed_by `norn.finance.invoice.currency`
- `norn.shopify.order.receivable.subtotal.price` subsumed_by `norn.finance.invoice.amount.net`
- `norn.shopify.order.receivable.total.outstanding` subsumed_by `norn.finance.invoice.amount.outstanding`
- `norn.shopify.order.receivable.total.tax` subsumed_by `norn.finance.invoice.amount.tax`
- `norn.shopify.order.total.price` subsumed_by `norn.commerce.order.amount.total`
- `norn.shopify.product` subsumed_by `norn.core.resource.type`
- `norn.shopify.product.variant` subsumed_by `norn.core.resource.type`
- `norn.shopify.refund` subsumed_by `norn.core.event.transfer`

### Residual (system-specific)

- `norn.shopify.customer.state`
- `norn.shopify.fulfillment.status`
- `norn.shopify.product.product.type`
- `norn.shopify.transaction.kind`
- `norn.shopify.transaction.status`

### Missing from the canon

- `norn.shopify.customer.email`
- `norn.shopify.fulfillment.create`
- `norn.shopify.order.cancel`
- `norn.shopify.order.cancelled.at`
- `norn.shopify.order.created.at`
- `norn.shopify.order.display.fulfillment.status`
- `norn.shopify.order.line.item`
- `norn.shopify.order.line.item.original.total`
- `norn.shopify.order.line.item.quantity`
- `norn.shopify.order.line.item.variant`
- `norn.shopify.order.mark.as.paid`
- `norn.shopify.order.receivable.display.financial.status`
- `norn.shopify.product.variant.sku`
- `norn.shopify.refund.create`
- `norn.shopify.transaction`
- `norn.shopify.transaction.amount`

### Non-conforming source behaviour (breaks a canon law; flagged, never mapped)

None.

## `norn.link.supply-lab.canon`

`norn.ontology-pack.supply-lab` → `norn.ontology-pack.canon`

### Maps

- `norn.supply.guard.movement.nonpositive` subsumed_by `norn.finance.guard.amount.nonpositive`
- `norn.supply.order.line` subsumed_by `norn.core.commitment`
- `norn.supply.purchase.order` subsumed_by `norn.core.commitment`
- `norn.supply.shipment.receipt` subsumed_by `norn.core.event.transfer`
- `norn.supply.shipment.receipt.order.line` subsumed_by `norn.core.event.fulfils`
- `norn.supply.shipment.receipt.received.qty` subsumed_by `norn.core.resource.quantity`
- `norn.supply.sku` subsumed_by `norn.core.resource.type`

### Residual (system-specific)

- `norn.supply.supplier.receipt.line`
- `norn.supply.wms.count`

### Missing from the canon

- `norn.supply.demand.forecast`
- `norn.supply.demand.observation`
- `norn.supply.guard.currency.mismatch`
- `norn.supply.guard.unit.mismatch`
- `norn.supply.inspection`
- `norn.supply.inventory.snapshot`
- `norn.supply.inventory.snapshot.quantity.basis`
- `norn.supply.purchase.order.supplier`
- `norn.supply.stock.movement`
- `norn.supply.stock.movement.qty`
- `norn.supply.warehouse`

### Non-conforming source behaviour (breaks a canon law; flagged, never mapped)

None.
