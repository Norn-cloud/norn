import { expect, test } from "bun:test"
import { PLAN_PATH, change } from "../src/apply.ts"
import type { Json } from "../src/canonical.ts"
import type { CheckerDiagnostic, RippleReport } from "../src/checker-abi.ts"
import { checkerDiagnostic, formatCliDiagnostic, formatWitnessLines } from "../src/cli-diagnostic.ts"
import { planDiff, renderPlan } from "../src/plan.ts"

function envelope(
  artifactId: string,
  kind: string,
  dependencies: ReadonlyArray<{ readonly artifactId: string; readonly kind: string; readonly version: string; readonly required: boolean }>,
  payload: Record<string, unknown>,
  version = "0.1.0",
): Json {
  return {
    irVersion: "0.1",
    artifactId,
    kind,
    namespace: artifactId.split(".")[0] ?? "demo",
    name: artifactId.split(".").pop() ?? artifactId,
    version,
    lifecycle: "draft",
    scope: { orgId: "org_demo", environmentId: "dev" },
    dependencies,
    provenance: { source: "authoring_sdk" },
    payload,
  } as unknown as Json
}

function dep(artifactId: string, kind: string) {
  return { artifactId, kind, version: "0.1.0", required: true }
}

function entity(artifactId: string, name: string, opts: { extends?: string; props?: Array<Record<string, unknown>>; deps?: ReturnType<typeof dep>[] } = {}): Json {
  return envelope(artifactId, "entity", opts.deps ?? [], {
    entityVersion: "2.0",
    objectType: {
      name,
      ...(opts.extends ? { extends: opts.extends } : {}),
      properties: opts.props ?? [{ id: "id", type: { kind: "text" } }],
    },
  })
}

function action(artifactId: string, actionId: string, entityArtifactId: string, objectKind: string): Json {
  return envelope(artifactId, "action", [dep(entityArtifactId, "entity")], {
    actionId,
    scopeRequired: ["orgId", "environmentId"],
    idempotent: true,
    ledgered: true,
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"], additionalProperties: false },
    outputSchema: { type: "object", properties: { ok: { type: "boolean" } }, required: ["ok"], additionalProperties: false },
    writes: [{ entityKind: objectKind, change: "modified" }],
    effectProgram: {
      kind: "upsert_input_object",
      objectIdField: "id",
      objectKind,
      entityArtifactId,
      entityArtifactVersion: "0.1.0",
    },
  })
}

function law(artifactId: string, id: string, subject: string, actionArtifactId: string): Json {
  return envelope(artifactId, "law", [dep(actionArtifactId, "action")], {
    lawVersion: "0.1",
    id,
    owner: "treasury",
    rationale: "Purity holds on every melt",
    subject,
    scope: { root: { kind: "entity", id: subject }, objectTypes: [subject] },
    modality: { kind: "always" },
    response: { kind: "action", artifactId: actionArtifactId },
    severity: "error",
    provenance: { source: "authoring_sdk" },
  })
}

function widget(artifactId: string, widgetId: string, actionArtifactId: string, actionId: string): Json {
  return envelope(artifactId, "widget", [dep(actionArtifactId, "action")], {
    widgetVersion: "0.1",
    widgetId,
    dataDependencies: [],
    viewStates: [{ stateId: "ready", kind: "ready" }],
    actionBindings: [{ bindingId: "melt_btn", actionId, label: "Melt", requiredPermissions: ["jewelry:melt"] }],
    permissions: [],
  })
}

