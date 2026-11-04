import { readFile, sha } from "./host"
import { array, object, parseJob } from "./protocol"
import { verifyCorrespondence } from "./correspondence"
import { oracleVerdict, parity } from "./oracle"

const [planPath, endpoint, corpus] = process.argv.slice(2)
if (!planPath || !endpoint || new URL(endpoint).hostname !== "127.0.0.1") throw new Error("usage: worker-smoke.ts PLAN LOOPBACK_URL")
const plan: unknown = JSON.parse(await readFile(planPath, "utf8"))
if (!object(plan) || !array(plan.jobs) || plan.jobs.length !== 9) throw new Error("complete acceptance plan required")
const measurements = []
for (const raw of plan.jobs) {
  const job = parseJob(raw)
  const source = await readFile(job.model, "utf8")
  if (sha(source) !== job.modelSha) throw new Error("generated model changed")
  const recorded: unknown = JSON.parse(await readFile(job.result, "utf8"))
  if (!object(recorded) || !object(recorded.oracleRun) || typeof recorded.oracleRun.exitCode !== "number" ||
      typeof recorded.oracleRaw !== "string" || recorded.modelSha !== job.modelSha || recorded.status === "unknown") throw new Error("missing qualified reference")
  const reference: unknown = JSON.parse(recorded.oracleRaw)
  if (!object(reference) || typeof reference.stdout !== "string" || typeof reference.stderr !== "string" ||
      (reference.traceRaw !== null && typeof reference.traceRaw !== "string")) throw new Error("malformed recorded reference")
  const oracle = oracleVerdict({ kind: "exited", code: recorded.oracleRun.exitCode,
    stdout: reference.stdout, stderr: reference.stderr }, reference.traceRaw === null ? null : JSON.parse(reference.traceRaw))
  const expectedStates = job.step === "idle" ? 1 : job.expectedStates
  const began = performance.now()
  const response = await fetch(endpoint, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ source, expectedStates, seed: job.seed }), signal: AbortSignal.timeout(120000) })
  const result: unknown = await response.json()
  if (!response.ok || !object(result) || result.modelSha256 !== job.modelSha ||
      typeof result.elapsedMs !== "number" || parity(result.outcome, oracle, expectedStates).status !== recorded.status) {
    throw new Error(`Worker parity refused: ${job.key}: ${JSON.stringify(result)}`)
  }
  measurements.push({ job: job.key, modelSha256: job.modelSha, verdict: recorded.status,
    elapsedMs: result.elapsedMs, wallMs: performance.now() - began })
}
process.stdout.write(`${JSON.stringify({ workerParityPassed: true, jobs: measurements })}\n`)

if (corpus) {
  const result = await verifyCorrespondence(corpus, async (source, expectedStates) => {
    const response = await fetch(endpoint, { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ source, expectedStates, seed: "1" }), signal: AbortSignal.timeout(120000) })
    const result: unknown = await response.json()
    if (!response.ok || !object(result) || result.modelSha256 !== sha(source)) throw new Error("Worker corpus response mismatch")
    return result.outcome
  })
  process.stdout.write(`${JSON.stringify({ workerCorrespondence: result })}\n`)
}
