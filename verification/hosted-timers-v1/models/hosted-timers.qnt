// The hosted cell's time-driven state (TIMER-ALARM-1, #1464): the design
// that Sol's round-1 review of #1464 asked for, not the round-1 code.
//
// Source of truth for the shape: apps/norn-cell-do/worker.mjs (rearm, alarm),
// crates/norn-api/src/timers.rs (next_due, run_due, MAX_DUE_STEPS,
// STUCK_RETRY_MS), crates/norn-api/src/decisions.rs (expire) and
// norn_kernel::flow_turn::timer, as of #1464 head 0d0cbbd.
//
// The design this model fixes (each is a mechanism a mutant removes):
//   D1 a write that commits a deadline registers its durable wake in the same
//      commit, whatever the response status and whether a later rearm fails;
//   D2 a delivered alarm first sets a durable recovery wake, before any work
//      or external I/O, so a lost, failed or evicted handler is woken again;
//   D3 existing cells are bootstrapped: every cell holding a deadline when
//      this design deploys has a wake no later than it;
//   D4 retry eligibility is persisted per item incarnation, a failed attempt
//      counts against the per-wake budget, and due items of every kind are
//      selected oldest-eligible first;
//   D5 a decision's expiry records and resolves in one commit, so a decision
//      never leaves the open set unresolved;
//   D6 a flow timer runs its run's pinned rule, never the active one; when
//      the pin is unavailable the timer stays and is retried.
//
// Time is logical. The handler runs between ticks: no time passes while it
// runs. Fairness of alarm delivery is an explicit assumption, encoded in
// `tick`: the clock never passes a durable wake by more than FAIR without the
// alarm firing. Handler losses (eviction, a throw, a crash at any point of a
// pass) are bounded by FAULTS. The two progress properties are bounded
// response under those assumptions, checked as invariants; this is not an
// unbounded LTL liveness proof.
module hosted_timers {
  type Mut = {
    rearmAfterCommit: bool,
    noRecoveryWake: bool,
    splitExpiry: bool,
    activeRuleTimer: bool,
    failureStopsPass: bool,
    advisoryRetry: bool,
  }
  pure val NOMUT: Mut = {
    rearmAfterCommit: false,
    noRecoveryWake: false,
    splitExpiry: false,
    activeRuleTimer: false,
    failureStopsPass: false,
    advisoryRetry: false,
  }

  // Bounds. Three item slots against a per-wake budget of two stand in for
  // MAX_DUE_STEPS = 64 against an unbounded backlog.
  // The minimum helpers below are unrolled over these three slots (Apalache
  // checks folds far more slowly); change both together.
  pure val ITEMS = Set(1, 2, 3)
  pure val KINDS = Set("reservation", "timer", "decision")
  pure val RULES = Set(1, 2)
  pure val BUDGET = 2
  pure val RETRY = 2
  pure val RECOVERY = 1
  pure val FAIR = 1
  pure val FAULTS = 1
  pure val MAX_TIME = 7
  pure val LEGACY_DEADLINES = Set(1, 2, 3)
  pure val NONE = -1

  // Durable cell state (survives eviction and restart).
  var now: int
  var deadline: int -> int      // NONE: the slot holds no live item
  var kind: int -> str
  var healthy: int -> bool      // false: handling fails (corrupt reservation, unavailable pin, retired flow)
  var retryAt: int -> int       // D4: persisted retry eligibility of the item's incarnation, 0 = none
  var pinned: int -> int        // a flow timer's run's pinned rule
  var firedRule: int -> int     // the rule a fired flow timer ran, 0 = not fired
  var dec: int -> str           // "none" | "open" | "recorded" | "resolved"
  var wake: int                 // the durable alarm, NONE = no alarm
  var activeRule: int
  // The alarm handler (volatile: lost on eviction).
  var phase: str                // "idle" | "running"
  var attempted: Set[int]
  var used: int
  var dueAtFire: Set[int]
  // History, for the properties.
  var lastFire: int             // time the last handler started, NONE before any
  var faults: int
  var skipped: int -> int       // completed passes that left the item due and unattempted

  pure def minWake(a: int, b: int): int =
    if (a == NONE) b else if (b == NONE) a else if (a < b) a else b
  pure def max(a: int, b: int): int = if (a > b) a else b

