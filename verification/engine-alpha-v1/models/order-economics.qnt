// Finite model before runtime integration. Exact minor units, two cells,
// two orders per cell, two economic kinds. Bounds are not a production limit.
module order_economics {
  type Event = { cell: int, order: int, kind: str, id: int, amount: int }
  type Totals = { invoice_count: int, invoiced_total: int, allocated_total: int }
  var ledger: List[Event]
  var totals: List[Totals]
  var approved: List[bool]
  var beforeLedger: List[Event]
  var beforeTotals: List[Totals]
  var accepted: bool

  pure val zero: Totals = { invoice_count: 0, invoiced_total: 0, allocated_total: 0 }
  pure def slot(cell: int, order: int): int = cell * 2 + order
  pure def add(t: Totals, e: Event): Totals = {
    invoice_count: t.invoice_count + (if (e.kind == "invoice") 1 else 0),
    invoiced_total: t.invoiced_total + (if (e.kind == "invoice") e.amount else 0),
    allocated_total: t.allocated_total + (if (e.kind == "payment") e.amount else 0),
  }
  pure def foldOrder(events: List[Event], cell: int, order: int): Totals =
    events.foldl(zero, (t, e) => if (e.cell == cell and e.order == order) add(t, e) else t)
  pure def invoiced(t: Totals): bool = t.invoice_count > 0
  pure def completed(t: Totals): bool = invoiced(t) and t.allocated_total >= t.invoiced_total
  pure def outstanding(t: Totals): int = t.invoiced_total - t.allocated_total

  action init = all {
    ledger' = [], totals' = [zero, zero, zero, zero],
    approved' = [false, false, false, false],
    beforeLedger' = [], beforeTotals' = [zero, zero, zero, zero], accepted' = false,
  }
  action record(e: Event): bool = {
    // Fact identity is scoped by cell and kind, not by the order it targets.
    // Reusing an id for a different order must also refuse.
    val duplicate = ledger.indices().exists(i => ledger[i].cell == e.cell
      and ledger[i].kind == e.kind and ledger[i].id == e.id)
    val allowed = Set("invoice", "payment").contains(e.kind)
      and approved[slot(e.cell, e.order)] and e.amount >= 0 and not(duplicate)
    all {
      beforeLedger' = ledger, beforeTotals' = totals, approved' = approved,
      accepted' = allowed,
      ledger' = if (allowed) ledger.append(e) else ledger,
      totals' = if (allowed) totals.replaceAt(slot(e.cell, e.order),
        add(totals[slot(e.cell, e.order)], e)) else totals,
    }
  }
  action approve(cell: int, order: int): bool = all {
    approved' = approved.replaceAt(slot(cell, order), true),
    beforeLedger' = ledger, beforeTotals' = totals,
    ledger' = ledger, totals' = totals, accepted' = false,
  }
  action step = {
    nondet cell = 0.to(1).oneOf()
    nondet order = 0.to(1).oneOf()
    nondet id = 0.to(1).oneOf()
    nondet amount = 0.to(2).oneOf()
    // Updates, deletes and setters are deliberately inadmissible event kinds.
    nondet kind = Set("invoice", "payment", "update", "delete", "set_computed").oneOf()
    any { record({ cell: cell, order: order, kind: kind, id: id, amount: amount }), approve(cell, order) }
  }
  def replayEqualsProjection = 0.to(1).forall(cell => 0.to(1).forall(order =>
    totals[slot(cell, order)] == foldOrder(ledger, cell, order)))
  def refusalIsNoop = not(accepted) implies (ledger == beforeLedger and totals == beforeTotals)
  def appendOnly = ledger.slice(0, beforeLedger.length()) == beforeLedger
  def derived = totals.indices().forall(i =>
    (completed(totals[i]) implies invoiced(totals[i]))
    and outstanding(totals[i]) + totals[i].allocated_total == totals[i].invoiced_total)
  def invariant = replayEqualsProjection and refusalIsNoop and appendOnly and derived
  temporal safety = always(invariant)

  // A feasibility witness: real economic events can reach completion, while
  // the other cell/order stays untouched. This rules out all-refuse vacuity.
  run partialThenCompletedTest = init
    .then(approve(0, 0))
    .then(record({ cell: 0, order: 0, kind: "invoice", id: 0, amount: 2 }))
    .then(record({ cell: 0, order: 0, kind: "payment", id: 0, amount: 1 }))
    .then(all { assert(invariant and invoiced(totals[0]) and not(completed(totals[0]))),
      record({ cell: 0, order: 0, kind: "payment", id: 1, amount: 1 }) })
    .then(all { assert(invariant and completed(totals[0]) and outstanding(totals[0]) == 0
      and totals[1] == zero and totals[2] == zero and totals[3] == zero),
      record({ cell: 0, order: 1, kind: "invoice", id: 0, amount: 1 }) })
    .then(all { assert(invariant and not(accepted)), approve(0, 1) })
    .then(record({ cell: 0, order: 1, kind: "invoice", id: 0, amount: 1 }))
    .then(all { assert(invariant and not(accepted)), approve(1, 0) })
    .then(record({ cell: 1, order: 0, kind: "invoice", id: 0, amount: 1 }))
    .then(all { assert(invariant and accepted and invoiced(totals[2])), approve(1, 1) })
}
