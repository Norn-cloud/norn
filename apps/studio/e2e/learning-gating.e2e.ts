import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { ALL_LEARNING_RESOURCES, advertiseLearning, learningCatalogPayload } from "./learning-cell.ts"
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
    return route.fulfill({ json: { definitionsVersion: 1, items: [] } })
  })

  await page.goto("/")
  await expect(page.getByText("Connecting…")).toHaveCount(0)

  const rail = page.getByRole("navigation", { name: "Applets" })
  await expect(rail).toBeVisible()
  await expect(rail.getByRole("link", { name: "Learning" })).toHaveCount(0)
  for (const label of ["Episodes", "Cases & eval sets", "Misfit board", "Proposals"]) {
    await expect(page.getByRole("link", { name: label })).toHaveCount(0)
  }

  for (const path of LEARNING_URLS) {
    await page.goto(path)
    // Require the settled catalog state ("ready"), not the initial pending state.
    await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="ready"]')).toBeVisible()
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
  // Require the settled failed-read state ("error" with Retry), not the initial pending state.
  const gate = page.locator('[data-learning-gate="unavailable"][data-gate-status="error"]')
  await expect(gate).toBeVisible()
  await expect(page.getByText("Unavailable in this workspace")).toBeVisible()
  await expect(gate.getByRole("button", { name: "Retry", exact: true })).toBeVisible()
  await expect(page.getByRole("navigation", { name: "Applets" }).getByRole("link", { name: "Learning" })).toHaveCount(0)
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
  await page.route("**/api/host/v1/actions**", (route) => route.fulfill({ json: { definitionsVersion: 1, items: [] } }))

  await page.goto("/")
  await page.getByRole("navigation", { name: "Applets" }).getByRole("link", { name: "Learning" }).click()
  const nav = page.getByRole("navigation", { name: "Learning menu" })
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

  await page.goto("/learning/episodes")
  await expect(nav.getByRole("link", { name: "Episodes" })).toBeVisible()
  await expect(nav.getByRole("link", { name: "Cases & eval sets" })).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Misfit board" })).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Proposals" })).toHaveCount(0)

  await page.goto("/learning/cases")
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="ready"]')).toBeVisible()
  await page.goto("/learning/misfit")
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="ready"]')).toBeVisible()
  expect(blockedCalls).toEqual([])
})