  def live(i: int): bool = deadline.get(i) != NONE
  def eligible(i: int): int = max(deadline.get(i), retryAt.get(i))
  def liveItems: Set[int] = ITEMS.filter(i => live(i))
  def eligibleAfter(i: int, after: int): int = if (live(i) and eligible(i) > after) eligible(i) else NONE
  // The earliest eligibility strictly after `after` (NONE: of every live item).
  def earliestAfter(after: int): int =
    minWake(minWake(eligibleAfter(1, after), eligibleAfter(2, after)), eligibleAfter(3, after))
  def candidates: Set[int] =
    ITEMS.filter(i => live(i) and eligible(i) <= now and not(attempted.contains(i)))

  action initWith(dl: int -> int, kd: int -> str, hl: int -> bool, bootstrap: bool): bool = all {
    now' = 0,
    deadline' = dl,
    kind' = kd,
    healthy' = hl,
    retryAt' = ITEMS.mapBy(_ => 0),
    pinned' = ITEMS.mapBy(_ => 1),
    firedRule' = ITEMS.mapBy(_ => 0),
    dec' = ITEMS.mapBy(i => if (dl.get(i) != NONE and kd.get(i) == "decision") "open" else "none"),
    // D3: the deploy bootstrap arms every existing cell for its earliest deadline.
    wake' = if (bootstrap) minWake(minWake(dl.get(1), dl.get(2)), dl.get(3)) else NONE,
    activeRule' = 1,
    phase' = "idle",
    attempted' = Set(),
    used' = 0,
    dueAtFire' = Set(),
    lastFire' = NONE,
    faults' = 0,
    skipped' = ITEMS.mapBy(_ => 0),
  }

  // Cells that already hold deadlines when the design deploys.
  action initAs(bootstrap: bool): bool = {
    nondet dl = ITEMS.setOfMaps(LEGACY_DEADLINES.union(Set(NONE))).oneOf()
    nondet kd = ITEMS.setOfMaps(KINDS).oneOf()
    nondet hl = ITEMS.setOfMaps(Set(true, false)).oneOf()
    initWith(dl, kd, hl, bootstrap)
  }
  action init = initAs(true)
  // Mutant init: cells from before the deploy have no alarm (round-1 gap).
  action initNoBootstrap = initAs(false)

  // A request commits a new item with a deadline (a reservation with an
  // expiry, a flow sleep/retry timer, a decision with a deadline). `ok` is
  // whether the reply was a success and a separate rearm would have worked.
  action write(i: int, d: int, k: str, h: bool, ok: bool, m: Mut): bool = all {
    not(live(i)),
    d > now,
    d <= MAX_TIME,
    deadline' = deadline.set(i, d),
    kind' = kind.set(i, k),
    healthy' = healthy.set(i, h),
    retryAt' = retryAt.set(i, 0),
    pinned' = pinned.set(i, activeRule),
    firedRule' = firedRule.set(i, 0),
    dec' = dec.set(i, if (k == "decision") "open" else "none"),
    skipped' = skipped.set(i, 0),
    // D1. Mutant: the wake is a best-effort rearm after the reply.
    wake' = if (m.rearmAfterCommit and not(ok)) wake else minWake(wake, d),
    now' = now, activeRule' = activeRule, phase' = phase, attempted' = attempted,
    used' = used, dueAtFire' = dueAtFire, lastFire' = lastFire, faults' = faults,
  }

  // The clock advances, never past a durable wake by more than FAIR.
  action tick(t: int): bool = all {
    phase == "idle",
    t > now,
    t <= MAX_TIME,
    wake == NONE or t <= wake + FAIR,
    now' = t,
    deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, dec' = dec, wake' = wake,
    activeRule' = activeRule, phase' = phase, attempted' = attempted, used' = used,
    dueAtFire' = dueAtFire, lastFire' = lastFire, faults' = faults, skipped' = skipped,
  }

  // The runtime delivers the alarm (starting the object if it was evicted).
  action fire(m: Mut): bool = all {
    phase == "idle",
    wake != NONE,
    now >= wake,
    phase' = "running",
    attempted' = Set(),
    used' = 0,
    dueAtFire' = candidates,
    lastFire' = now,
    // D2: the recovery wake goes down first, and never later than the next
    // deadline still ahead. Mutant: the delivered alarm is simply gone
    // until the handler re-arms at its end.
    wake' = if (m.noRecoveryWake) NONE
      else minWake(now + RECOVERY, earliestAfter(now)),
    now' = now, deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, dec' = dec, activeRule' = activeRule,
    faults' = faults, skipped' = skipped,
  }

