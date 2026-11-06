import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { EditorToolbar, RowActions, ValidationPanel, shortcutOf } from "../src/components/editor.tsx"
import { VERBS, history, list, modKey, sameValue, verbTable } from "../src/lib/editor.ts"
import { SUITES } from "./e2e/suites.ts"
import { defineVerbs, exitStatus, supported } from "./e2e/verbs.ts"

test("history undoes and redoes whole states, forgets the redo branch on a new edit, and keeps at most 100 steps", () => {
  let h = history.start(0)
  for (const n of [1, 2, 3]) h = history.set(h, n)
  h = history.undo(history.undo(h))
  expect(h).toEqual({ past: [0], present: 1, future: [2, 3] })
  expect(history.redo(h).present).toBe(2)
  h = history.set(h, 9)
  expect(h.future).toEqual([])
  expect(history.set(h, 9)).toBe(h)
  expect(history.undo(history.start(5))).toEqual(history.start(5))
  for (let n = 0; n < 150; n++) h = history.set(h, n + 10)
  expect(h.past.length).toBe(100)
})

test("list verbs duplicate as a separate copy, move within bounds and remove", () => {
  const rows = [{ a: 1 }, { a: 2 }, { a: 3 }]
  const copied = list.duplicate(rows, 0)
  expect(copied).toEqual([{ a: 1 }, { a: 1 }, { a: 2 }, { a: 3 }])
  expect(copied[1]).not.toBe(rows[0])
  expect(list.move(rows, 2, 0)).toEqual([{ a: 3 }, { a: 1 }, { a: 2 }])
  expect(list.move(rows, 0, 3)).toBe(rows)
  expect(list.remove(rows, 1)).toEqual([{ a: 1 }, { a: 3 }])
  expect(list.duplicate(rows, 7)).toBe(rows)
})

const matrix = { create: "supported", view: "supported", edit: "supported", delete: "supported", duplicate: "supported", reorder: { na: "a single value has no order" }, undo: "supported", redo: "supported", validate: "supported", save: "supported", reopen: "supported", run: { na: "nothing to run" } } as const

test("the verb matrix names every verb, and its table reads ✓ or N/A with the reason", () => {
  const table = verbTable({ Thing: matrix })
  expect(table.split("\n")[0]).toBe(`| Object | ${VERBS.join(" | ")} |`)
  expect(table).toContain("| Thing | ✓ | ✓ | ✓ | ✓ | ✓ | N/A: a single value has no order | ✓ | ✓ | ✓ | ✓ | ✓ | N/A: nothing to run |")
})

test("a verb suite must have a test for exactly the supported verbs (checked by the type, and here at run time)", () => {
  const noop = async () => {}
  const suite = defineVerbs("Thing", matrix, { create: noop, view: noop, edit: noop, delete: noop, duplicate: noop, undo: noop, redo: noop, validate: noop, save: noop, reopen: noop })
  expect(supported(suite)).toEqual(["create", "view", "edit", "delete", "duplicate", "undo", "redo", "validate", "save", "reopen"])
  // @ts-expect-error a supported verb without its end-to-end test does not compile
  defineVerbs("Thing", matrix, { create: noop })
  // @ts-expect-error neither does a test for a verb the matrix marks N/A
  defineVerbs("Thing", matrix, { create: noop, view: noop, edit: noop, delete: noop, duplicate: noop, undo: noop, redo: noop, validate: noop, save: noop, reopen: noop, run: noop })
  for (const registered of SUITES) expect(supported(registered).filter((verb) => typeof registered.specs[verb] !== "function")).toEqual([])
})

test("the toolbar shows undo, redo and save state; rows offer move, duplicate and remove within bounds", () => {
  const editor = { undo: () => {}, redo: () => {}, canUndo: false, canRedo: true, dirty: true }
  const toolbar = renderToStaticMarkup(<EditorToolbar editor={editor} onSave={() => {}} />)
  expect(toolbar).toMatch(/aria-label="Undo"[^>]*disabled/)
  expect(toolbar).not.toMatch(/aria-label="Redo"[^>]*disabled/)
  expect(toolbar).toContain("Unsaved changes")
  expect(renderToStaticMarkup(<EditorToolbar editor={{ ...editor, dirty: false }} onSave={() => {}} />)).toContain("All changes saved")
  const first = renderToStaticMarkup(<RowActions index={0} count={2} label="condition 1" onDuplicate={() => {}} onMove={() => {}} onRemove={() => {}} />)
  expect(first).toMatch(/aria-label="Move condition 1 up"[^>]*disabled/)
  expect(first).not.toMatch(/aria-label="Move condition 1 down"[^>]*disabled/)
  for (const verb of ["Duplicate", "Remove"]) expect(first).toContain(`aria-label="${verb} condition 1"`)
  expect(modKey("MacIntel")).toBe("⌘")
  expect(modKey("Linux x86_64")).toBe("Ctrl+")
})

