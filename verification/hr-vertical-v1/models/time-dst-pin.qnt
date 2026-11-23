// Core 01 TIME-PIN-1 pinned IANA tzdb wall-clock -> UTC interval resolution,
// bounded over normal hours, a spring-forward gap, and a fall-back fold under
// `dst_policy` in {reject, earlier, later}, plus tzdb version upgrades on replay.
//
// Under `tzdb = 1` (Cairo-style DST transition model, local hours 0..11):
//   0..2  : normal winter (UTC+2, utc = local - 2)
//   3     : spring-forward gap (nonexistent wall time -> always refused)
//   4..6  : normal summer (UTC+3, utc = local - 3)
//   7     : fall-back fold (earlier UTC+3 -> utc = 4; later UTC+2 -> utc = 5)
//   8..11 : normal winter (UTC+2, utc = local - 2)
// Under `tzdb = 2` (subsequent tzdb rule change where DST is abolished):
//   0..11 : normal standard (UTC+2, utc = local - 2)
//
// `step` enforces TIME-PIN-1 (persisted UTC bounds never change on replay after
// a tzdb upgrade). `reResolvePersistedUnpinned` / `stepUnpinned` explicitly
// demonstrate that re-resolving local wall time under a newer tzdb violates
// `replayPinImmutable`.
// This is a bounded model of TIME-PIN-1, not of the Rust temporal runtime.
module time_dst_pin {
  type Resolution = { ok: bool, utc: int, reason: str }
  type IntervalResult = { ok: bool, startUtc: int, endUtc: int, reason: str }
  type Record = {
    present: bool,
    tzdbVersion: int,
    policy: str,
    startLocal: int,
    endLocal: int,
    startUtc: int,
    endUtc: int,
  }

  var activeTzdb: int
  var records: List[Record]
  var beforeRecords: List[Record]
  var lastTzdb: int
  var lastPolicy: str
  var lastStartLocal: int
  var lastEndLocal: int
  var lastResult: IntervalResult
  var outcome: str

  pure val SLOTS: Set[int] = 0.to(2)
  pure val LOCAL_HOURS: Set[int] = 0.to(11)
  pure val POLICIES: Set[str] = Set("reject", "earlier", "later")
  pure val emptyRecord: Record = {
    present: false,
    tzdbVersion: 1,
    policy: "reject",
    startLocal: 0,
    endLocal: 0,
    startUtc: 0,
    endUtc: 0,
  }
  pure val initResult: IntervalResult = { ok: false, startUtc: 0, endUtc: 0, reason: "init" }

  pure def isGap(tzdb: int, localHour: int): bool =
    tzdb == 1 and localHour == 3

  pure def isFold(tzdb: int, localHour: int): bool =
    tzdb == 1 and localHour == 7

  pure def resolveInstant(tzdb: int, policy: str, localHour: int): Resolution =
    if (isGap(tzdb, localHour))
      { ok: false, utc: 0, reason: "dst_gap" }
    else if (isFold(tzdb, localHour))
      if (policy == "reject")
        { ok: false, utc: 0, reason: "time_ambiguous" }
      else if (policy == "earlier")
        { ok: true, utc: localHour - 3, reason: "ok" }
      else
        { ok: true, utc: localHour - 2, reason: "ok" }
    else if (tzdb == 1 and localHour >= 4 and localHour <= 6)
      { ok: true, utc: localHour - 3, reason: "ok" }
    else
      { ok: true, utc: localHour - 2, reason: "ok" }

  pure def resolveInterval(tzdb: int, policy: str, startLocal: int, endLocal: int): IntervalResult = {
    val s = resolveInstant(tzdb, policy, startLocal)
    val e = resolveInstant(tzdb, policy, endLocal)
    if (not(s.ok))
      { ok: false, startUtc: 0, endUtc: 0, reason: s.reason }
    else if (not(e.ok))
      { ok: false, startUtc: 0, endUtc: 0, reason: e.reason }
    else if (s.utc >= e.utc)
      { ok: false, startUtc: 0, endUtc: 0, reason: "non_half_open" }
    else
      { ok: true, startUtc: s.utc, endUtc: e.utc, reason: "ok" }
  }

  action init = all {
    activeTzdb' = 1,
    records' = [emptyRecord, emptyRecord, emptyRecord],
    beforeRecords' = [emptyRecord, emptyRecord, emptyRecord],
    lastTzdb' = 1,
    lastPolicy' = "reject",
    lastStartLocal' = 0,
    lastEndLocal' = 0,
    lastResult' = initResult,
    outcome' = "init",
  }

