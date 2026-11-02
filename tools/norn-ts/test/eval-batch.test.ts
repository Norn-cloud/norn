import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"

const native = process.env.NORN_BIN
if (process.env.CI && !native) throw new Error("eval batch tests require NORN_BIN")

// CLI-EVAL-BATCH-1: a refusal cannot be hidden by a later accepted record.
test("native eval reports every record and fails a partially refused batch", () => {
  if (!native) return
  const dir = mkdtempSync(join(tmpdir(), "norn-eval-batch-"))
  try {
    const source = join(dir, "rule.ts")
    writeFileSync(source, `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Task = Norn.type({name: "demo.Task", title: "Task", version: "1.0.0", properties: Schema.Struct({id: Schema.String}), identity: [{name: "primary", properties: ["id"]}]})
export const Rule = Norn.decision({
  id: "demo.batch", title: "Batch", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({slice: Schema.Struct({}), event: Schema.Struct({kind: Schema.String})}),
  output: Schema.Struct({facts: Schema.Array(Schema.Struct({op: Schema.Literal("delete"), objectType: Schema.Literal("demo.Task"), key: Schema.String})), commands: Schema.Array(Schema.Struct({actionId: Schema.String})), timers: Schema.Array(Schema.Struct({id: Schema.String}))}),
}, (input) => ({facts: [], commands: [], timers: []}))`)
    const compiled = compileDefinitions(source, {scope: {orgId: "org_demo", environmentId: "dev"}, sourcePath: "rule.ts"})
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    compiled.artifacts.forEach((artifact, index) => writeFileSync(join(dir, `${index}.json`), JSON.stringify(artifact)))
    const good = '{"slice":{},"event":{"kind":"start"}}\n'
    for (const [input, accepted, refused] of [
      [good + good, 2, 0],
      [good + '{"slice":{},"event":{}}\n' + good, 2, 1],
      ['not json\n' + good, 1, 1],
      ['not json\n\n', 0, 2],
      ['', 0, 0],
    ] as const) {
      const run = spawnSync(native, ["eval", "--defs", dir, "--org", "org_demo", "--env", "dev", "--rule", "demo.batch.rule"], {input, encoding: "utf8", timeout: 30_000})
      expect(run.error).toBeUndefined()
      expect(run.signal).toBeNull()
      expect(run.stderr).toBe(`norn eval: ${accepted} accepted, ${refused} refused\n`)
      expect(run.status).toBe(refused === 0 ? 0 : 1)
      const lines = run.stdout.trim() ? run.stdout.trim().split("\n") : []
      expect(lines).toHaveLength(accepted + refused)
      const responses = lines.map((line) => JSON.parse(line))
      expect(responses.filter((response) => response.ok === true)).toHaveLength(accepted)
      expect(responses.filter((response) => response.ok === false && typeof response.error === "string")).toHaveLength(refused)
      if (accepted > 0) expect(responses.at(-1)?.ok).toBe(true)
    }
  } finally { rmSync(dir, {recursive: true, force: true}) }
}, 60_000)
