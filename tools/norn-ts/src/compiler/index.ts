/** Syntax-only compilation. Authoring modules and callbacks are never imported or evaluated. */
import { readFileSync, statSync } from "node:fs"
import { createHash } from "node:crypto"
import { relative, resolve, sep } from "node:path"
import ts from "typescript"
import { COMPILER_OPTIONS, ProfileChecker, createHost, type Diagnostic } from "../check.ts"
import { INTRINSICS_PATH, TOOL_ROOT } from "../profile.ts"
import type { Json } from "../canonical.ts"
import type { FlowSourceMapDraft, FlowSourceMapLocation, FlowSourceMapStepKind } from "../flow-source-map.ts"
import { BudgetExceeded, ExpansionBudget } from "../builder/budget.ts"
import * as B from "../builder/index.ts"
import type { FlowDef, FnRef, Step, TaggedErrorDecl, SourceLocation, BuildOptions, RetryPolicy } from "../builder/index.ts"
import type { Expr } from "../builder/expr.ts"
import { CompileError, SchemaBudget, resultSchema, valueSchema } from "./types.ts"

export const COMPILER_LIMITS = { maxSourceBytes: 1_048_576, maxNodes: 100_000, maxModules: 128, maxDepth: 128, maxOutputBytes: 8_388_608, maxMillis: 30_000 } as const
export type CompilerLimits = { readonly [K in keyof typeof COMPILER_LIMITS]: number }
export type CompileResult =
  | { readonly ok: true; readonly definition: FlowDef; readonly artifacts: ReadonlyArray<Json>; readonly sourceMap: Json; readonly warnings: ReadonlyArray<Diagnostic>; readonly flowSourceMap?: FlowSourceMapDraft }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

type Env = ReadonlyMap<ts.Symbol, Expr>
interface Plan {
  readonly steps: ReadonlyArray<Step>
  readonly value: Expr
  /** Success types at this exact composition stage, before later pipe operators. */
  readonly outputs: ReadonlyArray<ts.Type>
}

/** Quotas are checked before local sources are parsed. Pinned declaration files are fixed inputs. */
function boundedHost(started: number, limits: CompilerLimits, declarations: Map<string, ts.SourceFile>): ts.CompilerHost {
  const host = createHost(), original = host.getSourceFile
  const sources = new Set<string>()
  let bytes = 0, tokens = 0
  host.getSourceFile = (file, languageVersion, onError, shouldCreateNewSourceFile) => {
    const path = resolve(file)
    const pinnedDeclarations = path.endsWith(".d.ts") && (path.startsWith(resolve(TOOL_ROOT, "node_modules/effect") + sep) || path.startsWith(resolve(TOOL_ROOT, "node_modules/typescript") + sep) || path === INTRINSICS_PATH)
    if (pinnedDeclarations && !shouldCreateNewSourceFile) {
      if (Date.now() - started >= limits.maxMillis) throw new CompileError("NORN-FLOW-UNBOUNDED", "cooperative compiler time bound exceeded", ts.createSourceFile(path, "", ts.ScriptTarget.ES2022))
      const known = declarations.get(path)
      if (known) return known
    }
    if (!pinnedDeclarations && !sources.has(path)) {
      if (host.fileExists(path) && statSync(path).size > limits.maxSourceBytes - bytes) {
        throw new CompileError("NORN-FLOW-UNBOUNDED", "source import closure exceeds compiler byte bound before read", ts.createSourceFile(path, "", ts.ScriptTarget.ES2022))
      }
      const text = host.readFile(path)
      if (text !== undefined) {
        const stub = ts.createSourceFile(path, "", ts.ScriptTarget.ES2022)
        sources.add(path); bytes += new TextEncoder().encode(text).length
        if (sources.size > limits.maxModules || bytes > limits.maxSourceBytes) throw new CompileError("NORN-FLOW-UNBOUNDED", "source import closure exceeds compiler module or byte bound", stub)
        const scanner = ts.createScanner(ts.ScriptTarget.ES2022, true, ts.LanguageVariant.Standard, text)
        let depth = 0
        for (let token = scanner.scan(); token !== ts.SyntaxKind.EndOfFileToken; token = scanner.scan()) {
          if (token === ts.SyntaxKind.OpenBraceToken || token === ts.SyntaxKind.OpenBracketToken || token === ts.SyntaxKind.OpenParenToken) depth++
          if (token === ts.SyntaxKind.CloseBraceToken || token === ts.SyntaxKind.CloseBracketToken || token === ts.SyntaxKind.CloseParenToken) depth--
          if (++tokens > limits.maxNodes || depth > limits.maxDepth || Date.now() - started >= limits.maxMillis) throw new CompileError("NORN-FLOW-UNBOUNDED", "source syntax exceeds compiler token, nesting or cooperative time bound", stub)
        }
      }
    }
    const source = original(file, languageVersion, onError, shouldCreateNewSourceFile)
    if (pinnedDeclarations && source) declarations.set(path, source)
    return source
  }
  return host
}

/** The exact JSON number a numeric literal denotes; refuses literals whose canonical JSON would change their value. */
export function exactNumber(node: ts.NumericLiteral, deadline: number): number {
  const check = (): void => {
    if (Date.now() >= deadline) throw new CompileError("NORN-FLOW-UNBOUNDED", "numeric normalization exceeds cooperative time bound", node)
  }
  check()
  const source = node.getText().replace(/_/g, ""), value = Number(source)
  check()
  if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) throw new CompileError("NORN-FLOW-EXPRESSION", "numeric literal has no exact JSON/IR representation", node)
  // Non-decimal literals are integers; the safe-integer check proves their exact representation.
  if (/^0[xXbBoO]/.test(source)) return value
  // Strip zeros lexically in linear time. No arbitrarily large integer is constructed or divided.
  const decimal = (text: string): readonly [string, number] => {
    const match = /^(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(text)
    if (!match) throw new CompileError("NORN-FLOW-EXPRESSION", "invalid decimal numeric literal", node)
    const digits = `${match[1] || "0"}${match[2] ?? ""}`
    let first = 0, end = digits.length
    while (first < end && digits.charCodeAt(first) === 48) { if ((first & 4095) === 0) check(); first++ }
    if (first === end) return ["0", 0]
    while (end > first && digits.charCodeAt(end - 1) === 48) { if ((end & 4095) === 0) check(); end-- }
    check()
    return [digits.slice(first, end), Number(match[3] ?? "0") - (match[2]?.length ?? 0) + digits.length - end]
  }
  const written = decimal(source), encoded = decimal(String(value))
  if (written[0] !== encoded[0] || written[1] !== encoded[1]) throw new CompileError("NORN-FLOW-EXPRESSION", "numeric literal loses precision in canonical JSON", node)
  return value
}

