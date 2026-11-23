// norn.core escrow / leave-hold conservation law (`core.law.escrow_non_negative`),
// bounded: balance accrual, single hold placement, atomic concurrent hold attempts
// (HR-LV-04), hold consume, hold release, and 7-day timer expiry (`hr.LeaveHold`
// `after: { days: 7 }`). Three hold slots over a single conserved balance, logical
// clock in days 0..21.
//
// `step` models the guarded atomic cell boundary; `unguardedConcurrentHold` /
// `stepUnguarded` explicitly demonstrate that unguarded concurrent holds reading
// a shared available balance violate `availableNonNegative` and
// `openHoldsBoundedByBalance`.
// This is a bounded model of the pack's law, not of the Rust kernel.
module core_escrow_holds {
  type Hold = { status: str, amount: int, createdAt: int, expiresAt: int }

  var now: int
  var balance: int
  var holds: List[Hold]
  var beforeBalance: int
  var beforeHolds: List[Hold]
  var outcome: str

  pure val SLOTS: Set[int] = 0.to(2)
  pure val HOLD_TTL_DAYS: int = 7
  pure val MAX_CLOCK: int = 21
  pure val MAX_BALANCE: int = 12
  pure val emptyHold: Hold = { status: "empty", amount: 0, createdAt: 0, expiresAt: 0 }

  pure def openHoldTotal(hs: List[Hold]): int =
    SLOTS.fold(0, (acc, s) => if (hs[s].status == "open") acc + hs[s].amount else acc)

  pure def availableOf(bal: int, hs: List[Hold]): int =
    bal - openHoldTotal(hs)

  pure def isTerminal(st: str): bool =
    st == "consumed" or st == "released" or st == "expired"

  action init = all {
    now' = 0,
    balance' = 0,
    holds' = [emptyHold, emptyHold, emptyHold],
    beforeBalance' = 0,
    beforeHolds' = [emptyHold, emptyHold, emptyHold],
    outcome' = "init",
  }

