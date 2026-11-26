// The lifecycle view's model (UI-LIFECYCLE-2): layout from the declaration, counts only from the
// cell's facets answer (`object_facets.rs`), and a counterexample stepped by its witness alone.
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { type ArrowRoute, type Box, type LifecycleDoc, type LifecycleFinding, type LifecycleViewDocument, type LiveAnswer, type LiveSubscriptionRefusal, type Point, countsFrom, createLifecycleReadController, isFacetsAnswer, layout, lifecycleStateRole, orderLifecycleStates, routeArrow, stepState } from "../src/elements/lifecycle-view.ts"
import { LIFECYCLE_CSS } from "../src/elements/lifecycle.css.ts"

const ORDER: LifecycleDoc = {
  property: "status",
  states: ["draft", "submitted", "approved", "rejected", "received", "cancelled", "archived"],
  initial: ["draft"],
  terminal: ["received", "cancelled"],
  endings: [{ state: "approved", outcome: "resting" }],
  transitions: [
    { id: "submit", from: ["draft"], to: "submitted" }, { id: "approve", from: ["submitted"], to: "approved" },
    { id: "reject", from: ["submitted"], to: "rejected" }, { id: "revise", from: ["rejected"], to: "draft" },
    { id: "receive", from: ["approved"], to: "received" }, { id: "cancel", from: ["submitted", "approved"], to: "cancelled" },
  ],
}

test("states are laid out by distance from the start; a state no run reaches goes last", () => {
  expect(layout(ORDER)).toEqual([["draft"], ["submitted"], ["approved", "rejected", "cancelled"], ["received"], ["archived"]])
  expect(orderLifecycleStates(ORDER)).toEqual(["draft", "submitted", "approved", "rejected", "cancelled", "received", "archived"])
  expect(lifecycleStateRole(ORDER, "draft", "en")).toBe("start")
  expect(lifecycleStateRole(ORDER, "received", "en")).toBe("ending")
  expect(lifecycleStateRole(ORDER, "approved", "en")).toBe("can rest here")
  expect(lifecycleStateRole(ORDER, "draft", "ar")).toBe("البداية")
  expect(lifecycleStateRole(ORDER, "received", "ar")).toBe("نهاية")
  expect(lifecycleStateRole(ORDER, "approved", "ar")).toBe("يمكن أن يستقر هنا")
})

test("peer branches follow declared enum order even when transitions are declared in reverse", () => {
  const branching: LifecycleDoc = {
    property: "status",
    states: ["start", "approved", "rejected"],
    initial: ["start"],
    terminal: ["approved", "rejected"],
    transitions: [
      { id: "reject", from: ["start"], to: "rejected" },
      { id: "approve", from: ["start"], to: "approved" },
    ],
  }
  expect(layout(branching)).toEqual([["start"], ["approved", "rejected"]])
  expect(orderLifecycleStates(branching)).toEqual(["start", "approved", "rejected"])
})

test("counts are the cell's: an omitted state is 0 only when otherRows is 0, and lowerBound marks every count", () => {
  const exact = { status: 200, body: { rows: 40, withheldRows: 3, quarantinedRows: 0, lowerBound: false, facets: [{ property: "status", values: [{ value: "draft", count: 30 }, { value: "submitted", count: 10 }], otherRows: 0, nulls: 1 }] } }
  const exactCounts = countsFrom(ORDER, exact)
  expect(exactCounts.states.get("draft")).toEqual({ kind: "count", count: 30, lowerBound: false })
  expect(exactCounts.states.get("cancelled")).toEqual({ kind: "count", count: 0, lowerBound: false })
  expect(exactCounts.notes).toEqual(["3 withheld from you", "1 without a state"])
  expect(exactCounts.refused).toBeNull()

  const bounded = { status: 200, body: { rows: 10_000, withheldRows: 3, quarantinedRows: 2, lowerBound: true, scanLimit: 10_000, facets: [{ property: "status", values: [{ value: "draft", count: 6_200 }, { value: "submitted", count: 3_800 }], otherRows: 0, nulls: 1 }] } }
  const boundedCounts = countsFrom(ORDER, bounded)
  expect(boundedCounts.states.get("draft")).toEqual({ kind: "count", count: 6_200, lowerBound: true })
  expect(boundedCounts.states.get("cancelled")).toEqual({ kind: "count", count: 0, lowerBound: true })
  expect(boundedCounts.notes).toEqual([
    "Scan limit reached (10,000 rows); counts are lower bounds",
    "at least 3 withheld from you",
    "at least 2 held in quarantine",
    "at least 1 without a state",
  ])
})

