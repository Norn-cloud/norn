// What `<NornAppRenderer>` draws and refuses (#1762 milestone C, L6). The capability walk is held to
// the checker's own `renderer_capabilities` through its WASM export (`rendererCapabilities`), and the
// behaviours it declares to the pinned UI SDK catalog.
import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { missingCapabilities, NATIVE_PRIMITIVES, RENDERED_BEHAVIORS, RENDERED_PRIMITIVES, RENDERER_CAPABILITIES, requiredCapabilities } from "../src/capabilities.ts"
import { refusalsOf } from "../src/renderer.tsx"
import type { WidgetUx } from "../src/ux.ts"
import { matrixArtifact } from "./fixtures/matrix-page.ts"

const ROOT = join(import.meta.dir, "../../..")
const read = (path: string) => JSON.parse(readFileSync(join(ROOT, path), "utf8")) as { payload: { ux: WidgetUx } }
const pkg = process.env.NORN_CHECKER_WASM_PKG
const checker = pkg
  ? ((await import(pathToFileURL(join(pkg, "norn_checker_wasm.js")).href)) as { initSync(options: { module: Uint8Array }): unknown; rendererCapabilities(uxJson: string): string })
  : null
checker?.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
if (!checker && process.env.NORN_CHECKER_WASM_REQUIRED) throw new Error("NORN_CHECKER_WASM_PKG is required")

const fixture = matrixArtifact().payload.ux as WidgetUx
const fixturePage = fixture.pages[0]!
const vocabV2: WidgetUx = {
  ...fixture,
  pages: [{
    ...fixturePage,
    view: { kind: "form", subject: { kind: "action", action: { id: "canon.valuation_policy.set.action", version: "0.1.0" } } },
    root: {
      ...fixturePage.root,
      children: [
        ...fixturePage.root.children,
        { nodeId: "tab-a", component: { kind: "section", section: { sectionId: "tab-a", kind: "tab", label: { en: "Tab A" }, fields: [], order: 1 } }, behaviors: ["focus/roving-focus"], children: [] },
        { nodeId: "tab-b", component: { kind: "section", section: { sectionId: "tab-b", kind: "tab", label: { en: "Tab B" }, fields: [], order: 2 } }, behaviors: ["disclosure/disclosure"], children: [] },
        { nodeId: "sel-list", component: { kind: "list", dataBindingId: "records", fields: [], selection: "multiple" }, behaviors: ["lists/listbox"], children: [] },
        { nodeId: "dlg", component: { kind: "dialog", bindingId: "save", blocking: true, dismissalMethods: ["escape", "visible_close"] }, children: [] },
        { nodeId: "frm", component: { kind: "form", formId: "edit-form" }, children: [] },
      ],
    },
  }],
}
const documents: Readonly<Record<string, WidgetUx>> = {
  fixture,
  "vocab v2 (tabs, selectable list, dialog, form)": vocabV2,
  "decision sheet": read("apps/studio/src/decisions/decision-sheet.widget.json").payload.ux,
  "lifecycle view": read("packages/norn-demo-ui/src/elements/lifecycle-view.widget.json").payload.ux,
  "record page": read("registry/studio/artifacts/norn.studio.record.widget.json").payload.ux,
}

describe.skipIf(checker === null)("the capability walk is the checker's", () => {
  for (const [name, ux] of Object.entries(documents)) {
    test(name, () => {
      expect(requiredCapabilities(ux)).toEqual(JSON.parse(checker!.rendererCapabilities(JSON.stringify(ux))) as string[])
    })
  }
})

test("the declared behaviours are the pinned catalog's for the primitives drawn", () => {
  const catalog = JSON.parse(readFileSync(join(ROOT, "crates/norn-ui-checker/catalog/ui-sdk.json"), "utf8")) as { primitives: Record<string, { behaviors: string[] }> }
  const expected = [...new Set([...RENDERED_PRIMITIVES, ...NATIVE_PRIMITIVES].flatMap((name) => catalog.primitives[name]!.behaviors))].sort()
  expect([...RENDERED_BEHAVIORS] as string[]).toEqual(expected)
})

test("the fixture and vocab v2 contract are drawn whole; every capability they need is declared", () => {
  expect(requiredCapabilities(fixture)).toContain("agent:redaction")
  expect(requiredCapabilities(fixture)).toContain("agent:confirm")
  expect(missingCapabilities(fixture)).toEqual([])
  expect(missingCapabilities(vocabV2)).toEqual([])
  expect(refusalsOf(fixture, { "fixture-meter": () => null, "fixture-status": () => null }, false)).toEqual([])
})

