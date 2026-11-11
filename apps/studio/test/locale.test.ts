import { expect, test } from "bun:test"
import arabic from "../../../crates/norn-checker/conformance/messages/ar.json"
import english from "../../../crates/norn-checker/conformance/messages/en.json"
import { catalogOf } from "../src/lib/catalog.ts"
import { labelIndex, pick, targetKey } from "../src/lib/labels.ts"
import { applyLocale, directionOf } from "../src/lib/locale.tsx"
import { formatMessage, isolate, localizeMessage, parseMessage, readValues } from "../src/lib/messages.ts"
import type { Artifact } from "../src/lib/transport.ts"

// A small seeded generator, so every property run is reproducible.
function random(seed: number) {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
const ID = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_"
const word = (next: () => number) => Array.from({ length: 1 + Math.floor(next() * 12) }, () => ID[Math.floor(next() * ID.length)]).join("")
const variables = (message: string) => parseMessage(message).flatMap((part) => "variable" in part ? [part.variable] : [])

const EN = (english as { messages: Record<string, { message: string }> }).messages
const AR = (arabic as { messages: Record<string, { message: string; review?: string }> }).messages

test("MESSAGE-LOCALE-1: every English template reads back exactly the values it was given", () => {
  const next = random(1)
  for (const [code, { message }] of Object.entries(EN)) {
    for (let run = 0; run < 8; run += 1) {
      const values = Object.fromEntries(variables(message).map((name) => [name, word(next)]))
      const said = parseMessage(message).map((part) => "text" in part ? part.text : values[part.variable]).join("")
      expect(readValues(message, said), code).toEqual(values)
    }
  }
})

test("MESSAGE-LOCALE-1: an Arabic message carries every value, isolated, and nothing else", () => {
  const next = random(2)
  expect(Object.keys(AR).sort()).toEqual(Object.keys(EN).sort())
  for (const [code, { message }] of Object.entries(EN)) {
    const values = Object.fromEntries(variables(message).map((name) => [name, word(next)]))
    const said = parseMessage(message).map((part) => "text" in part ? part.text : values[part.variable]).join("")
    const localized = localizeMessage(code, said, "ar")
    expect(localized.lang, code).toBe("ar")
    expect(localized.needsReview).toBe(true)
    for (const name of variables(AR[code]!.message)) expect(localized.text, code).toContain(isolate(values[name]!))
    expect(localized.text).toBe(formatMessage(AR[code]!.message, values))
  }
})

test("MESSAGE-LOCALE-1: a value that contains template text is read exactly or not at all, never split wrongly", () => {
  const next = random(3)
  // Values built from the fragments templates use as separators, so most parses are ambiguous.
  const fragments = [": ", " ", ", ", "; ", " -> ", "→", "(", ")", "'", "`", "a", "b", "1", "_", "."]
  const tricky = () => Array.from({ length: 1 + Math.floor(next() * 5) }, () => fragments[Math.floor(next() * fragments.length)]).join("")
  let refused = 0, read = 0
  for (const [code, { message }] of Object.entries(EN)) {
    if (variables(message).length === 0) continue
    for (let run = 0; run < 6; run += 1) {
      const values = Object.fromEntries(variables(message).map((name) => [name, tricky()]))
      const said = parseMessage(message).map((part) => "text" in part ? part.text : values[part.variable]).join("")
      const got = readValues(message, said)
      if (got === undefined) refused += 1
      else { read += 1; expect(got, `${code}: ${said}`).toEqual(values) }
      // Whatever was read, the Arabic text is either built from exactly these values or the server's words.
      const localized = localizeMessage(code, said, "ar")
      expect(localized.text, code).toBe(got === undefined ? said : formatMessage(AR[code]!.message, values))
    }
  }
  // Both outcomes occur: the generator reaches ambiguous and unambiguous messages.
  expect(refused).toBeGreaterThan(0)
  expect(read).toBeGreaterThan(0)
  // The reviewer's case: the rule's own text contains the template's ": ".
  const said = "the data breaks fk: line→order: no such order"
  expect(readValues(EN["consistency.instance.violation"]!.message, said)).toBeUndefined()
  expect(localizeMessage("consistency.instance.violation", said, "ar")).toEqual({ text: said, lang: "en", needsReview: false })
  expect(readValues(EN["consistency.instance.violation"]!.message, "the data breaks fk_line_order: no such order")).toEqual({ rule: "fk_line_order", detail: "no such order" })
})

test("an isolated value cannot close its own isolate: isolate controls inside it are removed", () => {
  expect(isolate("a\u2069b\u2066c\u2067d\u2068e")).toBe("\u2068abcde\u2069")
  const text = formatMessage("x {$v} y", { v: "1\u2069 2" })
  expect([...text].filter((char) => char === "\u2069")).toHaveLength(1)
  expect(text.endsWith("\u2069 y")).toBe(true)
})

test("a message that does not match its code's template, or an unknown code, stays in the server's words", () => {
  expect(localizeMessage("lifecycle.validation.empty_initial", "something else entirely", "ar")).toEqual({ text: "something else entirely", lang: "en", needsReview: false })
  expect(localizeMessage("demo.unknown_code", "kept as sent", "ar")).toEqual({ text: "kept as sent", lang: "en", needsReview: false })
  expect(localizeMessage("lifecycle.validation.empty_initial", "a lifecycle declares at least one initial state", "en").text).toBe("a lifecycle declares at least one initial state")
})

test("the simple MessageFormat 2 subset refuses every other expression", () => {
  for (const bad of ["{x}", "{$}", "{:number}", "a } b", "a \\n b", "{$x :number}"]) expect(() => parseMessage(bad)).toThrow()
  expect(parseMessage("a \\{literal\\} {$x} \\\\")).toEqual([{ text: "a {literal} " }, { variable: "x" }, { text: " \\" }])
})

const envelope = (artifactId: string, entries: unknown[]) => ({
  irVersion: "0.1", artifactId, kind: "labels", namespace: "t", name: artifactId, version: "1.0.0", lifecycle: "active",
  scope: {}, dependencies: [], provenance: { source: "system" }, payload: { labelsVersion: "0.1", entries },
}) as unknown as Artifact
const invoice = { kind: "objectType", objectType: "sales.Invoice" } as const

test("LABEL-LOCALE-1: later catalogs win per tag, a regional label serves its language, and no other language stands in", () => {
  const index = labelIndex([
    envelope("core.labels", [{ target: invoice, label: { en: "Invoice", ar: "فاتورة" } }]),
    envelope("tenant.labels", [{ target: invoice, label: { AR: "فاتورة ضريبية", "ar-EG": "فاتورة بيع" }, review: { "ar-eg": "needs_review" } }]),
  ])
  const key = targetKey(invoice)
  expect(pick(index, key, "ar")?.text).toBe("فاتورة ضريبية")
  expect(pick(index, key, "ar-EG")).toEqual({ text: "فاتورة بيع", description: undefined, tag: "ar-EG", needsReview: true })
  expect(pick(index, key, "en")?.text).toBe("Invoice")
  expect(pick(index, key, "fr")).toBeUndefined()
  // One locale rule: display falls back within the language only, both ways, and the label keeps its own tag.
  const regional = labelIndex([envelope("tenant.labels", [{ target: invoice, label: { "ar-EG": "فاتورة بيع", en: "Invoice" } }])])
  expect(pick(regional, key, "ar")?.tag).toBe("ar-EG")
  const base = labelIndex([envelope("tenant.labels", [{ target: invoice, label: { ar: "فاتورة", "ar-SA": "فاتورة", en: "Invoice" } }])])
  expect(pick(base, key, "ar-EG")?.tag).toBe("ar")
  expect(pick(base, key, "fr-CA")).toBeUndefined()
})

test("labels change the words, never the ids: a localized catalog keeps every name and fact id", () => {
  const entity = {
    irVersion: "0.1", artifactId: "sales.Invoice.entity", kind: "entity", namespace: "sales", name: "Invoice", version: "1.0.0", lifecycle: "active",
    scope: {}, dependencies: [], provenance: { source: "system" },
    payload: { entityVersion: "2.0", objectType: { name: "sales.Invoice", properties: [
      { id: "number", type: { kind: "text" } }, { id: "status", type: { kind: "enum", values: [{ value: "draft" }, { value: "posted" }] } },
    ] } },
  } as unknown as Artifact
  const labels = envelope("sales.labels", [
    { target: invoice, label: { ar: "فاتورة" }, description: { ar: "فاتورة بيع" } },
    { target: { kind: "property", objectType: "sales.Invoice", property: "status" }, label: { ar: "الحالة" } },
    { target: { kind: "enumValue", objectType: "sales.Invoice", property: "status", value: "posted" }, label: { ar: "مُرحّلة" } },
  ])
  const plain = catalogOf([entity, labels], "en").byName.get("sales.Invoice")!
  const arabic = catalogOf([entity, labels], "ar").byName.get("sales.Invoice")!
  expect([arabic.name, arabic.fields.map(({ id }) => id)]).toEqual([plain.name, plain.fields.map(({ id }) => id)])
  expect([plain.title, plain.titleLang]).toEqual(["Invoice", "en"])
  // The localized description is the Business summary; Technical keeps the registry's own description.
  expect([arabic.title, arabic.titleLang, arabic.summary, arabic.description]).toEqual(["فاتورة", "ar", "فاتورة بيع", undefined])
  // The envelope name repeating the English title is not a summary.
  expect(plain.summary).toBeUndefined()
  const fact = (id: string) => arabic.fields.find((field) => field.id === id)!
  expect([fact("status").label, fact("status").labelLang]).toEqual(["الحالة", "ar"])
  expect([fact("number").label, fact("number").labelLang]).toEqual(["number", "en"])
  expect([...fact("status").valueLabels ?? []]).toEqual([["posted", "مُرحّلة"]])
})

test("the document takes the reader's language and direction", () => {
  const root = { lang: "", dir: "" } as unknown as HTMLElement
  applyLocale("ar", root)
  expect([root.lang, root.dir, directionOf("en")]).toEqual(["ar", "rtl", "ltr"])
  applyLocale("en", root)
  expect([root.lang, root.dir]).toEqual(["en", "ltr"])
})
