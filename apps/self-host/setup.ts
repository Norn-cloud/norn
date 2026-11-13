// First-run setup of a self-hosted Norn, run by start.sh once the node serves. It talks to Studio
// over HTTP like any client: no database writes and no authority of its own.
//
// 1. The owner: NORN_OWNER_EMAIL signs up (the install's first account owns its workspace; after
//    it, sign-up is by invitation). The password is NORN_OWNER_PASSWORD, or one generated once
//    into /data/secrets/owner-password.
// 2. The owner's cell: `norn workspace create`, the same command the README shows, with no
//    artifacts of its own, so the cell starts with the host's trusted base.
//
// Every step is idempotent: on a restart the sign-up answers as before, and the cell replays.
import { spawnSync } from "node:child_process"
import { existsSync, readFileSync, writeFileSync } from "node:fs"
import { randomBytes } from "node:crypto"

const LOCAL = "http://127.0.0.1:8080"
const publicUrl = process.env.NORN_PUBLIC_URL ?? "http://localhost:8080"
const email = process.env.NORN_OWNER_EMAIL ?? "owner@norn.localhost"
const passwordFile = "/data/secrets/owner-password"

function ownerPassword(): string {
  const given = process.env.NORN_OWNER_PASSWORD
  if (given !== undefined && given !== "") {
    if (given.length < 12) throw new Error("NORN_OWNER_PASSWORD must be at least 12 characters")
    return given
  }
  if (!existsSync(passwordFile)) writeFileSync(passwordFile, randomBytes(18).toString("base64url"), { mode: 0o600, flag: "wx" })
  return readFileSync(passwordFile, "utf8").trim()
}

/** A request to the node, as a browser at the public origin would send it (Studio checks the origin). */
async function call(path: string, init: { method?: string; body?: unknown; cookie?: string } = {}): Promise<Response> {
  const headers: Record<string, string> = { origin: publicUrl, "x-real-ip": "127.0.0.1" }
  if (init.body !== undefined) headers["content-type"] = "application/json"
  if (init.cookie !== undefined) headers.cookie = init.cookie
  return fetch(`${LOCAL}${path}`, { method: init.method ?? (init.body === undefined ? "GET" : "POST"), headers, body: init.body === undefined ? undefined : JSON.stringify(init.body), redirect: "manual" })
}

async function served(): Promise<void> {
  const deadline = Date.now() + 180_000
  for (;;) {
    try {
      if ((await call("/")).status === 200) return
    } catch { /* not listening yet */ }
    if (Date.now() > deadline) throw new Error("the node did not serve Studio within 180 s")
    await Bun.sleep(1_000)
  }
}

async function main(): Promise<void> {
  await served()
  const password = ownerPassword()
  const signUp = await call("/api/auth/sign-up/email", { body: { email, password, name: "Owner" } })
  // 403: the owner exists already (a restart) and this address is not it; sign-in below says which.
  if (signUp.status !== 200 && signUp.status !== 403) throw new Error(`owner sign-up failed: HTTP ${signUp.status} ${await signUp.text()}`)
  const signIn = await call("/api/auth/sign-in/email", { body: { email, password } })
  if (signIn.status !== 200) throw new Error(`owner sign-in failed (HTTP ${signIn.status}): another account owns this install, or NORN_OWNER_PASSWORD changed`)
  // Straight to the node's loopback listener, naming the public origin Studio trusts.
  const created = spawnSync("norn", ["workspace", "create", "--studio", publicUrl, "--via", LOCAL, "--email", email, "--password-env", "NORN_OWNER_PASSWORD"], {
    env: { ...process.env, NORN_OWNER_PASSWORD: password }, encoding: "utf8",
  })
  if (created.status !== 0) throw new Error(`norn workspace create failed: ${created.stderr || created.stdout}`)
  console.log([
    "",
    `Norn is ready at ${publicUrl}`,
    `Sign in as ${email}.${process.env.NORN_OWNER_PASSWORD ? "" : " The password is in the data volume: docker compose exec norn cat /data/secrets/owner-password"}`,
    "",
  ].join("\n"))
}

await main()
