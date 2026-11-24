/**
 * `norn codegen --ts`: one deterministic, types-only `.d.ts` for an admitted definition set.
 *
 * It declares `ActionMap` (each action id → its `input` and `output` from the action's closed
 * `inputSchema` / `outputSchema`), `ObjectMap` (each object type → its stored fields) and
 * `DefinitionsDigest` (the content digest of the definitions it typed). Exact decimals are
 * `string`, never a JS `number` (VAL-EXACT-1). The file declares no runtime value; the generic
 * client in `@norn/sdk` takes `ActionMap` as its type parameter.
 *
 * A schema or property type the generator does not model fails the run: it never widens to
 * `unknown`, so generated types are never weaker than the definitions they claim to describe.
 */
import { createHash } from "node:crypto"
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { canonicalJson, type Json } from "./canonical.ts"
import { isArtifact, readArtifacts } from "./pack.ts"

export class CodegenError extends Error {
  constructor(readonly at: string, message: string) {
    super(`${at}: ${message}`)
    this.name = "CodegenError"
  }
}

type Node = { readonly [key: string]: unknown }
const record = (value: unknown): value is Node => typeof value === "object" && value !== null && !Array.isArray(value)

/** JSON Schema members that annotate or constrain a value without changing its TypeScript shape. */
const ANNOTATIONS = new Set([
  "title", "description", "x-norn", "nornType", "format", "pattern", "minLength", "maxLength", "minimum", "maximum",
  "exclusiveMinimum", "exclusiveMaximum", "minItems", "maxItems", "uniqueItems", "multipleOf", "default", "examples",
  "$comment", "propertyNames", "required", "properties", "additionalProperties", "items", "enum", "type", "const",
])
const JSON_DOMAIN = "array,boolean,null,number,object,string"

const doc = (text: string): string => text.replaceAll("*/", "*\\/").replace(/\s+/g, " ").trim()
const key = (name: string): string => JSON.stringify(name)
const pad = (depth: number): string => "  ".repeat(depth)

function decimalDoc(norn: Node): string {
  const parts = [`Exact decimal text${typeof norn["scale"] === "number" ? `, scale ${norn["scale"]}` : ""}`]
  const unit = record(norn["unit"]) && typeof norn["unit"]["code"] === "string" ? norn["unit"]["code"] : null
  if (unit !== null) parts.push(`unit ${unit}`)
  return `${parts.join(", ")}.`
}

