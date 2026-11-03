import { expect, test } from "bun:test"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

test("tla_ts_apalache_parity", async () => {
  const root = fileURLToPath(new URL("../../", import.meta.url))
  const tools = process.env.NORN_VERIFICATION_TOOL_ROOT
  if (!tools) throw new Error("NORN_VERIFICATION_TOOL_ROOT is required for real backend parity")
  const work = await mkdtemp(join(tmpdir(), "norn-proof-parity-"))
  try {
    const fixture: unknown = await Bun.file(join(root, "crates/norn-cell/tests/fixtures/constitution.json")).json()
    if (typeof fixture !== "object" || fixture === null || !("withLoss" in fixture) || !("withoutLoss" in fixture)) {
      throw new Error("constitution fixture requires original and mutant declarations")
    }
    await Bun.write(join(work, "original.json"), JSON.stringify(fixture.withLoss))
    await Bun.write(join(work, "mutant.json"), JSON.stringify(fixture.withoutLoss))
    const base: unknown[] = []
    for (const name of ["core.units", "core.Process.interface", "core.Conversion.interface"]) {
      base.push(await Bun.file(join(root, `registry/core/artifacts/${name}.json`)).json())
    }
    await Bun.write(join(work, "base.json"), JSON.stringify(base))
    const process = Bun.spawn(["python3", "scripts/run-generated-proofs.py", "target/debug/norn-proof-quint",
      join(work, "original.json"), join(work, "mutant.json"), join(work, "base.json"),
      "verification/profiles/constitution-one-create.json", join(work, "jobs"), tools],
      {cwd: root, stdout: "pipe", stderr: "inherit"})
    const output = await new Response(process.stdout).text()
    console.info(output)
    expect(await process.exited).toBe(0)
    const report: unknown = JSON.parse(output)
    expect(report).toMatchObject({acceptancePassed: true, staleRejected: true, cases: [
      {case: "original", total: 10, eligible: 3, discharged: 3, acceptancePassed: true},
      {case: "mutant", total: 10, eligible: 3, discharged: 2, acceptancePassed: true,
        goals: expect.arrayContaining([expect.objectContaining({kind: "preservation", status: "refuted",
          scenario: expect.objectContaining({law: "constitution.fine_mass.law", writer: "constitution.melt.convert.action"})})])},
      {case: "restored", total: 10, eligible: 3, discharged: 3, acceptancePassed: true},
    ]})
  } finally {
    await rm(work, {recursive: true, force: true})
  }
}, 600_000)


test("generated_solver_timeout_and_exit_status", async () => {
  const root = fileURLToPath(new URL("../../", import.meta.url))
  const child = Bun.spawn(["python3", "scripts/test_generated_proofs.py"], {cwd: root, stdout: "inherit", stderr: "inherit"})
  expect(await child.exited).toBe(0)
})
