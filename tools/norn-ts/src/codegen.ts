/**
 * `norn codegen --ts`: one deterministic, types-only `.d.ts` for an admitted definition set.
 *
 * It declares `ActionMap` (each action id → its `input` as a caller sends it and the `output` it
 * declares), `ObjectMap` (each object type → its stored fields) and `DefinitionsDigest` (the
 * content digest of the definitions it typed). Exact decimals are `string`, never a JS `number`
 * (VAL-EXACT-1). The file declares no runtime value; the generic client in `@norn/sdk` takes
 * `ActionMap` as its type parameter.
 *
 * The input is the caller's input, as `GET /v1/actions` serves it (`norn_api::action_catalog`):
 * a field the server derives from the verified actor (ACTION-ACTOR-1, `fromCaller`) is not part of
 * it, because the kernel refuses any caller value for it, and a field another derivation fills
 * when absent (`inputRequired: false`) is optional.
 *
 * Generation fails closed. A schema keyword, property type or entity shape outside the closed
 * dialect, or two definitions that claim the same name, stops the run: types never widen to
 * `unknown`, and the output never depends on artifact order. Shared value types and supertypes may
 * resolve from base artifacts (`--base`, or a pack's registry base); only the set itself is typed.
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
const compare = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0)
const same = (a: unknown, b: unknown): boolean => canonicalJson(a) === canonicalJson(b)

const doc = (text: string): string => text.replaceAll("*/", "*\\/").replace(/\s+/g, " ").trim()
const key = (name: string): string => JSON.stringify(name)
const pad = (depth: number): string => "  ".repeat(depth)
const pointer = (name: string): string => name.replaceAll("~", "~0").replaceAll("/", "~1")
const JSON_DOMAIN = "array,boolean,null,number,object,string"

/** Display labels (D66) admitted beside any closed schema node; they never change its type. */
const LABELS = ["title", "description"]
/**
 * The members each closed schema type admits, exactly as `norn_expr::schema::Schema::parse_action`
 * admits them. An admitted set already satisfies this; codegen re-checks so a set that was not
 * admitted can never yield types wider than the dialect.
 */
const ADMITTED: Readonly<Record<string, readonly string[]>> = {
  number: ["type", "nornType"],
  object: ["type", "properties", "required", "additionalProperties"],
  array: ["type", "items"],
  string: ["type", "enum"],
  integer: ["type"],
  boolean: ["type"],
  null: ["type"],
}
const SCALARS = new Set(["string", "integer", "number", "boolean"])

type Mode = "input" | "output"

function decimalDoc(norn: Node): string {
  const parts = [`Exact decimal text${typeof norn["scale"] === "number" ? `, scale ${norn["scale"]}` : ""}`]
  if (record(norn["unit"]) && typeof norn["unit"]["code"] === "string") parts.push(`unit ${norn["unit"]["code"]}`)
  return `${parts.join(", ")}.`
}

/** A schema node's JSDoc: its title and description, then what its type cannot say. */
function schemaDoc(node: unknown): string | null {
  if (!record(node)) return null
  const notes = [node["title"], node["description"]].filter((v): v is string => typeof v === "string").map(doc)
  const text = notes.join(" — ")
  const decimal = record(node["nornType"]) ? decimalDoc(node["nornType"]) : null
  return [text, decimal].filter((v): v is string => v !== null && v !== "").join(" ") || null
}