export function diagnostic(node: ts.Node, code: string, message: string): Diagnostic {
  const sf = node.getSourceFile()
  const start = sf.getLineAndCharacterOfPosition(node.getStart(sf))
  const end = sf.getLineAndCharacterOfPosition(node.getEnd())
  const text = node.getText(sf).replace(/\s+/g, " ").trim()
  return { code, start: { line: start.line + 1, column: start.character + 1 }, end: { line: end.line + 1, column: end.character + 1 }, construct: text.length > 80 ? `${text.slice(0, 77)}...` : text, message }
}

/** A profile-checked batch: one shared Program, the files that may be lowered, and every failure so far. */
export interface PreparedCompilation {
  readonly started: number
  readonly limits: CompilerLimits
  readonly profile: ProfileChecker
  /** The Program over the accepted roots; undefined when none was accepted. */
  readonly program: ts.Program | undefined
  readonly expansion: ExpansionBudget
  /** Resolved paths that passed the profile, in input order. */
  readonly accepted: ReadonlyArray<string>
  /** Resolved path → its diagnostics, for every root that cannot be lowered. */
  readonly failed: ReadonlyMap<string, ReadonlyArray<Diagnostic>>
}

/** Bounds, reads and profile-checks a batch. Shared by flow and definition compilation. */
export function prepareCompilation(files: ReadonlyArray<string>, overrides: Partial<CompilerLimits> = {}): PreparedCompilation {
  const started = Date.now()
  const limits = Object.fromEntries(Object.entries(COMPILER_LIMITS).map(([name, maximum]) => {
    const value = overrides[name as keyof CompilerLimits] ?? maximum
    if (!Number.isFinite(value) || value < 0 || !Number.isInteger(value)) throw new RangeError(`invalid compiler limit ${name}`)
    return [name, Math.min(value, maximum)]
  })) as CompilerLimits
  const profile = new ProfileChecker()
  const eligible: string[] = []
  const failed = new Map<string, ReadonlyArray<Diagnostic>>()
  let rootBytes = 0
  const readRoots = new Set<string>()
  for (const file of files) {
    const path = resolve(file)
    const rawBytes = readRoots.has(path) ? 0 : statSync(path).size
    if (rawBytes > limits.maxSourceBytes - rootBytes) {
      failed.set(path, [diagnostic(ts.createSourceFile(path, "", ts.ScriptTarget.ES2022), "NORN-FLOW-UNBOUNDED", "source exceeds compiler byte bound before read")])
      continue
    }
    const text = readFileSync(path, "utf8")
    if (new TextEncoder().encode(text).length > limits.maxSourceBytes) {
      const sf = ts.createSourceFile(path, "", ts.ScriptTarget.ES2022)
      failed.set(path, [diagnostic(sf, "NORN-FLOW-UNBOUNDED", "source exceeds compiler byte bound")])
    } else { eligible.push(path); rootBytes += rawBytes; readRoots.add(path) }
  }
  const expansion = new ExpansionBudget({ maxWork: limits.maxNodes, maxOutputBytes: limits.maxOutputBytes, deadline: started + limits.maxMillis })
  let reports: ReturnType<ProfileChecker["check"]>
  let program: ts.Program | undefined
  try {
    const budgetNode = ts.createSourceFile(eligible[0] ?? "input.ts", "", ts.ScriptTarget.ES2022)
    const cancellationToken: ts.CancellationToken = {
      isCancellationRequested: () => Date.now() - started >= limits.maxMillis,
      throwIfCancellationRequested: () => { if (Date.now() - started >= limits.maxMillis) throw new CompileError("NORN-FLOW-UNBOUNDED", "cooperative compiler time bound exceeded", budgetNode) },
    }
    // Pinned declaration syntax may be shared within this compilation. Keep
    // fresh Programs/checkers: profile queries can change union ordering, and
    // rejected authored roots must never contribute globals to lowering.
    const declarations = new Map<string, ts.SourceFile>()
    reports = profile.check(eligible, { host: boundedHost(started, limits, declarations), cancellationToken })
    const accepted = eligible.filter((_, i) => reports[i]!.diagnostics.length === 0)
    program = accepted.length > 0 ? ts.createProgram({ rootNames: [...accepted, INTRINSICS_PATH], options: COMPILER_OPTIONS, host: boundedHost(started, limits, declarations) }) : undefined
  } catch (error) {
    if (!(error instanceof CompileError)) throw error
    const fatal = [diagnostic(error.node, error.code, error.message)]
    for (const path of eligible) failed.set(path, fatal)
    return { started, limits, profile, program: undefined, expansion, accepted: [], failed }
  }
  const accepted: string[] = []
  for (let i = 0; i < eligible.length; i++) {
    const diagnostics = reports[i]!.diagnostics
    if (diagnostics.length > 0) failed.set(eligible[i]!, diagnostics)
    else accepted.push(eligible[i]!)
  }
  return { started, limits, profile, program, expansion, accepted, failed }
}

/** Bound the import closure a root lowers from: module count, bytes, syntax nodes and nesting. */
export function checkClosure(program: ts.Program, source: ts.SourceFile, limits: CompilerLimits): void {
  const local = program.getSourceFiles().filter((sf) => !sf.isDeclarationFile && !program.isSourceFileFromExternalLibrary(sf))
  if (local.length > limits.maxModules) throw new CompileError("NORN-FLOW-UNBOUNDED", "import closure exceeds compiler module bound", source)
  let nodes = 0
  const count = (node: ts.Node, depth: number): void => {
    if (++nodes > limits.maxNodes || depth > limits.maxDepth) throw new CompileError("NORN-FLOW-UNBOUNDED", "syntax exceeds compiler node or nesting bound", node)
    ts.forEachChild(node, (child) => count(child, depth + 1))
  }
  let bytes = 0
  for (const sf of local) { bytes += new TextEncoder().encode(sf.text).length; if (bytes > limits.maxSourceBytes) throw new CompileError("NORN-FLOW-UNBOUNDED", "import closure exceeds compiler byte bound", source); count(sf, 0) }
}

