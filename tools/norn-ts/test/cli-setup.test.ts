import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { registeredCodes } from "../src/cli-diagnostic.ts"
import { TOOL_ROOT } from "../src/profile.ts"

test("check explains missing or incomplete WASM setup without an uncaught stack", () => {
  const empty = mkdtempSync(join(tmpdir(), "norn-wasm-setup-"))
  const registered = registeredCodes().get("cli.setup.checker_unavailable")!
  try {
    const brokenPkg = join(empty, "broken-loader")
    mkdirSync(brokenPkg)
    writeFileSync(
      join(brokenPkg, "norn_checker_wasm.js"),
      'export function initSync() { throw new Error("corrupt\\u009bwasm\\r    at initSync (broken.js:9:3)\\nmore detail") }\n',
    )
    writeFileSync(join(brokenPkg, "norn_checker_wasm_bg.wasm"), "")

    const cases: ReadonlyArray<{ readonly pkg: string; readonly detail: string }> = [
      { pkg: "", detail: "NORN_CHECKER_WASM_PKG is not set" },
      { pkg: join(empty, "missing"), detail: `NORN_CHECKER_WASM_PKG=${join(empty, "missing")} holds no norn-checker-wasm package` },
      { pkg: empty, detail: `NORN_CHECKER_WASM_PKG=${empty} holds no norn-checker-wasm package` },
      {
        pkg: `${join(empty, "bad")}\n    at leakedFrame (fake.ts:1:1)\nsecond-line`,
        detail: `NORN_CHECKER_WASM_PKG=${join(empty, "bad")} second-line holds no norn-checker-wasm package`,
      },
      {
        pkg: `${join(empty, "cr")}\r    at crFrame (fake.ts:2:1)\u2028\u009b31mred`,
        detail: `NORN_CHECKER_WASM_PKG=${join(empty, "cr")} 31mred holds no norn-checker-wasm package`,
      },
      { pkg: brokenPkg, detail: "corrupt wasm more detail" },
    ]
    for (const { pkg, detail } of cases) {
      const run = spawnSync(process.execPath, [join(TOOL_ROOT, "src/cli.ts"), "check", join(TOOL_ROOT, "fixtures/quickstart"), "--org", "example", "--env", "local"], {
        cwd: join(TOOL_ROOT, "../.."), encoding: "utf8", timeout: 30_000,
        env: {...process.env, NORN_CHECKER_WASM_PKG: pkg, NORN_CACHE_DIR: join(empty, "no-cache")},
      })
      expect(run.error).toBeUndefined()
      expect(run.status).toBe(2)
      expect(run.stdout).toBe("")
      expect(run.stderr).toBe(`cli.setup.checker_unavailable: ${registered.message} (${detail})\n  fix: ${registered.fix}\n`)
      expect(run.stderr.trimEnd().split("\n")).toHaveLength(2)
      expect(run.stderr).toContain("norn setup")
      expect(run.stderr).toContain("unset NORN_CHECKER_WASM_PKG")
      expect(run.stderr).toContain("norn-checker-abi/1")
      expect(run.stderr).toContain('bash crates/norn-checker-wasm/scripts/build-pkg.sh "$PWD/target/norn-checker-wasm/pkg"')
      expect(run.stderr).toContain('export NORN_CHECKER_WASM_PKG="$PWD/target/norn-checker-wasm/pkg"')
      expect(run.stderr).not.toContain("at ")
    }
  } finally { rmSync(empty, {recursive: true, force: true}) }
}, 60_000)
