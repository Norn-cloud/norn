#!/usr/bin/env bun
/** Compile typed definitions and run the authoritative Rust checker ABI. */
import { statSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import { ProfileChecker } from "./check.ts"
import { compileQuestions } from "./compiler/definitions.ts"
import { loadChecker, sourceMappedDiagnostics } from "./checker-abi.ts"
import { admit, compilePack } from "./pack.ts"

if (process.argv[2] === "--version" || process.argv[2] === "-V") {
  const { versionLine } = await import("./build-info.ts")
  process.stdout.write(`${versionLine()}\n`)
  process.exit(0)
}
if (process.argv[2] === "fmt") {
  const { fmt } = await import("./fmt.ts")
  const { ProjectError } = await import("./project.ts")
  try { process.exit(fmt(process.argv.slice(3))) }
  catch (error) {
    if (!(error instanceof ProjectError)) throw error
    process.stdout.write(`${JSON.stringify(error.diagnostic)}\n`)
    process.exit(error.exitCode)
  }
}
if (process.argv[2] === "explain") {
  const { explain } = await import("./explain.ts")
  process.exit(await explain(process.argv.slice(3)))
}
if (process.argv[2] === "dev") {
  const { dev } = await import("./dev.ts")
  const { ProjectError } = await import("./project.ts")
  try { process.exit(await dev(process.argv.slice(3))) }
  catch (error) {
    if (!(error instanceof ProjectError)) throw error
    process.stdout.write(`${JSON.stringify(error.diagnostic)}\n`)
    process.exit(error.exitCode)
  }
}
if (process.argv[2] === "init") {
  const { init } = await import("./init.ts")
  const { ProjectError } = await import("./project.ts")
  try { await init(process.argv.slice(3)) }
  catch (error) {
    if (!(error instanceof ProjectError)) throw error
    process.stdout.write(`${JSON.stringify(error.diagnostic)}\n`)
    process.exit(error.exitCode)
  }
  process.exit(0)
}

const [command, input, ...flags] = process.argv.slice(2)
function option(name: string): string | undefined {
  const index = flags.indexOf(name)
  return index < 0 ? undefined : flags[index + 1]
}
if (!input || (command !== "check" && command !== "compile" && command !== "profile" && command !== "question")) {
  process.stderr.write("usage: norn <check|compile|profile> <pack-dir|file.ts> --org ID --env ID [--out FILE]\n       norn question <file.ts> [--out FILE]\n       norn init <dir> | norn dev [--port N] [--json] | norn fmt\n")
  process.exit(2)
}
if (command === "profile") {
  const reports = new ProfileChecker().check([resolve(input)])
  process.stdout.write(`${JSON.stringify(reports)}\n`)
  process.exit(reports.every((report) => report.diagnostics.length === 0) ? 0 : 1)
}
if (command === "question") {
  if (!statSync(input).isFile()) { process.stderr.write("question expects one TypeScript module\n"); process.exit(2) }
  const result = compileQuestions(resolve(input))
  if (!result.ok) { process.stdout.write(`${JSON.stringify(result)}\n`); process.exit(1) }
  const document = `${JSON.stringify(result.questions)}\n`
  const output = option("--out")
  if (output) writeFileSync(output, document)
  else process.stdout.write(document)
  process.exit(0)
}
const orgId = option("--org")
const environmentId = option("--env")
if (!orgId || !environmentId) {
  process.stderr.write("--org and --env are required\n")
  process.exit(2)
}
if (command === "check") {
  // Before compiling: a project whose sources do not compile is told too.
  const { warnAuthoringVersion } = await import("./authoring-version.ts")
  warnAuthoringVersion(resolve(input))
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
  let checker: Awaited<ReturnType<typeof loadChecker>>
  try {
    checker = await loadChecker()
    if (!checker) throw new Error("NORN_CHECKER_WASM_PKG is required")
  } catch (error) {
    process.stderr.write(`NORN-CHECKER-WASM-SETUP: ${String(error)}\nExpected norn-checker-abi/1 built from this checkout. From the repository root, run:\n  bash crates/norn-checker-wasm/scripts/build-pkg.sh "$PWD/target/norn-checker-wasm/pkg"\n  export NORN_CHECKER_WASM_PKG="$PWD/target/norn-checker-wasm/pkg"\nThen rerun this check.\n`)
    process.exit(2)
  }
  const { report } = admit(checker, pack)
  const diagnostics = [...pack.warnings, ...sourceMappedDiagnostics(report.diagnostics.diagnostics, pack.sourceMap)]
  process.stdout.write(`${JSON.stringify({ ...report, diagnostics })}\n`)
  process.exit(report.admitted ? 0 : 1)
}
