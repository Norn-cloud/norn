import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { layout, orderLifecycleStates } from "../../../packages/norn-demo-ui/src/elements/lifecycle-view.ts"
import type { Concept, Lifecycle } from "../src/lib/catalog.ts"
import type { Checker } from "../src/lib/canvas/checker.ts"
import { LocaleProvider } from "../src/lib/locale.tsx"
import { declaredLifecycleStates, lifecycleStates } from "../src/lib/objects.ts"
import { LifecycleStepper } from "../src/components/lifecycle-stepper.tsx"
import { findingsFor } from "../src/components/lifecycle-view.tsx"

const lifecycle: Lifecycle = {
  property: "status",
  initial: ["building"],
  terminal: ["approved"],
  endings: [{ state: "ready", outcome: "resting" }],
  transitions: [
    { id: "stage", from: ["building"], to: "ready" },
    { id: "approve", from: ["ready"], to: "approved" },
  ],
}

const concept: Concept = {
  name: "core.Example",
  title: "Example",
  titleLang: "en",
  titleNeedsReview: false,
  kind: "concept",
  definition: "object type",
  summary: undefined,
  summaryLang: "en",
  description: undefined,
  parents: [],
  fields: [{
    id: "status",
    label: "Status",
    labelLang: "en",
    valueLabels: new Map([["building", "Drafting"], ["ready", "Ready for review"], ["approved", "Approved"], ["archived", "Archived"]]),
    type: "One of: building, ready, approved, archived",
    optional: false,
    decimal: undefined,
    scalar: { kind: "enum", values: ["building", "ready", "approved", "archived"] },
  }],
  relationships: [],
  rules: [],
  laws: [],
  feeds: [],
  lifecycles: [lifecycle],
}

test("only the current state is marked; earlier declared states are not shown as taken", () => {
  const html = renderToStaticMarkup(<LifecycleStepper lifecycle={lifecycle} record={{ objectType: "t", id: "x", scope: "s", version: 1, values: { status: "ready" } }} />)
  expect(html.match(/aria-current="step"/g)?.length).toBe(1)
  expect(html).not.toContain("bg-accent")
  expect(html).not.toContain("rounded-full")
  const marked = html.match(/<li[^>]*aria-current="step"[^>]*>.*?<\/li>/)?.[0] ?? ""
  expect(marked).toContain('data-state="ready"')
  expect(marked).toContain("Ready")
  expect(marked).toContain("can rest here")
  expect(html).toContain("Approve")
  expect(html).toContain('data-state="approved"')
  expect(html).toContain('data-ending=""')
  expect(html).toContain("start")
  expect(html).toContain("ending")
})

test("stepper shares declared state labels, unreachable states, roles and RTL with the type-wide view", () => {
  const html = renderToStaticMarkup(
    <LocaleProvider initial="en">
      <LifecycleStepper lifecycle={lifecycle} record={{ objectType: "core.Example", id: "x", scope: "s", version: 1, values: { status: "ready" } }} concept={concept} />
    </LocaleProvider>,
  )
  expect(html).toContain("Drafting")
  expect(html).toContain("Ready for review")
  expect(html).toContain("Archived")
  expect(html.indexOf("Approved")).toBeLessThan(html.indexOf("Archived"))

  const arConcept: Concept = {
    ...concept,
    title: "مثال",
    fields: [{ ...concept.fields[0]!, label: "الحالة", valueLabels: new Map([["building", "قيد البناء"], ["ready", "جاهز"], ["approved", "معتمد"], ["archived", "مؤرشف"]]) }],
  }
  const arHtml = renderToStaticMarkup(
    <LocaleProvider initial="ar">
      <LifecycleStepper lifecycle={lifecycle} record={{ objectType: "core.Example", id: "x", scope: "s", version: 1, values: { status: "approved" } }} concept={arConcept} />
    </LocaleProvider>,
  )
  expect(arHtml).toContain('dir="rtl"')
  expect(arHtml).toContain("البداية")
  expect(arHtml).toContain("نهاية")
  expect(arHtml).toContain("←")
  expect(arHtml).toContain("لا يعلن التعريف أي خطوة من هنا.")
})