/** A closed input/output schema node as a TypeScript type. */
function schemaType(schema: unknown, at: string, depth: number): string {
  if (!record(schema)) throw new CodegenError(at, "a schema node is not an object")
  for (const member of Object.keys(schema)) {
    if (!ANNOTATIONS.has(member)) throw new CodegenError(at, `schema member \`${member}\` is not modelled by codegen`)
  }
  const type = schema["type"]
  if (Array.isArray(type)) {
    const names = [...new Set(type.map(String))].sort()
    if (names.join() === JSON_DOMAIN) return "NornJson"
    const inner = names.filter((name) => name !== "null")
    if (names.includes("null") && inner.length === 1) return `${schemaType({ ...schema, type: inner[0] }, at, depth)} | null`
    throw new CodegenError(at, `type union ${names.join(" | ")} is not modelled by codegen`)
  }
  if (schema["const"] !== undefined) {
    if (typeof schema["const"] !== "string") throw new CodegenError(at, "only string `const` is modelled by codegen")
    return JSON.stringify(schema["const"])
  }
  switch (type) {
    case "string": {
      const values = schema["enum"]
      if (values === undefined) return "string"
      if (!Array.isArray(values) || values.length === 0 || !values.every((v) => typeof v === "string")) throw new CodegenError(at, "a string enum lists non-string values")
      return values.map((v) => JSON.stringify(v)).join(" | ")
    }
    case "integer": return "number"
    case "number": {
      const norn = schema["nornType"]
      if (norn === undefined) return "number"
      if (record(norn) && norn["kind"] === "decimal") return "string"
      throw new CodegenError(at, `nornType ${JSON.stringify(norn)} is not modelled by codegen`)
    }
    case "boolean": return "boolean"
    case "array": return `ReadonlyArray<${schemaType(schema["items"], `${at}/items`, depth)}>`
    case "object": {
      if (schema["additionalProperties"] !== false) throw new CodegenError(at, "an object schema is not closed (`additionalProperties: false`)")
      const properties = schema["properties"] ?? {}
      if (!record(properties)) throw new CodegenError(at, "`properties` is not an object")
      const required = new Set(Array.isArray(schema["required"]) ? schema["required"].map(String) : [])
      const names = Object.keys(properties).sort()
      if (names.length === 0) return "{}"
      const lines = names.map((name) => {
        const child = properties[name]
        const childAt = `${at}/properties/${name.replaceAll("~", "~0").replaceAll("/", "~1")}`
        const notes: string[] = []
        if (record(child)) {
          if (typeof child["title"] === "string") notes.push(doc(child["title"]))
          if (typeof child["description"] === "string") notes.push(doc(child["description"]))
          if (record(child["nornType"]) && child["nornType"]["kind"] === "decimal") notes.push(decimalDoc(child["nornType"]))
          if (typeof child["format"] === "string") notes.push(`Format \`${child["format"]}\`.`)
        }
        const comment = notes.length === 0 ? "" : `${pad(depth + 1)}/** ${notes.join(" ")} */\n`
        return `${comment}${pad(depth + 1)}readonly ${key(name)}${required.has(name) ? "" : "?"}: ${schemaType(child, childAt, depth + 1)}`
      })
      return `{\n${lines.join("\n")}\n${pad(depth)}}`
    }
    default: throw new CodegenError(at, `schema type ${JSON.stringify(type)} is not modelled by codegen`)
  }
}

interface Property { readonly id: string; readonly type: unknown; readonly constraints?: unknown; readonly description?: unknown }

/** One EntityIR v2 property type as the TypeScript type of its stored value. */
function propertyType(type: unknown, at: string, catalog: ReadonlyMap<string, Property>, depth: number, seen: ReadonlySet<string> = new Set()): string {
  if (!record(type)) throw new CodegenError(at, "a property type is not an object")
  switch (type["kind"]) {
    case "text": case "timestamp": case "date": case "duration": return "string"
    case "integer": return "number"
    case "decimal": case "decimal-binding": return "string"
    case "boolean": return "boolean"
    case "json-ext": return "NornJson"
    case "enum": {
      const values = type["values"]
      if (!Array.isArray(values) || values.length === 0) throw new CodegenError(at, "an enum lists no values")
      return values.map((v: { value?: unknown }) => {
        if (typeof v?.value !== "string") throw new CodegenError(at, "an enum value is not a string")
        return JSON.stringify(v.value)
      }).join(" | ")
    }
    case "record": {
      const members = type["members"]
      if (!Array.isArray(members)) throw new CodegenError(at, "a record lists no members")
      return fieldsType(members as Property[], at, catalog, depth, seen)
    }
    case "ref": {
      const name = type["name"]
      if (typeof name !== "string") throw new CodegenError(at, "a ref names no value type")
      const shared = catalog.get(name)
      if (shared === undefined) throw new CodegenError(at, `value type \`${name}\` is not in the artifact set; include its property type catalog`)
      if (seen.has(name)) throw new CodegenError(at, `value type \`${name}\` refers to itself`)
      return nullable(propertyType(shared.type, `${at}(${name})`, catalog, depth, new Set([...seen, name])), shared.constraints)
    }
    default: throw new CodegenError(at, `property type ${JSON.stringify(type["kind"])} is not modelled by codegen`)
  }
}

const nullable = (type: string, constraints: unknown): string =>
  record(constraints) && constraints["nullable"] === true && !type.endsWith("| null") ? `${type} | null` : type

