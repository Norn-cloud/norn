// Approval fencing against definition changes (APPROVAL-REVISION-1) and
// revoked authority, under APPROVAL-RECOVERY-1 (#1483).
//
// Source of truth for the shape: crates/norn-api/src/actions.rs
// (call_action, through_gate, approval_of, effective_digest), approvals.rs
// (hold, call_command_id, require_requester), decisions.rs (decide),
// cell_definitions.rs, and the request's principal: crates/norn-cell-do/src/
// server.rs handle_all (RequestScope, `principals.resolve`), principal.rs
// TokenPrincipals (the roles are the verified token's) and policy.rs
// ReadPolicy (`member`, `action`, `verified_principal` read only that
// request's CallerSlot). A parked call records its action's effective digest
// and the gate revision in its command id. The approver's decide records the
// approval and runs nothing (`call: not_executed`). Only the requester's own
// retry runs the call: under the definition epoch and the cell it holds, it
// derives the command id from the definition active now, consumes the
// approval only when that id is the approved one, and decides the call again
// under the authority of the retry's own verified token. A retry whose id no
// longer matches parks the call anew (a fresh decision; the old approval is
// never consumed); a retry whose token carries no grant is denied, records
// nothing, and leaves the call approved and not executed. A definitions
// install takes the epoch exclusively.
//
// Authority is the request's token, not the issuer's state now. The cell
// holds no membership store: the roles are resolved from the bearer token
// before dispatch (server.rs, principal.rs) and the epoch is taken after
// (actions.rs call_action, call_action_bound). The issuer revokes a grant at
// any time, independently of the epoch, and a token minted before the
// revocation stays valid for its lifetime (at most MAX_LIFETIME_SECONDS, 300 s,
// crates/norn-api/src/principal.rs). So the guarantee is "never under
// authority older than the request's own verified token": revocation reaches
// an approved call within one token lifetime, as it reaches live reads
// (UI-LIVE-1). The witness witnessOutstandingTokenExecutes shows the gap is
// real, not hidden by the model.
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
//   F5 the requester's retry is decided under the authority of its own
//      verified token, never the roles recorded when the call parked;
//   F6 the verified principal lives exactly as long as its request
//      (RequestScope, end_request): a retry never decides on a principal an
//      earlier request left behind.
module approval_fencing {
  type Def = str -> int
  // A verified grant: whether it allows the call, and which token it came
  // from (tokens are numbered as the requester gets them).
  type Auth = { grant: bool, gen: int }
  type Mut = {
    unfencedCheck: bool,
    consumeSuperseded: bool,
    narrowDigest: bool,
    decideExecutes: bool,
    parkedAuthority: bool,
    staleSlot: bool,
  }
  pure val NOMUT: Mut = {
    unfencedCheck: false,
    consumeSuperseded: false,
    narrowDigest: false,
    decideExecutes: false,
    parkedAuthority: false,
    staleSlot: false,
  }

  pure val CALLS = Set(1, 2)
  pure val PARTS = Set("action", "writer", "defaults", "laws", "gate")
  // What the round-1 digest covered.
  pure val NARROW = Set("action", "writer")
  // A revision either changes one part or leaves every part as it was.
  pure val CHANGES = PARTS.union(Set("unrelated"))
  pure val MAX_REVISIONS = 2
  pure val MAX_ISSUER_CHANGES = 2
  pure val MAX_TOKENS = 3
  pure val STATES = Set("none", "parked", "approved", "checking", "executed")

  pure val FIRST_TOKEN: Auth = { grant: true, gen: 0 }