test("planDiff computes exact multi-hop transitive ripples across core -> subtype -> action -> law -> widget", () => {
  const holdingBefore = entity("core.Holding", "core.Holding")
  const holdingAfter = entity("core.Holding", "core.Holding", {
    props: [
      { id: "id", type: { kind: "text" } },
      { id: "grams", type: { kind: "decimal", scale: 3 } },
    ],
  })
  const metal = entity("jewelry.Metal", "jewelry.Metal", {
    extends: "core.Holding",
    deps: [dep("core.Holding", "entity")],
  })
  const melt = action("jewelry.melt.action", "jewelry.melt", "jewelry.Metal", "jewelry.Metal")
  const purity = law("jewelry.purity.law", "jewelry.purity", "jewelry.Metal", "jewelry.melt.action")
  const page = widget("jewelry.melt.widget", "jewelry.melt_page", "jewelry.melt.action", "jewelry.melt")

  const remote = [holdingBefore, metal, melt, purity, page]
  const local = [holdingAfter, metal, melt, purity, page]

  const plan = planDiff(local, remote)
  expect(plan.rippleStatus).toBe("exact")
  expect(plan.rippleBudgetExceeded).toBeUndefined()
  expect(plan.changed.map((c) => c.artifactId)).toEqual(["core.Holding"])

  const byId = new Map(plan.ripples.map((r) => [r.artifactId, r]))
  expect(byId.get("core.Holding")).toEqual({
    artifactId: "core.Holding",
    kind: "entity",
    triggeredBy: ["core.Holding"],
    via: ["core.Holding"],
    impactKind: "direct_change",
  })
  expect(byId.get("jewelry.Metal")?.impactKind).toBe("specialization_affected")
  expect(byId.get("jewelry.Metal")?.via).toEqual(["core.Holding", "jewelry.Metal"])
  expect(byId.get("jewelry.melt.action")?.impactKind).toBe("writer_affected")
  expect(byId.get("jewelry.melt.action")?.via).toEqual(["core.Holding", "jewelry.Metal", "jewelry.melt.action"])
  expect(byId.get("jewelry.purity.law")?.impactKind).toBe("law_affected")
  expect(byId.get("jewelry.melt.widget")?.impactKind).toBe("projection_or_ui_affected")
  expect(byId.get("jewelry.melt.widget")?.via).toEqual([
    "core.Holding",
    "jewelry.Metal",
    "jewelry.melt.action",
    "jewelry.melt.widget",
  ])

  const rendered = renderPlan(plan)
  expect(rendered).toContain("transitive ripples (5):")
  expect(rendered).toContain("[specialization_affected] jewelry.Metal [entity]")
  expect(rendered).toContain("core.Holding -> jewelry.Metal -> jewelry.melt.action -> jewelry.melt.widget")
})

test("planDiff detects removed dependencies, survives cycles deterministically, and fails closed on budget exhaustion", () => {
  const item = entity("core.Item", "core.Item")
  const createItem = action("app.create_item", "app.create_item", "core.Item", "core.Item")
  const removedPlan = planDiff([createItem], [item, createItem])
  expect(removedPlan.rippleStatus).toBe("exact")
  const broken = removedPlan.ripples.find((r) => r.artifactId === "app.create_item")
  expect(broken?.impactKind).toBe("removed_dependency_broken")
  expect(broken?.via).toEqual(["core.Item", "app.create_item"])

  // Cycle safety & order independence
  const aBefore = entity("cycle.A", "cycle.A", { deps: [dep("cycle.B", "entity")] })
  const aAfter = entity("cycle.A", "cycle.A", {
    deps: [dep("cycle.B", "entity")],
    props: [{ id: "id", type: { kind: "text" } }, { id: "v", type: { kind: "integer" } }],
  })
  const b = entity("cycle.B", "cycle.B", { deps: [dep("cycle.A", "entity")] })
  const forward = planDiff([aAfter, b], [aBefore, b])
  const reversed = planDiff([b, aAfter], [b, aBefore])
  expect(forward.ripples).toEqual(reversed.ripples)

  // Budget exhaustion returns explicit unknown, never silently truncates
  const exhausted = planDiff([aAfter, b], [aBefore, b], { budget: { maxNodes: 1, maxEdges: 10, maxSteps: 100 } })
  expect(exhausted.rippleStatus).toBe("unknown")
  expect(exhausted.ripples).toEqual([])
  expect(exhausted.rippleBudgetExceeded?.resource).toBe("nodes")
  expect(renderPlan(exhausted)).toContain("transitive ripples: UNKNOWN")
})