/** EntityIR v2 has one absence policy, nullable: every stored field is present, maybe `null`. */
function fieldsType(properties: readonly Property[], at: string, catalog: ReadonlyMap<string, Property>, depth: number, seen: ReadonlySet<string> = new Set()): string {
  if (properties.length === 0) return "{}"
  const lines = [...properties].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)).map((property) => {
    if (typeof property?.id !== "string") throw new CodegenError(at, "a property has no id")
    const type = nullable(propertyType(property.type, `${at}/${property.id}`, catalog, depth + 1, seen), property.constraints)
    const comment = typeof property.description === "string" ? `${pad(depth + 1)}/** ${doc(property.description)} */\n` : ""
    return `${comment}${pad(depth + 1)}readonly ${key(property.id)}: ${type}`
  })
  return `{\n${lines.join("\n")}\n${pad(depth)}}`
}

interface ObjectTypeDef { readonly name: string; readonly extends?: string; readonly properties?: readonly Property[] }

/** The definitions a set types: its actions, object types, extensions and shared value types. */
function collect(artifacts: readonly unknown[]) {
  const actions = new Map<string, { readonly artifactId: string; readonly payload: Node }>()
  const objects = new Map<string, { readonly artifactId: string; readonly type: ObjectTypeDef }>()
  const extensions: { readonly artifactId: string; readonly target: string; readonly properties: readonly Property[] }[] = []
  const catalog = new Map<string, Property>()
  const typed: Json[] = []
  for (const artifact of artifacts) {
    if (!isArtifact(artifact)) throw new CodegenError("$", "the set holds a document that is not an artifact envelope")
    const envelope = artifact as unknown as { readonly artifactId: string; readonly kind: string; readonly version?: unknown; readonly payload?: unknown }
    const payload = envelope.payload
    if (!record(payload)) continue
    const at = envelope.artifactId
    if (envelope.kind === "action") {
      const actionId = payload["actionId"]
      if (typeof actionId !== "string") throw new CodegenError(at, "an action has no actionId")
      if (actions.has(actionId)) throw new CodegenError(at, `action \`${actionId}\` appears twice in the set`)
      actions.set(actionId, { artifactId: at, payload })
      typed.push({ artifactId: at, version: String(envelope.version), payload: payload as Json })
    } else if (envelope.kind === "entity") {
      if ("fields" in payload && "objectKind" in payload) throw new CodegenError(at, "EntityIR v1 is not modelled by codegen; recompile the type as EntityIR v2")
      if (record(payload["objectType"])) {
        const type = payload["objectType"] as unknown as ObjectTypeDef
        if (objects.has(type.name)) throw new CodegenError(at, `object type \`${type.name}\` appears twice in the set`)
        objects.set(type.name, { artifactId: at, type })
      }
      if (record(payload["objectTypeExtension"])) {
        const extension = payload["objectTypeExtension"] as { target?: unknown; properties?: readonly Property[] }
        if (typeof extension.target !== "string") throw new CodegenError(at, "an extension names no target")
        extensions.push({ artifactId: at, target: extension.target, properties: extension.properties ?? [] })
      }
      if (Array.isArray(payload["propertyTypes"])) {
        for (const def of payload["propertyTypes"] as { name?: unknown; type?: unknown; constraints?: unknown; description?: unknown }[]) {
          if (typeof def.name !== "string") throw new CodegenError(at, "a shared value type has no name")
          catalog.set(def.name, { id: def.name, type: def.type, constraints: def.constraints, description: def.description })
        }
      }
      typed.push({ artifactId: at, version: String(envelope.version), payload: payload as Json })
    }
  }
  return { actions, objects, extensions, catalog, typed }
}

