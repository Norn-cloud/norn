import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { generateTypes } from "../src/codegen.ts"
import { msg, messageId } from "../src/builder/widget.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

/**
 * Studio's server-held pages (#1762 milestone C2): `registry/studio/*.ts` lower to
 * `registry/studio/artifacts/<artifactId>.json`, the definitions the demo workspace serves, and each
 * page's islands to the registry Studio is typed by. NORN_PACKS_UPDATE=1 regenerates both.
 */
const REPO = join(TOOL_ROOT, "../..")
const update = process.env.NORN_PACKS_UPDATE === "1"
const { recordPageArtifact, RECORD_PAGE_ID } = await import("../../../registry/studio/record-page.ts")

test("the record page is generated from its typed source, and its islands from the page", () => {
  const artifact = recordPageArtifact() as { readonly artifactId: string }
  expect(artifact.artifactId).toBe(RECORD_PAGE_ID)
  const path = join(REPO, `registry/studio/artifacts/${artifact.artifactId}.json`)
  const islands = join(REPO, "apps/studio/src/record/record-islands.d.ts")
  const types = generateTypes([recordPageArtifact()])
  if (update) {
    writeFileSync(path, `${JSON.stringify(artifact, null, 2)}\n`)
    writeFileSync(islands, types)
  }
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual(artifact)
  expect(readFileSync(islands, "utf8")).toBe(types)
})

test.skipIf(!checker)("the checker admits the record page on its own, and refuses a message whose id is not its text's", () => {
  const artifact = recordPageArtifact() as { payload: { ux: { catalog: { label: unknown }[] } } }
  const admitted = checker!.checkArtifactSet([artifact], { base: [] })
  if (!admitted.admitted) throw new Error(JSON.stringify(admitted.diagnostics))
  const stale = structuredClone(artifact)
  stale.payload.ux.catalog[0]!.label = { msg: { id: messageId("Read the record"), source: "Reading the record" } }
  const refused = checker!.checkArtifactSet([stale], { base: [] })
  expect(refused.admitted).toBe(false)
  expect(JSON.stringify(refused.diagnostics)).toContain("/payload/ux/catalog/0/label/msg/id")
})

test("a message's id is the hash of its text and context, the same in TypeScript as in the checker", () => {
  // norn_ir::message_id's own rows (crates/norn-ir/src/widget_ux.rs tests).
  expect(messageId("Reading the record")).toBe("mab651c50e3d7818b")
  expect(messageId("Close", "a dialog")).not.toBe(messageId("Close"))
  expect(msg("Close", { context: "a dialog" })).toEqual({ msg: { id: messageId("Close", "a dialog"), source: "Close", context: "a dialog" } })
  expect(msg("Retry")).toEqual({ msg: { id: messageId("Retry"), source: "Retry" } })
})