/** Batched profile recognition shares one Program; rejected roots never reach lowering. */
export function compileFiles(files: ReadonlyArray<string>, options: BuildOptions, overrides: Partial<CompilerLimits> = {}): CompileResult[] {
  const { started, limits, profile, program, expansion, accepted, failed } = prepareCompilation(files, overrides)
  const results = new Map<string, CompileResult>()
  for (const [path, diagnostics] of failed) results.set(path, { ok: false, diagnostics })
  for (const path of accepted) {
    const source = program!.getSourceFile(path)!
    try {
      const lowering = new SyntaxCompiler(profile, program!, source, started, limits, expansion, resolve(options.sourceRoot ?? resolve(TOOL_ROOT, "../..")))
      const definition = lowering.compile()
      expansion.assert(definition)
      const built = B.lowerFlow(definition, { ...options, expansionBudget: expansion })
      if (!built.ok) {
        results.set(path, { ok: false, diagnostics: built.diagnostics.map((d) => {
          const sf = d.at && program!.getSourceFiles().find((s) => relative(TOOL_ROOT, s.fileName).split(sep).join("/") === d.at!.file)
          const start = d.at ? { line: d.at.line, column: d.at.column ?? 1 } : { line: 1, column: 1 }
          const offset = sf?.getPositionOfLineAndCharacter(start.line - 1, start.column - 1)
          let target: ts.Node = source
          if (sf && offset !== undefined) {
            const find = (n: ts.Node): void => { if (n.getStart(sf) <= offset && n.getEnd() >= offset) { target = n; ts.forEachChild(n, find) } }
            find(sf)
          }
          return { ...diagnostic(target, d.code, d.message), start }
        }) })
      } else {
        lowering.checkBudget(source)
        const result = { definition, artifacts: built.artifacts, sourceMap: built.sourceMap, warnings: lowering.warnings, ...(built.flowSourceMap ? { flowSourceMap: built.flowSourceMap } : {}) }
        expansion.addOutput(result)
        results.set(path, { ok: true, ...result })
      }
    } catch (error) {
      if (error instanceof BudgetExceeded) { results.set(path, { ok: false, diagnostics: [diagnostic(source, "NORN-FLOW-UNBOUNDED", error.message)] }); continue }
      if (!(error instanceof CompileError)) throw error
      results.set(path, { ok: false, diagnostics: [diagnostic(error.node, error.code, error.message)] })
    }
  }
  return files.map((file) => results.get(resolve(file))!)
}

export function compileFile(file: string, options: BuildOptions, limits: Partial<CompilerLimits> = {}): CompileResult { return compileFiles([file], options, limits)[0]! }

export class SyntaxCompiler {
  private readonly checker: ts.TypeChecker
  private readonly names = new Set<string>()
  readonly warnings: Diagnostic[] = []
  readonly expressionSources = new WeakMap<Expr, ts.Expression>()
  private readonly constants = new Map<ts.Symbol, Expr>()
  private readonly resolving = new Set<ts.Symbol>()
  private readonly handles = new Map<ts.Symbol, FnRef>()
  private readonly errors = new Map<ts.Symbol, TaggedErrorDecl>()
  private readonly sourceMapOccurrences = new Map<string, number>()
  private ordinal = 0
  private depth = 0
  private readonly schemaBudget: SchemaBudget
  constructor(private readonly profile: ProfileChecker, private readonly program: ts.Program, private readonly source: ts.SourceFile, private readonly started: number, private readonly limits: CompilerLimits, private readonly expansion: ExpansionBudget, private readonly sourceRoot: string, private readonly subDecision?: (call: ts.CallExpression, input: Expr) => Expr) { this.checker = program.getTypeChecker(); this.schemaBudget = new SchemaBudget(expansion) }

  checkBudget(node: ts.Node): void {
    if (Date.now() - this.started >= this.limits.maxMillis || this.depth > this.limits.maxDepth) throw new CompileError("NORN-FLOW-UNBOUNDED", "compiler time or nesting bound exceeded", node)
  }
  private at(node: ts.Node): SourceLocation {
    const sf = node.getSourceFile(), p = sf.getLineAndCharacterOfPosition(node.getStart(sf))
    return { file: relative(TOOL_ROOT, sf.fileName).split(sep).join("/"), line: p.line + 1, column: p.character + 1 }
  }
  private sourceAt(node: ts.Node): FlowSourceMapLocation {
    const sf = node.getSourceFile(), p = sf.getLineAndCharacterOfPosition(node.getStart(sf))
    return { file: relative(this.sourceRoot, sf.fileName).split(sep).join("/"), line: p.line + 1, column: p.character + 1 }
  }
  private semanticTokens(node: ts.Node): string {
    const scanner = ts.createScanner(ts.ScriptTarget.Latest, true, ts.LanguageVariant.Standard, node.getText(node.getSourceFile()))
    const tokens: string[] = []
    while (true) {
      const kind = scanner.scan()
      if (kind === ts.SyntaxKind.EndOfFileToken) return tokens.join("\u001f")
      const text = scanner.getTokenText()
      tokens.push(`${kind}:${text.length}:${text}`)
    }
  }
  /** Explicit perform ids survive body edits; other sites use SHA-256 of trivia-free syntax tokens (128-bit prefix).
   * Identical repeated sites get a traversal-order suffix; changing tokens changes that site's id, while locations do not.
   */
  private sourceMapId(kind: FlowSourceMapStepKind, node: ts.Node, semanticId?: string): string {
    if (semanticId !== undefined) return `step_${kind}_${semanticId.replace(/[^A-Za-z0-9_-]/g, "_")}`
    const tokens = this.semanticTokens(node)
    const key = `${kind}\u0000${tokens}`
    const occurrence = this.sourceMapOccurrences.get(key) ?? 0
    this.sourceMapOccurrences.set(key, occurrence + 1)
    // The host compiler uses the platform crypto library; this fingerprint is
    // only for syntax-site identity, never for binding or admitting artifacts.
    const fingerprint = createHash("sha256").update(tokens).digest("hex").slice(0, 32)
    return `step_${kind}_${fingerprint}${occurrence === 0 ? "" : `_${occurrence + 1}`}`
  }
  private annotate<T extends Step>(step: T, node: ts.Node, kind: FlowSourceMapStepKind, semanticId?: string, branches?: ReadonlyArray<string>, otherwise?: string): T {
    return {
      ...step,
      sourceMap: {
        id: this.sourceMapId(kind, node, semanticId),
        kind,
        at: this.sourceAt(node),
        ...(branches ? { branchLabels: branches } : {}),
        ...(otherwise ? { otherwiseLabel: otherwise } : {}),
      },
    } as T
  }
  private symbol(node: ts.Node): ts.Symbol {
    const s = ts.isShorthandPropertyAssignment(node.parent) ? this.checker.getShorthandAssignmentValueSymbol(node.parent) : this.checker.getSymbolAtLocation(node)
    if (!s) throw new CompileError("NORN-FLOW-SYMBOL", "unresolved syntax symbol", node)
    return s.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(s) : s
  }
  private id(call: ts.CallExpression): string | undefined {
    const e = call.expression
    return this.profile.identity(this.program, ts.isPropertyAccessExpression(e) ? e.name : e)
  }
  private unwrap(e: ts.Expression): ts.Expression {
    while (ts.isParenthesizedExpression(e) || ts.isAsExpression(e) || ts.isSatisfiesExpression(e) || ts.isNonNullExpression(e) || ts.isTypeAssertionExpression(e)) e = e.expression
    return e
  }
  private fresh(hint = "value"): string {
    let name = /^[a-z][a-z0-9_]*$/.test(hint) ? hint : "value"
    if (this.names.has(name)) do { name = `value_${++this.ordinal}` } while (this.names.has(name))
    this.names.add(name)
    return name
  }
  private scalar(e: ts.Expression): string | number | boolean | null {
    const x = this.data(e, new Map())
    if (x._tag !== "Literal") throw new CompileError("NORN-FLOW-LITERAL", "expected scalar metadata literal", e)
    return x.value
  }
  private recordLiteral(e: ts.Expression): Record<string, Expr> {
    const x = this.data(e, new Map())
    if (x._tag !== "Record") throw new CompileError("NORN-FLOW-LITERAL", "metadata must be a record literal", e)
    return Object.fromEntries(x.fields)
  }
  private metadata(e: ts.Expression): { id: string; version: string } {
    const fields = this.recordLiteral(e)
    const id = fields.id, version = fields.version
    if (id?._tag !== "Literal" || typeof id.value !== "string" || version?._tag !== "Literal" || typeof version.value !== "string") throw new CompileError("NORN-FLOW-LITERAL", "id and version must be literal strings", e)
    return { id: id.value, version: version.value }
  }