/** A type's stored fields: its supertype's, narrowed by its own, then extension members. */
function objectProperties(name: string, objects: ReturnType<typeof collect>["objects"], extensions: ReturnType<typeof collect>["extensions"], chain: readonly string[] = []): Property[] {
  const found = objects.get(name)
  if (found === undefined) throw new CodegenError(chain.at(-1) ?? name, `supertype \`${name}\` is not in the artifact set`)
  if (chain.includes(name)) throw new CodegenError(found.artifactId, `\`extends\` cycles through \`${name}\``)
  const byId = new Map<string, Property>()
  if (found.type.extends !== undefined) for (const p of objectProperties(found.type.extends, objects, extensions, [...chain, name])) byId.set(p.id, p)
  for (const p of found.type.properties ?? []) byId.set(p.id, p)
  for (const extension of extensions) if (extension.target === name) for (const p of extension.properties) byId.set(p.id, p)
  return [...byId.values()]
}

const HEADER = [
  "// Generated by `norn codegen --ts`. Do not edit: change the definitions and regenerate.",
  "// Types only: this file declares no runtime value.",
  "",
  "/** Any JSON value: an open `json` leaf. */",
  "export type NornJson = null | boolean | number | string | ReadonlyArray<NornJson> | { readonly [key: string]: NornJson }",
  "",
]

/** The `.d.ts` text for an artifact set. Pure: the same set always yields the same bytes. */
export function generateTypes(artifacts: readonly unknown[]): string {
  const { actions, objects, extensions, catalog, typed } = collect(artifacts)
  for (const extension of extensions) {
    if (!objects.has(extension.target)) throw new CodegenError(extension.artifactId, `extension target \`${extension.target}\` is not in the artifact set`)
  }
  const digest = createHash("sha256")
    .update(canonicalJson([...typed].sort((a, b) => {
      const x = (a as { artifactId: string }).artifactId, y = (b as { artifactId: string }).artifactId
      return x < y ? -1 : x > y ? 1 : 0
    })))
    .digest("hex")
  const actionEntries = [...actions.keys()].sort().map((actionId) => {
    const { artifactId, payload } = actions.get(actionId)!
    const notes = [typeof payload["title"] === "string" ? doc(payload["title"]) : null, typeof payload["description"] === "string" ? doc(payload["description"]) : null].filter((v): v is string => v !== null)
    const comment = notes.length === 0 ? "" : `  /** ${notes.join(" — ")} */\n`
    const input = schemaType(payload["inputSchema"], `${artifactId}#/inputSchema`, 2)
    const output = schemaType(payload["outputSchema"], `${artifactId}#/outputSchema`, 2)
    return `${comment}  readonly ${key(actionId)}: {\n    readonly input: ${input}\n    readonly output: ${output}\n  }`
  })
  const objectEntries = [...objects.keys()].sort().map((name) => {
    const { artifactId } = objects.get(name)!
    return `  readonly ${key(name)}: ${fieldsType(objectProperties(name, objects, extensions), artifactId, catalog, 1)}`
  })
  const block = (entries: string[]) => (entries.length === 0 ? "{}" : `{\n${entries.join("\n")}\n}`)
  return [
    ...HEADER,
    "/** sha256 over the canonical JSON of the action and entity definitions this file types. */",
    `export type DefinitionsDigest = "sha256:${digest}"`,
    "",
    "/** Each action id → the input it takes and the output it declares. */",
    `export type ActionMap = ${block(actionEntries)}`,
    "",
    "/** Each object type → its stored fields (every field present; nullable ones may be `null`). */",
    `export type ObjectMap = ${block(objectEntries)}`,
    "",
    "/** The typed surface of this definition set. */",
    "export interface NornDefinitions {",
    "  readonly actions: ActionMap",
    "  readonly objects: ObjectMap",
    "  readonly definitionsDigest: DefinitionsDigest",
    "}",
    "",
  ].join("\n")
}

/** The artifacts of a compiled set: a JSON array (`norn compile --out`), one envelope, or a directory of envelopes. */
export function readArtifactSet(path: string): unknown[] {
  if (statSync(path).isDirectory()) return readArtifacts(path).map((entry) => entry.artifact)
  const value = JSON.parse(readFileSync(path, "utf8")) as unknown
  if (Array.isArray(value)) return value
  if (isArtifact(value)) return [value]
  throw new CodegenError(path, "not an artifact set: expected a JSON array of artifact envelopes")
}

