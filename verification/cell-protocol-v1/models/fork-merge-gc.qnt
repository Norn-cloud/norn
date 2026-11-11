// D83 L-model 2: fork, merge by replay and reachability GC over a
// content-addressed page store, with the world pin (D81; Core 08 "Fork
// lifecycle and isolation"; #1226 U1/U2, unit 3 page store, unit 6 merge).
//
// Implicit states this model names:
//   J1 fork pin        (parent, base, horizon): horizon is the main position
//                      where the fork chain left main; base is the parent head
//   J2 shared page     content addressing (HMAC(tenant, content)) dedupes the
//                      same (key, value) across worlds, so one page is shared
//   J3 GC between mark and sweep (gc.phase == "marked"), with its write barrier
//   J4 promoting       a merge in progress, with a durable cursor of commands
//                      already replayed into main; main keeps committing
//   J5 inherited label pre-U2b, a fork writes rows under the inherited "main"
//                      label, so only the pin can tell worlds apart
//   J6 fork-of-fork    receipts below a child's base may be minted by a fork
module fork_merge_gc {
  type Addr = (int, int)                    // content address: (key, value)
  type Row = { addr: Addr, label: int }
  type Cmd = { k: int, delta: int, perform: bool, readVer: int }
  type Receipt = { n: int, minter: int }
  type Pin = { parent: int, base: int, horizon: int }
  type Gc = { phase: str, marked: Set[Addr], barrier: Set[Addr] }
  type Shadow = { vals: int -> int, ver: int -> int, seq: int, ext: int }

  pure val MAIN = 0
  pure val WORLDS = Set(0, 1, 2)
  pure val FORKS = Set(1, 2)
  pure val KEYS = Set(1, 2)
  pure val MAX_VAL = 2
  pure val LIMIT = 3                        // main law: sum of values <= LIMIT
  pure val MAX_FORK_CMDS = 2

  var status: int -> str                    // absent | open | promoting | promoted | discarded | expired
  var pin: int -> Pin
  var root: int -> (int -> Row)
  var seq: int -> int
  var ver: int -> (int -> int)
  var receipts: int -> Set[Receipt]
  var stored: Set[Addr]
  var gc: Gc
  var forkLog: int -> List[Cmd]
  var cursor: int -> int
  var mainLog: List[Cmd]
  var ext: Set[Receipt]                     // external effects: minter = deciding world
  var exported: Set[Receipt]                // records presented as main-world evidence
  var shadow: Shadow                        // main run alone (t \ F)
  var copyVals: int -> (int -> int)         // the copying oracle's view of each world
  var baseRoot: int -> (int -> Addr)
  var lastAction: str

  pure val zeroK = KEYS.mapBy(_ => 0)
  pure val genesisRoot = KEYS.mapBy(k => { addr: (k, 0), label: MAIN })
  action init = all {
    status' = WORLDS.mapBy(w => if (w == MAIN) "open" else "absent"),
    pin' = WORLDS.mapBy(_ => { parent: MAIN, base: 0, horizon: 0 }),
    root' = WORLDS.mapBy(_ => genesisRoot),
    seq' = WORLDS.mapBy(_ => 0),
    ver' = WORLDS.mapBy(_ => zeroK),
    receipts' = WORLDS.mapBy(_ => Set()),
    stored' = KEYS.map(k => (k, 0)),
    gc' = { phase: "idle", marked: Set(), barrier: Set() },
    forkLog' = WORLDS.mapBy(_ => List()),
    cursor' = WORLDS.mapBy(_ => 0),
    mainLog' = List(),
    ext' = Set(), exported' = Set(),
    shadow' = { vals: zeroK, ver: zeroK, seq: 0, ext: 0 },
    copyVals' = WORLDS.mapBy(_ => zeroK),
    baseRoot' = WORLDS.mapBy(_ => KEYS.mapBy(k => (k, 0))),
    lastAction' = "init",
  }

  // Reads go through the page store; a freed page reads as -1.
  def valOf(w: int, k: int): int = {
    val a = root.get(w).get(k).addr
    if (stored.contains(a)) a._2 else -1
  }
  def lawAfter(w: int, k: int, delta: int): bool = {
    val nv = valOf(w, k) + delta
    nv >= 0 and nv <= MAX_VAL and valOf(w, k) >= 0
      and KEYS.fold(0, (a, j) => a + (if (j == k) nv else valOf(w, j))) <= LIMIT
  }
  def live(w: int): bool = Set("open", "promoting").contains(status.get(w))
  def liveAddrs: Set[Addr] = WORLDS.filter(w => live(w)).fold(Set(),
    (s, w) => s.union(KEYS.map(k => root.get(w).get(k).addr)))