test("planDiff delegates ripple computation to CheckerAbi.rippleEffects when supplied", () => {
  const aBefore = entity("core.A", "core.A")
  const aAfter = entity("core.A", "core.A", { props: [{ id: "id", type: { kind: "text" } }, { id: "n", type: { kind: "integer" } }] })
  let called = false
  const stubReport: RippleReport = {
    format: "norn-checker/ripple@1",
    status: "exact",
    roots: ["core.A"],
    ripples: [{ artifactId: "core.A", kind: "entity", triggeredBy: ["core.A"], via: ["core.A"], impactKind: "direct_change" }],
  }
  const plan = planDiff([aAfter], [aBefore], {
    checker: {
      rippleEffects: (before, after, changedIds) => {
        called = true
        expect(before).toHaveLength(1)
        expect(after).toHaveLength(1)
        expect(changedIds).toEqual(["core.A"])
        return stubReport
      },
    },
  })
  expect(called).toBe(true)
  expect(plan.ripples).toEqual(stubReport.ripples)
})

test("witnesses are preserved in JSON and formatted cleanly in CLI text across all counterexample shapes", async () => {
  // 1. Composed lifecycle/UI step witness
  const stepFinding: CheckerDiagnostic = {
    code: "lifecycle.soundness.trap",
    severity: "error",
    message: "state `stuck` has no path to an ending",
    artifactId: "app.Order",
    span: { pointer: "/payload/objectType/lifecycles/0" },
    witness: {
      steps: [
        { edge: 0, moves: [{ side: "ui", label: "submit" }, { side: "backend", label: "open" }] },
        { edge: 2, moves: [{ side: "backend", label: "freeze" }] },
      ],
    },
  }
  const v2 = checkerDiagnostic(stepFinding)
  expect(v2.witness).toEqual(stepFinding.witness)
  expect(JSON.parse(JSON.stringify(v2)).witness).toEqual(stepFinding.witness)
  const rendered = formatCliDiagnostic(v2)
  expect(rendered).toContain("witness: edge 0 [ui:submit, backend:open] -> edge 2 [backend:freeze]")

  // 2. Decision table overlap witness, chase conflict witness, law-blocked exit witness, red-team provenance witness
  expect(
    formatWitnessLines({ input: { tier: "gold", amount: 500 }, matchingRows: ["r1", "r2"], selectedRows: ["r1"] })[0],
  ).toContain("input={\"tier\":\"gold\",\"amount\":500}; matchingRows=[r1, r2]; selectedRows=[r1]")
  expect(
    formatWitnessLines({ rules: ["eq.parent"], facts: [["Order", "o1", "a"], ["Order", "o1", "b"]] })[0],
  ).toContain("rules=[eq.parent]; facts=[[\"Order\",\"o1\",\"a\"],[\"Order\",\"o1\",\"b\"]]")
  expect(
    formatWitnessLines({ state: "promised", run: ["create", "confirm"], blocked: ["settle", ["law.balance"]] })[0],
  ).toContain("state=promised; run=create -> confirm; blocked=[\"settle\",[\"law.balance\"]]")
  expect(
    formatWitnessLines({ role: "clerk", actionId: "vault.approve", objectType: "vault.Lot", transitionId: "approve" })[0],
  ).toContain("role=clerk; actionId=vault.approve; objectType=vault.Lot; transitionId=approve")

  // 3. Hosted plan/refusal formatting surfaces witnesses & ripples in text and keeps them in JSON
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      await request.text()
      if (url.pathname.endsWith(PLAN_PATH)) {
        return Response.json(
          {
            code: "defs.refused",
            message: "not admissible",
            explanation: "counterexample found",
            diagnostics: [
              {
                severity: "error",
                code: "lifecycle.soundness.law_blocked_exit",
                message: "exit blocked by law",
                span: { pointer: "/artifacts/0" },
                witness: { steps: [{ edge: 1, moves: [{ side: "backend", label: "approve" }] }] },
              },
            ],
          },
          { status: 422 },
        )
      }
      return Response.json({}, { status: 404 })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "o", env: "e", headers: {} }
    const textResult = await change(target, "plan", [], false)
    expect(textResult.ok).toBe(false)
    expect(textResult.out.join("\n")).toContain("witness: edge 1 [backend:approve]")

    const jsonResult = await change(target, "plan", [], true)
    expect(jsonResult.ok).toBe(false)
    const parsed = JSON.parse(jsonResult.out[0]!) as { diagnostics: Array<{ witness?: unknown }> }
    expect(parsed.diagnostics[0]?.witness).toEqual({
      steps: [{ edge: 1, moves: [{ side: "backend", label: "approve" }] }],
    })
  } finally {
    server.stop(true)
  }
})