test("when otherRows > 0, omitted declared states are unlisted rather than counted as 0", () => {
  const truncated = { status: 200, body: { rows: 42, withheldRows: 0, quarantinedRows: 0, lowerBound: false, facets: [{ property: "status", values: [{ value: "draft", count: 30 }, { value: "submitted", count: 10 }], otherRows: 2, nulls: 0 }] } }
  const counts = countsFrom(ORDER, truncated)
  expect(counts.states.get("draft")).toEqual({ kind: "count", count: 30, lowerBound: false })
  expect(counts.states.get("submitted")).toEqual({ kind: "count", count: 10, lowerBound: false })
  expect(counts.states.get("cancelled")).toEqual({ kind: "unlisted" })
  expect(counts.states.get("archived")).toEqual({ kind: "unlisted" })
  expect(counts.notes).toEqual(["2 in unlisted values (omitted states are not counted as 0)"])

  const truncatedAr = countsFrom(ORDER, { ...truncated, body: { ...truncated.body, lowerBound: true } }, "ar")
  expect(truncatedAr.states.get("cancelled")).toEqual({ kind: "unlisted" })
  expect(truncatedAr.notes).toEqual(["2 على الأقل في قيم غير مدرجة (الحالات غير المدرجة لا تُعدّ صفراً)"])
})

test("legacy approximate bodies without lowerBound are rejected as invalid facets documents", () => {
  expect(isFacetsAnswer({ rows: 10, withheldRows: 0, quarantinedRows: 0, approximate: true, facets: [] })).toBe(false)
  expect(isFacetsAnswer({ rows: 10, withheldRows: 0, quarantinedRows: 0, lowerBound: true, facets: [] })).toBe(true)
  const rejected = countsFrom(ORDER, { status: 200, body: { rows: 10, approximate: true, facets: [] } })
  expect(rejected.refused).toBe("The cell did not answer.")
  expect([...rejected.states.values()].every((count) => count.kind === "pending")).toBe(true)
})

test.each([null, { value: "draft", count: "3" }, { count: 3 }])("a malformed facet value (%j) leaves counts pending", (entry) => {
  const body = {
    rows: 3, withheldRows: 0, quarantinedRows: 0, lowerBound: false,
    facets: [{ property: "status", values: [entry], otherRows: 0, nulls: 0 }],
  }
  expect(isFacetsAnswer(body)).toBe(false)
  const rejected = countsFrom(ORDER, { status: 200, body })
  expect(rejected.refused).toBe("The cell did not answer.")
  expect(rejected.states.get("draft")).toEqual({ kind: "pending" })
})

test("before an answer every count is pending; a refusal shows no number at all", () => {
  expect([...countsFrom(ORDER, null).states.values()].every((count) => count.kind === "pending")).toBe(true)
  const refused = countsFrom(ORDER, { status: 403, body: { code: "authz.denied" } }, "ar")
  expect(refused.refused).toBe("لا يمكنك رؤية أعداد هذه السجلات.")
  expect([...refused.states.values()].every((count) => count.kind === "pending")).toBe(true)
  const explained = countsFrom(ORDER, { status: 403, body: { code: "authz.denied", message: "Role lacks facets permission." } }, "en")
  expect(explained.refused).toBe("Role lacks facets permission.")
})

test("LIFECYCLE_CSS follows DESIGN.md weights, calm case, and mobile touch targets", () => {
  expect(LIFECYCLE_CSS).not.toContain("font-weight: 600")
  expect(LIFECYCLE_CSS).not.toContain("text-transform: uppercase")
  expect(LIFECYCLE_CSS).toContain("min-height: 44px")
})

test("a counterexample steps by its witness: taken lifecycle transitions and where the object is", () => {
  const stuck: LifecycleFinding = {
    code: "widget.lifecycle.stuck",
    message: "page state `done` with the object `draft`: no ending of both is reachable",
    witness: { steps: [
      { edge: 0, moves: [{ side: "ui", label: "submit" }] },
      { edge: 3, moves: [{ side: "ui", label: "on-accepted" }, { side: "backend", label: "submit" }] },
      { edge: 9, moves: [{ side: "backend", label: "reject" }] },
      { edge: 14, moves: [{ side: "backend", label: "revise" }] },
    ] },
  }
  expect(stepState(ORDER, stuck, 0)).toEqual({ transitions: new Set(), state: null })
  expect(stepState(ORDER, stuck, 1)).toEqual({ transitions: new Set(), state: null })
  expect(stepState(ORDER, stuck, 2)).toEqual({ transitions: new Set(["submit"]), state: "submitted" })
  expect(stepState(ORDER, stuck, 4)).toEqual({ transitions: new Set(["submit", "reject", "revise"]), state: "draft" })
  expect(stepState(ORDER, { code: "widget.lifecycle.stuck", message: "unattributed", witness: null, unattributed: true }, 2)).toEqual({ transitions: new Set(), state: null })
})

