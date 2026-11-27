// SOURCE-CAPTURE-1: durable source-sync lease fencing, atomic complete-page
// capture with durable cursor advancement, idempotent full-page replay, and
// post-capture pending-processing crash recovery (Lane C2, #1923).
//
// Source of truth for the shape:
//   - crates/norn-cell/src/source_cursor.rs (acquire_source_lease,
//     release_source_lease, verify_active_lease, check_capture_precondition,
//     commit_cursor_in_tx, complete_source_pending, pending_source_captures)
//   - crates/norn-cell/src/mapping_page.rs (capture_source_page)
//   - crates/norn-api/src/host/source_capture.rs (capture_source_page_atomic,
//     ingest_source_page, recover_pending_source_pages)
//
// Bounded model: two workers compete on one connection and one dataset over
// three sequential source pages (cursors 0 -> 1 -> 2 -> 3), logical clock
// 0..6, lease TTL 2, and up to two injected crash/store faults per trace.
//
// The design mechanisms this model checks (each removed by one negative mutant):
//   M1 (noFencedWrite): capture verifies active lease (lease_id, generation,
//      now < expires_at) inside the SQLite write transaction.
//   M2 (cursorInboxAtomic): inbox rows, _norn_source_pending, and
//      _norn_source_cursor commit or roll back in one atomic SQLite transaction.
//   M3 (noDoubleCapture): exact full-page duplicate delivery hits
//      IdempotentReplay (via _norn_source_pending or _norn_source_cursor digest)
//      without creating fresh inbox rows; tampered digests conflict and refuse.
//   M4 (pendingRecoversAll): _norn_source_pending stays unprocessed until all
//      captured rows land canonically, so post-commit crashes recover every row.
//   M5 (noFencedWrite / staleRelease): release_source_lease checks
//      (lease_id, generation) so a superseded worker cannot expire a newer lease.
//   M6 (leaseGenerationMonotonic): release sets expires_at = 0 while preserving
//      the lease row, so subsequent acquisitions strictly increment generation.
module pull_cursor_atomicity {
  type Mut = {
    unfencedCapture: bool,
    splitCursorCommit: bool,
    noIdempotentReplay: bool,
    dropPendingBeforeLand: bool,
    staleRelease: bool,
    resetGenOnRelease: bool,
  }

  pure val NOMUT: Mut = {
    unfencedCapture: false,
    splitCursorCommit: false,
    noIdempotentReplay: false,
    dropPendingBeforeLand: false,
    staleRelease: false,
    resetGenOnRelease: false,
  }

  pure val WORKERS = Set(1, 2)
  pure val PAGES = Set(1, 2, 3)
  pure val MAX_TIME = 6
  pure val TTL = 2
  pure val MAX_FAULTS = 2
  pure val NONE = 0

  // Durable SQLite state (_norn_source_lease, _norn_source_cursor,
  // _norn_source_pending, _norn_inbox, canonical table).
  var now: int
  var hasLeaseRow: bool
  var leaseOwner: int             // worker id owning the lease row (0 = none)
  var leaseGen: int               // monotonic fencing generation (0 = never acquired)
  var leaseExpires: int           // exclusive expiry; active iff hasLeaseRow and now < leaseExpires
  var maxGenSeen: int             // ghost: highest generation ever issued

  var cursor: int                 // committed cursor in 0..3 (0 = unset)
  var cursorGen: int              // generation that committed cursor
  var cursorDigest: int           // digest of page that committed cursor (0 = unset)
  var pageDigest: int -> int      // page id -> committed content digest (0 = uncaptured)

  var inboxCount: int -> int      // page id -> number of times captured into _norn_inbox
  var pendingOpen: Set[int]       // page ids in _norn_source_pending with processed = 0
  var pendingDone: Set[int]       // page ids in _norn_source_pending with processed = 1
  var landed: Set[int]            // page ids canonically processed into target tables

  // Worker-local held credentials (may become stale when lease expires or is taken over).
  var workerGen: int -> int       // last generation acquired by worker (0 = none)
  var workerExp: int -> int       // expiry observed by worker

