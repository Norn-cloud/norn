import { expect, test } from "bun:test"
import { join } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import * as C from "../src/builder/connector.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const options = { scope: { orgId: "acme", environmentId: "dev" }, sourcePath: "connector.source.ts" }
const compiled = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/connector.source.ts"), options)
const artifacts = () => {
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  return compiled.artifacts
}

test("connector syntax lowers the complete canonical envelope and payload", () => {
  expect(artifacts()).toHaveLength(3)
  expect(artifacts()[1]).toEqual({
    irVersion: "0.1", artifactId: "private.ledger", kind: "connector", namespace: "private", name: "Ledger", version: "0.1.0", lifecycle: "draft",
    scope: options.scope, dependencies: [], provenance: { source: "authoring_sdk", sourcePath: options.sourcePath },
    payload: {
      connectorVersion: "0.1",
      auth: { kind: "api_key", authId: "ledger", placement: "header", name: "Authorization", secretRef: "LEDGER_READ_KEY", prefix: "token ", format: { kind: "pair", separator: ":" } },
      resources: [{ kind: "paginated", resourceId: "entries", endpoint: "/entries", method: "GET", pageParam: "start", pageSizeParam: "limit", pageMode: "offset", pageStart: 0, pageSize: 100, itemsPath: "/data", queryParams: { fields: "id" } }],
      mappings: [{ kind: "field", resourceId: "entries", sourcePath: "/id", targetObjectKind: "ledger.Entry", targetField: "name" }],
    },
  })
})

test("syntax and data builders emit identical canonical bytes", () => {
  const built = C.lowerConnector(C.connector({
    id: "private.mail", title: "Mailbox", version: "0.1.0",
    auth: { kind: "oauth2", authId: "mail", authorizationUrl: "https://example.com/oauth/authorize", tokenUrl: "https://example.com/oauth/token", scopes: ["mail.readonly"], clientSecretRef: "MAIL_CLIENT_SECRET" },
    resources: [{ kind: "cursor", resourceId: "messages", endpoint: "/messages", cursorParam: "pageToken", nextCursorPath: "/nextPageToken", itemsPath: "/messages", missingItemsIsEmpty: true }],
    mappings: [{ kind: "external_id", resourceId: "messages", sourcePath: "/id", targetObjectKind: "mail.Message", externalSource: "mail" }],
    cursor: { cursorId: "mail", resourceId: "messages", storageKey: "mail.messages" }, deadLetterPolicy: { maxFailures: 3, route: "ledger" },
    poll: { feeds: [{ resourceId: "messages", recordKeyPath: "/id" }] },
  }), options)
  if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
  expect(canonicalJson(artifacts()[0])).toBe(canonicalJson(built.artifact))
})

test("all auth declarations are admitted by the Rust checker", () => {
  if (!checker) return
  const admitted = checker.checkArtifactSet(artifacts())
  expect(admitted.diagnostics.diagnostics).toEqual([])
  expect(admitted.admitted).toBe(true)
})

for (const fixture of ["connector-secret", "connector-write"]) test(`${fixture} is rejected at the typed authoring boundary`, () => {
  const result = compileDefinitions(join(TOOL_ROOT, `fixtures/definitions/invalid/${fixture}.ts`), options)
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.some((d) => d.code === "NORN-FLOW-TYPE")).toBe(true)
})

test("Rust admission rejects missing resource references", () => {
  if (!checker) return
  const invalid = JSON.parse(canonicalJson(artifacts()[0]))
  invalid.payload.cursor.resourceId = "missing"
  expect(checker.checkArtifactSet([invalid]).admitted).toBe(false)
})

test("const aliases cannot carry undeclared credential members into IR", () => {
  const result = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/invalid/connector-aliased-secret.ts"), options)
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.some((d) => d.code === "NORN-FLOW-LITERAL")).toBe(true)
})


test("direct data builders reject undeclared credential members through aliases", () => {
  const auth = { kind: "api_key" as const, authId: "source", placement: "header" as const, name: "Authorization", secretRef: "SOURCE_READ_KEY", token: "must-never-survive" }
  const result = C.lowerConnector(C.connector({ id: "private.source", title: "Source", version: "0.1.0", auth, resources: [], mappings: [] }), options)
  expect(result.ok).toBe(false)
  expect(JSON.stringify(result)).not.toContain("must-never-survive")
})