test("a transition ID shared with a state uses its own name in the next step", () => {
  const withSharedId: Lifecycle = {
    ...lifecycle,
    transitions: [{ id: "approved", from: ["ready"], to: "ready" }],
  }
  const html = renderToStaticMarkup(
    <LifecycleStepper lifecycle={withSharedId} concept={{
      ...concept,
      fields: concept.fields.map((field) => ({ ...field, valueLabels: new Map([...field.valueLabels ?? [], ["approved", "Approval complete"]]) })),
    }} record={{ objectType: "core.Example", id: "x", scope: "s", version: 1, values: { status: "ready" } }} />,
  )
  expect(html).toContain("Approval complete")
  expect(html).toContain(">Approved</b> (to Ready for review)")
})

test("findingsFor matches the exact widget page machine by pointer and ignores null witnesses", () => {
  const widget = {
    artifactId: "commerce.orders.widget",
    kind: "widget",
    payload: {
      ux: {
        pages: [
          {
            machine: {
              initialStateId: "open",
              transitions: [{ transitionId: "on-accepted", target: "done" }],
              lifecycle: { objectType: "commerce.PurchaseOrder", property: "status" },
            },
          },
          {
            machine: {
              initialStateId: "paying",
              transitions: [{ transitionId: "on-paid", target: "settled" }],
              lifecycle: { objectType: "commerce.PurchaseOrder", property: "payment_status" },
            },
          },
        ],
      },
    },
  }
  const checker: Checker = () => ({
    admitted: false,
    diagnostics: {
      diagnostics: [
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "page state `done` with the object `draft`",
          span: { pointer: "/payload/ux/pages/0/machine/states/2" },
          artifactId: "commerce.orders.widget",
          witness: { steps: [{ edge: 1, moves: [{ side: "ui", label: "on-accepted" }, { side: "backend", label: "submit" }] }] },
        },
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "page state `settled` with the object `unpaid`",
          span: { pointer: "/payload/ux/pages/1/machine/states/1" },
          artifactId: "commerce.orders.widget",
          witness: { steps: [{ edge: 2, moves: [{ side: "ui", label: "on-paid" }] }] },
        },
        {
          code: "widget.lifecycle.uncovered_transition",
          severity: "info",
          message: "uncovered",
          span: { pointer: "/payload/ux/pages/0/machine/lifecycle/syncs/0" },
          artifactId: "commerce.orders.widget",
          witness: null,
        },
      ],
    },
  })
  const statusFindings = findingsFor("commerce.PurchaseOrder", "status", [widget], checker)
  expect(statusFindings).toHaveLength(1)
  expect(statusFindings[0]?.pageState).toBe("done")

  const paymentFindings = findingsFor("commerce.PurchaseOrder", "payment_status", [widget], checker)
  expect(paymentFindings).toHaveLength(1)
  expect(paymentFindings[0]?.pageState).toBe("settled")
})

