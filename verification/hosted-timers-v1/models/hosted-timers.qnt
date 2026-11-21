// The hosted cell's time-driven state (TIMER-ALARM-1, TIMER-RETRY-1,
// TIMER-PIN-1, DECISION-TERMINAL-1) as #1464 merged it (962938d).
//
// Source of truth for the shape:
//   apps/norn-cell-do/wake.mjs        beforeWrite, afterWrite, opened, alarm
//   apps/norn-cell-do/worker.mjs      open, fetch, alarm
//   crates/norn-api/src/timers.rs     run_due, eligible, next_wake, MAX_DUE_STEPS
//   crates/norn-cell/src/due_retry.rs record_due_failure, backoff, eligibility
//   crates/norn-api/src/decisions.rs  expire, resolve_call, stalled_expiries
//   crates/norn-api/src/run_routes.rs pinned_flow
//
// The design this model fixes (each is a mechanism a mutant removes):
//   W1 write-ahead guard: before a request may write, an alarm no later than
//      RECOVERY from now is durable (beforeWrite);
//   W2 after the request, whatever its reply, the alarm moves to the cell's
//      next deadline when that is sooner; this rearm can fail, and the guard
//      then stays (afterWrite, earliest);
//   W3 an alarm first sets a recovery wake, runs the due work, then sets the
//      alarm exactly to the next deadline; while a request is still writing
//      it keeps the recovery wake, and only a sooner deadline replaces it;
//   W4 opening a serving cell arms for whatever is already due (a cell from
//      before alarms holds deadlines and no alarm);
//   R1 retry eligibility is persisted per item incarnation with a doubling
//      backoff; a failed try counts against the per-wake budget;
//   R2 due items are tried by their ORIGINAL deadline (then kind, then id)
//      across all three kinds; retry eligibility only filters;
//   E1 an expiry records the step's Expired result, then commits the
//      resolution consuming it; a recorded, unresolved expiry stays
//      scheduled and is resumed with exactly that result (recoverable);
//   E2 the first recorded result wins: nothing overwrites a recorded expiry,
//      and a resolution is always the recorded result (atomic);
//   P1 a flow timer runs the rule of the definitions its run STARTED under,
//      never the rule active when it was scheduled or when it fires.
//
// Time is logical: one unit is 60 s (RECOVERY_MS = STUCK_RETRY_MS). No time
// passes while one alarm handler runs (wasm calls are synchronous); requests
// interleave with it at its awaits. Alarm delivery is fair (`tick`: the clock
// never passes a durable alarm by more than FAIR without it firing). Isolate
// losses and failed resolving commits are bounded by FAULTS. Progress is
// bounded response under those assumptions, checked as invariants to the
// stated depth; it is not unbounded LTL liveness.
module hosted_timers {
  type Mut = {
    noGuard: bool,                  // W1 removed: no write-ahead guard
    laterAlarmReplaces: bool,       // W2/W4: rearm and open set the alarm exactly, even later
    noRecoveryWake: bool,           // W3: the delivered alarm is gone until the handler's end
    dropRecoveryWhileWriting: bool, // W3: the end of a run sets the exact alarm while a request writes
    noOpenArm: bool,                // W4: opening a cell does not arm it
    splitExpiry: bool,              // E1: a recorded expiry leaves the schedule (round-1 shape)
    noFirstWins: bool,              // E2: a decider's result overwrites a recorded expiry
    activeRuleTimer: bool,          // P1: the timer runs the rule active when it fires
    scheduledRuleTimer: bool,       // P1: the timer runs the rule active when it was scheduled
    failureStopsPass: bool,         // R1: the first failure ends the pass, nothing persisted
    advisoryRetry: bool,            // R1: retry eligibility is not persisted
    deferOverflow: bool,            // R2: a pass that leaves eligible work asks for the recovery wake, not at once
  }
  pure val NOMUT: Mut = {
    noGuard: false, laterAlarmReplaces: false, noRecoveryWake: false,
    dropRecoveryWhileWriting: false, noOpenArm: false, splitExpiry: false,
    noFirstWins: false, activeRuleTimer: false, scheduledRuleTimer: false,
    failureStopsPass: false, advisoryRetry: false, deferOverflow: false,
  }

  // Bounds. Three item slots against a per-wake budget of two stand in for
  // MAX_DUE_STEPS = 64 against an unbounded backlog (the conformance test
  // drives each slot as 32 real rows). The rank helpers below range over
  // these slots; change them together.
  pure val ITEMS = Set(1, 2, 3)
  // One request in flight against the alarm: the writing count of wake.mjs
  // is then 0 or 1 (more requests only repeat its guard and rearm).
  pure val REQS = Set(1)
  pure val KINDS = Set("reservation", "timer", "decision")
  pure val RULES = Set(1, 2)
  pure val BUDGET = 2
  pure val RECOVERY = 1
  pure val FAIR = 1
  pure val FAULTS = 1
  pure val MAX_TIME = 7
  pure val LEGACY_DEADLINES = Set(0, 1, 2, 3)
  pure val NONE = -1
  // A healthy item is handled within this long after its deadline when no
  // fault happens (two passes may go to older failing items, R2 + R1).
  pure val LATENESS = 3

