import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { Database } from "lucide-react"
import { renderToStaticMarkup } from "react-dom/server"
import { avatarIndex, Initials, initialsOf, ListRow, RowList, TypeDisc } from "../src/components/system/list-row"
import { Panel, Well } from "../src/components/system/panel"
import { SourceTag } from "../src/components/system/source-tag"
import { StatusDot } from "../src/components/system/status-dot"
import { TableCard } from "../src/components/system/table-card"
import { filledTicks, TICKS } from "../src/components/system/tick-bar"
import { contrastOn, parseColor } from "../src/lib/color.ts"
import { rulesSummary } from "../src/lib/rules-summary.ts"
import type { Law } from "../src/lib/catalog.ts"
import { skinVars } from "../scripts/theme-palettes.ts"

test("a ListRow is a button named by its title, with the code, tag and one end slot after it", () => {
  const html = renderToStaticMarkup(<RowList><li><ListRow lead={<TypeDisc icon={Database} />} title="Orders" code="ord.Order" tag={<SourceTag>erp</SourceTag>} end="12" /></li></RowList>)
  expect(html).toContain('data-slot="row-list"')
  expect(html).toMatch(/^<ul[^>]*><li><button type="button"[^>]*data-slot="list-row"/)
  expect(html).toContain("min-h-12")
  expect(html).toContain("max-sm:min-h-[44px]")
  // Title first, so the accessible name starts with it; the code is mono and stays left-to-right.
  expect(html.indexOf("Orders")).toBeLessThan(html.indexOf("ord.Order"))
  expect(html).toContain('<bdi class="min-w-0 shrink-[2] truncate font-mono text-meta')
  expect(html).toContain('data-slot="list-row-end"')
  expect(html).not.toMatch(/font-(medium|semibold|bold)/)
})

test("a selected row is the card with a panel ring; a passive row is a plain div with no hover or focus", () => {
  const selected = renderToStaticMarkup(<ListRow title="A" selected />)
  expect(selected).toContain('data-selected="true"')
  expect(selected).toContain("data-selected:bg-card")
  const passive = renderToStaticMarkup(<ListRow title="A" passive />)
  expect(passive).toMatch(/^<div /)
  expect(passive).not.toContain("hover:bg-")
  expect(passive).not.toContain('type="button"')
})

test("a ListRow renders as the element it is given, keeping its own slot and classes", () => {
  const html = renderToStaticMarkup(<ListRow render={<a href="/sources" />} title="Add a source" />)
  expect(html).toMatch(/^<a href="\/sources"[^>]*data-slot="list-row"/)
})

test("an avatar disc hashes the id, never the name, onto the six avatar pairs", () => {
  expect(avatarIndex("principal:42")).toBe(avatarIndex("principal:42"))
  const ids = Array.from({ length: 60 }, (_, index) => `id-${index}`)
  expect(new Set(ids.map(avatarIndex)).size).toBe(6)
  for (const id of ids) expect(avatarIndex(id)).toBeGreaterThanOrEqual(0)
  // A rename or a switch of language keeps the colour.
  const en = renderToStaticMarkup(<Initials id="emp_7" name="Salma Adel" />)
  const ar = renderToStaticMarkup(<Initials id="emp_7" name="سلمى عادل" />)
  expect(en.match(/data-avatar="(\d)"/)?.[1]).toBe(ar.match(/data-avatar="(\d)"/)?.[1])
  expect(en).toContain(`bg-avatar-${avatarIndex("emp_7") + 1}-bg`)
  expect(en).toContain(">SA<")
  expect(initialsOf("Ada")).toBe("AD")
  expect(initialsOf("  ")).toBe("")
  expect(initialsOf("سلمى عادل")).toBe("سع")
})

test("a StatusDot is a dot and its word in muted text, never a filled pill; a SourceTag is an outline mono tag", () => {
  const dot = renderToStaticMarkup(<StatusDot tone="held">Held back</StatusDot>)
  expect(dot).toContain("bg-dot-held")
  expect(dot).toContain("Held back")
  expect(dot).toContain("text-muted-foreground")
  expect(dot).not.toMatch(/\bbg-(?!dot-)/)
  const tag = renderToStaticMarkup(<SourceTag>erpnext</SourceTag>)
  expect(tag).toContain("ring-line")
  expect(tag).toContain("font-mono")
  expect(tag).toContain("<bdi")
  expect(tag).not.toMatch(/\bbg-/)
})

