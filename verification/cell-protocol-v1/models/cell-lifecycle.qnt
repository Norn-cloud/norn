// D83 L-model 1: the cell's command, perform and timer lifecycle, with the
// states the code leaves implicit made explicit (#718, #1223).
//
// Source of truth: norn-kernel cell_host.rs (peek/consume, generations,
// timer incarnations), norn-cell host.rs (claim_perform, PerformClaim,
// complete_perform, claim_reconciled_perform) and store.rs (EffectRow,
// record_performed). EFFECT-IDENTITY-1 (flow_identity.rs): a command's
// idempotency key is its business operation, stable across reconciliation.
//
// Implicit states this model names (each is a variable or a derived state):
//   I1 enqueued      gen[c] > consumedGen[c], no claim for gen[c]
//   I2 claimed       claimed[c] == gen[c], not yet dispatched (v.fresh)
//   I3 in flight     dispatched by this process, awaiting its result (v.inflight)
//   I4 on the wire   request travelling to the provider (env.toProvider)
//   I5 executed      provider executed the key (env.done), result on the wire
//   I6 recorded      resultGen[c] == gen[c], not consumed
//   I7 peeked        a turn holds the delivery between peek and commit (v.turn)
//   I8 outcome unknown  claimed, no result, and no live process owns it
//   I9 fenced        a generation replaced while pending (g.replaced)
//   I10 timer due    scheduled incarnation with now >= due; fired = consumed
//   I11 evicted/crashed  every volatile field lost, durable store intact
//   I12 fenced at the provider, refusal not yet committed (a crash between
//       the provider's fence and the cell's refusal commit leaves this state)
// Environment: results are duplicated (delivery never removes a message),
// dropped and reordered (the wire is a set).
module cell_lifecycle {
  type Pair = (int, int)
  type Msg = { cmd: int, gen: int, value: int }
  type Delivery = { kind: str, id: int, gen: int, value: int }
  type Entry = { seq: int, kind: str, id: int, gen: int, value: int }
  type Durable = { gen: int -> int, claimed: int -> int, resultGen: int -> int,
    resultVal: int -> int, consumedGen: int -> int, refusedGen: int -> int,
    timerInc: int -> int, timerDue: int -> int, seq: int, facts: int, log: Set[Entry] }
  type Volatile = { up: bool, cache: int, fresh: Set[Pair], inflight: Set[Pair], turn: Delivery }
  type Env = { now: int, toProvider: Set[Pair], net: Set[Msg], done: int -> int,
    doneVal: int -> int, fenced: int -> bool }
  type Ghost = { dispatches: Pair -> int, claimedEver: Set[Pair], replaced: Set[Pair],
    last: str, rescheduledIncs: Set[int], epoch: int, claimEpoch: Pair -> int, lastRecorded: Pair,
    staleIncs: Set[int], reissued: Set[Pair] }
  // Mutants: each flag removes one mechanism; the matching step must refute
  // the invariant that depends on it (manifest `variants`).
  type Mut = { unfenced: bool, noTimerFence: bool, keepLicence: bool, noConsumeMark: bool,
    noResultFence: bool, noProviderDedupe: bool }

  pure val CMDS = Set(1, 2)
  pure val TIMERS = Set(1)
  pure val MAX_GEN = 2
  pure val MAX_TIME = 3
  pure val VALS = Set(1, 2)
  pure val GENS = 1.to(MAX_GEN)
  pure val PAIRS = tuples(CMDS, GENS)
  pure val NONE: Delivery = { kind: "none", id: 0, gen: 0, value: 0 }
  pure val NOMUT: Mut = { unfenced: false, noTimerFence: false, keepLicence: false,
    noConsumeMark: false, noResultFence: false, noProviderDedupe: false }

  var d: Durable
  var v: Volatile
  var env: Env
  var g: Ghost

  pure val zeroC = CMDS.mapBy(_ => 0)
  pure val zeroT = TIMERS.mapBy(_ => 0)
  pure val emptyVolatile: Volatile = { up: true, cache: 0, fresh: Set(), inflight: Set(),
    turn: NONE }

