import { expect, test } from "bun:test"
import { Wrench } from "lucide-react"
import { renderToStaticMarkup } from "react-dom/server"
import { EmptyState } from "../src/components/panel"

test("an empty state reads its own title, or No data yet", () => {
  const bound = renderToStaticMarkup(<EmptyState icon={Wrench} title="No tools bound yet">No tool can run from Norn until an admin binds it.</EmptyState>)
  expect(bound).toContain("No tools bound yet")
  expect(bound).toContain("No tool can run from Norn until an admin binds it.")
  expect(bound).not.toContain("No data yet")
  expect(renderToStaticMarkup(<EmptyState icon={Wrench}>Nothing here.</EmptyState>)).toContain("No data yet")
})