test("dialog and form components are declared capabilities; child_grid and composition remain refused", () => {
  const page = fixture.pages[0]!
  const form = { ...fixture, pages: [{ ...page, root: { ...page.root, children: [...page.root.children, { nodeId: "a-form", component: { kind: "form" as const, formId: "f" }, children: [] }] } }] }
  expect(requiredCapabilities(form)).toContain("component:form")
  expect(missingCapabilities(form)).toEqual([])
  const dialog = { ...fixture, pages: [{ ...page, root: { ...page.root, children: [...page.root.children, { nodeId: "a-dialog", component: { kind: "dialog" as const, bindingId: "save", blocking: true, dismissalMethods: ["escape" as const] }, children: [] }] } }] }
  expect(requiredCapabilities(dialog)).toContain("component:dialog")
  expect(missingCapabilities(dialog)).toEqual([])
  const formView = { ...fixture, pages: [{ ...page, view: { kind: "form" as const, subject: { kind: "action", action: { id: "a", version: "0.1.0" } } } }] }
  expect(requiredCapabilities(formView)).toContain("view:form")
  expect(missingCapabilities(formView)).toEqual([])
  const childGrid = { ...fixture, pages: [{ ...page, root: { ...page.root, children: [...page.root.children, { nodeId: "a-grid", component: { kind: "child_grid" as const, formId: "f" }, children: [] }] } }] }
  expect(missingCapabilities(childGrid)).toEqual(["component:child_grid"])
  const composition = { ...fixture, pages: [{ ...page, root: { ...page.root, children: [...page.root.children, { nodeId: "a-comp", component: { kind: "composition" as const, widget: { id: "w", version: "0.1.0" }, pageId: "p" }, children: [] }] } }] }
  expect(missingCapabilities(composition)).toEqual(["component:composition"])
  // Swipe-only or dismissal-less dialogs fail closed so no user can be trapped.
  const swipeOnly = { ...fixture, pages: [{ ...page, root: { ...page.root, children: [...page.root.children, { nodeId: "swipe-dlg", component: { kind: "dialog" as const, blocking: false, dismissalMethods: ["swipe" as const] }, children: [] }] } }] }
  expect(missingCapabilities(swipeOnly)).toEqual(["dialog:swipe-only (swipe-dlg)"])
  const noDismiss = { ...fixture, pages: [{ ...page, root: { ...page.root, children: [...page.root.children, { nodeId: "trap-dlg", component: { kind: "dialog" as const, blocking: false, dismissalMethods: [] }, children: [] }] } }] }
  expect(missingCapabilities(noDismiss)).toEqual(["dialog:no-dismissal (trap-dlg)"])
})

test("a page that needs what the renderer does not draw is refused, saying what", () => {
  const page = fixture.pages[0]!
  const operation = { ...fixture, pages: [{ ...page, machine: { ...page.machine!, transitions: [...page.machine!.transitions, { transitionId: "op", from: "browsing", on: "save", target: "saving", effects: [{ kind: "start" as const, call: { kind: "operation" as const, operationId: "x" } }] }] } }] }
  expect(missingCapabilities(operation)).toEqual(["effect:start:operation"])
  const filtered = { ...fixture, dataBindings: fixture.dataBindings.map((binding) => ({ ...binding, query: { kind: "object_list" as const, filters: [{}], sort: [], page: { size: 1, maxSize: 1 } } })) }
  expect(missingCapabilities(filtered)).toEqual(["read:object_list:filtered (records)"])
  const declared = new Set([...RENDERER_CAPABILITIES].filter((capability) => capability !== "agent:redaction"))
  expect(missingCapabilities(fixture, declared)).toEqual(["agent:redaction"])
  expect(refusalsOf(fixture, { "fixture-meter": () => null }, false)).toEqual(["island:fixture-status"])
  const subscribed = { ...fixture, pages: [{ ...page, machine: { ...page.machine!, events: [...page.machine!.events, { eventId: "changed", source: { kind: "subscription" as const, bindingId: "records" } }] } }] }
  expect(refusalsOf(subscribed, { "fixture-meter": () => null, "fixture-status": () => null }, false)).toEqual(["live:feed"])
})
