// D83 L-model 4: a cross-cell commitment (#493; design consult on #493,
// "Commitments, escrow, guarantees and placement"): the source cell's
// governed action reserves and enqueues; the target cell accepts through
// its own governed action with a receipt, deduplicated by commitment id;
// the source reconciles (settled / refused / unknown) and may later
// compensate with a reverse commitment that can itself fail.
//
// The invariant is the consult's stronger one: conservation over settled +
// reserved + in-flight claims, never the original immediate law.
//
// Implicit states this model names:
//   X1 reserved + outbox pending  source committed, command not yet accepted
//   X2 on the wire                command, receipt and query messages are
//                                 duplicated, dropped and reordered
//   X3 accepted, unreceipted      target accepted; the source still holds
//                                 the reservation (the in-flight claim)
//   X4 overdue / unknown          deadline passed with no receipt; visible,
//                                 and a missed deadline stays missed
//   X5 fenced at the target       a query found the id unseen and fenced it:
//                                 a late command is refused, never accepted
//   X6 foreign receipt            a third cell's receipt for the same id
//   X7 compensation pending / failed
module cross_cell_commitment {
  type Commitment = { src: int, dst: int, k: int, deadline: int }
  type Receipt = { id: int, issuer: int, outcome: str }

  pure val A = 1
  pure val B = 2
  pure val C = 3                            // a third cell that shares the id space
  pure val CELLS = Set(1, 2, 3)
  pure val IDS = Set(1, 2)                  // 1: transfer A -> B; 2: its compensation B -> A
  pure val QTY = Set(1, 2)
  pure val CAP = 3                          // each cell's capacity law: avail + reserved <= CAP
  pure val INIT_AVAIL = Map(1 -> 2, 2 -> 1, 3 -> 0)
  pure val TOTAL = 3
  pure val MAX_TIME = 4
  pure val DEADLINE = 2
  pure val NO_C: Commitment = { src: 0, dst: 0, k: 0, deadline: 0 }

  var avail: int -> int
  var reserved: int -> int
  var consumed: int
  var cmt: int -> Commitment
  var srcStatus: int -> str                 // none | pending | unknown | settled | refused
  var tgtSeen: int -> str                   // none | accepted | refused | fenced
  var comp: str                             // none | pending | compensated | failed
  var cmdNet: Set[int]
  var rcptNet: Set[Receipt]
  var queryNet: Set[int]
  var now: int
  var missed: Set[int]                      // ghost: deadlines missed, never cleared
  var acceptCount: int -> int               // ghost
  var settledBy: int -> int                 // ghost: issuer of the receipt the source applied
  var lastAction: str

  action init = all {
    avail' = INIT_AVAIL, reserved' = CELLS.mapBy(_ => 0), consumed' = 0,
    cmt' = IDS.mapBy(_ => NO_C), srcStatus' = IDS.mapBy(_ => "none"),
    tgtSeen' = IDS.mapBy(_ => "none"), comp' = "none",
    cmdNet' = Set(), rcptNet' = Set(), queryNet' = Set(), now' = 0,
    missed' = Set(), acceptCount' = IDS.mapBy(_ => 0), settledBy' = IDS.mapBy(_ => 0),
    lastAction' = "init",
  }
  def capOk(c: int, extra: int): bool = avail.get(c) + reserved.get(c) + extra <= CAP

  // Source governed action: reserve and enqueue in one commit (X1).
  action propose(id: int, src: int, dst: int, k: int, compNext: str): bool = all {
    srcStatus.get(id) == "none", avail.get(src) >= k,
    avail' = avail.setBy(src, x => x - k), reserved' = reserved.setBy(src, x => x + k),
    cmt' = cmt.set(id, { src: src, dst: dst, k: k, deadline: if (now + DEADLINE > MAX_TIME) MAX_TIME else now + DEADLINE }),
    srcStatus' = srcStatus.set(id, "pending"), cmdNet' = cmdNet.union(Set(id)),
    consumed' = consumed, tgtSeen' = tgtSeen, comp' = compNext, rcptNet' = rcptNet,
    queryNet' = queryNet, now' = now, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "propose",
  }
  action proposeTransfer(k: int): bool = propose(1, A, B, k, comp)