  // Fault budget & history flags for invariants and reachability witnesses.
  var faults: int
  var fencedWriteOccurred: bool
  var rolledBackCapture: bool
  var ledgerFaultOccurred: bool
  var recoveredAfterCrash: bool
  var idempotentReplayCount: int
  var takeoverFencedOld: bool
  var staleAttemptRefused: bool

  pure def leaseActive(hasRow: bool, owner: int, exp: int, t: int): bool =
    hasRow and owner != NONE and t < exp

  action init = all {
    now' = 0,
    hasLeaseRow' = false,
    leaseOwner' = NONE,
    leaseGen' = 0,
    leaseExpires' = 0,
    maxGenSeen' = 0,
    cursor' = 0,
    cursorGen' = 0,
    cursorDigest' = 0,
    pageDigest' = PAGES.mapBy(_ => 0),
    inboxCount' = PAGES.mapBy(_ => 0),
    pendingOpen' = Set(),
    pendingDone' = Set(),
    landed' = Set(),
    workerGen' = WORKERS.mapBy(_ => 0),
    workerExp' = WORKERS.mapBy(_ => 0),
    faults' = 0,
    fencedWriteOccurred' = false,
    rolledBackCapture' = false,
    ledgerFaultOccurred' = false,
    recoveredAfterCrash' = false,
    idempotentReplayCount' = 0,
    takeoverFencedOld' = false,
    staleAttemptRefused' = false,
  }

  // Advance logical clock (may expire an active lease).
  action tick = all {
    now < MAX_TIME,
    now' = now + 1,
    hasLeaseRow' = hasLeaseRow,
    leaseOwner' = leaseOwner,
    leaseGen' = leaseGen,
    leaseExpires' = leaseExpires,
    maxGenSeen' = maxGenSeen,
    cursor' = cursor,
    cursorGen' = cursorGen,
    cursorDigest' = cursorDigest,
    pageDigest' = pageDigest,
    inboxCount' = inboxCount,
    pendingOpen' = pendingOpen,
    pendingDone' = pendingDone,
    landed' = landed,
    workerGen' = workerGen,
    workerExp' = workerExp,
    faults' = faults,
    fencedWriteOccurred' = fencedWriteOccurred,
    rolledBackCapture' = rolledBackCapture,
    ledgerFaultOccurred' = ledgerFaultOccurred,
    recoveredAfterCrash' = recoveredAfterCrash,
    idempotentReplayCount' = idempotentReplayCount,
    takeoverFencedOld' = takeoverFencedOld,
    staleAttemptRefused' = staleAttemptRefused,
  }

  // Acquire or take over an expired lease with strictly monotonic generation.
  action acquireLease(w: int, m: Mut): bool = {
    val activeOther = leaseActive(hasLeaseRow, leaseOwner, leaseExpires, now) and leaseOwner != w
    val nextGen = if (hasLeaseRow) leaseGen + 1 else 1
    val nextExp = now + TTL
    val tookOver = hasLeaseRow and leaseOwner != w and now >= leaseExpires
    all {
      not(activeOther),
      hasLeaseRow' = true,
      leaseOwner' = w,
      leaseGen' = nextGen,
      leaseExpires' = nextExp,
      maxGenSeen' = if (nextGen > maxGenSeen) nextGen else maxGenSeen,
      workerGen' = workerGen.set(w, nextGen),
      workerExp' = workerExp.set(w, nextExp),
      takeoverFencedOld' = (takeoverFencedOld or tookOver),
      now' = now,
      cursor' = cursor,
      cursorGen' = cursorGen,
      cursorDigest' = cursorDigest,
      pageDigest' = pageDigest,
      inboxCount' = inboxCount,
      pendingOpen' = pendingOpen,
      pendingDone' = pendingDone,
      landed' = landed,
      faults' = faults,
      fencedWriteOccurred' = fencedWriteOccurred,
      rolledBackCapture' = rolledBackCapture,
      ledgerFaultOccurred' = ledgerFaultOccurred,
      recoveredAfterCrash' = recoveredAfterCrash,
      idempotentReplayCount' = idempotentReplayCount,
      staleAttemptRefused' = staleAttemptRefused,
    }
  }

