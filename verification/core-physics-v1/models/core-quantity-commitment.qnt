// norn.core resource and commitment laws, bounded: quantity conservation,
// escrow and quota non-negativity, and commitment completion. One resource in
// one unit with two inside locations; two commitments due at times 1 and 2.
// The cell clock's deadline timer raises lapse or breach for every open
// commitment once its due time is reached. A bounded model of the pack's
// laws, not of the Rust kernel's quantity ledger or timers.
module core_quantity_commitment {
  type Commitment = { state: str, due: int }
  var stock: List[int]
  var held: List[int]
  var received: int
  var issued: int
  var commitments: List[Commitment]
  var before: List[Commitment]
  var now: int
  var outcome: str

  pure val LOCS = 0.to(1)
  pure val IDS = 0.to(1)
  pure val TERMINAL = Set("fulfilled", "cancelled", "breached")
  pure def transition(s: str, e: str): str =
    if (s == "requested" and e == "promise") "promised"
    else if (s == "promised" and e == "fulfil") "fulfilled"
    else if ((s == "requested" or s == "promised") and e == "cancel") "cancelled"
    else if (s == "requested" and e == "lapse") "cancelled"
    else if (s == "promised" and e == "breach") "breached"
    else "refused"
  pure def expire(c: Commitment, t: int): Commitment =
    if (c.due <= t and c.state == "requested") { state: "cancelled", due: c.due }
    else if (c.due <= t and c.state == "promised") { state: "breached", due: c.due }
    else c

  action init = all {
    stock' = [0, 0], held' = [0, 0], received' = 0, issued' = 0,
    commitments' = [{ state: "requested", due: 1 }, { state: "requested", due: 2 }],
    before' = [{ state: "requested", due: 1 }, { state: "requested", due: 2 }],
    now' = 0, outcome' = "init",
  }

  action unchangedCommitments = all { commitments' = commitments, before' = commitments, now' = now }
  action unchangedStock = all { stock' = stock, held' = held, received' = received, issued' = issued }

  // Quantity moves: receive from outside, issue to outside, transfer between
  // locations, and reserve, release or consume a hold. Each refuses rather
  // than spend quantity that is held or absent.
  action quantity(kind: str, loc: int, other: int, q: int): bool = {
    val avail = stock[loc] - held[loc]
    val ok = if (kind == "receive") true
      else if (kind == "issue" or kind == "transfer" or kind == "reserve") q <= avail and (kind != "transfer" or loc != other)
      else q <= held[loc]
    all {
      unchangedCommitments,
      outcome' = if (ok) "commit" else "insufficient",
      stock' = if (not(ok)) stock
        else if (kind == "receive") stock.replaceAt(loc, stock[loc] + q)
        else if (kind == "issue" or kind == "consume") stock.replaceAt(loc, stock[loc] - q)
        else if (kind == "transfer") stock.replaceAt(loc, stock[loc] - q).replaceAt(other, stock[other] + q)
        else stock,
      held' = if (not(ok)) held
        else if (kind == "reserve") held.replaceAt(loc, held[loc] + q)
        else if (kind == "release" or kind == "consume") held.replaceAt(loc, held[loc] - q)
        else held,
      received' = if (ok and kind == "receive") received + q else received,
      issued' = if (ok and (kind == "issue" or kind == "consume")) issued + q else issued,
    }
  }

  action event(i: int, e: str): bool = {
    val c = commitments[i]
    val timed = e == "lapse" or e == "breach"
    val nextState = transition(c.state, e)
    val ok = nextState != "refused" and not(timed and now < c.due)
    all {
      unchangedStock,
      before' = commitments,
      now' = now,
      outcome' = if (ok) "commit" else "refused",
      commitments' = if (ok) commitments.replaceAt(i, { state: nextState, due: c.due }) else commitments,
    }
  }

  // The clock advances and the deadline timer fires in the same cell turn.
  action tick = all {
    unchangedStock,
    before' = commitments,
    now' = now + 1,
    outcome' = "tick",
    commitments' = commitments.foldl([], (acc, c) => acc.append(expire(c, now + 1))),
  }

  action step = {
    nondet kind = Set("receive", "issue", "transfer", "reserve", "release", "consume").oneOf()
    nondet loc = LOCS.oneOf()
    nondet other = LOCS.oneOf()
    nondet q = 1.to(2).oneOf()
    nondet i = IDS.oneOf()
    nondet e = Set("promise", "fulfil", "cancel", "lapse", "breach", "delete").oneOf()
    nondet which = Set("quantity", "event", "tick").oneOf()
    if (which == "quantity") quantity(kind, loc, other, q)
    else if (which == "event") event(i, e)
    else if (now < 3) tick
    else event(i, e)
  }

  def typeOk = stock.length() == 2 and held.length() == 2 and commitments.length() == 2

  // core.law.quantity_conservation: stock in = stock out + on hand.
  def quantityConserved = received == issued + LOCS.fold(0, (acc, l) => acc + stock[l])

  // core.law.escrow_non_negative: holds are never negative and never exceed stock.
  def escrowNonNegative = LOCS.forall(l => 0 <= held[l] and held[l] <= stock[l])

  // core.law.commitment_completion: no commitment is dropped, terminal states
  // are absorbing, and none is still open once the clock is past its due time.
  def neverDropped = commitments.length() == 2 and IDS.forall(i => commitments[i].due == before[i].due)
  def terminalAbsorbing = IDS.forall(i => TERMINAL.contains(before[i].state) implies commitments[i] == before[i])
  def endsByDue = IDS.forall(i => commitments[i].due <= now implies TERMINAL.contains(commitments[i].state))
}
