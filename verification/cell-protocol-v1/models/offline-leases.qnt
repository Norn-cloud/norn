// D83 L-model 3: per-device stock leases, offline sales, deterministic sync,
// the shortfall bucket and resolution obligations with deadlines (#1225).
//
// Facts (a sale already made, a count) are never refused; requests (an
// online order) may be. Conservation is strict; availability >= 0 is the
// defeasible policy whose excess lands in Shortfall, never in negative stock.
//
// Implicit states this model names:
//   K1 unsynced fact     recorded on a device, not yet applied (device queue)
//   K2 on the wire       a fact or ack in flight: duplicated, reordered, dropped
//   K3 gap               a fact whose predecessor has not been applied waits
//   K4 grant in flight   the cell granted lease units the device has not seen
//   K5 return in flight  the device gave units back; the cell has not seen it.
//                        It must travel in the device's ordered stream: an
//                        unsequenced return races later sales (variant below)
//   K6 local deficit     a sale beyond the device's lease leaves its local
//                        balance negative, which a later grant must repay
//   K7 obligation        open -> resolved | escalated (deadline passed, still owed)
module offline_leases {
  // A device stream record: a sale (a fact) or a lease return, sequenced together.
  type Fact = { d: int, lseq: int, q: int, within: bool, kind: str }
  type Obl = { qty: int, deadline: int, state: str }
  type Books = { avail: int, leased: int -> int, delivered: int, shortfall: int,
    refunded: int, loss: int, received: int, gain: int, sold: int,
    obl: int -> Obl, nextObl: int }
  type Event = { kind: str, d: int, q: int, k: int, o: int }
  type Device = { local: int, nextLseq: int, queue: Set[Fact], recorded: Set[Fact],
    grantIn: int, returnOut: int }

  pure val DEVICES = Set(1, 2)
  pure val INIT = 2
  pure val QTY = Set(1, 2)
  pure val MAX_FACTS = 3                    // stream records per device
  pure val MAX_RECEIVED = 2
  pure val MAX_TIME = 4
  pure val DEADLINE = 2
  pure val RISK_LIMIT = 2                   // open shortfall above which requests refuse
  pure val OBLS = 1.to(6)                   // at most one per stream record
  pure val OBL_SLOTS = 0.to(6)              // slot 0 is never used
  pure val NO_OBL: Obl = { qty: 0, deadline: 0, state: "none" }

  var books: Books
  var applied: int -> int                   // highest local seq applied per device
  var cellLog: List[Event]                  // the single writer's ordered log
  var factLog: List[{ f: Fact, excess: int }]
  var dev: int -> Device
  var partitioned: int -> bool
  var inbox: Set[Fact]
  var acks: Set[(int, int)]
  var now: int
  var lastAction: str

