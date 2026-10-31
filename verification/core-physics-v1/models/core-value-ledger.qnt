// norn.core value laws, bounded: conservation, posted immutability, and
// corrections as linked exact reversals. Four journal slots of two legs each,
// two accounts per side, two units, amounts in -2..2. The only admitted
// changes are post and reverse; amend and delete attempts are always refused.
// This is a bounded model of the pack's laws, not of the Rust kernel.
module core_value_ledger {
  type Leg = { account: int, unit: int, amount: int }
  type Entry = { status: str, legs: List[Leg], reverses: int }
  var book: List[Entry]
  var before: List[Entry]
  var outcome: str

  pure val SLOTS = 0.to(3)
  pure val emptyEntry: Entry = { status: "empty", legs: [], reverses: -1 }
  pure def negateLeg(l: Leg): Leg = { account: l.account, unit: l.unit, amount: 0 - l.amount }
  pure def unitSum(legs: List[Leg], u: int): int =
    legs.foldl(0, (acc, l) => if (l.unit == u) acc + l.amount else acc)
  pure def balanced(legs: List[Leg]): bool = 0.to(1).forall(u => unitSum(legs, u) == 0)
  pure def reversalCount(b: List[Entry], t: int): int =
    SLOTS.fold(0, (acc, s) => if (b[s].status == "posted" and b[s].reverses == t) acc + 1 else acc)

  action init = all {
    book' = [emptyEntry, emptyEntry, emptyEntry, emptyEntry],
    before' = [emptyEntry, emptyEntry, emptyEntry, emptyEntry],
    outcome' = "init",
  }

  action post(slot: int, legs: List[Leg]): bool = {
    val verdict = if (book[slot].status != "empty") "duplicate"
      else if (not(balanced(legs))) "unbalanced"
      else "commit"
    all {
      before' = book,
      outcome' = verdict,
      book' = if (verdict == "commit")
        book.replaceAt(slot, { status: "posted", legs: legs, reverses: -1 })
        else book,
    }
  }

  action reverse(slot: int, target: int): bool = {
    val original = book[target]
    val verdict = if (book[slot].status != "empty") "duplicate"
      else if (original.status != "posted") "not_found"
      else if (original.reverses != -1) "cannot_reverse_reversal"
      else if (reversalCount(book, target) != 0) "already_reversed"
      else "commit"
    all {
      before' = book,
      outcome' = verdict,
      book' = if (verdict == "commit")
        book.replaceAt(slot, { status: "posted", legs: original.legs.foldl([], (acc, l) => acc.append(negateLeg(l))), reverses: target })
        else book,
    }
  }

  // The pack exposes no edit or delete of a posted entry: every attempt is refused.
  action amendOrDelete(slot: int): bool = all {
    before' = book,
    outcome' = if (book[slot].status == "posted") "posted_immutable" else "not_found",
    book' = book,
  }

  action step = {
    nondet slot = SLOTS.oneOf()
    nondet target = SLOTS.oneOf()
    nondet a1 = 0.to(1).oneOf()
    nondet a2 = 2.to(3).oneOf()
    nondet u1 = 0.to(1).oneOf()
    nondet u2 = 0.to(1).oneOf()
    nondet x1 = (-2).to(2).oneOf()
    nondet x2 = (-2).to(2).oneOf()
    nondet kind = Set("post", "reverse", "amend").oneOf()
    if (kind == "post")
      post(slot, [{ account: a1, unit: u1, amount: x1 }, { account: a2, unit: u2, amount: x2 }])
    else if (kind == "reverse") reverse(slot, target)
    else amendOrDelete(slot)
  }

  def typeOk = book.length() == 4 and before.length() == 4

  // core.law.value_conservation: every posted journal nets to zero in every unit.
  def allPostedBalanced = SLOTS.forall(s => book[s].status == "posted" implies balanced(book[s].legs))

  // core.law.posted_immutable: a step never changes an entry that was posted before it.
  def postedImmutable = SLOTS.forall(s => before[s].status == "posted" implies book[s] == before[s])

  // core.law.correction_is_linked_reversal: a reversal links a posted original
  // that is not itself a reversal, and negates every leg exactly.
  def reversalLinkedExact = SLOTS.forall(s => (book[s].status == "posted" and book[s].reverses != -1) implies {
    val t = book[s].reverses
    and {
      book[t].status == "posted",
      book[t].reverses == -1,
      book[s].legs.length() == book[t].legs.length(),
      book[s].legs.indices().forall(i => book[s].legs[i] == negateLeg(book[t].legs[i])),
    }
  })

  def reversedAtMostOnce = SLOTS.forall(t => reversalCount(book, t) <= 1)
}
