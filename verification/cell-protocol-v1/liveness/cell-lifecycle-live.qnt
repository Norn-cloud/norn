// Liveness harness for cell_lifecycle (D83). Faults (crash, eviction and
// dropped messages) and timer churn get a finite budget, so the state space
// is finite and "eventually the faults stop" is explicit. Every recovery
// step is weakly fair. TLC was run on `live` over this instance and did not
// finish within its time budget; the verdict is unknown (results.json).
module cell_lifecycle_live {
  import cell_lifecycle as L from "../models/cell-lifecycle"

  pure val MAX_FAULTS = 2
  pure val MAX_TIMER_OPS = 2
  var faults: int
  var timerOps: int

  action init = all { L::init, faults' = 0, timerOps' = 0 }
  action same = all { faults' = faults, timerOps' = timerOps }
  action fault = all { faults < MAX_FAULTS, faults' = faults + 1, timerOps' = timerOps }
  action timerOp = all { timerOps < MAX_TIMER_OPS, timerOps' = timerOps + 1, faults' = faults }

  // Fair recovery and delivery steps.
  action reopenF = all { L::reopen, same }
  action dispatchF(p: L::Pair): bool = all { L::dispatch(p), same }
  action execF(p: L::Pair): bool = { nondet x = L::VALS.oneOf() all { L::providerExec(p, x), same } }
  action recordF(c: int): bool = all {
    L::env.net.exists(m => m.cmd == c),
    { nondet m = L::env.net.filter(m => m.cmd == c).oneOf()
      all { L::record(m), same } },
  }
  action timeoutF(p: L::Pair): bool = all { L::timeout(p), same }
  action reconcileF(c: int): bool = all { any { L::providerFence(c), L::reconcileRefuse(c), L::reconcileReissue(c) }, same }
  action peekResultF(c: int): bool = all { L::peekResult(c), same }
  action peekTimerF(t: int): bool = all { L::peekTimer(t), same }
  action commitF = all { L::commitTurn, same }
  action tickF = all { L::tick, same }

  action step = {
    nondet c = L::CMDS.oneOf()
    nondet t = L::TIMERS.oneOf()
    nondet p = L::PAIRS.oneOf()
    nondet due = 0.to(L::MAX_TIME).oneOf()
    any {
      reopenF, dispatchF(p), execF(p), recordF(c), timeoutF(p), reconcileF(c),
      peekResultF(c), peekTimerF(t), commitF, tickF,
      // unfair: new work
      all { L::enqueue(c), same }, all { L::claim(c), same },
      all { L::schedule(t, due), timerOp }, all { L::cancel(t), timerOp },
      // unfair and budgeted: faults
      all { L::crash, fault }, all { L::evict, fault },
      all { L::dropRequest(p), fault },
      all { L::env.net.size() > 0, { nondet m = L::env.net.oneOf() all { L::dropResult(m), fault } } },
    }
  }

  temporal fairness = and {
    weakFair(reopenF, (L::d, L::v, L::env, L::g, faults, timerOps)),
    weakFair(commitF, (L::d, L::v, L::env, L::g, faults, timerOps)),
    weakFair(tickF, (L::d, L::v, L::env, L::g, faults, timerOps)),
    L::PAIRS.forall(p => weakFair(dispatchF(p), (L::d, L::v, L::env, L::g, faults, timerOps))),
    L::PAIRS.forall(p => weakFair(execF(p), (L::d, L::v, L::env, L::g, faults, timerOps))),
    L::PAIRS.forall(p => weakFair(timeoutF(p), (L::d, L::v, L::env, L::g, faults, timerOps))),
    L::CMDS.forall(c => weakFair(recordF(c), (L::d, L::v, L::env, L::g, faults, timerOps))),
    L::CMDS.forall(c => weakFair(reconcileF(c), (L::d, L::v, L::env, L::g, faults, timerOps))),
    L::CMDS.forall(c => weakFair(peekResultF(c), (L::d, L::v, L::env, L::g, faults, timerOps))),
    L::TIMERS.forall(t => weakFair(peekTimerF(t), (L::d, L::v, L::env, L::g, faults, timerOps))),
  }
  // Every claimed generation is eventually consumed, refused or fenced.
  temporal claimedResolves = L::PAIRS.forall(p =>
    always(L::g.claimedEver.contains(p) implies eventually(L::resolved(p))))
  // Every due timer eventually fires (or is cancelled or rescheduled later).
  def due(t: int): bool = L::d.timerInc.get(t) != 0 and L::env.now >= L::d.timerDue.get(t)
  temporal timerFires = L::TIMERS.forall(t => always(due(t) implies eventually(not(due(t)))))
  temporal live = fairness implies (claimedResolves and timerFires)
  def invariant = L::invariantWithProgress
}
