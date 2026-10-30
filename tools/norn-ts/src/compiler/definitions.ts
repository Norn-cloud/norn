/**
 * Syntax compilation of ontology definitions (T7-EFX-4 #2787, T7-TS-2 #2690).
 *
 * Every top-level `const X = Norn.type({...})` / `Norn.interfaceType({...})`
 * in a root module becomes builder data (`src/builder/entity.ts`) and then one
 * `entity` artifact. As with flows, the authored module is never imported or
 * evaluated: metadata is read from literal syntax, properties from Effect
 * Schema syntax (`./schema.ts`), and references to other definitions
 * (`implements: [Order]`) are followed by resolved symbol, across local
 * modules. Definitions imported from another module are references, not
 * outputs: each artifact is emitted by the module that declares it.
 */
import { relative, resolve, sep } from "node:path"
import ts from "typescript"
import type { Diagnostic, ProfileChecker } from "../check.ts"
import type { Json } from "../canonical.ts"
import { BudgetExceeded, type ExpansionBudget } from "../builder/budget.ts"
import * as C from "../builder/connector.ts"
import * as A from "../builder/action.ts"
import * as P from "../builder/access.ts"
import * as E from "../builder/entity.ts"
import * as SS from "../builder/source-schema.ts"
import * as OP from "../builder/ontology-pack.ts"
import * as M from "../builder/mapping.ts"
import * as L from "../builder/law.ts"
import * as R from "../builder/rule.ts"
import * as X from "../builder/expr.ts"
import type * as S from "../builder/schema.ts"
import type { EnvelopeOptions } from "../builder/envelope.ts"
import type { SourceLocation } from "../builder/diagnostics.ts"
import { TOOL_ROOT } from "../profile.ts"
import { type CompilerLimits, SyntaxCompiler, checkClosure, diagnostic, exactNumber, prepareCompilation } from "./index.ts"
import { SchemaProjection } from "./schema.ts"
import { CompileError } from "./types.ts"

/** One lowered definition: an object type or interface, an action, or an access policy. */
export type Definition = E.EntityDef | E.MarkingsDef | SS.SourceSchemaDef | A.ActionDef | P.AccessDef | R.DecisionDef | M.MappingDef | L.LawDef | C.ConnectorDef | OP.OntologyPackDef

export interface DefinitionSource {
  readonly artifactId: string
  readonly pointer: string
  readonly at: SourceLocation
}

export type DefinitionsResult =
  | {
      readonly ok: true
      readonly definitions: ReadonlyArray<Definition>
      readonly artifacts: ReadonlyArray<Json>
      /** Top-level `Norn.flow` definitions the module also declares (lowered by the flow compiler, `compileFile`). */
      readonly flows: number
      readonly sourceMap: ReadonlyArray<DefinitionSource>
      readonly warnings: ReadonlyArray<Diagnostic>
    }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

const SOURCE_SCHEMA = "@norn/authoring#Norn.sourceSchema"
const ONTOLOGY_PACK = "@norn/authoring#Norn.ontologyPack"
const MARKINGS = "@norn/authoring#Norn.markings"
const TYPE = "@norn/authoring#Norn.type"
const INTERFACE = "@norn/authoring#Norn.interfaceType"
const ACTION = "@norn/authoring#Norn.action"
const ACCESS = "@norn/authoring#Norn.access"
const DECISION = "@norn/authoring#Norn.decision"
const CONNECTOR = "@norn/authoring#Norn.connector"
const MAPPING = "@norn/authoring#Norn.mapping"
const LAW = "@norn/authoring#Norn.law"
const FLOW = "@norn/authoring#Norn.flow"
const DEFINITION_CALLS: ReadonlySet<string> = new Set([SOURCE_SCHEMA, ONTOLOGY_PACK, MARKINGS, TYPE, INTERFACE, ACTION, ACCESS, DECISION, MAPPING, LAW, CONNECTOR])
const OBJECT_TYPES: ReadonlySet<string> = new Set([TYPE])
const INTERFACES: ReadonlySet<string> = new Set([INTERFACE])
const ANY_TYPE: ReadonlySet<string> = new Set([TYPE, INTERFACE])
const ACTIONS: ReadonlySet<string> = new Set([ACTION])
const DECISIONS: ReadonlySet<string> = new Set([DECISION])
/** Nesting bound for literal JSON data (access conditions, graphs). */
const MAX_DATA_DEPTH = 64
const VALUE_TYPE_CALLS = new Set(["@norn/authoring#Norn.decimal", "@norn/authoring#Norn.ref", "@norn/authoring#Norn.timestamp", "@norn/authoring#Norn.date"])
/** `DECIMAL_LITERAL_PATTERN` without the sign (the sign is a prefix `-`). */
const DECIMAL_TEXT = /^(0|[1-9][0-9]*)(\.[0-9]+)?$/

type Lowered<T> = { readonly ok: true; readonly value: T } | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

/** Profile-check a batch, then lower each accepted root with `lower`; failures carry source spans. */
function lowerEach<T>(
  files: ReadonlyArray<string>,
  overrides: Partial<CompilerLimits>,
  lower: (compiler: DefinitionCompiler, source: ts.SourceFile) => Lowered<T>,
): Lowered<T>[] {
  const { started, limits, profile, program, expansion, accepted, failed } = prepareCompilation(files, overrides)
  const results = new Map<string, Lowered<T>>()
  for (const [path, diagnostics] of failed) results.set(path, { ok: false, diagnostics })
  for (const path of accepted) {
    const source = program!.getSourceFile(path)!
    try {
      checkClosure(program!, source, limits)
      const lowered = lower(new DefinitionCompiler(profile, program!, started, limits, expansion), source)
      if (lowered.ok) expansion.addOutput(lowered.value)
      results.set(path, lowered)
    } catch (error) {
      if (error instanceof BudgetExceeded) { results.set(path, { ok: false, diagnostics: [diagnostic(source, "NORN-FLOW-UNBOUNDED", error.message)] }); continue }
      if (!(error instanceof CompileError)) throw error
      results.set(path, { ok: false, diagnostics: [diagnostic(error.node, error.code, error.message)] })
    }
  }
  return files.map((file) => results.get(resolve(file))!)
}

/**
 * Compile the ontology definitions of each root module in one shared Program.
 * `options` may vary per root (envelope provenance names each module).
 */
export function compileDefinitionFiles(
  files: ReadonlyArray<string>,
  options: EnvelopeOptions | ((path: string) => EnvelopeOptions),
  overrides: Partial<CompilerLimits> = {},
  /** `allowEmpty`: a module with no ontology definition (a flow-only module) is not an error. */
  mode: { readonly allowEmpty?: boolean } = {},
): DefinitionsResult[] {
  return lowerEach<{ readonly definitions: Definition[]; readonly artifacts: Json[]; readonly flows: number; readonly sourceMap: DefinitionSource[]; readonly warnings: Diagnostic[] }>(files, overrides, (compiler, source) => {
    const { definitions, flows } = compiler.compile(source, mode.allowEmpty === true)
    const envelopeOptions = typeof options === "function" ? options(resolve(source.fileName)) : options
    const artifacts: Json[] = []
    const diagnostics: Diagnostic[] = []
    for (const def of definitions) {
      const built =
        def._tag === "Action" ? A.lowerAction(def, envelopeOptions)
        : def._tag === "Access" ? P.lowerAccess(def, envelopeOptions)
        : def._tag === "Decision" ? R.lowerDecision(def, envelopeOptions)
        : def._tag === "Connector" ? C.lowerConnector(def, envelopeOptions)
        : def._tag === "Mapping" ? M.lowerMapping(def, envelopeOptions)
        : def._tag === "Law" ? L.lowerLaw(def, envelopeOptions)
        : def._tag === "SourceSchema" ? SS.lowerSourceSchema(def, envelopeOptions)
        : def._tag === "OntologyPack" ? OP.lowerOntologyPack(def, envelopeOptions)
        : def._tag === "Markings" ? E.lowerMarkings(def, envelopeOptions)
        : E.lowerEntity(def, envelopeOptions)
      if (built.ok) {
        artifacts.push(built.artifact)
        if ("at" in def && def.at) compiler.sourceMap.push({ artifactId: (built.artifact as { artifactId: string }).artifactId, pointer: "", at: def.at })
      }
      else for (const d of built.diagnostics) diagnostics.push({ ...diagnostic(source, d.code, d.message), ...(d.at ? { start: { line: d.at.line, column: d.at.column ?? 1 } } : {}) })
    }
    return diagnostics.length > 0 ? { ok: false, diagnostics } : { ok: true, value: { definitions, artifacts, flows, sourceMap: compiler.sourceMap, warnings: compiler.warnings } }
  }).map((r): DefinitionsResult => (r.ok ? { ok: true, ...r.value } : r))
}

