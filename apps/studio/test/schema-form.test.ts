import { expect, test } from "bun:test"
import { buildInput, formFields } from "../src/lib/schema-form.ts"

const schema = {
  type: "object",
  properties: {
    state: { enum: ["open", "closed"], description: "Issue state" },
    repo: { type: "string", title: "Repository" },
    limit: { type: "integer" },
    since: { type: ["string", "null"], format: "date-time" },
    draft: { type: "boolean" },
    labels: { type: "array", items: { type: "string" } },
  },
  required: ["repo"],
}

test("a tool's input schema becomes form fields, required first", () => {
  const fields = formFields(schema)
  expect(fields?.map((f) => [f.name, f.kind, f.required])).toEqual([
    ["repo", "text", true], ["draft", "boolean", false], ["labels", "json", false],
    ["limit", "integer", false], ["since", "datetime", false], ["state", "enum", false],
  ])
  expect(fields?.[0]?.label).toBe("Repository")
  expect(fields?.find((f) => f.name === "state")?.options).toEqual(["open", "closed"])
  expect(formFields({ type: "string" })).toBeNull()
  expect(formFields(null)).toBeNull()
})

test("field text becomes typed input, empty optional fields are left out", () => {
  const fields = formFields(schema) ?? []
  expect(buildInput(fields, { repo: "norn", limit: "5", draft: "false", labels: '["bug"]', state: "open" }))
    .toEqual({ ok: true, value: { repo: "norn", limit: 5, draft: false, labels: ["bug"], state: "open" } })
  expect(buildInput(fields, {})).toEqual({ ok: false, error: "Repository is required." })
  expect(buildInput(fields, { repo: "n", limit: "1.5" })).toEqual({ ok: false, error: "limit must be a whole number." })
  expect(buildInput(fields, { repo: "n", state: "merged" })).toEqual({ ok: false, error: "state must be one of open, closed." })
  expect(buildInput(fields, { repo: "n", labels: "[" })).toEqual({ ok: false, error: "labels must be JSON." })
})
