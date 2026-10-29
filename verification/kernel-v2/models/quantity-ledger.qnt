// #3356 v2 decisions: conserving slices, gross/net funding, exact key CAS,
// durable expiry, global committed-time floor. Bounded exact integers, no floats.
module quantity_ledger {
  type Reservation = { status: str, partition: int, out: int, back: int, dest: int }
  type State = { posted: List[int], held: List[int], versions: List[int],
    reservations: List[Reservation], floor: int, commits: int }
  type Request = { kind: str, slot: int, partition: int, out: int, back: int,
    dest: int, now: int, sourceVersion: int, destVersion: int }
  var state: State
  var before: State
  var snapshot: List[int]
  var request: Request
  var outcome: str
  var turn: int
  pure def empty = { status: "missing", partition: 0, out: 0, back: 0, dest: 0 }
  pure def source(p: int, r: int): int = p * 4 + r + 1
  pure def destination(p: int): int = p * 4 + 3
  pure def hold(r: int, outgoing: int, incoming: int): int =
    if (r == 0) outgoing else outgoing - incoming
  pure val initial: State = { posted: [-4, 2, 2, 0, -4, 2, 2, 0, -4, 2, 2, 0, -4, 2, 2, 0],
      held: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
      versions: [1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0],
      reservations: [empty, empty], floor: 0, commits: 0 }
  action init = all {
    state' = initial, before' = initial, snapshot' = initial.versions, turn' = 0,
    request' = { kind: "init", slot: 0, partition: 0, out: 0, back: 0,
      dest: 0, now: 0, sourceVersion: 1, destVersion: 0 }, outcome' = "init",
  }
  action submit(r: Request): bool = {
    val old = state.reservations[r.slot]
    val src = source(r.partition, r.slot)
    val dst = destination(r.partition)
    val reserve = r.kind == "reserve"
    val release = Set("post_slice", "void_slice", "lazy_expire", "scheduled_expire").contains(r.kind)
    val expire = Set("lazy_expire", "scheduled_expire").contains(r.kind)
    val post = Set("post", "post_slice").contains(r.kind)
    val quantity = reserve or release or post
    val oldHold = if (release) hold(r.slot, old.out, old.back) else 0
    val newOut = if (release) old.out - r.out else r.out
    val newBack = if (release) old.back - r.back else r.back
    val newDest = if (release) old.dest - r.dest else r.dest
    val newHold = if (release or reserve) hold(r.slot, newOut, newBack) else 0
    val postedDelta = if (post) r.back - r.out else 0
    val destDelta = if (post) r.dest else 0
    val heldDelta = if (release) newHold - oldHold else if (reserve) newHold else 0
    val required = if (post or reserve) hold(r.slot, r.out, r.back) else 0
    val sourceFence = postedDelta != 0 or heldDelta != 0 or required > 0
    val destFence = destDelta != 0
    val invalidSlice = r.out < 0 or r.back < 0 or r.dest < 0
      or r.out != r.back + r.dest or (release and
        (r.partition != old.partition or r.out > old.out or r.back > old.back or r.dest > old.dest))
      or (not(expire) and quantity and r.out == 0)
    val verdict = if (r.kind == "snapshot") "snapshot"
      else if (r.now < state.floor) "clock"
      else if (release and old.status != "open") "terminal"
      else if (reserve and old.status != "missing") "duplicate"
      else if (quantity and invalidSlice) "invalid"
      else if (reserve and r.now >= 3) "expired"
      else if (release and not(expire) and r.now >= 3) "expired"
      else if (expire and r.now < 3) "not_expired"
      else if ((sourceFence and r.sourceVersion != state.versions[src])
        or (destFence and r.destVersion != state.versions[dst])) "stale"
      else if ((post or reserve) and state.posted[src] - (state.held[src] - oldHold) < required) "funding"
      else if (state.posted[src] + postedDelta < state.held[src] + heldDelta) "funding"
      else "commit"
    val nextReservation = { status: if (newOut != 0 or newBack != 0 or newDest != 0) "open"
        else if (expire) "expired" else if (r.kind == "void_slice") "void" else "posted",
      partition: r.partition, out: newOut, back: newBack, dest: newDest }
    all {
      request' = r, before' = state, outcome' = verdict, turn' = turn + 1,
      snapshot' = if (r.kind == "snapshot") state.versions else snapshot,
      state' = if (verdict != "commit") state else {
        posted: state.posted.replaceAt(src, state.posted[src] + postedDelta)
          .replaceAt(dst, state.posted[dst] + destDelta),
        held: state.held.replaceAt(src, state.held[src] + heldDelta),
        versions: state.versions.replaceAt(src, state.versions[src] +
          (if (postedDelta != 0 or heldDelta != 0) 1 else 0))
          .replaceAt(dst, state.versions[dst] + (if (destDelta != 0) 1 else 0)),
        reservations: if (reserve or release) state.reservations.replaceAt(r.slot, nextReservation)
          else state.reservations,
        floor: r.now, commits: state.commits + 1,
      },
    }
  }
  action step = {
    nondet kind = Set("reserve", "post", "post_slice", "void_slice", "lazy_expire",
      "scheduled_expire", "ordinary", "snapshot").oneOf()
    nondet slot = 0.to(1).oneOf()
    nondet partition = 0.to(3).oneOf()
    nondet outgoing = 1.to(2).oneOf()
    nondet incoming = 0.to(outgoing).oneOf()
    nondet now = 0.to(4).oneOf()
    nondet useSnapshot = Set(false, true).oneOf()
    val old = state.reservations[slot]
    val expire = Set("lazy_expire", "scheduled_expire").contains(kind)
    val p = if (Set("post_slice", "void_slice", "lazy_expire", "scheduled_expire").contains(kind))
      old.partition else partition
    val fences = if (useSnapshot) snapshot else state.versions
    submit({ kind: kind, slot: slot, partition: p,
      out: if (expire) old.out else outgoing,
      back: if (expire) old.back else incoming,
      dest: if (expire) old.dest else outgoing - incoming,
      now: now, sourceVersion: fences[source(p, slot)], destVersion: fences[destination(p)] })
  }
  action guided(kind: str, slot: int, p: int, out: int, back: int, dest: int, now: int, snap: bool): bool = {
    val fences = if (snap) snapshot else state.versions
    submit({ kind: kind, slot: slot, partition: p, out: out, back: back, dest: dest,
      now: now, sourceVersion: fences[source(p, slot)], destVersion: fences[destination(p)] })
  }
  // A model-generated scenario supplements random seeds with rare enabled
  // transitions. It uses the same submit relation and invariant as step.
  action scenario_path(expiry: str): bool = {
    if (turn == 0) guided("snapshot", 0, 0, 0, 0, 0, 0, false)
    else if (turn == 1) guided("reserve", 0, 0, 2, 0, 2, 0, true)
    else if (turn == 2) guided("reserve", 1, 1, 2, 1, 1, 0, true)
    else if (turn == 3) guided("post", 0, 2, 1, 0, 1, 0, true)
    else if (turn == 4) guided("post", 0, 0, 1, 0, 1, 0, true)
    else if (turn == 5) guided("post_slice", 0, 0, 1, 0, 1, 0, false)
    else if (turn == 6) guided("void_slice", 0, 0, 1, 0, 1, 0, false)
    else if (turn == 7) guided("post", 0, 0, 2, 0, 2, 0, false)
    else if (turn == 8) guided("post_slice", 1, 1, 1, 1, 0, 0, false)
    else if (turn == 9) guided("ordinary", 0, 0, 0, 0, 0, 1, false)
    else if (turn == 10) guided("reserve", 0, 0, 1, 0, 1, 1, false)
    else if (turn == 11) guided("ordinary", 0, 0, 0, 0, 0, 0, false)
    else if (turn == 12) guided("post_slice", 1, 1, 1, 0, 1, 3, false)
    else if (turn == 13) guided(expiry, 1, 1, 1, 0, 1, 3, false)
    else guided("snapshot", 0, 0, 0, 0, 0, 3, false)
  }
  action scenario_step = {
    nondet expiry = Set("lazy_expire", "scheduled_expire").oneOf()
    scenario_path(expiry)
  }
  // Smaller bounded instance, reusing submit and the full invariant unchanged.
  // Fixed gross-funded slot/partition/time; terminal post/void and repeated
  // resolution remain nondeterministic. No interleaving or expiry is claimed.
  action conservation_step = {
    nondet resolve = Set("post_slice", "void_slice").oneOf()
    if (turn == 0) guided("reserve", 0, 0, 2, 0, 2, 0, false)
    else if (turn == 1) guided("post_slice", 0, 0, 1, 0, 1, 0, false)
    else guided(resolve, 0, 0, 1, 0, 1, 0, false)
  }
  def conserved = 0.to(3).forall(p => range(p * 4, p * 4 + 4)
    .foldl(0, (a, i) => a + state.posted[i]) == 0)
  def funded = 0.to(15).forall(i => state.held[i] >= 0 and
    (i % 4 == 0 or state.posted[i] >= state.held[i]))
  def holdsAreRemainders = 0.to(15).forall(i => state.held[i] ==
    0.to(1).fold(0, (a, r) => a + (if (state.reservations[r].status == "open"
      and source(state.reservations[r].partition, r) == i)
      hold(r, state.reservations[r].out, state.reservations[r].back) else 0)))
  def partialConservation = 0.to(1).forall(i => { val r = state.reservations[i]
    r.out == r.back + r.dest and r.out >= 0 and r.back >= 0 and r.dest >= 0 })
  def refusalTransparent = if (outcome == "commit") true else state == before
  def monotonicClock = state.floor >= before.floor
  def expiredNeverPosts = if (outcome == "commit" and request.kind == "post_slice")
    request.now < 3 else true
  def keyVersions = 0.to(15).forall(i => state.versions[i] == before.versions[i] +
    (if (state.posted[i] != before.posted[i] or state.held[i] != before.held[i]) 1 else 0))
  def invariant = conserved and funded and holdsAreRemainders and partialConservation
    and refusalTransparent and monotonicClock and expiredNeverPosts and keyVersions
  // These six runs contain no nondeterministic choices. They enumerate every
  // distinct path in the two explicitly restricted finite drivers, checking
  // the original invariant at init and after every semantic transition.
  action checkConservationOutcome(expected: str): bool = all {
    assert(invariant and outcome == expected),
    state' = state, before' = before, snapshot' = snapshot,
    turn' = turn, request' = request, outcome' = outcome,
  }
  run conservationCase(terminalKind: str, retryKind: str): bool = init
    .then(all { assert(invariant), guided("reserve", 0, 0, 2, 0, 2, 0, false) })
    .then(all { assert(invariant and outcome == "commit"),
      guided("post_slice", 0, 0, 1, 0, 1, 0, false) })
    .then(all { assert(invariant and outcome == "commit"),
      guided(terminalKind, 0, 0, 1, 0, 1, 0, false) })
    .then(all { assert(invariant and outcome == "commit"),
      guided(retryKind, 0, 0, 1, 0, 1, 0, false) })
    .then(checkConservationOutcome("terminal"))
  run conservationPostPostTest = conservationCase("post_slice", "post_slice")
  run conservationPostVoidTest = conservationCase("post_slice", "void_slice")
  run conservationVoidPostTest = conservationCase("void_slice", "post_slice")
  run conservationVoidVoidTest = conservationCase("void_slice", "void_slice")
  pure val conservationScenarioOutcomes = ["init", "snapshot", "commit",
    "commit", "commit", "stale", "commit", "commit", "funding", "commit",
    "commit", "duplicate", "clock", "expired", "commit"]
  run conservationScenarioCase(expiry: str): bool = init
    .then(15.reps(i => all {
      assert(invariant and outcome == conservationScenarioOutcomes[i]),
      scenario_path(expiry),
    }))
    .then(checkConservationOutcome("snapshot"))
  run conservationLazyExpiryTest = conservationScenarioCase("lazy_expire")
  run conservationScheduledExpiryTest = conservationScenarioCase("scheduled_expire")
  temporal safety = always(invariant)
  // Explicit environmental assumptions: time reaches the deadline and the host
  // eventually submits an exact fresh Expire for every due open reservation.
  temporal expiryProgress = always((state.floor >= 3 and state.reservations[0].status == "open")
    implies eventually(state.reservations[0].status != "open"))
}