  action init = all {
    d' = { gen: zeroC, claimed: zeroC, resultGen: zeroC, resultVal: zeroC,
      consumedGen: zeroC, refusedGen: zeroC, timerInc: zeroT, timerDue: zeroT,
      seq: 0, facts: 0, log: Set() },
    v' = emptyVolatile,
    env' = { now: 0, toProvider: Set(), net: Set(), done: zeroC, doneVal: zeroC,
      fenced: CMDS.mapBy(_ => false) },
    g' = { dispatches: PAIRS.mapBy(_ => 0), claimedEver: Set(), replaced: Set(),
      last: "init", rescheduledIncs: Set(), epoch: 0, claimEpoch: PAIRS.mapBy(_ => 0),
      lastRecorded: (0, 0), staleIncs: Set(), reissued: Set() },
  }

  def pending(c: int): bool = d.gen.get(c) > d.consumedGen.get(c)
  // I8: claimed, no recorded result, and no live process owns the claim.
  def outcomeUnknown(c: int): bool = { val p = (c, d.gen.get(c))
    pending(c) and d.claimed.get(c) == d.gen.get(c) and d.resultGen.get(c) != d.gen.get(c)
      and not(v.fresh.contains(p)) and not(v.inflight.contains(p)) }

  // A commit is one transaction: ledger head, durable rows and log together.
  // --- Commands --------------------------------------------------------
  // EffectRow::Enqueue. First issue, or a newer generation of a pending
  // command, which replaces it in place and fences the old generation.
  // Not modelled: EffectRow::Enqueue also appends a newer generation of an
  // already consumed command. Under EFFECT-IDENTITY-1 that is a new business
  // operation (a new operation key), so it is a fresh command here; reusing
  // a consumed command id is outside this model (see manifest `exclusions`).
  action enqueueAs(c: int, reissue: bool): bool = {
    val ng = d.gen.get(c) + 1
    all {
      v.up, ng <= MAX_GEN, d.gen.get(c) == 0 or pending(c),
      d' = { ...d, gen: d.gen.set(c, ng), seq: d.seq + 1 },
      v' = v, env' = env,
      g' = { ...g, last: if (reissue) "reissue" else "enqueue",
        replaced: if (pending(c)) g.replaced.union(Set((c, d.gen.get(c)))) else g.replaced,
        reissued: if (reissue) g.reissued.union(Set((c, ng))) else g.reissued },
    }
  }
  action enqueue(c: int): bool = enqueueAs(c, false)
  // EffectRow::ClaimPerform: a zero-event commit before any dispatch.
  action claim(c: int): bool = all {
    v.up, pending(c), d.claimed.get(c) != d.gen.get(c), d.resultGen.get(c) != d.gen.get(c),
    d' = { ...d, claimed: d.claimed.set(c, d.gen.get(c)), seq: d.seq + 1 },
    v' = { ...v, fresh: v.fresh.union(Set((c, d.gen.get(c)))) },
    env' = env,
    g' = { ...g, last: "claim", claimedEver: g.claimedEver.union(Set((c, d.gen.get(c)))),
      claimEpoch: g.claimEpoch.set((c, d.gen.get(c)), g.epoch) },
  }
  // PerformClaim::Fresh is the only dispatch licence, and it is volatile.
  // Mutant `keepLicence`: dispatch leaves the licence in place.
  action dispatchWith(p: Pair, keepLicence: bool): bool = all {
    v.up, v.fresh.contains(p),
    d' = d,
    v' = { ...v, fresh: if (keepLicence) v.fresh else v.fresh.exclude(Set(p)),
      inflight: v.inflight.union(Set(p)) },
    env' = { ...env, toProvider: env.toProvider.union(Set(p)) },
    g' = { ...g, last: "dispatch", dispatches: g.dispatches.setBy(p, n => n + 1) },
  }
  action dispatch(p: Pair): bool = dispatchWith(p, false)
  // The provider deduplicates on the operation key (the command id): a
  // repeated key returns the recorded outcome. A fenced key never executes.
  // Mutant `noProviderDedupe` (drops assumption A1): every request executes.
  action providerExecWith(p: Pair, x: int, dedupe: bool): bool = {
    val c = p._1
    all {
      env.toProvider.contains(p),
      d' = d, v' = v, g' = { ...g, last: "providerExec" },
      env' = if (env.fenced.get(c)) { ...env, toProvider: env.toProvider.exclude(Set(p)) }
        else if (dedupe and env.done.get(c) != 0) { ...env, toProvider: env.toProvider.exclude(Set(p)),
          net: env.net.union(Set({ cmd: c, gen: p._2, value: env.doneVal.get(c) })) }
        else { ...env, toProvider: env.toProvider.exclude(Set(p)),
          done: env.done.set(c, p._2), doneVal: env.doneVal.set(c, x),
          net: env.net.union(Set({ cmd: c, gen: p._2, value: x })) },
    }
  }
  action providerExec(p: Pair, x: int): bool = providerExecWith(p, x, true)
  action dropRequest(p: Pair): bool = all {
    env.toProvider.contains(p),
    d' = d, v' = v, g' = { ...g, last: "dropRequest" },
    env' = { ...env, toProvider: env.toProvider.exclude(Set(p)) },
  }
  action dropResult(m: Msg): bool = all {
    env.net.contains(m),
    d' = d, v' = v, g' = { ...g, last: "dropResult" },
    env' = { ...env, net: env.net.exclude(Set(m)) },
  }
  // record_performed: its own transaction; only the current, unresulted,
  // unconsumed generation. Delivery never removes the message (duplication).
  action record(m: Msg): bool = all {
    v.up, env.net.contains(m), d.gen.get(m.cmd) == m.gen, pending(m.cmd),
    d.resultGen.get(m.cmd) != m.gen,
    d' = { ...d, resultGen: d.resultGen.set(m.cmd, m.gen), resultVal: d.resultVal.set(m.cmd, m.value) },
    v' = { ...v, inflight: v.inflight.exclude(Set((m.cmd, m.gen))) },
    env' = env, g' = { ...g, last: "record", lastRecorded: (m.cmd, m.gen) },
  }
  // A dispatched call that times out leaves its claim with no owner (I8).
  action timeout(p: Pair): bool = all {
    v.up, v.inflight.contains(p),
    d' = d, v' = { ...v, inflight: v.inflight.exclude(Set(p)) }, env' = env,
    g' = { ...g, last: "timeout" },
  }
  // OutcomeUnknown, policy 1: claim_reconciled_perform authorizes a newer
  // generation under the same operation key (replace in place).
  action reconcileReissue(c: int): bool = all {
    outcomeUnknown(c), enqueueAs(c, true),
  }
  // OutcomeUnknown, policy 2, in two steps with a crash point between them
  // (I12). Step 1, at the provider: lookup-and-fence on the operation key
  // (assumption A2: the provider fences atomically with its lookup). Step 2,
  // in the cell: an executed key returns its recorded outcome; a fenced,
  // unexecuted key is refused by a cell commit.
  action providerFence(c: int): bool = all {
    v.up, outcomeUnknown(c), env.done.get(c) == 0, not(env.fenced.get(c)),
    d' = d, v' = v, g' = { ...g, last: "providerFence" },
    env' = { ...env, fenced: env.fenced.set(c, true) },
  }
  action reconcileRefuse(c: int): bool = {
    val gc = d.gen.get(c)
    all {
      v.up, outcomeUnknown(c), env.done.get(c) != 0 or env.fenced.get(c),
      env' = if (env.done.get(c) != 0)
          { ...env, net: env.net.union(Set({ cmd: c, gen: gc, value: env.doneVal.get(c) })) }
        else env,
      d' = if (env.done.get(c) != 0) d
        else { ...d, consumedGen: d.consumedGen.set(c, gc), refusedGen: d.refusedGen.set(c, gc),
          seq: d.seq + 1, log: d.log.union(Set({ seq: d.seq + 1, kind: "refused", id: c, gen: gc, value: 0 })) },
      v' = v,
      g' = { ...g, last: if (env.done.get(c) != 0) "reconcileRefetch" else "reconcileRefuse" },
    }
  }
  // Variant: refuse after a lookup with no fence. Not in `step`; it exists
  // so `stepUnfenced` can exhibit the lost-effect counterexample.
  action reconcileRefuseUnfenced(c: int): bool = {
    val gc = d.gen.get(c)
    all {
      v.up, outcomeUnknown(c), env.done.get(c) == 0,
      d' = { ...d, consumedGen: d.consumedGen.set(c, gc), refusedGen: d.refusedGen.set(c, gc),
        seq: d.seq + 1, log: d.log.union(Set({ seq: d.seq + 1, kind: "refused", id: c, gen: gc, value: 0 })) },
      v' = v, env' = env, g' = { ...g, last: "reconcileRefuseUnfenced" },
    }
  }

  // --- Timers ----------------------------------------------------------
  // EffectRow::Schedule: insert or replace; the incarnation is this commit.
  action schedule(t: int, due: int): bool = all {
    v.up, due >= env.now,
    d' = { ...d, seq: d.seq + 1, timerInc: d.timerInc.set(t, d.seq + 1), timerDue: d.timerDue.set(t, due) },
    v' = v, env' = env,
    g' = { ...g, last: if (d.timerInc.get(t) != 0) "reschedule" else "schedule",
      rescheduledIncs: if (d.timerInc.get(t) != 0) g.rescheduledIncs.union(Set(d.seq + 1)) else g.rescheduledIncs,
      staleIncs: if (d.timerInc.get(t) != 0) g.staleIncs.union(Set(d.timerInc.get(t))) else g.staleIncs },
  }
  action cancel(t: int): bool = all {
    v.up, d.timerInc.get(t) != 0,
    d' = { ...d, seq: d.seq + 1, timerInc: d.timerInc.set(t, 0) },
    v' = v, env' = env,
    g' = { ...g, last: "cancel", staleIncs: g.staleIncs.union(Set(d.timerInc.get(t))) },
  }
  action tick = all {
    env.now < MAX_TIME,
    d' = d, v' = v, env' = { ...env, now: env.now + 1 }, g' = { ...g, last: "tick" },
  }

  // --- Turns: peek, then an atomic commit that consumes the delivery -----
  action peekResult(c: int): bool = all {
    v.up, v.turn == NONE, pending(c), d.resultGen.get(c) == d.gen.get(c),
    d' = d, env' = env, g' = { ...g, last: "peekResult" },
    v' = { ...v, turn: { kind: "result", id: c, gen: d.gen.get(c), value: d.resultVal.get(c) } },
  }
  action peekTimer(t: int): bool = all {
    v.up, v.turn == NONE, d.timerInc.get(t) != 0, env.now >= d.timerDue.get(t),
    d' = d, env' = env, g' = { ...g, last: "peekTimer" },
    v' = { ...v, turn: { kind: "timer", id: t, gen: d.timerInc.get(t), value: 1 } },
  }
  // The consume fence: the delivery must still be pending under the same
  // generation or incarnation. A failing fence refuses and writes nothing.
  def fenceHolds(t: Delivery): bool =
    if (t.kind == "result") d.gen.get(t.id) == t.gen and pending(t.id) and d.resultGen.get(t.id) == t.gen
    else d.timerInc.get(t.id) == t.gen
  // Mutants: `noResultFence` / `noTimerFence` skip the fence for that kind;
  // `noConsumeMark` commits without marking the generation consumed.
  action commitTurnWith(mut: Mut): bool = {
    val t = v.turn
    val n = d.seq + 1
    val skip = (t.kind == "result" and mut.noResultFence) or (t.kind == "timer" and mut.noTimerFence)
    all {
      v.up, t != NONE,
      env' = env,
      if (skip or fenceHolds(t)) all {
        d' = { ...d, seq: n, facts: d.facts + t.value,
          log: d.log.union(Set({ seq: n, kind: t.kind, id: t.id, gen: t.gen, value: t.value })),
          consumedGen: if (t.kind == "result" and not(mut.noConsumeMark)) d.consumedGen.set(t.id, t.gen) else d.consumedGen,
          timerInc: if (t.kind == "timer") d.timerInc.set(t.id, 0) else d.timerInc },
        v' = { ...v, turn: NONE, cache: d.facts + t.value },
        g' = { ...g, last: "commit" },
      } else all {
        d' = d, v' = { ...v, turn: NONE }, g' = { ...g, last: "refuseStale" },
      },
    }
  }
  action commitTurn = commitTurnWith(NOMUT)

  // --- Process failure -------------------------------------------------
  // A crash can strike anywhere, including between peek and commit (the
  // commit is one transaction, so mid-commit equals before-commit).
  action crash = all {
    v.up, d' = d, env' = env, g' = { ...g, last: "crash" },
    v' = { ...emptyVolatile, up: false },
  }
  // DO eviction happens only when idle: no turn, no call in flight.
  action evict = all {
    v.up, v.turn == NONE, v.inflight == Set(),
    d' = d, env' = env, g' = { ...g, last: "evict" },
    v' = { ...emptyVolatile, up: false },
  }
  // Reopen reloads the cache from the durable store; a retry redelivers.
  action reopen = all {
    not(v.up), d' = d, env' = env, g' = { ...g, last: "reopen", epoch: g.epoch + 1 },
    v' = { ...emptyVolatile, cache: d.facts },
  }

  action stepWith(mut: Mut): bool = {
    nondet c = CMDS.oneOf()
    nondet t = TIMERS.oneOf()
    nondet p = PAIRS.oneOf()
    nondet x = VALS.oneOf()
    nondet due = 0.to(MAX_TIME).oneOf()
    nondet m = env.net.union(Set({ cmd: 1, gen: 0, value: 0 })).oneOf()
    any {
      enqueue(c), claim(c), dispatchWith(p, mut.keepLicence),
      providerExecWith(p, x, not(mut.noProviderDedupe)), dropRequest(p),
      dropResult(m), record(m), timeout(p), reconcileReissue(c), providerFence(c),
      if (mut.unfenced) reconcileRefuseUnfenced(c) else reconcileRefuse(c),
      schedule(t, due), cancel(t), tick, peekResult(c), peekTimer(t), commitTurnWith(mut),
      crash, evict, reopen,
    }
  }
  action step = stepWith(NOMUT)
  action stepUnfenced = stepWith({ ...NOMUT, unfenced: true })
  action stepNoTimerFence = stepWith({ ...NOMUT, noTimerFence: true })
  action stepDoubleDispatch = stepWith({ ...NOMUT, keepLicence: true })
  action stepNoConsumeMark = stepWith({ ...NOMUT, noConsumeMark: true })
  action stepNoResultFence = stepWith({ ...NOMUT, noResultFence: true })
  action stepNoProviderDedupe = stepWith({ ...NOMUT, noProviderDedupe: true })

  // --- Invariants ------------------------------------------------------
  // Candidate theorems (each has a mutant that refutes it, see manifest):
  // exactlyOnce, dispatchOnce, noStaleGeneration, noStaleTimer, atomicity,
  // resultFidelity, refusedNeverExecutes. Consistency checks that hold by
  // construction of this model (not theorem candidates): dispatchNeedsClaim,
  // replayEqualsLive (state and log are written by the same assignment),
  // progressEnabled (follows from the definition of outcomeUnknown).
  def sameDelivery(a: Entry, b: Entry): bool =
    a.id == b.id and a.gen == b.gen
      and (a.kind == "timer") == (b.kind == "timer")
  // EXACTLY-ONCE: a generation (or timer incarnation) is consumed at most once.
  def exactlyOnce = d.log.forall(a => d.log.forall(b => sameDelivery(a, b) implies a.seq == b.seq))
  // AT-MOST-ONCE DISPATCH: a generation is dispatched at most once, across crashes.
  def dispatchOnce = PAIRS.forall(p => g.dispatches.get(p) <= 1)
  def dispatchNeedsClaim = PAIRS.forall(p => g.dispatches.get(p) > 0 implies g.claimedEver.contains(p))
  // NO STALE GENERATION: a fenced (replaced) generation is never consumed.
  def noStaleGeneration = d.log.forall(e => e.kind == "result" implies not(g.replaced.contains((e.id, e.gen))))
  // NO STALE TIMER: a cancelled or replaced incarnation never fires, and a
  // fired incarnation is no longer scheduled.
  def noStaleTimer = d.log.forall(e => e.kind == "timer" implies
    (not(g.staleIncs.contains(e.gen)) and d.timerInc.get(e.id) != e.gen))
  // A lambda fold must end its conjunction: `quint compile --target tlaplus`
  // 0.32.0 prints `x = LET .. IN fold(..) /\ Q`, where the LET swallows Q.
  // (Apalache rejects a named fold function, so the lambda stays.)
  // consumedGen[c] is the highest command generation of c in the ledger.
  def consumedIsLoggedMax(c: int): bool = {
    val mine = d.log.filter(e => e.kind != "timer" and e.id == c)
    if (d.consumedGen.get(c) == 0) mine == Set()
    else mine.exists(e => e.gen == d.consumedGen.get(c)) and mine.forall(e => e.gen <= d.consumedGen.get(c))
  }
  // ATOMICITY: ledger head, consumption marks and facts move together.
  def atomicity = d.seq >= d.log.size() and d.log.forall(e => e.seq <= d.seq)
    and CMDS.forall(c => consumedIsLoggedMax(c))
  // REPLAY = LIVE: folding the ledger reproduces the live state, and a live
  // process's cache equals the durable state.
  def replayEqualsLive = (v.up and v.turn == NONE implies v.cache == d.facts)
    and d.facts == d.log.fold(0, (a, e) => a + e.value)
  // RESULT FIDELITY: a consumed result is the provider's one execution.
  def resultFidelity = d.log.forall(e => e.kind == "result" implies
    (env.done.get(e.id) != 0 and e.value == env.doneVal.get(e.id)))
  // NO LOST EFFECT: a refused command never executes at the provider.
  def refusedNeverExecutes = CMDS.forall(c => d.refusedGen.get(c) != 0 implies env.done.get(c) == 0)
  def invariant = exactlyOnce and dispatchOnce and dispatchNeedsClaim and noStaleGeneration
    and noStaleTimer and atomicity and replayEqualsLive and resultFidelity and refusedNeverExecutes
  temporal safety = always(invariant)

  // Non-vacuity witnesses: each must be reachable (a run violating
  // `not(witness)` exports the witnessing trace).
  // A generation created by reconcileReissue (not a plain replacement) was consumed.
  def witnessReissuedConsumed = d.log.exists(e => e.kind == "result" and g.reissued.contains((e.id, e.gen)))
  // The provider fenced and the cell has not committed the refusal (I12).
  def witnessFencedUncommitted = CMDS.exists(c => env.fenced.get(c) and d.refusedGen.get(c) == 0)
  def witnessRefused = d.log.exists(e => e.kind == "refused")
  def witnessStaleRefused = g.last == "refuseStale"
  // A fired incarnation that replaced a still-scheduled timer.
  def witnessRescheduledFired = d.log.exists(e => e.kind == "timer" and g.rescheduledIncs.contains(e.gen))
  // A result recorded by a later process incarnation than the one that
  // claimed it: the crash/eviction recovery path.
  def witnessRecordedAfterReopen = g.last == "record" and g.epoch > 0
    and g.claimEpoch.get(g.lastRecorded) < g.epoch

  def resolved(p: Pair): bool = d.consumedGen.get(p._1) >= p._2 or g.replaced.contains(p)
  // Enabledness check (by construction; not evidence for liveness): while a
  // claimed generation is unresolved, some step that could lower its rank is
  // enabled. Weak fairness alone does not give `claimedResolves`: unbounded
  // timer turns can keep the single turn slot busy and peekResult repeatedly
  // disabled. The intended premise is weak fairness plus a finite budget of
  // crashes, drops and timer churn (liveness/cell-lifecycle-live.qnt), or
  // strong fairness of peekResult. That liveness claim is NOT established:
  // TLC did not finish on the harness (results.json: unknown).
  def progressEnabled = PAIRS.forall(p => (g.claimedEver.contains(p) and not(resolved(p))) implies (
    not(v.up) or v.fresh.contains(p) or v.inflight.contains(p) or outcomeUnknown(p._1)
      or v.turn != NONE or d.resultGen.get(p._1) == p._2))
  def invariantWithProgress = invariant and progressEnabled

  // LIVENESS: every claimed generation is eventually consumed, refused or
  // fenced. This is the conclusion only; its fairness and finite-fault
  // premises are encoded in liveness/cell-lifecycle-live.qnt (`live`).
  temporal claimedResolves = PAIRS.forall(p =>
    always(g.claimedEver.contains(p) implies eventually(resolved(p))))
}