  compile(): FlowDef {
    checkClosure(this.program, this.source, this.limits)
    const symbol = this.checker.getSymbolAtLocation(this.source)
    const exported = symbol && this.checker.getExportsOfModule(symbol).find((s) => s.name === "default")
    const target = exported && (exported.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(exported) : exported)
    const declaration = target?.declarations?.[0]
    let expression: ts.Expression | undefined
    if (declaration && ts.isExportAssignment(declaration)) expression = declaration.expression
    else if (declaration && ts.isVariableDeclaration(declaration)) expression = declaration.initializer
    if (!expression) throw new CompileError("NORN-FLOW-RESULT", "export one default Norn.flow definition", this.source)
    expression = this.unwrap(expression)
    if (ts.isIdentifier(expression)) { const d = this.symbol(expression).valueDeclaration; if (d && ts.isVariableDeclaration(d) && d.initializer) expression = this.unwrap(d.initializer) }
    if (!ts.isCallExpression(expression) || this.id(expression) !== "@norn/authoring#Norn.flow") throw new CompileError("NORN-FLOW-RESULT", "default export must be Norn.flow", expression)
    const meta = this.metadata(expression.arguments[0]!)
    return B.flow(meta, this.root(expression.arguments[1]!, new Map()), { at: this.at(expression) })
  }