test("gating (same-tab environment switch): invalidates cached catalog, unmounts page while replacement catalog is pending, and sends zero Learning fetches to the new cell before advertisement", async ({ page }) => {
  let currentEnv = "env-a"
  const episodeTemplate = episodesFixture.records[0] as {
    id: string
    values: { decision: { question: string } }
  }
  const makeEpisode = (id: string, question: string) => {
    const copy = structuredClone(episodeTemplate)
    copy.id = id
    copy.values.decision.question = question
    return copy
  }

  // Track every Learning request per environment, and hold B/C/D /v1/definitions pending until released.
  const learningRequestsByEnv: Record<string, string[]> = { "env-a": [], "env-b": [], "env-c": [], "env-d": [] }
  const defsRequested: Record<string, boolean> = {}
  const defsWaiters: Record<string, () => void> = {}
  const defsGates: Record<string, Promise<void>> = {}
  for (const env of ["env-b", "env-c", "env-d"]) {
    defsGates[env] = new Promise<void>((resolve) => { defsWaiters[env] = resolve })
  }
  let releaseCReady = false

  await page.route("**/api/me", (route) =>
    route.fulfill({
      json: {
        user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" },
        organization: { id: "org_demo", name: "Demo", role: "viewer" },
        environmentId: currentEnv,
      },
    }),
  )

  await page.route("**/api/host/v1/definitions**", async (route) => {
    const env = new URL(route.request().url()).searchParams.get("environmentId") ?? ""
    defsRequested[env] = true
    if (env === "env-a") {
      return route.fulfill({ json: learningCatalogPayload(ALL_LEARNING_RESOURCES) })
    }
    if (env === "env-b") {
      await defsGates["env-b"]
      return route.fulfill({ json: learningCatalogPayload({ objectTypes: [], publishedQuestions: [] }) })
    }
    if (env === "env-c") {
      await defsGates["env-c"]
      releaseCReady = true
      return route.fulfill({ json: learningCatalogPayload({ objectTypes: ["learning.Episode"], publishedQuestions: [] }) })
    }
    if (env === "env-d") {
      await defsGates["env-d"]
      return route.fulfill({ json: learningCatalogPayload({ objectTypes: [], publishedQuestions: [] }) })
    }
    return route.fulfill({ status: 404, body: "unknown env" })
  })

  await page.route("**/api/host/v1/objects/learning.**", (route) => {
    const url = new URL(route.request().url())
    const env = url.searchParams.get("environmentId") ?? ""
    ;(learningRequestsByEnv[env] ??= []).push(url.toString())
    if (env === "env-a") {
      return route.fulfill({
        json: { objectType: "learning.Episode", records: [makeEpisode("ep_cell_a", "Cell A episode")], query: { sort: [], filter: null }, total: { count: 1, approximate: false }, next: null, quarantineCount: 0 },
      })
    }
    if (env === "env-c") {
      expect(releaseCReady, "env-c episode fetch must wait until env-c catalog resolved").toBe(true)
      return route.fulfill({
        json: { objectType: "learning.Episode", records: [makeEpisode("ep_cell_c", "Cell C episode")], query: { sort: [], filter: null }, total: { count: 1, approximate: false }, next: null, quarantineCount: 0 },
      })
    }
    return route.fulfill({ status: 500, body: `unexpected Learning object fetch to ${env}` })
  })

  await page.route("**/api/host/v1/questions/learning.**", (route) => {
    const url = new URL(route.request().url())
    const env = url.searchParams.get("environmentId") ?? ""
    ;(learningRequestsByEnv[env] ??= []).push(url.toString())
    return route.fulfill({ status: 500, body: `unexpected Learning question fetch to ${env}` })
  })

  await page.route("**/api/host/v1/actions**", (route) => {
    const url = new URL(route.request().url())
    const env = url.searchParams.get("environmentId") ?? ""
    if ((url.searchParams.get("objectKind") ?? "").startsWith("learning.")) {
      ;(learningRequestsByEnv[env] ??= []).push(url.toString())
      if (env !== "env-a" && env !== "env-c") return route.fulfill({ status: 500, body: `unexpected Learning action fetch to ${env}` })
    }
    return route.fulfill({ json: { definitionsVersion: 1, items: [] } })
  })

  // 1. Mount /learning/episodes in org_demo / env-a (advertises all Learning resources).
  await page.goto("/learning/episodes")
  const nav = page.getByRole("navigation", { name: "Learning menu" })
  await expect(page.getByText("Cell A episode")).toBeVisible()
  await expect(nav.getByRole("link", { name: "Episodes" })).toBeVisible()
  expect(learningRequestsByEnv["env-a"]!.length).toBeGreaterThan(0)

  // 2. Same user, same org: server switches environment to env-b (whose catalog is held pending).
  // Clicking Refresh triggers a bound Learning read BEFORE focus/session refetch: it must NOT hit env-b,
  // and must immediately close the gate (unmounting Cell A's table) while env-b's catalog is pending.
  currentEnv = "env-b"
  await page.getByRole("button", { name: "Refresh" }).click()
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="pending"]')).toBeVisible()
  await expect(page.getByText("Cell A episode")).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Episodes" })).toHaveCount(0)
  await expect.poll(() => defsRequested["env-b"]).toBe(true)
  expect(learningRequestsByEnv["env-b"]).toEqual([])

  // 3. Release env-b's catalog with NO learning resources: gate settles as ready-unavailable with zero fetches to env-b.
  defsWaiters["env-b"]!()
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="ready"]')).toBeVisible()
  await expect(nav).not.toContainText("Learning")
  expect(learningRequestsByEnv["env-b"]).toEqual([])

  // 4. Switch in the same tab (via visibilitychange/focus refetch, no reload) to env-c with env-c's catalog held pending.
  currentEnv = "env-c"
  await page.evaluate(() => {
    document.dispatchEvent(new Event("visibilitychange", { bubbles: true }))
    window.dispatchEvent(new Event("focus"))
  })
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="pending"]')).toBeVisible()
  await expect.poll(() => defsRequested["env-c"]).toBe(true)
  expect(learningRequestsByEnv["env-c"]).toEqual([])

  // Release env-c's catalog advertising ONLY learning.Episode -> gate opens and reads Cell C (never stale Cell A).
  defsWaiters["env-c"]!()
  await expect(page.getByText("Cell C episode")).toBeVisible()
  await expect(page.getByText("Cell A episode")).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Episodes" })).toBeVisible()
  await expect(nav.getByRole("link", { name: "Cases & eval sets" })).toHaveCount(0)
  expect(learningRequestsByEnv["env-c"]!.length).toBeGreaterThan(0)

  // 5. While Cell C's Episodes page is mounted, visibilitychange/focus-driven switch to env-d (catalog held pending)
  // immediately unmounts the page and sends zero Learning fetches to env-d (even though focus triggers refetches concurrently).
  currentEnv = "env-d"
  await page.evaluate(() => {
    document.dispatchEvent(new Event("visibilitychange", { bubbles: true }))
    window.dispatchEvent(new Event("focus"))
  })
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="pending"]')).toBeVisible()
  await expect(page.getByText("Cell C episode")).toHaveCount(0)
  await expect(nav.getByRole("link", { name: "Episodes" })).toHaveCount(0)
  await expect.poll(() => defsRequested["env-d"]).toBe(true)
  expect(learningRequestsByEnv["env-d"]).toEqual([])
  defsWaiters["env-d"]!()
  await expect(page.locator('[data-learning-gate="unavailable"][data-gate-status="ready"]')).toBeVisible()
  expect(learningRequestsByEnv["env-d"]).toEqual([])
})