const VIEW_DOC = JSON.parse(readFileSync(join(import.meta.dir, "../src/elements/lifecycle-view.widget.json"), "utf8")) as LifecycleViewDocument
const VIEW_MACHINE = VIEW_DOC.payload.ux.pages[0]!.machine

const facetsAnswer = (draftCount: number, digest?: string): LiveAnswer => ({
  status: 200,
  body: {
    rows: draftCount,
    withheldRows: 0,
    quarantinedRows: 0,
    lowerBound: false,
    facets: [{ property: "status", values: [{ value: "draft", count: draftCount }], otherRows: 0, nulls: 0 }],
  },
  ...(digest ? { digest } : {}),
})

const draftCountOf = (answer: LiveAnswer | null): number | null => {
  if (!answer || !isFacetsAnswer(answer.body)) return null
  return answer.body.facets[0]?.values.find((entry) => entry.value === "draft")?.count ?? null
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => { resolve = r })
  return { promise, resolve }
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 10))

describe("first-snapshot and initial-GET reconciliation", () => {
  test("socket first snapshot (count=2) arriving BEFORE older in-flight GET (count=1) reconciles to 2", async () => {
    const firstGet = deferred<LiveAnswer>()
    const secondGet = deferred<LiveAnswer>()
    const reads: Promise<LiveAnswer>[] = [firstGet.promise, secondGet.promise]
    let readCalls = 0
    let emitSnapshot!: (answer: LiveAnswer) => void
    const seenCounts: (number | null)[] = []

    const controller = createLifecycleReadController({
      machine: VIEW_MACHINE,
      read: () => {
        const next = reads[readCalls++]
        if (!next) throw new Error(`unexpected read #${readCalls}`)
        return next
      },
      changes: (listener) => {
        emitSnapshot = listener
        return () => {}
      },
      onState: (state, last) => {
        if (state === "showing") seenCounts.push(draftCountOf(last))
      },
    })

    // 1. Newer socket snapshot (count=2) arrives while initial GET is still in flight.
    emitSnapshot(facetsAnswer(2, "rev-2"))
    expect(controller.state()).toBe("reading")

    // 2. Older initial GET (count=1) resolves afterward.
    firstGet.resolve(facetsAnswer(1))
    await settle()

    // Controller must NOT settle at stale 1; it issues a reconciliation GET.
    expect(readCalls).toBe(2)
    secondGet.resolve(facetsAnswer(2))
    await settle()

    expect(controller.state()).toBe("showing")
    expect(draftCountOf(controller.lastAnswer())).toBe(2)
    expect(seenCounts).toEqual([2])
    controller.dispose()
  })

  test("older initial GET (count=1) resolving BEFORE newer socket first snapshot (count=2) reconciles to 2", async () => {
    const firstGet = deferred<LiveAnswer>()
    const secondGet = deferred<LiveAnswer>()
    const reads: Promise<LiveAnswer>[] = [firstGet.promise, secondGet.promise]
    let readCalls = 0
    let emitSnapshot!: (answer: LiveAnswer) => void

    const controller = createLifecycleReadController({
      machine: VIEW_MACHINE,
      read: () => {
        const next = reads[readCalls++]
        if (!next) throw new Error(`unexpected read #${readCalls}`)
        return next
      },
      changes: (listener) => {
        emitSnapshot = listener
        return () => {}
      },
      onState: () => {},
    })

    // 1. Initial GET (count=1) resolves first.
    firstGet.resolve(facetsAnswer(1))
    await settle()
    expect(controller.state()).toBe("showing")
    expect(draftCountOf(controller.lastAnswer())).toBe(1)
    expect(readCalls).toBe(1)

    // 2. Newer first socket snapshot (count=2) arrives next.
    emitSnapshot(facetsAnswer(2, "rev-2"))
    await settle()
    expect(readCalls).toBe(2)
    secondGet.resolve(facetsAnswer(2))
    await settle()

    expect(controller.state()).toBe("showing")
    expect(draftCountOf(controller.lastAnswer())).toBe(2)
    controller.dispose()
  })

  test("stale socket first snapshot (count=1) arriving BEFORE newer GET (count=2) never regresses to 1", async () => {
    const firstGet = deferred<LiveAnswer>()
    const secondGet = deferred<LiveAnswer>()
    const reads: Promise<LiveAnswer>[] = [firstGet.promise, secondGet.promise]
    let readCalls = 0
    let emitSnapshot!: (answer: LiveAnswer) => void

    const controller = createLifecycleReadController({
      machine: VIEW_MACHINE,
      read: () => reads[readCalls++]!,
      changes: (listener) => {
        emitSnapshot = listener
        return () => {}
      },
      onState: () => {},
    })

    emitSnapshot(facetsAnswer(1, "rev-1"))
    firstGet.resolve(facetsAnswer(2))
    await settle()
    expect(readCalls).toBe(2)
    secondGet.resolve(facetsAnswer(2))
    await settle()
    expect(draftCountOf(controller.lastAnswer())).toBe(2)
    controller.dispose()
  })

  test("matching initial GET and first socket snapshot issue only one GET in either arrival order", async () => {
    for (const socketFirst of [true, false]) {
      const firstGet = deferred<LiveAnswer>()
      let readCalls = 0
      let emitSnapshot!: (answer: LiveAnswer) => void
      const controller = createLifecycleReadController({
        machine: VIEW_MACHINE,
        read: () => {
          readCalls += 1
          return firstGet.promise
        },
        changes: (listener) => {
          emitSnapshot = listener
          return () => {}
        },
        onState: () => {},
      })
      if (socketFirst) {
        emitSnapshot(facetsAnswer(2, "rev-2"))
        firstGet.resolve(facetsAnswer(2))
      } else {
        firstGet.resolve(facetsAnswer(2))
        await settle()
        emitSnapshot(facetsAnswer(2, "rev-2"))
      }
      await settle()
      expect(readCalls).toBe(1)
      expect(draftCountOf(controller.lastAnswer())).toBe(2)
      controller.dispose()
    }
  })

  test("live refusal surfaces live-updates-off and manual refresh fetches latest counts", async () => {
    let count = 5
    let emitRefused!: (refusal: LiveSubscriptionRefusal) => void
    const refusals: (LiveSubscriptionRefusal | null)[] = []

    const controller = createLifecycleReadController({
      machine: VIEW_MACHINE,
      read: async () => facetsAnswer(count),
      changes: (_listener, onRefused) => {
        emitRefused = onRefused!
        return () => {}
      },
      onState: () => {},
      onLiveRefusal: (refusal) => refusals.push(refusal),
    })

    await settle()
    expect(draftCountOf(controller.lastAnswer())).toBe(5)

    emitRefused({ code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" })
    expect(controller.liveRefusal()).toEqual({ code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" })
    expect(refusals).toEqual([controller.liveRefusal()])

    count = 9
    controller.refresh()
    await settle()
    expect(draftCountOf(controller.lastAnswer())).toBe(9)
    controller.dispose()
  })

  test("the first snapshot after a capacity refusal reconciles even when its digest repeats", async () => {
    let count = 1
    let readCalls = 0
    let emitSnapshot!: (answer: LiveAnswer) => void
    let emitRefused!: (refusal: LiveSubscriptionRefusal) => void
    const controller = createLifecycleReadController({
      machine: VIEW_MACHINE,
      read: async () => {
        readCalls += 1
        return facetsAnswer(count)
      },
      changes: (listener, onRefused) => {
        emitSnapshot = listener
        emitRefused = onRefused!
        return () => {}
      },
      onState: () => {},
    })

    await settle()
    const original = facetsAnswer(1, "rev-1")
    emitSnapshot(original)
    expect(draftCountOf(controller.lastAnswer())).toBe(1)
    expect(readCalls).toBe(1)

    emitRefused({ code: "live.too_many_subscriptions", message: "this socket's subscriptions are full" })
    expect(controller.liveRefusal()?.code).toBe("live.too_many_subscriptions")
    count = 2
    controller.refresh()
    await settle()
    expect(draftCountOf(controller.lastAnswer())).toBe(2)
    expect(readCalls).toBe(2)

    count = 1
    emitSnapshot(original)
    await settle()
    expect(controller.liveRefusal()).toBeNull()
    expect(draftCountOf(controller.lastAnswer())).toBe(1)
    expect(readCalls).toBe(3)
    controller.dispose()
  })
})

// A property over generated lifecycles, in both directions: on the board's grid (11rem cards,
// 4rem between columns) no arrow passes through a card, whichever states a transition joins.
test("no arrow crosses a card, for any lifecycle the layout can produce", () => {
  let seed = 0x9e3779b9
  const random = () => { seed = (seed + 0x6d2b79f5) | 0; let t = Math.imul(seed ^ (seed >>> 15), 1 | seed); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296 }
  const pick = <T>(items: readonly T[]) => items[Math.floor(random() * items.length)]!
  const sample = (route: ArrowRoute): Point[] => {
    const points: Point[] = []
    let from = route.start
    for (const segment of route.segments) {
      const control = [from, ...(segment.via ?? []), segment.to]
      for (let step = 1; step < 40; step += 1) {
        let level = control
        while (level.length > 1) level = level.slice(1).map((point, index) => [level[index]![0] + (point[0] - level[index]![0]) * (step / 40), level[index]![1] + (point[1] - level[index]![1]) * (step / 40)] as const)
        points.push(level[0]!)
      }
      from = segment.to
    }
    return points
  }
  let arrows = 0
  for (let round = 0; round < 300; round += 1) {
    const states = Array.from({ length: 2 + Math.floor(random() * 7) }, (_, index) => `s${index}`)
    const lifecycle: LifecycleDoc = {
      property: "status", states, initial: [states[0]!], terminal: [],
      transitions: Array.from({ length: 1 + Math.floor(random() * 12) }, (_, index) => ({ id: `t${index}`, from: [pick(states), pick(states)], to: pick(states) })),
    }
    const columns = layout(lifecycle)
    const column = new Map(columns.flatMap((col, index) => col.map((state) => [state, index] as const)))
    for (const rtl of [false, true]) {
      const boxes = new Map<string, Box>()
      columns.forEach((col, index) => {
        let top = 20
        for (const state of col) {
          const height = 60 + Math.floor(random() * 40)
          const left = rtl ? 2000 - index * 240 - 176 : 20 + index * 240
          boxes.set(state, { left, right: left + 176, top, bottom: top + height })
          top += height + 12
        }
      })
      const floor = Math.max(...[...boxes.values()].map((box) => box.bottom))
      let lane = 0
      for (const transition of lifecycle.transitions) {
        for (const from of transition.from) {
          const hop = column.get(transition.to)! - column.get(from)!
          expect(hop).toBeLessThanOrEqual(1)
          const route = routeArrow(boxes.get(from)!, boxes.get(transition.to)!, hop, rtl, floor, hop < -1 ? lane++ : 0)
          arrows += 1
          for (const [x, y] of sample(route)) {
            const inside = [...boxes.entries()].find(([, box]) => x > box.left + 0.5 && x < box.right - 0.5 && y > box.top + 0.5 && y < box.bottom - 0.5)
            if (inside) throw new Error(`${from} -> ${transition.to} (hop ${hop}, rtl ${rtl}) passes through ${inside[0]} at ${x},${y}`)
          }
        }
      }
    }
  }
  expect(arrows).toBeGreaterThan(1000)
})

// The view's load states are a declared machine, admitted by the checker like any page's: every
// state can exit (no endless "Counting…"), every outcome of the read is handled.
const pkg = process.env.NORN_CHECKER_WASM_PKG
if (!pkg && process.env.NORN_CHECKER_WASM_REQUIRED === "1") {
  throw new Error("NORN_CHECKER_WASM_PKG is required when NORN_CHECKER_WASM_REQUIRED=1")
}
const withChecker = pkg ? describe : describe.skip
withChecker("the checker", () => {
  test("admits the view's machine: no widget.machine.* finding", async () => {
    const checker = (await import(pathToFileURL(join(pkg!, "norn_checker_wasm.js")).href)) as {
      initSync(options: { module: Uint8Array }): unknown
      checkArtifactSet(artifactsJson: string, contextJson: string): string
    }
    checker.initSync({ module: readFileSync(join(pkg!, "norn_checker_wasm_bg.wasm")) })
    const document = readFileSync(join(import.meta.dir, "../src/elements/lifecycle-view.widget.json"), "utf8")
    const result = JSON.parse(checker.checkArtifactSet(`[${document}]`, "null")) as { diagnostics: { diagnostics: { code: string; message: string }[] } }
    const machine = result.diagnostics.diagnostics.filter((finding) => finding.code.startsWith("widget.machine."))
    expect(machine).toEqual([])
  })
})
