import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { authSchemaSql } from "../scripts/auth-schema.ts"

test("the committed D1 migration is exactly Better Auth's schema for worker/auth.ts", async () => {
  const committed = readFileSync(new URL("../worker/migrations/0001_better_auth.sql", import.meta.url), "utf8")
  expect(committed).toBe(await authSchemaSql())
})