  // Target governed action. A first delivery decides (accept under the
  // capacity law, or refuse by policy) and records its receipt in the same
  // commit; a duplicate re-sends the recorded receipt (X2). `dedupe` false
  // is the variant that decides every delivery afresh.
  action accept(id: int, refuseByPolicy: bool, dedupe: bool): bool = {
    val c = cmt.get(id)
    val fresh = tgtSeen.get(id) == "none" or (not(dedupe) and tgtSeen.get(id) == "accepted")
    val ok = not(refuseByPolicy) and capOk(c.dst, c.k)
    all {
      cmdNet.contains(id),
      if (fresh) all {
        avail' = if (ok) avail.setBy(c.dst, x => x + c.k) else avail,
        tgtSeen' = tgtSeen.set(id, if (ok) "accepted" else "refused"),
        acceptCount' = if (ok) acceptCount.setBy(id, x => x + 1) else acceptCount,
        rcptNet' = rcptNet.union(Set({ id: id, issuer: c.dst, outcome: if (ok) "accepted" else "refused" })),
        lastAction' = if (ok) "accept" else "refuse",
      } else all {
        avail' = avail, tgtSeen' = tgtSeen, acceptCount' = acceptCount,
        rcptNet' = rcptNet.union(Set({ id: id, issuer: c.dst,
          outcome: if (tgtSeen.get(id) == "accepted") "accepted" else "refused" })),
        lastAction' = "redeliverReceipt",
      },
      reserved' = reserved, consumed' = consumed, cmt' = cmt, srcStatus' = srcStatus,
      comp' = comp, cmdNet' = cmdNet, queryNet' = queryNet, now' = now, missed' = missed,
      settledBy' = settledBy,
    }
  }
  // Another cell emits a receipt for the same id (aliasing or a misroute, X6).
  action foreignReceipt(id: int, outcome: str): bool = all {
    srcStatus.get(id) != "none",
    rcptNet' = rcptNet.union(Set({ id: id, issuer: C, outcome: outcome })),
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    srcStatus' = srcStatus, tgtSeen' = tgtSeen, comp' = comp, cmdNet' = cmdNet,
    queryNet' = queryNet, now' = now, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "foreignReceipt",
  }
  // Source reconciliation. A receipt counts only if its issuer is the
  // commitment's target; `checkIssuer` false is the variant that trusts any.
  action reconcile(r: Receipt, checkIssuer: bool): bool = {
    val c = cmt.get(r.id)
    val settled = r.outcome == "accepted"
    all {
      rcptNet.contains(r), Set("pending", "unknown").contains(srcStatus.get(r.id)),
      not(checkIssuer) or r.issuer == c.dst,
      srcStatus' = srcStatus.set(r.id, if (settled) "settled" else "refused"),
      reserved' = reserved.setBy(c.src, x => x - c.k),
      avail' = if (settled) avail else avail.setBy(c.src, x => x + c.k),
      settledBy' = settledBy.set(r.id, r.issuer),
      comp' = if (r.id == 2 and comp == "pending") (if (settled) "compensated" else "failed") else comp,
      consumed' = consumed, cmt' = cmt, tgtSeen' = tgtSeen, cmdNet' = cmdNet,
      rcptNet' = rcptNet, queryNet' = queryNet, now' = now, missed' = missed,
      acceptCount' = acceptCount, lastAction' = if (settled) "settle" else "release",
    }
  }
  // X4: the deadline passes with no receipt. Visible, and recorded forever.
  action overdue(id: int): bool = all {
    srcStatus.get(id) == "pending", now >= cmt.get(id).deadline,
    srcStatus' = srcStatus.set(id, "unknown"), missed' = missed.union(Set(id)),
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    tgtSeen' = tgtSeen, comp' = comp, cmdNet' = cmdNet, rcptNet' = rcptNet,
    queryNet' = queryNet, now' = now, acceptCount' = acceptCount, settledBy' = settledBy,
    lastAction' = "overdue",
  }
  action query(id: int): bool = all {
    srcStatus.get(id) == "unknown",
    queryNet' = queryNet.union(Set(id)),
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    srcStatus' = srcStatus, tgtSeen' = tgtSeen, comp' = comp, cmdNet' = cmdNet,
    rcptNet' = rcptNet, now' = now, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "query",
  }
  // The target answers a query. An unseen id is fenced (X5) and refused, so
  // a late command can never be accepted afterwards. `fence` false is the
  // variant that answers "refused" but leaves the id open.
  action answer(id: int, fence: bool): bool = {
    val c = cmt.get(id)
    val unseen = tgtSeen.get(id) == "none"
    all {
      queryNet.contains(id),
      tgtSeen' = if (unseen and fence) tgtSeen.set(id, "fenced") else tgtSeen,
      rcptNet' = rcptNet.union(Set({ id: id, issuer: c.dst,
        outcome: if (tgtSeen.get(id) == "accepted") "accepted" else "refused" })),
      avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
      srcStatus' = srcStatus, comp' = comp, cmdNet' = cmdNet, queryNet' = queryNet,
      now' = now, missed' = missed, acceptCount' = acceptCount, settledBy' = settledBy,
      lastAction' = "answer",
    }
  }
  // A business reversal after settlement: the target compensates with a
  // reverse commitment, which fails at once if it no longer holds the units.
  action compensate = {
    val c = cmt.get(1)
    all {
      srcStatus.get(1) == "settled", comp == "none",
      if (avail.get(c.dst) >= c.k) all {
        propose(2, c.dst, c.src, c.k, "pending"),
      } else all {
        comp' = "failed",
        avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
        srcStatus' = srcStatus, tgtSeen' = tgtSeen, cmdNet' = cmdNet, rcptNet' = rcptNet,
        queryNet' = queryNet, now' = now, missed' = missed, acceptCount' = acceptCount,
        settledBy' = settledBy, lastAction' = "compensationFailed",
      },
    }
  }
  // A cell legitimately uses up stock (it leaves the system).
  action consume(c: int, k: int): bool = all {
    avail.get(c) >= k,
    avail' = avail.setBy(c, x => x - k), consumed' = consumed + k,
    reserved' = reserved, cmt' = cmt, srcStatus' = srcStatus, tgtSeen' = tgtSeen,
    comp' = comp, cmdNet' = cmdNet, rcptNet' = rcptNet, queryNet' = queryNet, now' = now,
    missed' = missed, acceptCount' = acceptCount, settledBy' = settledBy, lastAction' = "consume",
  }
  action dropCmd(id: int): bool = all {
    cmdNet.contains(id), cmdNet' = cmdNet.exclude(Set(id)),
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    srcStatus' = srcStatus, tgtSeen' = tgtSeen, comp' = comp, rcptNet' = rcptNet,
    queryNet' = queryNet, now' = now, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "dropCmd",
  }
  action dropReceipt(r: Receipt): bool = all {
    rcptNet.contains(r), rcptNet' = rcptNet.exclude(Set(r)),
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    srcStatus' = srcStatus, tgtSeen' = tgtSeen, comp' = comp, cmdNet' = cmdNet,
    queryNet' = queryNet, now' = now, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "dropReceipt",
  }
  action dropQuery(id: int): bool = all {
    queryNet.contains(id), queryNet' = queryNet.exclude(Set(id)),
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    srcStatus' = srcStatus, tgtSeen' = tgtSeen, comp' = comp, cmdNet' = cmdNet,
    rcptNet' = rcptNet, now' = now, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "dropQuery",
  }
  action tick = all {
    now < MAX_TIME, now' = now + 1,
    avail' = avail, reserved' = reserved, consumed' = consumed, cmt' = cmt,
    srcStatus' = srcStatus, tgtSeen' = tgtSeen, comp' = comp, cmdNet' = cmdNet,
    rcptNet' = rcptNet, queryNet' = queryNet, missed' = missed, acceptCount' = acceptCount,
    settledBy' = settledBy, lastAction' = "tick",
  }

  action stepWith(checkIssuer: bool, fence: bool, dedupe: bool): bool = {
    nondet id = IDS.oneOf()
    nondet k = QTY.oneOf()
    nondet c = Set(A, B).oneOf()
    nondet policy = Set(false, true).oneOf()
    nondet outcome = Set("accepted", "refused").oneOf()
    nondet r = rcptNet.union(Set({ id: 1, issuer: 0, outcome: "none" })).oneOf()
    any {
      proposeTransfer(k), accept(id, policy, dedupe), foreignReceipt(id, outcome),
      reconcile(r, checkIssuer), overdue(id), query(id), answer(id, fence), compensate,
      consume(c, k), dropCmd(id), dropReceipt(r), dropQuery(id), tick,
    }
  }
  action step = stepWith(true, true, true)
  action stepNoIssuerCheck = stepWith(false, true, true)
  action stepNoFence = stepWith(true, false, true)
  action stepNoDedupe = stepWith(true, true, false)

  // --- Invariants (candidate theorem statements) -------------------------
  // In-flight claims: accepted at the target, still reserved at the source.
  def inFlight: int = IDS.fold(0, (a, id) => a +
    (if (tgtSeen.get(id) == "accepted" and Set("pending", "unknown").contains(srcStatus.get(id)))
      cmt.get(id).k else 0))
  // CONSERVATION over settled + reserved + in-flight claims (the consult's
  // stronger invariant): units are never duplicated or lost by the protocol.
  def conservation = CELLS.fold(0, (a, c) => a + avail.get(c) + reserved.get(c))
    + consumed - inFlight == TOTAL
  def nonNegative = CELLS.forall(c => avail.get(c) >= 0 and reserved.get(c) >= 0)
  def capacityLaw = CELLS.forall(c => avail.get(c) + reserved.get(c) <= CAP)
  // EXACTLY-ONCE ACCEPTANCE: deduplicated by commitment id.
  def acceptOnce = IDS.forall(id => acceptCount.get(id) <= 1)
  // AGREEMENT: the source's outcome matches the target's decision, and a
  // refused or fenced id is never later accepted.
  def agreement = IDS.forall(id =>
    (srcStatus.get(id) == "settled" implies tgtSeen.get(id) == "accepted")
      and (srcStatus.get(id) == "refused" implies Set("refused", "fenced").contains(tgtSeen.get(id))))
  // RECEIPT BINDING: only the target's own receipt reconciles a commitment.
  def receiptBinding = IDS.forall(id => Set("settled", "refused").contains(srcStatus.get(id))
    implies settledBy.get(id) == cmt.get(id).dst)
  // DEADLINE HONESTY: unknown is visible and every missed deadline is kept.
  def deadlineHonest = IDS.forall(id => srcStatus.get(id) == "unknown" implies missed.contains(id))
  // COMPENSATION: pending only while its commitment is open; never silent.
  def compensationTracked = (comp == "pending" implies Set("pending", "unknown").contains(srcStatus.get(2)))
    and (comp == "compensated" implies srcStatus.get(2) == "settled")
  def invariant = conservation and nonNegative and capacityLaw and acceptOnce and agreement
    and receiptBinding and deadlineHonest and compensationTracked
  temporal safety = always(invariant)

  // Non-vacuity witnesses: each must be reachable.
  def witnessSettled = srcStatus.get(1) == "settled"
  def witnessRefused = srcStatus.get(1) == "refused" and tgtSeen.get(1) == "refused"
  def witnessFencedThenRefused = srcStatus.get(1) == "refused" and tgtSeen.get(1) == "fenced"
  def witnessUnknownThenSettled = srcStatus.get(1) == "settled" and missed.contains(1)
  def witnessCompensated = comp == "compensated"
  def witnessCompensationFailed = comp == "failed"
  def witnessForeignIgnored = rcptNet.exists(r => r.issuer == C) and srcStatus.get(1) == "settled"

  // A commitment is resolved once its source has settled or released it.
  def resolvedId(id: int): bool = Set("settled", "refused").contains(srcStatus.get(id))
  // LIVENESS: the conclusion only; its fairness and finite-fault premises
  // are encoded in liveness/cross-cell-commitment-live.qnt (`live`).
  temporal commitmentResolves = IDS.forall(id =>
    always(srcStatus.get(id) == "pending" implies eventually(resolvedId(id))))
}