  pure val genesis: Books = { avail: INIT, leased: DEVICES.mapBy(_ => 0), delivered: 0,
    shortfall: 0, refunded: 0, loss: 0, received: 0, gain: 0, sold: 0,
    obl: OBL_SLOTS.mapBy(_ => NO_OBL), nextObl: 1 }
  action init = all {
    books' = genesis, applied' = DEVICES.mapBy(_ => 0), cellLog' = List(), factLog' = List(),
    dev' = DEVICES.mapBy(_ => { local: 0, nextLseq: 0, queue: Set(), recorded: Set(),
      grantIn: 0, returnOut: 0 }),
    partitioned' = DEVICES.mapBy(_ => false), inbox' = Set(), acks' = Set(), now' = 0,
    lastAction' = "init",
  }

  pure def min(a: int, b: int): int = if (a < b) a else b
  // The excess a fact cannot draw from its device's lease or from the
  // shared available stock: it becomes shortfall.
  pure def excessOf(b: Books, e: Event): int = {
    val rest = e.q - min(e.q, b.leased.get(e.d))
    rest - min(rest, b.avail)
  }
  // The cell's state transition, a pure function of the ordered log. The
  // obligation deadline rides in `k` for facts.
  pure def applyEvent(b: Books, e: Event): Books = {
    val o = b.obl.get(e.o)
    if (e.kind == "fact") {
      val fromLease = min(e.q, b.leased.get(e.d))
      val rest = e.q - fromLease
      val fromAvail = min(rest, b.avail)
      val excess = rest - fromAvail
      { ...b, leased: b.leased.set(e.d, b.leased.get(e.d) - fromLease), avail: b.avail - fromAvail,
        delivered: b.delivered + fromLease + fromAvail, shortfall: b.shortfall + excess,
        sold: b.sold + e.q,
        obl: if (excess > 0) b.obl.set(b.nextObl, { qty: excess, deadline: e.k, state: "open" }) else b.obl,
        nextObl: if (excess > 0) b.nextObl + 1 else b.nextObl }
    } else if (e.kind == "request") { ...b, avail: b.avail - e.q, delivered: b.delivered + e.q, sold: b.sold + e.q }
    else if (e.kind == "grant") { ...b, avail: b.avail - e.k, leased: b.leased.setBy(e.d, x => x + e.k) }
    else if (e.kind == "return") { ...b, avail: b.avail + e.k, leased: b.leased.setBy(e.d, x => x - e.k) }
    else if (e.kind == "reclaim") { ...b, avail: b.avail + b.leased.get(e.d), leased: b.leased.set(e.d, 0) }
    else if (e.kind == "receive") { ...b, avail: b.avail + e.k, received: b.received + e.k }
    else if (e.kind == "loss") { ...b, avail: b.avail - e.k, loss: b.loss + e.k }
    else if (e.kind == "escalate") { ...b, obl: b.obl.set(e.o, { ...o, state: "escalated" }) }
    else {
      // fulfill (from stock), refund (sale cancelled), gain (count found it)
      val closed = { ...o, qty: o.qty - e.k, state: if (o.qty == e.k) "resolved" else o.state }
      val b1 = { ...b, obl: b.obl.set(e.o, closed), shortfall: b.shortfall - e.k }
      if (e.kind == "fulfill") { ...b1, avail: b.avail - e.k, delivered: b.delivered + e.k }
      else if (e.kind == "refund") { ...b1, refunded: b.refunded + e.k }
      else { ...b1, gain: b.gain + e.k, delivered: b.delivered + e.k }
    }
  }
  pure def replay(log: List[Event]): Books = log.foldl(genesis, (b, e) => applyEvent(b, e))

  // A cell commit: append to the log and apply. Nothing else changes books.
  action commitEvent(e: Event, label: str): bool = all {
    books' = applyEvent(books, e), cellLog' = cellLog.append(e), lastAction' = label,
  }
  def owed(o: Obl): bool = Set("open", "escalated").contains(o.state)

  // --- Devices ---------------------------------------------------------
  // A sale is a fact: always recorded. Within the lease iff the local
  // balance covers it; `clampDeficit` is the naive variant that forgets K6.
  action sell(d: int, q: int, clampDeficit: bool): bool = {
    val dv = dev.get(d)
    val f = { d: d, lseq: dv.nextLseq + 1, q: q, within: dv.local >= q, kind: "sale" }
    val left = dv.local - q
    all {
      dv.nextLseq < MAX_FACTS,
      dev' = dev.set(d, { ...dv, nextLseq: f.lseq, queue: dv.queue.union(Set(f)),
        recorded: dv.recorded.union(Set(f)),
        local: if (clampDeficit and left < 0) 0 else left }),
      books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog,
      partitioned' = partitioned, inbox' = inbox, acks' = acks, now' = now, lastAction' = "sell",
    }
  }
  // Retransmit every unacknowledged fact (duplication on the wire).
  action send(d: int): bool = all {
    not(partitioned.get(d)), dev.get(d).queue != Set(),
    inbox' = inbox.union(dev.get(d).queue),
    books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog, dev' = dev,
    partitioned' = partitioned, acks' = acks, now' = now, lastAction' = "send",
  }
  action dropFact(f: Fact): bool = all {
    inbox.contains(f), inbox' = inbox.exclude(Set(f)),
    books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog, dev' = dev,
    partitioned' = partitioned, acks' = acks, now' = now, lastAction' = "dropFact",
  }
  action receiveAck(a: (int, int)): bool = {
    val dv = dev.get(a._1)
    all {
      acks.contains(a), not(partitioned.get(a._1)),
      dev' = dev.set(a._1, { ...dv, queue: dv.queue.filter(f => f.lseq != a._2) }),
      acks' = acks.exclude(Set(a)),
      books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog,
      partitioned' = partitioned, inbox' = inbox, now' = now, lastAction' = "ack",
    }
  }
  action dropAck(a: (int, int)): bool = all {
    acks.contains(a), acks' = acks.exclude(Set(a)),
    books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog, dev' = dev,
    partitioned' = partitioned, inbox' = inbox, now' = now, lastAction' = "dropAck",
  }
  action deliverGrant(d: int): bool = {
    val dv = dev.get(d)
    all {
      not(partitioned.get(d)), dv.grantIn > 0,
      dev' = dev.set(d, { ...dv, local: dv.local + dv.grantIn, grantIn: 0 }),
      books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog,
      partitioned' = partitioned, inbox' = inbox, acks' = acks, now' = now, lastAction' = "deliverGrant",
    }
  }
  // The device gives back its unused balance and stops selling against it
  // at once. Sequenced: the return is the next record in its stream, so the
  // cell applies it after every earlier sale and before every later one.
  // Unsequenced (variant): a side message the cell credits when it lands.
  action returnLease(d: int, sequenced: bool): bool = {
    val dv = dev.get(d)
    val r = { d: d, lseq: dv.nextLseq + 1, q: dv.local, within: false, kind: "return" }
    all {
      dv.local > 0, not(sequenced) or dv.nextLseq < MAX_FACTS,
      dev' = dev.set(d, if (sequenced) { ...dv, local: 0, nextLseq: r.lseq,
          queue: dv.queue.union(Set(r)), recorded: dv.recorded.union(Set(r)) }
        else { ...dv, local: 0, returnOut: dv.returnOut + dv.local }),
      books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog,
      partitioned' = partitioned, inbox' = inbox, acks' = acks, now' = now, lastAction' = "returnLease",
    }
  }
  action setPartition(d: int, p: bool): bool = all {
    partitioned.get(d) != p, partitioned' = partitioned.set(d, p),
    books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog, dev' = dev,
    inbox' = inbox, acks' = acks, now' = now, lastAction' = if (p) "partition" else "heal",
  }
  action tick = all {
    now < MAX_TIME, now' = now + 1,
    books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog, dev' = dev,
    partitioned' = partitioned, inbox' = inbox, acks' = acks, lastAction' = "tick",
  }

  // --- The cell (single writer) ----------------------------------------
  // Deterministic sync: per device in local-seq order; a duplicate is
  // re-acknowledged and changes nothing; a gap waits (K3).
  action applyFact(f: Fact): bool = {
    val e = if (f.kind == "return") { kind: "return", d: f.d, q: 0, k: f.q, o: 0 }
      else { kind: "fact", d: f.d, q: f.q, k: min(now + DEADLINE, MAX_TIME), o: 0 }
    all {
      inbox.contains(f), f.lseq <= applied.get(f.d) + 1,
      acks' = acks.union(Set((f.d, f.lseq))),
      dev' = dev, partitioned' = partitioned, inbox' = inbox, now' = now,
      if (f.lseq <= applied.get(f.d)) all {
        books' = books, applied' = applied, cellLog' = cellLog, factLog' = factLog,
        lastAction' = "duplicateFact",
      } else all {
        commitEvent(e, "applyFact"),
        applied' = applied.set(f.d, f.lseq),
        factLog' = factLog.append({ f: f, excess: if (f.kind == "return") 0 else excessOf(books, e) }),
      },
    }
  }
  action cellOnly(e: Event, label: str): bool = all {
    commitEvent(e, label),
    applied' = applied, factLog' = factLog, dev' = dev, partitioned' = partitioned,
    inbox' = inbox, acks' = acks, now' = now,
  }
  // A request draws only from available stock, within the risk limit.
  action request(q: int): bool = all {
    books.avail >= q, books.shortfall <= RISK_LIMIT,
    cellOnly({ kind: "request", d: 0, q: q, k: 0, o: 0 }, "request"),
  }
  action grant(d: int, k: int): bool = all {
    books.avail >= k,
    commitEvent({ kind: "grant", d: d, q: 0, k: k, o: 0 }, "grant"),
    dev' = dev.setBy(d, dv => { ...dv, grantIn: dv.grantIn + k }),
    applied' = applied, factLog' = factLog, partitioned' = partitioned,
    inbox' = inbox, acks' = acks, now' = now,
  }
  action landReturn(d: int): bool = {
    val dv = dev.get(d)
    all {
      not(partitioned.get(d)), dv.returnOut > 0,
      commitEvent({ kind: "return", d: d, q: 0, k: dv.returnOut, o: 0 }, "landReturn"),
      dev' = dev.set(d, { ...dv, returnOut: 0 }),
      applied' = applied, factLog' = factLog, partitioned' = partitioned,
      inbox' = inbox, acks' = acks, now' = now,
    }
  }
  // Variant: the cell takes back a partitioned device's lease on a timeout,
  // while the device still believes it holds it.
  action reclaimOffline(d: int): bool = all {
    partitioned.get(d), books.leased.get(d) > 0,
    cellOnly({ kind: "reclaim", d: d, q: 0, k: 0, o: 0 }, "reclaimOffline"),
  }
  action receive(k: int): bool = all {
    books.received + k <= MAX_RECEIVED,
    cellOnly({ kind: "receive", d: 0, q: 0, k: k, o: 0 }, "receive"),
  }
  action postLoss(k: int): bool = all {
    books.avail >= k,
    cellOnly({ kind: "loss", d: 0, q: 0, k: k, o: 0 }, "loss"),
  }
  // Resolutions are governed actions with receipts: each is a cell commit.
  action resolve(o: int, kind: str, k: int): bool = {
    val ob = books.obl.get(o)
    all {
      owed(ob), k >= 1, k <= ob.qty, kind != "fulfill" or books.avail >= k,
      cellOnly({ kind: kind, d: 0, q: 0, k: k, o: o }, kind),
    }
  }
  action escalate(o: int): bool = all {
    books.obl.get(o).state == "open", now >= books.obl.get(o).deadline,
    cellOnly({ kind: "escalate", d: 0, q: 0, k: 0, o: o }, "escalate"),
  }

  action stepWith(clampDeficit: bool, reclaim: bool, sequenced: bool): bool = {
    nondet d = DEVICES.oneOf()
    nondet q = QTY.oneOf()
    nondet o = OBLS.oneOf()
    nondet kind = Set("fulfill", "refund", "gain").oneOf()
    nondet p = Set(false, true).oneOf()
    nondet f = inbox.union(Set({ d: 1, lseq: 0, q: 1, within: false, kind: "sale" })).oneOf()
    nondet a = acks.union(Set((1, 0))).oneOf()
    any {
      sell(d, q, clampDeficit), send(d), dropFact(f), receiveAck(a), dropAck(a),
      deliverGrant(d), returnLease(d, sequenced), setPartition(d, p), tick,
      applyFact(f), request(q), grant(d, q), landReturn(d),
      receive(q), postLoss(q), resolve(o, kind, q), escalate(o),
      if (reclaim) reclaimOffline(d) else tick,
    }
  }
  action step = stepWith(false, false, true)
  action stepClampDeficit = stepWith(true, false, true)
  action stepReclaimOffline = stepWith(false, true, true)
  action stepUnsequencedReturn = stepWith(false, false, false)

  // --- Invariants ------------------------------------------------------
  // Candidate theorems, each refuted by a recorded mutant: onHandNonNegative
  // (stepUnsequencedReturn), withinLeaseNeverShort (stepClampDeficit,
  // stepReclaimOffline). No recorded mutant yet: localTracksLease,
  // conservation, shortfallObligated, factNeverRefused, prefixApplied. Hold by construction
  // here (consistency checks, not theorem candidates): replayEqualsLive (the
  // books and the log are written by the same commit), shortfallOnlyFromFacts,
  // obligationProgress (deadlines are clamped to MAX_TIME).
  def leasedTotal = DEVICES.fold(0, (a, d) => a + books.leased.get(d))
  def owedTotal = OBLS.fold(0, (a, o) => a + (if (owed(books.obl.get(o))) books.obl.get(o).qty else 0))
  // CONSERVATION: every unit is on hand (available or leased), delivered or
  // lost; every sold unit is delivered, owed (shortfall) or refunded.
  def conservation = books.avail + leasedTotal + books.delivered + books.loss
      == INIT + books.received + books.gain
    and books.sold == books.delivered + books.shortfall + books.refunded
  def onHandNonNegative = books.avail >= 0 and DEVICES.forall(d => books.leased.get(d) >= 0)
  // SHORTFALL = OWED OBLIGATIONS: no owed unit without an obligation.
  def shortfallObligated = books.shortfall == owedTotal and books.shortfall >= 0
    and OBLS.forall(o => books.obl.get(o).qty >= 0
      and (books.obl.get(o).state == "resolved" implies books.obl.get(o).qty == 0)
      and (owed(books.obl.get(o)) implies books.obl.get(o).qty > 0))
  // A FACT IS NEVER REFUSED: every recorded fact is queued or applied, and
  // the cell applies each device's facts as a gap-free prefix.
  def factNeverRefused = DEVICES.forall(d => dev.get(d).recorded.forall(f =>
    dev.get(d).queue.contains(f) or f.lseq <= applied.get(d)))
  def prefixApplied = DEVICES.forall(d => applied.get(d) <= dev.get(d).nextLseq
    and applied.get(d) == factLog.select(x => x.f.d == d).length())
  // DETERMINISTIC SYNC / REPLAY = LIVE: the books are a pure fold of the
  // single writer's ordered log.
  def replayEqualsLive = replay(cellLog) == books
  // REQUESTS NEVER OVERSELL: only facts create shortfall.
  def shortfallOnlyFromFacts = factLog.foldl(0, (a, x) => a + x.excess) >= books.shortfall
  // LEASE SOUNDNESS: a sale within the device's lease never becomes shortfall.
  def withinLeaseNeverShort = factLog.foldl(true, (a, x) => a and (x.f.within implies x.excess == 0))
  // The device's local balance never exceeds what the cell holds for it,
  // net of grants and returns in flight and its unsynced sales (K4, K5, K6).
  // One-sided on purpose, and the gap is a known hazard (manifest
  // `exclusions`, #1225): lease 1, sell 2 offline (local -1), apply (lease
  // 0), grant 1 (local 0, lease 1). The cell then holds a unit for the
  // device that the device can neither sell within its lease nor return.
  def unappliedQty(d: int): int =
    dev.get(d).recorded.fold(0, (a, f) => a + (if (f.lseq > applied.get(d)) f.q else 0))
  def localTracksLease = DEVICES.forall(d => dev.get(d).local <=
    books.leased.get(d) - dev.get(d).grantIn - dev.get(d).returnOut - unappliedQty(d))

  // Enabledness check (true by construction, since deadlines are clamped to
  // MAX_TIME; not evidence for liveness): an open obligation can always
  // advance time or escalate.
  def obligationProgress = OBLS.forall(o => books.obl.get(o).state == "open" implies
    (books.obl.get(o).deadline <= MAX_TIME and (now < MAX_TIME or now >= books.obl.get(o).deadline)))
  def invariant = conservation and onHandNonNegative and shortfallObligated and factNeverRefused
    and prefixApplied and replayEqualsLive and shortfallOnlyFromFacts and withinLeaseNeverShort
    and localTracksLease and obligationProgress

  // Non-vacuity witnesses: each must be reachable.
  def witnessResolved = OBLS.exists(o => books.obl.get(o).state == "resolved")
  def witnessEscalated = OBLS.exists(o => books.obl.get(o).state == "escalated")
  def witnessDuplicate = lastAction == "duplicateFact"
  def witnessGap = inbox.exists(f => f.lseq > applied.get(f.d) + 1)
  def witnessWithinLeaseApplied = factLog.foldl(false, (a, x) => a or x.f.within)
  def witnessSequencedReturn = factLog.foldl(false, (a, x) => a or x.f.kind == "return")
  temporal safety = always(invariant)

  // LIVENESS: every shortfall slot is eventually resolved or escalated. Under
  // fairness of tick and escalate alone this says the deadline timer fires,
  // not that the shortfall is resolved.
  // This is the conclusion only; its weak-fairness premises (tick, escalate,
  // sync, heal) and the finite fault budget are encoded in
  // liveness/offline-leases-live.qnt (`live`).
  temporal shortfallResolves = OBLS.forall(o => always(books.obl.get(o).state == "open"
    implies eventually(books.obl.get(o).state != "open")))
}