  var active: Def               // the active definition, one version per part
  var revisions: int
  var issuer: bool              // the issuer grants the requester the action now
  var issuerChanges: int
  var token: Auth               // the requester's latest verified token
  var tokenIssues: int
  var slot: Auth                // the principal the last request verified
  var readers: Set[int]         // calls holding the definition epoch for read
  var state: int -> str
  var parked: int -> Def        // the definition the call's current decision parked under
  var parkedAuth: int -> Auth   // the token the call's last park was verified with
  var superseded: int -> Def    // the definition its last superseded approval parked under
  var hasSuperseded: int -> bool
  var matched: int -> bool      // the retry's command comparison
  var requestAuth: int -> Auth  // the retry's own verified token
  var usedAuth: int -> Auth     // the authority the retry was decided under
  var allowed: int -> bool      // the retry's authority decision
  var ran: int -> Def           // the definition an executed step ran under
  var consumed: int -> Def      // the definition the approval it consumed parked under
  var ranAuth: int -> Auth      // the authority it ran under
  var ranRequest: int -> Auth   // the token of the request that ran it
  var issuerAtRun: int -> bool  // the issuer's grant when it ran
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
    issuer' = true,
    issuerChanges' = 0,
    token' = FIRST_TOKEN,
    tokenIssues' = 0,
    slot' = FIRST_TOKEN,
    readers' = Set(),
    state' = CALLS.mapBy(_ => "none"),
    parked' = CALLS.mapBy(_ => INITIAL),
    parkedAuth' = CALLS.mapBy(_ => FIRST_TOKEN),
    superseded' = CALLS.mapBy(_ => INITIAL),
    hasSuperseded' = CALLS.mapBy(_ => false),
    matched' = CALLS.mapBy(_ => false),
    requestAuth' = CALLS.mapBy(_ => FIRST_TOKEN),
    usedAuth' = CALLS.mapBy(_ => FIRST_TOKEN),
    allowed' = CALLS.mapBy(_ => false),
    ran' = CALLS.mapBy(_ => INITIAL),
    consumed' = CALLS.mapBy(_ => INITIAL),
    ranAuth' = CALLS.mapBy(_ => FIRST_TOKEN),
    ranRequest' = CALLS.mapBy(_ => FIRST_TOKEN),
    issuerAtRun' = CALLS.mapBy(_ => true),
    parkedAt' = CALLS.mapBy(_ => 0),
    ranAt' = CALLS.mapBy(_ => 0),
    reparks' = CALLS.mapBy(_ => 0),
    denials' = CALLS.mapBy(_ => 0),
  }

  // The requester's call parks for approval under the active definition. A
  // call its requester's token does not allow is refused as it would be
  // ungated and parks nothing.
  action park(c: int): bool = all {
    state.get(c) == "none",
    token.grant,
    state' = state.set(c, "parked"),
    parked' = parked.set(c, active),
    parkedAuth' = parkedAuth.set(c, token),
    parkedAt' = parkedAt.set(c, revisions),
    slot' = token,
    active' = active, revisions' = revisions, issuer' = issuer, issuerChanges' = issuerChanges,
    token' = token, tokenIssues' = tokenIssues, readers' = readers, superseded' = superseded,
    hasSuperseded' = hasSuperseded, matched' = matched, requestAuth' = requestAuth,
    usedAuth' = usedAuth, allowed' = allowed, ran' = ran, consumed' = consumed, ranAuth' = ranAuth,
    ranRequest' = ranRequest, issuerAtRun' = issuerAtRun, ranAt' = ranAt, reparks' = reparks,
    denials' = denials,
  }

  // A definition change takes the epoch exclusively.
  action change(p: str): bool = all {
    readers == Set(),
    revisions < MAX_REVISIONS,
    active' = if (p == "unrelated") active else active.set(p, active.get(p) + 1),
    revisions' = revisions + 1,
    issuer' = issuer, issuerChanges' = issuerChanges, token' = token, tokenIssues' = tokenIssues,
    slot' = slot, readers' = readers, state' = state, parked' = parked, parkedAuth' = parkedAuth,
    superseded' = superseded, hasSuperseded' = hasSuperseded, matched' = matched,
    requestAuth' = requestAuth, usedAuth' = usedAuth, allowed' = allowed, ran' = ran,
    consumed' = consumed, ranAuth' = ranAuth, ranRequest' = ranRequest, issuerAtRun' = issuerAtRun,
    parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks, denials' = denials,
  }

  // The issuer revokes or restores the requester's grant. It is not the
  // cell's state: it happens at any time, a dispatch's hold included, and
  // reaches the cell only through the next token.
  action issuerChange: bool = all {
    issuerChanges < MAX_ISSUER_CHANGES,
    issuer' = not(issuer),
    issuerChanges' = issuerChanges + 1,
    active' = active, revisions' = revisions, token' = token, tokenIssues' = tokenIssues,
    slot' = slot, readers' = readers, state' = state, parked' = parked, parkedAuth' = parkedAuth,
    superseded' = superseded, hasSuperseded' = hasSuperseded, matched' = matched,
    requestAuth' = requestAuth, usedAuth' = usedAuth, allowed' = allowed, ran' = ran,
    consumed' = consumed, ranAuth' = ranAuth, ranRequest' = ranRequest, issuerAtRun' = issuerAtRun,
    parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks, denials' = denials,
  }

  // The requester gets a new token: the issuer's grant as of now. Its
  // requests carry it until the next one.
  action issueToken: bool = all {
    tokenIssues < MAX_TOKENS,
    token' = { grant: issuer, gen: tokenIssues + 1 },
    tokenIssues' = tokenIssues + 1,
    active' = active, revisions' = revisions, issuer' = issuer, issuerChanges' = issuerChanges,
    slot' = slot, readers' = readers, state' = state, parked' = parked, parkedAuth' = parkedAuth,
    superseded' = superseded, hasSuperseded' = hasSuperseded, matched' = matched,
    requestAuth' = requestAuth, usedAuth' = usedAuth, allowed' = allowed, ran' = ran,
    consumed' = consumed, ranAuth' = ranAuth, ranRequest' = ranRequest, issuerAtRun' = issuerAtRun,
    parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks, denials' = denials,
  }

  // The approver approves: the approval is recorded and nothing runs.
  // Mutant (F4): the decide runs the call itself, as the requester recorded
  // when it parked, inside one fenced step; the requester's own token now is
  // the latest one.
  action approve(c: int, m: Mut): bool = all {
    state.get(c) == "parked",
    val fresh = digest(active, m) == digest(parked.get(c), m)
    val runs = m.decideExecutes and fresh
    all {
      state' = state.set(c, if (runs) "executed" else "approved"),
      ran' = if (runs) ran.set(c, active) else ran,
      consumed' = if (runs) consumed.set(c, parked.get(c)) else consumed,
      ranAuth' = if (runs) ranAuth.set(c, parkedAuth.get(c)) else ranAuth,
      ranRequest' = if (runs) ranRequest.set(c, token) else ranRequest,
      issuerAtRun' = if (runs) issuerAtRun.set(c, issuer) else issuerAtRun,
      ranAt' = if (runs) ranAt.set(c, revisions) else ranAt,
    },
    active' = active, revisions' = revisions, issuer' = issuer, issuerChanges' = issuerChanges,
    token' = token, tokenIssues' = tokenIssues, slot' = slot, readers' = readers,
    parked' = parked, parkedAuth' = parkedAuth, superseded' = superseded,
    hasSuperseded' = hasSuperseded, matched' = matched, requestAuth' = requestAuth,
    usedAuth' = usedAuth, allowed' = allowed, parkedAt' = parkedAt, reparks' = reparks,
    denials' = denials,
  }

  // The requester retries the approved call with its current token: the
  // request verifies the token, then under the held epoch derives the
  // command id from the active definition and decides the call under that
  // token's grant.
  // Mutant (F1): the read is released as soon as the command is compared.
  // Mutant (F5): the authority is the one recorded when the call parked.
  // Mutant (F6): the authority is the principal the last request left.
  action retry(c: int, m: Mut): bool = all {
    state.get(c) == "approved",
    val used = if (m.parkedAuthority) parkedAuth.get(c) else if (m.staleSlot) slot else token
    all {
      usedAuth' = usedAuth.set(c, used),
      allowed' = allowed.set(c, used.grant),
    },
    state' = state.set(c, "checking"),
    matched' = matched.set(c, digest(active, m) == digest(parked.get(c), m)),
    requestAuth' = requestAuth.set(c, token),
    slot' = token,
    readers' = if (m.unfencedCheck) readers else readers.union(Set(c)),
    active' = active, revisions' = revisions, issuer' = issuer, issuerChanges' = issuerChanges,
    token' = token, tokenIssues' = tokenIssues, parked' = parked, parkedAuth' = parkedAuth,
    superseded' = superseded, hasSuperseded' = hasSuperseded, ran' = ran, consumed' = consumed,
    ranAuth' = ranAuth, ranRequest' = ranRequest, issuerAtRun' = issuerAtRun,
    parkedAt' = parkedAt, ranAt' = ranAt, reparks' = reparks, denials' = denials,
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
      ranAuth' = if (runs) ranAuth.set(c, usedAuth.get(c)) else ranAuth,
      ranRequest' = if (runs) ranRequest.set(c, requestAuth.get(c)) else ranRequest,
      issuerAtRun' = if (runs) issuerAtRun.set(c, issuer) else issuerAtRun,
      ranAt' = if (runs) ranAt.set(c, revisions) else ranAt,
      parked' = if (reparked) parked.set(c, active) else parked,
      parkedAuth' = if (reparked) parkedAuth.set(c, requestAuth.get(c)) else parkedAuth,
      parkedAt' = if (reparked) parkedAt.set(c, revisions) else parkedAt,
      superseded' = if (reparked) superseded.set(c, parked.get(c)) else superseded,
      hasSuperseded' = if (reparked) hasSuperseded.set(c, true) else hasSuperseded,
      reparks' = if (reparked) reparks.set(c, reparks.get(c) + 1) else reparks,
      denials' = if (denied) denials.set(c, denials.get(c) + 1) else denials,
    },
    readers' = readers.exclude(Set(c)),
    active' = active, revisions' = revisions, issuer' = issuer, issuerChanges' = issuerChanges,
    token' = token, tokenIssues' = tokenIssues, slot' = slot, matched' = matched,
    requestAuth' = requestAuth, usedAuth' = usedAuth, allowed' = allowed,
  }

  // Mutant only (F2): the requester's retry consumes the superseded approval
  // (found by its idempotency key) after the call parked anew.
  action consumeSuperseded(c: int, m: Mut): bool = all {
    m.consumeSuperseded,
    hasSuperseded.get(c),
    token.grant,
    state.get(c) == "parked" or state.get(c) == "approved",
    state' = state.set(c, "executed"),
    ran' = ran.set(c, active),
    consumed' = consumed.set(c, superseded.get(c)),
    ranAuth' = ranAuth.set(c, token),
    ranRequest' = ranRequest.set(c, token),
    issuerAtRun' = issuerAtRun.set(c, issuer),
    ranAt' = ranAt.set(c, revisions),
    slot' = token,
    active' = active, revisions' = revisions, issuer' = issuer, issuerChanges' = issuerChanges,
    token' = token, tokenIssues' = tokenIssues, readers' = readers, parked' = parked,
    parkedAuth' = parkedAuth, superseded' = superseded, hasSuperseded' = hasSuperseded,
    matched' = matched, requestAuth' = requestAuth, usedAuth' = usedAuth, allowed' = allowed,
    parkedAt' = parkedAt, reparks' = reparks, denials' = denials,
  }

  action stepWith(m: Mut): bool = {
    nondet c = CALLS.oneOf()
    nondet p = CHANGES.oneOf()
    any {
      park(c), change(p), issuerChange, issueToken, approve(c, m), retry(c, m), dispatch(c),
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
      issuerChange,
      issueToken,
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
  action stepStaleSlot = stepWith({ ...NOMUT, staleSlot: true })

  def typeOk: bool = and {
    revisions >= 0, revisions <= MAX_REVISIONS,
    issuerChanges >= 0, issuerChanges <= MAX_ISSUER_CHANGES,
    tokenIssues >= 0, tokenIssues <= MAX_TOKENS, token.gen == tokenIssues,
    readers.subseteq(CALLS),
    CALLS.forall(c => STATES.contains(state.get(c))),
    PARTS.forall(p => active.get(p) >= 0 and active.get(p) <= MAX_REVISIONS),
  }

  // No stale-approved step executes: a step runs only under exactly the
  // definition the approval it consumed was given for, and only under a
  // grant no older than the verified token of the request that ran it (none
  // under a token that carries no grant). Authority is as of the request's
  // own verified token: an issuer revocation that no token carries yet is
  // not seen (witnessOutstandingTokenExecutes).
  def noStaleApprovedExecution: bool = CALLS.forall(c =>
    state.get(c) == "executed" implies and {
      ran.get(c) == consumed.get(c),
      ranAuth.get(c).grant,
      ranAuth.get(c).gen >= ranRequest.get(c).gen,
    })

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
  // A retry under a token without the grant was denied and the call stays
  // approved.
  def witnessDeniedStaysApproved: bool = not(CALLS.exists(c =>
    state.get(c) == "approved" and denials.get(c) >= 1 and not(token.grant)))
  // Denied under a revoked token, restored, and the retry then executed the
  // call.
  def witnessRestoredThenExecuted: bool = not(CALLS.exists(c =>
    state.get(c) == "executed" and denials.get(c) >= 1))
  // The guarantee's edge, reached on purpose: the issuer had revoked the
  // grant when the call ran, under a token verified before the revocation.
  def witnessOutstandingTokenExecutes: bool = not(CALLS.exists(c =>
    state.get(c) == "executed" and not(issuerAtRun.get(c))))
}
