// Approval fencing against definition changes (APPROVAL-REVISION-1) and
// revoked authority, under APPROVAL-RECOVERY-1 (#1483).
//
// Source of truth for the shape: crates/norn-api/src/actions.rs
// (through_gate, approval_of, effective_digest), approvals.rs (hold,
// call_command_id, require_requester), decisions.rs (decide) and
// cell_definitions.rs. A parked call records its action's effective digest
// and the gate revision in its command id. The approver's decide records the
// approval and runs nothing (`call: not_executed`). Only the requester's own
// retry runs the call: under the definition epoch and the cell it holds, it
// derives the command id from the definition active now, consumes the
// approval only when that id is the approved one, and decides the call again
// under the requester's current authority. A retry whose id no longer
// matches parks the call anew (a fresh decision; the old approval is never
// consumed); a retry whose authority was revoked is denied, records
// nothing, and leaves the call approved and not executed. A definitions
// install takes the epoch exclusively.
//
// A supporting check of the hosted approval path. It is not evidence for
// INV-AUTH-STALE-REJECT (the owner-version compare-and-swap).
//
// The design this model fixes (each is a mechanism a mutant removes):
//   F1 the command comparison and the commit happen inside one held read of
//      the definition epoch, so no definition change lands between the
//      check and the step it licenses;
//   F2 an approval whose command no longer matches is never consumed: the
//      call parks anew and only a fresh approval of it runs;
//   F3 the digest covers every part of the definition the step's behaviour
//      and authority depend on: the action, its pinned writer and helpers,
//      output-type defaults, relevant law pins and the gate's decider roles;
//   F4 the approver's decide records the approval only and never runs the
//      call (APPROVAL-RECOVERY-1);
//   F5 the requester's retry is decided under the requester's authority as
//      verified now, never the roles recorded when the call parked.
module approval_fencing {
  type Def = str -> int
  type Mut = {
    unfencedCheck: bool,
    consumeSuperseded: bool,
    narrowDigest: bool,
    decideExecutes: bool,
    parkedAuthority: bool,
  }
  pure val NOMUT: Mut = {
    unfencedCheck: false,
    consumeSuperseded: false,
    narrowDigest: false,
    decideExecutes: false,
    parkedAuthority: false,
  }

  pure val CALLS = Set(1, 2)
  pure val PARTS = Set("action", "writer", "defaults", "laws", "gate")
  // What the round-1 digest covered.
  pure val NARROW = Set("action", "writer")
  // A revision either changes one part or leaves every part as it was.
  pure val CHANGES = PARTS.union(Set("unrelated"))
  pure val MAX_REVISIONS = 2
  pure val MAX_AUTHORITY_CHANGES = 2
  pure val STATES = Set("none", "parked", "approved", "checking", "executed")

  var active: Def               // the active definition, one version per part
  var revisions: int
  var authorized: bool          // the requester holds the action's grant now
  var authorityChanges: int
  var readers: Set[int]         // calls holding the definition epoch for read
  var state: int -> str
  var parked: int -> Def        // the definition the call's current decision parked under
  var superseded: int -> Def    // the definition its last superseded approval parked under
  var hasSuperseded: int -> bool
  var matched: int -> bool      // the retry's command comparison
  var allowed: int -> bool      // the retry's authority decision
  var ran: int -> Def           // the definition an executed step ran under
  var consumed: int -> Def      // the definition the approval it consumed parked under
  var ranAuthorized: int -> bool // the requester's authority when it ran
  // History, for the witnesses.
  var parkedAt: int -> int      // revisions when the call last parked
  var ranAt: int -> int         // revisions when it executed
  var reparks: int -> int       // how often a retry parked it anew
  var denials: int -> int       // how often a retry was denied

  pure def digest(d: Def, m: Mut): Def = (if (m.narrowDigest) NARROW else PARTS).mapBy(p => d.get(p))

  pure val INITIAL: Def = PARTS.mapBy(_ => 0)

  action init = all {
    active' = INITIAL,
    revisions' = 0,
    authorized' = true,
    authorityChanges' = 0,
    readers' = Set(),
    state' = CALLS.mapBy(_ => "none"),
    parked' = CALLS.mapBy(_ => INITIAL),
    superseded' = CALLS.mapBy(_ => INITIAL),
    hasSuperseded' = CALLS.mapBy(_ => false),
    matched' = CALLS.mapBy(_ => false),
    allowed' = CALLS.mapBy(_ => false),
    ran' = CALLS.mapBy(_ => INITIAL),
    consumed' = CALLS.mapBy(_ => INITIAL),
    ranAuthorized' = CALLS.mapBy(_ => false),
    parkedAt' = CALLS.mapBy(_ => 0),
    ranAt' = CALLS.mapBy(_ => 0),
    reparks' = CALLS.mapBy(_ => 0),
    denials' = CALLS.mapBy(_ => 0),
  }

