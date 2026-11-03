import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

test("check explains missing or incomplete WASM setup without an uncaught stack", () => {
  const empty = mkdtempSync(join(tmpdir(), "norn-wasm-setup-"))
  try {
    for (const pkg of ["", join(empty, "missing"), empty]) {
      const run = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "check", join(TOOL_ROOT, "fixtures/quickstart"), "--org", "example", "--env", "local"], {
        cwd: join(TOOL_ROOT, "../.."), encoding: "utf8", timeout: 30_000,
        env: {...process.env, NORN_CHECKER_WASM_PKG: pkg},
      })
      expect(run.error).toBeUndefined()
      expect(run.status).toBe(2)
      expect(run.stdout).toBe("")
      expect(run.stderr).toContain("NORN-CHECKER-WASM-SETUP:")
      expect(run.stderr).toContain("norn-checker-abi/1")
      expect(run.stderr).toContain('bash crates/norn-checker-wasm/scripts/build-pkg.sh "$PWD/target/norn-checker-wasm/pkg"')
      expect(run.stderr).toContain('export NORN_CHECKER_WASM_PKG="$PWD/target/norn-checker-wasm/pkg"')
      expect(run.stderr).not.toContain("at ")
    }
  } finally { rmSync(empty, {recursive: true, force: true}) }
}, 60_000)