/** A closed action schema node as a TypeScript type. */
function schemaType(schema: unknown, at: string, depth: number, mode: Mode, root = false): string {
  if (!record(schema)) throw new CodegenError(at, "a schema node is not an object")
  const type = schema["type"]
  if (Array.isArray(type)) return unionType(schema, type, at, depth, mode)
  if (typeof type !== "string" || ADMITTED[type] === undefined) throw new CodegenError(at, `schema type ${JSON.stringify(type)} is not in the closed dialect`)
  const allowed = [...ADMITTED[type]!, ...LABELS, ...(root && mode === "input" && type === "object" ? ["x-norn"] : [])]
  const unknown = Object.keys(schema).find((member) => !allowed.includes(member))
  if (unknown !== undefined) throw new CodegenError(at, `\`${unknown}\` is not admitted in a closed \`${type}\` schema`)
  for (const label of LABELS) {
    if (schema[label] !== undefined && typeof schema[label] !== "string") throw new CodegenError(at, `\`${label}\` is not a string`)
  }
  switch (type) {
    case "string": {
      const values = schema["enum"]
      if (values === undefined) return "string"
      if (!Array.isArray(values) || values.length === 0 || !values.every((v) => typeof v === "string") || new Set(values).size !== values.length) {
        throw new CodegenError(at, "`enum` is not a non-empty list of distinct strings")
      }
      return values.map((v) => JSON.stringify(v)).join(" | ")
    }
    case "integer": return "number"
    case "number": {
      const norn = schema["nornType"]
      if (norn === undefined) return "number"
      if (record(norn) && norn["kind"] === "decimal") return "string"
      throw new CodegenError(at, "`nornType` on a number is not a decimal")
    }
    case "boolean": return "boolean"
    case "null": return "null"
    case "array": return `ReadonlyArray<${schemaType(schema["items"], `${at}/items`, depth, mode)}>`
    case "object": return objectType(schema, at, depth, mode, new Set(), new Set())
    default: throw new CodegenError(at, `schema type ${JSON.stringify(type)} is not in the closed dialect`)
  }
}

/** A type array: a nullable leaf, or the complete JSON value domain. */
function unionType(schema: Node, members: readonly unknown[], at: string, depth: number, mode: Mode): string {
  if (!members.every((m) => typeof m === "string")) throw new CodegenError(at, "a type array holds a non-name")
  const names = [...new Set(members as string[])].sort()
  if (names.length !== members.length) throw new CodegenError(at, "a type array repeats a name")
  if (names.join() === JSON_DOMAIN) {
    if (Object.keys(schema).length !== 1) throw new CodegenError(at, "the JSON value domain leaf admits only `type`")
    return "NornJson"
  }
  const inner = names.filter((name) => name !== "null")
  if (names.length !== 2 || inner.length !== 1) throw new CodegenError(at, `type array ${names.join(" | ")} is neither a nullable leaf nor the JSON value domain`)
  // ActionIR inputs admit a nullable scalar only; nullable records and lists are RuleIR forms.
  if (mode === "input" && !SCALARS.has(inner[0]!)) throw new CodegenError(at, `a nullable \`${inner[0]}\` is not admitted in an action input`)
  return `${schemaType({ ...schema, type: inner[0] }, at, depth, mode)} | null`
}

/**
 * A closed object. `omit`: members the caller never sends (actor-derived); `optional`: members a
 * derivation fills when absent. Both apply to the input root only.
 */
function objectType(schema: Node, at: string, depth: number, mode: Mode, omit: ReadonlySet<string>, optional: ReadonlySet<string>): string {
  if (schema["additionalProperties"] !== false) throw new CodegenError(at, "an object schema is not closed (`additionalProperties: false`)")
  const properties = schema["properties"] ?? {}
  if (!record(properties)) throw new CodegenError(at, "`properties` is not an object")
  const requiredList = schema["required"] ?? []
  if (!Array.isArray(requiredList) || !requiredList.every((name) => typeof name === "string" && name in properties)) {
    throw new CodegenError(at, "`required` names a member the object does not declare")
  }
  const required = new Set(requiredList as string[])
  const names = Object.keys(properties).filter((name) => !omit.has(name)).sort(compare)
  if (names.length === 0) return "{}"
  const lines = names.map((name) => {
    const child = properties[name]
    const comment = schemaDoc(child)
    const present = required.has(name) && !optional.has(name)
    return `${comment === null ? "" : `${pad(depth + 1)}/** ${comment} */\n`}${pad(depth + 1)}readonly ${key(name)}${present ? "" : "?"}: ${schemaType(child, `${at}/properties/${pointer(name)}`, depth + 1, mode)}`
  })
  return `{\n${lines.join("\n")}\n${pad(depth)}}`
}

