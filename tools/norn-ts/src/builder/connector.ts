/** Minimum read-only connector authoring surface; Rust owns admission and execution. */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

/** Authentication declarations contain logical secret references, never resolved credentials. */
export type ConnectorAuth =
  | { readonly kind: "none" }
  | { readonly kind: "api_key"; readonly authId: string; readonly placement: "header" | "query"; readonly name: string; readonly secretRef: string; readonly prefix?: string; readonly format?: { readonly kind: "raw" } | { readonly kind: "pair"; readonly separator: string } }
  | { readonly kind: "oauth2"; readonly authId: string; readonly authorizationUrl: string; readonly tokenUrl: string; readonly scopes: ReadonlyArray<string>; readonly clientSecretRef?: string }

interface ResourceOptions {
  readonly resourceId: string
  readonly endpoint: string
  readonly itemsPath: string
  readonly missingItemsIsEmpty?: boolean
  readonly queryParams?: Readonly<Record<string, string>>
}

/** Existing HTTP resources. POST is for host-approved read queries such as Cloudflare analytics; the runtime closes the query and forbids mutations. */
export type ConnectorResource =
  | (ResourceOptions & { readonly kind: "paginated"; readonly method: "GET" | "POST"; readonly pageParam: string; readonly pageSizeParam?: string; readonly pageMode?: "page" | "offset"; readonly pageStart?: number; readonly pageSize?: number })
  | (ResourceOptions & { readonly kind: "cursor"; readonly cursorParam: string; readonly nextCursorPath: string })
  | (ResourceOptions & { readonly kind: "delta"; readonly sinceParam: string; readonly updatedAtPath: string })
  | { readonly kind: "webhook"; readonly resourceId: string; readonly eventType: string; readonly payloadPath: string }
  | { readonly kind: "csv_import"; readonly resourceId: string; readonly delimiter: "," | ";" | "\t"; readonly columns: ReadonlyArray<string> }

export type ConnectorMapping =
  | { readonly kind: "field"; readonly resourceId: string; readonly sourcePath: string; readonly targetObjectKind: string; readonly targetField: string }
  | { readonly kind: "external_id"; readonly resourceId: string; readonly sourcePath: string; readonly targetObjectKind: string; readonly externalSource: string }

export interface ConnectorSpec {
  readonly id: string
  readonly title: string
  readonly version: string
  readonly auth: ConnectorAuth
  readonly resources: ReadonlyArray<ConnectorResource>
  readonly mappings: ReadonlyArray<ConnectorMapping>
  readonly cursor?: { readonly cursorId: string; readonly resourceId: string; readonly storageKey: string }
  readonly deadLetterPolicy?: { readonly maxFailures: number; readonly route: "ledger" | "queue" }
  /** Ordered feeds lower to the existing provider-neutral norn.poll/1 extension. */
  readonly poll?: { readonly feeds: ReadonlyArray<{ readonly resourceId: string; readonly recordKeyPath?: string; readonly fanout?: { readonly parentResourceId: string; readonly pathBinding: string; readonly keyPath: string } }> }
}

export interface ConnectorDef extends ConnectorSpec {
  readonly _tag: "Connector"
  readonly at?: SourceLocation
}

export function connector(meta: ConnectorSpec & { readonly at?: SourceLocation | undefined }): ConnectorDef {
  const { at, ...rest } = meta
  return located({ _tag: "Connector" as const, ...rest }, at)
}

export function lowerConnector(def: ConnectorDef, options: EnvelopeOptions):
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> } {
  const diagnostics = new Diagnostics()
  // Direct data builders share the syntax compiler's closed credential boundary.
  const closed = (value: object, allowed: ReadonlyArray<string>, path: string): void => {
    for (const key of Object.keys(value)) if (!allowed.includes(key)) diagnostics.report("NORN-FLOW-LITERAL", `unknown connector member ${path}.${key}`, def.at)
  }
  closed(def, ["_tag", "at", "id", "title", "version", "auth", "resources", "mappings", "cursor", "deadLetterPolicy", "poll"], "connector")
  const authKeys = def.auth.kind === "none" ? ["kind"] : def.auth.kind === "api_key" ? ["kind", "authId", "placement", "name", "secretRef", "prefix", "format"] : ["kind", "authId", "authorizationUrl", "tokenUrl", "scopes", "clientSecretRef"]
  closed(def.auth, authKeys, "auth")
  if (def.auth.kind === "api_key" && def.auth.format) closed(def.auth.format, def.auth.format.kind === "pair" ? ["kind", "separator"] : ["kind"], "auth.format")
  const common = ["kind", "resourceId", "endpoint", "itemsPath", "missingItemsIsEmpty", "queryParams"]
  for (const r of def.resources) closed(r, r.kind === "paginated" ? [...common, "method", "pageParam", "pageSizeParam", "pageMode", "pageStart", "pageSize"] : r.kind === "cursor" ? [...common, "cursorParam", "nextCursorPath"] : r.kind === "delta" ? [...common, "sinceParam", "updatedAtPath"] : r.kind === "webhook" ? ["kind", "resourceId", "eventType", "payloadPath"] : ["kind", "resourceId", "delimiter", "columns"], "resource")
  for (const m of def.mappings) closed(m, ["kind", "resourceId", "sourcePath", "targetObjectKind", m.kind === "field" ? "targetField" : "externalSource"], "mapping")
  if (def.cursor) closed(def.cursor, ["cursorId", "resourceId", "storageKey"], "cursor")
  if (def.deadLetterPolicy) closed(def.deadLetterPolicy, ["maxFailures", "route"], "deadLetterPolicy")
  if (def.poll) {
    closed(def.poll, ["feeds"], "poll")
    for (const feed of def.poll.feeds) {
      closed(feed, ["resourceId", "recordKeyPath", "fanout"], "poll.feed")
      if (feed.fanout) closed(feed.fanout, ["parentResourceId", "pathBinding", "keyPath"], "poll.feed.fanout")
    }
  }
  if (!/^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/.test(def.id)) diagnostics.report("NORN-FLOW-LITERAL", "connector id must be dotted lower-case segments", def.at)
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  return { ok: true, artifact: envelope(
    { kind: "connector", artifactId: def.id, namespace: def.id.slice(0, def.id.indexOf(".")), name: def.title, version: def.version },
    {
      connectorVersion: "0.1", auth: def.auth, resources: def.resources, mappings: def.mappings,
      cursor: def.cursor, deadLetterPolicy: def.deadLetterPolicy,
      extensions: def.poll ? { "norn.poll/1": def.poll } : undefined,
    }, options,
  ) }
}