test("formatWitnessLines preserves unrecognized keys in deterministic lexical order alongside known fields", () => {
  const mixed = formatWitnessLines({
    state: "frozen",
    zetaDetail: 42,
    alphaContext: { expected: "open" },
    BetaFlag: true,
  })
  expect(mixed).toEqual([
    '  witness: state=frozen; BetaFlag=true; alphaContext={"expected":"open"}; zetaDetail=42',
  ])

  const stepsWithExtras = formatWitnessLines({
    steps: [{ edge: 0, moves: [{ side: "backend", label: "close" }] }],
    extraNote: "invariant_failed",
    Attempt: 2,
  })
  expect(stepsWithExtras).toEqual([
    '  witness: edge 0 [backend:close]; Attempt=2; extraNote="invariant_failed"',
  ])
})

test("planDiff fallback projects pathMappings, law-interpretation layers, code-point lexical order, and fails closed on tight budget or projection errors", () => {
  const srcSchema = envelope("src.erp.schema", "source_schema", [], { schemaVersion: "0.1" })
  const srcEntityBefore = entity("erp.Invoice", "erp.Invoice")
  const srcEntityAfter = entity("erp.Invoice", "erp.Invoice", {
    props: [{ id: "id", type: { kind: "text" } }, { id: "total", type: { kind: "decimal", scale: 2 } }],
  })
  const tgtEntity = entity("ledger.Entry", "ledger.Entry")
  const pathMapping = envelope("erp.invoice.mapping", "mapping", [], {
    format: "norn.mapping/1",
    source: { schemaId: "src.erp.schema" },
    maps: [],
    pathMappings: [{ sourceEntities: ["erp.Invoice"], targetEntities: ["ledger.Entry"] }],
  })
  const interpMapping = envelope("erp.to_ledger.interp", "mapping", [], {
    format: "norn.interpretation/1",
    sourceLayer: { artifactId: "erp.Invoice" },
    targetLayer: { artifactId: "ledger.Entry" },
  })

  const mappingPlan = planDiff(
    [srcSchema, srcEntityAfter, tgtEntity, pathMapping, interpMapping],
    [srcSchema, srcEntityBefore, tgtEntity, pathMapping, interpMapping],
  )
  expect(mappingPlan.rippleStatus).toBe("exact")
  const mappingIds = mappingPlan.ripples.map((r) => r.artifactId)
  expect(mappingIds).toContain("erp.invoice.mapping")
  expect(mappingIds).toContain("erp.to_ledger.interp")

  // Code-point lexical ordering: uppercase ASCII ('Z') sorts before lowercase ('a'), unlike localeCompare.
  const baseBefore = entity("lex.Root", "lex.Root")
  const baseAfter = entity("lex.Root", "lex.Root", {
    props: [{ id: "id", type: { kind: "text" } }, { id: "k", type: { kind: "text" } }],
  })
  const upperDep = entity("Z.Child", "Z.Child", { extends: "lex.Root", deps: [dep("lex.Root", "entity")] })
  const lowerDep = entity("a.Child", "a.Child", { extends: "lex.Root", deps: [dep("lex.Root", "entity")] })
  const lexPlan = planDiff([baseAfter, lowerDep, upperDep], [baseBefore, lowerDep, upperDep])
  expect(lexPlan.rippleStatus).toBe("exact")
  expect(lexPlan.ripples.map((r) => r.artifactId)).toEqual(["lex.Root", "Z.Child", "a.Child"])

  // Tight step budget during projection fails closed to unknown (never claims exact).
  const tightSteps = planDiff(
    [baseAfter, lowerDep, upperDep],
    [baseBefore, lowerDep, upperDep],
    { budget: { maxNodes: 100, maxEdges: 100, maxSteps: 2 } },
  )
  expect(tightSteps.rippleStatus).toBe("unknown")
  expect(tightSteps.ripples).toEqual([])
  expect(tightSteps.rippleBudgetExceeded?.resource).toBe("steps")

  // Malformed/unsupported artifact payload fails closed to unknown rather than silently dropping edges.
  const malformed = envelope("bad.Entity", "entity", [dep("lex.Root", "entity")], { bogus: true })
  const malformedPlan = planDiff([baseAfter, malformed], [baseBefore, malformed])
  expect(malformedPlan.rippleStatus).toBe("unknown")
  expect(malformedPlan.ripples).toEqual([])
  expect(malformedPlan.rippleBudgetExceeded).toBeUndefined()
  expect(malformedPlan.rippleReason).toContain("bad.Entity")
  expect(renderPlan(malformedPlan)).toContain("transitive ripples: UNKNOWN")
})

