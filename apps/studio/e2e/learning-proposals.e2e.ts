import { readFileSync } from "node:fs"
import { expect, test, type Page } from "@playwright/test"
import { session } from "./object-cell.ts"

// Proposal review and replay table island (#1220 L-5): server-filtered proposal list, draft/edit/
// withdraw/undo, submit + replay table showing past cases a tightened numeric limit would have
// refused, protected-case admission refusal, and ratify → Ruling receipt.

const fixture = JSON.parse(readFileSync(new URL("../test/fixtures/learning-proposals.json", import.meta.url), "utf8")) as {
  readonly records: { id: string; objectType: string; scope: string; version: number; values: Record<string, unknown> }[]
}
const served = JSON.parse(readFileSync(new URL("../test/fixtures/action-call.json", import.meta.url), "utf8")) as {
  readonly committed: { readonly receipt: object }
}

const DRAFT_PROPOSAL = {
  actionId: "learning.draft_gap_proposal",
  title: "Draft proposal",
  description: "Draft a definition diff citing decision episodes.",
  objectKind: "learning.GapProposal",
  effect: "create_object",
  objectIdField: "proposalId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["target", "title", "summary"],
    properties: {
      target: { type: "string", title: "Target artifact" },
      title: { type: "string", title: "Title" },
      summary: { type: "string", title: "Summary" },
    },
  },
}

const EDIT_PROPOSAL = {
  actionId: "learning.edit_gap_proposal",
  title: "Edit draft",
  description: "Update the title or summary of a draft proposal.",
  objectKind: "learning.GapProposal",
  effect: "fire_transition",
  objectIdField: "proposalId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["proposalId", "title"],
    properties: {
      proposalId: { type: "string", title: "Proposal" },
      title: { type: "string", title: "Title" },
    },
  },
}

const SUBMIT_PROPOSAL = {
  actionId: "learning.submit_gap_proposal",
  title: "Submit for replay",
  description: "Run the checker and replay curated cases against the proposed definition.",
  objectKind: "learning.GapProposal",
  effect: "fire_transition",
  objectIdField: "proposalId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["proposalId"],
    properties: { proposalId: { type: "string", title: "Proposal" } },
  },
}

const WITHDRAW_PROPOSAL = {
  actionId: "learning.withdraw_gap_proposal",
  title: "Withdraw",
  description: "Withdraw this proposal without ratifying it.",
  objectKind: "learning.GapProposal",
  effect: "fire_transition",
  objectIdField: "proposalId",
  uiEffects: { irreversible: false, destructive: true, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["proposalId"],
    properties: { proposalId: { type: "string", title: "Proposal" } },
  },
}

const REOPEN_PROPOSAL = {
  actionId: "learning.reopen_gap_proposal",
  title: "Reopen draft",
  description: "Return a withdrawn proposal to draft status.",
  objectKind: "learning.GapProposal",
  effect: "fire_transition",
  objectIdField: "proposalId",
  uiEffects: { irreversible: false, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["proposalId"],
    properties: { proposalId: { type: "string", title: "Proposal" } },
  },
}

