import { expect, test } from "bun:test"
import type { StudioScope } from "../src/lib/definitions.ts"
import type { RecordsQuery } from "../src/lib/records-query.ts"
import type { Transport } from "../src/lib/transport.ts"
import type { DecidedScope, Workspace } from "../src/lib/workspace.tsx"

/**
 * #1762: a live record read without a decided workspace does not compile. Each `@ts-expect-error` row
 * is checked by `tsc` (`bun run typecheck` covers `test/`): if the line under it ever compiles, the
 * directive is unused and the typecheck fails. The rows are types only and never run.
 */
declare const session: StudioScope
declare const query: RecordsQuery
declare const workspace: Workspace
declare const transport: Transport
declare const readRecordsAtScope: typeof import("../src/lib/raw/records.ts").readRecordsAtScope
declare const readObjectAtScope: typeof import("../src/lib/raw/record.ts").readObjectAtScope
declare const readFindings: typeof import("../src/lib/raw/record.ts").readFindings
declare const views: import("../src/lib/views.ts").ViewsClient
declare const ProfilePanel: typeof import("../src/grid/profile-panel.tsx").ProfilePanel
declare const QuarantinePanel: typeof import("../src/grid/quarantine-panel.tsx").QuarantinePanel
declare const RecordsGrid: typeof import("../src/grid/records-grid.tsx").RecordsGrid
declare const concept: import("../src/lib/catalog.ts").Concept

export function rows(): readonly unknown[] {
  return [
    // A session's scope (or any scope the mode did not answer) is not a decided one.
    // @ts-expect-error: a StudioScope is not a DecidedScope
    readRecordsAtScope("t", query, session),
    // @ts-expect-error: a StudioScope is not a DecidedScope
    readObjectAtScope("t", "id", session),
    // @ts-expect-error: a StudioScope is not a DecidedScope
    readFindings("t", "id", session),
    // @ts-expect-error: a saved view's preview reads only at a decided workspace
    views.preview("t", query, session),
    // @ts-expect-error: a DecidedScope cannot be written as a literal
    ((): DecidedScope => ({ orgId: "o", environmentId: "e" }))(),
    // @ts-expect-error: the record panels read at a decided workspace only
    ProfilePanel({ objectType: "t", scope: session }),
    // @ts-expect-error: the record panels read at a decided workspace only
    QuarantinePanel({ objectType: "t", scope: session }),
    // A workspace's reader exists only once it is decided.
    // @ts-expect-error: deciding and unavailable workspaces have no reader
    workspace.reader.records("t", query),
    // @ts-expect-error: only a live workspace has a scope
    workspace.scope,
    // A grid reads through a decided workspace's reader, and nothing else.
    // @ts-expect-error: a grid has no reader of its own
    RecordsGrid({ concept }),
    // @ts-expect-error: the grid takes no scope or read function: only a reader
    RecordsGrid({ concept, scope: session, read: async () => ({ kind: "none" }) }),
    // Records are not read through the mode-deciding transport.
    // @ts-expect-error: the transport serves no record
    transport.object("t", "id"),
    // @ts-expect-error: the transport serves no records page
    transport.objectPage("t"),
  ]
}

export function decided(): readonly unknown[] {
  if (workspace.kind === "recorded") {
    return [
      workspace.reader.records("t", query),
      workspace.reader.object("t", "id"),
      // @ts-expect-error: the recording keeps no findings, and its reader reads nothing live
      workspace.reader.findings("t", "id"),
      // @ts-expect-error: the recording has no workspace scope
      workspace.scope,
    ]
  }
  if (workspace.kind !== "live") return []
  // A live workspace's scope and reader are what every live read takes.
  return [
    readRecordsAtScope("t", query, workspace.scope),
    readObjectAtScope("t", "id", workspace.reader.scope),
    workspace.reader.findings("t", "id"),
    views.preview("t", query, workspace.scope),
    RecordsGrid({ concept, reader: workspace.reader }),
  ]
}

test("the type rows are types only: tsc checks them, nothing runs", () => {
  expect(typeof rows).toBe("function")
  expect(typeof decided).toBe("function")
})
