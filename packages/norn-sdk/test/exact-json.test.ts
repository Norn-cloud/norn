import { expect, test } from "bun:test"
import { parseExactJson } from "../src/exact-json.ts"

test("EXACT-JSON-1: numbers stay their source text; everything else reads as JSON.parse reads it", () => {
  expect(parseExactJson('{"a":9007199254740993,"b":[12.500,-0.10,1e400,0],"c":{"d":"x\\"y\\u00e9","e":true,"f":null}}')).toEqual({
    a: "9007199254740993", b: ["12.500", "-0.10", "1e400", "0"], c: { d: "x\"yé", e: true, f: null },
  })
  const same: ReadonlyArray<readonly [string, unknown]> = [
    ['"\\u2028 \\/ \\b"', "\u2028 / \b"], ["[]", []], ["{}", {}], ["  [ 1 , { } ]  ", ["1", {}]], ["true", true], ["null", null], ['"\\ud83d\\ude00"', "\u{1F600}"],
  ]
  for (const [text, expected] of same) expect([text, parseExactJson(text)] as unknown[]).toEqual([text, expected])
})

test("EXACT-JSON-2: a `__proto__` key is an own member, never the object's prototype", () => {
  const value = parseExactJson('{"__proto__":{"polluted":"1"},"constructor":"x"}') as Record<string, unknown>
  expect(Object.hasOwn(value, "__proto__")).toBe(true)
  expect(Object.getPrototypeOf(value)).toBe(Object.prototype)
  expect(({} as Record<string, unknown>)["polluted"]).toBeUndefined()
})

test("EXACT-JSON-3: anything that is not JSON reads as undefined", () => {
  for (const text of ["", "{", "[1,]", "{\"a\":1,}", "01", "1.", ".5", "+1", "NaN", "'x'", "\"\u0001\"", "\"\\x\"", "tru", "[1] 2", "{a:1}", "[".repeat(100_000)]) {
    expect([text.slice(0, 20), parseExactJson(text)]).toEqual([text.slice(0, 20), undefined])
  }
})

test("EXACT-JSON-4: strings are scanned in one pass, so hostile escapes stay linear", () => {
  const hostile = `"${"\\\"".repeat(200_000)}`
  const start = performance.now()
  expect(parseExactJson(hostile)).toBeUndefined()
  expect(parseExactJson(`${hostile}"`)).toBe("\"".repeat(200_000))
  expect(performance.now() - start).toBeLessThan(1000)
})