const USAGE = "usage: norn codegen --ts <artifacts.json|artifact-dir|pack-dir|project-dir> [--out FILE.d.ts] [--check] [--org ID --env ID]\n"

/**
 * `norn codegen --ts`. A compiled set (JSON) is read as admitted; a pack or project is compiled
 * and admitted by the checker first, and a set the checker refuses emits nothing.
 */
export async function codegenCli(args: readonly string[]): Promise<number> {
  const positional: string[] = []
  let ts = false, check = false, out: string | undefined, orgId: string | undefined, environmentId: string | undefined
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!
    if (arg === "--ts") ts = true
    else if (arg === "--check") check = true
    else if (arg === "--out" || arg === "--org" || arg === "--env") {
      const value = args[++i]
      if (value === undefined) { process.stderr.write(USAGE); return 2 }
      if (arg === "--out") out = value
      else if (arg === "--org") orgId = value
      else environmentId = value
    } else if (arg.startsWith("-")) { process.stderr.write(USAGE); return 2 }
    else positional.push(arg)
  }
  const input = positional[0]
  if (!ts || input === undefined || positional.length > 1 || (check && out === undefined)) { process.stderr.write(USAGE); return 2 }
  if (!existsSync(input)) { process.stderr.write(`codegen: ${input} does not exist\n`); return 2 }
  let artifacts: unknown[]
  try {
    artifacts = await loadSet(resolve(input), orgId, environmentId)
  } catch (error) {
    if (!(error instanceof AdmissionRefused)) throw error
    process.stdout.write(`${JSON.stringify(error.report)}\n`)
    return error.exitCode
  }
  let text: string
  try { text = generateTypes(artifacts) } catch (error) {
    if (!(error instanceof CodegenError)) throw error
    process.stderr.write(`codegen: ${error.message}\n`)
    return 1
  }
  if (out === undefined) { process.stdout.write(text); return 0 }
  if (check) {
    const current = existsSync(out) ? readFileSync(out, "utf8") : null
    if (current === text) return 0
    process.stderr.write(`codegen: ${out} ${current === null ? "is missing" : "is out of date"}; run norn codegen --ts ${input} --out ${out}\n`)
    return 1
  }
  writeFileSync(out, text)
  return 0
}

class AdmissionRefused extends Error {
  constructor(readonly report: unknown, readonly exitCode: number) { super("the definition set was not admitted") }
}

async function loadSet(input: string, orgId: string | undefined, environmentId: string | undefined): Promise<unknown[]> {
  const directory = statSync(input).isDirectory()
  const project = directory && existsSync(join(input, "norn.json"))
  const { compilePack, admit, packModules } = await import("./pack.ts")
  const isPack = project || input.endsWith(".ts") || (directory && packModules(input).length > 0)
  if (!isPack) return readArtifactSet(input)
  if (!project && (orgId === undefined || environmentId === undefined)) throw new AdmissionRefused({ ok: false, message: "--org and --env are required to compile a pack" }, 2)
  const pack = project
    ? await (async () => {
      const { readProject } = await import("./project.ts")
      const { compileProjectPack } = await import("./worker.ts")
      const { root, manifest } = readProject(input)
      return compileProjectPack(root, manifest)
    })()
    : compilePack(input, { scope: { orgId: orgId!, environmentId: environmentId! }, root: process.cwd(), ...(directory ? {} : { modules: [input] }) })
  if (!pack.ok) throw new AdmissionRefused(pack, 1)
  const { loadChecker } = await import("./checker-abi.ts")
  const checker = await loadChecker()
  if (!checker) throw new AdmissionRefused({ ok: false, message: "codegen admits a pack before typing it; set NORN_CHECKER_WASM_PKG (norn setup), or pass the compiled set of an admitted pack" }, 2)
  const { report } = admit(checker, pack)
  if (!report.admitted) throw new AdmissionRefused(report, 1)
  return [...pack.artifacts]
}
