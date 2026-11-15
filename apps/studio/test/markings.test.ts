import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { catalogOf } from "../src/lib/catalog.ts"
import { markingCatalog, markingLabel, projectionWords, revealRoles } from "../src/lib/markings.ts"
import type { Artifact } from "../src/lib/transport.ts"

// The registry's own privacy pack: the marking catalog, the reveal policy and the Party overlay
// that marks display_name, over the recorded three-systems world.
const json = (path: string) => JSON.parse(readFileSync(new URL(`../../../${path}`, import.meta.url), "utf8"))
const overlay = json("registry/privacy/overlays/canon.Party.entity.json") as Artifact
const world: Artifact[] = [
  ...(json("fixtures/demo/explorer/three-systems.definitions.json") as Artifact[]).map((artifact) => artifact.artifactId === overlay.artifactId ? overlay : artifact),
  json("registry/privacy/artifacts/privacy.markings.entity.json"),
  json("registry/privacy/artifacts/privacy.reveal.policy.json"),
]

test("markings come from the server's declarations: labels, projection, and the roles the reveal policy names", () => {
  const markings = markingCatalog(world)
  expect(markingLabel(markings, "pii.contact")).toBe("Contact")
  expect(markingLabel(markings, "pii.identity")).toBe("Government identity")
  expect(projectionWords(markings.declared.get("pii.contact"))).toBe('Shown as a stable pseudonym ("Customer #…")')
  expect(projectionWords(markings.declared.get("pii.identity"))).toBe("Never shown; the server withholds it")
  expect(revealRoles(markings, "pii.contact")).toEqual(["admin", "owner"])
  // A marking a type names but the catalog does not declare keeps its id and no reveal role.
  expect(markingLabel(markings, "pii.health")).toBe("pii.health")
  expect(revealRoles(markings, "pii.health")).toEqual([])
})

test("a fact carries its own markings and its named type's", () => {
  const catalog = catalogOf(world)
  const party = catalog.byName.get("canon.Party")
  expect(party?.fields.find(({ id }) => id === "display_name")?.markings).toEqual(["pii.contact"])
  expect(party?.fields.find(({ id }) => id === "kind")?.markings).toBeUndefined()
  // Without the overlay, display_name is unmarked: nothing is inferred.
  const plain = catalogOf(json("fixtures/demo/explorer/three-systems.definitions.json")).byName.get("canon.Party")
  expect(plain?.fields.find(({ id }) => id === "display_name")?.markings).toBeUndefined()
  // A type whose property is a named, marked type (core.email_address) inherits the marking.
  const typed = catalogOf([...world, { ...overlay, artifactId: "demo.Contact.entity", payload: { entityVersion: "2.0", objectType: { name: "demo.Contact", properties: [{ id: "email", type: { kind: "ref", name: "core.email_address" } }] } } } as unknown as Artifact])
  expect(typed.byName.get("demo.Contact")?.fields[0]?.markings).toEqual(["pii.contact"])
})
