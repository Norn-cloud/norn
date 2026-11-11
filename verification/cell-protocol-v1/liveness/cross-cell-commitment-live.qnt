// Liveness harness for cross_cell_commitment (D83). Drops, foreign
// receipts and consumption get a finite budget; delivery, the target's
// decision, reconciliation, the deadline, query, answer and time are weakly
// fair. TLC checks `live`.
module cross_cell_commitment_live {
  import cross_cell_commitment as L from "../models/cross-cell-commitment"

  pure val MAX_FAULTS = 2
  var faults: int
  action init = all { L::init, faults' = 0 }
  action same = faults' = faults
  action fault = all { faults < MAX_FAULTS, faults' = faults + 1 }

  action acceptF(id: int): bool = { nondet policy = Set(false, true).oneOf() all { L::accept(id, policy, true), same } }
  // Reconcile with a receipt from the commitment's target.
  action reconcileF(id: int): bool = all {
    L::rcptNet.exists(r => r.id == id and r.issuer == L::cmt.get(id).dst),
    { nondet r = L::rcptNet.filter(r => r.id == id and r.issuer == L::cmt.get(id).dst).oneOf()
      all { L::reconcile(r, true), same } },
  }
  action overdueF(id: int): bool = all { L::overdue(id), same }
  action queryF(id: int): bool = all { L::query(id), same }
  action answerF(id: int): bool = all { L::answer(id, true), same }
  action tickF = all { L::tick, same }

  action step = {
    nondet id = L::IDS.oneOf()
    nondet k = L::QTY.oneOf()
    nondet c = Set(L::A, L::B).oneOf()
    nondet outcome = Set("accepted", "refused").oneOf()
    any {
      acceptF(id), reconcileF(id), overdueF(id), queryF(id), answerF(id), tickF,
      all { L::proposeTransfer(k), same }, all { L::compensate, same },
      // a receipt from any issuer may be offered; only the target's counts
      all { L::rcptNet.size() > 0, { nondet r = L::rcptNet.oneOf() all { L::reconcile(r, true), same } } },
      // unfair and budgeted
      all { L::foreignReceipt(id, outcome), fault }, all { L::consume(c, k), fault },
      all { L::dropCmd(id), fault }, all { L::dropQuery(id), fault },
      all { L::rcptNet.size() > 0, { nondet r = L::rcptNet.oneOf() all { L::dropReceipt(r), fault } } },
    }
  }

  temporal fairness = and {
    weakFair(tickF, (L::avail, L::reserved, L::consumed, L::cmt, L::srcStatus, L::tgtSeen, L::comp, L::cmdNet, L::rcptNet, L::queryNet, L::now, L::missed, L::acceptCount, L::settledBy, L::lastAction, faults)),
    L::IDS.forall(id => weakFair(acceptF(id), (L::avail, L::reserved, L::consumed, L::cmt, L::srcStatus, L::tgtSeen, L::comp, L::cmdNet, L::rcptNet, L::queryNet, L::now, L::missed, L::acceptCount, L::settledBy, L::lastAction, faults))),
    L::IDS.forall(id => weakFair(reconcileF(id), (L::avail, L::reserved, L::consumed, L::cmt, L::srcStatus, L::tgtSeen, L::comp, L::cmdNet, L::rcptNet, L::queryNet, L::now, L::missed, L::acceptCount, L::settledBy, L::lastAction, faults))),
    L::IDS.forall(id => weakFair(overdueF(id), (L::avail, L::reserved, L::consumed, L::cmt, L::srcStatus, L::tgtSeen, L::comp, L::cmdNet, L::rcptNet, L::queryNet, L::now, L::missed, L::acceptCount, L::settledBy, L::lastAction, faults))),
    L::IDS.forall(id => weakFair(queryF(id), (L::avail, L::reserved, L::consumed, L::cmt, L::srcStatus, L::tgtSeen, L::comp, L::cmdNet, L::rcptNet, L::queryNet, L::now, L::missed, L::acceptCount, L::settledBy, L::lastAction, faults))),
    L::IDS.forall(id => weakFair(answerF(id), (L::avail, L::reserved, L::consumed, L::cmt, L::srcStatus, L::tgtSeen, L::comp, L::cmdNet, L::rcptNet, L::queryNet, L::now, L::missed, L::acceptCount, L::settledBy, L::lastAction, faults))),
  }
  // Every proposed commitment is eventually settled or refused, and a
  // requested compensation eventually completes or fails visibly.
  temporal commitmentResolves = L::IDS.forall(id =>
    always(L::srcStatus.get(id) == "pending" implies eventually(L::resolvedId(id))))
  temporal compensationEnds = always(L::comp == "pending" implies eventually(L::comp != "pending"))
  temporal live = fairness implies (commitmentResolves and compensationEnds)
  def invariant = L::invariant
}