  // backoff(attempts) of due_retry.rs over STUCK_RETRY_MS = 1 unit and
  // MAX_RETRY_MS = 60 units: 1, 2, 4, 8 (the cap is beyond MAX_TIME).
  pure def backoff(a: int): int = if (a <= 1) 1 else if (a == 2) 2 else if (a == 3) 4 else 8
  pure def kindRank(k: str): int = if (k == "reservation") 0 else if (k == "timer") 1 else 2
  pure def minWake(a: int, b: int): int =
    if (a == NONE) b else if (b == NONE) a else if (a < b) a else b
  pure def max(a: int, b: int): int = if (a > b) a else b

  // Durable cell state.
  var now: int
  var opened: bool                 // a serving isolate opened the cell since the deploy
  var live: int -> bool            // the slot holds a pending item
  var dl: int -> int               // the item's original deadline (kept after it is handled)
  var kind: int -> str
  var healthy: int -> bool         // false: its handling fails (corrupt row, unavailable pin, retired flow)
  var attempts: int -> int         // failed tries at this incarnation (_norn_due_retry)
  var retryAt: int -> int          // its retry eligibility, 0 = none
  var pinned: int -> int           // a flow timer's run's starting rule
  var ruleAtWrite: int -> int      // the rule active when the timer was scheduled
  var firedRule: int -> int        // the rule the fired timer ran, 0 = not fired
  var activeAtFire: int -> int     // the rule active when it fired
  var dec: int -> str              // "none" | "open" | "recorded" | "resolved"
  var decRec: int -> str           // the recorded result: "" | "expired" | "decided"
  var firstRec: int -> str         // the first result ever recorded
  var decRes: int -> str           // the committed resolution
  var resumed: int -> bool         // resolved by resuming a stalled record
  var wake: int                    // the durable alarm, NONE = none
  var activeRule: int
  var seenRules: Set[int]          // rules that have been active (a run may have started under any)
  // The isolate (volatile).
  var phase: str                   // "idle" | "ran": the handler ran its due work, its final set pending
  var req: int -> str              // "none" | "guarded" | "committed"
  // History, for the properties.
  var faults: int
  var skipped: int -> int          // consecutive completed passes that left the item due and untried (kept once handled)
  var everSkipped: int -> bool
  var doneAt: int -> int           // when it was handled, NONE before
  var firstFail: int -> int        // when it first failed, NONE before
  var lostIsolate: bool            // an isolate was lost (a fault apart from a failed resolving commit)

  def eligible(i: int): int = max(dl.get(i), retryAt.get(i))
  def writing: bool = REQS.exists(r => req.get(r) != "none")
  // cell.nextDue(now): the earliest eligibility of any pending item (it may have passed).
  def dueOf(i: int): int = if (live.get(i)) eligible(i) else NONE
  def nextDue: int = minWake(minWake(dueOf(1), dueOf(2)), dueOf(3))
  // Wake.earliest: move the alarm to `at` (no earlier than now) when none is set or the set one is later.
  def earliest(at: int): int = if (at == NONE) wake else minWake(wake, max(at, now))
  def exactly(at: int): int = if (at == NONE) wake else max(at, now)

  // ---- One run_due pass at `now` ----------------------------------------
  def cand: Set[int] = ITEMS.filter(i => live.get(i) and eligible(i) <= now)
  // R2: the original deadline, then kind, then id.
  def before(j: int, i: int): bool =
    dl.get(j) < dl.get(i)
      or (dl.get(j) == dl.get(i) and (kindRank(kind.get(j)) < kindRank(kind.get(i))
        or (kindRank(kind.get(j)) == kindRank(kind.get(i)) and j < i)))
  // Counted by hand over the three slots: Apalache encodes Cardinality and
  // fold far more slowly than this arithmetic.
  pure def b2i(b: bool): int = if (b) 1 else 0
  def inCand(i: int): bool = live.get(i) and eligible(i) <= now
  def rank(i: int): int =
    b2i(inCand(1) and before(1, i)) + b2i(inCand(2) and before(2, i)) + b2i(inCand(3) and before(3, i))
  def chosen: Set[int] = cand.filter(i => rank(i) < BUDGET)
  // Whether trying `i` succeeds; `cf` is the item whose resolving commit fails (NONE: none).
  def ok(i: int, cf: int): bool = healthy.get(i) and cf != i
  // The items tried: the chosen ones, up to `cut` when the isolate is lost;
  // with failureStopsPass, up to the first failure.
  def tried(cut: int, lost: bool, cf: int, m: Mut): Set[int] =
    val upto = chosen.filter(i => not(lost) or rank(i) < cut)
    if (m.failureStopsPass)
      upto.filter(i => upto.forall(j => rank(j) < rank(i) implies ok(j, cf)))
    else upto