export function compileDefinitions(file: string, options: EnvelopeOptions, limits: Partial<CompilerLimits> = {}, mode: { readonly allowEmpty?: boolean } = {}): DefinitionsResult {
  return compileDefinitionFiles([file], options, limits, mode)[0]!
}

export type ValueSchemasResult =
  | { readonly ok: true; readonly schemas: Readonly<Record<string, S.ValueSchema>> }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

/**
 * The closed value schema (Function I/O, action and RuleIR contracts) of every
 * exported top-level `const` whose initializer is Effect Schema syntax.
 */
export function compileValueSchemas(file: string, limits: Partial<CompilerLimits> = {}): ValueSchemasResult {
  const result = lowerEach<Record<string, S.ValueSchema>>([file], limits, (compiler, source) => ({ ok: true, value: compiler.valueSchemas(source) }))[0]!
  if (!result.ok) return result
  return { ok: true, schemas: result.value }
}

/** Literal metadata members, read from syntax. */
type Members = ReadonlyMap<string, ts.Expression>

class DefinitionCompiler {
  readonly sourceMap: DefinitionSource[] = []
  readonly warnings: Diagnostic[] = []
  private readonly checker: ts.TypeChecker
  private readonly schemas: SchemaProjection

  constructor(
    private readonly profile: ProfileChecker,
    private readonly program: ts.Program,
    private readonly started: number,
    private readonly limits: CompilerLimits,
    private readonly expansion: ExpansionBudget,
  ) {
    this.checker = program.getTypeChecker()
    this.schemas = new SchemaProjection(profile, program, (node) => this.guard(node), limits.maxNodes, (work) => expansion.reserve(work))
  }

  private guard(node: ts.Node): void {
    if (Date.now() - this.started >= this.limits.maxMillis) throw new CompileError("NORN-FLOW-UNBOUNDED", "compiler time bound exceeded", node)
  }

  private at(node: ts.Node): SourceLocation {
    const sf = node.getSourceFile(), p = sf.getLineAndCharacterOfPosition(node.getStart(sf))
    return { file: relative(TOOL_ROOT, sf.fileName).split(sep).join("/"), line: p.line + 1, column: p.character + 1 }
  }

  private id(call: ts.CallExpression): string | undefined {
    const e = call.expression
    return this.profile.identity(this.program, ts.isPropertyAccessExpression(e) ? e.name : e)
  }

  private unwrap(e: ts.Expression): ts.Expression {
    while (ts.isParenthesizedExpression(e) || ts.isSatisfiesExpression(e) || (ts.isAsExpression(e) && ts.isConstTypeReference(e.type))) e = e.expression
    return e
  }

  /** Follow a const binding (local, imported, or namespace-qualified) to its initializer. */
  private deref(input: ts.Expression): ts.Expression {
    let e = this.unwrap(input)
    for (let hops = 0; ts.isIdentifier(e) || ts.isPropertyAccessExpression(e); hops++) {
      if (hops > 128) throw new CompileError("NORN-FLOW-UNBOUNDED", "constant references exceed the compiler bound", input)
      const name = ts.isPropertyAccessExpression(e) ? e.name : e
      // `{ name }` names the member; the value it reads is the binding `name`.
      const raw = ts.isShorthandPropertyAssignment(name.parent) && name.parent.name === name
        ? this.checker.getShorthandAssignmentValueSymbol(name.parent)
        : this.checker.getSymbolAtLocation(name)
      const symbol = raw && raw.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(raw) : raw
      const d = symbol?.valueDeclaration
      if (!d || !ts.isVariableDeclaration(d) || !d.initializer || (d.parent.flags & ts.NodeFlags.Const) === 0) return e
      e = this.unwrap(d.initializer)
    }
    return e
  }

  compile(source: ts.SourceFile, allowEmpty = false): { readonly definitions: Definition[]; readonly flows: number } {
    const out: Definition[] = []
    let flows = 0
    for (const statement of source.statements) {
      if (ts.isExportAssignment(statement)) {
        const e = this.unwrap(statement.expression)
        if (ts.isCallExpression(e) && this.id(e) === FLOW) flows++
        continue
      }
      if (!ts.isVariableStatement(statement)) continue
      for (const d of statement.declarationList.declarations) {
        const flow = d.initializer && this.unwrap(d.initializer)
        if (flow && ts.isCallExpression(flow) && this.id(flow) === FLOW) flows++
      }
      for (const d of statement.declarationList.declarations) {
        const init = d.initializer && this.unwrap(d.initializer)
        const id = init && ts.isCallExpression(init) ? this.id(init) : undefined
        if (!init || !ts.isCallExpression(init) || id === undefined || !DEFINITION_CALLS.has(id)) continue
        const def =
          id === SOURCE_SCHEMA ? this.sourceSchema(init)
          : id === ONTOLOGY_PACK ? this.ontologyPack(init)
          : id === MARKINGS ? this.markings(init)
          : id === TYPE ? this.objectType(init)
          : id === INTERFACE ? this.interfaceType(init)
          : id === ACTION ? this.action(init)
          : id === ACCESS ? this.access(init)
          : id === CONNECTOR ? this.connector(init)
          : id === MAPPING ? this.mapping(init)
          : id === LAW ? this.law(init)
          : this.decision(init)
        this.expansion.assert(def)
        out.push(def)
      }
    }
    if (out.length === 0 && !allowEmpty) throw new CompileError("NORN-FLOW-RESULT", "the module declares no Norn.type, Norn.interfaceType, Norn.action, Norn.access, Norn.decision, Norn.mapping, or Norn.connector definition", source)
    return { definitions: out, flows }
  }