  // One due item, oldest eligible first, through its own commit.
  action runItem(i: int, m: Mut): bool = all {
    phase == "running",
    used < BUDGET,
    candidates.contains(i),
    candidates.forall(j => eligible(i) <= eligible(j)),
    val ok = healthy.get(i)
    val k = kind.get(i)
    all {
      attempted' = attempted.union(Set(i)),
      // D4: a failure spends budget like a success. Mutant: it ends the pass.
      used' = if (not(ok) and m.failureStopsPass) BUDGET else used + 1,
      deadline' = if (ok) deadline.set(i, NONE) else deadline,
      // D4: the retry time is the item's own, on disk. Mutants: advisory only.
      retryAt' = if (ok or m.advisoryRetry or m.failureStopsPass) retryAt
        else retryAt.set(i, now + RETRY),
      // D6: the run's pinned rule. Mutant: whatever rule is active now.
      firedRule' = if (ok and k == "timer")
          firedRule.set(i, if (m.activeRuleTimer) activeRule else pinned.get(i))
        else firedRule,
      // D5: recorded and resolved in one commit. Mutant: the expiry is
      // recorded (the decision leaves the open set) and resolved later.
      dec' = if (ok and k == "decision") dec.set(i, if (m.splitExpiry) "recorded" else "resolved")
        else dec,
    },
    now' = now, kind' = kind, healthy' = healthy, pinned' = pinned, wake' = wake,
    activeRule' = activeRule, phase' = phase, dueAtFire' = dueAtFire, lastFire' = lastFire,
    faults' = faults,
    // The attempt is durable once its commit lands, whether or not the pass completes.
    skipped' = skipped.set(i, 0),
  }

  // Mutant only: the second transaction of a split expiry.
  action resolveRecorded(i: int): bool = all {
    phase == "running",
    dec.get(i) == "recorded",
    dec' = dec.set(i, "resolved"),
    now' = now, deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, wake' = wake, activeRule' = activeRule,
    phase' = phase, attempted' = attempted, used' = used, dueAtFire' = dueAtFire,
    lastFire' = lastFire, faults' = faults, skipped' = skipped,
  }