test("findingsFor requires a canonical RFC 6901 page pointer when two pages share one lifecycle, leaving missing or invalid pointers unattributed with no witness", () => {
  const twoPageWidget = {
    artifactId: "commerce.two_pages.widget",
    kind: "widget",
    payload: {
      ux: {
        pages: [
          {
            machine: {
              initialStateId: "open_zero",
              transitions: [{ transitionId: "on-accepted", target: "done_zero" }],
              lifecycle: { objectType: "commerce.PurchaseOrder", property: "status" },
            },
          },
          {
            machine: {
              initialStateId: "open_one",
              transitions: [{ transitionId: "on-accepted", target: "done_one" }],
              lifecycle: { objectType: "commerce.PurchaseOrder", property: "status" },
            },
          },
          {
            machine: {
              initialStateId: "paying",
              transitions: [{ transitionId: "on-paid", target: "settled" }],
              lifecycle: { objectType: "commerce.PurchaseOrder", property: "payment_status" },
            },
          },
        ],
      },
    },
  }
  const witness = { steps: [{ edge: 1, moves: [{ side: "ui" as const, label: "on-accepted" }, { side: "backend" as const, label: "submit" }] }] }
  const checker: Checker = () => ({
    admitted: false,
    diagnostics: {
      diagnostics: [
        // Canonical page 0 -> attributed to page 0.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "page 0 stuck",
          span: { pointer: "/payload/ux/pages/0/machine/states/2" },
          artifactId: "commerce.two_pages.widget",
          witness,
        },
        // Canonical page 1 -> attributed to page 1.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "page 1 stuck",
          span: { pointer: "/payload/ux/pages/1/machine/states/2" },
          artifactId: "commerce.two_pages.widget",
          witness,
        },
        // Non-canonical leading zero `/pages/01/` (forbidden by RFC 6901) -> unattributed, no witness attached.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "leading zero pointer",
          span: { pointer: "/payload/ux/pages/01/machine/states/2" },
          artifactId: "commerce.two_pages.widget",
          witness,
        },
        // Missing pointer -> unattributed, never selects page 0.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "missing pointer",
          span: null,
          artifactId: "commerce.two_pages.widget",
          witness,
        },
        // Unrecognized pointer -> unattributed, never selects page 0.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "unrecognized pointer",
          span: { pointer: "/payload/ux/other" },
          artifactId: "commerce.two_pages.widget",
          witness,
        },
        // Out-of-bounds page index -> unattributed.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "out of bounds pointer",
          span: { pointer: "/payload/ux/pages/9/machine/states/0" },
          artifactId: "commerce.two_pages.widget",
          witness,
        },
        // Canonical pointer to page 2 (different lifecycle: payment_status) -> excluded from status.
        {
          code: "widget.lifecycle.stuck",
          severity: "error",
          message: "payment page stuck",
          span: { pointer: "/payload/ux/pages/2/machine/states/1" },
          artifactId: "commerce.two_pages.widget",
          witness: { steps: [{ edge: 2, moves: [{ side: "ui" as const, label: "on-paid" }] }] },
        },
      ],
    },
  })

  const statusFindings = findingsFor("commerce.PurchaseOrder", "status", [twoPageWidget], checker)
  expect(statusFindings.map((f) => ({
    message: f.message,
    pageState: f.pageState,
    unattributed: f.unattributed ?? false,
    hasWitness: f.witness != null,
  }))).toEqual([
    { message: "page 0 stuck", pageState: "done_zero", unattributed: false, hasWitness: true },
    { message: "page 1 stuck", pageState: "done_one", unattributed: false, hasWitness: true },
    { message: "leading zero pointer", pageState: undefined, unattributed: true, hasWitness: false },
    { message: "missing pointer", pageState: undefined, unattributed: true, hasWitness: false },
    { message: "unrecognized pointer", pageState: undefined, unattributed: true, hasWitness: false },
    { message: "out of bounds pointer", pageState: undefined, unattributed: true, hasWitness: false },
  ])
})

test("record stepper and type-wide graph derive identical peer branch order from one shared ordering", () => {
  const branching: Lifecycle = {
    property: "status",
    initial: ["start"],
    terminal: ["approved", "rejected"],
    transitions: [
      { id: "reject", from: ["start"], to: "rejected" },
      { id: "approve", from: ["start"], to: "approved" },
    ],
  }
  const branchingConcept: Concept = {
    ...concept,
    fields: [{
      id: "status",
      label: "Status",
      labelLang: "en",
      valueLabels: new Map([["start", "Start"], ["approved", "Approved"], ["rejected", "Rejected"]]),
      type: "One of: start, approved, rejected",
      optional: false,
      decimal: undefined,
      scalar: { kind: "enum", values: ["start", "approved", "rejected"] },
    }],
    lifecycles: [branching],
  }
  const declared = declaredLifecycleStates(branching, branchingConcept.fields[0])
  const doc = { property: branching.property, states: declared, initial: branching.initial, terminal: branching.terminal ?? [], transitions: branching.transitions ?? [] }
  const graphOrder = layout(doc).flat()
  const sharedOrder = orderLifecycleStates(doc)
  const stepperOrder = lifecycleStates(branching, declared)
  expect(graphOrder).toEqual(["start", "approved", "rejected"])
  expect(sharedOrder).toEqual(graphOrder)
  expect(stepperOrder).toEqual(graphOrder)

  const html = renderToStaticMarkup(
    <LocaleProvider initial="en">
      <LifecycleStepper
        lifecycle={branching}
        concept={branchingConcept}
        record={{ objectType: "core.Example", id: "x", scope: "s", version: 1, values: { status: "start" } }}
      />
    </LocaleProvider>,
  )
  const renderedStates = [...html.matchAll(/data-state="([^"]+)"/g)].map((match) => match[1])
  expect(renderedStates).toEqual(["start", "approved", "rejected"])
})