const RATIFY_PROPOSAL = {
  actionId: "learning.ratify_gap_proposal",
  title: "Ratify proposal",
  description: "Commit a Ruling receipt admitting the proposed definition.",
  objectKind: "learning.GapProposal",
  effect: "fire_transition",
  objectIdField: "proposalId",
  uiEffects: { irreversible: true, destructive: false, money: [] },
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["proposalId"],
    properties: { proposalId: { type: "string", title: "Proposal" } },
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
  test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the learning gap-proposal stand-in`)
})

async function mountCell(page: Page, options: { mode?: "fixture" | "unserved" | "malformed-replay" } = {}) {
  await session(page)
  const mode = options.mode ?? "fixture"
  const proposals = structuredClone(fixture.records)
  const queries: URL[] = []
  let seq = 9000

  await page.route("**/api/host/v1/objects/learning.GapProposal**", (route) => {
    const url = new URL(route.request().url())
    const suffix = url.pathname.split("/v1/objects/learning.GapProposal")[1] ?? ""
    if (suffix.startsWith("/") && suffix.length > 1) {
      if (mode === "unserved") return route.fulfill({ status: 404, body: "not found" })
      const id = decodeURIComponent(suffix.slice(1))
      const found = proposals.find((p) => p.id === id)
      if (!found) return route.fulfill({ status: 404, json: { code: "object.not_found", message: "No such proposal." } })
      if (mode === "malformed-replay" && found.values["replay"]) {
        const broken = structuredClone(found)
        ;(broken.values["replay"] as Record<string, unknown>)["admissionStatus"] = "admitted"
        return route.fulfill({ json: broken })
      }
      return route.fulfill({ json: found })
    }
    if (mode === "unserved") return route.fulfill({ status: 404, json: { code: "object.type_unknown", message: "Not served" } })
    queries.push(url)
    const rawFilter = url.searchParams.get("filter")
    const filter = rawFilter ? (JSON.parse(rawFilter) as { all?: { property: string; op: string; value?: unknown }[] }) : null
    const statusClause = filter?.all?.find((c) => c.property === "status" && c.op === "eq")
    const filtered = statusClause ? proposals.filter((p) => p.values["status"] === statusClause.value) : proposals
    return route.fulfill({
      json: {
        objectType: "learning.GapProposal",
        records: filtered,
        query: { sort: [], filter },
        total: { count: filtered.length, exact: true },
        next: null,
        quarantineCount: 0,
      },
    })
  })

  await page.route("**/api/host/v1/actions**", (route) => {
    if (mode === "unserved") return route.fulfill({ status: 404, body: "not found" })
    const request = route.request()
    const url = new URL(request.url())
    if (request.method() === "GET") {
      const objectId = url.searchParams.get("objectId")
      if (!objectId) return route.fulfill({ json: { items: [DRAFT_PROPOSAL] } })
      const found = proposals.find((p) => p.id === objectId)
      const status = found?.values["status"]
      const items = status === "draft"
        ? [EDIT_PROPOSAL, SUBMIT_PROPOSAL, WITHDRAW_PROPOSAL]
        : status === "admitted"
          ? [RATIFY_PROPOSAL, WITHDRAW_PROPOSAL]
          : status === "refused"
            ? [WITHDRAW_PROPOSAL]
            : []
      return route.fulfill({ json: { items } })
    }

    const actionId = decodeURIComponent(url.pathname.split("/v1/actions/")[1] ?? "")
    const { input } = JSON.parse(request.postData() ?? "{}") as { input: Record<string, unknown> }
    seq += 1

    if (actionId === DRAFT_PROPOSAL.actionId) {
      const created = {
        objectType: "learning.GapProposal",
        id: `prop_new_${seq}`,
        scope: "org_demo/analytics",
        version: 1,
        values: {
          title: String(input["title"]),
          summary: String(input["summary"]),
          status: "draft",
          target: { artifactId: String(input["target"]), kind: "law", label: null },
          diff: {
            summary: String(input["summary"]),
            before: "limit <= 0.20",
            after: "limit <= 0.15",
            patch: "--- a/law\n+++ b/law\n-  limit: 0.20\n+  limit: 0.15",
          },
          episodeIds: ["ep_demo_overridden"],
          author: "you",
          createdAtMs: Date.now(),
          updatedAtMs: Date.now(),
          checker: { status: "not_run", diagnostics: [] },
          replay: null,
          ruling: null,
          supersedesProposalId: null,
        },
      }
      proposals.unshift(created)
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.GapProposal", objectId: created.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    const target = proposals.find((p) => p.id === input["proposalId"])!
    if (actionId === EDIT_PROPOSAL.actionId) {
      target.values["title"] = String(input["title"])
      target.values["updatedAtMs"] = Date.now()
      target.version += 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.GapProposal", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === SUBMIT_PROPOSAL.actionId) {
      target.values["status"] = "admitted"
      target.values["checker"] = { status: "admitted", diagnostics: [] }
      target.values["replay"] = {
        evalSetId: "eval_credit_holdout_v1",
        evalSetName: "Credit release holdout",
        evalSetVersion: 1,
        definitionPin: "sha256:9999000011112222333344445555666677778888999900001111222233334444",
        replayedAtMs: Date.now(),
        totalCases: 1,
        flippedCount: 0,
        protectedFlippedCount: 0,
        admissionStatus: "admitted",
        refusalReason: null,
        cases: [{
          caseId: "case_credit_phone_01",
          episodeId: "ep_demo_overridden",
          label: "Release the held shipment? (phone-confirmed payment)",
          protected: true,
          supersededByRuling: false,
          beforeOutcome: "allowed",
          afterOutcome: "allowed",
          flipped: false,
          detail: "Remittance path matches protected case.",
        }],
      }
      target.version += 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.GapProposal", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === WITHDRAW_PROPOSAL.actionId) {
      target.values["status"] = "withdrawn"
      target.version += 1
      return route.fulfill({
        json: {
          ...served.committed,
          objectType: "learning.GapProposal",
          objectId: target.id,
          receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() },
          undo: { actionId: REOPEN_PROPOSAL.actionId, input: { proposalId: target.id } },
        },
      })
    }

    if (actionId === REOPEN_PROPOSAL.actionId) {
      target.values["status"] = "draft"
      target.version += 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.GapProposal", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    if (actionId === RATIFY_PROPOSAL.actionId) {
      target.values["status"] = "ratified"
      target.values["ruling"] = {
        receiptHash: "sha256:efef111122223333444455556666777788889999aaaabbbbccccddddeeeeffff",
        commitSeq: seq,
        ratifiedBy: "you",
        ratifiedAtMs: Date.now(),
        supersededCaseIds: [],
      }
      target.version += 1
      return route.fulfill({
        json: { ...served.committed, objectType: "learning.GapProposal", objectId: target.id, receipt: { ...served.committed.receipt, commitSeq: seq, recordedAtMs: Date.now() } },
      })
    }

    return route.fulfill({ status: 404, json: { code: "action.not_found", message: "Unknown action" } })
  })

  return { queries }
}

const dialog = (page: Page) => page.getByRole("dialog").last()

test("proposal replay table: shows past cases a tightened limit would have refused and protected flips blocking admission", async ({ page }) => {
  await mountCell(page)
  await page.goto("/learning/proposals/prop_demo_credit_tighten")

  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Tighten credit over-limit threshold from 20% to 10%")
  await expect(page.getByText("credit_overage_limit <= 0.20")).toBeVisible()
  await expect(page.getByText("credit_overage_limit <= 0.10")).toBeVisible()

  // Refusal banner explains the flipped protected case.
  const refusal = page.locator('[data-replay-refusal="true"]')
  await expect(refusal).toContainText("Admission refused by replay")
  await expect(refusal).toContainText("case_credit_phone_01")

  // Replay table lists all 4 cases, sorting blocking protected flips first.
  const replayTable = page.getByRole("table", { name: "Replayed cases" })
  const rows = replayTable.locator("tbody tr")
  await expect(rows).toHaveCount(4)
  await expect(rows.nth(0)).toHaveAttribute("data-replay-case", "case_credit_phone_01")
  await expect(rows.nth(0)).toHaveAttribute("data-protected-flip", "true")
  await expect(rows.nth(0)).toContainText("Blocks admission")
  await expect(rows.nth(0)).toContainText("Allowed")
  await expect(rows.nth(0)).toContainText("Would refuse")
  await expect(rows.nth(1)).toHaveAttribute("data-replay-case", "case_credit_standard_03")
  await expect(rows.nth(1)).toContainText("Flipped")

  await shoot(page, "proposal-replay")

  // Toggle filters narrow the replay table cleanly.
  await page.getByRole("radio", { name: "Flipped (2)" }).click()
  await expect(rows).toHaveCount(2)
  await page.getByRole("radio", { name: "Protected flipped (1)" }).click()
  await expect(rows).toHaveCount(1)
  await expect(rows.nth(0)).toHaveAttribute("data-replay-case", "case_credit_phone_01")

  // Cell offers Withdraw on a refused proposal, NOT Ratify.
  const actions = page.getByRole("group", { name: "Proposal actions" })
  await expect(actions.getByRole("button", { name: "Withdraw" })).toBeVisible()
  await expect(actions.getByRole("button", { name: "Ratify proposal" })).toHaveCount(0)
})

test("proposals lifecycle: server-filter list, draft, edit, withdraw + undo, submit for replay, and ratify → Ruling receipt", async ({ page }) => {
  const { queries } = await mountCell(page)
  await page.goto("/learning/proposals")

  const table = page.getByRole("table", { name: "Gap proposals" })
  await expect(table).toContainText("Tighten credit over-limit threshold from 20% to 10%")
  await expect(table).toContainText("Tighten auto-approved discount ceiling from 15% to 12%")
  await shoot(page, "proposals-list")

  // Server-side status filter.
  await page.getByLabel("Filter proposals by status").selectOption("refused")
  await expect(page.getByText("Applied by cell: Admission refused")).toBeVisible()
  await expect(table.locator("tbody tr")).toHaveCount(1)
  expect(queries.some((u) => (u.searchParams.get("filter") ?? "").includes('"refused"'))).toBe(true)
  await page.getByLabel("Filter proposals by status").selectOption("")

  // Draft a new proposal from the list toolbar.
  await page.getByRole("button", { name: "Draft proposal" }).click()
  await dialog(page).getByLabel("Target artifact").fill("demo.credit.hold_over_limit.law")
  await dialog(page).getByLabel("Title").fill("Add remittance grace window")
  await dialog(page).getByLabel("Summary").fill("Reduce manual overrides on confirmed remittances.")
  await dialog(page).getByRole("button", { name: "Draft proposal", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await table.getByRole("link", { name: "Add remittance grace window" }).click()
  await expect(page.getByText("Not replayed yet")).toBeVisible()

  // Edit draft, withdraw + Undo (returns to draft), then submit → replay populates.
  const actions = page.getByRole("group", { name: "Proposal actions" })
  await actions.getByRole("button", { name: "Edit draft" }).click()
  await dialog(page).getByLabel("Title").fill("Add 48h remittance grace window")
  await dialog(page).getByRole("button", { name: "Edit draft", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Add 48h remittance grace window")

  await actions.getByRole("button", { name: "Withdraw" }).click()
  await dialog(page).getByRole("button", { name: "Withdraw", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Yes, withdraw" }).click()
  await expect(dialog(page).getByRole("status")).toBeVisible()
  await dialog(page).getByRole("button", { name: "Undo" }).click()
  await expect(dialog(page).getByText(/^Undone · /)).toBeVisible()
  await dialog(page).getByRole("button", { name: "Done" }).click()

  await actions.getByRole("button", { name: "Submit for replay" }).click()
  await dialog(page).getByRole("button", { name: "Submit for replay", exact: true }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("table", { name: "Replayed cases" })).toContainText("case_credit_phone_01")

  // Ratify the admitted proposal (irreversible confirm) → Ruling receipt appears.
  await actions.getByRole("button", { name: "Ratify proposal" }).click()
  await dialog(page).getByRole("button", { name: "Ratify proposal", exact: true }).click()
  await expect(dialog(page).getByRole("alertdialog")).toContainText("This can't be undone.")
  await dialog(page).getByRole("button", { name: "Yes, ratify proposal" }).click()
  await dialog(page).getByRole("button", { name: "Done" }).click()
  await expect(page.getByRole("complementary", { name: "Proposal details" })).toContainText("Ratified by")
  await expect(page.getByRole("complementary", { name: "Proposal details" })).toContainText("sha256:efef111")
})

test("proposals: fail-closed when replay claims admitted with a flipped protected case, and unserved state says so", async ({ page }) => {
  await mountCell(page, { mode: "malformed-replay" })
  await page.goto("/learning/proposals/prop_demo_credit_tighten")
  await expect(page.getByText("Can't show this proposal")).toBeVisible()
  await expect(page.getByText("cannot be admitted when a protected case flips")).toBeVisible()

  await page.unrouteAll({ behavior: "ignoreErrors" })
  await mountCell(page, { mode: "unserved" })
  await page.goto("/learning/proposals")
  await expect(page.getByText("This cell doesn't serve gap proposals yet.")).toBeVisible()
})