/**
 * ACTION-ACTOR-1 (`norn_kernel::actor_derived_fields`): a derivation the caller may not pre-empt
 * (`inputRequired: false`) that reads the verified actor. The kernel decides "reads" on the parsed
 * expression and treats an unparseable one as reading it; codegen has no parser, so it counts any
 * `actor` reference outside a member access as a read. That can only drop a field, never offer one
 * the kernel refuses.
 */
const READS_ACTOR = /(^|[^A-Za-z0-9_$.])actor\b/

function callerInput(payload: Node, at: string): string {
  const schema = payload["inputSchema"]
  if (!record(schema)) throw new CodegenError(`${at}#/inputSchema`, "the input schema is not an object")
  if (schema["type"] !== "object") throw new CodegenError(`${at}#/inputSchema`, "an action input is not a closed object")
  const derive = payload["derive"] ?? []
  if (!Array.isArray(derive)) throw new CodegenError(`${at}#/derive`, "`derive` is not a list")
  const omit = new Set<string>(), optional = new Set<string>()
  derive.forEach((step: unknown, index) => {
    if (!record(step) || typeof step["field"] !== "string" || typeof step["expression"] !== "string" || typeof step["inputRequired"] !== "boolean") {
      throw new CodegenError(`${at}#/derive/${index}`, "a derivation is not `{ field, expression, inputRequired }`")
    }
    if (step["inputRequired"]) return
    if (READS_ACTOR.test(step["expression"])) omit.add(step["field"])
    else optional.add(step["field"])
  })
  // Validate the whole root (including the omitted members) before projecting it.
  schemaType(schema, `${at}#/inputSchema`, 2, "input", true)
  return objectType(schema, `${at}#/inputSchema`, 2, "input", omit, optional)
}

interface Property { readonly id: string; readonly type: unknown; readonly constraints?: unknown; readonly description?: unknown }

const nullable = (type: string, constraints: unknown): string =>
  record(constraints) && constraints["nullable"] === true && !type.endsWith("| null") ? `${type} | null` : type

/** One EntityIR v2 property type as the TypeScript type of its stored value. */
function propertyType(type: unknown, at: string, catalog: Catalog, depth: number, seen: readonly string[] = []): string {
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
      if (shared === undefined) throw new CodegenError(at, `value type \`${name}\` is in neither the set nor its base; pass its catalog with --base`)
      if (seen.includes(name)) throw new CodegenError(at, `value type \`${name}\` refers to itself`)
      shared.used = true
      return nullable(propertyType(shared.def.type, `${at}(${name})`, catalog, depth, [...seen, name]), shared.def.constraints)
    }
    default: throw new CodegenError(at, `property type ${JSON.stringify(type["kind"])} is not in EntityIR v2`)
  }
}

/** EntityIR v2 has one absence policy, nullable: every stored field is present, maybe `null`. */
function fieldsType(properties: readonly Property[], at: string, catalog: Catalog, depth: number, seen: readonly string[] = []): string {
  if (properties.length === 0) return "{}"
  const ids = new Set<string>()
  const lines = [...properties].sort((a, b) => compare(String(a?.id), String(b?.id))).map((property) => {
    if (!record(property) || typeof property.id !== "string") throw new CodegenError(at, "a property has no id")
    if (ids.has(property.id)) throw new CodegenError(at, `property \`${property.id}\` is declared twice`)
    ids.add(property.id)
    const type = nullable(propertyType(property.type, `${at}/${property.id}`, catalog, depth + 1, seen), property.constraints)
    const comment = typeof property.description === "string" ? `${pad(depth + 1)}/** ${doc(property.description)} */\n` : ""
    return `${comment}${pad(depth + 1)}readonly ${key(property.id)}: ${type}`
  })
  return `{\n${lines.join("\n")}\n${pad(depth)}}`
}

interface ObjectTypeDef { readonly name: string; readonly extends?: string; readonly properties?: readonly Property[] }
interface Owned<T> { readonly artifact: Envelope; readonly value: T; readonly base: boolean; used?: boolean }
type Catalog = Map<string, Owned<unknown> & { readonly def: Property }>
interface Envelope { readonly artifactId: string; readonly kind: string; readonly version?: unknown; readonly payload?: unknown }

