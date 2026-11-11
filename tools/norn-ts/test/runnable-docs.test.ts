import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, readFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

const root = join(TOOL_ROOT, "../..")
const native = process.env.NORN_BIN
const wasm = process.env.NORN_CHECKER_WASM_PKG
if (process.env.CI && (!native || !wasm)) throw new Error("runnable docs require NORN_BIN and NORN_CHECKER_WASM_PKG")
const releaseTest = native && wasm ? test : test.skip

releaseTest("the runnable guide executes and native/WASM admission receipts agree", () => {
  const scratch = mkdtempSync(join(tmpdir(), "norn-docs-"))
  try {
    const guide = readFileSync(join(root, "docs/authoring/runnable/README.md"), "utf8")
    const blocks = [...guide.matchAll(/```sh runnable\n([\s\S]*?)```/g)]
    expect(blocks).toHaveLength(1)
    for (const block of blocks) {
      const run = spawnSync("bash", ["-euo", "pipefail", "-c", block[1]!], {cwd: root, env: {...process.env, NORN_DOC_TMP: scratch}, encoding: "utf8", timeout: 60_000})
      expect(run.error).toBeUndefined()
      if (run.status !== 0) throw new Error(run.stderr + run.stdout)
    }
    const reports = ["wasm", "native"].map((name) => JSON.parse(readFileSync(join(scratch, `${name}-report.json`), "utf8")))
    for (const report of reports) {
      expect(report.admitted).toBe(true)
      expect(report.receipt).not.toBeNull()
      expect(report.receiptSha256).toMatch(/^[a-f0-9]{64}$/)
    }
    expect(reports[0].receiptSha256).toBe(reports[1].receiptSha256)
  } finally { rmSync(scratch, {recursive: true, force: true}) }
}, 90_000)

const STATUS_ROWS = 8
const STATUS_COMMAND_TIMEOUT_MS = 600_000
const STATUS_TEST_TIMEOUT_MS = STATUS_ROWS * STATUS_COMMAND_TIMEOUT_MS + 60_000

releaseTest("each core status row executes one named release test", () => {
  const matrix = readFileSync(join(root, "docs/authoring/runnable/status.md"), "utf8")
  const commands = [...matrix.matchAll(/`(cargo test --locked -p [a-z-]+ --test [a-z_]+ [a-z_]+ -- --exact)`/g)]
  expect(commands).toHaveLength(STATUS_ROWS)
  for (const [, command] of commands) {
    const run = spawnSync("bash", ["-euo", "pipefail", "-c", command!], {cwd: root, encoding: "utf8", timeout: STATUS_COMMAND_TIMEOUT_MS})
    expect(run.error).toBeUndefined()
    if (run.status !== 0) throw new Error(run.stderr + run.stdout)
    expect(run.stdout).toContain("1 passed; 0 failed")
    expect(run.stdout).not.toContain("1 ignored")
  }
}, STATUS_TEST_TIMEOUT_MS)
