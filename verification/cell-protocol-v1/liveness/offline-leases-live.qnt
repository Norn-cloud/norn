// Liveness harness for offline_leases (D83). Partitions and dropped
// messages get a finite budget; sync, per-device in-order application,
// heal, time and escalation are weakly fair. TLC was run on `live` and did
// not finish within its time budget; the verdict is unknown (results.json).
// `healF` is environmental: a partitioned device is assumed to come back.
module offline_leases_live {
  import offline_leases as L from "../models/offline-leases"

  // A smaller instance than the model's safety bounds: liveness checking
  // walks the whole state graph. Device 1 sells and returns (two records);
  // device 2 only holds and syncs grants. One fault; unit grants, requests,
  // receipts and resolutions; no loss postings.
  pure val MAX_FAULTS = 1
  pure val LIVE_RECORDS = 2
  var faults: int
  action init = all { L::init, faults' = 0 }
  action same = faults' = faults
  action fault = all { faults < MAX_FAULTS, faults' = faults + 1 }

  action sendF(d: int): bool = all { L::send(d), same }
  // Apply the device's next record in order (not a duplicate re-ack).
  action applyNextF(d: int): bool = all {
    L::inbox.exists(f => f.d == d and f.lseq == L::applied.get(d) + 1),
    { nondet f = L::inbox.filter(f => f.d == d and f.lseq == L::applied.get(d) + 1).oneOf()
      all { L::applyFact(f), same } },
  }
  action healF(d: int): bool = all { L::setPartition(d, false), same }
  action ackF(d: int): bool = all {
    L::acks.exists(a => a._1 == d),
    { nondet a = L::acks.filter(a => a._1 == d).oneOf()
      all { L::receiveAck(a), same } },
  }
  action escalateF(o: int): bool = all { L::escalate(o), same }
  action tickF = all { L::tick, same }

  action step = {
    nondet d = L::DEVICES.oneOf()
    nondet q = L::QTY.oneOf()
    nondet o = L::OBLS.oneOf()
    nondet kind = Set("fulfill", "refund", "gain").oneOf()
    any {
      sendF(d), applyNextF(d), healF(d), ackF(d), tickF, escalateF(o),
      // unfair business steps (each bounded by the model's own bounds)
      all { d == 1, L::dev.get(d).nextLseq < LIVE_RECORDS, L::sell(d, q, false), same },
      all { L::deliverGrant(d), same },
      all { d == 1, L::dev.get(d).nextLseq < LIVE_RECORDS, L::returnLease(d, true), same },
      all { L::request(1), same }, all { L::grant(d, 1), same },
      all { L::books.received == 0, L::receive(1), same }, all { L::resolve(o, kind, 1), same },
      all { L::inbox.size() > 0, { nondet f = L::inbox.oneOf() all { L::applyFact(f), same } } },
      // unfair and budgeted faults
      all { L::setPartition(d, true), fault },
      all { L::inbox.size() > 0, { nondet f = L::inbox.oneOf() all { L::dropFact(f), fault } } },
      all { L::acks.size() > 0, { nondet a = L::acks.oneOf() all { L::dropAck(a), fault } } },
    }
  }

  temporal fairness = and {
    weakFair(tickF, (L::books, L::applied, L::cellLog, L::factLog, L::dev, L::partitioned, L::inbox, L::acks, L::now, L::lastAction, faults)),
    L::DEVICES.forall(d => weakFair(sendF(d), (L::books, L::applied, L::cellLog, L::factLog, L::dev, L::partitioned, L::inbox, L::acks, L::now, L::lastAction, faults))),
    L::DEVICES.forall(d => weakFair(applyNextF(d), (L::books, L::applied, L::cellLog, L::factLog, L::dev, L::partitioned, L::inbox, L::acks, L::now, L::lastAction, faults))),
    L::DEVICES.forall(d => weakFair(healF(d), (L::books, L::applied, L::cellLog, L::factLog, L::dev, L::partitioned, L::inbox, L::acks, L::now, L::lastAction, faults))),
    L::DEVICES.forall(d => weakFair(ackF(d), (L::books, L::applied, L::cellLog, L::factLog, L::dev, L::partitioned, L::inbox, L::acks, L::now, L::lastAction, faults))),
    L::OBLS.forall(o => weakFair(escalateF(o), (L::books, L::applied, L::cellLog, L::factLog, L::dev, L::partitioned, L::inbox, L::acks, L::now, L::lastAction, faults))),
  }
  // Every shortfall is eventually resolved or escalated.
  temporal shortfallResolves = L::OBLS.forall(o =>
    always(L::books.obl.get(o).state == "open" implies eventually(L::books.obl.get(o).state != "open")))
  // A fact is never refused: every recorded stream record is eventually applied.
  temporal factApplied = L::DEVICES.forall(d => 1.to(LIVE_RECORDS).forall(l =>
    always(L::dev.get(d).nextLseq >= l implies eventually(L::applied.get(d) >= l))))
  temporal live = fairness implies (shortfallResolves and factApplied)
  def invariant = L::invariant
}
