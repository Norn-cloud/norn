import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { advertiseLearning } from "./learning-cell.ts"
import { session } from "./object-cell.ts"

// Curated cases and eval sets (#1220 L-3): create, protect, uncurate, eval set CRUD, seal,
// new version, undo and cell refusals. Every verb is driven solely by GET/POST /v1/actions.
// Skips against E2E_BASE until the learning pack serves learning.CuratedCase and learning.EvalSet.

const fixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-cases.json", import.meta.url), "utf8")) as {
  readonly cases: { id: string; objectType: string; scope: string; version: number; values: Record<string, unknown> }[]
  readonly evalSets: { id: string; objectType: string; scope: string; version: number; values: Record<string, unknown> }[]
}
const served = JSON.parse(readFileSync(new URL("../test/fixtures/action-call.json", import.meta.url), "utf8")) as {
  readonly committed: { readonly receipt: object }
}

const CURATE_CASE = {
  actionId: "learning.curate_case",
  title: "Curate case",
  description: "Pin a measured episode as a ledger-referenced curated case.",
  objectKind: "learning.CuratedCase",
  effect: "create_object",
  objectIdField: "caseId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["episodeId", "note"],
    properties: {
      episodeId: { type: "string", title: "Episode" },
      note: { type: "string", title: "Curator note" },
      protected: { type: "boolean", title: "Protect against regression" },
    },
  },
}

const SET_PROTECTED = {
  actionId: "learning.set_case_protected",
  title: "Set protection",
  description: "Mark or unmark a curated case as protected during proposal replay.",
  objectKind: "learning.CuratedCase",
  effect: "fire_transition",
  objectIdField: "caseId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["caseId", "protected"],
    properties: {
      caseId: { type: "string", title: "Case" },
      protected: { type: "boolean", title: "Protected" },
    },
  },
}

const UNCURATE_CASE = {
  actionId: "learning.uncurate_case",
  title: "Uncurate case",
  description: "Remove this curated case pin from the catalog.",
  objectKind: "learning.CuratedCase",
  effect: "fire_transition",
  objectIdField: "caseId",
  uiEffects: { irreversible: false, destructive: true, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["caseId"],
    properties: { caseId: { type: "string", title: "Case" } },
  },
}

const CREATE_EVAL_SET = {
  actionId: "learning.create_eval_set",
  title: "New eval set",
  description: "Create an unsealed eval set under a split key.",
  objectKind: "learning.EvalSet",
  effect: "create_object",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["name", "splitKey"],
    properties: {
      name: { type: "string", title: "Set name" },
      splitKey: { type: "string", title: "Split key" },
    },
  },
}

const RENAME_EVAL_SET = {
  actionId: "learning.rename_eval_set",
  title: "Rename",
  description: "Rename this unsealed eval set.",
  objectKind: "learning.EvalSet",
  effect: "fire_transition",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["evalSetId", "name"],
    properties: {
      evalSetId: { type: "string", title: "Eval set" },
      name: { type: "string", title: "New name" },
    },
  },
}

const ADD_CASE = {
  actionId: "learning.add_case",
  title: "Add case",
  description: "Pin a curated case reference into this unsealed eval set.",
  objectKind: "learning.EvalSet",
  effect: "fire_transition",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["evalSetId", "caseId"],
    properties: {
      evalSetId: { type: "string", title: "Eval set" },
      caseId: { type: "string", title: "Curated case" },
    },
  },
}

const REMOVE_CASE = {
  actionId: "learning.remove_case",
  title: "Remove case",
  description: "Remove a curated case reference from this unsealed eval set.",
  objectKind: "learning.EvalSet",
  effect: "fire_transition",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["evalSetId", "caseId"],
    properties: {
      evalSetId: { type: "string", title: "Eval set" },
      caseId: { type: "string", title: "Curated case" },
    },
  },
}

const SEAL_EVAL_SET = {
  actionId: "learning.seal_eval_set",
  title: "Seal eval set",
  description: "Seal this eval set so its pinned cases become immutable.",
  objectKind: "learning.EvalSet",
  effect: "fire_transition",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: true, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["evalSetId"],
    properties: { evalSetId: { type: "string", title: "Eval set" } },
  },
}