  // The page write every commit makes: store (or dedupe) and, while a GC is
  // between mark and sweep, grey the page. `barrierOnHit` false is the
  // variant that greys only freshly allocated pages, not dedupe hits.
  action writePage(a: Addr, barrierOnHit: bool): bool = all {
    stored' = stored.union(Set(a)),
    gc' = if (gc.phase == "marked" and (barrierOnHit or not(stored.contains(a))))
      { ...gc, barrier: gc.barrier.union(Set(a)) } else gc,
  }
  // One admitted commit in world w (main, a fork, or a merge replay into
  // main). Pre-U2b every row is written under the inherited main label (J5).
  // `escape` dispatches a fork's perform: reachable only in the label variant.
  action apply(w: int, c: Cmd, replay: bool, escape: bool, barrierOnHit: bool): bool = {
    val nv = valOf(w, c.k) + c.delta
    val a = (c.k, nv)
    val n = seq.get(w) + 1
    all {
      writePage(a, barrierOnHit),
      root' = root.setBy(w, r => r.set(c.k, { addr: a, label: MAIN })),
      seq' = seq.set(w, n),
      ver' = ver.setBy(w, m => m.setBy(c.k, x => x + 1)),
      receipts' = receipts.setBy(w, s => s.union(Set({ n: n, minter: w }))),
      copyVals' = copyVals.setBy(w, m => m.set(c.k, nv)),
      mainLog' = if (w == MAIN) mainLog.append(c) else mainLog,
      forkLog' = if (w != MAIN and not(replay)) forkLog.setBy(w, l => l.append(c)) else forkLog,
      ext' = if (c.perform and (w == MAIN or escape)) ext.union(Set({ n: n, minter: w })) else ext,
      shadow' = if (w == MAIN) { vals: shadow.vals.setBy(c.k, x => x + c.delta),
        ver: shadow.ver.setBy(c.k, x => x + 1), seq: shadow.seq + 1,
        ext: shadow.ext + (if (c.perform) 1 else 0) } else shadow,
    }
  }

  // WORLD-ISOLATE-1 decided by the pin (L5): perform in a fork refuses before
  // anything is written. `byLabel` is the variant that reads the row label.
  action commit(w: int, k: int, delta: int, perform: bool, byLabel: bool, barrierOnHit: bool): bool = {
    val c = { k: k, delta: delta, perform: perform, readVer: ver.get(w).get(k) }
    val isMain = if (byLabel) root.get(w).get(k).label == MAIN else w == MAIN
    all {
      status.get(w) == "open", lawAfter(w, k, delta),
      w == MAIN or forkLog.get(w).length() < MAX_FORK_CMDS,
      if (perform and not(isMain)) all {
        // core.world.perform_forbidden: nothing written
        status' = status, pin' = pin, root' = root, seq' = seq, ver' = ver,
        receipts' = receipts, stored' = stored, gc' = gc, forkLog' = forkLog,
        cursor' = cursor, mainLog' = mainLog, ext' = ext, exported' = exported,
        shadow' = shadow, copyVals' = copyVals, baseRoot' = baseRoot,
        lastAction' = "refusePerform",
      } else all {
        apply(w, c, false, w != MAIN, barrierOnHit),
        status' = status, pin' = pin, cursor' = cursor, exported' = exported,
        baseRoot' = baseRoot, lastAction' = if (w == MAIN) "commitMain" else "commitFork",
      },
    }
  }

  // FORK-PIN-1: only at the parent's verified head; O(1) root copy.
  action create(parent: int, child: int, base: int): bool = all {
    status.get(child) == "absent", status.get(parent) == "open", child != parent,
    if (base != seq.get(parent)) all {
      // core.world.fork_stale_base: nothing created
      status' = status, pin' = pin, root' = root, seq' = seq, ver' = ver,
      receipts' = receipts, stored' = stored, gc' = gc, forkLog' = forkLog,
      cursor' = cursor, mainLog' = mainLog, ext' = ext, exported' = exported,
      shadow' = shadow, copyVals' = copyVals, baseRoot' = baseRoot,
      lastAction' = "refuseStaleBase",
    } else all {
      status' = status.set(child, "open"),
      pin' = pin.set(child, { parent: parent, base: base,
        horizon: if (parent == MAIN) base else pin.get(parent).horizon }),
      root' = root.set(child, root.get(parent)),
      seq' = seq.set(child, seq.get(parent)),
      ver' = ver.set(child, ver.get(parent)),
      receipts' = receipts.set(child, receipts.get(parent)),
      copyVals' = copyVals.set(child, KEYS.mapBy(k => valOf(parent, k))),
      baseRoot' = baseRoot.set(child, KEYS.mapBy(k => root.get(parent).get(k).addr)),
      stored' = stored, gc' = gc, forkLog' = forkLog, cursor' = cursor,
      mainLog' = mainLog, ext' = ext, exported' = exported, shadow' = shadow,
      lastAction' = "create",
    },
  }

  action close(w: int, how: str): bool = all {
    w != MAIN, status.get(w) == "open",
    status' = status.set(w, how),
    pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
    stored' = stored, gc' = gc, forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
    ext' = ext, exported' = exported, shadow' = shadow, copyVals' = copyVals,
    baseRoot' = baseRoot, lastAction' = how,
  }

  // FORK-MERGE-1: replay each command the fork admitted after its base,
  // against main's current state under main's law. The cursor is durable:
  // a refused merge reopens the fork and a later promote resumes there.
  action promote(w: int): bool = all {
    status.get(w) == "open", pin.get(w).parent == MAIN,
    status' = status.set(w, "promoting"),
    pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
    stored' = stored, gc' = gc, forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
    ext' = ext, exported' = exported, shadow' = shadow, copyVals' = copyVals,
    baseRoot' = baseRoot, lastAction' = "promote",
  }
  action mergeStep(w: int, barrierOnHit: bool): bool = {
    val i = cursor.get(w)
    all {
      status.get(w) == "promoting",
      if (i >= forkLog.get(w).length()) all {
        status' = status.set(w, "promoted"),
        pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
        stored' = stored, gc' = gc, forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
        ext' = ext, exported' = exported, shadow' = shadow, copyVals' = copyVals,
        baseRoot' = baseRoot, lastAction' = "promoted",
      } else {
        val c = forkLog.get(w)[i]
        if (lawAfter(MAIN, c.k, c.delta)) all {
          apply(MAIN, { ...c, readVer: ver.get(MAIN).get(c.k) }, true, false, barrierOnHit),
          cursor' = cursor.set(w, i + 1),
          status' = status, pin' = pin, exported' = exported, baseRoot' = baseRoot,
          lastAction' = "mergeReplay",
        } else all {
          // core.world.merge_refused: main unchanged, the fork reopens
          status' = status.set(w, "open"),
          pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
          stored' = stored, gc' = gc, forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
          ext' = ext, exported' = exported, shadow' = shadow, copyVals' = copyVals,
          baseRoot' = baseRoot, lastAction' = "mergeRefused",
        }
      },
    }
  }

  // Reachability GC in two phases (J3).
  action gcMark = all {
    gc.phase == "idle",
    gc' = { phase: "marked", marked: liveAddrs, barrier: Set() },
    status' = status, pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
    stored' = stored, forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
    ext' = ext, exported' = exported, shadow' = shadow, copyVals' = copyVals,
    baseRoot' = baseRoot, lastAction' = "gcMark",
  }
  action gcSweep = all {
    gc.phase == "marked",
    stored' = stored.intersect(gc.marked.union(gc.barrier)),
    gc' = { phase: "idle", marked: Set(), barrier: Set() },
    status' = status, pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
    forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
    ext' = ext, exported' = exported, shadow' = shadow, copyVals' = copyVals,
    baseRoot' = baseRoot,
    lastAction' = if (stored.intersect(gc.marked.union(gc.barrier)) != stored) "gcSweepFreed" else "gcSweep",
  }

  // FORK-EVIDENCE-1 fence. Correct: a fork exports only records at or below
  // its chain's main horizon. `ownBase` is the fence as #1226 writes it
  // (`commit_seq > pin.base.commits`), which a fork of a fork passes for
  // records its parent fork minted.
  action exportRecord(w: int, r: Receipt, ownBase: bool): bool = {
    val limit = if (ownBase) pin.get(w).base else pin.get(w).horizon
    all {
      live(w), receipts.get(w).contains(r), w == MAIN or r.n <= limit,
      exported' = exported.union(Set(r)),
      status' = status, pin' = pin, root' = root, seq' = seq, ver' = ver, receipts' = receipts,
      stored' = stored, gc' = gc, forkLog' = forkLog, cursor' = cursor, mainLog' = mainLog,
      ext' = ext, shadow' = shadow, copyVals' = copyVals, baseRoot' = baseRoot,
      lastAction' = if (w == MAIN) "exportMain" else "exportFork",
    }
  }

  action stepWith(byLabel: bool, barrierOnHit: bool, ownBase: bool): bool = {
    nondet w = WORLDS.oneOf()
    nondet f = FORKS.oneOf()
    nondet k = KEYS.oneOf()
    nondet delta = Set(-1, 1).oneOf()
    nondet perform = Set(false, true).oneOf()
    nondet stale = Set(false, true).oneOf()
    nondet how = Set("discarded", "expired").oneOf()
    nondet r = receipts.get(w).union(Set({ n: 0, minter: -1 })).oneOf()
    any {
      commit(w, k, delta, perform, byLabel, barrierOnHit),
      create(w, f, if (stale) seq.get(w) - 1 else seq.get(w)),
      close(f, how), promote(f), mergeStep(f, barrierOnHit),
      gcMark, gcSweep, exportRecord(w, r, ownBase),
    }
  }
  action step = stepWith(false, true, false)
  action stepByLabel = stepWith(true, true, false)
  action stepNoDedupeBarrier = stepWith(false, false, false)
  action stepOwnBaseFence = stepWith(false, true, true)

  // --- Invariants (candidate theorem statements) -------------------------
  // FORK-ISOLATE 1, non-interference: main equals main run alone.
  def nonInterference = KEYS.forall(k => valOf(MAIN, k) == shadow.vals.get(k)
    and ver.get(MAIN).get(k) == shadow.ver.get(k)) and seq.get(MAIN) == shadow.seq
    and ext.filter(e => e.minter == MAIN).size() == shadow.ext
  // FORK-ISOLATE 2, no escape: every external effect is decided in main.
  def noEscape = ext.forall(e => e.minter == MAIN)
  // FORK-ISOLATE 3, merge: main is reachable by main alone. Replaying main's
  // log from genesis gives its state, and main's law held after every prefix.
  def mainReplay: { vals: int -> int, ok: bool } = mainLog.foldl({ vals: zeroK, ok: true }, (acc, c) => {
    val nv = acc.vals.setBy(c.k, x => x + c.delta)
    { vals: nv, ok: acc.ok and KEYS.fold(0, (a, k) => a + nv.get(k)) <= LIMIT
      and KEYS.forall(k => nv.get(k) >= 0 and nv.get(k) <= MAX_VAL) }
  })
  def mergeReachable = mainReplay.ok and KEYS.forall(k => shadow.vals.get(k) == mainReplay.vals.get(k))
  // Each fork command is replayed into main at most once.
  def mergeOnce = FORKS.forall(w => cursor.get(w) <= forkLog.get(w).length())
  // L1 holds by construction: an address is its content. L2: a fork's root
  // differs from its base root only at keys the fork wrote.
  def snapshotRoot = FORKS.forall(w => status.get(w) != "absent" implies KEYS.forall(k =>
    root.get(w).get(k).addr == baseRoot.get(w).get(k)
      or forkLog.get(w).foldl(false, (a, c) => a or c.k == k)))
  // L3: GC never frees a page reachable from a live root.
  def gcSafe = liveAddrs.subseteq(stored)
  // L4: every live world reads exactly what its full copy reads.
  def readsMatchCopy = WORLDS.forall(w => live(w) implies KEYS.forall(k => valOf(w, k) == copyVals.get(w).get(k)))
  // L6: what a fork minted is never presented as main evidence.
  def evidenceIsMain = exported.forall(r => r.minter == MAIN)
  // FORK-PIN-1: a fork's horizon is a main position no later than its base.
  def pinWellFormed = FORKS.forall(w => status.get(w) != "absent" implies
    (pin.get(w).horizon <= pin.get(w).base and pin.get(w).horizon <= seq.get(MAIN)))
  def invariant = nonInterference and noEscape and mergeReachable and mergeOnce
    and snapshotRoot and gcSafe and readsMatchCopy and evidenceIsMain and pinWellFormed
  // Non-vacuity witnesses: each must be reachable.
  def witnessPromoted = FORKS.exists(w => status.get(w) == "promoted" and forkLog.get(w).length() > 0)
  def witnessMergeRefused = lastAction == "mergeRefused"
  // A sweep that actually removed a page, after a fork was discarded.
  def witnessGcFreed = lastAction == "gcSweepFreed" and FORKS.exists(w => status.get(w) == "discarded")
  // A fork (not main) passed the export fence.
  def witnessForkExport = lastAction == "exportFork" and exported.exists(r => r.n > 0)
  def witnessForkOfForkCommit = FORKS.exists(w => pin.get(w).parent != MAIN and status.get(w) == "open"
    and forkLog.get(w).length() > 0)
  def witnessRefusedPerform = lastAction == "refusePerform"
  temporal safety = always(invariant)
}