  action materialize(slot: int, policy: str, startLocal: int, endLocal: int): bool = {
    val res = resolveInterval(activeTzdb, policy, startLocal, endLocal)
    val verdict = if (records[slot].present) "slot_occupied"
      else if (not(res.ok)) res.reason
      else "commit"
    val nextRec: Record = {
      present: true,
      tzdbVersion: activeTzdb,
      policy: policy,
      startLocal: startLocal,
      endLocal: endLocal,
      startUtc: res.startUtc,
      endUtc: res.endUtc,
    }
    all {
      activeTzdb' = activeTzdb,
      beforeRecords' = records,
      records' = if (verdict == "commit") records.replaceAt(slot, nextRec) else records,
      lastTzdb' = activeTzdb,
      lastPolicy' = policy,
      lastStartLocal' = startLocal,
      lastEndLocal' = endLocal,
      lastResult' = res,
      outcome' = verdict,
    }
  }

  // Upgrading the active IANA tzdb affects only future materializations; already
  // persisted UTC records retain their resolved UTC bounds and pinned tzdbVersion.
  action upgradeTzdb(newVersion: int): bool = {
    val valid = newVersion == 1 or newVersion == 2
    all {
      activeTzdb' = if (valid) newVersion else activeTzdb,
      beforeRecords' = records,
      records' = records,
      lastTzdb' = lastTzdb,
      lastPolicy' = lastPolicy,
      lastStartLocal' = lastStartLocal,
      lastEndLocal' = lastEndLocal,
      lastResult' = lastResult,
      outcome' = if (valid) "tzdb_upgraded" else "invalid_tzdb",
    }
  }

  action step = {
    nondet slot = SLOTS.oneOf()
    nondet policy = POLICIES.oneOf()
    nondet startLocal = LOCAL_HOURS.oneOf()
    nondet endLocal = LOCAL_HOURS.oneOf()
    nondet newVersion = Set(1, 2).oneOf()
    nondet kind = Set("materialize", "upgrade").oneOf()
    if (kind == "materialize")
      materialize(slot, policy, startLocal, endLocal)
    else
      upgradeTzdb(newVersion)
  }

  // Negative refutation action (excluded from guarded `step`): re-resolves an
  // existing committed record's wall-clock times under the current `activeTzdb`
  // instead of replaying its persisted UTC bounds. Refutes `replayPinImmutable`.
  action reResolvePersistedUnpinned(slot: int): bool = {
    val rec = records[slot]
    val recomputed = resolveInterval(activeTzdb, rec.policy, rec.startLocal, rec.endLocal)
    val mutated: Record = {
      present: rec.present,
      tzdbVersion: activeTzdb,
      policy: rec.policy,
      startLocal: rec.startLocal,
      endLocal: rec.endLocal,
      startUtc: recomputed.startUtc,
      endUtc: recomputed.endUtc,
    }
    all {
      activeTzdb' = activeTzdb,
      beforeRecords' = records,
      records' = if (rec.present and recomputed.ok) records.replaceAt(slot, mutated) else records,
      lastTzdb' = activeTzdb,
      lastPolicy' = rec.policy,
      lastStartLocal' = rec.startLocal,
      lastEndLocal' = rec.endLocal,
      lastResult' = recomputed,
      outcome' = "unpinned_replay",
    }
  }

  action stepUnpinned = {
    nondet slot = SLOTS.oneOf()
    any {
      step,
      reResolvePersistedUnpinned(slot),
    }
  }

  def typeOk = all {
    activeTzdb.in(Set(1, 2)),
    records.length() == 3,
    beforeRecords.length() == 3,
    SLOTS.forall(s => records[s].policy.in(POLICIES) and records[s].tzdbVersion.in(Set(1, 2))),
  }

  // TIME-PIN-1: resolution is a pure deterministic function of pinned inputs.
  def deterministicResolution =
    outcome != "init" implies
      (lastResult == resolveInterval(lastTzdb, lastPolicy, lastStartLocal, lastEndLocal))

  // TIME-PIN-1: every admitted UTC interval is strictly half-open [startUtc, endUtc).
  def admittedIntervalHalfOpen =
    SLOTS.forall(s => records[s].present implies records[s].startUtc < records[s].endUtc)

  // TIME-PIN-1: spring-forward gaps are refused fail-closed under every dst_policy.
  def gapAlwaysRefused =
    SLOTS.forall(s =>
      records[s].present implies
        (not(isGap(records[s].tzdbVersion, records[s].startLocal)) and
         not(isGap(records[s].tzdbVersion, records[s].endLocal)))
    )

  // TIME-PIN-1: ambiguous fall-back folds are refused fail-closed under dst_policy = reject.
  def foldUnderRejectAlwaysRefused =
    SLOTS.forall(s =>
      (records[s].present and records[s].policy == "reject") implies
        (not(isFold(records[s].tzdbVersion, records[s].startLocal)) and
         not(isFold(records[s].tzdbVersion, records[s].endLocal)))
    )

  // TIME-PIN-1: once materialized and persisted, a record's pinned UTC bounds never change.
  def replayPinImmutable =
    SLOTS.forall(s => beforeRecords[s].present implies records[s] == beforeRecords[s])
}