  // Release lease; under NOMUT only the current (owner, gen) may expire it,
  // preserving the row so generation stays strictly monotonic.
  action releaseLease(w: int, m: Mut): bool = {
    val g = workerGen.get(w)
    val ownsNow = hasLeaseRow and leaseOwner == w and leaseGen == g
    val allowed = if (m.staleRelease) (hasLeaseRow and g > 0) else ownsNow
    all {
      allowed,
      hasLeaseRow' = if (m.resetGenOnRelease) false else true,
      leaseOwner' = if (m.resetGenOnRelease) NONE else leaseOwner,
      leaseGen' = if (m.resetGenOnRelease) 0 else leaseGen,
      leaseExpires' = 0,
      fencedWriteOccurred' = (fencedWriteOccurred or not(ownsNow)),
      now' = now,
      maxGenSeen' = maxGenSeen,
      cursor' = cursor,
      cursorGen' = cursorGen,
      cursorDigest' = cursorDigest,
      pageDigest' = pageDigest,
      inboxCount' = inboxCount,
      pendingOpen' = pendingOpen,
      pendingDone' = pendingDone,
      landed' = landed,
      workerGen' = workerGen,
      workerExp' = workerExp,
      faults' = faults,
      rolledBackCapture' = rolledBackCapture,
      ledgerFaultOccurred' = ledgerFaultOccurred,
      recoveredAfterCrash' = recoveredAfterCrash,
      idempotentReplayCount' = idempotentReplayCount,
      takeoverFencedOld' = takeoverFencedOld,
      staleAttemptRefused' = staleAttemptRefused,
    }
  }

  // Explicit observation of a fenced capture refusal when a worker with stale
  // or expired credentials attempts to write under NOMUT.
  action refuseFencedCapture(w: int): bool = {
    val g = workerGen.get(w)
    val validLease = hasLeaseRow and leaseOwner == w and leaseGen == g and now < leaseExpires
    all {
      g > 0,
      not(validLease),
      staleAttemptRefused' = true,
      now' = now,
      hasLeaseRow' = hasLeaseRow,
      leaseOwner' = leaseOwner,
      leaseGen' = leaseGen,
      leaseExpires' = leaseExpires,
      maxGenSeen' = maxGenSeen,
      cursor' = cursor,
      cursorGen' = cursorGen,
      cursorDigest' = cursorDigest,
      pageDigest' = pageDigest,
      inboxCount' = inboxCount,
      pendingOpen' = pendingOpen,
      pendingDone' = pendingDone,
      landed' = landed,
      workerGen' = workerGen,
      workerExp' = workerExp,
      faults' = faults,
      fencedWriteOccurred' = fencedWriteOccurred,
      rolledBackCapture' = rolledBackCapture,
      ledgerFaultOccurred' = ledgerFaultOccurred,
      recoveredAfterCrash' = recoveredAfterCrash,
      idempotentReplayCount' = idempotentReplayCount,
      takeoverFencedOld' = takeoverFencedOld,
    }
  }

