// One metal, company and one custody owner; 0..6 exact normalized fine ticks.
// Fine ticks are micrograms * purity numerator; denominator 24,000,000 is
// implicit, with no division or rounding. All bounded amounts fit signed i128; parsing and
// overflow refusal in the runtime are unbound. Synthetic valuation: 1 per tick.
module jewelry_refining {
  type Book = { received: List[int], stock: List[int], lost: List[int],
    delivered: List[int], assets: int, processLoss: int, ledgerVersion: int,
    events: List[str], receipts: List[int] }
  var book: Book
  var before: Book
  var reason: str
  var customerAction: bool
  pure val initial: Book = { received: [6, 0], stock: [6, 0], lost: [0, 0],
    delivered: [0, 0], assets: 6, processLoss: 0, ledgerVersion: 0,
    events: [], receipts: [] }
  action init = all { book' = initial, before' = initial,
    reason' = "", customerAction' = false }

  action publish(candidate: Book, verdict: str, customer: bool, event: str): bool = all {
    before' = book, reason' = verdict, customerAction' = customer,
    book' = if (verdict != "") book else { ...candidate,
      events: book.events.append(event), receipts: book.receipts.append(book.events.length() + 1) },
  }
  action receiveCustody = publish({ ...book, received: [6, 6], stock: [book.stock[0], 6] },
    if (book.received[1] == 0) "" else "duplicate_receipt", true, "custody_receipt")

  action refine(owner: int, outputOwner: int, input: int, output: int, loss: int,
      documented: bool, processAccount: bool): bool = {
    val verdict = if (owner != outputOwner) "title_reassignment"
      else if (input <= 0 or output < 0 or loss < 0) "invalid_fine_amount"
      else if (input > book.stock[owner]) "insufficient_fine_metal"
      else if (input != output + loss) "fine_mismatch"
      else if (loss > 0 and not(documented)) "missing_loss_reason"
      else if (owner == 0 and loss > 0 and not(processAccount)) "missing_process_loss_account"
      else ""
    publish({ ...book,
      stock: book.stock.replaceAt(owner, book.stock[owner] - input + output),
      lost: book.lost.replaceAt(owner, book.lost[owner] + loss),
      assets: book.assets - (if (owner == 0) loss else 0),
      processLoss: book.processLoss + (if (owner == 0) loss else 0),
      ledgerVersion: book.ledgerVersion + (if (owner == 0) 1 else 0),
    }, verdict, owner == 1, "refine")
  }
  action sell(owner: int, amount: int): bool = {
    val verdict = if (owner == 1) "custody_sale"
      else if (amount <= 0 or amount > book.stock[owner]) "insufficient_fine_metal"
      else ""
    publish({ ...book, stock: book.stock.replaceAt(owner, book.stock[owner] - amount),
      delivered: book.delivered.replaceAt(owner, book.delivered[owner] + amount),
      assets: book.assets - amount, ledgerVersion: book.ledgerVersion + 1,
    }, verdict, owner == 1, "sale")
  }
  action returnCustody(amount: int): bool = {
    val verdict = if (amount <= 0 or amount > book.stock[1]) "insufficient_custody_metal" else ""
    publish({ ...book, stock: book.stock.replaceAt(1, book.stock[1] - amount),
      delivered: book.delivered.replaceAt(1, book.delivered[1] + amount),
    }, verdict, true, "custody_return")
  }
  action step = {
    nondet owner = 0.to(1).oneOf()
    nondet outputOwner = 0.to(1).oneOf()
    nondet input = 0.to(6).oneOf()
    nondet output = 0.to(6).oneOf()
    nondet loss = 0.to(6).oneOf()
    nondet documented = Set(false, true).oneOf()
    nondet processAccount = Set(false, true).oneOf()
    any { receiveCustody, refine(owner, outputOwner, input, output, loss, documented, processAccount),
      sell(owner, input), returnCustody(input) }
  }
  def nonnegative = 0.to(1).forall(i => book.stock[i] >= 0 and book.lost[i] >= 0
    and book.delivered[i] >= 0)
  def fineConserved = 0.to(1).forall(i =>
    book.received[i] == book.stock[i] + book.lost[i] + book.delivered[i])
  def lossValued = book.assets == book.stock[0] and book.processLoss == book.lost[0]
  def recordsAligned = book.events.length() == book.receipts.length()
    and book.ledgerVersion <= book.events.length()
  def custodyIsolated = not(customerAction) or
    (book.assets == before.assets and book.processLoss == before.processLoss
      and book.stock[0] == before.stock[0] and book.lost[0] == before.lost[0]
      and book.delivered[0] == before.delivered[0] and book.ledgerVersion == before.ledgerVersion)
  def refusedIsNoop = reason == "" or book == before

  action check(condition: bool): bool = all {
    assert(condition and nonnegative and fineConserved and lossValued
      and recordsAligned and custodyIsolated and refusedIsNoop),
    book' = book, before' = before, reason' = reason, customerAction' = customerAction,
  }
  run livePathTest = init.then(receiveCustody).then(refine(1, 1, 6, 5, 1, true, false))
    .then(returnCustody(5)).then(check(book.stock == [6, 0] and book.lost == [0, 1]
      and book.delivered == [0, 5] and book.assets == 6 and book.ledgerVersion == 0
      and book.events == ["custody_receipt", "refine", "custody_return"] and book.receipts == [1, 2, 3]))
  run companyLossTest = init.then(refine(0, 0, 6, 5, 1, true, true))
    .then(check(book.assets == 5 and book.processLoss == 1 and book.ledgerVersion == 1))
  run oneUnitMismatchTest = init.then(refine(0, 0, 6, 4, 1, true, true))
    .then(check(reason == "fine_mismatch" and book == initial))
  run missingLossTest = init.then(refine(0, 0, 6, 5, 0, true, true))
    .then(check(reason == "fine_mismatch" and book == initial))
  run excessOutputTest = init.then(refine(0, 0, 6, 6, 1, true, true))
    .then(check(reason == "fine_mismatch" and book == initial))
  run custodySaleTest = init.then(receiveCustody).then(sell(1, 2))
    .then(check(reason == "custody_sale" and book == before))
  run reassignmentTest = init.then(receiveCustody).then(refine(1, 0, 6, 5, 1, true, true))
    .then(check(reason == "title_reassignment" and book == before))
  run undocumentedLossTest = init.then(refine(0, 0, 6, 5, 1, false, true))
    .then(check(reason == "missing_loss_reason" and book == initial))
  run missingAccountTest = init.then(refine(0, 0, 6, 5, 1, true, false))
    .then(check(reason == "missing_process_loss_account" and book == initial))
  run refusedAfterCommitTest = init.then(refine(0, 0, 6, 5, 1, true, true))
    .then(refine(0, 0, 5, 4, 0, true, true))
    .then(check(reason == "fine_mismatch" and book == before and book.receipts == [1]))
}
