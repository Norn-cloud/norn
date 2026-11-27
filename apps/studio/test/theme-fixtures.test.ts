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
  test(`${file}: wire validators (parseServedTheme + lowerTheme) answer ${want.length === 0 ? "admitted" : want.join(", ")}, matching Ring-1`, () => {
    const payload = JSON.parse(readFileSync(join(DIR, file), "utf8")) as unknown
    // Wire theme admission: canonical colours only, matching Ring-1 `norn-ui-checker` and `defineTheme`.
    const served = parseServedTheme(payload)
    expect(codes(served)).toEqual([...want].sort())
    expect(served.ok).toBe(want.length === 0)
    const lowered = lowerTheme(payload as Parameters<typeof lowerTheme>[0])
    expect(lowered.ok ? [] : codes(lowered)).toEqual([...want].sort())

    // Studio's human editor normalizes CSS Color 4 input (such as named colours) into canonical
    // wire form before sending to the cell; every admitted editor output must pass `parseServedTheme`.
    const edited = parseStudioTheme(payload)
    if (file === "named-color.json") {
      expect(edited.ok).toBe(true)
      if (edited.ok) {
        expect(edited.theme.colors?.["chart-1"]).toBe("#ff0000")
        expect(parseServedTheme(edited.theme).ok).toBe(true)
      }
    } else {
      expect(codes(edited)).toEqual([...want].sort())
    }
  })
}