test("hosted plan summary renders nested DefinitionsPlan.ripples object for both exact and unknown reports", async () => {
  let mode: "exact" | "unknown" = "exact"
  const server = Bun.serve({
    port: 0,
    async fetch(request) {
      await request.text()
      return Response.json({
        baseVersion: 1,
        nextVersion: 2,
        unchanged: false,
        added: [],
        changed: [{ artifactId: "core.Holding", kind: "entity", fromVersion: "0.1.0", toVersion: "0.2.0" }],
        same: [],
        newObjectTypes: [],
        checkedObjects: 0,
        diagnostics: [],
        receiptSha256: "abc",
        ripples: mode === "exact"
          ? {
              status: "exact",
              roots: ["core.Holding"],
              ripples: [
                {
                  artifactId: "jewelry.Metal",
                  kind: "entity",
                  triggeredBy: ["core.Holding"],
                  via: ["core.Holding", "jewelry.Metal"],
                  impactKind: "specialization_affected",
                },
              ],
            }
          : {
              status: "unknown",
              roots: ["core.Holding"],
              ripples: [],
              reason: "ripple graph steps budget (10) exceeded",
              budgetExceeded: { resource: "steps", limit: 10, detail: "ripple graph steps budget (10) exceeded" },
            },
      })
    },
  })
  try {
    const target = { url: `http://127.0.0.1:${server.port}/api/host`, org: "o", env: "e", headers: {} }
    const exactOut = await change(target, "plan", [], false)
    expect(exactOut.ok).toBe(true)
    expect(exactOut.out.join("\n")).toContain("transitive ripples (1):")
    expect(exactOut.out.join("\n")).toContain("[specialization_affected] jewelry.Metal [entity] via core.Holding -> jewelry.Metal")

    mode = "unknown"
    const unknownOut = await change(target, "plan", [], false)
    expect(unknownOut.ok).toBe(true)
    expect(unknownOut.out.join("\n")).toContain("ripples: UNKNOWN (ripple graph steps budget (10) exceeded)")
  } finally {
    server.stop(true)
  }
})