  // Attempt atomic complete-page capture for page p (advancing expected prev = p - 1 to next = p).
  // `altPayload` selects an alternate valid payload digest (100 + p vs p):
  //   - On a fresh cursor transition (cursor == p - 1), SQLite accepts either payload and binds its digest.
  //   - On a redelivery of an already-committed page p, matching digest hits IdempotentReplay while
  //     mismatched digest conflicts (SourceCursorConflict) and is refused.
  action capturePage(
    w: int,
    p: int,
    altPayload: bool,
    crashBeforeCommit: bool,
    m: Mut
  ): bool = {
    val g = workerGen.get(w)
    val validLease = hasLeaseRow and leaseOwner == w and leaseGen == g and now < leaseExpires
    val passLease = if (m.unfencedCapture) (g > 0) else validLease
    val prevCursor = p - 1
    val digest = if (altPayload) 100 + p else p
    val committedDigest = pageDigest.get(p)
    val isPendingDup = (pendingOpen.contains(p) or pendingDone.contains(p)) and committedDigest == digest
    val isCursorDup = cursor == p and cursorDigest == digest
    val isIdemReplay = not(m.noIdempotentReplay) and (isPendingDup or isCursorDup)
    val canAdvance = cursor == prevCursor
    val canMutantDup = m.noIdempotentReplay and (isPendingDup or isCursorDup)
    val willFault = crashBeforeCommit and faults < MAX_FAULTS
    all {
      passLease,
      (isIdemReplay or canAdvance or canMutantDup),
      if (isIdemReplay) all {
        // Exact full-page duplicate replay: zero new inbox writes, cursor unchanged.
        inboxCount' = inboxCount,
        pendingOpen' = pendingOpen,
        pendingDone' = pendingDone,
        cursor' = cursor,
        cursorGen' = cursorGen,
        cursorDigest' = cursorDigest,
        pageDigest' = pageDigest,
        faults' = faults,
        rolledBackCapture' = rolledBackCapture,
        idempotentReplayCount' = idempotentReplayCount + 1,
        fencedWriteOccurred' = (fencedWriteOccurred or not(validLease)),
      } else if (willFault) all {
        // Pre-commit SQLite failure: NOMUT rolls back everything atomically;
        // splitCursorCommit mutant leaves cursor advanced without inbox/pending!
        inboxCount' = inboxCount,
        pendingOpen' = pendingOpen,
        pendingDone' = pendingDone,
        cursor' = if (m.splitCursorCommit) p else cursor,
        cursorGen' = if (m.splitCursorCommit) g else cursorGen,
        cursorDigest' = if (m.splitCursorCommit) digest else cursorDigest,
        pageDigest' = if (m.splitCursorCommit) pageDigest.set(p, digest) else pageDigest,
        faults' = faults + 1,
        rolledBackCapture' = true,
        idempotentReplayCount' = idempotentReplayCount,
        fencedWriteOccurred' = (fencedWriteOccurred or not(validLease)),
      } else all {
        // Atomic forward capture commit: inbox + pending + cursor in one tx,
        // reaping previously processed pending entries once cursor advances.
        inboxCount' = inboxCount.set(p, inboxCount.get(p) + 1),
        pendingOpen' = pendingOpen.union(Set(p)),
        pendingDone' = Set(),
        cursor' = p,
        cursorGen' = g,
        cursorDigest' = digest,
        pageDigest' = pageDigest.set(p, digest),
        faults' = faults,
        rolledBackCapture' = rolledBackCapture,
        idempotentReplayCount' = idempotentReplayCount,
        fencedWriteOccurred' = (fencedWriteOccurred or not(validLease)),
      },
      now' = now,
      hasLeaseRow' = hasLeaseRow,
      leaseOwner' = leaseOwner,
      leaseGen' = leaseGen,
      leaseExpires' = leaseExpires,
      maxGenSeen' = maxGenSeen,
      landed' = landed,
      workerGen' = workerGen,
      workerExp' = workerExp,
      ledgerFaultOccurred' = ledgerFaultOccurred,
      recoveredAfterCrash' = recoveredAfterCrash,
      takeoverFencedOld' = takeoverFencedOld,
      staleAttemptRefused' = staleAttemptRefused,
    }
  }

  // Post-capture canonical processing (or crash/reopen recovery) of a page in
  // _norn_source_pending (processed = 0). `crashDuringLand` models a ledger
  // fault after cursor commit: under NOMUT pending stays open so recovery lands
  // it later; under M4 (dropPendingBeforeLand) pending is prematurely cleared.
  action processPending(p: int, crashDuringLand: bool, isRecovery: bool, m: Mut): bool = {
    val willFault = crashDuringLand and faults < MAX_FAULTS
    all {
      pendingOpen.contains(p),
      if (willFault) all {
        landed' = landed,
        pendingOpen' = if (m.dropPendingBeforeLand) pendingOpen.exclude(Set(p)) else pendingOpen,
        pendingDone' = if (m.dropPendingBeforeLand) pendingDone.union(Set(p)) else pendingDone,
        faults' = faults + 1,
        ledgerFaultOccurred' = true,
        recoveredAfterCrash' = recoveredAfterCrash,
      } else all {
        landed' = landed.union(Set(p)),
        pendingOpen' = pendingOpen.exclude(Set(p)),
        pendingDone' = pendingDone.union(Set(p)),
        faults' = faults,
        ledgerFaultOccurred' = ledgerFaultOccurred,
        recoveredAfterCrash' = (recoveredAfterCrash or (isRecovery and ledgerFaultOccurred)),
      },
      now' = now,
      hasLeaseRow' = hasLeaseRow,
      leaseOwner' = leaseOwner,
      leaseGen' = leaseGen,
      leaseExpires' = leaseExpires,
      maxGenSeen' = maxGenSeen,
      cursor' = cursor,
      cursorGen' = cursorGen,
      cursorDigest' = cursorDigest,
      pageDigest' = pageDigest,
      inboxCount' = inboxCount,
      workerGen' = workerGen,
      workerExp' = workerExp,
      fencedWriteOccurred' = fencedWriteOccurred,
      rolledBackCapture' = rolledBackCapture,
      idempotentReplayCount' = idempotentReplayCount,
      takeoverFencedOld' = takeoverFencedOld,
      staleAttemptRefused' = staleAttemptRefused,
    }
  }

