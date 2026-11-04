import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { CorrectionsStatus } from "../src/restatement-status.tsx"

test("the corrections anchor is there while reading and after a failed read", () => {
  expect(renderToStaticMarkup(<CorrectionsStatus />)).toContain('id="corrections"')
  const failed = renderToStaticMarkup(<CorrectionsStatus error={new Error("read failed")} />)
  expect(failed).toContain('id="corrections"')
  expect(failed).toContain("read failed")
})
