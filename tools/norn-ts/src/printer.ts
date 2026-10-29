/**
 * RuleIR v2 → TypeScript printer (T7-TS-3, #2691): the "JSON → TS" leg of the
 * `jdm-editor` round trip. A decision edited in `jdm-editor` is RuleIR JSON;
 * {@link printDecision} turns it into a `Norn.decision` module whose graph is
 * the editor's JDM JSON verbatim (ZEN Expression cells untouched) and whose
 * closed input/output schemas are printed back as Effect Schema syntax.
 * Compiling the module reproduces the artifact's canonical bytes.
 *
 * The printer is the inverse of the closed-schema projection
 * (`src/compiler/schema.ts`) and refuses anything that projection cannot
 * produce, so an unprintable artifact fails loudly instead of drifting.
 */

import { NonJsonValue, toJson } from "./canonical.ts"

type Obj = { readonly [key: string]: unknown }

const isObject = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v)

export class UnprintableArtifact extends Error {}

const IDENTIFIER = /^[A-Za-z_$][A-Za-z0-9_$]*$/
/** Array-index-like names: JavaScript orders them numerically in every object, so declaration order is lost. */
const INDEX_LIKE = /^(0|[1-9][0-9]*)$/
const key = (name: string): string => (IDENTIFIER.test(name) ? name : JSON.stringify(name))
const pad = (depth: number): string => "  ".repeat(depth)

function only(value: Obj, allowed: ReadonlyArray<string>, where: string): void {
  for (const k of Object.keys(value)) if (!allowed.includes(k)) throw new UnprintableArtifact(`${where}: member ${k} has no TypeScript authoring form`)
}

/** Effect Schema syntax for one closed value schema. */
export function printSchema(schema: unknown, depth = 0, where = "$"): string {
  if (!isObject(schema)) throw new UnprintableArtifact(`${where}: not a schema object`)
  if (Array.isArray(schema.type)) {
    const nonNull = schema.type.filter((type) => type !== "null")
    if (schema.type.length !== 2 || nonNull.length !== 1 || !schema.type.includes("null")) {
      throw new UnprintableArtifact(`${where}: only one closed type or null is authored`)
    }
    return `Schema.NullOr(${printSchema({ ...schema, type: nonNull[0] }, depth, where)})`
  }
  switch (schema.type) {
    case "string": {
      only(schema, ["type", "enum"], where)
      const values = schema.enum
      if (values === undefined) return "Schema.String"
      if (!Array.isArray(values) || values.length === 0 || !values.every((v) => typeof v === "string")) throw new UnprintableArtifact(`${where}: enum must list strings`)
      return values.length === 1 ? `Schema.Literal(${JSON.stringify(values[0])})` : `Schema.Literals([${values.map((v) => JSON.stringify(v)).join(", ")}])`
    }
    case "integer":
      only(schema, ["type"], where)
      return "Schema.Int"
    case "number":
      only(schema, ["type"], where)
      return "Schema.Finite"
    case "boolean":
      only(schema, ["type"], where)
      return "Schema.Boolean"
    case "null":
      only(schema, ["type"], where)
      return "Schema.Null"
    case "array":
      only(schema, ["type", "items"], where)
      return `Schema.Array(${printSchema(schema.items, depth, `${where}[]`)})`
    case "object": {
      only(schema, ["type", "properties", "required", "additionalProperties"], where)
      if (schema.additionalProperties !== false) throw new UnprintableArtifact(`${where}: only closed records (additionalProperties: false) are authored`)
      const properties = schema.properties
      if (!isObject(properties)) throw new UnprintableArtifact(`${where}: properties must be an object`)
      if (Array.isArray(schema.required) && schema.required.length === 0) throw new UnprintableArtifact(`${where}: the projection omits an empty required list`)
      const required = schema.required ?? []
      if (!Array.isArray(required) || !required.every((r) => typeof r === "string" && Object.hasOwn(properties, r))) throw new UnprintableArtifact(`${where}: required must list declared properties`)
      if (new Set(required).size !== required.length) throw new UnprintableArtifact(`${where}: required lists a member twice`)
      for (const name of Object.keys(properties)) {
        if (name === "__proto__" || INDEX_LIKE.test(name)) throw new UnprintableArtifact(`${where}: member name ${JSON.stringify(name)} cannot keep its declared order or identity in a JavaScript object`)
      }
      // Required members first, in `required` order, so the projection reproduces that order;
      // then optional members sorted, so the module does not depend on input key order.
      const order = [...(required as string[]), ...Object.keys(properties).filter((k) => !required.includes(k)).sort()]
      if (order.length === 0) return "Schema.Struct({})"
      const fields = order.map((name) => {
        const inner = printSchema(properties[name], depth + 1, `${where}.${name}`)
        return `${pad(depth + 1)}${key(name)}: ${required.includes(name) ? inner : `Schema.optionalKey(${inner})`},`
      })
      return `Schema.Struct({\n${fields.join("\n")}\n${pad(depth)}})`
    }
    default:
      throw new UnprintableArtifact(`${where}: schema type ${JSON.stringify(schema.type)} has no authoring form`)
  }
}

