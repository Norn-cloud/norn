import { expect, test } from "bun:test"
import { D1Unreachable, migrate, type Migration, type Remote } from "../scripts/celld-migrate.ts"

const sql = 'CREATE TABLE "connector_binding" ("id" text);\n'
const m: Migration = { name: "0002_connector_binding.sql", sha256: new Bun.CryptoHasher("sha256").update(sql).digest("hex"), sql }

/** A host whose operator project holds `files` (name → sha256) and whose apply answers `apply`. */
function host(files: Record<string, string>, apply: { status: number; output: string }) {
  const commands: string[] = []
  const remote: Remote = (command, input) => {
    commands.push(command)
    if (command.includes("migrations apply")) return apply
    const name = /migrations\/([^ ;"]+\.sql)/.exec(command)?.[1] ?? ""
    if (files[name] !== undefined) return { status: 0, output: `${files[name]}\n` }
    files[name] = new Bun.CryptoHasher("sha256").update(input ?? "").digest("hex")
    return { status: 0, output: "installed\n" }
  }
  return { remote, commands, files }
}

test("migrations are copied into the stage's operator project and applied there, idempotently", () => {
  const fresh = host({}, { status: 0, output: "Applied 0002_connector_binding.sql\n" })
  expect(migrate(fresh.remote, "next", [m])).toBe("Applied 0002_connector_binding.sql")
  expect(fresh.files["0002_connector_binding.sql"]).toBe(m.sha256)
  expect(fresh.commands.at(-1)).toBe("/opt/norn-demo/d1-next migrations apply norn-studio-auth-next 2>&1")
  expect(fresh.commands[0]).toContain("/opt/norn-demo/ops-next/migrations/0002_connector_binding.sql")
  // A repeat run copies nothing new and applies nothing, and passes.
  const again = host({ "0002_connector_binding.sql": m.sha256 }, { status: 0, output: "No migrations to apply.\n" })
  expect(migrate(again.remote, "next", [m])).toBe("No migrations to apply.")
})

test("an applied migration that changed is refused before anything is applied", () => {
  const edited = host({ "0002_connector_binding.sql": "f".repeat(64) }, { status: 0, output: "Applied x" })
  expect(() => migrate(edited.remote, "next", [m])).toThrow("never edited")
  expect(edited.commands.some((c) => c.includes("migrations apply"))).toBe(false)
})

test("an unreachable D1 is its own refusal, so the deploy stops before shipping", () => {
  const refused = host({}, { status: 1, output: "error: connect to http://127.0.0.1:39673/runtime/__D1Database: Connection refused (os error 111)\n" })
  expect(() => migrate(refused.remote, "next", [m])).toThrow(D1Unreachable)
  const noHost: Remote = () => ({ status: 255, output: "ssh: connect to host hetzner port 22: Connection timed out" })
  expect(() => migrate(noHost, "next", [m])).toThrow(D1Unreachable)
  const broken = host({}, { status: 1, output: "near \"ALTR\": syntax error\n" })
  expect(() => migrate(broken.remote, "next", [m])).toThrow("migrations apply failed")
  expect(() => migrate(broken.remote, "next", [m])).not.toThrow(D1Unreachable)
  expect(() => migrate(host({}, { status: 0, output: "" }).remote, "next", [m])).toThrow("gave no result")
})

test("only known stages and NNNN_name.sql files are migrated", () => {
  const any = host({}, { status: 0, output: "Applied" })
  expect(() => migrate(any.remote, "prod", [m])).toThrow("no operator project")
  expect(() => migrate(any.remote, "next", [{ ...m, name: "../x.sql" }])).toThrow("NNNN_name.sql")
})

test("a new migration is published atomically: temporary file, hash check, no-overwrite link", () => {
  const fresh = host({}, { status: 0, output: "Applied 0002_connector_binding.sql\n" })
  migrate(fresh.remote, "next", [m])
  const copy = fresh.commands[0] ?? ""
  expect(copy).toContain('mktemp "$t.partial.XXXXXX"')
  expect(copy).toContain(`= "${m.sha256}" ]`)
  expect(copy).toContain('ln "$tmp" "$t"')
  expect(copy).not.toContain("install -m")
  // A copy whose bytes arrive wrong fails before anything is published or applied.
  const mangled: Remote = (command) => command.includes("migrations apply") ? { status: 0, output: "Applied" } : { status: 4, output: "copy hash mismatch" }
  expect(() => migrate(mangled, "next", [m])).toThrow("copying 0002_connector_binding.sql failed")
})