  /** Pure decision callbacks share the exact expression lowering used by Effect flows. */
  decisionRows(fn: ts.ArrowFunction | ts.FunctionExpression): Array<{ condition?: Expr; value: Expr; at: SourceLocation }> {
    const env = new Map<ts.Symbol, Expr>()
    if (fn.parameters[0]) env.set(this.symbol(fn.parameters[0].name), B.X.ref("input"))
    const rows: Array<{ condition?: Expr; value: Expr; at: SourceLocation }> = []
    const visit = (statements: ReadonlyArray<ts.Statement>, bindings: Env, condition?: Expr): void => {
      this.checkBudget(fn)
      this.expansion.reserve(1)
      if (rows.length >= B.FLOW_LIMITS.maxRows) throw new CompileError("NORN-FLOW-UNBOUNDED", "decision exceeds the 1024-row bound", fn)
      if (statements.length === 0) throw new CompileError("NORN-FLOW-RESULT", "decision must return a value on every path", fn)
      const [statement, ...rest] = statements
      if (ts.isVariableStatement(statement!)) {
        const next = new Map(bindings)
        for (const d of statement.declarationList.declarations) {
          if (!d.initializer || !ts.isIdentifier(d.name)) throw new CompileError("NORN-FLOW-CAPTURE", "use const name = value", d)
          next.set(this.symbol(d.name), this.data(d.initializer, next))
        }
        visit(rest, next, condition)
        return
      }
      if (ts.isReturnStatement(statement!) && statement.expression) {
        rows.push({ ...(condition ? { condition } : {}), value: this.data(statement.expression, bindings), at: this.at(statement.expression) })
        return
      }
      if (ts.isIfStatement(statement!)) {
        const test = this.data(statement.expression, bindings)
        const branch = (body: ts.Statement): ts.Statement[] => ts.isBlock(body) ? [...body.statements] : [body]
        visit([...branch(statement.thenStatement), ...rest], new Map(bindings), condition ? B.X.and(condition, test) : test)
        const negative = B.X.not(test)
        visit([...(statement.elseStatement ? branch(statement.elseStatement) : []), ...rest], new Map(bindings), condition ? B.X.and(condition, negative) : negative)
        return
      }
      throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "decision callbacks admit const, if/else and return; put effects in Norn.flow", statement!)
    }
    if (ts.isBlock(fn.body)) visit(fn.body.statements, env)
    else rows.push({ value: this.data(fn.body, env), at: this.at(fn.body) })
    return rows
  }

  private data(input: ts.Expression, env: Env): Expr {
    const value = this.dataInner(input, env)
    if (!this.expressionSources.has(value)) this.expressionSources.set(value, input)
    try { this.expansion.assert(value) } catch (error) {
      if (error instanceof BudgetExceeded) throw new CompileError("NORN-FLOW-UNBOUNDED", error.message, input)
      throw error
    }
    return value
  }

  private dataInner(input: ts.Expression, env: Env): Expr {
    this.checkBudget(input)
    this.depth++
    try {
      const e = this.unwrap(input)
      if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return B.X.lit(e.text)
      if (ts.isNumericLiteral(e)) return B.X.lit(exactNumber(e, this.started + this.limits.maxMillis))
      if (e.kind === ts.SyntaxKind.TrueKeyword || e.kind === ts.SyntaxKind.FalseKeyword) return B.X.lit(e.kind === ts.SyntaxKind.TrueKeyword)
      if (e.kind === ts.SyntaxKind.NullKeyword) return B.X.lit(null)
      if (ts.isIdentifier(e)) {
        const symbol = this.symbol(e), bound = env.get(symbol) ?? this.constants.get(symbol)
        if (bound) return bound
        if (this.resolving.has(symbol)) throw new CompileError("NORN-FLOW-CAPTURE", "cyclic data constant", e)
        const d = symbol.valueDeclaration
        if (!d || !ts.isVariableDeclaration(d) || !d.initializer) throw new CompileError("NORN-FLOW-VALUE", "unbound pure value", e)
        this.resolving.add(symbol)
        try { const value = this.data(d.initializer, env); this.constants.set(symbol, value); return value } finally { this.resolving.delete(symbol) }
      }
      if (ts.isPropertyAccessExpression(e) || ts.isElementAccessExpression(e)) {
        if (ts.isPropertyAccessExpression(e)) {
          const s = this.checker.getSymbolAtLocation(e.name)
          const actual = s && (s.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(s) : s)
          const d = actual?.valueDeclaration
          if (d && ts.isVariableDeclaration(d) && d.initializer && d.getSourceFile() !== e.getSourceFile()) return this.data(d.initializer, env)
        }
        if (ts.isPropertyAccessExpression(e) && e.name.text === "length") {
          const t = this.checker.getTypeAtLocation(e.expression)
          if (this.checker.isArrayType(t) || this.checker.isTupleType(t)) return B.X.call("len", this.data(e.expression, env))
          if ((t.flags & ts.TypeFlags.StringLike) !== 0) throw new CompileError("NORN-FLOW-EXPRESSION", "string.length counts UTF-16 units in JavaScript; use Norn.byteLength(text) for the canonical UTF-8 byte count", e)
        }
        const base = this.data(e.expression, env)
        const key = ts.isPropertyAccessExpression(e) ? e.name.text : String(this.scalar(e.argumentExpression!))
        if (base._tag === "Ref") return B.X.ref(base.name, ...base.path, key)
        if (base._tag === "Record") { const field = base.fields.find(([name]) => name === key); if (field) return field[1] }
        if (base._tag === "List" && /^\d+$/.test(key) && base.items[Number(key)]) return base.items[Number(key)]!
        throw new CompileError("NORN-FLOW-SLICE", "property is not present in compiled data", e)
      }
      if (ts.isArrayLiteralExpression(e)) return B.X.list(...e.elements.map((x) => this.data(x as ts.Expression, env)))
      if (ts.isObjectLiteralExpression(e)) {
        const fields: Array<readonly [string, Expr]> = []
        for (const p of e.properties) {
          if (ts.isShorthandPropertyAssignment(p)) fields.push([p.name.text, this.data(p.name, env)])
          else if (ts.isPropertyAssignment(p) && (ts.isIdentifier(p.name) || ts.isStringLiteral(p.name) || ts.isNumericLiteral(p.name))) fields.push([p.name.text, this.data(p.initializer, env)])
          else throw new CompileError("NORN-FLOW-EXPRESSION", "unsupported record field", p)
        }
        return { _tag: "Record", fields }
      }
      if (ts.isPrefixUnaryExpression(e)) {
        if (e.operator === ts.SyntaxKind.ExclamationToken) return B.X.not(this.data(e.operand, env))
        if (e.operator === ts.SyntaxKind.MinusToken && ts.isNumericLiteral(e.operand)) return B.X.lit(-exactNumber(e.operand, this.started + this.limits.maxMillis))
        if (e.operator === ts.SyntaxKind.MinusToken) return B.X.binary("-", B.X.lit(0), this.data(e.operand, env))
      }
      if (ts.isConditionalExpression(e)) return B.X.conditional(this.data(e.condition, env), this.data(e.whenTrue, env), this.data(e.whenFalse, env))
      if (ts.isCallExpression(e)) {
        const id = this.id(e)
        if (id === "@norn/authoring#Norn.evaluate" && this.subDecision) return this.subDecision(e, this.data(e.arguments[1]!, env))
        if (id === "@norn/authoring#Norn.unsafeZen") {
          const text = this.scalar(e.arguments[0]!)
          if (typeof text !== "string" || !e.typeArguments?.[0]) throw new CompileError("NORN-FLOW-TYPE", "unsafeZen<T> requires its declared result type and literal ZEN text", e)
          this.warnings.push(diagnostic(e, "NORN-AUTHORING-UNSAFE-ZEN", "unsafeZen bypasses typed authoring; replace it with plain TypeScript"))
          return B.X.unsafeZen(text, valueSchema(this.checker, this.checker.getTypeFromTypeNode(e.typeArguments[0]), e, new Set(), this.schemaBudget))
        }
        if (id === "@norn/authoring#Norn.byteLength") return B.X.call("len", this.data(e.arguments[0]!, env))
        if (id === "@norn/authoring#Norn.dateBefore") return B.X.call("dateBefore", ...e.arguments.map((a) => this.data(a, env)))
        if (ts.isPropertyAccessExpression(e.expression)) {
          const receiver = this.data(e.expression.expression, env), name = e.expression.name.text
          const args = e.arguments.map((a) => this.data(a, env))
          if (name === "includes") {
            const t = this.checker.getTypeAtLocation(e.expression.expression)
            return this.checker.isArrayType(t) || this.checker.isTupleType(t) ? B.X.binary("in", args[0]!, receiver) : B.X.call("contains", receiver, ...args)
          }
          const methods = { toLowerCase: "lower", toUpperCase: "upper", trim: "trim", startsWith: "startsWith", endsWith: "endsWith" } as const
          if (name in methods) return B.X.call(methods[name as keyof typeof methods], receiver, ...args)
        }
      }
      if (ts.isBinaryExpression(e)) {
        const ops: Partial<Record<ts.SyntaxKind, (l: Expr, r: Expr) => Expr>> = {
          [ts.SyntaxKind.EqualsEqualsToken]: B.X.eq, [ts.SyntaxKind.EqualsEqualsEqualsToken]: B.X.eq,
          [ts.SyntaxKind.ExclamationEqualsToken]: B.X.ne, [ts.SyntaxKind.ExclamationEqualsEqualsToken]: B.X.ne,
          [ts.SyntaxKind.LessThanToken]: B.X.lt, [ts.SyntaxKind.LessThanEqualsToken]: B.X.le,
          [ts.SyntaxKind.GreaterThanToken]: B.X.gt, [ts.SyntaxKind.GreaterThanEqualsToken]: B.X.ge,
          [ts.SyntaxKind.PlusToken]: (l, r) => B.X.binary("+", l, r),
          [ts.SyntaxKind.MinusToken]: (l, r) => B.X.binary("-", l, r),
          [ts.SyntaxKind.AsteriskToken]: (l, r) => B.X.binary("*", l, r),
          [ts.SyntaxKind.SlashToken]: (l, r) => B.X.binary("/", l, r),
          [ts.SyntaxKind.PercentToken]: (l, r) => B.X.binary("%", l, r),
          [ts.SyntaxKind.QuestionQuestionToken]: B.X.coalesce,
          [ts.SyntaxKind.AmpersandAmpersandToken]: B.X.and, [ts.SyntaxKind.BarBarToken]: B.X.or,
        }
        const op = ops[e.operatorToken.kind]
        if (op) return op(this.data(e.left, env), this.data(e.right, env))
      }
      throw new CompileError("NORN-FLOW-EXPRESSION", "expression has no first-order lowering", e)
    } finally { this.depth-- }
  }

  private error(symbol: ts.Symbol, node: ts.Node): TaggedErrorDecl {
    const known = this.errors.get(symbol)
    if (known) return known
    const declaration = symbol.declarations?.find(ts.isClassDeclaration)
    const heritage = declaration?.heritageClauses?.[0]?.types[0]
    if (!heritage || !ts.isCallExpression(heritage.expression) || this.id(heritage.expression) !== "effect/Data#TaggedError") throw new CompileError("NORN-FLOW-TAG", "error must be a registered Data.TaggedError class", node)
    const tag = this.scalar(heritage.expression.arguments[0]!)
    const fields = heritage.typeArguments?.[0]
    const schema = fields ? valueSchema(this.checker, this.checker.getTypeFromTypeNode(fields), fields, new Set(), this.schemaBudget) : B.S.record({})
    if (typeof tag !== "string" || schema.type !== "object") throw new CompileError("NORN-FLOW-TAG", "tagged errors need a literal tag and record fields", node)
    const result = B.taggedError(tag, schema, { at: this.at(declaration!) }); this.errors.set(symbol, result); return result
  }
  private handle(e: ts.Expression): FnRef {
    const symbol = this.symbol(e), known = this.handles.get(symbol)
    if (known) return known
    const d = symbol.valueDeclaration
    if (!d || !ts.isVariableDeclaration(d) || !d.initializer || !ts.isCallExpression(d.initializer)) throw new CompileError("NORN-FLOW-SYMBOL", "expected a declared Function handle", e)
    const call = d.initializer, args = call.typeArguments
    if (this.id(call) !== "@norn/authoring#Norn.fn" || !args?.[0] || !args[1]) throw new CompileError("NORN-FLOW-VALUE", "Norn.fn needs explicit structural I/O types", e)
    const input = valueSchema(this.checker, this.checker.getTypeFromTypeNode(args[0]), args[0], new Set(), this.schemaBudget)
    if (input.type !== "object") throw new CompileError("NORN-FLOW-VALUE", "Function input must be a closed record", args[0])
    const output = valueSchema(this.checker, this.checker.getTypeFromTypeNode(args[1]), args[1], new Set(), this.schemaBudget)
    const errors: TaggedErrorDecl[] = []
    const third = args[2]
    if (third) {
      const type = this.checker.getTypeFromTypeNode(third)
      for (const t of type.isUnion() ? type.types : [type]) {
        if (t.flags & ts.TypeFlags.Never) continue
        const errorSymbol = t.getSymbol()
        if (!errorSymbol) throw new CompileError("NORN-FLOW-TAG", "Function failures must be tagged error declarations", third)
        errors.push(this.error(errorSymbol, third))
      }
    }
    const result = B.fn({ ...this.metadata(call.arguments[0]!), input, output, errors, at: this.at(call) }); this.handles.set(symbol, result); return result
  }

  private duration(e: ts.Expression): number {
    const value = this.scalar(e)
    if (typeof value === "number") return value
    if (typeof value === "string") {
      const match = /^\s*(\d+(?:\.\d+)?)\s*(millis(?:econds?)?|ms|seconds?|s|minutes?|m|hours?|h|days?|d)\s*$/.exec(value)
      if (match) { const unit = match[2]!, scale = /^(millis|ms)/.test(unit) ? 1 : /^(second|s$)/.test(unit) ? 1000 : /^(minute|m$)/.test(unit) ? 60_000 : /^(hour|h$)/.test(unit) ? 3_600_000 : 86_400_000; return Number(match[1]) * scale }
    }
    throw new CompileError("NORN-FLOW-LITERAL", "duration must be numeric milliseconds or a finite duration string", e)
  }
  private policy(e: ts.Expression): RetryPolicy {
    const call = this.unwrap(e)
    if (!ts.isCallExpression(call)) throw new CompileError("NORN-FLOW-LITERAL", "expected literal Schedule policy", e)
    switch (this.id(call)) {
      case "effect/Schedule#recurs": {
        const recurs = Number(this.scalar(call.arguments[0]!))
        if (!Number.isInteger(recurs) || recurs < 1 || recurs > B.FLOW_LIMITS.maxRecurs) throw new CompileError("NORN-FLOW-UNBOUNDED", "each finite schedule count must satisfy the retry bound", call)
        return { recurs }
      }
      case "effect/Schedule#spaced": return { delay: { kind: "spaced", millis: this.duration(call.arguments[0]!) } }
      case "effect/Schedule#exponential": return { delay: { kind: "exponential", baseMillis: this.duration(call.arguments[0]!), factor: call.arguments[1] ? Number(this.scalar(call.arguments[1])) : 2 } }
      case "effect/Schedule#max": {
        const list = call.arguments[0]
        if (!list || !ts.isArrayLiteralExpression(list)) break
        let policy: RetryPolicy = {}
        for (const item of list.elements) {
          const part = this.policy(item as ts.Expression)
          const recurs = part.recurs === undefined ? policy.recurs : policy.recurs === undefined ? part.recurs : Math.min(policy.recurs, part.recurs)
          const delay = part.delay === undefined ? policy.delay : policy.delay === undefined ? part.delay : { kind: "max" as const, delays: [policy.delay, part.delay] }
          policy = { ...(recurs === undefined ? {} : { recurs }), ...(delay === undefined ? {} : { delay }) }
        }
        return policy
      }
    }
    throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "unsupported Schedule policy", e)
  }

  /** Normalize pipes into static calls without invoking their curried callbacks. */
  private pipe(e: ts.CallExpression): { base: ts.Expression; operators: ReadonlyArray<ts.Expression> } | undefined {
    const id = this.id(e)
    if (id === "effect/Pipeable#Pipeable.pipe" && ts.isPropertyAccessExpression(e.expression)) return { base: e.expression.expression, operators: e.arguments }
    if (id === "effect/Function#pipe") return { base: e.arguments[0]!, operators: e.arguments.slice(1) }
    return undefined
  }
  private callback(e: ts.Expression): ts.ArrowFunction | ts.FunctionExpression {
    if (!ts.isArrowFunction(e) && !ts.isFunctionExpression(e)) throw new CompileError("NORN-FLOW-CAPTURE", "continuation must be inline syntax", e)
    return e
  }
  private callbackEnv(fn: ts.ArrowFunction | ts.FunctionExpression, value: Expr, env: Env): Env {
    const next = new Map(env)
    if (fn.parameters[0]) next.set(this.symbol(fn.parameters[0].name), value)
    return next
  }
  private effectOutputs(type: ts.Type, node: ts.Node): ReadonlyArray<ts.Type> {
    if (type.flags & ts.TypeFlags.Never) return []
    if (type.isUnion()) return type.types.flatMap((branch) => this.effectOutputs(branch, node))
    const args = (type.flags & ts.TypeFlags.Object) ? this.checker.getTypeArguments(type as ts.TypeReference) : []
    if (!args[0]) throw new CompileError("NORN-FLOW-VALUE", "effect output needs a closed value type", node)
    return args[0].flags & ts.TypeFlags.Never ? [] : [args[0]]
  }
  private outputs(e: ts.Expression): ReadonlyArray<ts.Type> { return this.effectOutputs(this.checker.getTypeAtLocation(e), e) }
  private outputSchema(outputs: ReadonlyArray<ts.Type>, node: ts.Node): B.S.ValueSchema {
    // An effect with no success path never reaches its Value terminator.
    if (outputs.length === 0) return B.S.nullValue
    let schema = resultSchema(this.checker, outputs[0]!, node, this.schemaBudget)
    for (const output of outputs.slice(1)) {
      const next = resultSchema(this.checker, output, node, this.schemaBudget)
      const merged = B.S.mergeSchemas(schema, next)
      if (typeof merged === "string") throw new CompileError("NORN-FLOW-VALUE", `effect results have no closed shared shape: ${merged}`, node)
      this.expansion.assert(merged)
      schema = merged
    }
    return schema
  }
  private compose(body: ReadonlyArray<Step>, outputs: ReadonlyArray<ts.Type>, node: ts.Node, hint?: string): Plan {
    const schema = this.outputSchema(outputs, node)
    const name = this.fresh(hint)
    const composed = this.annotate({ _tag: "Compose", name, schema, body, at: this.at(node) }, node, "compose")
    return { steps: [composed], value: B.X.ref(name), outputs }
  }
  private returned(value: Expr, node: ts.Node, root: boolean): Step {
    return root ? this.annotate(B.succeed(value, { at: this.at(node) }), node, "succeed") : { _tag: "Value", value, at: this.at(node) }
  }

  private callbackPlan(fn: ts.ArrowFunction | ts.FunctionExpression, env: Env, pure: boolean, hint?: string): Plan {
    const signature = this.checker.getSignatureFromDeclaration(fn)
    if (!signature) throw new CompileError("NORN-FLOW-TYPE", "callback has no signature", fn)
    const returnType = this.checker.getReturnTypeOfSignature(signature)
    if (!ts.isBlock(fn.body)) return pure ? { steps: [], value: this.data(fn.body, env), outputs: [returnType] } : this.effect(fn.body, env, hint)
    const outputs = pure ? [returnType] : this.effectOutputs(returnType, fn)
    return this.compose(this.statements(fn.body.statements, env, false, !pure), outputs, fn, hint)
  }

  private effect(input: ts.Expression, env: Env, hint?: string | null): Plan {
    this.checkBudget(input)
    const e = this.unwrap(input)
    if (!ts.isCallExpression(e)) throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "expected visible effect syntax", e)
    const pipe = this.pipe(e)
    if (pipe) {
      let plan = this.effect(pipe.base, env, hint)
      for (const operator of pipe.operators) {
        if (!ts.isCallExpression(operator)) throw new CompileError("NORN-FLOW-CAPTURE", "pipe operator must be visible syntax", operator)
        plan = this.operator(operator, plan, env, hint)
      }
      return plan
    }
    const a = e.arguments
    switch (this.id(e)) {
      case "@norn/authoring#Norn.perform": {
        const name = hint === null ? undefined : this.fresh(hint ?? "result")
        const step = String(this.scalar(a[0]!))
        const node = this.annotate(B.perform(step, this.handle(a[1]!), this.data(a[2]!, env), { ...(name ? { bind: name } : {}), at: this.at(e) }), e, "perform", step)
        return { steps: [node], value: name ? B.X.ref(name) : B.X.lit(null), outputs: this.outputs(e) }
      }
      case "effect/Effect#succeed": return { steps: [], value: this.data(a[0]!, env), outputs: this.outputs(e) }
      case "effect/Effect#sleep": {
        const step = hint ?? `sleep_${++this.ordinal}`
        const node = this.annotate(B.sleep(step, { millis: this.duration(a[0]!) }, { at: this.at(e) }), e, "sleep")
        return { steps: [node], value: B.X.lit(null), outputs: this.outputs(e) }
      }
      case "effect/Effect#fail": {
        const error = a[0]
        if (!error || !ts.isNewExpression(error)) throw new CompileError("NORN-FLOW-TAG", "expected tagged error construction", e)
        const fields = error.arguments?.[0]
        const node = this.annotate(B.fail(this.error(this.symbol(error.expression), error), fields ? this.data(fields, env) : B.X.record({}), { at: this.at(e) }), e, "fail")
        return { steps: [node], value: B.X.lit(null), outputs: [] }
      }
      case "effect/Effect#all": {
        const list = a[0]
        if (!list || !ts.isArrayLiteralExpression(list)) throw new CompileError("NORN-FLOW-UNBOUNDED", "Effect.all needs a literal tuple", e)
        const steps: Step[] = [], values: Expr[] = []
        for (const item of list.elements) { const part = this.effect(item as ts.Expression, env); steps.push(...part.steps); values.push(part.value) }
        const node: Step = this.annotate({ _tag: "All", steps, at: this.at(e) }, e, "all")
        return { steps: [node], value: B.X.list(...values), outputs: this.outputs(e) }
      }
      case "effect/Effect#gen": {
        const fn = a[0]
        if (!fn || !ts.isFunctionExpression(fn)) throw new CompileError("NORN-FLOW-CAPTURE", "generator must be inline", e)
        return this.compose(this.statements(fn.body.statements, env, false, false), this.outputs(e), e, hint ?? undefined)
      }
      case "effect/Effect#map":
      case "effect/Effect#flatMap":
      case "effect/Effect#catchTag":
      case "effect/Effect#retry": {
        const plan = this.effect(a[0]!, env, this.id(e) === "effect/Effect#retry" ? hint : undefined)
        return this.operator(e, plan, env, hint, true)
      }
    }
    throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "effect syntax has no lowering", e)
  }

  private operator(call: ts.CallExpression, plan: Plan, env: Env, hint?: string | null, direct = false): Plan {
    const a = call.arguments, offset = direct ? 1 : 0
    switch (this.id(call)) {
      case "effect/Effect#map":
      case "effect/Effect#flatMap": {
        const fn = this.callback(a[offset]!), next = this.callbackPlan(fn, this.callbackEnv(fn, plan.value, env), this.id(call) === "effect/Effect#map", hint ?? undefined)
        return { steps: [...plan.steps, ...next.steps], value: next.value, outputs: next.outputs }
      }
      case "effect/Effect#retry": {
        if (plan.steps.length !== 1 || plan.steps[0]?._tag !== "Perform") throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "retry wraps exactly one Norn.perform", call)
        const perform = plan.steps[0]
        return { steps: [this.annotate(B.retry(perform, this.policy(a[offset]!), { at: this.at(call) }), call, "retry", perform.step)], value: plan.value, outputs: plan.outputs }
      }
      case "effect/Effect#catchTag": {
        const fn = this.callback(a[offset + 1]!), param = fn.parameters[0], name = param ? this.fresh(param.name.getText()) : undefined
        const nextEnv = name ? this.callbackEnv(fn, B.X.ref(name), env) : env
        const handler = this.callbackPlan(fn, nextEnv, false)
        const body: Step[] = [...plan.steps, this.returned(plan.value, call, false)]
        const recovery: Step[] = [...handler.steps, this.returned(handler.value, fn, false)]
        const caught = this.annotate(B.catchTag(body, String(this.scalar(a[offset]!)), recovery, { ...(name ? { as: name } : {}), at: this.at(call) }), call, "catchTag")
        return this.compose([caught], [...plan.outputs, ...handler.outputs], call, hint ?? undefined)
      }
    }
    throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "unsupported pipe operator", call)
  }

  private root(input: ts.Expression, env: Env): ReadonlyArray<Step> {
    const e = this.unwrap(input)
    if (ts.isCallExpression(e)) {
      const pipe = this.pipe(e)
      if (pipe && pipe.operators.every((op) => ts.isCallExpression(op) && this.id(op) === "effect/Effect#catchTag")) {
        let body = this.root(pipe.base, env)
        for (const operator of pipe.operators) {
          const op = operator as ts.CallExpression, fn = this.callback(op.arguments[1]!), p = fn.parameters[0]
          const name = p ? this.fresh(p.name.getText()) : undefined
          const next = name ? this.callbackEnv(fn, B.X.ref(name), env) : env
          const handler = ts.isBlock(fn.body) ? this.statements(fn.body.statements, next, true, true) : this.root(fn.body, next)
          body = [this.annotate(B.catchTag(body, String(this.scalar(op.arguments[0]!)), handler, { ...(name ? { as: name } : {}), at: this.at(op) }), op, "catchTag")]
        }
        return body
      }
      if (this.id(e) === "effect/Effect#gen") {
        const fn = e.arguments[0]
        if (fn && ts.isFunctionExpression(fn)) return this.statements(fn.body.statements, env, true, false)
      }
    }
    const plan = this.effect(e, env)
    return [...plan.steps, this.annotate(B.succeed(plan.value, { at: this.at(e) }), e, "succeed")]
  }

  private statements(statements: ReadonlyArray<ts.Statement>, initial: Env, root: boolean, effectReturn: boolean): ReadonlyArray<Step> {
    const env = new Map(initial), steps: Step[] = []
    for (const s of statements) {
      this.checkBudget(s)
      if (ts.isVariableStatement(s)) {
        for (const d of s.declarationList.declarations) {
          if (!ts.isIdentifier(d.name) || !d.initializer) throw new CompileError("NORN-FLOW-CAPTURE", "const binding needs an identifier and value", d)
          const init = this.unwrap(d.initializer)
          const plan = ts.isYieldExpression(init) ? this.effect(init.expression!, env, d.name.text) : { steps: [], value: this.data(init, env) }
          steps.push(...plan.steps); env.set(this.symbol(d.name), plan.value)
        }
      } else if (ts.isExpressionStatement(s) && ts.isYieldExpression(s.expression)) {
        steps.push(...this.effect(s.expression.expression!, env, null).steps)
      } else if (ts.isReturnStatement(s)) {
        const e = s.expression!
        if (effectReturn) {
          if (root) steps.push(...this.root(e, env))
          else { const plan = this.effect(e, env); steps.push(...plan.steps, this.returned(plan.value, s, false)) }
        } else if (ts.isYieldExpression(e)) { const plan = this.effect(e.expression!, env); steps.push(...plan.steps, this.returned(plan.value, s, root)) }
        else steps.push(this.returned(this.data(e, env), s, root))
        break
      } else if (ts.isIfStatement(s)) {
        const then = this.statementBody(s.thenStatement, env, root, effectReturn)
        const otherwise = s.elseStatement ? this.statementBody(s.elseStatement, env, root, effectReturn) : []
        steps.push(this.annotate(B.route([{ when: this.data(s.expression, env), then }], otherwise, { at: this.at(s) }), s, "if", undefined, ["then"], "else"))
      } else if (ts.isSwitchStatement(s)) {
        const branches: Array<{ when: Expr; then: ReadonlyArray<Step> }> = [], value = this.data(s.expression, env)
        let otherwise: ReadonlyArray<Step> = []
        let defaultPresent = false
        for (const clause of s.caseBlock.clauses) {
          const body = this.statements(clause.statements.filter((x) => !ts.isBreakStatement(x)), env, root, effectReturn)
          if (ts.isCaseClause(clause) && body.length > 0) branches.push({ when: B.X.eq(value, this.data(clause.expression, env)), then: body })
          else if (ts.isDefaultClause(clause)) { otherwise = body; defaultPresent = true }
        }
        steps.push(this.annotate(B.route(branches, otherwise, { at: this.at(s) }), s, "switch", undefined, branches.map((_, index) => `case-${index + 1}`), defaultPresent ? "default" : "otherwise"))
      } else if (ts.isBlock(s)) steps.push(...this.statements(s.statements, env, root, effectReturn))
      else if (!ts.isEmptyStatement(s) && !ts.isBreakStatement(s)) throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "statement has no lowering", s)
    }
    return steps
  }
  private statementBody(s: ts.Statement, env: Env, root: boolean, effectReturn: boolean): ReadonlyArray<Step> { return this.statements(ts.isBlock(s) ? s.statements : [s], env, root, effectReturn) }
}