/** A JSON value as a TypeScript literal (JSON is TypeScript expression syntax), keys sorted. */
function printJson(value: unknown, depth: number, where: string): string {
  try {
    return JSON.stringify(toJson(value), null, 2).split("\n").join(`\n${pad(depth)}`)
  } catch (error) {
    if (error instanceof NonJsonValue) throw new UnprintableArtifact(`${where}: ${error.message}`)
    throw error
  }
}

function str(value: unknown, where: string): string {
  if (typeof value !== "string") throw new UnprintableArtifact(`${where}: expected a string`)
  return value
}

/**
 * Print a RuleIR v2 `rule` artifact as a TypeScript module declaring one
 * `Norn.decision` named `exportName`. Scope and provenance are build options,
 * so the envelope must otherwise be the builder's (`irVersion` 0.1, `draft`,
 * required version-pinned rule dependencies).
 */
export function printDecision(artifact: unknown, exportName = "Decision"): string {
  if (!isObject(artifact)) throw new UnprintableArtifact("not an artifact envelope")
  only(artifact, ["irVersion", "artifactId", "kind", "namespace", "name", "version", "lifecycle", "scope", "dependencies", "provenance", "payload"], "envelope")
  if (artifact.irVersion !== "0.1" || artifact.kind !== "rule" || artifact.lifecycle !== "draft") throw new UnprintableArtifact("envelope must be an irVersion 0.1 draft rule")
  if (!Array.isArray(artifact.dependencies)) throw new UnprintableArtifact("envelope dependencies must be a list")
  const dependencies = artifact.dependencies.map((dependency, i) => {
    if (!isObject(dependency)) throw new UnprintableArtifact("dependencies[" + i + "] must be an object")
    only(dependency, ["artifactId", "kind", "version", "required"], "dependencies[" + i + "]")
    if (dependency.kind !== "rule" || dependency.required !== true || typeof dependency.version !== "string") {
      throw new UnprintableArtifact("dependencies[" + i + "] must be a required, version-pinned rule dependency")
    }
    return { artifactId: str(dependency.artifactId, "dependencies[" + i + "].artifactId"), version: dependency.version }
  })
  // Scope and provenance are build options: only what they can restore is printable.
  const provenance = artifact.provenance
  if (!isObject(provenance) || provenance.source !== "authoring_sdk" || typeof provenance.sourcePath !== "string") throw new UnprintableArtifact("provenance must be { source: \"authoring_sdk\", sourcePath }")
  only(provenance, ["source", "sourcePath"], "provenance")
  if (!isObject(artifact.scope) || !Object.values(artifact.scope).every((v) => typeof v === "string")) throw new UnprintableArtifact("scope must map dimensions to strings")
  const p = artifact.payload
  if (!isObject(p)) throw new UnprintableArtifact("payload must be an object")
  only(p, ["ruleId", "ruleVersion", "contract", "triggers", "slice", "inputSchema", "outputSchema", "scopeRequired", "graph"], "payload")
  if (p.ruleVersion !== "2.0") throw new UnprintableArtifact("only RuleIR v2 is authored")
  const id = str(p.ruleId, "ruleId")
  if (artifact.namespace !== id.slice(0, id.indexOf("."))) throw new UnprintableArtifact("the envelope namespace must be the rule id's first segment")
  const lines: string[] = [
    `// Printed from ${str(artifact.artifactId, "artifactId")} (RuleIR v2 -> TypeScript, tools/norn-ts/src/printer.ts).`,
    `import { Schema } from "effect"`,
    `import { Norn } from "@norn/authoring"`,
    ``,
    `export const ${exportName} = Norn.decision({`,
    `  id: ${JSON.stringify(id)},`,
  ]
  if (artifact.artifactId !== `${id}.rule`) lines.push(`  artifactId: ${JSON.stringify(artifact.artifactId)},`)
  lines.push(`  title: ${JSON.stringify(str(artifact.name, "name"))},`, `  version: ${JSON.stringify(str(artifact.version, "version"))},`, `  contract: ${JSON.stringify(str(p.contract, "contract"))},`)
  if (dependencies.length > 0) {
    lines.push(
      "  dependencies: [",
      ...dependencies.map((dependency) => "    { artifactId: " + JSON.stringify(dependency.artifactId) + ", version: " + JSON.stringify(dependency.version) + " },"),
      "  ],",
    )
  }
  if (p.triggers !== undefined) {
    if (!Array.isArray(p.triggers)) throw new UnprintableArtifact("triggers must be a list")
    if (p.triggers.length === 0) throw new UnprintableArtifact("triggers: the builder omits an empty list")
    const triggers = p.triggers.map((t, i) => {
      if (!isObject(t)) throw new UnprintableArtifact(`triggers[${i}] must be an object`)
      only(t, ["on", "objectType", "transition", "source"], `triggers[${i}]`)
      const parts = [`on: ${JSON.stringify(t.on)}`]
      if (t.objectType !== undefined) parts.push(`type: ${JSON.stringify(t.objectType)}`)
      if (t.transition !== undefined) parts.push(`transition: ${JSON.stringify(t.transition)}`)
      if (t.source !== undefined) parts.push(`source: ${JSON.stringify(t.source)}`)
      return `    { ${parts.join(", ")} },`
    })
    lines.push(`  triggers: [`, ...triggers, `  ],`)
  }
  if (p.slice !== undefined) {
    if (!Array.isArray(p.slice)) throw new UnprintableArtifact("slice must be a list")
    if (p.slice.length === 0) throw new UnprintableArtifact("slice: the builder omits an empty list")
    const slice = p.slice.map((s, i) => {
      if (!isObject(s)) throw new UnprintableArtifact(`slice[${i}] must be an object`)
      only(s, ["name", "objectType", "key", "optional"], `slice[${i}]`)
      if (s.optional !== undefined && s.optional !== true) throw new UnprintableArtifact(`slice[${i}].optional: the builder omits false`)
      const optional = s.optional === true ? ", optional: true" : ""
      return `    { name: ${JSON.stringify(s.name)}, type: ${JSON.stringify(s.objectType)}, key: ${JSON.stringify(toJson(s.key))}${optional} },`
    })
    lines.push(`  slice: [`, ...slice, `  ],`)
  }
  lines.push(
    `  input: ${printSchema(p.inputSchema, 1, "inputSchema")},`,
    `  output: ${printSchema(p.outputSchema, 1, "outputSchema")},`,
    `  scopeRequired: ${JSON.stringify(p.scopeRequired)},`,
    `  graph: Norn.unsafeZen(${printJson(p.graph, 1, "graph")}),`,
    `})`,
    ``,
  )
  return lines.join("\n")
}
