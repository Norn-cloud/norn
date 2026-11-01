// One metal and a fixed custody owner; already-normalized exact fine ticks.
// Reference statechart, not a runtime/valuation/refinement correspondence.
module jewelry_refining {
  type Book = { stock: List[int], lost: List[int], delivered: List[int],
    assets: int, events: int }
  var book: Book
  var before: Book
  var reason: str
  var customerAction: bool
  pure val initial: Book = { stock: [6, 6], lost: [0, 0], delivered: [0, 0],
    assets: 6, events: 0 }
  action init = all { book' = initial, before' = initial,
    reason' = "", customerAction' = false }

  action refine(owner: int, input: int, output: int, loss: int, documented: bool): bool = {
    val verdict = if (input <= 0 or output < 0 or loss < 0) "Invalid fine amount"
      else if (input > book.stock[owner]) "Insufficient fine metal"
      else if (input != output + loss) "Fine input must equal outputs plus recorded loss"
      else if (loss > 0 and not(documented)) "Loss needs a recorded reason"
      else ""
    all {
      before' = book, reason' = verdict, customerAction' = owner == 1,
      book' = if (verdict != "") book else {
        stock: book.stock.replaceAt(owner, book.stock[owner] - input + output),
        lost: book.lost.replaceAt(owner, book.lost[owner] + loss),
        delivered: book.delivered,
        assets: book.assets - (if (owner == 0) loss else 0),
        events: book.events + 1,
      },
    }
  }
  action sell(owner: int, amount: int): bool = {
    val verdict = if (owner == 1) "Customer-owned metal cannot be sold"
      else if (amount <= 0 or amount > book.stock[owner]) "Insufficient fine metal"
      else ""
    all {
      before' = book, reason' = verdict, customerAction' = owner == 1,
      book' = if (verdict != "") book else {
        stock: book.stock.replaceAt(owner, book.stock[owner] - amount),
        lost: book.lost,
        delivered: book.delivered.replaceAt(owner, book.delivered[owner] + amount),
        assets: book.assets - amount, events: book.events + 1,
      },
    }
  }
  action returnCustody(amount: int): bool = {
    val verdict = if (amount <= 0 or amount > book.stock[1]) "Insufficient custody metal" else ""
    all {
      before' = book, reason' = verdict, customerAction' = true,
      book' = if (verdict != "") book else {
        stock: book.stock.replaceAt(1, book.stock[1] - amount), lost: book.lost,
        delivered: book.delivered.replaceAt(1, book.delivered[1] + amount),
        assets: book.assets, events: book.events + 1,
      },
    }
  }
  action step = {
    nondet owner = 0.to(1).oneOf()
    nondet input = 1.to(6).oneOf()
    nondet output = 0.to(6).oneOf()
    nondet loss = 0.to(6).oneOf()
    nondet documented = Set(false, true).oneOf()
    any { refine(owner, input, output, loss, documented),
      sell(owner, input), returnCustody(input) }
  }
  def nonnegative = 0.to(1).forall(i => book.stock[i] >= 0 and book.lost[i] >= 0
    and book.delivered[i] >= 0)
  def fineConserved = 0.to(1).forall(i =>
    book.stock[i] + book.lost[i] + book.delivered[i] == 6)
  def assetsExcludeCustody = book.assets == book.stock[0]
  def custodyIsolated = not(customerAction) or
    (book.assets == before.assets and book.stock[0] == before.stock[0])
  def refusedIsNoop = reason == "" or book == before

  run explicitLossTest = init.then(refine(1, 6, 5, 1, true))
    .then(assert(book.stock == [6, 5] and book.lost == [0, 1] and book.assets == 6))
  run silentLossTest = init.then(refine(0, 6, 5, 0, true))
    .then(assert(reason == "Fine input must equal outputs plus recorded loss" and book == initial))
  run custodySaleTest = init.then(sell(1, 2))
    .then(assert(reason == "Customer-owned metal cannot be sold" and book == initial))
  run undocumentedLossTest = init.then(refine(0, 6, 5, 1, false))
    .then(assert(reason == "Loss needs a recorded reason" and book == initial))
}
