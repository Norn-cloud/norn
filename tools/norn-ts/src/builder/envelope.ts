/**
 * The artifact envelope every builder emits (`irVersion` 0.1). Scope and
 * provenance are install-time facts from {@link EnvelopeOptions}, never part
 * of a definition, so a builder-written and a compiler-written definition
 * produce the same bytes.
 */
import { type Json, toJson } from "../canonical.ts"

export interface EnvelopeOptions {
  /** Envelope scope (`orgId`, `environmentId`, …). */
  readonly scope: Readonly<Record<string, string>>
  /** Repo-relative path of the authoring module (envelope provenance). */
  readonly sourcePath: string
  /** Repo-relative registry package path; when present, emit a registry artifact envelope. */
  readonly registryPackagePath?: string
}

export interface EnvelopeMeta {
  readonly kind: "entity" | "rule" | "action" | "policy" | "mapping" | "law" | "connector" | "source_schema" | "claim" | "ruling"
  readonly artifactId: string
  readonly namespace: string
  /** Human-readable title (the envelope `name`). */
  readonly name: string
  readonly version: string
  readonly dependencies?: ReadonlyArray<Json>
}

export function envelope(meta: EnvelopeMeta, payload: unknown, options: EnvelopeOptions): Json {
  const registry = options.registryPackagePath !== undefined
  const registryRoot = options.registryPackagePath?.replaceAll("\\", "/").replace(/\/+$/, "")
  return toJson({
    irVersion: "0.1",
    artifactId: meta.artifactId,
    kind: meta.kind,
    namespace: meta.namespace,
    name: meta.name,
    version: meta.version,
    lifecycle: registry ? "active" : "draft",
    scope: registry ? {} : { ...options.scope },
    dependencies: meta.dependencies ?? [],
    provenance: {
      source: registry ? "system" : "authoring_sdk",
      sourcePath: registry ? `${registryRoot}/artifacts/${meta.artifactId}.json` : options.sourcePath,
    },
    payload,
  })
}