  action stepWith(m: Mut): bool = {
    nondet w = WORKERS.oneOf()
    nondet p = PAGES.oneOf()
    nondet altPayload = Set(false, true).oneOf()
    nondet crash = Set(false, true).oneOf()
    nondet isRec = Set(false, true).oneOf()
    any {
      tick,
      acquireLease(w, m),
      releaseLease(w, m),
      refuseFencedCapture(w),
      capturePage(w, p, altPayload, crash, m),
      processPending(p, crash, isRec, m),
    }
  }

  action step = stepWith(NOMUT)

  // Negative mutant step actions (each must be refuted by Apalache).
  action stepUnfencedCapture = stepWith({ ...NOMUT, unfencedCapture: true })
  action stepSplitCursorCommit = stepWith({ ...NOMUT, splitCursorCommit: true })
  action stepNoIdempotentReplay = stepWith({ ...NOMUT, noIdempotentReplay: true })
  action stepDropPendingBeforeLand = stepWith({ ...NOMUT, dropPendingBeforeLand: true })
  action stepStaleRelease = stepWith({ ...NOMUT, staleRelease: true })
  action stepResetGenOnRelease = stepWith({ ...NOMUT, resetGenOnRelease: true })

  // ---- Safety & Recovery Invariants ----

  def typeOk = and {
    now >= 0 and now <= MAX_TIME,
    leaseGen >= 0,
    maxGenSeen >= 0,
    cursor >= 0 and cursor <= 3,
    faults >= 0 and faults <= MAX_FAULTS,
    PAGES.forall(p => inboxCount.get(p) >= 0),
  }

  // Lease generation never regresses and stays == maxGenSeen once initialized.
  def leaseGenerationMonotonic =
    hasLeaseRow implies (leaseGen == maxGenSeen and leaseGen >= 1)

  // No expired or stale-generation worker ever mutates cursor, inbox, or lease.
  def noFencedWrite = not(fencedWriteOccurred)

  // Every page up through the committed cursor was durably captured in _norn_inbox,
  // and no page beyond the committed cursor leaked into _norn_inbox.
  def cursorInboxAtomic = PAGES.forall(p =>
    if (p <= cursor) inboxCount.get(p) >= 1 else inboxCount.get(p) == 0
  )

  // Idempotent full-page replay never creates duplicate inbox captures.
  def noDoubleCapture = PAGES.forall(p => inboxCount.get(p) <= 1)

  // Every captured page up through the committed cursor is either already
  // canonically landed or still open in _norn_source_pending for crash recovery.
  def pendingRecoversAll = PAGES.forall(p =>
    (p <= cursor) implies (landed.contains(p) or pendingOpen.contains(p))
  )

  def safety = and {
    typeOk,
    leaseGenerationMonotonic,
    noFencedWrite,
    cursorInboxAtomic,
    noDoubleCapture,
    pendingRecoversAll,
  }

  // ---- Reachability Witnesses (negated invariants; Apalache must violate each) ----

  def witnessAdvancedAndLanded = not(cursor >= 2 and landed.contains(1) and landed.contains(2))
  def witnessCrashBeforeCommitRollsBack = not(rolledBackCapture and cursor >= 1 and landed.contains(1))
  def witnessCrashAfterCommitRecovers = not(ledgerFaultOccurred and recoveredAfterCrash and landed.contains(1))
  def witnessIdempotentReplay = not(idempotentReplayCount >= 1 and cursor >= 1)
  def witnessExpiredLeaseTakeoverFencesOldWorker =
    not(takeoverFencedOld and staleAttemptRefused and leaseGen >= 2 and cursor >= 1)
}