  // The requester's call parks for approval under the active definition. A
  // call its requester may not make is refused as it would be ungated and
  // parks nothing.
  action park(c: int): bool = all {
    state.get(c) == "none",
    authorized,
    state' = state.set(c, "parked"),
    parked' = parked.set(c, active),
    parkedAt' = parkedAt.set(c, revisions),
    active' = active, revisions' = revisions, authorized' = authorized,
    authorityChanges' = authorityChanges, readers' = readers, superseded' = superseded,
    hasSuperseded' = hasSuperseded, matched' = matched, allowed' = allowed, ran' = ran,
    consumed' = consumed, ranAuthorized' = ranAuthorized, ranAt' = ranAt, reparks' = reparks,
    denials' = denials,
  }

  // A definition change takes the epoch exclusively.
  action change(p: str): bool = all {
    readers == Set(),
    revisions < MAX_REVISIONS,
    active' = if (p == "unrelated") active else active.set(p, active.get(p) + 1),
    revisions' = revisions + 1,
    authorized' = authorized, authorityChanges' = authorityChanges, readers' = readers,
    state' = state, parked' = parked, superseded' = superseded, hasSuperseded' = hasSuperseded,
    matched' = matched, allowed' = allowed, ran' = ran, consumed' = consumed,
    ranAuthorized' = ranAuthorized, parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks,
    denials' = denials,
  }

  // The requester's grant is revoked or restored. A role change commits in
  // the cell, never inside a dispatch's hold.
  action reauthorize: bool = all {
    readers == Set(),
    authorityChanges < MAX_AUTHORITY_CHANGES,
    authorized' = not(authorized),
    authorityChanges' = authorityChanges + 1,
    active' = active, revisions' = revisions, readers' = readers, state' = state,
    parked' = parked, superseded' = superseded, hasSuperseded' = hasSuperseded,
    matched' = matched, allowed' = allowed, ran' = ran, consumed' = consumed,
    ranAuthorized' = ranAuthorized, parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks,
    denials' = denials,
  }

  // The approver approves: the approval is recorded and nothing runs.
  // Mutant (F4): the decide runs the call itself, as the requester recorded
  // when it parked, inside one fenced step.
  action approve(c: int, m: Mut): bool = all {
    state.get(c) == "parked",
    val fresh = digest(active, m) == digest(parked.get(c), m)
    val runs = m.decideExecutes and fresh
    all {
      state' = state.set(c, if (runs) "executed" else "approved"),
      ran' = if (runs) ran.set(c, active) else ran,
      consumed' = if (runs) consumed.set(c, parked.get(c)) else consumed,
      ranAuthorized' = if (runs) ranAuthorized.set(c, authorized) else ranAuthorized,
      ranAt' = if (runs) ranAt.set(c, revisions) else ranAt,
    },
    active' = active, revisions' = revisions, authorized' = authorized,
    authorityChanges' = authorityChanges, readers' = readers, parked' = parked,
    superseded' = superseded, hasSuperseded' = hasSuperseded, matched' = matched,
    allowed' = allowed, parkedAt' = parkedAt, reparks' = reparks, denials' = denials,
  }

  // The requester retries the approved call: under the held epoch it derives
  // the command id from the active definition and decides its authority now.
  // Mutant (F1): the read is released as soon as the command is compared.
  // Mutant (F5): the authority is the one recorded when the call parked.
  action retry(c: int, m: Mut): bool = all {
    state.get(c) == "approved",
    state' = state.set(c, "checking"),
    matched' = matched.set(c, digest(active, m) == digest(parked.get(c), m)),
    allowed' = allowed.set(c, m.parkedAuthority or authorized),
    readers' = if (m.unfencedCheck) readers else readers.union(Set(c)),
    active' = active, revisions' = revisions, authorized' = authorized,
    authorityChanges' = authorityChanges, parked' = parked, superseded' = superseded,
    hasSuperseded' = hasSuperseded, ran' = ran, consumed' = consumed,
    ranAuthorized' = ranAuthorized, parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks,
    denials' = denials,
  }

  // Still in the same epoch: a denied retry records nothing and leaves the
  // call approved; a matching one commits, consuming the approval; any other
  // parks the call anew under the active definition (a fresh decision).
  action dispatch(c: int): bool = all {
    state.get(c) == "checking",
    val denied = not(allowed.get(c))
    val runs = allowed.get(c) and matched.get(c)
    val reparked = allowed.get(c) and not(matched.get(c))
    all {
      state' = state.set(c, if (denied) "approved" else if (runs) "executed" else "parked"),
      ran' = if (runs) ran.set(c, active) else ran,
      consumed' = if (runs) consumed.set(c, parked.get(c)) else consumed,
      ranAuthorized' = if (runs) ranAuthorized.set(c, authorized) else ranAuthorized,
      ranAt' = if (runs) ranAt.set(c, revisions) else ranAt,
      parked' = if (reparked) parked.set(c, active) else parked,
      parkedAt' = if (reparked) parkedAt.set(c, revisions) else parkedAt,
      superseded' = if (reparked) superseded.set(c, parked.get(c)) else superseded,
      hasSuperseded' = if (reparked) hasSuperseded.set(c, true) else hasSuperseded,
      reparks' = if (reparked) reparks.set(c, reparks.get(c) + 1) else reparks,
      denials' = if (denied) denials.set(c, denials.get(c) + 1) else denials,
    },
    readers' = readers.exclude(Set(c)),
    active' = active, revisions' = revisions, authorized' = authorized,
    authorityChanges' = authorityChanges, matched' = matched, allowed' = allowed,
  }

