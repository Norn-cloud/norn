import { expect, test } from "bun:test"
import { Database } from "bun:sqlite"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { D1Unreachable, migrate, type Migration, type Remote } from "../scripts/celld-migrate.ts"

const sql = 'CREATE TABLE "connector_binding" ("id" text);\n'
const m: Migration = { name: "0002_connector_binding.sql", sha256: new Bun.CryptoHasher("sha256").update(sql).digest("hex"), sql }

/** A host whose operator project holds `files` (name → sha256) and whose apply answers `apply`. */
function host(files: Record<string, string>, apply: { status: number; output: string }, execute = { status: 0, output: "ok\n" }) {
  const commands: string[] = []
  const remote: Remote = (command, input) => {
    commands.push(command)
    if (command.includes("migrations apply")) return apply
    if (command.includes(" execute ")) return execute
    const name = /migrations\/([^ ;"]+\.sql)/.exec(command)?.[1] ?? ""
    if (files[name] !== undefined) return { status: 0, output: `${files[name]}\n` }
    files[name] = new Bun.CryptoHasher("sha256").update(input ?? "").digest("hex")
    return { status: 0, output: "installed\n" }
  }
  return { remote, commands, files }
}

test("migrations are copied into the stage's operator project and applied there, idempotently", () => {
  const fresh = host({}, { status: 0, output: "Applied 0002_connector_binding.sql\n" })
  expect(migrate(fresh.remote, "next", [m], false)).toBe("Applied 0002_connector_binding.sql\nbilling mode: off")
  expect(fresh.files["0002_connector_binding.sql"]).toBe(m.sha256)
  expect(fresh.commands.at(-2)).toBe("/opt/norn-demo/d1-next migrations apply norn-studio-auth-next 2>&1")
  expect(fresh.commands[0]).toContain("/opt/norn-demo/ops-next/migrations/0002_connector_binding.sql")
  // A repeat run copies nothing new and applies nothing, and passes.
  const again = host({ "0002_connector_binding.sql": m.sha256 }, { status: 0, output: "No migrations to apply.\n" })
  expect(migrate(again.remote, "next", [m], false)).toBe("No migrations to apply.\nbilling mode: off")
})

test("an applied migration that changed is refused before anything is applied", () => {
  const edited = host({ "0002_connector_binding.sql": "f".repeat(64) }, { status: 0, output: "Applied x" })
  expect(() => migrate(edited.remote, "next", [m], false)).toThrow("never edited")
  expect(edited.commands.some((c) => c.includes("migrations apply"))).toBe(false)
})

test("an unreachable D1 is its own refusal, so the deploy stops before shipping", () => {
  const refused = host({}, { status: 1, output: "error: connect to http://127.0.0.1:39673/runtime/__D1Database: Connection refused (os error 111)\n" })
  expect(() => migrate(refused.remote, "next", [m], false)).toThrow(D1Unreachable)
  const noHost: Remote = () => ({ status: 255, output: "ssh: connect to host cell-node port 22: Connection timed out" })
  expect(() => migrate(noHost, "next", [m], false)).toThrow(D1Unreachable)
  const broken = host({}, { status: 1, output: "near \"ALTR\": syntax error\n" })
  expect(() => migrate(broken.remote, "next", [m], false)).toThrow("migrations apply failed")
  expect(() => migrate(broken.remote, "next", [m], false)).not.toThrow(D1Unreachable)
  expect(() => migrate(host({}, { status: 0, output: "" }).remote, "next", [m], false)).toThrow("gave no result")
})

test("only known stages and NNNN_name.sql files are migrated", () => {
  const any = host({}, { status: 0, output: "Applied" })
  expect(() => migrate(any.remote, "prod", [m], false)).toThrow("no operator project")
  expect(() => migrate(any.remote, "next", [{ ...m, name: "../x.sql" }], false)).toThrow("NNNN_name.sql")
})

test("a new migration is published atomically: temporary file, hash check, no-overwrite link", () => {
  const fresh = host({}, { status: 0, output: "Applied 0002_connector_binding.sql\n" })
  migrate(fresh.remote, "next", [m], false)
  const copy = fresh.commands[0] ?? ""
  expect(copy).toContain('mktemp "$t.partial.XXXXXX"')
  expect(copy).toContain(`= "${m.sha256}" ]`)
  expect(copy).toContain('ln "$tmp" "$t"')
  expect(copy).not.toContain("install -m")
  // A copy whose bytes arrive wrong fails before anything is published or applied.
  const mangled: Remote = (command) => command.includes("migrations apply") ? { status: 0, output: "Applied" } : { status: 4, output: "copy hash mismatch" }
  expect(() => migrate(mangled, "next", [m], false)).toThrow("copying 0002_connector_binding.sql failed")
})

test("the deploy step writes the deployment's billing mode once, after the migrations, in one statement", () => {
  for (const billing of [true, false]) {
    const fresh = host({}, { status: 0, output: "Applied 0002_connector_binding.sql\n" })
    expect(migrate(fresh.remote, "next", [m], billing)).toBe(`Applied 0002_connector_binding.sql\nbilling mode: ${billing ? "on" : "off"}`)
    const writes = fresh.commands.filter((command) => command.includes("billing_mode"))
    expect(writes).toHaveLength(1)
    expect(fresh.commands.at(-1)).toBe(writes[0])
    // The statement it sends, run against the migrated schema, sets exactly that mode, from either one.
    const statement = /--command '([^']*)'/.exec(writes[0] ?? "")?.[1] ?? ""
    const db = new Database(":memory:")
    const dir = join(import.meta.dir, "../worker/migrations")
    for (const file of readdirSync(dir).filter((name) => name.endsWith(".sql")).sort()) db.exec(readFileSync(join(dir, file), "utf8"))
    for (const before of [0, 1]) {
      db.run(`update billing_mode set enabled = ${before}`)
      db.run(statement)
      expect(db.query("select id, enabled from billing_mode").all()).toEqual([{ id: 1, enabled: billing ? 1 : 0 }])
    }
  }
  // A failed write fails the step, so the deploy stops; an unreachable D1 says so.
  expect(() => migrate(host({}, { status: 0, output: "Applied 0002_connector_binding.sql" }, { status: 1, output: "no such table" }).remote, "next", [m], true)).toThrow("writing the billing mode failed")
  expect(() => migrate(host({}, { status: 0, output: "Applied 0002_connector_binding.sql" }, { status: 1, output: "Connection refused" }).remote, "next", [m], true)).toThrow(D1Unreachable)
})