  action accrue(delta: int): bool = {
    val verdict = if (delta <= 0) "non_positive_accrual"
      else if (balance + delta > MAX_BALANCE) "bound_exceeded"
      else "commit"
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = if (verdict == "commit") balance + delta else balance,
      holds' = holds,
    }
  }

  action placeHold(slot: int, amount: int): bool = {
    val avail = availableOf(balance, holds)
    val verdict = if (holds[slot].status != "empty") "slot_occupied"
      else if (amount <= 0) "non_positive_hold"
      else if (amount > avail) "insufficient_available"
      else "commit"
    val nextHold: Hold = { status: "open", amount: amount, createdAt: now, expiresAt: now + HOLD_TTL_DAYS }
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = balance,
      holds' = if (verdict == "commit") holds.replaceAt(slot, nextHold) else holds,
    }
  }

  // Guarded atomic serialization of two concurrent hold requests (e.g. HR-LV-04:
  // balance = 3d with two competing 2d requests admits at most one).
  action concurrentHoldAttempt(s1: int, a1: int, s2: int, a2: int): bool = {
    val avail = availableOf(balance, holds)
    val validArgs = s1 != s2 and holds[s1].status == "empty" and holds[s2].status == "empty" and a1 > 0 and a2 > 0
    val h1: Hold = { status: "open", amount: a1, createdAt: now, expiresAt: now + HOLD_TTL_DAYS }
    val h2: Hold = { status: "open", amount: a2, createdAt: now, expiresAt: now + HOLD_TTL_DAYS }
    val verdict = if (not(validArgs)) "invalid_concurrent_hold"
      else if (a1 + a2 <= avail or a1 <= avail or a2 <= avail) "commit"
      else "insufficient_available"
    val nextHolds = if (not(validArgs)) holds
      else if (a1 + a2 <= avail) holds.replaceAt(s1, h1).replaceAt(s2, h2)
      else if (a1 <= avail) holds.replaceAt(s1, h1)
      else if (a2 <= avail) holds.replaceAt(s2, h2)
      else holds
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = balance,
      holds' = nextHolds,
    }
  }

  action consumeHold(slot: int): bool = {
    val h = holds[slot]
    val verdict = if (h.status != "open") "not_open"
      else if (balance < h.amount) "insufficient_balance"
      else "commit"
    val consumed: Hold = { status: "consumed", amount: h.amount, createdAt: h.createdAt, expiresAt: h.expiresAt }
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = if (verdict == "commit") balance - h.amount else balance,
      holds' = if (verdict == "commit") holds.replaceAt(slot, consumed) else holds,
    }
  }

  action releaseHold(slot: int): bool = {
    val h = holds[slot]
    val verdict = if (h.status != "open") "not_open" else "commit"
    val released: Hold = { status: "released", amount: h.amount, createdAt: h.createdAt, expiresAt: h.expiresAt }
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = balance,
      holds' = if (verdict == "commit") holds.replaceAt(slot, released) else holds,
    }
  }

  // Auto-expiry after 7 days (`hr.LeaveHold` `after: { days: 7 }`).
  action expireHold(slot: int): bool = {
    val h = holds[slot]
    val verdict = if (h.status != "open") "not_open"
      else if (now < h.expiresAt) "not_yet_expired"
      else "commit"
    val expired: Hold = { status: "expired", amount: h.amount, createdAt: h.createdAt, expiresAt: h.expiresAt }
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = balance,
      holds' = if (verdict == "commit") holds.replaceAt(slot, expired) else holds,
    }
  }

  action advanceClock(delta: int): bool = {
    val verdict = if (delta <= 0 or now + delta > MAX_CLOCK) "invalid_tick" else "commit"
    all {
      now' = if (verdict == "commit") now + delta else now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      outcome' = verdict,
      balance' = balance,
      holds' = holds,
    }
  }

  action step = {
    nondet s1 = SLOTS.oneOf()
    nondet s2 = SLOTS.oneOf()
    nondet a1 = (-1).to(4).oneOf()
    nondet a2 = 1.to(3).oneOf()
    nondet tick = Set(1, 3, 7).oneOf()
    nondet kind = Set("accrue", "place", "concurrent", "consume", "release", "expire", "tick").oneOf()
    if (kind == "accrue") accrue(a1)
    else if (kind == "place") placeHold(s1, a1)
    else if (kind == "concurrent") concurrentHoldAttempt(s1, a1, s2, a2)
    else if (kind == "consume") consumeHold(s1)
    else if (kind == "release") releaseHold(s1)
    else if (kind == "expire") expireHold(s1)
    else advanceClock(tick)
  }

  // Negative refutation action (excluded from guarded `step`): models two
  // concurrent hold requests that each check `avail` independently without
  // cell-level serialization. Running Apalache with `--step stepUnguarded`
  // refutes `availableNonNegative` and `openHoldsBoundedByBalance`
  // (e.g. accrue(3), then unguardedConcurrentHold(0, 2, 1, 2) => available = -1).
  action unguardedConcurrentHold(s1: int, a1: int, s2: int, a2: int): bool = {
    val avail = availableOf(balance, holds)
    val eachFitsAlone =
      s1 != s2 and
      holds[s1].status == "empty" and
      holds[s2].status == "empty" and
      a1 > 0 and a2 > 0 and
      a1 <= avail and a2 <= avail
    val h1: Hold = { status: "open", amount: a1, createdAt: now, expiresAt: now + HOLD_TTL_DAYS }
    val h2: Hold = { status: "open", amount: a2, createdAt: now, expiresAt: now + HOLD_TTL_DAYS }
    all {
      now' = now,
      beforeBalance' = balance,
      beforeHolds' = holds,
      balance' = balance,
      outcome' = if (eachFitsAlone) "unguarded_commit" else "refused",
      holds' = if (eachFitsAlone) holds.replaceAt(s1, h1).replaceAt(s2, h2) else holds,
    }
  }

  action stepUnguarded = {
    nondet s1 = SLOTS.oneOf()
    nondet s2 = SLOTS.oneOf()
    nondet a1 = 1.to(3).oneOf()
    nondet a2 = 1.to(3).oneOf()
    any {
      step,
      unguardedConcurrentHold(s1, a1, s2, a2),
    }
  }

  def typeOk = all {
    holds.length() == 3,
    beforeHolds.length() == 3,
    now >= 0,
    now <= MAX_CLOCK,
    SLOTS.forall(s =>
      holds[s].status.in(Set("empty", "open", "consumed", "released", "expired")) and
      holds[s].expiresAt >= holds[s].createdAt
    ),
  }

  def balanceNonNegative = balance >= 0

  // core.law.escrow_non_negative: holds are never negative, and open holds are strictly positive.
  def allHoldsNonNegative = SLOTS.forall(s =>
    holds[s].amount >= 0 and (holds[s].status == "open" implies holds[s].amount > 0)
  )

  // core.law.escrow_non_negative: total open holds never exceed the backing balance.
  def openHoldsBoundedByBalance = openHoldTotal(holds) <= balance

  // core.law.escrow_non_negative: available quantity is never negative.
  def availableNonNegative = availableOf(balance, holds) >= 0

  // Terminal holds (consumed, released, expired) never mutate again.
  def terminalHoldsImmutable = SLOTS.forall(s =>
    isTerminal(beforeHolds[s].status) implies holds[s] == beforeHolds[s]
  )

  // Refused steps leave balance and holds unchanged.
  def refusedStepPreservesState =
    (outcome != "init" and outcome != "commit") implies
      (balance == beforeBalance and holds == beforeHolds)
}
