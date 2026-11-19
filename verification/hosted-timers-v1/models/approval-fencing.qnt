// Approval fencing against definition changes (APPROVAL-REVISION-1, #1464):
// the design that Sol's round-1 review of #1464 asked for (S6 and the
// digest-coverage finding), not the round-1 code.
//
// Source of truth for the shape: crates/norn-api/src/approvals.rs,
// action_admission.rs and cell_definitions.rs as of #1464 head 0d0cbbd. A
// parked call records its action's digest; approving compares it with the
// current definition's digest; a definition change takes the definition
// epoch exclusively.
//
// The design this model fixes (each is a mechanism a mutant removes):
//   F1 the digest comparison and the dispatch (park or write) happen inside
//      one held read of the definition epoch, so no definition change lands
//      between the check and the step it licenses;
//   F2 a refused (stale) call is never driven: the waiting step only runs
//      after a fresh approval of a call parked under the current definition;
//   F3 the digest covers every part of the definition the step's behaviour
//      and authority depend on: the action, its pinned writer and helpers,
//      output-type defaults, relevant law pins and the gate's decider roles.
module approval_fencing {
  type Def = str -> int
  type Mut = { unfencedCheck: bool, driveAfterRefusal: bool, narrowDigest: bool }
  pure val NOMUT: Mut = { unfencedCheck: false, driveAfterRefusal: false, narrowDigest: false }

  pure val CALLS = Set(1, 2)
  pure val PARTS = Set("action", "writer", "defaults", "laws", "gate")
  // What the round-1 digest covered.
  pure val NARROW = Set("action", "writer")
  // A revision either changes one part or leaves every part as it was.
  pure val CHANGES = PARTS.union(Set("unrelated"))
  pure val MAX_REVISIONS = 2
  pure val STATES = Set("none", "parked", "checking", "executed", "refused")

  var active: Def               // the active definition, one version per part
  var revisions: int
  var readers: Set[int]         // calls holding the definition epoch for read
  var state: int -> str
  var parked: int -> Def        // the definition each call parked under
  var matched: int -> bool      // the digest comparison's result
  var ran: int -> Def           // the definition an executed step ran under

  pure def digest(d: Def, m: Mut): Def = (if (m.narrowDigest) NARROW else PARTS).mapBy(p => d.get(p))

  pure val INITIAL: Def = PARTS.mapBy(_ => 0)

  action init = all {
    active' = INITIAL,
    revisions' = 0,
    readers' = Set(),
    state' = CALLS.mapBy(_ => "none"),
    parked' = CALLS.mapBy(_ => INITIAL),
    matched' = CALLS.mapBy(_ => false),
    ran' = CALLS.mapBy(_ => INITIAL),
  }

  // A requester's call parks for approval under the active definition; a
  // refused call's same request parks anew.
  action park(c: int): bool = all {
    state.get(c) == "none" or state.get(c) == "refused",
    state' = state.set(c, "parked"),
    parked' = parked.set(c, active),
    active' = active, revisions' = revisions, readers' = readers, matched' = matched, ran' = ran,
  }

  // A definition change takes the epoch exclusively.
  action change(p: str): bool = all {
    readers == Set(),
    revisions < MAX_REVISIONS,
    active' = if (p == "unrelated") active else active.set(p, active.get(p) + 1),
    revisions' = revisions + 1,
    readers' = readers, state' = state, parked' = parked, matched' = matched, ran' = ran,
  }

  // The approver approves: the digest is compared under the held epoch.
  // Mutant (F1): the read is released as soon as the digest is compared.
  action approve(c: int, m: Mut): bool = all {
    state.get(c) == "parked",
    state' = state.set(c, "checking"),
    matched' = matched.set(c, digest(active, m) == digest(parked.get(c), m)),
    readers' = if (m.unfencedCheck) readers else readers.union(Set(c)),
    active' = active, revisions' = revisions, parked' = parked, ran' = ran,
  }

  // Still in the same epoch: run the step it licensed, or refuse with
  // approval.definition_changed and run nothing.
  action dispatch(c: int): bool = all {
    state.get(c) == "checking",
    state' = state.set(c, if (matched.get(c)) "executed" else "refused"),
    ran' = if (matched.get(c)) ran.set(c, active) else ran,
    readers' = readers.exclude(Set(c)),
    active' = active, revisions' = revisions, parked' = parked, matched' = matched,
  }

  // Mutant only (F2): the server drives the waiting step after refusing it.
  action drive(c: int, m: Mut): bool = all {
    m.driveAfterRefusal,
    state.get(c) == "refused",
    state' = state.set(c, "executed"),
    ran' = ran.set(c, active),
    active' = active, revisions' = revisions, readers' = readers, parked' = parked, matched' = matched,
  }

  action stepWith(m: Mut): bool = {
    nondet c = CALLS.oneOf()
    nondet p = CHANGES.oneOf()
    any { park(c), change(p), approve(c, m), dispatch(c), drive(c, m) }
  }

  action step = stepWith(NOMUT)
  action stepUnfencedCheck = stepWith({ ...NOMUT, unfencedCheck: true })
  action stepDriveAfterRefusal = stepWith({ ...NOMUT, driveAfterRefusal: true })
  action stepNarrowDigest = stepWith({ ...NOMUT, narrowDigest: true })

  def typeOk: bool = and {
    revisions >= 0, revisions <= MAX_REVISIONS,
    readers.subseteq(CALLS),
    CALLS.forall(c => STATES.contains(state.get(c))),
    PARTS.forall(p => active.get(p) >= 0 and active.get(p) <= MAX_REVISIONS),
  }

  // No stale-approved step executes: a step runs only under exactly the
  // definition its call parked (and was approved) under.
  def noStaleApprovedExecution: bool = CALLS.forall(c =>
    state.get(c) == "executed" implies ran.get(c) == parked.get(c))

  def safety: bool = and { typeOk, noStaleApprovedExecution }

  // ---- Non-vacuity witnesses (each must be violated: the state is reached) --

  def witnessExecuted: bool = not(CALLS.exists(c => state.get(c) == "executed"))
  def witnessRefusedAfterChange: bool = not(CALLS.exists(c => state.get(c) == "refused"))
  def witnessUnrelatedRevisionKeepsApproval: bool =
    not(revisions > 0 and CALLS.exists(c => state.get(c) == "executed" and ran.get(c) == INITIAL))
  def witnessReparkedThenExecuted: bool =
    not(CALLS.exists(c => state.get(c) == "executed" and ran.get(c) != INITIAL))
}
