import { join } from "node:path"
import { sha, readFile, writeFile, mkdir, mkdtemp, rm, readdir, runProcess, environment } from "./host"
import { object, array, parseJob, type Job } from "./protocol"
import { oracleVerdict, parity } from "./oracle"

const [binary, original, mutant, base, bounds, work, tools] = process.argv.slice(2)
if (process.argv.length !== 9 || !binary || !original || !mutant || !base || !bounds || !work || !tools) {
  throw new Error("usage: run.ts BINARY ORIGINAL MUTANT BASE BOUNDS WORK TOOLS")
}
const env = environment(tools)
const execute = (command: readonly [string, ...string[]], input = "") =>
  runProcess({ command, input, env, timeoutMs: 120000, maxBytes: 16777216 })
const decode = (bytes: Uint8Array) => new TextDecoder("utf-8", { fatal: true }).decode(bytes)
const json = async (path: string): Promise<unknown> => JSON.parse(decode(await readFile(path)))
const checked = async (command: readonly [string, ...string[]], input = "") => {
  const result = await execute(command, input)
  if (result.kind !== "exited" || result.code !== 0) throw new Error(`process failed: ${JSON.stringify(result)}`)
  return result.stdout
}
if ((await checked([process.execPath, "--version"])).trim() !== "1.3.14") throw new Error("Bun version mismatch")
const packagePath = "tools/norn-proofs"
const packageMetadata = await json(join(packagePath, "node_modules/tla-ts/package.json"))
if (!object(packageMetadata) || packageMetadata.version !== "0.0.7") throw new Error("tla-ts version mismatch")
const apalache = join(tools, "quint-home/apalache-dist-0.56.1/apalache")
const jar = join(apalache, "lib/apalache.jar")
if (sha(await readFile(jar)) !== "4753c0ebb2cbb266e2c6ac19ab5ca3827d726cc80fd1fc5d7c1eeb64736cd60b") throw new Error("Apalache digest mismatch")

async function javascript(directory: string): Promise<string[]> {
  const files = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) files.push(...await javascript(path))
    else if (entry.isFile() && /\.(js|mjs|cjs)$/.test(entry.name)) files.push(path)
  }
  return files.sort()
}
const assets = [process.execPath, join(tools, "bin/java"), jar, join(apalache, "bin/apalache-mc"),
  ...["run.ts", "check.ts", "core.ts", "host.ts", "oracle.ts", "protocol.ts", "bun.lock", "node_modules/tla-ts/package.json", "node_modules/fflate/package.json"]
    .map(name => join(packagePath, name)),
  ...await javascript(join(packagePath, "node_modules/tla-ts/dist")),
  ...await javascript(join(packagePath, "node_modules/fflate"))]
const toolchain: Record<string, string> = {}
for (const path of assets) toolchain[path.startsWith(packagePath) ? path : path === process.execPath ? "bun" : path.endsWith("/java") ? "java" : path.endsWith(".jar") ? "apalache.jar" : "apalache.launcher"] = sha(await readFile(path))
const results: Record<string, { resultSha: string; modelSha: string; challenge: string }> = {}
const control = { toolchain, results }
const plan: unknown = JSON.parse(await checked([binary, "prepare", original, mutant, base, bounds, work, tools], JSON.stringify(control)))
if (!object(plan) || !array(plan.jobs) || Object.keys(plan).length !== 1) throw new Error("malformed generated plan")
const jobs = plan.jobs.map(parseJob)
await mkdir(work, { recursive: true })
await writeFile(join(work, "plan.json"), JSON.stringify(plan))
if (new Set(jobs.map(job => job.key)).size !== jobs.length) throw new Error("duplicate generated job key")

async function run(job: Job) {
  if (sha(await readFile(job.model)) !== job.modelSha) throw new Error("generated model changed")
  await mkdir(work, { recursive: true })
  const directory = await mkdtemp(join(work, "backend-"))
  try {
    const primaryPath = join(directory, "primary.json")
    const expected = job.step === "idle" ? 1 : job.expectedStates
    await checked([process.execPath, "run", join(packagePath, "check.ts"), job.model, primaryPath, job.seed, String(expected)])
    const primaryRaw = decode(await readFile(primaryPath))
    const envelope: unknown = JSON.parse(primaryRaw)
    if (!object(envelope) || envelope.modelSha256 !== job.modelSha) throw new Error("primary checked another model")
    const runDirectory = join(directory, "oracle-run")
    const reference = await execute([join(apalache, "bin/apalache-mc"), `--out-dir=${join(directory, "oracle-output")}`,
      `--run-dir=${runDirectory}`, "check", "--init=q_init", "--next=q_step", "--inv=q_inv", "--length=1", "--output-traces=true", job.model])
    let traceRaw: string | null = null
    if (reference.kind === "exited" && reference.code === 12) traceRaw = decode(await readFile(join(runDirectory, "violation.itf.json")))
    const oracle = oracleVerdict(reference, traceRaw === null ? null : JSON.parse(traceRaw))
    const verdict = parity(envelope.outcome, oracle, expected)
    return { status: verdict.status, modelSha: job.modelSha, errors: [],
      reason: verdict.status === "unknown" ? verdict.reason : "backend parity", primaryRaw,
      oracleRaw: JSON.stringify({ stdout: reference.stdout, stderr: reference.stderr, traceRaw }),
      oracleRun: { exitCode: reference.kind === "exited" ? reference.code : null,
        timedOut: reference.kind === "unknown" && reference.timedOut },
      tlaSha256: job.modelSha, traceBackend: "apalache", trace: verdict.status === "violation" ? verdict.trace : null }
  } finally { await rm(directory, { recursive: true, force: true }) }
}
for (const job of jobs) {
  let result
  try { result = await run(job) }
  catch (error) { result = { status: "unknown", modelSha: job.modelSha, errors: [], reason: String(error) } }
  await writeFile(job.result, JSON.stringify(result))
  results[job.key] = { resultSha: sha(await readFile(job.result)), modelSha: job.modelSha, challenge: job.challenge }
}
const bound = await execute([binary, "bind", original, mutant, base, bounds, work, tools], JSON.stringify(control))
process.stdout.write(bound.stdout)
process.stderr.write(bound.stderr)
if (bound.kind === "unknown") {
  if (!bound.stdout) process.stdout.write(JSON.stringify({ acceptancePassed: false, status: "unknown", reason: bound.reason }))
  process.exitCode = 1
} else process.exitCode = bound.code