  // The pass ends: re-arm for the next eligible item, at once when the
  // budget ran out with due work left.
  action finish: bool = all {
    phase == "running",
    used >= BUDGET or candidates == Set(),
    phase' = "idle",
    wake' = if (used >= BUDGET and candidates != Set()) now else earliestAfter(NONE),
    skipped' = ITEMS.mapBy(i =>
      if (attempted.contains(i)) 0
      else if (dueAtFire.contains(i) and live(i)) skipped.get(i) + 1
      else skipped.get(i)),
    attempted' = Set(),
    used' = 0,
    dueAtFire' = Set(),
    now' = now, deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, dec' = dec, activeRule' = activeRule,
    lastFire' = lastFire, faults' = faults,
  }

  // The handler is lost mid-pass: eviction, a throw the runtime gives up
  // on, a crash between two item commits or before the final rearm.
  action lose: bool = all {
    phase == "running",
    faults < FAULTS,
    phase' = "idle",
    attempted' = Set(),
    used' = 0,
    dueAtFire' = Set(),
    faults' = faults + 1,
    now' = now, deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, dec' = dec, wake' = wake,
    activeRule' = activeRule, lastFire' = lastFire, skipped' = skipped,
  }

  // A request opens the object after a restart: it re-arms for what is due.
  action open: bool = all {
    phase == "idle",
    wake' = minWake(wake, earliestAfter(NONE)),
    now' = now, deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, dec' = dec, activeRule' = activeRule,
    phase' = phase, attempted' = attempted, used' = used, dueAtFire' = dueAtFire,
    lastFire' = lastFire, faults' = faults, skipped' = skipped,
  }

  // A definition change activates another rule; running flows keep their pin.
  action changeRule(r: int): bool = all {
    r != activeRule,
    activeRule' = r,
    now' = now, deadline' = deadline, kind' = kind, healthy' = healthy, retryAt' = retryAt,
    pinned' = pinned, firedRule' = firedRule, dec' = dec, wake' = wake, phase' = phase,
    attempted' = attempted, used' = used, dueAtFire' = dueAtFire, lastFire' = lastFire,
    faults' = faults, skipped' = skipped,
  }

  action stepWith(m: Mut): bool = {
    nondet i = ITEMS.oneOf()
    nondet d = 1.to(MAX_TIME).oneOf()
    nondet k = KINDS.oneOf()
    nondet h = Set(true, false).oneOf()
    nondet ok = Set(true, false).oneOf()
    nondet r = RULES.oneOf()
    any {
      write(i, d, k, h, ok, m),
      tick(d),
      fire(m),
      runItem(i, m),
      resolveRecorded(i),
      finish,
      lose,
      open,
      changeRule(r),
    }
  }

  // ---- Whole passes, for the progress properties --------------------------
  // `pass` is fire; runItem for each chosen item, oldest eligible first;
  // then finish, as one step. With `lost`, the handler is lost after
  // committing `done`, any part of the chosen items (lose). Progress is
  // checked with this step and tick only: the items of the deploy-time
  // cell, no new writes or definition changes.
  pure def eligibleIn(dl: int -> int, ra: int -> int, i: int): int = max(dl.get(i), ra.get(i))
  pure def afterIn(dl: int -> int, ra: int -> int, i: int, after: int): int =
    if (dl.get(i) != NONE and eligibleIn(dl, ra, i) > after) eligibleIn(dl, ra, i) else NONE
  pure def earliestIn(dl: int -> int, ra: int -> int, after: int): int =
    minWake(minWake(afterIn(dl, ra, 1, after), afterIn(dl, ra, 2, after)), afterIn(dl, ra, 3, after))

  action pass(m: Mut, lost: bool): bool = {
    nondet chosen = ITEMS.powerset().oneOf()
    nondet done = ITEMS.powerset().oneOf()
    val cand = candidates
    val fails = chosen.filter(i => not(healthy.get(i)))
    val ok = done.filter(i => healthy.get(i))
    val failed = done.filter(i => not(healthy.get(i)))
    val stopped = m.failureStopsPass and fails != Set()
    val dl2 = ITEMS.mapBy(i => if (ok.contains(i)) NONE else deadline.get(i))
    val ra2 = ITEMS.mapBy(i =>
      if (failed.contains(i) and not(m.advisoryRetry or m.failureStopsPass)) now + RETRY
      else retryAt.get(i))
    val left = ITEMS.filter(i =>
      dl2.get(i) != NONE and eligibleIn(dl2, ra2, i) <= now and not(chosen.contains(i)))
    all {
      phase == "idle",
      wake != NONE,
      now >= wake,
      faults < FAULTS or not(lost),
      chosen.subseteq(cand),
      // Oldest eligible first (ties in any order).
      cand.forall(b => chosen.contains(b) or chosen.forall(a => eligible(a) <= eligible(b))),
      // The budget: every attempt counts. Mutant: the first failure ends the pass.
      if (m.failureStopsPass)
        fails.size() <= 1 and fails.forall(f => chosen.forall(a => eligible(a) <= eligible(f)))
          and (fails == Set() implies
            chosen.size() == (if (cand.size() < BUDGET) cand.size() else BUDGET))
          and chosen.size() <= BUDGET
      else chosen.size() == (if (cand.size() < BUDGET) cand.size() else BUDGET),
      if (lost) done.subseteq(chosen) else done == chosen,
      deadline' = dl2,
      retryAt' = ra2,
      firedRule' = ITEMS.mapBy(i =>
        if (ok.contains(i) and kind.get(i) == "timer")
          (if (m.activeRuleTimer) activeRule else pinned.get(i))
        else firedRule.get(i)),
      dec' = ITEMS.mapBy(i =>
        if (ok.contains(i) and kind.get(i) == "decision") (if (m.splitExpiry) "recorded" else "resolved")
        else dec.get(i)),
      wake' =
        if (lost) (if (m.noRecoveryWake) NONE else minWake(now + RECOVERY, earliestAfter(now)))
        else if ((chosen.size() >= BUDGET or stopped) and left != Set()) now
        else earliestIn(dl2, ra2, NONE),
      skipped' = ITEMS.mapBy(i =>
        // An attempt is durable once its commit lands, whether or not the pass completes.
        if (done.contains(i)) 0
        else if (lost) skipped.get(i)
        else if (cand.contains(i) and dl2.get(i) != NONE) skipped.get(i) + 1
        else skipped.get(i)),
      faults' = if (lost) faults + 1 else faults,
      lastFire' = now,
      now' = now, kind' = kind, healthy' = healthy, pinned' = pinned, activeRule' = activeRule,
      phase' = phase, attempted' = attempted, used' = used, dueAtFire' = dueAtFire,
    }
  }

  action progressWith(m: Mut): bool = {
    nondet t = 1.to(MAX_TIME).oneOf()
    nondet lost = Set(true, false).oneOf()
    any { tick(t), pass(m, lost) }
  }

  action step = stepWith(NOMUT)
  action stepProgress = progressWith(NOMUT)
  action stepRearmAfterCommit = stepWith({ ...NOMUT, rearmAfterCommit: true })
  action stepNoRecoveryWake = stepWith({ ...NOMUT, noRecoveryWake: true })
  action stepSplitExpiry = stepWith({ ...NOMUT, splitExpiry: true })
  action stepActiveRuleTimer = stepWith({ ...NOMUT, activeRuleTimer: true })
  action stepFailureStopsPass = progressWith({ ...NOMUT, failureStopsPass: true })
  action stepAdvisoryRetry = progressWith({ ...NOMUT, advisoryRetry: true })

  // ---- Safety -----------------------------------------------------------

  def typeOk: bool = and {
    now >= 0, now <= MAX_TIME,
    ITEMS.forall(i => and {
      deadline.get(i) == NONE or (deadline.get(i) >= 1 and deadline.get(i) <= MAX_TIME),
      KINDS.contains(kind.get(i)),
      retryAt.get(i) >= 0,
      RULES.contains(pinned.get(i)),
      firedRule.get(i) == 0 or RULES.contains(firedRule.get(i)),
      Set("none", "open", "recorded", "resolved").contains(dec.get(i)),
      skipped.get(i) >= 0,
    }),
    Set("idle", "running").contains(phase),
    RULES.contains(activeRule),
    used >= 0, used <= BUDGET,
    attempted.subseteq(ITEMS),
    faults >= 0, faults <= FAULTS,
  }

  // No committed deadline without a durable wake no later than it. The one
  // allowance is D2's recovery wake: an item already due when the last
  // handler started may be covered by that handler's recovery wake.
  def wakeCovers: bool = liveItems.forall(i => and {
    wake != NONE,
    wake <= eligible(i)
      or (lastFire != NONE and eligible(i) <= lastFire and wake <= lastFire + RECOVERY),
  })

  // Expiry is atomic: no decision is out of the open (scheduled) set while
  // unresolved.
  def expiryAtomic: bool = ITEMS.forall(i =>
    (kind.get(i) == "decision" and (dec.get(i) == "open" or dec.get(i) == "recorded"))
      implies live(i))

  // A fired flow timer ran its run's pinned rule.
  def timerRunsPinnedRule: bool = ITEMS.forall(i =>
    firedRule.get(i) != 0 implies firedRule.get(i) == pinned.get(i))

  // ---- Bounded progress (fairness: FAIR; losses: FAULTS) -----------------

  // Every item due when a pass starts is attempted (processed, or failed
  // and given its own retry time) within two completed passes.
  def dueProgress: bool = ITEMS.forall(i => skipped.get(i) <= 1)

  // A healthy item is processed within 2*FAIR of its deadline, plus one
  // recovery interval per lost handler, however many failing items there are.
  def healthyNotStarved: bool = liveItems.forall(i =>
    healthy.get(i) implies now <= deadline.get(i) + 2 * FAIR + faults * (RECOVERY + FAIR))

  def safety: bool = and { typeOk, wakeCovers, expiryAtomic, timerRunsPinnedRule }
  def progress: bool = and { typeOk, dueProgress, healthyNotStarved }

  // ---- Non-vacuity witnesses (each must be violated: the state is reached) --

  def witnessDecisionResolved: bool = not(ITEMS.exists(i => dec.get(i) == "resolved"))
  def witnessPinnedAfterChange: bool =
    not(ITEMS.exists(i => firedRule.get(i) != 0 and firedRule.get(i) != activeRule))
  def witnessRetried: bool = not(ITEMS.exists(i => retryAt.get(i) > 0 and live(i)))
  def witnessLostWithDueWork: bool =
    not(faults > 0 and phase == "idle" and liveItems.exists(i => eligible(i) <= now))
  def witnessHealthyAfterFailures: bool = not(ITEMS.exists(i =>
    not(healthy.get(i)) and retryAt.get(i) > 0 and ITEMS.exists(j =>
      j != i and healthy.get(j) and not(live(j)) and dec.get(j) == "resolved")))
}
