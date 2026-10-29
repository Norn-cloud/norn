/** Compile typed definitions and run the authoritative Rust checker ABI. */
import { statSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import { ProfileChecker } from "./check.ts"
import { loadChecker, sourceMappedDiagnostics } from "./checker-abi.ts"
import { checkPack, compilePack, finalizeFlowSourceMap } from "./pack.ts"

const [command, input, ...flags] = process.argv.slice(2)
function option(name: string): string | undefined {
  const index = flags.indexOf(name)
  return index < 0 ? undefined : flags[index + 1]
}
if (!input || (command !== "check" && command !== "compile" && command !== "profile")) {
  process.stderr.write("usage: norn <check|compile|profile> <pack-dir|file.ts> --org ID --env ID [--out FILE]\n")
  process.exit(2)
}
if (command === "profile") {
  const reports = new ProfileChecker().check([resolve(input)])
  process.stdout.write(`${JSON.stringify(reports)}\n`)
  process.exit(reports.every((report) => report.diagnostics.length === 0) ? 0 : 1)
}
const orgId = option("--org")
const environmentId = option("--env")
if (!orgId || !environmentId) {
  process.stderr.write("--org and --env are required\n")
  process.exit(2)
}
const pack = compilePack(resolve(input), { scope: { orgId, environmentId }, root: process.cwd(), ...(statSync(input).isFile() ? { modules: [resolve(input)] } : {}) })
if (!pack.ok) {
  process.stdout.write(`${JSON.stringify(pack)}\n`)
  process.exit(1)
}
if (command === "compile") {
  const output = option("--out")
  const document = `${JSON.stringify(pack.artifacts)}\n`
  if (output) writeFileSync(output, document)
  else process.stdout.write(document)
} else {
  const checker = await loadChecker()
  if (!checker) throw new Error("NORN_CHECKER_WASM_PKG is required")
  const checked = checkPack(checker, pack.artifacts, pack.baseArtifacts)
  const flowSourceMap = pack.flowSourceMap ? finalizeFlowSourceMap(pack.flowSourceMap, pack.flows, checked.receipt) : undefined
  const report = flowSourceMap ? checkPack(checker, pack.artifacts, pack.baseArtifacts, flowSourceMap) : checked
  const diagnostics = [...pack.warnings, ...sourceMappedDiagnostics(report.diagnostics.diagnostics, pack.sourceMap)]
  process.stdout.write(`${JSON.stringify({ ...report, diagnostics })}\n`)
  process.exit(report.admitted ? 0 : 1)
}
