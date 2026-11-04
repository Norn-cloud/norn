import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import type { Lifecycle } from "../src/lib/catalog.ts"
import { LifecycleStepper } from "../src/objects.tsx"

const lifecycle: Lifecycle = {
  property: "status",
  initial: ["building"],
  transitions: [
    { id: "stage", from: ["building"], to: "ready" },
    { id: "approve", from: ["ready"], to: "approved" },
  ],
}

test("only the current state is marked; earlier declared states are not shown as taken", () => {
  const html = renderToStaticMarkup(<LifecycleStepper lifecycle={lifecycle} record={{ objectType: "t", id: "x", scope: "s", version: 1, values: { status: "ready" } }} />)
  expect(html.match(/aria-current="step"/g)?.length).toBe(1)
  expect(html).not.toContain("bg-accent")
  const marked = html.match(/<li[^>]*aria-current="step"[^>]*>.*?<\/li>/)?.[0] ?? ""
  expect(marked).toContain("ready")
  expect(html).toContain("approve")
})