test("a TableCard is always a Panel's well: it brings a Panel only when it is not in one", () => {
  const alone = renderToStaticMarkup(<TableCard label="Rows"><table /></TableCard>)
  expect(alone).toMatch(/^<section[^>]*data-slot="panel"/)
  expect(alone).toContain(">Rows</h2>")
  expect(alone).toContain('data-slot="table-card"')
  const inside = renderToStaticMarkup(<Panel label="Rows"><TableCard><table /></TableCard></Panel>)
  expect(inside.match(/data-slot="panel"/g)).toHaveLength(1)
  const headless = renderToStaticMarkup(<TableCard><table /></TableCard>)
  expect(headless).not.toContain("<header")
  // Nothing else draws a rim: no shadow or panel radius on the table itself.
  expect(renderToStaticMarkup(<Panel label="x"><Well><TableCard><table /></TableCard></Well></Panel>)).toContain("[[data-slot=well]_&amp;]:border-x-0")
})

test("the tick bar fills its share in proportion, shows any share, and never fills all while one is not", () => {
  expect(TICKS).toBe(10)
  expect(filledTicks(0, 14)).toBe(0)
  expect(filledTicks(3, 0)).toBe(0)
  expect(filledTicks(1, 30)).toBe(1)
  expect(filledTicks(5, 10)).toBe(5)
  expect(filledTicks(29, 30)).toBe(9)
  expect(filledTicks(30, 30)).toBe(10)
  expect(filledTicks(7, 3)).toBe(10)
})

const law = (id: string, artifactId?: string): Law => ({ id, artifactId, subject: "x", rationale: `${id} holds`, severity: "error", modality: "always", response: "predicate", owner: "pack" })

test("the rules summary reads only the server: a rule the inbox lists is holding records back, the rest claim nothing", () => {
  const laws = [law("a", "art.a"), law("b"), law("c")]
  const groups = [
    { ruleRef: { kind: "law", artifactId: "art.a", lawId: "a" }, count: 2, approximate: false },
    { ruleRef: { kind: "law", artifactId: "art.a", lawId: "a" }, count: 1, approximate: false },
    { ruleRef: { kind: "law", artifactId: "art.c", lawId: "c" }, count: 4, approximate: true },
    { ruleRef: { kind: "invariant" }, count: 9, approximate: false },
  ]
  const summary = rulesSummary(laws, groups, { count: 16, approximate: false })
  expect(summary.holding).toBe(2)
  expect(summary.rules.map(({ law: { id }, held }) => [id, held])).toEqual([["c", 4], ["a", 3], ["b", 0]])
  expect(summary.rules.find(({ law: { id } }) => id === "c")?.approximate).toBe(true)
  expect(summary.partial).toBe(false)
  expect(summary.clear).toBe(false)
  // The cell listed fewer records than its section counted, or hit its scan bound: a lower bound.
  expect(rulesSummary(laws, groups, { count: 40, approximate: false }).partial).toBe(true)
  expect(rulesSummary(laws, groups, { count: 16, approximate: true }).partial).toBe(true)
  // Only an exact empty section says nothing is held back; a missing read says nothing at all.
  expect(rulesSummary(laws, [], { count: 0, approximate: false }).clear).toBe(true)
  expect(rulesSummary(laws, [], { count: 0, approximate: true }).clear).toBe(false)
  const unread = rulesSummary(laws, undefined, undefined)
  expect(unread).toEqual({ rules: unread.rules, holding: 0, partial: false, clear: false })
})

test("every avatar pair reads at 4.5:1 or more in both schemes (DESIGN.md 4.4, 16.2)", () => {
  const css = readFileSync(join(import.meta.dir, "../src/index.css"), "utf8")
  for (const skin of ["polar", "open"] as const) {
    for (const scheme of ["light", "dark"] as const) {
      const vars = skinVars(css, skin, scheme)
      for (let n = 1; n <= 6; n++) {
        const ratio = contrastOn(parseColor(vars[`avatar-${n}-fg`]!)!, parseColor(vars[`avatar-${n}-bg`]!)!)
        expect(ratio, `${skin} ${scheme} avatar ${n}`).toBeGreaterThanOrEqual(4.5)
      }
    }
  }
})