test("the validation panel links each placed finding to its field and states the rest", () => {
  const html = renderToStaticMarkup(<ValidationPanel issues={[{ code: "a", message: "No metric demo.purity", field: "measure" }, { code: "b", message: "The engine evaluates weekly questions only", field: null }]} />)
  expect(html).toContain('role="alert"')
  expect(html).toMatch(/<button[^>]*>No metric demo.purity<\/button>/)
  expect(html).toMatch(/<span>The engine evaluates weekly questions only<\/span>/)
  expect(renderToStaticMarkup(<ValidationPanel issues={[]} />)).toBe("")
})

test("a refusal shows once: the validation panel, with the codes under Details; an empty section says what is missing in its own words", async () => {
  const html = renderToStaticMarkup(<ValidationPanel issues={[{ code: "question.window_order", message: "The time window must be an ordered half-open UTC interval.", field: "window" }]} title="The checker refused this question" details={<div>question.window_order</div>} />)
  expect(html.match(/The time window must be an ordered half-open UTC interval\./g)?.length).toBe(1)
  expect(html).toMatch(/<details[^>]*><summary[^>]*>Details<\/summary><div[^>]*><div>question\.window_order<\/div>/)
  const { EmptyState } = await import("../src/components/panel.tsx")
  const { Wrench } = await import("lucide-react")
  expect(renderToStaticMarkup(<EmptyState icon={Wrench} title="No tools bound yet">No tool can run from Norn until an admin binds it.</EmptyState>)).toContain(">No tools bound yet<")
  expect(renderToStaticMarkup(<EmptyState icon={Wrench}>Nothing.</EmptyState>)).toContain(">No data yet<")
})

test("the e2e runner fails when no verb ran, as well as when one failed", () => {
  expect(exitStatus(0, 0)).toBe(1)
  expect(exitStatus(3, 1)).toBe(1)
  expect(exitStatus(3, 0)).toBe(0)
})

test("undo and redo shortcuts go to the editor that owns them; save and run only where offered", () => {
  const key = (key: string, extra: Partial<KeyboardEvent> = {}) => ({ key, metaKey: false, ctrlKey: true, shiftKey: false, ...extra })
  const owner = { shortcuts: true, save: true, run: false }
  expect(shortcutOf(key("z"), owner)).toBe("undo")
  expect(shortcutOf(key("Z", { shiftKey: true }), owner)).toBe("redo")
  expect(shortcutOf(key("y"), owner)).toBe("redo")
  expect(shortcutOf(key("s"), owner)).toBe("save")
  expect(shortcutOf(key("Enter"), owner)).toBeUndefined()
  expect(shortcutOf(key("z", { ctrlKey: false }), owner)).toBeUndefined()
  const covered = { ...owner, shortcuts: false }
  expect(shortcutOf(key("z"), covered)).toBeUndefined()
  expect(shortcutOf(key("y"), covered)).toBeUndefined()
  expect(shortcutOf(key("s"), covered)).toBeUndefined()
})

test("the dirty check compares maps, sets, dates and cycles, not their JSON", () => {
  expect(sameValue({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] })).toBe(true)
  expect(sameValue({ a: 1 }, { a: 1, b: undefined })).toBe(false)
  expect(sameValue(new Map([["k", 1]]), new Map([["k", 2]]))).toBe(false)
  expect(sameValue(new Map([["k", { v: 1 }]]), new Map([["k", { v: 1 }]]))).toBe(true)
  expect(sameValue(new Set([1, 2]), new Set([1, 3]))).toBe(false)
  expect(sameValue(new Date(1), new Date(2))).toBe(false)
  const a: { self?: unknown; n: number } = { n: 1 }; a.self = a
  const b: { self?: unknown; n: number } = { n: 1 }; b.self = b
  const c: { self?: unknown; n: number } = { n: 2 }; c.self = c
  expect(sameValue(a, b)).toBe(true)
  expect(sameValue(a, c)).toBe(false)
})