  valueSchemas(source: ts.SourceFile): Record<string, S.ValueSchema> {
    const out: Record<string, S.ValueSchema> = Object.create(null) as Record<string, S.ValueSchema>
    for (const statement of source.statements) {
      if (!ts.isVariableStatement(statement) || !statement.modifiers?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword)) continue
      for (const d of statement.declarationList.declarations) {
        if (!ts.isIdentifier(d.name) || !d.initializer || !this.isSchemaSyntax(d.initializer)) continue
        out[d.name.text] = this.schemas.closed(this.schemas.read(d.initializer))
      }
    }
    return out
  }

  /** Whether an initializer is Effect Schema syntax (a Schema value, a Schema call, a pipe over one, a Norn value type, or a const alias of one). */
  private isSchemaSyntax(input: ts.Expression): boolean {
    const e = this.deref(input)
    const name = ts.isCallExpression(e)
      ? (ts.isPropertyAccessExpression(e.expression) ? e.expression.name : e.expression)
      : ts.isPropertyAccessExpression(e) ? e.name
      : ts.isIdentifier(e) ? e // an imported Schema value (`import { String as Text } from "effect/Schema"`)
      : undefined
    const id = name && this.profile.identity(this.program, name)
    return id !== undefined && (id.startsWith("effect/Schema#") || id === "effect/Pipeable#Pipeable.pipe" || id === "effect/Function#pipe" || VALUE_TYPE_CALLS.has(id))
  }

  /**
   * The metadata of a referenced definition (a const bound to one of `kinds`,
   * local, imported, or namespace-qualified). References read only literal
   * members, never lower the target, so mutually referencing types are fine.
   */
  private referenced(input: ts.Expression, kinds: ReadonlySet<string>, what: string): { readonly call: ts.CallExpression; readonly members: Members } {
    const call = this.deref(input)
    const id = ts.isCallExpression(call) ? this.id(call) : undefined
    if (!ts.isCallExpression(call) || id === undefined || !kinds.has(id) || !call.arguments[0]) {
      throw new CompileError("NORN-FLOW-VALUE", `expected a reference to ${what}`, input)
    }
    return { call, members: this.record(call.arguments[0]) }
  }

  private member(members: Members, key: string, at: ts.Node): ts.Expression {
    const value = members.get(key)
    if (!value) throw new CompileError("NORN-FLOW-LITERAL", `a definition needs ${key}`, at)
    return value
  }

  /** A referenced type's qualified name and version. */
  private typeRef(input: ts.Expression, kinds: ReadonlySet<string>, what: string): { readonly name: string; readonly version: string } {
    const { call, members } = this.referenced(input, kinds, what)
    return { name: this.string(this.member(members, "name", call)), version: this.string(this.member(members, "version", call)) }
  }

  /** A type by reference, or an installed type by its qualified name literal. */
  private typeName(input: ts.Expression, kinds: ReadonlySet<string> = ANY_TYPE): string {
    const e = this.deref(input)
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return e.text
    return this.typeRef(input, kinds, "a Norn.type definition (or a qualified type name)").name
  }

  /** An action by reference, or a platform action by its id literal. */
  private actionId(input: ts.Expression): string {
    const e = this.deref(input)
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return e.text
    const { call, members } = this.referenced(input, ACTIONS, "a Norn.action definition (or an action id)")
    return this.string(this.member(members, "id", call))
  }

  private meta(call: ts.CallExpression, allowed: ReadonlyArray<string>): Members {
    const arg = call.arguments[0]
    if (!arg || (call.arguments.length !== 1 && !(this.id(call) === DECISION && call.arguments.length === 2))) throw new CompileError("NORN-FLOW-LITERAL", "a definition takes one metadata object", call)
    const members = this.record(arg)
    for (const [key, value] of members) {
      if (!allowed.includes(key)) throw new CompileError("NORN-FLOW-LITERAL", `unknown definition member ${key}`, value.parent)
    }
    return members
  }

  private base(call: ts.CallExpression, members: Members) {
    const required = (key: string): ts.Expression => {
      const value = members.get(key)
      if (!value) throw new CompileError("NORN-FLOW-LITERAL", `a definition needs ${key}`, call)
      return value
    }
    const description = members.get("description")
    const markings = members.get("markings")
    const schema = this.schemas.read(required("properties"))
    const projected = this.schemas.properties(schema)
    return {
      name: this.string(required("name")),
      title: this.string(required("title")),
      version: this.string(required("version")),
      ...(description ? { description: this.string(description) } : {}),
      ...(markings ? { markings: this.strings(markings) } : {}),
      projected,
    }
  }

  private propertyMarkings(members: Members, properties: ReadonlyArray<string>): Map<string, ReadonlyArray<string>> {
    const out = new Map<string, ReadonlyArray<string>>()
    const node = members.get("propertyMarkings")
    if (node) for (const [id, value] of this.closedRecord(node, properties, [])) out.set(id, this.strings(value))
    return out
  }

  private sourceSchema(call: ts.CallExpression): SS.SourceSchemaDef {
    const m = this.meta(call, ["id", "title", "version", "source", "observedAt", "adapter", "rawMetadata", "datasets", "fields", "constraints", "codeSets"])
    const need = (key: string): ts.Expression => this.member(m, key, call)
    this.closedRecord(need("source"), ["system", "instance", "version"], ["system", "instance"])
    this.closedRecord(need("adapter"), ["name", "version"], ["name", "version"])
    this.closedRecord(need("rawMetadata"), ["uri", "sha256"], ["uri", "sha256"])
    for (const node of this.list(need("datasets"))) this.closedRecord(node, ["id", "nativeName", "namespace", "kind"], ["id", "nativeName", "kind"])
    for (const node of this.list(need("fields"))) {
      const field = this.closedRecord(node, ["dataset", "name", "ordinal", "nativeType", "typeCandidate", "required", "markings"], ["dataset", "name", "ordinal", "nativeType", "typeCandidate"])
      this.closedRecord(field.get("typeCandidate")!, ["type", "origin"], ["type", "origin"])
    }
    const codeSets = m.get("codeSets")
    for (const node of codeSets ? this.list(codeSets) : []) {
      const set = this.closedRecord(node, ["id", "dataset", "field", "origin", "values"], ["id", "dataset", "field", "origin", "values"])
      for (const value of this.list(set.get("values")!)) this.closedRecord(value, ["value", "label"], ["value"])
    }
    const constraints = m.get("constraints")
    for (const node of constraints ? this.list(constraints) : []) {
      const constraint = this.closedRecord(node, ["id", "kind", "dataset", "fields", "target", "collection", "origin"], ["id", "kind", "dataset", "fields", "origin"])
      const target = constraint.get("target")
      if (target) this.closedRecord(target, ["dataset", "fields"], ["dataset", "fields"])
    }
    const data = Object.fromEntries([...m].map(([key, node]) => [key, this.json(node)]))
    return SS.sourceSchema({ ...data as unknown as SS.SourceSchemaSpec, at: this.at(call) })
  }

  /** Closed records at every level; identifier, pin, licence and reference rules are the Rust ontology checker's. */
  private ontologyPack(call: ts.CallExpression): OP.OntologyPackDef {
    const m = this.meta(call, ["id", "title", "version", "sources", "terms", "mappings", "shapes"])
    const need = (key: string): ts.Expression => this.member(m, key, call)
    for (const node of this.list(need("sources"))) {
      const source = this.closedRecord(node, ["sourceId", "releaseId", "releasedAt", "retrievedAt", "canonicalUrl", "license", "redistribution", "importMode", "importer", "files"], ["sourceId", "releaseId", "releasedAt", "retrievedAt", "canonicalUrl", "license", "redistribution", "importMode"])
      const importer = source.get("importer")
      if (importer) this.closedRecord(importer, ["name", "version"], ["name", "version"])
      const files = source.get("files")
      for (const file of files ? this.list(files) : []) this.closedRecord(file, ["path", "sha256"], ["path", "sha256"])
    }
    for (const node of this.list(need("terms"))) this.closedRecord(node, ["termId", "label", "definition", "termKind", "status", "semanticAuthority", "parentTermId", "supersededBy", "domainTermId"], ["termId", "label", "definition", "termKind", "status", "semanticAuthority"])
    for (const node of this.list(need("mappings"))) this.closedRecord(node, ["mappingId", "termId", "sourceId", "externalIri", "relation", "justification", "evidenceRefs", "confidenceBasisPoints", "reviewedBy", "reviewedAt", "supersededByMappingId", "ratifiedBy", "ratifiedAt"], ["mappingId", "termId", "sourceId", "externalIri", "relation", "justification", "evidenceRefs", "confidenceBasisPoints", "reviewedBy", "reviewedAt"])
    for (const node of this.list(need("shapes"))) {
      const shape = this.closedRecord(node, ["shapeId", "termId", "closed", "fields"], ["shapeId", "termId", "closed", "fields"])
      for (const field of this.list(shape.get("fields")!)) this.closedRecord(field, ["fieldId", "valueType", "targetTermId", "minCount", "maxCount"], ["fieldId", "valueType", "minCount", "maxCount"])
    }
    const data = Object.fromEntries([...m].map(([key, node]) => [key, this.json(node)]))
    return OP.ontologyPack({ ...data as unknown as OP.OntologyPackSpec, at: this.at(call) })
  }

  private markings(call: ts.CallExpression): E.MarkingsDef {
    const m = this.meta(call, ["id", "title", "version", "markings"])
    const need = (key: string): ts.Expression => this.member(m, key, call)
    return E.markings({ id: this.string(need("id")), title: this.string(need("title")), version: this.string(need("version")), markings: this.list(need("markings")).map((node) => {
      const d = this.closedRecord(node, ["id", "label", "severity", "category", "mask"], ["id", "label", "severity", "category", "mask"])
      const mask = d.get("mask")!
      const kind = this.string(this.closedRecord(mask, ["kind", "count"], ["kind"]).get("kind")!)
      this.closedRecord(mask, kind === "last-n" ? ["kind", "count"] : ["kind"], kind === "last-n" ? ["kind", "count"] : ["kind"])
      return this.json(node) as unknown as E.MarkingDecl
    }), at: this.at(call) })
  }

  private interfaceType(call: ts.CallExpression): E.InterfaceDef {
    const members = this.meta(call, [
      "name", "title", "version", "description", "extends", "properties", "invariants", "markings", "propertyMarkings",
    ])
    const { projected, ...rest } = this.base(call, members)
    const propertyMarkings = this.propertyMarkings(members, projected.map((p) => p.id))
    const invariantsNode = members.get("invariants")
    return E.interfaceType({
      ...rest,
      extends: members.has("extends") ? this.list(members.get("extends")!).map((parent) => this.typeName(parent, INTERFACES)) : [],
      properties: projected.map((p) => ({ id: p.id, ...(propertyMarkings.has(p.id) ? { markings: propertyMarkings.get(p.id)! } : {}), type: p.projection.type, ...(p.projection.nullable ? { nullable: true } : {}), at: this.at(p.node) })),
      invariants: invariantsNode ? this.list(invariantsNode).map((x) => this.invariant(x)) : [],
      at: this.at(call),
    })
  }

  private invariant(input: ts.Expression): E.InvariantDecl {
    const m = this.closedRecord(
      input,
      ["id", "description", "classification", "expression", "message"],
      ["id", "expression", "message"],
    )
    const description = m.get("description")
    const classification = m.get("classification")
    const constraintClass = classification ? this.string(classification) : undefined
    if (constraintClass !== undefined && constraintClass !== "integrity" && constraintClass !== "quality") {
      throw new CompileError("NORN-FLOW-LITERAL", "constraint classification must be integrity or quality", classification!)
    }
    return {
      ...(constraintClass === "quality" ? { classification: constraintClass } : {}),
      id: this.string(m.get("id")!),
      ...(description ? { description: this.string(description) } : {}),
      expression: this.zen(m.get("expression")!, "self"),
      message: this.string(m.get("message")!),
    }
  }

  private objectType(call: ts.CallExpression): E.ObjectTypeDef {
    const members = this.meta(call, ["name", "title", "version", "description", "properties", "extends", "implements", "links", "identity", "defaults", "lifecycles", "markings", "propertyMarkings"])
    const { projected, ...rest } = this.base(call, members)
    const propertyMarkings = this.propertyMarkings(members, projected.map((p) => p.id))
    const defaults = new Map<string, E.DefaultValue>()
    const defaultsNode = members.get("defaults")
    if (defaultsNode) {
      for (const [key, value] of this.record(defaultsNode)) {
        if (!projected.some((p) => p.id === key)) throw new CompileError("NORN-FLOW-LITERAL", `default for undeclared property ${key}`, value)
        defaults.set(key, this.defaultValue(value))
      }
    }
    const implementsNode = members.get("implements")
    const extendsNode = members.get("extends")
    const linksNode = members.get("links")
    const identityNode = members.get("identity")
    const lifecyclesNode = members.get("lifecycles")
    return E.objectType({
      ...rest,
      ...(extendsNode ? { extends: this.typeName(extendsNode, OBJECT_TYPES) } : {}),
      properties: projected.map((p) => {
        const value = defaults.get(p.id)
        return {
          id: p.id,
          ...(propertyMarkings.has(p.id) ? { markings: propertyMarkings.get(p.id)! } : {}),
          type: p.projection.type,
          ...(p.projection.nullable ? { nullable: true } : {}),
          ...(value === undefined ? {} : { default: value }),
          at: this.at(p.node),
        }
      }),
      implements: implementsNode ? this.list(implementsNode).map((x) => this.typeName(x, INTERFACES)) : [],
      links: linksNode ? this.list(linksNode).map((x) => this.link(x)) : [],
      identity: identityNode ? this.list(identityNode).map((x) => this.key(x)) : [],
      lifecycles: lifecyclesNode ? this.list(lifecyclesNode).map((x) => this.lifecycle(x)) : [],
      at: this.at(call),
    })
  }

  private link(input: ts.Expression): E.LinkDecl {
    const m = this.closedRecord(input, ["id", "description", "target", "cardinality", "inverse", "acyclic"], ["id", "target", "cardinality"])
    const c = this.closedRecord(m.get("cardinality")!, ["to", "from", "required"], ["to", "from"])
    const multiplicity = (e: ts.Expression): E.Multiplicity => {
      const v = this.string(e)
      if (v !== "one" && v !== "many") throw new CompileError("NORN-FLOW-LITERAL", "a multiplicity is \"one\" or \"many\"", e)
      return v
    }
    const description = m.get("description"), inverse = m.get("inverse"), required = c.get("required")
    const acyclic = m.get("acyclic")
    return {
      id: this.string(m.get("id")!),
      ...(description ? { description: this.string(description) } : {}),
      target: this.typeName(m.get("target")!, OBJECT_TYPES),
      cardinality: { to: multiplicity(c.get("to")!), from: multiplicity(c.get("from")!), ...(required ? { required: this.boolean(required) } : {}) },
      ...(inverse ? { inverse: this.string(inverse) } : {}),
      ...(acyclic ? { acyclic: this.boolean(acyclic) } : {}),
    }
  }

  private action(call: ts.CallExpression): A.ActionDef {
    const members = this.meta(call, ["id", "title", "version", "description", "scopeRequired", "idempotent", "ledgered", "ledgerCategory", "input", "output", "slice", "derive", "preconditions", "createLinks", "commands", "writes", "effect", "compensation"])
    const need = (key: string): ts.Expression => this.member(members, key, call)
    const schema = (key: string): S.ObjectSchema => {
      const node = need(key)
      const closed = this.schemas.closed(this.schemas.read(node))
      if (closed.type !== "object") throw new CompileError("NORN-FLOW-SCHEMA-PROJECTION", `action ${key} is a Schema.Struct`, node)
      return closed
    }
    const effectNode = members.get("effect"), compensation = members.get("compensation")
    const description = members.get("description"), ledgerCategory = members.get("ledgerCategory")
    return A.action({
      id: this.string(need("id")),
      title: this.string(need("title")),
      version: this.string(need("version")),
      ...(description ? { description: this.string(description) } : {}),
      scopeRequired: this.strings(need("scopeRequired")),
      idempotent: this.boolean(need("idempotent")),
      ledgered: this.boolean(need("ledgered")),
      ...(ledgerCategory ? { ledgerCategory: this.string(ledgerCategory) } : {}),
      input: schema("input"),
      output: schema("output"),
      slice: members.has("slice") ? this.list(need("slice")).map((entry) => this.sliceEntry(entry)) : [],
      derive: members.has("derive") ? this.list(need("derive")).map((entry, index) => {
        const m = this.closedRecord(entry, ["field", "expression", "inputRequired"], ["field", "expression"])
        const inputRequired = m.get("inputRequired")
        return {
          field: this.string(m.get("field")!),
          expression: this.zenAt(m.get("expression")!, this.string(need("id")) + ".action", `/payload/derive/${index}/expression`),
          ...(inputRequired && !this.boolean(inputRequired) ? { inputRequired: false } : {}),
        }
      }) : [],
      preconditions: members.has("preconditions") ? this.list(need("preconditions")).map((e, index) => this.zenAt(e, this.string(need("id")) + ".action", `/payload/preconditions/${index}`, "")) : [],
      commands: members.has("commands") ? this.list(need("commands")).map((c) => this.command(c)) : [],
      createLinks: members.has("createLinks") ? this.list(need("createLinks")).map((entry) => {
        const m = this.closedRecord(entry, ["link", "targetType", "targetIdField"], ["link", "targetType", "targetIdField"])
        return { link: this.string(m.get("link")!), targetType: this.typeName(m.get("targetType")!, OBJECT_TYPES), targetIdField: this.string(m.get("targetIdField")!) }
      }) : [],
      writes: members.has("writes")
        ? this.list(need("writes")).map((w) => {
            const m = this.closedRecord(w, ["type", "change"], ["type", "change"])
            const change = this.string(m.get("change")!)
            if (change !== "created" && change !== "modified" && change !== "deleted") throw new CompileError("NORN-FLOW-LITERAL", "a write change is created, modified, or deleted", m.get("change")!)
            return { entityKind: this.typeName(m.get("type")!, OBJECT_TYPES), change }
          })
        : [],
      ...(effectNode ? { effect: this.effect(effectNode) } : {}),
      ...(compensation ? { compensation: this.actionId(compensation) } : {}),
      at: this.at(call),
    })
  }

  private zenAt(input: ts.Expression, artifactId: string, pointer: string, root = "input"): string {
    this.sourceMap.push({ artifactId, pointer, at: this.at(input) })
    return this.zen(input, root)
  }

  /** Read syntax-only expression wrappers in action/lifecycle expression slots. */
  private zen(input: ts.Expression, root = "input"): string {
    const e = this.deref(input)
    if (ts.isCallExpression(e) && this.id(e) === "@norn/authoring#Norn.unsafeZen") {
      this.warnings.push(diagnostic(e, "NORN-AUTHORING-UNSAFE-ZEN", "unsafeZen bypasses typed authoring; replace it with a TypeScript expression callback"))
      return this.string(e.arguments[0]!)
    }
    if (ts.isCallExpression(e) && this.id(e) === "@norn/authoring#Norn.expression") {
      const schema = this.schemas.expressionInput(this.schemas.read(e.arguments[0]!))
      const body = e.arguments[1]
      if (!body || (!ts.isArrowFunction(body) && !ts.isFunctionExpression(body))) throw new CompileError("NORN-FLOW-CAPTURE", "use an inline expression callback", e)
      const syntax = new SyntaxCompiler(this.profile, this.program, e.getSourceFile(), this.started, this.limits, this.expansion, TOOL_ROOT)
      const rows = syntax.decisionRows(body)
      this.warnings.push(...syntax.warnings)
      if (rows.length !== 1) throw new CompileError("NORN-FLOW-EXPRESSION", "use a conditional expression for a scalar expression", body)
      return X.render(rows[0]!.value, new Map([["input", { zen: root, schema }]]))
    }
    throw new CompileError("NORN-FLOW-EXPRESSION", "raw ZEN is not typed authoring; use Norn.expression(Schema, input => ...) or the explicit unsafeZen escape", input)
  }

  private command(input: ts.Expression): A.ActionCommand {
    const m = this.closedRecord(input, ["connector", "operation", "doctype", "name", "fields"], ["connector", "operation", "doctype"])
    const operation = this.string(m.get("operation")!)
    if (operation !== "submit" && operation !== "create" && operation !== "update" && operation !== "cancel") throw new CompileError("NORN-FLOW-LITERAL", "a command operation is submit, create, update, or cancel", m.get("operation")!)
    const name = m.get("name"), fields = m.get("fields")
    const values: Record<string, string> = {}
    if (fields) {
      for (const [field, value] of this.record(fields)) {
        if (field === "__proto__") throw new CompileError("NORN-FLOW-LITERAL", "__proto__ is not a portable JSON member name", value.parent)
        values[field] = this.zen(value)
      }
    }
    return {
      connector: this.string(m.get("connector")!),
      operation,
      doctype: this.string(m.get("doctype")!),
      ...(name ? { name: this.zen(name) } : {}),
      ...(fields ? { fields: values } : {}),
    }
  }

  private effect(input: ts.Expression): A.ActionEffect {
    const m = this.closedRecord(input, ["kind", "type", "objectIdField", "lifecycle", "transition", "writeMode"], ["kind", "type", "objectIdField"])
    const kind = this.string(m.get("kind")!)
    const type = this.typeRef(m.get("type")!, OBJECT_TYPES, "a Norn.type definition")
    const field = this.string(m.get("objectIdField")!)
    if (kind === "upsert_input_object") {
      if (m.has("lifecycle") || m.has("transition")) throw new CompileError("NORN-FLOW-LITERAL", "upsert_input_object takes no lifecycle or transition", input)
      const writeMode = m.get("writeMode")
      return A.upsertInputObject(type, field, writeMode ? this.upsertWriteMode(writeMode) : undefined)
    }
    if (kind !== "fire_transition") throw new CompileError("NORN-FLOW-LITERAL", "an effect kind is fire_transition or upsert_input_object", m.get("kind")!)
    if (m.has("writeMode")) throw new CompileError("NORN-FLOW-LITERAL", "writeMode belongs to upsert_input_object", m.get("writeMode")!)
    const lifecycle = m.get("lifecycle"), transition = m.get("transition")
    if (!lifecycle || !transition) throw new CompileError("NORN-FLOW-LITERAL", "fire_transition names its lifecycle and transition", input)
    return A.fireTransition(type, field, this.string(lifecycle), this.string(transition))
  }

  private upsertWriteMode(input: ts.Expression): A.UpsertWriteMode {
    const m = this.closedRecord(input, ["kind", "lifecycle", "state"], ["kind"])
    const kind = this.string(m.get("kind")!)
    if (kind === "upsert" || kind === "create_only") {
      if (m.has("lifecycle") || m.has("state")) {
        throw new CompileError("NORN-FLOW-LITERAL", `${kind} writeMode takes only kind`, input)
      }
      return { kind }
    }
    if (kind === "draft_only") {
      const lifecycle = m.get("lifecycle"), state = m.get("state")
      if (!lifecycle || !state) throw new CompileError("NORN-FLOW-LITERAL", "draft_only writeMode needs lifecycle and state", input)
      return { kind, lifecycle: this.string(lifecycle), state: this.string(state) }
    }
    throw new CompileError("NORN-FLOW-LITERAL", "writeMode kind is upsert, create_only, or draft_only", m.get("kind")!)
  }

  private access(call: ts.CallExpression): P.AccessDef {
    const members = this.meta(call, ["id", "title", "version", "rules"])
    const need = (key: string): ts.Expression => this.member(members, key, call)
    return P.access({
      id: this.string(need("id")),
      title: this.string(need("title")),
      version: this.string(need("version")),
      rules: this.list(need("rules")).map((r) => {
        const m = this.closedRecord(r, ["ruleId", "actions", "resourceType", "scope", "allOf"], ["ruleId", "actions", "resourceType", "scope"])
        const scope = this.closedRecord(m.get("scope")!, ["dimensions"], ["dimensions"])
        const allOf = m.get("allOf")
        return {
          ruleId: this.string(m.get("ruleId")!),
          actions: this.list(m.get("actions")!).map((a) => this.actionId(a)),
          resourceType: this.typeName(m.get("resourceType")!, OBJECT_TYPES),
          // The closed grammar (dimension names, condition nodes) is the Rust checker's.
          scope: { dimensions: this.strings(scope.get("dimensions")!) as P.ScopeDimension[] },
          ...(allOf ? { allOf: this.list(allOf).map((c) => this.json(c) as unknown as P.AccessCondition) } : {}),
        }
      }),
      at: this.at(call),
    })
  }

  private decision(call: ts.CallExpression): R.DecisionDef {
    const members = this.meta(call, ["id", "artifactId", "title", "version", "contract", "dependencies", "triggers", "slice", "input", "output", "scopeRequired", "graph"])
    const need = (key: string): ts.Expression => this.member(members, key, call)
    const schema = (key: string): S.ObjectSchema => {
      const node = need(key)
      const closed = this.schemas.closed(this.schemas.read(node), true)
      if (closed.type !== "object") throw new CompileError("NORN-FLOW-SCHEMA-PROJECTION", `decision ${key} is a Schema.Struct`, node)
      return closed
    }
    const contract = this.string(need("contract"))
    if (contract !== "decide" && contract !== "sub_decision") throw new CompileError("NORN-FLOW-LITERAL", "a decision contract is decide or sub_decision", need("contract"))
    const artifactId = members.get("artifactId"), triggers = members.get("triggers"), slice = members.get("slice"), dependencies = members.get("dependencies")
    const inputSchema = schema("input")
    const calledDependencies: R.DecisionDependencyPin[] = []
    const body = call.arguments[1]
    let graph: Json
    if (body) {
      if (members.has("graph")) throw new CompileError("NORN-FLOW-LITERAL", "choose a callback or an imported editor graph", body)
      if (!ts.isArrowFunction(body) && !ts.isFunctionExpression(body)) throw new CompileError("NORN-FLOW-CAPTURE", "decision body must be an inline callback", body)
      const scope = new Map<string, X.Binding>([["input", { zen: "", schema: inputSchema }]])
      const calls: Json[] = []
      const syntax = new SyntaxCompiler(this.profile, this.program, call.getSourceFile(), this.started, this.limits, this.expansion, TOOL_ROOT, (site, input) => {
        if (!ts.isVariableDeclaration(site.parent) || !ts.isVariableDeclarationList(site.parent.parent) || !ts.isVariableStatement(site.parent.parent.parent) || site.parent.parent.parent.parent !== body.body) throw new CompileError("NORN-FLOW-EXPRESSION", "evaluate a sub-decision in a top-level const before branching", site)
        const child = this.referenced(site.arguments[0]!, DECISIONS, "a Norn.decision definition")
        if (this.string(this.member(child.members, "contract", child.call)) !== "sub_decision") throw new CompileError("NORN-FLOW-TYPE", "evaluate requires contract sub_decision", site.arguments[0]!)
        const pin = this.decisionDependency(site.arguments[0]!)
        if (!calledDependencies.some((p) => p.artifactId === pin.artifactId && p.version === pin.version)) calledDependencies.push(pin)
        const name = `_norn_sub_${calls.length}`
        if (Object.hasOwn(inputSchema.properties, name)) throw new CompileError("NORN-FLOW-LITERAL", `${name} is reserved for compiler results`, site)
        calls.push({ id: name, name, type: "decisionNode", content: { key: pin.artifactId, inputField: X.render(input, scope), outputPath: name, passThrough: true } })
        scope.set(name, { zen: name, schema: this.schemas.closed(this.schemas.read(this.member(child.members, "output", child.call)), true) })
        return X.ref(name)
      })
      const rows = syntax.decisionRows(body)
      this.warnings.push(...syntax.warnings)
      const outputs = Object.keys(schema("output").properties)
      const artifactId = members.get("artifactId") ? this.string(members.get("artifactId")!) : this.string(need("id")) + ".rule"
      const locate = (expr: X.Expr, pointer: string): void => {
        const node = syntax.expressionSources.get(expr) ?? body
        this.sourceMap.push({ artifactId, pointer, at: this.at(node) })
      }
      const rules = rows.map((row, i): Json => {
        const project = (value: X.Expr, field: string): X.Expr | undefined => {
          if (value._tag === "Record") return value.fields.find(([key]) => key === field)?.[1]
          if (value._tag === "Ref") return X.ref(value.name, ...value.path, field)
          if (value._tag === "Conditional") {
            const yes = project(value.yes, field), no = project(value.no, field)
            return yes && no ? X.conditional(value.condition, yes, no) : undefined
          }
          return undefined
        }
        try {
          if (row.condition && X.typeOf(row.condition, scope).type !== "boolean") throw new X.ExprError("NORN-FLOW-EXPRESSION", "expected a boolean row condition")
          if (row.condition) locate(row.condition, `/payload/graph/nodes/${calls.length + 1}/content/rules/${i}/condition`)
          const cells: Record<string, Json> = { _id: `row_${i}`, condition: row.condition ? X.render(row.condition, scope) : "" }
          for (const field of outputs) {
            const value = project(row.value, field)
            if (!value) throw new X.ExprError("NORN-FLOW-RESULT", `expected output field ${field}; return every declared output`)
            locate(value, `/payload/graph/nodes/${calls.length + 1}/content/rules/${i}/out_${field.replace(/~/g, "~0").replace(/\//g, "~1")}`)
            cells[`out_${field}`] = X.render(value, scope)
          }
          return cells
        } catch (error) {
          if (!(error instanceof X.ExprError)) throw error
          throw new CompileError(error.code, error.message, body)
        }
      })
      graph = { contentType: R.JDM_CONTENT_TYPE, nodes: [
        { id: "in", name: "input", type: "inputNode" },
        ...calls,
        { id: "decision", name: "decision", type: "decisionTableNode", content: { hitPolicy: "first", inputs: [{ id: "condition", name: "condition", type: "expression" }], outputs: outputs.map((field) => ({ id: `out_${field}`, name: field, field })), rules } },
        { id: "out", name: "output", type: "outputNode" },
      ], edges: ["in", ...calls.map((c) => (c as { id: string }).id), "decision", "out"].slice(1).map((targetId, index) => ({ id: `edge_${index}`, sourceId: ["in", ...calls.map((c) => (c as { id: string }).id), "decision"][index]!, targetId, type: "edge" })) }
    } else graph = this.json(need("graph"))
    if (graph === null || typeof graph !== "object" || Array.isArray(graph)) throw new CompileError("NORN-FLOW-LITERAL", "a decision graph is a JDM object { nodes, edges }", need("graph"))
    return R.decision({
      id: this.string(need("id")),
      ...(artifactId ? { artifactId: this.string(artifactId) } : {}),
      title: this.string(need("title")),
      version: this.string(need("version")),
      contract,
      dependencies: [...(dependencies ? this.list(dependencies).map((dependency) => this.decisionDependency(dependency)) : []), ...calledDependencies],
      triggers: triggers ? this.list(triggers).map((t) => this.trigger(t)) : [],
      slice: slice ? this.list(slice).map((x) => this.sliceEntry(x)) : [],
      input: inputSchema,
      output: schema("output"),
      scopeRequired: this.strings(need("scopeRequired")),
      // The JDM shape (node kinds, edges, cells) is the Rust checker's to validate.
      graph: graph as unknown as R.JdmGraph,
      at: this.at(call),
    })
  }

  /** Resolve a local Norn.decision reference or read an explicit artifact/version pin. */
  private decisionDependency(input: ts.Expression): { readonly artifactId: string; readonly version: string } {
    const referenced = this.deref(input)
    if (ts.isCallExpression(referenced)) {
      const { call, members } = this.referenced(input, DECISIONS, "a Norn.decision definition")
      const id = this.string(this.member(members, "id", call))
      const artifactId = members.get("artifactId")
      return {
        artifactId: artifactId ? this.string(artifactId) : id + ".rule",
        version: this.string(this.member(members, "version", call)),
      }
    }
    const pin = this.closedRecord(input, ["artifactId", "version"], ["artifactId", "version"])
    return { artifactId: this.string(pin.get("artifactId")!), version: this.string(pin.get("version")!) }
  }

  private law(call: ts.CallExpression): L.LawDef {
    const members = this.meta(call, ["id", "title", "version", "law", "dependencies"])
    const need = (key: string): ts.Expression => this.member(members, key, call)
    return L.law({ id: this.string(need("id")), title: this.string(need("title")), version: this.string(need("version")), law: this.json(need("law")) as unknown as L.LawIR, dependencies: this.json(need("dependencies")) as unknown as L.LawDef["dependencies"] })
  }

  private connector(call: ts.CallExpression): C.ConnectorDef {
    const members = this.meta(call, ["id", "title", "version", "auth", "resources", "mappings", "cursor", "deadLetterPolicy", "poll"])
    const need = (key: string): ts.Expression => this.member(members, key, call)
    // Close every record even through const aliases: TypeScript excess-property
    // checks alone would allow an alias carrying undeclared credential material.
    const auth = need("auth")
    const authKind = this.string(this.closedRecord(auth, ["kind", "authId", "placement", "name", "secretRef", "prefix", "format", "authorizationUrl", "tokenUrl", "scopes", "clientSecretRef"], ["kind"]).get("kind")!)
    const authKeys = authKind === "none" ? ["kind"] : authKind === "api_key" ? ["kind", "authId", "placement", "name", "secretRef", "prefix", "format"] : authKind === "oauth2" ? ["kind", "authId", "authorizationUrl", "tokenUrl", "scopes", "clientSecretRef"] : undefined
    if (!authKeys) throw new CompileError("NORN-FLOW-LITERAL", "unknown connector auth kind", auth)
    const authMembers = this.closedRecord(auth, authKeys, ["kind"])
    const format = authMembers.get("format")
    if (format) {
      const m = this.closedRecord(format, ["kind", "separator"], ["kind"])
      this.closedRecord(format, this.string(m.get("kind")!) === "pair" ? ["kind", "separator"] : ["kind"], ["kind"])
    }
    const resourceKeys: Readonly<Record<string, ReadonlyArray<string>>> = {
      paginated: ["kind", "resourceId", "endpoint", "itemsPath", "missingItemsIsEmpty", "queryParams", "method", "pageParam", "pageSizeParam", "pageMode", "pageStart", "pageSize"],
      cursor: ["kind", "resourceId", "endpoint", "itemsPath", "missingItemsIsEmpty", "queryParams", "cursorParam", "nextCursorPath"],
      delta: ["kind", "resourceId", "endpoint", "itemsPath", "missingItemsIsEmpty", "queryParams", "sinceParam", "updatedAtPath"],
      webhook: ["kind", "resourceId", "eventType", "payloadPath"], csv_import: ["kind", "resourceId", "delimiter", "columns"],
    }
    for (const resource of this.list(need("resources"))) {
      const m = this.record(resource)
      const kind = this.string(this.member(m, "kind", resource))
      const keys = Object.hasOwn(resourceKeys, kind) ? resourceKeys[kind] : undefined
      if (!keys) throw new CompileError("NORN-FLOW-LITERAL", "unknown connector resource kind", resource)
      this.closedRecord(resource, keys, ["kind", "resourceId"])
    }
    for (const mapping of this.list(need("mappings"))) {
      const m = this.record(mapping)
      const kind = this.string(this.member(m, "kind", mapping))
      if (kind !== "field" && kind !== "external_id") throw new CompileError("NORN-FLOW-LITERAL", "unknown connector mapping kind", mapping)
      this.closedRecord(mapping, ["kind", "resourceId", "sourcePath", "targetObjectKind", kind === "field" ? "targetField" : "externalSource"], ["kind", "resourceId", "sourcePath", "targetObjectKind"])
    }
    const cursor = members.get("cursor"), deadLetter = members.get("deadLetterPolicy"), poll = members.get("poll")
    if (cursor) this.closedRecord(cursor, ["cursorId", "resourceId", "storageKey"], ["cursorId", "resourceId", "storageKey"])
    if (deadLetter) this.closedRecord(deadLetter, ["maxFailures", "route"], ["maxFailures", "route"])
    if (poll) {
      const m = this.closedRecord(poll, ["feeds"], ["feeds"])
      for (const feed of this.list(m.get("feeds")!)) {
        const f = this.closedRecord(feed, ["resourceId", "recordKeyPath", "fanout"], ["resourceId"])
        const fanout = f.get("fanout")
        if (fanout) this.closedRecord(fanout, ["parentResourceId", "pathBinding", "keyPath"], ["parentResourceId", "pathBinding", "keyPath"])
      }
    }
    return C.connector({
      id: this.string(need("id")), title: this.string(need("title")), version: this.string(need("version")),
      auth: this.json(auth) as unknown as C.ConnectorAuth,
      resources: this.json(need("resources")) as unknown as C.ConnectorSpec["resources"],
      mappings: this.json(need("mappings")) as unknown as C.ConnectorSpec["mappings"],
      ...(cursor ? { cursor: this.json(cursor) as unknown as NonNullable<C.ConnectorSpec["cursor"]> } : {}),
      ...(deadLetter ? { deadLetterPolicy: this.json(deadLetter) as unknown as NonNullable<C.ConnectorSpec["deadLetterPolicy"]> } : {}),
      ...(poll ? { poll: this.json(poll) as unknown as NonNullable<C.ConnectorSpec["poll"]> } : {}), at: this.at(call),
    })
  }

  private mappingExpression(input: ts.Expression): string {
    const e = this.deref(input)
    // Existing string form remains accepted for existing packs; typed source uses syntax expressions.
    return ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e) ? this.string(e) : this.zen(input)
  }

  private mapping(call: ts.CallExpression): M.MappingDef {
    const members = this.meta(call, ["id", "title", "version", "source", "sourceContract", "interfaces", "maps"])
    const need = (key: string): ts.Expression => this.member(members, key, call)
    const source = this.closedRecord(need("source"), ["schemaId", "version", "sha256"], ["schemaId", "version", "sha256"])
    const pin = (input: ts.Expression, kinds: ReadonlySet<string>, suffix: "entity" | "interface"): M.ArtifactPin => {
      const ref = this.typeRef(input, kinds, suffix === "entity" ? "a Norn.type definition" : "a Norn.interfaceType definition")
      return { artifactId: `${ref.name}.${suffix}`, version: ref.version }
    }
    const contractNode = members.get("sourceContract")
    const sourceContract = contractNode ? ((): M.MappingSourceContract => {
      const contractMembers = this.closedRecord(contractNode, ["kind", "system", "versions", "apiTerms"], ["kind"])
      const kind = this.string(this.member(contractMembers, "kind", contractNode))
      if (kind === "generic") {
        this.closedRecord(contractNode, ["kind"], ["kind"])
        return { kind: "generic" }
      }
      if (kind !== "vendor") throw new CompileError("NORN-FLOW-LITERAL", "sourceContract is generic or vendor", contractNode)
      const fields = this.closedRecord(contractNode, ["kind", "system", "versions", "apiTerms"], ["kind", "system", "versions", "apiTerms"])
      const versions = this.list(this.member(fields, "versions", contractNode)).map((node) => this.string(node))
      const [first, ...rest] = versions
      if (first === undefined) throw new CompileError("NORN-FLOW-LITERAL", "vendor mapping needs exact supported versions", contractNode)
      const termsNode = this.member(fields, "apiTerms", contractNode)
      const terms = this.closedRecord(termsNode, ["uri", "sha256"], ["uri", "sha256"])
      const contract: M.VendorMappingContract = {
        system: this.string(this.member(fields, "system", contractNode)),
        versions: [first, ...rest],
        apiTerms: { uri: this.string(this.member(terms, "uri", termsNode)), sha256: this.string(this.member(terms, "sha256", termsNode)) },
      }
      return { kind: "vendor", ...contract }
    })() : undefined
    return M.mapping({
      ...(sourceContract === undefined ? {} : { sourceContract }),
      id: this.string(need("id")),
      title: this.string(need("title")),
      version: this.string(need("version")),
      source: { schemaId: this.string(source.get("schemaId")!), version: this.string(source.get("version")!), sha256: this.string(source.get("sha256")!) },
      interfaces: this.list(need("interfaces")).map((x) => pin(x, INTERFACES, "interface")),
      maps: this.list(need("maps")).map((x) => {
        const m = this.closedRecord(x, ["id", "source", "target", "subject", "sourceRecord", "properties", "links", "lifecycles", "gaps", "ignoredFields", "when"], ["id", "source", "target", "subject", "sourceRecord"])
        const src = this.closedRecord(m.get("source")!, ["dataset", "nestedIn"], ["dataset"])
        const nested = src.get("nestedIn")
        const nestedIn = nested ? this.closedRecord(nested, ["dataset", "field"], ["dataset", "field"]) : undefined
        const target = this.closedRecord(m.get("target")!, ["type", "interface"], ["type", "interface"])
        /** A list of closed records whose members are all strings, in declaration order. */
        const records = (key: string, allowed: ReadonlyArray<string>, required: ReadonlyArray<string>): Array<Record<string, string>> | undefined => {
          const node = m.get(key)
          if (!node) return undefined
          return this.list(node).map((e) => {
            const r = this.closedRecord(e, allowed, required)
            const out: Record<string, string> = {}
            for (const name of allowed) {
              const value = r.get(name)
              if (value) out[name] = key === "properties" && name === "value" ? this.mappingExpression(value) : this.string(value)
            }
            return out
          })
        }
        const gaps = records("gaps", ["member", "reason", "note"], ["member", "reason"])
        for (const g of gaps ?? []) {
          if (!["not_in_source", "defaulted", "deferred", "derived_elsewhere"].includes(g.reason!)) throw new CompileError("NORN-FLOW-LITERAL", "a gap reason is not_in_source, defaulted, deferred, or derived_elsewhere", m.get("gaps")!)
        }
        const decl = {
          id: this.string(m.get("id")!),
          source: { dataset: this.string(src.get("dataset")!), ...(nestedIn ? { nestedIn: { dataset: this.string(nestedIn.get("dataset")!), field: this.string(nestedIn.get("field")!) } } : {}) },
          target: { entity: pin(target.get("type")!, OBJECT_TYPES, "entity"), interface: pin(target.get("interface")!, INTERFACES, "interface") },
          ...(m.has("when") ? { when: this.zen(m.get("when")!) } : {}),
          subject: this.mappingExpression(m.get("subject")!),
          sourceRecord: this.mappingExpression(m.get("sourceRecord")!),
          properties: records("properties", ["property", "value"], ["property", "value"]),
          links: records("links", ["link", "targetMap", "key"], ["link", "targetMap", "key"]),
          lifecycles: records("lifecycles", ["property", "state", "transition", "onDelete"], ["property"]),
          gaps,
          ignoredFields: records("ignoredFields", ["field", "reason"], ["field", "reason"]),
        }
        // The member shapes are fixed by `records`; their closed grammar and the expressions are the Rust checker's.
        return decl as unknown as M.ObjectMapDecl
      }),
      at: this.at(call),
    })
  }

  private trigger(input: ts.Expression): R.RuleTrigger {
    const m = this.closedRecord(input, ["on", "type", "transition", "source"], ["on"])
    const on = this.string(m.get("on")!)
    const only = (allowed: ReadonlyArray<string>): void => {
      for (const key of m.keys()) if (key !== "on" && !allowed.includes(key)) throw new CompileError("NORN-FLOW-LITERAL", `a ${on} trigger takes no ${key}`, m.get(key)!)
      for (const key of allowed) if (!m.has(key)) throw new CompileError("NORN-FLOW-LITERAL", `a ${on} trigger needs ${key}`, input)
    }
    switch (on) {
      case "transitioned":
        only(["type", "transition"])
        return { on, objectType: this.typeName(m.get("type")!, OBJECT_TYPES), transition: this.string(m.get("transition")!) }
      case "created":
      case "deleted":
        only(["type"])
        return { on, objectType: this.typeName(m.get("type")!, OBJECT_TYPES) }
      case "observed":
        only(["source"])
        return { on, source: this.string(m.get("source")!) }
      default:
        throw new CompileError("NORN-FLOW-LITERAL", "a trigger is transitioned, created, deleted, or observed", m.get("on")!)
    }
  }

  private sliceEntry(input: ts.Expression): R.SliceDecl {
    const m = this.closedRecord(input, ["name", "type", "key", "optional"], ["name", "type", "key"])
    const keyNode = this.deref(m.get("key")!)
    let key: R.SliceDecl["key"]
    if (ts.isStringLiteral(keyNode) || ts.isNoSubstitutionTemplateLiteral(keyNode)) {
      if (keyNode.text !== "triggerObject") throw new CompileError("NORN-FLOW-LITERAL", "a slice key is \"triggerObject\", { property: { from, property } }, or { link: { from, link } }", keyNode)
      key = "triggerObject"
    } else {
      const k = this.closedRecord(keyNode, ["property", "link"], [])
      const property = k.get("property"), link = k.get("link")
      if ((property === undefined) === (link === undefined)) throw new CompileError("NORN-FLOW-LITERAL", "a slice key is { property: { from, property } } or { link: { from, link } }", keyNode)
      if (property) {
        const p = this.closedRecord(property, ["from", "property"], ["from", "property"])
        key = { property: { from: this.string(p.get("from")!), property: this.string(p.get("property")!) } }
      } else {
        const l = this.closedRecord(link!, ["from", "link"], ["from", "link"])
        key = { link: { from: this.string(l.get("from")!), link: this.string(l.get("link")!) } }
      }
    }
    const optional = m.get("optional")
    return {
      name: this.string(m.get("name")!),
      objectType: this.typeName(m.get("type")!, OBJECT_TYPES),
      key,
      ...(optional && this.boolean(optional) ? { optional: true } : {}),
    }
  }

  private key(input: ts.Expression): E.KeyDecl {
    const m = this.closedRecord(input, ["name", "properties"], ["name", "properties"])
    return { name: this.string(m.get("name")!), properties: this.strings(m.get("properties")!) }
  }

  private lifecycle(input: ts.Expression): E.LifecycleDecl {
    const m = this.closedRecord(input, ["property", "description", "initial", "terminal", "transitions"], ["property", "initial", "transitions"])
    const description = m.get("description"), terminal = m.get("terminal")
    return {
      property: this.string(m.get("property")!),
      ...(description ? { description: this.string(description) } : {}),
      initial: this.strings(m.get("initial")!),
      ...(terminal ? { terminal: this.strings(terminal) } : {}),
      transitions: this.list(m.get("transitions")!).map((t) => this.transition(t)),
    }
  }

  private transition(input: ts.Expression): E.TransitionDecl {
    const m = this.closedRecord(input, ["id", "description", "from", "to", "guards"], ["id", "from", "to"])
    const description = m.get("description"), guards = m.get("guards")
    return {
      id: this.string(m.get("id")!),
      ...(description ? { description: this.string(description) } : {}),
      from: this.strings(m.get("from")!),
      to: this.string(m.get("to")!),
      ...(guards ? { guards: this.list(guards).map((e) => this.zen(e, "self")) } : {}),
    }
  }

  // ------------------------------------------------------- literal data

  private record(input: ts.Expression): Map<string, ts.Expression> {
    const o = this.deref(input)
    if (!ts.isObjectLiteralExpression(o)) throw new CompileError("NORN-FLOW-LITERAL", "expected an object literal", o)
    const out = new Map<string, ts.Expression>()
    for (const p of o.properties) {
      this.guard(p)
      if (ts.isPropertyAssignment(p) && (ts.isIdentifier(p.name) || ts.isStringLiteral(p.name))) out.set(p.name.text, p.initializer)
      else if (ts.isShorthandPropertyAssignment(p)) out.set(p.name.text, p.name)
      else throw new CompileError("NORN-FLOW-LITERAL", "expected plain name: value members", p)
    }
    return out
  }

  private closedRecord(input: ts.Expression, allowed: ReadonlyArray<string>, required: ReadonlyArray<string>): Map<string, ts.Expression> {
    const m = this.record(input)
    for (const [key, value] of m) if (!allowed.includes(key)) throw new CompileError("NORN-FLOW-LITERAL", `unknown member ${key}`, value.parent)
    for (const key of required) if (!m.has(key)) throw new CompileError("NORN-FLOW-LITERAL", `missing member ${key}`, input)
    return m
  }

  private list(input: ts.Expression): ts.Expression[] {
    const a = this.deref(input)
    if (!ts.isArrayLiteralExpression(a)) throw new CompileError("NORN-FLOW-LITERAL", "expected an array literal", a)
    return a.elements.map((x) => {
      if (ts.isSpreadElement(x) || ts.isOmittedExpression(x)) throw new CompileError("NORN-FLOW-LITERAL", "array elements must be listed explicitly", x)
      this.guard(x)
      return x
    })
  }

  private string(input: ts.Expression): string {
    const e = this.deref(input)
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return e.text
    throw new CompileError("NORN-FLOW-LITERAL", "expected a string literal", e)
  }

  private strings(input: ts.Expression): string[] {
    return this.list(input).map((x) => this.string(x))
  }

  private boolean(input: ts.Expression): boolean {
    const e = this.deref(input)
    if (e.kind === ts.SyntaxKind.TrueKeyword) return true
    if (e.kind === ts.SyntaxKind.FalseKeyword) return false
    throw new CompileError("NORN-FLOW-LITERAL", "expected a boolean literal", e)
  }

  /**
   * Literal JSON data: strings, booleans, `null`, exact numbers, arrays, and
   * objects, through const references. Numbers whose canonical JSON would
   * change their value are refused.
   */
  private json(input: ts.Expression, depth = 0): Json {
    this.guard(input)
    if (depth > MAX_DATA_DEPTH) throw new CompileError("NORN-FLOW-UNBOUNDED", "literal data nesting exceeds the compiler bound", input)
    const e = this.deref(input)
    if (ts.isCallExpression(e) && this.id(e) === "@norn/authoring#Norn.unsafeZen") {
      this.warnings.push(diagnostic(e, "NORN-AUTHORING-UNSAFE-ZEN", "unsafeZen imports untyped ZEN/JDM; prefer a plain TypeScript decision callback"))
      return this.json(e.arguments[0]!, depth + 1)
    }
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return e.text
    if (e.kind === ts.SyntaxKind.TrueKeyword) return true
    if (e.kind === ts.SyntaxKind.FalseKeyword) return false
    if (e.kind === ts.SyntaxKind.NullKeyword) return null
    const deadline = this.started + this.limits.maxMillis
    if (ts.isNumericLiteral(e)) return exactNumber(e, deadline)
    if (ts.isPrefixUnaryExpression(e) && e.operator === ts.SyntaxKind.MinusToken && ts.isNumericLiteral(e.operand)) return -exactNumber(e.operand, deadline)
    if (ts.isArrayLiteralExpression(e)) return this.list(e).map((x) => this.json(x, depth + 1))
    if (ts.isObjectLiteralExpression(e)) {
      const out: Record<string, Json> = {}
      for (const [key, value] of this.record(e)) {
        // An ordinary object would turn this member into a prototype write and drop it.
        if (key === "__proto__") throw new CompileError("NORN-FLOW-LITERAL", "__proto__ is not a portable JSON member name", value.parent)
        out[key] = this.json(value, depth + 1)
      }
      return out
    }
    throw new CompileError("NORN-FLOW-LITERAL", "expected literal JSON data", e)
  }

  /**
   * A literal default. A numeric literal becomes its exact decimal source
   * text (trailing fraction zeros carry the scale); it is never parsed into
   * a binary float.
   */
  private defaultValue(input: ts.Expression): E.DefaultValue {
    const e = this.deref(input)
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return e.text
    if (e.kind === ts.SyntaxKind.TrueKeyword) return true
    if (e.kind === ts.SyntaxKind.FalseKeyword) return false
    const negative = ts.isPrefixUnaryExpression(e) && e.operator === ts.SyntaxKind.MinusToken
    const literal = ts.isPrefixUnaryExpression(e) && e.operator === ts.SyntaxKind.MinusToken ? this.unwrap(e.operand) : e
    if (ts.isNumericLiteral(literal)) {
      const text = literal.getText()
      if (!DECIMAL_TEXT.test(text)) {
        throw new CompileError("NORN-FLOW-LITERAL", "write a numeric default as a plain decimal literal (no exponent, separators, or radix prefix)", literal)
      }
      return { decimal: negative ? `-${text}` : text }
    }
    if (e.kind === ts.SyntaxKind.NullKeyword) throw new CompileError("NORN-FLOW-LITERAL", "a default cannot be null; omit it (an absent default already means no value)", e)
    throw new CompileError("NORN-FLOW-LITERAL", "a default is a string, boolean, or decimal literal", e)
  }
}
