module order_economics {
  // Abstract the immutable invoice/payment facts that lane A folds.
  var status: str
  var invoices: int
  var invoicedTotal: int
  var allocatedTotal: int
  val invoiced = invoices > 0
  val completed = invoiced and allocatedTotal >= invoicedTotal
  val outstanding = invoicedTotal - allocatedTotal
  action init = all {
    status' = "draft", invoices' = 0, invoicedTotal' = 0, allocatedTotal' = 0,
  }
  action transition(nextStatus: str): bool = all {
    any {
      status == "draft" and nextStatus == "submitted",
      status == "submitted" and nextStatus == "approved",
      status != "cancelled" and nextStatus == "cancelled" and not(invoiced) and allocatedTotal == 0,
    },
    status' = nextStatus, invoices' = invoices,
    invoicedTotal' = invoicedTotal, allocatedTotal' = allocatedTotal,
  }
  action invoice(amount: int): bool = all {
    status == "approved", amount > 0,
    status' = status, invoices' = invoices + 1,
    invoicedTotal' = invoicedTotal + amount, allocatedTotal' = allocatedTotal,
  }
  action pay(amount: int): bool = all {
    status == "approved", amount > 0,
    status' = status, invoices' = invoices,
    invoicedTotal' = invoicedTotal, allocatedTotal' = allocatedTotal + amount,
  }
  action step = any {
    transition("submitted"), transition("approved"), transition("cancelled"),
    invoice(1), invoice(2), pay(1), pay(2),
  }
  val safety = all {
    invoiced implies status == "approved",
    completed implies invoiced,
    status == "cancelled" implies allocatedTotal == 0,
    completed implies outstanding <= 0,
    invoices >= 0 and invoicedTotal >= 0 and allocatedTotal >= 0,
  }
}