/** The entity payload members EntityIR v2 defines; one of the shape members must be present. */
const ENTITY_MEMBERS = new Set(["entityVersion", "extensions", "mutationAuthority", "objectType", "interface", "objectTypeExtension", "propertyTypes", "markings"])
const ENTITY_SHAPES = ["objectType", "interface", "objectTypeExtension", "propertyTypes", "markings"]

interface Collected {
  readonly actions: Map<string, Owned<Node>>
  readonly objects: Map<string, Owned<ObjectTypeDef>>
  readonly extensions: Owned<{ readonly target: string; readonly properties: readonly Property[] }>[]
  readonly catalog: Catalog
}

/** Every definition of the set and its base, refusing two that claim one name. */
function collect(set: readonly unknown[], base: readonly unknown[]): Collected {
  const out: Collected = { actions: new Map(), objects: new Map(), extensions: [], catalog: new Map() }
  const envelopes = new Map<string, { readonly artifact: Envelope; readonly base: boolean }>()
  for (const [list, isBase] of [[set, false], [base, true]] as const) {
    for (const value of list) {
      if (!isArtifact(value)) throw new CodegenError(isBase ? "--base" : "$", "the set holds a document that is not an artifact envelope")
      const artifact = value as unknown as Envelope
      const prior = envelopes.get(artifact.artifactId)
      if (prior !== undefined) {
        // The set's own copy of a base artifact is the one typed; any other repeat must be identical.
        if (prior.base === isBase && !same(prior.artifact, artifact)) throw new CodegenError(artifact.artifactId, "the artifact appears twice with different content")
        continue
      }
      envelopes.set(artifact.artifactId, { artifact, base: isBase })
    }
  }
  const claim = <T>(map: Map<string, Owned<T>>, name: string, owned: Owned<T>, what: string) => {
    const prior = map.get(name)
    if (prior !== undefined && !same(prior.value, owned.value)) {
      throw new CodegenError(owned.artifact.artifactId, `${what} \`${name}\` is also defined by ${prior.artifact.artifactId}`)
    }
    if (prior === undefined || (prior.base && !owned.base)) map.set(name, owned)
  }
  for (const { artifact, base: isBase } of [...envelopes.values()].sort((a, b) => compare(a.artifact.artifactId, b.artifact.artifactId))) {
    const at = artifact.artifactId
    const payload = artifact.payload
    if (artifact.kind === "action") {
      if (!record(payload)) throw new CodegenError(at, "an action payload is not an object")
      if (isBase) continue
      const actionId = payload["actionId"]
      if (typeof actionId !== "string") throw new CodegenError(at, "an action has no actionId")
      if (out.actions.has(actionId)) throw new CodegenError(at, `action \`${actionId}\` is also defined by ${out.actions.get(actionId)!.artifact.artifactId}`)
      out.actions.set(actionId, { artifact, value: payload, base: false })
      continue
    }
    if (artifact.kind !== "entity") continue
    if (!record(payload)) throw new CodegenError(at, "an entity payload is not an object")
    if ("fields" in payload || "objectKind" in payload) throw new CodegenError(at, "EntityIR v1 is not typed by codegen; recompile the type as EntityIR v2")
    if (payload["entityVersion"] !== "2.0") throw new CodegenError(at, `entityVersion ${JSON.stringify(payload["entityVersion"])} is not EntityIR v2`)
    const stray = Object.keys(payload).find((member) => !ENTITY_MEMBERS.has(member))
    if (stray !== undefined) throw new CodegenError(at, `entity payload member \`${stray}\` is not EntityIR v2`)
    if (!ENTITY_SHAPES.some((shape) => shape in payload)) throw new CodegenError(at, "an entity payload declares no object type, interface, extension or catalog")
    if ("objectType" in payload) {
      const type = payload["objectType"]
      if (!record(type) || typeof type["name"] !== "string") throw new CodegenError(at, "an object type has no name")
      claim(out.objects, type["name"], { artifact, value: type as unknown as ObjectTypeDef, base: isBase }, "object type")
    }
    if ("objectTypeExtension" in payload) {
      const extension = payload["objectTypeExtension"]
      if (!record(extension) || typeof extension["target"] !== "string") throw new CodegenError(at, "an extension names no target")
      const properties = extension["properties"] ?? []
      if (!Array.isArray(properties)) throw new CodegenError(at, "an extension's properties are not a list")
      out.extensions.push({ artifact, value: { target: extension["target"], properties: properties as Property[] }, base: isBase })
    }
    if ("propertyTypes" in payload) {
      const defs = payload["propertyTypes"]
      if (!Array.isArray(defs)) throw new CodegenError(at, "`propertyTypes` is not a list")
      for (const def of defs as Node[]) {
        if (!record(def) || typeof def["name"] !== "string") throw new CodegenError(at, "a shared value type has no name")
        const property: Property = { id: def["name"], type: def["type"], constraints: def["constraints"], description: def["description"] }
        claim(out.catalog as Map<string, Owned<unknown>>, def["name"], { artifact, value: def, base: isBase, def: property } as Owned<unknown> & { def: Property }, "value type")
      }
    }
  }
  out.extensions.sort((a, b) => compare(a.artifact.artifactId, b.artifact.artifactId))
  return out
}