  // The state a pass leaves, as one simultaneous update of the item maps.
  action runDue(cut: int, lost: bool, cf: int, m: Mut): bool = {
    val t = tried(cut, lost, cf, m)
    val handled = t.filter(i => ok(i, cf))
    val failed = t.filter(i => not(ok(i, cf)))
    // A healthy decision records its expiry before the resolving commit
    // (which may fail); a stalled one keeps its record.
    val recorded = t.filter(i => kind.get(i) == "decision" and healthy.get(i))
    val persist = not(m.advisoryRetry or m.failureStopsPass)
    all {
      live' = ITEMS.mapBy(i =>
        if (handled.contains(i)) false
        else if (m.splitExpiry and recorded.contains(i)) false
        else live.get(i)),
      attempts' = ITEMS.mapBy(i =>
        if (handled.contains(i)) 0
        else if (failed.contains(i) and persist) attempts.get(i) + 1
        else attempts.get(i)),
      retryAt' = ITEMS.mapBy(i =>
        if (handled.contains(i)) 0
        else if (failed.contains(i) and persist) now + backoff(attempts.get(i) + 1)
        else retryAt.get(i)),
      firedRule' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "timer")
          (if (m.activeRuleTimer) activeRule
           else if (m.scheduledRuleTimer) ruleAtWrite.get(i)
           else pinned.get(i))
        else firedRule.get(i)),
      activeAtFire' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "timer") activeRule else activeAtFire.get(i)),
      dec' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "decision") "resolved"
        else if (recorded.contains(i)) "recorded"
        else dec.get(i)),
      decRec' = ITEMS.mapBy(i =>
        if (recorded.contains(i) and dec.get(i) == "open") "expired" else decRec.get(i)),
      firstRec' = ITEMS.mapBy(i =>
        if (recorded.contains(i) and firstRec.get(i) == "") "expired" else firstRec.get(i)),
      // E1/E2: the resolution is exactly the recorded result.
      decRes' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "decision")
          (if (dec.get(i) == "open") "expired" else decRec.get(i))
        else decRes.get(i)),
      resumed' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "decision" and dec.get(i) == "recorded") true
        else resumed.get(i)),
      // The progress history (doneAt, firstFail, skipped, everSkipped) is
      // kept by the whole rounds of stepProgress (roundAt), where the
      // progress invariants and their witnesses are checked; no safety
      // invariant or safety-step witness reads it, so this step leaves it.
      doneAt' = doneAt, firstFail' = firstFail, skipped' = skipped, everSkipped' = everSkipped,
    }
  }
  // wake.alarm's final set, after the due work.
  pure def finalWake(n: int, nx: int, w: bool, m: Mut): int =
    if (w and not(m.dropRecoveryWhileWriting))
      (if (nx != NONE and max(nx, n) < n + RECOVERY) max(nx, n) else n + RECOVERY)
    else if (nx != NONE) max(nx, n) else NONE

  // ---- Init -----------------------------------------------------------------

  action initWith(d0: int -> int, kd: int -> str, hl: int -> bool, op: bool,
                  pn: int -> int, rw: int -> int, ar: int, m: Mut): bool = all {
    now' = 0,
    opened' = op,
    live' = ITEMS.mapBy(i => d0.get(i) != NONE),
    dl' = d0,
    kind' = kd,
    healthy' = hl,
    attempts' = ITEMS.mapBy(_ => 0),
    retryAt' = ITEMS.mapBy(_ => 0),
    pinned' = pn,
    ruleAtWrite' = rw,
    firedRule' = ITEMS.mapBy(_ => 0),
    activeAtFire' = ITEMS.mapBy(_ => 0),
    dec' = ITEMS.mapBy(i => if (d0.get(i) != NONE and kd.get(i) == "decision") "open" else "none"),
    decRec' = ITEMS.mapBy(_ => ""),
    firstRec' = ITEMS.mapBy(_ => ""),
    decRes' = ITEMS.mapBy(_ => ""),
    resumed' = ITEMS.mapBy(_ => false),
    // A cell from before alarms holds deadlines and no alarm; an opened one
    // was armed by its open (W4).
    wake' = if (op and not(m.noOpenArm))
        minWake(minWake(d0.get(1), d0.get(2)), d0.get(3))
      else NONE,
    activeRule' = ar,
    seenRules' = if (pn == ITEMS.mapBy(_ => 1) and rw == pn and ar == 1) Set(1) else RULES,
    phase' = "idle",
    req' = REQS.mapBy(_ => "none"),
    faults' = 0,
    skipped' = ITEMS.mapBy(_ => 0),
    everSkipped' = ITEMS.mapBy(_ => false),
    doneAt' = ITEMS.mapBy(_ => NONE),
    firstFail' = ITEMS.mapBy(_ => NONE),
    lostIsolate' = false,
  }
  action initAs(op: bool, m: Mut): bool = {
    nondet d0 = ITEMS.setOfMaps(LEGACY_DEADLINES.union(Set(NONE))).oneOf()
    nondet kd = ITEMS.setOfMaps(KINDS).oneOf()
    nondet hl = ITEMS.setOfMaps(Set(true, false)).oneOf()
    // A legacy cell may already hold timers of runs that started under an
    // earlier rule, scheduled before or after a definitions change.
    nondet pn = ITEMS.setOfMaps(RULES).oneOf()
    nondet rw = ITEMS.setOfMaps(RULES).oneOf()
    nondet ar = RULES.oneOf()
    initWith(d0, kd, hl, op, pn, rw, ar, m)
  }
  // Safety: a cell from before the deploy, not yet opened.
  action init = initAs(false, NOMUT)
  // Progress: the deploy-time cell, opened (and armed) at time 0.
  action initOpened = initAs(true, NOMUT)
  // Conformance (tests/hosted_timers_quint.rs): reservations that fail and
  // flow timers that succeed or fail, opened at time 0.
  action initConform = {
    nondet d0 = ITEMS.setOfMaps(LEGACY_DEADLINES.union(Set(NONE))).oneOf()
    nondet hl = ITEMS.setOfMaps(Set(true, false)).oneOf()
    initWith(d0, ITEMS.mapBy(i => if (hl.get(i)) "timer" else (if (i == 2) "reservation" else "timer")),
      hl, true, ITEMS.mapBy(_ => 1), ITEMS.mapBy(_ => 1), 1, NOMUT)
  }

  // ---- The isolate's steps ----------------------------------------------------

  // A request opens the cell (W4).
  action open(m: Mut): bool = all {
    phase == "idle",
    opened' = true,
    wake' = if (m.noOpenArm) wake else if (m.laterAlarmReplaces) exactly(nextDue) else earliest(nextDue),
    now' = now, live' = live, dl' = dl, kind' = kind, healthy' = healthy, attempts' = attempts,
    retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite, firedRule' = firedRule,
    activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec, firstRec' = firstRec,
    decRes' = decRes, resumed' = resumed, activeRule' = activeRule, seenRules' = seenRules,
    phase' = phase, req' = req, faults' = faults, skipped' = skipped, everSkipped' = everSkipped,
    doneAt' = doneAt, firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  // beforeWrite (W1): the guard is durable before the request may write.
  action guard(r: int, m: Mut): bool = all {
    opened,
    req.get(r) == "none",
    req' = req.set(r, "guarded"),
    wake' = if (m.noGuard) wake else earliest(now + RECOVERY),
    now' = now, opened' = opened, live' = live, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec,
    firstRec' = firstRec, decRes' = decRes, resumed' = resumed, activeRule' = activeRule,
    seenRules' = seenRules, phase' = phase, faults' = faults, skipped' = skipped,
    everSkipped' = everSkipped, doneAt' = doneAt, firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  // The request commits a new item with a deadline (a reservation with an
  // expiry, a flow timer of a run started under `pin`, a decision). A new
  // incarnation starts with no retry record.
  action commit(r: int, i: int, off: int, k: str, h: bool, pin: int): bool = all {
    val d = now + off
    all {
    req.get(r) == "guarded",
    not(live.get(i)),
    d <= MAX_TIME,
    seenRules.contains(pin),
    req' = req.set(r, "committed"),
    live' = live.set(i, true),
    dl' = dl.set(i, d),
    kind' = kind.set(i, k),
    healthy' = healthy.set(i, h),
    attempts' = attempts.set(i, 0),
    retryAt' = retryAt.set(i, 0),
    pinned' = pinned.set(i, pin),
    ruleAtWrite' = ruleAtWrite.set(i, activeRule),
    firedRule' = firedRule.set(i, 0),
    activeAtFire' = activeAtFire.set(i, 0),
    dec' = dec.set(i, if (k == "decision") "open" else "none"),
    decRec' = decRec.set(i, ""),
    firstRec' = firstRec.set(i, ""),
    decRes' = decRes.set(i, ""),
    resumed' = resumed.set(i, false),
    skipped' = skipped.set(i, 0),
    everSkipped' = everSkipped.set(i, false),
    doneAt' = doneAt.set(i, NONE),
    firstFail' = firstFail.set(i, NONE),
    lostIsolate' = lostIsolate,
    now' = now, opened' = opened, wake' = wake, activeRule' = activeRule, seenRules' = seenRules,
    phase' = phase, faults' = faults,
    }
  }

  // afterWrite (W2), whatever the reply; the scheduling call can fail.
  action rearm(r: int, okSet: bool, m: Mut): bool = all {
    req.get(r) != "none",
    req' = req.set(r, "none"),
    wake' = if (not(okSet)) wake else if (m.laterAlarmReplaces) exactly(nextDue) else earliest(nextDue),
    now' = now, opened' = opened, live' = live, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec,
    firstRec' = firstRec, decRes' = decRes, resumed' = resumed, activeRule' = activeRule,
    seenRules' = seenRules, phase' = phase, faults' = faults, skipped' = skipped,
    everSkipped' = everSkipped, doneAt' = doneAt, firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  // A decider resolves an open decision: its result is recorded first
  // (complete_parked, first wins) and committed. Mutant: it overwrites a
  // recorded expiry.
  action decide(i: int, m: Mut): bool = all {
    live.get(i),
    kind.get(i) == "decision",
    dec.get(i) == "open" or (m.noFirstWins and dec.get(i) == "recorded"),
    live' = live.set(i, false),
    dec' = dec.set(i, "resolved"),
    decRec' = decRec.set(i, "decided"),
    firstRec' = firstRec.set(i, if (firstRec.get(i) == "") "decided" else firstRec.get(i)),
    decRes' = decRes.set(i, "decided"),
    now' = now, opened' = opened, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, resumed' = resumed, wake' = wake,
    activeRule' = activeRule, seenRules' = seenRules, phase' = phase, req' = req,
    faults' = faults, skipped' = skipped, everSkipped' = everSkipped, doneAt' = doneAt,
    firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  // The clock advances, never past a durable alarm by more than FAIR.
  action tick(t: int): bool = all {
    phase == "idle",
    t > now, t <= MAX_TIME,
    wake == NONE or t <= wake + FAIR,
    now' = t,
    opened' = opened, live' = live, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec,
    firstRec' = firstRec, decRes' = decRes, resumed' = resumed, wake' = wake,
    activeRule' = activeRule, seenRules' = seenRules, phase' = phase, req' = req,
    faults' = faults, skipped' = skipped, everSkipped' = everSkipped, doneAt' = doneAt,
    firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  // The runtime delivers the alarm: the recovery wake goes down first (W3),
  // then the due work runs. With `lost`, the isolate is lost after the first
  // `cut` tries (and with it every request in flight); `cf` are the
  // decisions whose resolving commit fails. Losses and failed commits are
  // faults.
  action fire(cut: int, lost: bool, cf: int, m: Mut): bool = all {
    phase == "idle",
    wake != NONE,
    now >= wake,
    faults + b2i(lost) + b2i(cf != NONE) <= FAULTS,
    cf == NONE or (live.get(cf) and kind.get(cf) == "decision" and healthy.get(cf)),
    runDue(cut, lost, cf, m),
    wake' = if (m.noRecoveryWake) NONE else now + RECOVERY,
    phase' = if (lost) "idle" else "ran",
    req' = if (lost) REQS.mapBy(_ => "none") else req,
    faults' = faults + b2i(lost) + b2i(cf != NONE),
    lostIsolate' = (lostIsolate or lost),
    now' = now, opened' = true, dl' = dl, kind' = kind, healthy' = healthy, pinned' = pinned,
    ruleAtWrite' = ruleAtWrite, activeRule' = activeRule, seenRules' = seenRules,
  }

  // The end of the alarm run: the exact next alarm, or the recovery wake
  // while a request writes (W3).
  action finish(m: Mut): bool = all {
    phase == "ran",
    phase' = "idle",
    wake' = finalWake(now, nextDue, writing, m),
    now' = now, opened' = opened, live' = live, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec,
    firstRec' = firstRec, decRes' = decRes, resumed' = resumed, activeRule' = activeRule,
    seenRules' = seenRules, req' = req, faults' = faults, skipped' = skipped,
    everSkipped' = everSkipped, doneAt' = doneAt, firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  // The isolate is lost between steps (eviction, a throw the runtime gives
  // up on): the handler's final set and every request's rearm never run.
  action lose: bool = all {
    phase == "ran" or writing,
    faults < FAULTS,
    phase' = "idle",
    req' = REQS.mapBy(_ => "none"),
    faults' = faults + 1,
    lostIsolate' = true,
    now' = now, opened' = opened, live' = live, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec,
    firstRec' = firstRec, decRes' = decRes, resumed' = resumed, wake' = wake,
    activeRule' = activeRule, seenRules' = seenRules, skipped' = skipped,
    everSkipped' = everSkipped, doneAt' = doneAt, firstFail' = firstFail,
  }

  // A definitions change activates another rule; running flows keep their pin.
  action changeRule(r: int): bool = all {
    r != activeRule,
    activeRule' = r,
    seenRules' = seenRules.union(Set(r)),
    now' = now, opened' = opened, live' = live, dl' = dl, kind' = kind, healthy' = healthy,
    attempts' = attempts, retryAt' = retryAt, pinned' = pinned, ruleAtWrite' = ruleAtWrite,
    firedRule' = firedRule, activeAtFire' = activeAtFire, dec' = dec, decRec' = decRec,
    firstRec' = firstRec, decRes' = decRes, resumed' = resumed, wake' = wake, phase' = phase,
    req' = req, faults' = faults, skipped' = skipped, everSkipped' = everSkipped,
    doneAt' = doneAt, firstFail' = firstFail,
    lostIsolate' = lostIsolate,
  }

  action stepWith(m: Mut): bool = {
    nondet r = REQS.oneOf()
    nondet i = ITEMS.oneOf()
    nondet d = 1.to(MAX_TIME).oneOf()
    // A new deadline 1..3 units ahead: only its order against the alarm and
    // the other deadlines matters.
    nondet off = 1.to(3).oneOf()
    nondet k = KINDS.oneOf()
    nondet h = Set(true, false).oneOf()
    nondet b = Set(true, false).oneOf()
    nondet rule = RULES.oneOf()
    nondet cut = 0.to(BUDGET).oneOf()
    // At most one failed resolving commit per pass (FAULTS = 1).
    nondet cf = ITEMS.union(Set(NONE)).oneOf()
    any {
      open(m),
      guard(r, m),
      commit(r, i, off, k, h, rule),
      rearm(r, b, m),
      decide(i, m),
      tick(d),
      fire(cut, b, cf, m),
      finish(m),
      lose,
      changeRule(rule),
    }
  }

  // ---- Whole rounds, for the progress properties ------------------------------
  // The clock moves to `t` (at most FAIR past the alarm) and the alarm runs
  // its whole pass, completed or lost after `cut` tries; no requests (the
  // deploy-time cell's items only). Fusing tick and pass keeps one alarm
  // delivery per step.
  action round(t: int, cut: int, lost: bool, cf: int, m: Mut): bool = all {
    phase == "idle",
    wake != NONE,
    t >= now, t >= wake, t <= wake + FAIR, t <= MAX_TIME,
    // The pass runs at `t`: evaluate it at the new time.
    now' = t,
    faults + b2i(lost) + b2i(cf != NONE) <= FAULTS,
    cf == NONE or (live.get(cf) and kind.get(cf) == "decision" and healthy.get(cf)),
    roundAt(t, cut, lost, cf, m),
  }
  // A pass at time t, written against a state whose clock reads t.
  action roundAt(t: int, cut: int, lost: bool, cf: int, m: Mut): bool = {
    // The candidates and order at time t (cand reads `now`; restate at t).
    val ct = ITEMS.filter(i => live.get(i) and eligible(i) <= t)
    def inCt(j: int): bool = live.get(j) and eligible(j) <= t
    val rk = ITEMS.mapBy(i =>
      b2i(inCt(1) and before(1, i)) + b2i(inCt(2) and before(2, i)) + b2i(inCt(3) and before(3, i)))
    val ctCount = b2i(inCt(1)) + b2i(inCt(2)) + b2i(inCt(3))
    val ch = ct.filter(i => rk.get(i) < BUDGET)
    val upto = ch.filter(i => not(lost) or rk.get(i) < cut)
    val tr = if (m.failureStopsPass)
        upto.filter(i => upto.forall(j => rk.get(j) < rk.get(i) implies ok(j, cf)))
      else upto
    val handled = tr.filter(i => ok(i, cf))
    val failed = tr.filter(i => not(ok(i, cf)))
    val recorded = tr.filter(i => kind.get(i) == "decision" and healthy.get(i))
    val persist = not(m.advisoryRetry or m.failureStopsPass)
    val live2 = ITEMS.mapBy(i =>
      if (handled.contains(i)) false
      else if (m.splitExpiry and recorded.contains(i)) false
      else live.get(i))
    val ra2 = ITEMS.mapBy(i =>
      if (handled.contains(i)) 0
      else if (failed.contains(i) and persist) t + backoff(attempts.get(i) + 1)
      else retryAt.get(i))
    val stopped = m.failureStopsPass and ch.exists(i => not(tr.contains(i)))
    def due2(i: int): int = if (live2.get(i)) max(dl.get(i), ra2.get(i)) else NONE
    // Work left for the next wake: wake at once (DueWork::next = the wake time).
    val nx = if (ctCount > BUDGET or stopped) (if (m.deferOverflow) t + RECOVERY else t)
      else minWake(minWake(due2(1), due2(2)), due2(3))
    all {
      live' = live2,
      retryAt' = ra2,
      attempts' = ITEMS.mapBy(i =>
        if (handled.contains(i)) 0
        else if (failed.contains(i) and persist) attempts.get(i) + 1
        else attempts.get(i)),
      dec' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "decision") "resolved"
        else if (recorded.contains(i)) "recorded"
        else dec.get(i)),
      decRec' = ITEMS.mapBy(i =>
        if (recorded.contains(i) and dec.get(i) == "open") "expired" else decRec.get(i)),
      firstRec' = ITEMS.mapBy(i =>
        if (recorded.contains(i) and firstRec.get(i) == "") "expired" else firstRec.get(i)),
      decRes' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "decision")
          (if (dec.get(i) == "open") "expired" else decRec.get(i))
        else decRes.get(i)),
      resumed' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "decision" and dec.get(i) == "recorded") true
        else resumed.get(i)),
      firedRule' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "timer") pinned.get(i) else firedRule.get(i)),
      activeAtFire' = ITEMS.mapBy(i =>
        if (handled.contains(i) and kind.get(i) == "timer") activeRule else activeAtFire.get(i)),
      doneAt' = ITEMS.mapBy(i => if (handled.contains(i)) t else doneAt.get(i)),
      firstFail' = ITEMS.mapBy(i =>
        if (failed.contains(i) and firstFail.get(i) == NONE) t else firstFail.get(i)),
      skipped' = ITEMS.mapBy(i =>
        if (failed.contains(i)) 0
        else if (handled.contains(i)) skipped.get(i)
        else if (not(lost) and ct.contains(i)) skipped.get(i) + 1
        else skipped.get(i)),
      everSkipped' = ITEMS.mapBy(i =>
        everSkipped.get(i) or (not(lost) and ct.contains(i) and not(tr.contains(i)))),
      wake' = if (lost) t + RECOVERY else finalWake(t, nx, false, m),
      faults' = faults + b2i(lost) + b2i(cf != NONE),
      lostIsolate' = (lostIsolate or lost),
      opened' = opened, dl' = dl, kind' = kind, healthy' = healthy, pinned' = pinned,
      ruleAtWrite' = ruleAtWrite, activeRule' = activeRule, seenRules' = seenRules,
      phase' = phase, req' = req,
    }
  }
  // Time passes with nothing to fire.
  action idle(t: int): bool = all { wake == NONE or t < wake, tick(t) }

  action progressWith(m: Mut): bool = {
    nondet t = 0.to(MAX_TIME).oneOf()
    nondet cut = 0.to(BUDGET).oneOf()
    nondet lost = Set(true, false).oneOf()
    // At most one failed resolving commit per pass (FAULTS = 1).
    nondet cf = ITEMS.union(Set(NONE)).oneOf()
    any { idle(t), round(t, cut, lost, cf, m) }
  }

  // Conformance traces: completed passes and idle time, no faults.
  action stepConform = {
    nondet t = 0.to(MAX_TIME).oneOf()
    any { idle(t), round(t, BUDGET, false, NONE, NOMUT) }
  }

  action step = stepWith(NOMUT)
  action stepProgress = progressWith(NOMUT)
  action stepNoGuard = stepWith({ ...NOMUT, noGuard: true })
  action stepLaterAlarmReplaces = stepWith({ ...NOMUT, laterAlarmReplaces: true })
  action stepNoRecoveryWake = stepWith({ ...NOMUT, noRecoveryWake: true })
  action stepDropRecoveryWhileWriting = stepWith({ ...NOMUT, dropRecoveryWhileWriting: true })
  action stepNoOpenArm = stepWith({ ...NOMUT, noOpenArm: true })
  action stepSplitExpiry = stepWith({ ...NOMUT, splitExpiry: true })
  action stepNoFirstWins = stepWith({ ...NOMUT, noFirstWins: true })
  action stepActiveRuleTimer = stepWith({ ...NOMUT, activeRuleTimer: true })
  action stepScheduledRuleTimer = stepWith({ ...NOMUT, scheduledRuleTimer: true })
  action stepFailureStopsPass = progressWith({ ...NOMUT, failureStopsPass: true })
  action stepAdvisoryRetry = progressWith({ ...NOMUT, advisoryRetry: true })
  action stepDeferOverflow = progressWith({ ...NOMUT, deferOverflow: true })

  // ---- Safety -----------------------------------------------------------------

  def typeOk: bool = and {
    now >= 0, now <= MAX_TIME,
    ITEMS.forall(i => and {
      dl.get(i) == NONE or (dl.get(i) >= 0 and dl.get(i) <= MAX_TIME),
      live.get(i) implies dl.get(i) != NONE,
      KINDS.contains(kind.get(i)),
      attempts.get(i) >= 0, retryAt.get(i) >= 0,
      RULES.contains(pinned.get(i)),
      firedRule.get(i) == 0 or RULES.contains(firedRule.get(i)),
      Set("none", "open", "recorded", "resolved").contains(dec.get(i)),
      skipped.get(i) >= 0,
    }),
    Set("idle", "ran").contains(phase),
    REQS.forall(r => Set("none", "guarded", "committed").contains(req.get(r))),
    RULES.contains(activeRule),
    seenRules.contains(activeRule),
    faults >= 0, faults <= FAULTS,
  }

  // W1-W4: once the cell is opened, every pending item has a durable alarm
  // no later than RECOVERY past its eligibility (or past now, if it is
  // already due). The allowance is the guard or recovery wake that covers a
  // commit whose rearm never ran, or due work whose handler was lost. One
  // time unit is RECOVERY_MS, so the model cannot place a deadline inside a
  // guard's minute; the code's guard bound is the same RECOVERY_MS.
  def wakeCovers: bool = opened implies ITEMS.forall(i => live.get(i) implies and {
    wake != NONE,
    wake <= max(eligible(i), now) + RECOVERY,
  })

  // E1: a recorded, unresolved expiry stays scheduled (so wakeCovers covers it).
  def expiryRecoverable: bool = ITEMS.forall(i =>
    (kind.get(i) == "decision" and dec.get(i) == "recorded") implies live.get(i))

  // E2: the first recorded result is never replaced, and a resolution is it.
  def expiryAtomic: bool = ITEMS.forall(i => and {
    decRec.get(i) == firstRec.get(i),
    decRes.get(i) == "" or decRes.get(i) == decRec.get(i),
  })

  // P1: a fired flow timer ran its run's starting rule.
  def timerRunsPinnedRule: bool = ITEMS.forall(i =>
    firedRule.get(i) != 0 implies firedRule.get(i) == pinned.get(i))

  // ---- Bounded progress (fairness FAIR, FAULTS) ---------------------------------

  // R1/R2: an item due at a completed pass is tried within three of them,
  // plus one per fault. Two skips come from the order alone (older failing
  // items fill the budget, and are eligible again when the next alarm lags
  // past their first backoff); a lost pass lets their second backoff pass
  // too. The doubling backoff then lets the item through.
  def dueProgress: bool = ITEMS.forall(i => skipped.get(i) <= 2 + faults)

  // R1/R2: a healthy item is handled within LATENESS of its deadline, plus a
  // recovery interval and a fairness lag per fault, however many failing
  // items are older. Checked while it waits and on the pass that handles it
  // (doneAt), so a pass that completes it late is refuted too.
  def lateBound(i: int): int = dl.get(i) + LATENESS + faults * (RECOVERY + FAIR + 1)
  def healthyNotStarved: bool = ITEMS.forall(i => healthy.get(i) implies and {
    live.get(i) implies now <= lateBound(i),
    doneAt.get(i) != NONE implies doneAt.get(i) <= lateBound(i),
  })

  def safety: bool = and { typeOk, wakeCovers, expiryRecoverable, expiryAtomic, timerRunsPinnedRule }
  def progress: bool = and { typeOk, dueProgress, healthyNotStarved }

  // ---- Non-vacuity witnesses (each must be violated: the state is reached) ----

  // The wake resolved a decision as expired.
  def witnessDecisionResolved: bool = not(ITEMS.exists(i => decRes.get(i) == "expired"))
  // A recorded expiry whose resolving commit failed (or whose isolate was
  // lost) was resumed with exactly its recorded result.
  def witnessExpiryResumed: bool = not(ITEMS.exists(i => resumed.get(i) and decRes.get(i) == "expired"))
  // The rule changed after the run started and before its timer fired, and
  // the timer ran the run's pin.
  def witnessPinnedAfterChange: bool = not(ITEMS.exists(i =>
    firedRule.get(i) != 0 and firedRule.get(i) == pinned.get(i) and activeAtFire.get(i) != pinned.get(i)))
  // The rule changed between the run's start and the timer's scheduling, and
  // the timer ran the run's pin (the scheduledRuleTimer mutant's state).
  def witnessPinnedBeforeScheduling: bool = not(ITEMS.exists(i =>
    firedRule.get(i) != 0 and firedRule.get(i) == pinned.get(i) and ruleAtWrite.get(i) != pinned.get(i)))
  // An item failed twice at one incarnation: it was retried after its backoff.
  def witnessRetried: bool = not(ITEMS.exists(i => attempts.get(i) >= 2))
  // wakeCovers' RECOVERY allowance is used: a pending item is covered only
  // by a recovery wake later than its eligibility. (At this granularity a
  // guard covers a new deadline exactly: both are at least one unit ahead.)
  def witnessLateCovered: bool = not(opened and wake != NONE and ITEMS.exists(i =>
    live.get(i) and wake > eligible(i)))
  // An isolate was lost (not a failed resolving commit) with due work pending.
  def witnessLostWithDueWork: bool =
    not(lostIsolate and phase == "idle" and ITEMS.exists(i => live.get(i) and eligible(i) <= now))
  // A healthy item was skipped once for older failing items, then handled.
  def witnessHealthyAfterSkip: bool = not(ITEMS.exists(j =>
    healthy.get(j) and everSkipped.get(j) and doneAt.get(j) != NONE and ITEMS.exists(i =>
      not(healthy.get(i)) and firstFail.get(i) != NONE and firstFail.get(i) <= doneAt.get(j)
        and dl.get(i) <= dl.get(j))))
  // healthyNotStarved's bound is reached: a healthy item skipped three times
  // (two for older failing items, one more after a fault) is handled exactly
  // at its lateness bound.
  def witnessHealthyAtLateBound: bool = not(ITEMS.exists(i =>
    healthy.get(i) and doneAt.get(i) != NONE and skipped.get(i) == 3 and doneAt.get(i) == lateBound(i)))
}