const DELETE_EVAL_SET = {
  actionId: "learning.delete_eval_set",
  title: "Delete eval set",
  description: "Delete this unsealed eval set.",
  objectKind: "learning.EvalSet",
  effect: "fire_transition",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: false, destructive: true, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["evalSetId"],
    properties: { evalSetId: { type: "string", title: "Eval set" } },
  },
}

const NEW_VERSION = {
  actionId: "learning.new_eval_set_version",
  title: "New version",
  description: "Start a new unsealed eval set version derived from this sealed set.",
  objectKind: "learning.EvalSet",
  effect: "fire_transition",
  objectIdField: "evalSetId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["evalSetId"],
    properties: { evalSetId: { type: "string", title: "Parent eval set" } },
  },
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
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the learning cases/eval-sets stand-in`)
})

interface StandInRecord {
  objectType: string
  id: string
  scope: string
  version: number | null
  values: Record<string, unknown> | null
  withheld?: boolean
  withheldProperties?: readonly string[]
}

interface MountOptions {
  readonly unserved?: boolean
  readonly pageSize?: number
  readonly initialCases?: readonly StandInRecord[]
  readonly initialEvalSets?: readonly StandInRecord[]
}

async function mountCell(page: Page, options: MountOptions = {}) {
  await session(page)
  await advertiseLearning(page, { objectTypes: ["learning.CuratedCase", "learning.EvalSet"] })
  const cases: StandInRecord[] = structuredClone(options.initialCases ?? fixture.cases) as StandInRecord[]
  const evalSets: StandInRecord[] = structuredClone(options.initialEvalSets ?? fixture.evalSets) as StandInRecord[]
  const removedCases = new Map<string, StandInRecord>()
  const removedEvalSets = new Map<string, StandInRecord>()
  const withheldCaseIds = new Set<string>()
  const withheldSetIds = new Set<string>()
  const calls: { actionId: string; input: Record<string, unknown> }[] = []
  const caseQueries: URL[] = []
  const evalSetQueries: URL[] = []
  let seq = 7000

  function projectRecord(r: StandInRecord, withheld: ReadonlySet<string>): StandInRecord {
    return withheld.has(r.id)
      ? { objectType: r.objectType, id: r.id, scope: r.scope, version: null, values: null, withheld: true, withheldProperties: [] }
      : r
  }

  function paginate(records: readonly StandInRecord[], url: URL, prefix: string, withheld: ReadonlySet<string>) {
    const requestedLimit = Number(url.searchParams.get("limit") ?? "50")
    const size = options.pageSize ?? requestedLimit
    const after = url.searchParams.get("after")
    let offset = 0
    if (after !== null) {
      const m = new RegExp(`^${prefix}:(\\d+)$`).exec(after)
      offset = m ? Number(m[1]) : 0
    }
    const slice = records.slice(offset, offset + size).map((r) => projectRecord(r, withheld))
    const next = offset + size < records.length ? `${prefix}:${offset + size}` : null
    return { slice, next }
  }

  await page.route("**/api/host/v1/objects/learning.CuratedCase**", (route) => {
    const url = new URL(route.request().url())
    caseQueries.push(url)
    if (options.unserved) return route.fulfill({ status: 404, json: { code: "object.type_unknown", message: "Not served" } })
    const { slice, next } = paginate(cases, url, "cases", withheldCaseIds)
    return route.fulfill({
      json: { objectType: "learning.CuratedCase", records: slice, total: { count: cases.length, approximate: false }, next, quarantineCount: 0 },
    })
  })

  await page.route("**/api/host/v1/objects/learning.EvalSet**", (route) => {
    const url = new URL(route.request().url())
    evalSetQueries.push(url)
    if (options.unserved) return route.fulfill({ status: 404, json: { code: "object.type_unknown", message: "Not served" } })
    const { slice, next } = paginate(evalSets, url, "sets", withheldSetIds)
    return route.fulfill({
      json: { objectType: "learning.EvalSet", records: slice, total: { count: evalSets.length, approximate: false }, next, quarantineCount: 0 },
    })
  })

  await page.route("**/api/host/v1/actions**", (route) => {
    if (options.unserved) return route.fulfill({ status: 404, body: "not found" })
    const request = route.request()
    const url = new URL(request.url())
    if (request.method() === "GET") {
      const kind = url.searchParams.get("objectKind")
      const objectId = url.searchParams.get("objectId")
      if (kind === "learning.CuratedCase") {
        return route.fulfill({ json: { definitionsVersion: 1, items: objectId ? [SET_PROTECTED, UNCURATE_CASE] : [CURATE_CASE] } })
      }
      if (kind === "learning.EvalSet") {
        if (!objectId) return route.fulfill({ json: { definitionsVersion: 1, items: [CREATE_EVAL_SET] } })
        const found = evalSets.find((s) => s.id === objectId)
        const items = found?.values?.["status"] === "sealed"
          ? [NEW_VERSION]
          : [RENAME_EVAL_SET, ADD_CASE, REMOVE_CASE, SEAL_EVAL_SET, DELETE_EVAL_SET]
        return route.fulfill({ json: { definitionsVersion: 1, items } })
      }
      return route.fulfill({ json: { definitionsVersion: 1, items: [] } })
    }

    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const { input } = JSON.parse(request.postData() ?? "{}") as { input: Record<string, unknown> }
    calls.push({ actionId, input })
    seq += 1

    if (actionId === CURATE_CASE.actionId) {
      if (input["episodeId"] === "ep_unmeasured") {
        return route.fulfill({
          status: 422,
          json: {
            code: "action.precondition_failed",
            message: "Only episodes with a recorded outcome can be curated.",
            diagnostics: [{ code: "learning.episode_unmeasured", message: "Episode outcome is not recorded yet.", span: { pointer: "/episodeId" } }],
          },
        })
      }
      const restored = typeof input["restoreCaseId"] === "string" ? removedCases.get(input["restoreCaseId"]) : undefined
      if (restored) {
        cases.unshift(restored)
        removedCases.delete(restored.id)
        return route.fulfill({
          json: { ...served.committed, objectType: "learning.CuratedCase", objectId: restored.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
        })
      }
      const created: StandInRecord = {
        objectType: "learning.CuratedCase",
        id: `case_new_${seq}`,
        scope: "org_demo/analytics",
        version: 1,
        values: {
          episodeId: String(input["episodeId"]),
          decisionReceipt: { receiptHash: "sha256:1010101010101010101010101010101010101010101010101010101010101010", commitSeq: seq },
          outcomeReceipt: { receiptHash: "sha256:2020202020202020202020202020202020202020202020202020202020202020", commitSeq: seq + 1 },
          question: "Curated measured shipment decision",
          expectedChoice: "release",
          agreement: "overridden",
          protected: Boolean(input["protected"]),
          note: String(input["note"] ?? ""),
          curatedBy: "you",
          curatedAtMs: Date.now(),
          evalSetIds: [],
        },
      }
      cases.unshift(created)
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.CuratedCase", objectId: created.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === SET_PROTECTED.actionId) {
      const target = cases.find((c) => c.id === input["caseId"])!
      target.values!["protected"] = Boolean(input["protected"])
      target.version = (target.version ?? 0) + 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.CuratedCase", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === UNCURATE_CASE.actionId) {
      const idx = cases.findIndex((c) => c.id === input["caseId"])
      const [removed] = cases.splice(idx, 1)
      if (removed) removedCases.set(removed.id, removed)
      return route.fulfill({
        json: {
          ...served.committed,
          objectType: "learning.CuratedCase",
          objectId: String(input["caseId"]),
          receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() },
          undo: { actionId: CURATE_CASE.actionId, input: { restoreCaseId: String(input["caseId"]), episodeId: removed?.values?.["episodeId"] ?? "ep_demo_overridden", note: "Restored" } },
        },
      })
    }

    if (actionId === CREATE_EVAL_SET.actionId) {
      const restored = typeof input["restoreEvalSetId"] === "string" ? removedEvalSets.get(input["restoreEvalSetId"]) : undefined
      if (restored) {
        evalSets.unshift(restored)
        removedEvalSets.delete(restored.id)
        return route.fulfill({
          json: { ...served.committed, objectType: "learning.EvalSet", objectId: restored.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
        })
      }
      const created: StandInRecord = {
        objectType: "learning.EvalSet",
        id: `eval_new_${seq}`,
        scope: "org_demo/analytics",
        version: 1,
        values: {
          name: String(input["name"]),
          splitKey: String(input["splitKey"]),
          setVersion: 1,
          status: "unsealed",
          sealedAtMs: null,
          sealedBy: null,
          parentSetId: null,
          caseRefs: [],
          createdBy: "you",
          createdAtMs: Date.now(),
        },
      }
      evalSets.unshift(created)
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.EvalSet", objectId: created.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === RENAME_EVAL_SET.actionId) {
      const target = evalSets.find((s) => s.id === input["evalSetId"])!
      target.values!["name"] = String(input["name"])
      target.version = (target.version ?? 0) + 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.EvalSet", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === ADD_CASE.actionId) {
      const target = evalSets.find((s) => s.id === input["evalSetId"])!
      const refs = target.values!["caseRefs"] as { caseId: string; episodeId: string; receiptHash: string; protected: boolean; label: string | null }[]
      const found = cases.find((c) => c.id === input["caseId"])
      refs.push({
        caseId: String(input["caseId"]),
        episodeId: String(found?.values?.["episodeId"] ?? "ep_demo_overridden"),
        receiptHash: "sha256:5f0c9a1e7b2d4c6a8e0f1b3d5c7a9e2f4b6d8c0a1e3f5b7d9c2a4e6f8b0d1c3a",
        protected: Boolean(found?.values?.["protected"] ?? false),
        label: String(found?.values?.["question"] ?? input["caseId"]),
      })
      target.version = (target.version ?? 0) + 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.EvalSet", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === REMOVE_CASE.actionId) {
      const target = evalSets.find((s) => s.id === input["evalSetId"])!
      const refs = target.values!["caseRefs"] as { caseId: string }[]
      target.values!["caseRefs"] = refs.filter((r) => r.caseId !== input["caseId"])
      target.version = (target.version ?? 0) + 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.EvalSet", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === SEAL_EVAL_SET.actionId) {
      const target = evalSets.find((s) => s.id === input["evalSetId"])!
      target.values!["status"] = "sealed"
      target.values!["sealedAtMs"] = Date.now()
      target.values!["sealedBy"] = "you"
      target.version = (target.version ?? 0) + 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.EvalSet", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === NEW_VERSION.actionId) {
      const parent = evalSets.find((s) => s.id === input["evalSetId"])!
      const nextVer = Number(parent.values!["setVersion"]) + 1
      const created: StandInRecord = {
        objectType: "learning.EvalSet",
        id: `${parent.id}_v${nextVer}`,
        scope: "org_demo/analytics",
        version: 1,
        values: {
          ...structuredClone(parent.values!),
          setVersion: nextVer,
          status: "unsealed",
          sealedAtMs: null,
          sealedBy: null,
          parentSetId: parent.id,
          createdBy: "you",
          createdAtMs: Date.now(),
        },
      }
      evalSets.unshift(created)
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.EvalSet", objectId: created.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === DELETE_EVAL_SET.actionId) {
      const idx = evalSets.findIndex((s) => s.id === input["evalSetId"])
      const [removed] = idx >= 0 ? evalSets.splice(idx, 1) : []
      if (removed) removedEvalSets.set(removed.id, removed)
      return route.fulfill({
        json: {
          ...served.committed,
          objectType: "learning.EvalSet",
          objectId: String(input["evalSetId"]),
          receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() },
          undo: {
            actionId: CREATE_EVAL_SET.actionId,
            input: {
              restoreEvalSetId: String(input["evalSetId"]),
              name: String(removed?.values?.["name"] ?? "Restored set"),
              splitKey: String(removed?.values?.["splitKey"] ?? "restored"),
            },
          },
        },
      })
    }

    return route.fulfill({ status: 404, json: { code: "action.not_found", message: "Unknown action" } })
  })

  await page.goto("/learning/cases")
  return { calls, cases, evalSets, withheldCaseIds, withheldSetIds, caseQueries, evalSetQueries }
}

const dialog = (page: Page) => page.getByRole("dialog").last()

test("curated cases: curate measured episode, refuse unmeasured, protect, uncurate and undo through the cell", async ({ page }) => {
  const { calls } = await mountCell(page)
  const table = page.getByRole("table", { name: "Curated cases" })
  await expect(table).toContainText("Release the held shipment?")
  await expect(table).toContainText("Approve the restock order?")
  await shoot(page, "cases")

  // Attempt to curate an unmeasured episode: cell refuses at the field, nothing is added.
  await page.getByRole("button", { name: "Curate case" }).click()
  await dialog(page).getByLabel("Episode").fill("ep_unmeasured")
  await dialog(page).getByLabel("Curator note").fill("Should be refused")
  await dialog(page).getByRole("button", { name: "Curate case", exact: true }).click()
  await expect(dialog(page).getByRole("alert").first()).toContainText("Episode outcome is not recorded yet.")

  // Curate a measured episode.
  await dialog(page).getByLabel("Episode").fill("ep_demo_measured_9")
  await dialog(page).getByLabel("Curator note").fill("Confirmed delivery payout")
  await dialog(page).getByLabel("Protect against regression").selectOption("true")
  await dialog(page).getByRole("button", { name: "Curate case", exact: true }).click()
  await expect(dialog(page).getByRole("status")).toBeVisible()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(table).toContainText("Curated measured shipment decision")

  // Open a standard case's detail sheet, toggle protection on, then uncurate + Undo.
  await table.getByRole("button", { name: "Approve the restock order?" }).click()
  const caseSheet = page.getByRole("dialog", { name: "Approve the restock order?" })
  await expect(caseSheet).toContainText("case_credit_overdue_02")
  await caseSheet.getByRole("button", { name: "Set protection" }).click()
  await dialog(page).getByLabel("Protected").selectOption("true")
  await dialog(page).getByRole("button", { name: "Set protection", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(caseSheet).toContainText("Protected")

  // Uncurate (destructive → confirm) then Undo restores the case.
  await caseSheet.getByRole("button", { name: "Uncurate case" }).click()
  await dialog(page).getByRole("button", { name: "Uncurate case", exact: true }).click()
  await expect(dialog(page).getByRole("alertdialog")).toContainText("This removes business data.")
  await dialog(page).getByRole("button", { name: "Yes, uncurate case" }).click()
  await expect(dialog(page).getByRole("status")).toBeVisible()
  await dialog(page).getByRole("button", { name: "Undo" }).click()
  await expect(dialog(page).getByText(/^Undone · /)).toBeVisible()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(table).toContainText("Approve the restock order?")
  expect(calls.map((c) => c.actionId)).toContain("learning.uncurate_case")
})

test("eval sets: create, rename, add/remove case, seal (immutable), new version from sealed, and delete unsealed", async ({ page }) => {
  await mountCell(page)
  await page.getByRole("tab", { name: "Eval sets" }).click()
  await expect(page.getByRole("list", { name: "Eval sets list" })).toContainText("Credit release regression")
  await expect(page.getByRole("list", { name: "Eval sets list" })).toContainText("Credit release holdout")
  await shoot(page, "eval-sets")

  // Create a new unsealed eval set from the toolbar primary action.
  await page.getByRole("button", { name: "New eval set" }).click()
  await dialog(page).getByLabel("Set name").fill("Q4 Credit Canary")
  await dialog(page).getByLabel("Split key").fill("credit_canary")
  await dialog(page).getByRole("button", { name: "New eval set", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await page.getByRole("list", { name: "Eval sets list" }).getByRole("button", { name: /Q4 Credit Canary/ }).click()

  const actions = page.getByRole("group", { name: "Eval set actions" })

  // Rename it.
  await actions.getByRole("button", { name: "Rename" }).click()
  await dialog(page).getByLabel("New name").fill("Q4 Credit Regression Canary")
  await dialog(page).getByRole("button", { name: "Rename", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("list", { name: "Eval sets list" })).toContainText("Q4 Credit Regression Canary")

  // Add a case, then remove it.
  await actions.getByRole("button", { name: "Add case" }).click()
  await dialog(page).getByLabel("Curated case").fill("case_credit_phone_01")
  await dialog(page).getByRole("button", { name: "Add case", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("table", { name: "Pinned case references" })).toContainText("ep_demo_overridden")

  await actions.getByRole("button", { name: "Remove case" }).click()
  await dialog(page).getByLabel("Curated case").fill("case_credit_phone_01")
  await dialog(page).getByRole("button", { name: "Remove case", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByText("No cases pinned yet")).toBeVisible()

  // Seal it (irreversible confirmation): once sealed, only server-offered "New version" remains.
  await actions.getByRole("button", { name: "Seal eval set" }).click()
  await dialog(page).getByRole("button", { name: "Seal eval set", exact: true }).click()
  await expect(dialog(page).getByRole("alertdialog")).toContainText("This can't be undone.")
  await dialog(page).getByRole("button", { name: "Yes, seal eval set" }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(actions.getByRole("button", { name: "New version" })).toBeVisible()
  await expect(actions.getByRole("button", { name: "Rename" })).toHaveCount(0)
  await expect(actions.getByRole("button", { name: "Delete eval set" })).toHaveCount(0)

  // Start a new unsealed version from the sealed set, then delete that unsealed version.
  await actions.getByRole("button", { name: "New version" }).click()
  await dialog(page).getByRole("button", { name: "New version", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await page.getByRole("list", { name: "Eval sets list" }).getByRole("button", { name: /Q4 Credit Regression Canary.*v2/ }).click()
  await expect(actions.getByRole("button", { name: "Delete eval set" })).toBeVisible()
  await actions.getByRole("button", { name: "Delete eval set" }).click()
  await dialog(page).getByRole("button", { name: "Delete eval set", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Yes, delete eval set" }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("list", { name: "Eval sets list" }).getByRole("button", { name: /Q4 Credit Regression Canary.*v2/ })).toHaveCount(0)
})

test("S3: refetching a withheld case or eval set clears stale details and offers immediately", async ({ page }) => {
  const { withheldCaseIds, withheldSetIds } = await mountCell(page)
  const table = page.getByRole("table", { name: "Curated cases" })

  // Open a curated case sheet and verify live details and actions render.
  await table.getByRole("button", { name: "Approve the restock order?" }).click()
  const caseSheet = page.getByRole("dialog", { name: "Approve the restock order?" })
  await expect(caseSheet.locator('[data-case-sheet-state="ok"]')).toBeVisible()
  await expect(caseSheet.getByRole("button", { name: "Set protection" })).toBeVisible()

  // Trigger a refetch while the sheet stays open (via Set protection → refreshAll) with the case now withheld.
  await caseSheet.getByRole("button", { name: "Set protection" }).click()
  await dialog(page).getByLabel("Protected").selectOption("true")
  withheldCaseIds.add("case_credit_overdue_02")
  await dialog(page).getByRole("button", { name: "Set protection", exact: true }).click()
  await expect(dialog(page).getByRole("status")).toBeVisible()
  await dialog(page).getByRole("button", { name: "Done" }).click()

  // Stale details and offers are gone immediately; the open sheet renders the withheld state.
  const updatedSheet = page.getByRole("dialog", { name: "Case withheld" })
  await expect(updatedSheet.locator('[data-case-sheet-state="withheld"]')).toBeVisible()
  await expect(updatedSheet).toContainText("Withheld by cell policy")
  await expect(updatedSheet.getByRole("button", { name: "Set protection" })).toHaveCount(0)
  await expect(updatedSheet).not.toContainText("Approve the restock order?")
  await page.keyboard.press("Escape")

  // Switch to Eval sets, verify live details + actions, then withhold the selected set and click Refresh.
  await page.getByRole("tab", { name: "Eval sets" }).click()
  await expect(page.locator('[data-selected-eval-set="eval_credit_draft_v2"]')).toBeVisible()
  await expect(page.getByRole("group", { name: "Eval set actions" })).toBeVisible()
  withheldSetIds.add("eval_credit_draft_v2")
  await page.getByRole("button", { name: "Refresh" }).click()

  const withheldPanel = page.locator('[data-eval-set-state="withheld"]')
  await expect(withheldPanel).toBeVisible()
  await expect(withheldPanel).toContainText("Withheld by cell policy")
  await expect(page.locator('[data-selected-eval-set="eval_credit_draft_v2"]')).toHaveCount(0)
  await expect(page.getByRole("group", { name: "Eval set actions" })).toHaveCount(0)
})

test("S3: deleting a selected eval set (including the last remaining eval set) keeps ActionSheet receipt and Undo mounted without jumping selection", async ({ page }) => {
  // Part 1: with 2 eval sets, deleting the selected set shows omitted state (no silent jump) and Undo restores it.
  await mountCell(page)
  await page.getByRole("tab", { name: "Eval sets" }).click()
  await expect(page.locator('[data-selected-eval-set="eval_credit_draft_v2"]')).toBeVisible()

  await page.getByRole("group", { name: "Eval set actions" }).getByRole("button", { name: "Delete eval set" }).click()
  await dialog(page).getByRole("button", { name: "Delete eval set", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Yes, delete eval set" }).click()
  await expect(dialog(page).getByRole("status")).toBeVisible()
  // Behind the open ActionSheet, the detail panel reports omitted rather than switching to eval_credit_holdout_v1.
  await expect(page.locator('[data-eval-set-state="omitted"]')).toBeVisible()
  await expect(page.locator('[data-selected-eval-set]')).toHaveCount(0)
  // Server-offered Undo stays mounted and restores the deleted set.
  await dialog(page).getByRole("button", { name: "Undo" }).click()
  await expect(dialog(page).getByText(/^Undone · /)).toBeVisible()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.locator('[data-selected-eval-set="eval_credit_draft_v2"]')).toBeVisible()

  // Part 2: deleting the LAST remaining eval set empties the list to "No eval sets yet",
  // yet ActionSheet stays mounted with its receipt and server-offered Undo.
  await page.unrouteAll({ behavior: "ignoreErrors" })
  await mountCell(page, { initialEvalSets: [fixture.evalSets[0]!] })
  await page.getByRole("tab", { name: "Eval sets" }).click()
  await expect(page.locator('[data-selected-eval-set="eval_credit_draft_v2"]')).toBeVisible()

  await page.getByRole("group", { name: "Eval set actions" }).getByRole("button", { name: "Delete eval set" }).click()
  await dialog(page).getByRole("button", { name: "Delete eval set", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Yes, delete eval set" }).click()
  await expect(dialog(page).getByRole("status")).toBeVisible()
  // The list behind the dialog emptied completely to "No eval sets yet", with zero stale details.
  await expect(page.locator("text=No eval sets yet")).toBeVisible()
  await expect(page.locator('[data-selected-eval-set]')).toHaveCount(0)
  // ActionSheet remained mounted and its Undo restores the last set.
  await dialog(page).getByRole("button", { name: "Undo" }).click()
  await expect(dialog(page).getByText(/^Undone · /)).toBeVisible()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("list", { name: "Eval sets list" })).toContainText("Credit release regression")
})

test("S4: cursor pagination pages through curated cases and eval sets and opens page-2 items", async ({ page }) => {
  const { caseQueries, evalSetQueries } = await mountCell(page, { pageSize: 1 })
  const table = page.getByRole("table", { name: "Curated cases" })

  // Curated cases: page 1 shows the first case, Next requests `after=cases:1` and shows page 2.
  await expect(table.locator("tbody tr")).toHaveCount(1)
  await expect(table).toContainText("Release the held shipment?")
  const casesNav = page.getByRole("navigation", { name: "Curated cases pages" })
  await expect(casesNav.getByRole("button", { name: "Previous" })).toBeDisabled()
  await casesNav.getByRole("button", { name: "Next" }).click()
  await expect(casesNav).toContainText("Page 2")
  await expect(table).toContainText("Approve the restock order?")
  expect(caseQueries.some((u) => u.searchParams.get("after") === "cases:1")).toBe(true)

  // Open the page-2 curated case in the detail sheet.
  await table.getByRole("button", { name: "Approve the restock order?" }).click()
  await expect(page.getByRole("dialog", { name: "Approve the restock order?" })).toContainText("case_credit_overdue_02")
  await page.keyboard.press("Escape")

  // Eval sets: page 1 shows the first set, Next requests `after=sets:1` and selects page-2 set.
  await page.getByRole("tab", { name: "Eval sets" }).click()
  const setsList = page.getByRole("list", { name: "Eval sets list" })
  await expect(setsList).toContainText("Credit release regression")
  await expect(setsList).not.toContainText("Credit release holdout")
  const setsNav = page.getByRole("navigation", { name: "Eval sets pages" })
  await setsNav.getByRole("button", { name: "Next" }).click()
  await expect(setsNav).toContainText("Page 2")
  await expect(setsList).toContainText("Credit release holdout")
  expect(evalSetQueries.some((u) => u.searchParams.get("after") === "sets:1")).toBe(true)
  await expect(page.locator('[data-selected-eval-set="eval_credit_holdout_v1"]')).toBeVisible()
  await expect(page.getByRole("group", { name: "Eval set actions" }).getByRole("button", { name: "New version" })).toBeVisible()
})

test("unserved: when the cell doesn't serve curated cases or eval sets, both tabs say so cleanly", async ({ page }) => {
  await mountCell(page, { unserved: true })
  await expect(page.getByText("This cell doesn't serve curated cases yet.")).toBeVisible()
  await page.getByRole("tab", { name: "Eval sets" }).click()
  await expect(page.getByText("This cell doesn't serve eval sets yet.")).toBeVisible()
})