/** A type's stored fields: its supertype's, narrowed by its own, then extension members (which never shadow). */
function objectProperties(name: string, found: Collected, chain: readonly string[] = []): Property[] {
  const owned = found.objects.get(name)
  if (owned === undefined) throw new CodegenError(chain.at(-1) ?? name, `supertype \`${name}\` is in neither the set nor its base; pass it with --base`)
  if (chain.includes(name)) throw new CodegenError(owned.artifact.artifactId, `\`extends\` cycles through \`${name}\``)
  owned.used = true
  const byId = new Map<string, Property>()
  const type = owned.value
  if (type.extends !== undefined) for (const p of objectProperties(type.extends, found, [...chain, name])) byId.set(p.id, p)
  for (const p of type.properties ?? []) byId.set(p.id, p)
  for (const extension of found.extensions) {
    if (extension.value.target !== name) continue
    extension.used = true
    for (const p of extension.value.properties) {
      if (byId.has(p.id)) throw new CodegenError(extension.artifact.artifactId, `extension member \`${p.id}\` shadows a member of \`${name}\``)
      byId.set(p.id, p)
    }
  }
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

export interface GenerateOptions {
  /** Artifacts that resolve value types, supertypes and extensions but are not typed themselves. */
  readonly base?: readonly unknown[]
}

/** The `.d.ts` text for an artifact set. Pure: the same set and base always yield the same bytes, in any order. */
export function generateTypes(artifacts: readonly unknown[], options: GenerateOptions = {}): string {
  const found = collect(artifacts, options.base ?? [])
  for (const extension of found.extensions) {
    if (!extension.base && !found.objects.has(extension.value.target)) throw new CodegenError(extension.artifact.artifactId, `extension target \`${extension.value.target}\` is in neither the set nor its base`)
  }
  const actionEntries = [...found.actions.keys()].sort(compare).map((actionId) => {
    const { artifact, value: payload } = found.actions.get(actionId)!
    const comment = schemaDoc(payload)
    const input = callerInput(payload, artifact.artifactId)
    const output = schemaType(payload["outputSchema"], `${artifact.artifactId}#/outputSchema`, 2, "output")
    return `${comment === null ? "" : `  /** ${comment} */\n`}  readonly ${key(actionId)}: {\n    readonly input: ${input}\n    readonly output: ${output}\n  }`
  })
  const objectEntries = [...found.objects.entries()].filter(([, owned]) => !owned.base).map(([name]) => name).sort(compare).map((name) => {
    const owned = found.objects.get(name)!
    return `  readonly ${key(name)}: ${fieldsType(objectProperties(name, found), owned.artifact.artifactId, found.catalog, 1)}`
  })
  // The digest covers what the types were made from: the set's typed definitions and every base
  // definition they resolved through.
  const typed = [
    ...[...found.actions.values()],
    ...[...found.objects.values()].filter((o) => !o.base || o.used),
    ...found.extensions.filter((e) => !e.base || e.used),
    ...[...found.catalog.values()].filter((c) => !c.base || c.used),
  ].map((owned) => ({ artifactId: owned.artifact.artifactId, version: String(owned.artifact.version), payload: owned.artifact.payload as Json }))
  const unique = [...new Map(typed.map((t) => [t.artifactId, t])).values()].sort((a, b) => compare(a.artifactId, b.artifactId))
  const digest = createHash("sha256").update(canonicalJson(unique)).digest("hex")
  const block = (entries: string[]) => (entries.length === 0 ? "{}" : `{\n${entries.join("\n")}\n}`)
  return [
    ...HEADER,
    "/** sha256 over the canonical JSON of the definitions this file types and every base definition they resolve through. */",
    `export type DefinitionsDigest = "sha256:${digest}"`,
    "",
    "/** Each action id → the input a caller sends (actor-derived fields excluded) and the output it declares. */",
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

const USAGE = "usage: norn codegen --ts <artifacts.json|artifact-dir|pack-dir|project-dir> [--base <artifacts.json|artifact-dir>]... [--out FILE.d.ts] [--check] [--org ID --env ID]\n"

/**
 * `norn codegen --ts`. A compiled set (JSON) is read as admitted; a pack or project is compiled
 * and admitted by the checker first (over its base), and a set the checker refuses emits nothing.
 */
export async function codegenCli(args: readonly string[]): Promise<number> {
  const positional: string[] = [], bases: string[] = []
  let ts = false, check = false, out: string | undefined, orgId: string | undefined, environmentId: string | undefined
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!
    if (arg === "--ts") ts = true
    else if (arg === "--check") check = true
    else if (arg === "--out" || arg === "--org" || arg === "--env" || arg === "--base") {
      const value = args[++i]
      if (value === undefined) { process.stderr.write(USAGE); return 2 }
      if (arg === "--out") out = value
      else if (arg === "--org") orgId = value
      else if (arg === "--env") environmentId = value
      else bases.push(value)
    } else if (arg.startsWith("-")) { process.stderr.write(USAGE); return 2 }
    else positional.push(arg)
  }
  const input = positional[0]
  if (!ts || input === undefined || positional.length > 1 || (check && out === undefined)) { process.stderr.write(USAGE); return 2 }
  for (const path of [input, ...bases]) if (!existsSync(path)) { process.stderr.write(`codegen: ${path} does not exist\n`); return 2 }
  let text: string
  try {
    const base = bases.flatMap((path) => readArtifactSet(resolve(path)))
    const set = await loadSet(resolve(input), base, orgId, environmentId)
    text = generateTypes(set.artifacts, { base: set.base })
  } catch (error) {
    if (error instanceof AdmissionRefused) { process.stdout.write(`${JSON.stringify(error.report)}\n`); return error.exitCode }
    if (error instanceof CodegenError) { process.stderr.write(`codegen: ${error.message}\n`); return 1 }
    throw error
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

async function loadSet(input: string, base: readonly unknown[], orgId: string | undefined, environmentId: string | undefined): Promise<{ readonly artifacts: readonly unknown[]; readonly base: readonly unknown[] }> {
  const { compilePack, admit, packModules } = await import("./pack.ts")
  const directory = statSync(input).isDirectory()
  const project = directory && existsSync(join(input, "norn.json"))
  const isPack = project || input.endsWith(".ts") || (directory && packModules(input).length > 0)
  if (!isPack) return { artifacts: readArtifactSet(input), base }
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
  const resolved = [...pack.baseArtifacts, ...base]
  const { report } = admit(checker, { ...pack, baseArtifacts: resolved as typeof pack.baseArtifacts })
  if (!report.admitted) throw new AdmissionRefused(report, 1)
  return { artifacts: [...pack.artifacts], base: resolved }
}
