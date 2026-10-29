import { describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import type { FlowDef, Step } from "../src/builder/index.ts"
import { X, flow, route, succeed, lowerFlow } from "../src/builder/index.ts"
import { canonicalJson, toJson } from "../src/canonical.ts"
import { compileFile, compileFiles } from "../src/compiler/index.ts"
import { flowSourceMapGraph } from "../src/flow-source-map.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const artifactIdForTest = (artifact: unknown): string => (artifact as { readonly artifactId: string }).artifactId
const FLOWS = join(TOOL_ROOT, "fixtures/flows")
const names = ["sequence", "route", "typed-failure", "retry-timer", "terminal-fact"]
const options = (name: string) => ({
  scope: { orgId: "org_demo", environmentId: "dev" },
  sourcePath: `tools/norn-ts/fixtures/flows/${name}.flow.ts`,
  sourceRoot: join(TOOL_ROOT, "..", ".."),
})
const twins = new Map<string, FlowDef>()
for (const name of names) {
  twins.set(name, ((await import(join(FLOWS, `${name}.builder.ts`))) as { default: FlowDef }).default)
}

describe("compiler builder equivalence", () => {
  for (const name of names) {
    test(`${name}: source syntax becomes identical canonical checked IR`, () => {
      const file = join(FLOWS, `${name}.source.ts`)
      const compiled = compileFile(file, options(name))
      if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
      const built = lowerFlow(twins.get(name)!, options(name))
      if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
      const json = canonicalJson(compiled.artifacts)
      expect(json).toBe(canonicalJson(built.artifacts))
      expect(json).toBe(readFileSync(join(FLOWS, `${name}.ir.json`), "utf8"))
      expect(JSON.parse(json)).toEqual(toJson(compiled.artifacts))
      expect(() => toJson(compiled.definition)).not.toThrow()
      const map = compiled.sourceMap as unknown as { steps: Record<string, { file: string; line: number; column: number }> }
      const lines = readFileSync(file, "utf8").split("\n")
      for (const [step, at] of Object.entries(map.steps)) {
        expect(at.file).toBe(`fixtures/flows/${name}.source.ts`)
        expect(at.column).toBeGreaterThan(0)
        expect(lines[at.line - 1]).toContain(step)
      }
      const again = compileFile(file, options(name))
      if (!again.ok) throw new Error(JSON.stringify(again.diagnostics))
      expect(canonicalJson(again.artifacts)).toBe(json)
      expect(canonicalJson(again.sourceMap)).toBe(canonicalJson(compiled.sourceMap))
      if (checker) {
        const admitted = checker.checkArtifactSet(compiled.artifacts)
        expect(admitted.diagnostics.diagnostics).toEqual([])
        expect(admitted.admitted).toBe(true)
      }
    })
  }
})

const rejectDir = join(TOOL_ROOT, "corpus/reject")
const rejected = readdirSync(rejectDir).filter((f) => f.endsWith(".ts")).sort()
const rejections = compileFiles(rejected.map((f) => join(rejectDir, f)), options("corpus"))

describe("compiler preserves the whole profile rejection corpus", () => {
  for (let i = 0; i < rejected.length; i++) {
    const file = rejected[i]!
    test(file, () => {
      const result = rejections[i]!
      expect(result.ok).toBe(false)
      if (result.ok) return
      const actual = result.diagnostics.map((d) => ({
        code: d.code,
        start: `${d.start.line}:${d.start.column}`,
        end: `${d.end.line}:${d.end.column}`,
        construct: d.construct,
      }))
      const golden = JSON.parse(readFileSync(join(rejectDir, file.replace(/\.ts$/, ".expected.json")), "utf8")) as { diagnostics: typeof actual }
      expect(actual).toEqual(golden.diagnostics)
    })
  }
})


describe("compiler composition", () => {
  for (const name of ["composition", "branch-continuation", "caught-continuation", "nested-projection", "pipe-catch-map", "pipe-flat-map"]) {
    test(`${name}: tuples, inline continuations and branch values cross a durable boundary`, () => {
      const file = join(TOOL_ROOT, "fixtures/compiler", `${name}.source.ts`)
      const result = compileFile(file, options(name))
      if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
      expect(() => toJson(result.definition)).not.toThrow()
      if (checker) {
        const admitted = checker.checkArtifactSet(result.artifacts)
        expect(admitted.diagnostics.diagnostics).toEqual([])
        expect(admitted.admitted).toBe(true)
      }
    })
  }
})

test("flow source map projects every authoring construct and resolves exact lowered ids", () => {
  const file = join(TOOL_ROOT, "fixtures/compiler/flow-source-map.source.ts")
  const sourceOptions = { ...options("flow-source-map"), sourcePath: "tools/norn-ts/fixtures/compiler/flow-source-map.source.ts" }
  const compiled = compileFile(file, sourceOptions)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const sidecar = compiled.flowSourceMap
  expect(sidecar?.format).toBe("norn-flow-source-map-draft/1")
  expect(sidecar?.flows).toHaveLength(1)
  const flow = sidecar!.flows[0]!
  expect(flow.sourcePath).toBe(sourceOptions.sourcePath)
  expect(flow.artifactRefs).toEqual([])
  expect(flow.steps.map((step) => step.kind)).toEqual(expect.arrayContaining([
    "perform", "retry", "sleep", "all", "switch", "if", "fail", "catchTag", "compose", "succeed",
  ]))
  expect(flow.steps.every((step) => step.at != null && step.at.file === "tools/norn-ts/fixtures/compiler/flow-source-map.source.ts" && step.at.line > 0 && (step.at.column ?? 0) > 0)).toBe(true)

  const retry = flow.steps.find((step) => step.kind === "retry")!
  expect(flow.edges).toContainEqual({ from: retry.id, to: "step_perform_charge", kind: "retry" })
  const all = flow.steps.find((step) => step.kind === "all")!
  expect(flow.edges).toContainEqual({ from: all.id, to: "step_perform_parallel_notice", kind: "contains", label: "steps" })
  const catcher = flow.steps.find((step) => step.kind === "catchTag")!
  expect(flow.edges).toContainEqual({ from: catcher.id, to: "step_perform_load", kind: "contains", label: "body" })
  expect(flow.edges).toContainEqual({ from: catcher.id, to: "step_perform_declined_notice", kind: "contains", label: "handler" })
  expect(flow.edges).toContainEqual({ from: catcher.id, to: "step_perform_declined_notice", kind: "catch", label: "PaymentDeclined" })
  const ifStep = flow.steps.find((step) => step.kind === "if")!
  expect(flow.edges.filter((candidate) => candidate.from === ifStep.id && candidate.kind === "branch")).toEqual([
    { from: ifStep.id, to: "step_perform_overdue_notice", kind: "branch", label: "then" },
    { from: ifStep.id, to: flow.steps.find((step) => step.kind === "fail")!.id, kind: "branch", label: "else" },
  ])
  const switchStep = flow.steps.find((step) => step.kind === "switch")!
  expect(flow.edges.filter((candidate) => candidate.from === switchStep.id && candidate.kind === "branch")).toEqual([
    { from: switchStep.id, to: "step_perform_paid_notice", kind: "branch", label: "case-1" },
    { from: switchStep.id, to: flow.steps.find((step) => step.kind === "sleep")!.id, kind: "branch", label: "case-2" },
    { from: switchStep.id, to: "step_perform_open_notice", kind: "branch", label: "default" },
  ])
  const afterSwitch = ifStep
  const switchBranchRoots = new Set(flow.edges.filter((candidate) => candidate.from === switchStep.id && candidate.kind === "branch").map((candidate) => candidate.to))
  const switchContinuationSources = new Set(flow.edges.filter((candidate) => candidate.kind === "next" && candidate.to === afterSwitch.id).map((candidate) => candidate.from))
  expect(switchContinuationSources).toEqual(switchBranchRoots)
  const overlaps = (left: ReadonlyArray<string>, right: ReadonlyArray<string>) => left.filter((id) => right.includes(id))
  const laterPerform = flow.steps.find((step) => step.id === "step_perform_open_notice")!
  expect(overlaps(all.stateIds, laterPerform.stateIds)).toEqual([])
  expect(overlaps(all.decisionRowIds, laterPerform.decisionRowIds)).toEqual([])
  expect(overlaps(switchStep.stateIds, ifStep.stateIds)).toEqual([])
  expect(overlaps(switchStep.decisionRowIds, ifStep.decisionRowIds)).toEqual([])
  const compose = flow.steps.find((step) => step.kind === "compose")!
  const afterCompose = flow.steps.find((step) => step.id === "step_perform_charge")!
  expect(overlaps(compose.stateIds, afterCompose.stateIds)).toEqual([])
  expect(overlaps(compose.decisionRowIds, afterCompose.decisionRowIds)).toEqual([])

  const artifacts = new Map(compiled.artifacts.map((artifact) => [artifactIdForTest(artifact), artifact as unknown as {
    kind: string
    payload: {
      objectType?: { properties: Array<{ id: string; type: { values?: Array<{ value: string }> } }> }
      graph?: { nodes: Array<{ type: string; content?: { rules?: Array<{ _id: string }> } }> }
    }
  }]))
  const entityStates = new Set([...artifacts.values()].flatMap((artifact) => artifact.kind === "entity"
    ? artifact.payload.objectType?.properties.find((property) => property.id === "status")?.type.values?.map((value) => value.value) ?? []
    : []))
  const decisionRows = new Set([...artifacts.values()].flatMap((artifact) => artifact.kind === "rule"
    ? artifact.payload.graph?.nodes.find((node) => node.type === "decisionTableNode")?.content?.rules?.map((row) => row._id) ?? []
    : []))
  for (const step of flow.steps) {
    for (const id of step.artifactIds) expect(artifacts.has(id)).toBe(true)
    for (const id of step.stateIds) expect(entityStates.has(id)).toBe(true)
    for (const id of step.decisionRowIds) expect(decisionRows.has(id)).toBe(true)
  }

  const withoutMap = compileFile(file, { ...sourceOptions, includeFlowSourceMap: false })
  if (!withoutMap.ok) throw new Error(JSON.stringify(withoutMap.diagnostics))
  expect(withoutMap.flowSourceMap).toBeUndefined()
  expect(canonicalJson(withoutMap.artifacts)).toBe(canonicalJson(compiled.artifacts))

  const flattenAll = (steps: ReadonlyArray<Step>): Step[] => steps.flatMap((step) => {
    if (step._tag === "All") return flattenAll(step.steps)
    if (step._tag === "Compose") return [{ ...step, body: flattenAll(step.body) }]
    if (step._tag === "Route") return [{ ...step, branches: step.branches.map((branch) => ({ ...branch, then: flattenAll(branch.then) })), otherwise: flattenAll(step.otherwise) }]
    if (step._tag === "CatchTag") return [{ ...step, body: flattenAll(step.body), handler: flattenAll(step.handler) }]
    return [step]
  })
  const flattened = lowerFlow({ ...compiled.definition, body: flattenAll(compiled.definition.body) }, sourceOptions)
  if (!flattened.ok) throw new Error(JSON.stringify(flattened.diagnostics))
  expect(canonicalJson(flattened.artifacts)).toBe(canonicalJson(compiled.artifacts))
})

test("flow source-map containers list every direct member without absorbing following steps", () => {
  const leaf = (id: string): Step => ({ _tag: "Perform", sourceMap: { id, kind: "perform" } }) as unknown as Step
  const compose = {
    _tag: "Compose",
    name: "composed",
    body: [leaf("compose_a"), leaf("compose_b")],
    sourceMap: { id: "compose", kind: "compose" },
  } as unknown as Step
  const all = {
    _tag: "All",
    steps: [leaf("all_a"), leaf("all_b")],
    sourceMap: { id: "all", kind: "all" },
  } as unknown as Step
  const route = {
    _tag: "Route",
    branches: [
      { when: {}, then: [leaf("then_a"), leaf("then_b")] },
      { when: {}, then: [leaf("else_a"), leaf("else_b")] },
    ],
    otherwise: [leaf("default_a"), leaf("default_b")],
    sourceMap: { id: "route", kind: "switch", branchLabels: ["then", "else"], otherwiseLabel: "default" },
  } as unknown as Step
  const catcher = {
    _tag: "CatchTag",
    tag: "Expected",
    body: [leaf("body_a"), leaf("body_b")],
    handler: [leaf("handler_a"), leaf("handler_b")],
    sourceMap: { id: "catcher", kind: "catchTag" },
  } as unknown as Step
  const retry = {
    _tag: "Retry",
    perform: leaf("retry_perform"),
    sourceMap: { id: "retry", kind: "retry" },
  } as unknown as Step
  const graph = flowSourceMapGraph([
    compose, leaf("after_compose"),
    all, leaf("after_all"),
    route, leaf("after_route"),
    catcher, leaf("after_catcher"),
    retry, leaf("after_retry"),
  ])
  const outgoing = (from: string, kind: "next" | "branch" | "catch" | "retry" | "contains") =>
    graph.edges.filter((edge) => edge.from === from && edge.kind === kind).map((edge) => ({ to: edge.to, label: edge.label }))

  expect(outgoing("compose", "contains")).toEqual([
    { to: "compose_a", label: "body" },
    { to: "compose_b", label: "body" },
  ])
  expect(outgoing("all", "contains")).toEqual([
    { to: "all_a", label: "steps" },
    { to: "all_b", label: "steps" },
  ])
  expect(outgoing("route", "contains")).toEqual([
    { to: "then_a", label: "then" },
    { to: "then_b", label: "then" },
    { to: "else_a", label: "else" },
    { to: "else_b", label: "else" },
    { to: "default_a", label: "default" },
    { to: "default_b", label: "default" },
  ])
  expect(outgoing("route", "branch")).toEqual([
    { to: "then_a", label: "then" },
    { to: "else_a", label: "else" },
    { to: "default_a", label: "default" },
  ])
  expect(outgoing("catcher", "contains")).toEqual([
    { to: "body_a", label: "body" },
    { to: "body_b", label: "body" },
    { to: "handler_a", label: "handler" },
    { to: "handler_b", label: "handler" },
  ])
  expect(outgoing("catcher", "catch")).toEqual([{ to: "handler_a", label: "Expected" }])
  expect(outgoing("retry", "contains")).toEqual([{ to: "retry_perform", label: "perform" }])
  expect(outgoing("retry", "retry")).toEqual([{ to: "retry_perform", label: undefined }])
  expect(outgoing("compose_b", "next")).toContainEqual({ to: "after_compose", label: undefined })
  expect(outgoing("all_b", "next")).toContainEqual({ to: "after_all", label: undefined })
  expect(outgoing("then_b", "next")).toContainEqual({ to: "after_route", label: undefined })
  expect(outgoing("body_b", "next")).toContainEqual({ to: "after_catcher", label: undefined })
  expect(outgoing("handler_b", "next")).toContainEqual({ to: "after_catcher", label: undefined })
  expect(outgoing("retry_perform", "next")).toContainEqual({ to: "after_retry", label: undefined })
})

test("source-map node ids survive comment and whitespace edits", () => {
  const source = join(TOOL_ROOT, "fixtures/compiler/flow-source-map.source.ts")
  const dir = mkdtempSync(join(TOOL_ROOT, "fixtures/compiler/source-map-"))
  try {
    const edited = join(dir, "flow.source.ts")
    const text = readFileSync(source, "utf8")
      .replace("../flows/support/billing.source.ts", "../../flows/support/billing.source.ts")
      .replace("Effect.all([", "Effect.all( /* layout-only comment */ [")
    writeFileSync(edited, `\n\n// inserted comment\n${text.replace(/\n/g, "\n  ")}`)
    const original = compileFile(source, { ...options("flow-source-map"), sourcePath: "tools/norn-ts/fixtures/compiler/flow-source-map.source.ts" })
    const rewritten = compileFile(edited, { ...options("flow-source-map"), sourcePath: "tools/norn-ts/fixtures/compiler/source-map/flow.source.ts" })
    if (!original.ok) throw new Error(JSON.stringify(original.diagnostics))
    if (!rewritten.ok) throw new Error(JSON.stringify(rewritten.diagnostics))
    expect(rewritten.flowSourceMap?.flows[0]?.steps.map((step) => step.id)).toEqual(original.flowSourceMap?.flows[0]?.steps.map((step) => step.id))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})


function rows(artifacts: ReadonlyArray<unknown>) {
  const rule = artifacts[1] as { payload: { graph: { nodes: Array<{
    type: string
    content?: { rules?: Array<{ _id: string; c_when: string; o_facts: string; o_commands: string }> }
  }> } } }
  return rule.payload.graph.nodes.find((node) => node.type === "decisionTableNode")?.content?.rules ?? []
}

test("branch result is stored before perform, restored on resume and visible to a catch handler", () => {
  for (const name of ["branch-continuation", "caught-continuation"]) {
    const result = compileFile(join(TOOL_ROOT, "fixtures/compiler", `${name}.source.ts`), options(name))
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    const decisionRows = rows(result.artifacts)
    for (const label of ["overdue", "current"]) {
      expect(decisionRows.some((row) => row.o_facts.includes(`label: '${label}'`))).toBe(true)
    }
    const terminalRows = decisionRows.filter((row) => row.o_facts.includes("transition: 'to_completed'"))
    expect(terminalRows.length).toBeGreaterThan(0)
    expect(terminalRows.some((row) => row.o_facts.includes("slice.run.slots.bindings.label"))).toBe(true)
    if (name === "caught-continuation") {
      const handled = terminalRows.find((row) => row.c_when.includes("event.failure._tag == 'PaymentDeclined'"))
      expect(handled?.o_facts).toContain("slice.run.slots.bindings.label")
    }
  }
})


for (const name of ["inexact-integer", "inexact-decimal"]) {
  test(`${name}: numeric syntax cannot silently lose precision`, () => {
    const result = compileFile(join(TOOL_ROOT, "fixtures/compiler", `${name}.source.ts`), options(name))
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.diagnostics.map((diagnostic) => diagnostic.code)).toContain("NORN-FLOW-EXPRESSION")
    expect(result.diagnostics.some((diagnostic) => diagnostic.start.line === 4)).toBe(true)
  })
}


describe("compiler resource quotas", () => {
  for (const [name, quota] of Object.entries({
    bytes: { maxSourceBytes: 20 },
    modules: { maxModules: 1 },
    nodes: { maxNodes: 10 },
    depth: { maxDepth: 2 },
    output: { maxOutputBytes: 1 },
    time: { maxMillis: 0 },
  })) {
    test(`${name}: exceeding a tightened quota refuses compilation`, () => {
      const result = compileFile(join(FLOWS, "sequence.source.ts"), options("sequence"), quota)
      expect(result.ok).toBe(false)
      if (result.ok) return
      expect(result.diagnostics.map((diagnostic) => diagnostic.code)).toEqual(["NORN-FLOW-UNBOUNDED"])
    })
  }
})


test("aliased Norn, namespace Effect and subpath gen resolve to the same lowering", () => {
  const result = compileFile(join(TOOL_ROOT, "fixtures/compiler/aliases.source.ts"), options("sequence"))
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(canonicalJson(result.artifacts)).toBe(readFileSync(join(FLOWS, "sequence.ir.json"), "utf8"))
  if (checker) expect(checker.checkArtifactSet(result.artifacts).admitted).toBe(true)
})


test("lowering stops route expansion at the row budget", () => {
  const branches = Array.from({ length: 12 }, () => route([{ when: X.lit(true), then: [] }]))
  const result = lowerFlow(flow({ id: "billing.expansion", version: "1.0.0" }, [...branches, succeed(X.lit(true))]), options("expansion"))
  expect(result.ok).toBe(false)
  if (result.ok) return
  expect(result.diagnostics.map((diagnostic) => diagnostic.code)).toContain("NORN-FLOW-UNBOUNDED")
})


for (const name of ["expanded-local", "expanded-import", "expanded-callback", "expanded-function-schema"]) {
  test(`${name}: shared values are bounded before schema/render/output expansion`, () => {
    const result = compileFile(join(TOOL_ROOT, "fixtures/compiler", `${name}.source.ts`), options(name))
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.diagnostics.map((diagnostic) => diagnostic.code)).toContain("NORN-FLOW-UNBOUNDED")
  })
}

test("long exact decimal syntax normalizes before coefficient conversion", () => {
  const dir = mkdtempSync(join(TOOL_ROOT, "fixtures/compiler/number-"))
  try {
    const file = join(dir, "source.ts")
    writeFileSync(file, `import { Effect } from "effect"\nimport { Norn } from "@norn/authoring"\nexport default Norn.flow({ id: "billing.long_decimal", version: "1.0.0" }, Effect.gen(function* () { return 0.1${"0".repeat(300_000)} }))\n`)
    const result = compileFile(file, options("long-decimal"))
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    expect(result.definition.body).toEqual([expect.objectContaining({ _tag: "Succeed", value: { _tag: "Literal", value: 0.1 } })])
    if (checker) expect(checker.checkArtifactSet(result.artifacts).admitted).toBe(true)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