  // Mutant only (F2): the requester's retry consumes the superseded approval
  // (found by its idempotency key) after the call parked anew.
  action consumeSuperseded(c: int, m: Mut): bool = all {
    m.consumeSuperseded,
    hasSuperseded.get(c),
    authorized,
    state.get(c) == "parked" or state.get(c) == "approved",
    state' = state.set(c, "executed"),
    ran' = ran.set(c, active),
    consumed' = consumed.set(c, superseded.get(c)),
    ranAuthorized' = ranAuthorized.set(c, authorized),
    ranAt' = ranAt.set(c, revisions),
    active' = active, revisions' = revisions, authorized' = authorized,
    authorityChanges' = authorityChanges, readers' = readers, parked' = parked,
    superseded' = superseded, hasSuperseded' = hasSuperseded, matched' = matched,
    allowed' = allowed, parkedAt' = parkedAt, reparks' = reparks, denials' = denials,
  }

  action stepWith(m: Mut): bool = {
    nondet c = CALLS.oneOf()
    nondet p = CHANGES.oneOf()
    any {
      park(c), change(p), reauthorize, approve(c, m), retry(c, m), dispatch(c),
      consumeSuperseded(c, m),
    }
  }

  action step = stepWith(NOMUT)
  // Conformance traces (crates/norn-cell-do/tests/approval_fencing_quint.rs):
  // the fixture has two versions of the action's writer, so the action
  // changes at most once; any other revision reinstalls it unchanged.
  action stepConform = {
    nondet c = CALLS.oneOf()
    nondet p = Set("action", "unrelated").oneOf()
    any {
      park(c),
      all { p == "unrelated" or active.get("action") == 0, change(p) },
      reauthorize,
      approve(c, NOMUT),
      retry(c, NOMUT),
      dispatch(c),
    }
  }
  action stepUnfencedCheck = stepWith({ ...NOMUT, unfencedCheck: true })
  action stepConsumeSuperseded = stepWith({ ...NOMUT, consumeSuperseded: true })
  action stepNarrowDigest = stepWith({ ...NOMUT, narrowDigest: true })
  action stepDecideExecutes = stepWith({ ...NOMUT, decideExecutes: true })
  action stepParkedAuthority = stepWith({ ...NOMUT, parkedAuthority: true })

  def typeOk: bool = and {
    revisions >= 0, revisions <= MAX_REVISIONS,
    authorityChanges >= 0, authorityChanges <= MAX_AUTHORITY_CHANGES,
    readers.subseteq(CALLS),
    CALLS.forall(c => STATES.contains(state.get(c))),
    PARTS.forall(p => active.get(p) >= 0 and active.get(p) <= MAX_REVISIONS),
  }

  // No stale-approved step executes: a step runs only under exactly the
  // definition the approval it consumed was given for, and only while its
  // requester holds the authority to make the call (none under revoked
  // authority).
  def noStaleApprovedExecution: bool = CALLS.forall(c =>
    state.get(c) == "executed" implies (ran.get(c) == consumed.get(c) and ranAuthorized.get(c)))

  def safety: bool = and { typeOk, noStaleApprovedExecution }

  // ---- Non-vacuity witnesses (each must be violated: the state is reached) --

  def witnessExecuted: bool = not(CALLS.exists(c => state.get(c) == "executed"))
  // A definition change made an approval stale: the retry parked the call anew.
  def witnessReparkedAfterChange: bool = not(CALLS.exists(c => reparks.get(c) >= 1))
  // A revision that kept every part landed between the park and the
  // execution, and the approval still executed the call.
  def witnessUnrelatedRevisionKeepsApproval: bool = not(CALLS.exists(c =>
    state.get(c) == "executed" and ranAt.get(c) > parkedAt.get(c) and ran.get(c) == parked.get(c)))
  // Parked, made stale by a change, parked anew, approved and executed.
  def witnessReparkedThenExecuted: bool = not(CALLS.exists(c =>
    state.get(c) == "executed" and reparks.get(c) >= 1 and parkedAt.get(c) >= 1))
  // A revoked requester's retry was denied and the call stays approved.
  def witnessDeniedStaysApproved: bool = not(CALLS.exists(c =>
    state.get(c) == "approved" and denials.get(c) >= 1 and not(authorized)))
  // Denied while revoked, restored, and the retry then executed the call.
  def witnessRestoredThenExecuted: bool = not(CALLS.exists(c =>
    state.get(c) == "executed" and denials.get(c) >= 1))
}
