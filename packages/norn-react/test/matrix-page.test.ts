// The fixture page the state matrix proves the renderer on (#1762 milestone C): the checker admits it
// against the recorded demo definitions. Its island registry is codegen's (tools/norn-ts codegen.test.ts).
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { demoDefinitions, matrixArtifact } from "./fixtures/matrix-page.ts"

const pkg = process.env.NORN_CHECKER_WASM_PKG
const wasm = pkg
  ? ((await import(pathToFileURL(join(pkg, "norn_checker_wasm.js")).href)) as { initSync(options: { module: Uint8Array }): unknown; checkArtifactSet(artifactsJson: string, contextJson: string): string })
  : null
wasm?.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
if (!wasm && process.env.NORN_CHECKER_WASM_REQUIRED) throw new Error("NORN_CHECKER_WASM_PKG is required")
type Checked = { readonly diagnostics: { readonly diagnostics: readonly { readonly severity: string; readonly code: string; readonly message: string; readonly pointer?: string }[] } }
const checker = wasm === null ? null : { checkArtifactSet: (artifacts: readonly unknown[]) => JSON.parse(wasm.checkArtifactSet(JSON.stringify(artifacts), "null")) as Checked }

test.skipIf(!checker)("the checker admits the fixture page: every outcome handled, every law held", () => {
  const artifact = matrixArtifact()
  const checked = checker!.checkArtifactSet([...demoDefinitions(), artifact])
  const errors = checked.diagnostics.diagnostics.filter((diagnostic) => diagnostic.severity === "error").map((diagnostic) => `${diagnostic.code} ${diagnostic.pointer ?? ""}: ${diagnostic.message}`)
  expect(errors).toEqual([])
})
