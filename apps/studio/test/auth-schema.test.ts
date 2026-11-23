import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { authSchemaSql, oauthSchemaSql } from "../scripts/auth-schema.ts"

test("the committed D1 migration is exactly Better Auth's schema for worker/auth.ts", async () => {
  const committed = readFileSync(new URL("../worker/migrations/0001_better_auth.sql", import.meta.url), "utf8")
  expect(committed).toBe(await authSchemaSql())
})

test("0007 is exactly what the assistants' OAuth server adds to 0001", async () => {
  const committed = readFileSync(new URL("../worker/migrations/0007_oauth_provider.sql", import.meta.url), "utf8")
  expect(committed).toBe(await oauthSchemaSql())
})
