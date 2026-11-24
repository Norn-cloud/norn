import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { ALL_LEARNING_RESOURCES, advertiseLearning } from "./learning-cell.ts"
import { session } from "./object-cell.ts"

/**
 * Capability gating for Studio Learning destinations (#1220, PR #1631 S1):
 * - A signed-in cell advertising no learning resources hides the Learning sidebar section and
 *   mounts "Unavailable in this workspace" on every direct Learning URL with ZERO data fetches.
 * - A failed capability read (e.g. 403 on `/v1/definitions`) also hides Learning and blocks data
 *   fetches on direct URLs.
 * - A signed-in cell advertising learning resources reveals only the destinations whose required
 *   resources it advertises.
 */

const episodesFixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-episode.json", import.meta.url), "utf8")) as {
  readonly records: readonly object[]
}
const casesFixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-cases.json", import.meta.url), "utf8")) as {
  readonly cases: readonly object[]
  readonly evalSets: readonly object[]
}
const misfitFixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-misfit.json", import.meta.url), "utf8")) as {
  readonly misfitByLaw: object
  readonly triageOverTime: object
}
const proposalsFixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-proposals.json", import.meta.url), "utf8")) as {
  readonly records: readonly object[]
}

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/learning-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

test.beforeEach(({ }, info) => {
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the learning capability-gate stand-in`)
})

const LEARNING_URLS = [
  "/learning/episodes",
  "/episodes/ep_demo_overridden",
  "/learning/cases",
  "/learning/misfit",
  "/learning/proposals",
  "/learning/proposals/prop_demo_credit_tighten",
] as const

test("gating (negative): signed-in cell advertising no learning resources hides Learning nav and blocks page fetches on direct URLs", async ({ page }) => {
  await session(page)
  await advertiseLearning(page, { objectTypes: [], publishedQuestions: [] })

  const leakedDataCalls: string[] = []
  await page.route("**/api/host/v1/objects/learning.**", (route) => {
    leakedDataCalls.push(route.request().url())
    return route.fulfill({ status: 500, body: "unexpected learning object fetch" })
  })
  await page.route("**/api/host/v1/questions/learning.**", (route) => {
    leakedDataCalls.push(route.request().url())
    return route.fulfill({ status: 500, body: "unexpected learning question fetch" })
  })
  await page.route("**/api/host/v1/actions**", (route) => {
    const url = new URL(route.request().url())
    if ((url.searchParams.get("objectKind") ?? "").startsWith("learning.")) {
      leakedDataCalls.push(route.request().url())
      return route.fulfill({ status: 500, body: "unexpected learning action fetch" })
    }
    return route.fulfill({ json: { items: [] } })
  })

  await page.goto("/")
  await expect(page.getByText("Connecting…")).toHaveCount(0)

  const nav = page.getByRole("navigation", { name: "Workspace" })
  await expect(nav).toBeVisible()
  await expect(nav).not.toContainText("Learning")
  for (const label of ["Episodes", "Cases & eval sets", "Misfit board", "Proposals"]) {
    await expect(nav.getByRole("link", { name: label })).toHaveCount(0)
  }

  for (const path of LEARNING_URLS) {
    await page.goto(path)
    await expect(page.locator('[data-learning-gate="unavailable"]')).toBeVisible()
    await expect(page.getByText("Unavailable in this workspace")).toBeVisible()
    if (path === "/learning/episodes") await shoot(page, "gated-unavailable")
  }

  expect(leakedDataCalls).toEqual([])
})

test("gating (failed read): when /v1/definitions fails, Learning stays hidden and direct URLs stay unavailable with zero data fetches", async ({ page }) => {
  await session(page)
  await page.route("**/api/host/v1/definitions**", (route) =>
    route.fulfill({ status: 403, json: { code: "authz.denied", message: "denied" } }),
  )

  const leakedDataCalls: string[] = []
  await page.route("**/api/host/v1/objects/learning.**", (route) => {
    leakedDataCalls.push(route.request().url())
    return route.fulfill({ status: 500, body: "unexpected fetch" })
  })
  await page.route("**/api/host/v1/questions/learning.**", (route) => {
    leakedDataCalls.push(route.request().url())
    return route.fulfill({ status: 500, body: "unexpected fetch" })
  })

  await page.goto("/learning/cases")
  await expect(page.locator('[data-learning-gate="unavailable"]')).toBeVisible()
  await expect(page.getByText("Unavailable in this workspace")).toBeVisible()
  const nav = page.getByRole("navigation", { name: "Workspace" })
  await expect(nav).not.toContainText("Learning")
  expect(leakedDataCalls).toEqual([])
})

test("gating (positive & partial): cell advertising all learning resources shows all four links; partial advertisement reveals only satisfied destinations", async ({ page }) => {
  await session(page)
  await advertiseLearning(page, ALL_LEARNING_RESOURCES)

  await page.route("**/api/host/v1/objects/learning.Episode**", (route) =>
    route.fulfill({ json: { objectType: "learning.Episode", records: episodesFixture.records, query: { sort: [], filter: null }, total: { count: 3, approximate: false }, next: null, quarantineCount: 0 } }),
  )
  await page.route("**/api/host/v1/objects/learning.CuratedCase**", (route) =>
    route.fulfill({ json: { objectType: "learning.CuratedCase", records: casesFixture.cases, total: { count: 2, approximate: false }, next: null, quarantineCount: 0 } }),
  )
  await page.route("**/api/host/v1/objects/learning.EvalSet**", (route) =>
    route.fulfill({ json: { objectType: "learning.EvalSet", records: casesFixture.evalSets, total: { count: 2, approximate: false }, next: null, quarantineCount: 0 } }),
  )
  await page.route("**/api/host/v1/questions/learning.misfit_by_law/answer**", (route) =>
    route.fulfill({ json: misfitFixture.misfitByLaw }),
  )
  await page.route("**/api/host/v1/questions/learning.triage_over_time/answer**", (route) =>
    route.fulfill({ json: misfitFixture.triageOverTime }),
  )
  await page.route("**/api/host/v1/objects/learning.GapProposal**", (route) =>
    route.fulfill({ json: { objectType: "learning.GapProposal", records: proposalsFixture.records, query: { sort: [], filter: null }, total: { count: 4, approximate: false }, next: null, quarantineCount: 0 } }),
  )
  await page.route("**/api/host/v1/actions**", (route) => route.fulfill({ json: { items: [] } }))

  await page.goto("/")
  const nav = page.getByRole("navigation", { name: "Workspace" })
  await expect(nav).toContainText("Learning")
  for (const label of ["Episodes", "Cases & eval sets", "Misfit board", "Proposals"]) {
    await expect(nav.getByRole("link", { name: label })).toBeVisible()
  }

  await nav.getByRole("link", { name: "Episodes" }).click()
  await expect(page.getByRole("table", { name: "Decision episodes" })).toBeVisible()

  await nav.getByRole("link", { name: "Cases & eval sets" }).click()
  await expect(page.getByRole("table", { name: "Curated cases" })).toBeVisible()

  await nav.getByRole("link", { name: "Misfit board" }).click()
  await expect(page.getByRole("table", { name: "Misfit by law" })).toBeVisible()

  await nav.getByRole("link", { name: "Proposals" }).click()
  await expect(page.getByRole("table", { name: "Gap proposals" })).toBeVisible()

  // Partial advertisement: only Episode + one of two Misfit questions → only Episodes appears.
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await session(page)
  await advertiseLearning(page, {
    objectTypes: ["learning.Episode", "learning.CuratedCase"],
    publishedQuestions: ["learning.misfit_by_law"],
  })
  const blockedCalls: string[] = []
  await page.route("**/api/host/v1/objects/learning.Episode**", (route) =>
    route.fulfill({ json: { objectType: "learning.Episode", records: episodesFixture.records, query: { sort: [], filter: null }, total: { count: 3, approximate: false }, next: null, quarantineCount: 0 } }),
  )
  await page.route("**/api/host/v1/objects/learning.CuratedCase**", (route) => {
    blockedCalls.push(route.request().url())
    return route.fulfill({ status: 500, body: "should stay gated when EvalSet is absent" })
  })
  await page.route("**/api/host/v1/questions/learning.**", (route) => {
    blockedCalls.push(route.request().url())
    return route.fulfill({ status: 500, body: "should stay gated when second question is absent" })
  })

  await page.goto("/")
  await expect(nav.getByRole("link", { name: "Episodes" })).toBeVisible()
  await expect(nav.getByRole("link", { name: "Cases & eval sets" })).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Misfit board" })).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Proposals" })).toHaveCount(0)

  await page.goto("/learning/cases")
  await expect(page.locator('[data-learning-gate="unavailable"]')).toBeVisible()
  await page.goto("/learning/misfit")
  await expect(page.locator('[data-learning-gate="unavailable"]')).toBeVisible()
  expect(blockedCalls).toEqual([])
})
