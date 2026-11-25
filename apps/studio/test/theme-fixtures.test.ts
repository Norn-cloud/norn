// Themes as data: the server's checker and Studio's validators give the same answers. The golden
// themes in fixtures/themes are shared with the checker; expected.json holds each one's codes.
import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { lowerTheme } from "../../../tools/norn-ts/src/builder/theme.ts"
import { parseServedTheme, parseStudioTheme } from "../src/lib/studio-theme.ts"

const DIR = join(import.meta.dir, "../../../fixtures/themes")
const expected = JSON.parse(readFileSync(join(DIR, "expected.json"), "utf8")) as Record<string, readonly string[]>
const codes = (result: { readonly ok: boolean; readonly issues: readonly { readonly code: string }[] }) => [...new Set(result.issues.map((issue) => issue.code))].sort()

test("every golden theme has an expected answer, and every answer a theme", () => {
  const themes = readdirSync(DIR).filter((name) => name.endsWith(".json") && name !== "expected.json").sort()
  expect(themes).toEqual(Object.keys(expected).sort())
  expect(themes.length).toBeGreaterThan(0)
})

for (const [file, want] of Object.entries(expected)) {
  test(`${file}: Studio's validators answer ${want.length === 0 ? "admitted" : want.join(", ")}, as the checker does`, () => {
    const payload = JSON.parse(readFileSync(join(DIR, file), "utf8")) as unknown
    // A served theme is gated as the checker gates it (canonical colours only) …
    const served = parseServedTheme(payload)
    expect(codes(served)).toEqual([...want].sort())
    expect(served.ok).toBe(want.length === 0)
    // … and so is the same theme typed into the editor, or written as code with `defineTheme`.
    expect(codes(parseStudioTheme(payload))).toEqual([...want].sort())
    const lowered = lowerTheme(payload as Parameters<typeof lowerTheme>[0])
    expect(lowered.ok ? [] : codes(lowered)).toEqual([...want].sort())
  })
}
