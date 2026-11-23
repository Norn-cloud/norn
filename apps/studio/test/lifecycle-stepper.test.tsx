import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import type { Concept, Lifecycle } from "../src/lib/catalog.ts"
import { LocaleProvider } from "../src/lib/locale.tsx"
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
  const checker = () => ({
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
