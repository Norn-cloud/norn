import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { registeredCodes } from "../src/cli-diagnostic.ts"
import { TOOL_ROOT } from "../src/profile.ts"

test("check explains missing or incomplete WASM setup without an uncaught stack", () => {
  const empty = mkdtempSync(join(tmpdir(), "norn-wasm-setup-"))
  const registered = registeredCodes().get("cli.setup.checker_unavailable")!
  try {
    for (const pkg of ["", join(empty, "missing"), empty]) {
      const run = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "check", join(TOOL_ROOT, "fixtures/quickstart"), "--org", "example", "--env", "local"], {
        cwd: join(TOOL_ROOT, "../.."), encoding: "utf8", timeout: 30_000,
        env: {...process.env, NORN_CHECKER_WASM_PKG: pkg, NORN_CACHE_DIR: join(empty, "no-cache")},
      })
      expect(run.error).toBeUndefined()
      expect(run.status).toBe(2)
      expect(run.stdout).toBe("")
      expect(run.stderr).toStartWith("cli.setup.checker_unavailable: ")
      expect(run.stderr).toContain(registered.message)
      expect(run.stderr).toContain(pkg ? "holds no norn-checker-wasm package" : "NORN_CHECKER_WASM_PKG is not set")
      expect(run.stderr).toContain(`  fix: ${registered.fix}`)
      expect(run.stderr).toContain("norn setup")
      expect(run.stderr).toContain("norn-checker-abi/1")
      expect(run.stderr).toContain('bash crates/norn-checker-wasm/scripts/build-pkg.sh "$PWD/target/norn-checker-wasm/pkg"')
      expect(run.stderr).toContain('export NORN_CHECKER_WASM_PKG="$PWD/target/norn-checker-wasm/pkg"')
      expect(run.stderr).not.toContain("NORN-CHECKER-WASM-SETUP")
      expect(run.stderr).not.toContain("at ")
    }
  } finally { rmSync(empty, {recursive: true, force: true}) }
}, 60_000)
