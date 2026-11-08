import { execFileSync } from "node:child_process"
import { existsSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { writeSecretConfig } from "../celld-config.ts"

// Deploy the Norn cell Worker (a built norn-cell-do bundle) to one celld stage, and
// prove the stage serves it. A cell-only deploy has three traps, each handled here:
//  1. celld makes the last deployed script the fleet's application, so the stage's
//     Studio is restored as the fleet application right after;
//  2. that leaves the fleet pointer unchanged, so the node would keep the old cell
//     until something else moves: the node is told to reload (POST /reload on its
//     internal listener, over ssh to the host);
//  3. "deployed" is not "served": a route only the new build has is probed, and the
//     deploy fails unless the stage answers it.
//
//   bun scripts/celld-deploy-cell.ts --stage next --bundle <dir with worker.mjs and pkg/>
//
// Environment: CELLD_BUCKET, S3_ENDPOINT, AWS_REGION, AWS_ACCESS_KEY_ID,
// AWS_SECRET_ACCESS_KEY (the stage's own bucket credentials), NORN_LINEAGE_KEY,
// NORN_PROBE_EMAIL + NORN_PROBE_PASSWORD (a stage account), NORN_CELLD_SSH (the
// ssh host of the stage node; required). Secrets travel in the 0600 config file, stdin or the
// environment, never argv or logs.

/** What one stage's cell deploy touches, derived from its name alone. */
export interface CellStage {
  readonly stage: string
  readonly origin: string
  readonly unit: string
  readonly studio: string
  readonly bucket: string
}

export function cellStage(stage: string | undefined, bucket: string | undefined): CellStage {
  if (stage === undefined || !/^[a-z][a-z0-9-]{0,30}$/.test(stage)) throw new Error("--stage must name a stage, for example next")
  if (stage === "prod") throw new Error("production is not a celld stage")
  // Each stage owns its bucket: a deploy with another stage's credentials refuses.
  if (bucket === undefined || !bucket.startsWith(`s3://norn-${stage}-celld`)) throw new Error(`CELLD_BUCKET must be stage ${stage}'s own bucket (s3://norn-${stage}-celld…)`)
  return { stage, origin: `https://${stage}.norn.cloud`, unit: `norn-${stage}-celld`, studio: `norn-studio-${stage}`, bucket }
}

/** The Studio pointer to restore as the fleet application, checked to be the stage's own. */
export function studioPointer(document: string, studio: string): string {
  const pointer = JSON.parse(document) as { script_name?: unknown }
  if (pointer.script_name !== studio) throw new Error(`the named pointer is not ${studio}'s`)
  return document
}

/**
 * The probe passes once the stage answers the new build's route: a 2xx, or a 4xx that carries the
 * host's own error code other than route.not_found. A bare 4xx (a proxy or CDN page) is not the new build.
 */
export function probeServesNewBuild(status: number, body: string): boolean {
  if (status >= 200 && status < 300) return true
  if (status < 400 || status >= 500) return false
  try {
    const code = (JSON.parse(body) as { code?: unknown }).code
    return typeof code === "string" && code !== "route.not_found"
  } catch {
    return false
  }
}

const env = (name: string): string => {
  const value = process.env[name]
  if (value === undefined || value === "") throw new Error(`${name} is required`)
  return value
}

/** One S3 request to the stage's bucket, credentials on stdin (never argv). */
function s3(method: "GET" | "PUT", key: string, bodyFile?: string): string {
  const url = `${env("S3_ENDPOINT")}/${env("CELLD_BUCKET").slice("s3://".length).split("/")[0]}/${key}`
  const args = ["-sS", "--fail", "-X", method, "--aws-sigv4", "aws:amz:auto:s3", "-K", "-", ...(bodyFile === undefined ? [] : ["--data-binary", `@${bodyFile}`]), url]
  return execFileSync("curl", args, { input: `user = "${env("AWS_ACCESS_KEY_ID")}:${env("AWS_SECRET_ACCESS_KEY")}"\n`, encoding: "utf8" })
}

/** POST /reload on the stage node's internal listener, over ssh; the listener is loopback-only. */
function reload(stage: CellStage): void {
  const host = env("NORN_CELLD_SSH")
  // The unit name is derived from the validated stage name: nothing here comes from input text.
  // The internal listener is the node's other loopback socket: not its public --listen address.
  const remote = [
    `P=$(systemctl show ${stage.unit} -p MainPID --value)`,
    `L=$(systemctl show ${stage.unit} -p ExecStart --value | grep -o -- '--listen [0-9.:]*' | awk '{print $2}')`,
    `I=$(ss -ltnp | grep "pid=$P," | awk '{print $4}' | grep -vxF "$L" | head -1)`,
    'test -n "$P" -a -n "$I" && curl -sS -m 60 -X POST "http://$I/reload"',
  ].join("; ")
  const answer = execFileSync("ssh", ["-o", "IdentityAgent=none", host, remote], { encoding: "utf8" })
  const reloaded = JSON.parse(answer) as { ok?: unknown; outcome?: unknown; generation?: unknown }
  if (reloaded.ok !== true) throw new Error(`the ${stage.unit} node did not reload: ${answer.slice(0, 200)}`)
  console.log(`${stage.unit}: reload ${String(reloaded.outcome)} (generation ${String(reloaded.generation)})`)
}

/** Sign in as the probe account and POST an empty submission to the new build's route. */
async function probe(stage: CellStage, deadlineMs: number): Promise<void> {
  const headers = { "content-type": "application/json", origin: stage.origin, "user-agent": "norn-cell-deploy/1" }
  const signIn = await fetch(`${stage.origin}/api/auth/sign-in/email`, { method: "POST", headers, body: JSON.stringify({ email: env("NORN_PROBE_EMAIL"), password: env("NORN_PROBE_PASSWORD") }) })
  if (!signIn.ok) throw new Error(`the probe account could not sign in (${signIn.status})`)
  const cookie = signIn.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
  const path = `/api/host/v1/questions:evaluate?${process.env.NORN_PROBE_SCOPE ?? "orgId=org_demo&environmentId=analytics"}`
  let last = ""
  while (Date.now() < deadlineMs) {
    const response = await fetch(`${stage.origin}${path}`, { method: "POST", headers: { ...headers, cookie }, body: "{}", signal: AbortSignal.timeout(Math.max(1, deadlineMs - Date.now())) }).catch(() => undefined)
    const body = response === undefined ? "" : await response.text()
    last = `${response?.status ?? "no answer"} ${body.slice(0, 120)}`
    if (response !== undefined && probeServesNewBuild(response.status, body)) {
      console.log(`${stage.origin} serves the new cell build: POST :evaluate {} answered ${response.status}`)
      return
    }
    await new Promise((resolve) => setTimeout(resolve, 3000))
  }
  throw new Error(`${stage.origin} still serves the old cell build after the reload: ${last}`)
}

async function main(argv: readonly string[]) {
  const value = (flag: string) => {
    const at = argv.indexOf(flag)
    return at < 0 ? undefined : argv[at + 1]
  }
  const stage = cellStage(value("--stage"), process.env.CELLD_BUCKET)
  const bundle = value("--bundle")
  if (bundle === undefined || !existsSync(join(bundle, "worker.mjs")) || !existsSync(join(bundle, "pkg", "norn_cell_do_bg.wasm"))) {
    throw new Error("--bundle must hold worker.mjs and pkg/norn_cell_do_bg.wasm (apps/norn-cell-do/README.md)")
  }
  const config = join(bundle, "wrangler.json")
  writeSecretConfig(config, {
    name: "norn-cell",
    main: "worker.mjs",
    compatibility_date: "2026-11-01",
    durable_objects: { bindings: [{ name: "CELLS", class_name: "Cell" }] },
    migrations: [{ tag: "v1", new_sqlite_classes: ["Cell"] }],
    vars: { NORN_HOST_ISSUER: stage.origin, NORN_HOST_JWKS_URL: `${stage.origin}/api/auth/jwks`, NORN_LINEAGE_KEY: env("NORN_LINEAGE_KEY") },
  })
  try {
    execFileSync("celld", ["deploy", ".", "--json"], { cwd: bundle, stdio: ["ignore", "inherit", "inherit"] })
  } finally {
    rmSync(config, { force: true })
  }
  const pointer = studioPointer(s3("GET", `deploy/${stage.studio}/current.json`), stage.studio)
  const pointerFile = join(bundle, "studio-pointer.json")
  writeFileSync(pointerFile, pointer)
  try {
    s3("PUT", "deploy/current.json", pointerFile)
  } finally {
    rmSync(pointerFile, { force: true })
  }
  console.log(`${stage.studio} is the fleet application again`)
  reload(stage)
  await probe(stage, Date.now() + 150_000)
}

if (import.meta.main) await main(process.argv.slice(2))
