/**
 * `norn-effect/1` profile checker (T7-EFX-1).
 *
 * Uses the pinned TypeScript Compiler API to resolve every reference by
 * symbol (following import and re-export aliases), then walks the source
 * against the positive grammar of the profile. It reports admitted profile
 * symbol uses and `NORN-FLOW-*` diagnostics with source spans. It does not
 * lower anything: the syntax compiler (T7-EFX-2) reuses this recognition and
 * adds lowering, slices, and size bounds.
 *
 * Recognition is by resolved declaration, never by spelling: `effect` and
 * `effect/*` are always resolved against the pinned package under this tool,
 * so a vendored or lookalike copy next to a source file cannot impersonate it.
 */
import { readFileSync } from "node:fs"
import { join, relative, resolve as resolvePath, sep } from "node:path"
import ts from "typescript"
import {
  type AdmittedSymbol,
  INTRINSICS_PATH,
  type Profile,
  ProfileError,
  TOOL_ROOT,
  loadProfile,
} from "./profile.ts"

export interface Position {
  readonly line: number
  readonly column: number
}

export interface Diagnostic {
  readonly code: string
  readonly start: Position
  readonly end: Position
  readonly construct: string
  readonly message: string
}

export interface SymbolUse {
  readonly at: string
  readonly symbol: string
  readonly row: string
}

export interface FileReport {
  readonly file: string
  readonly profile: string
  readonly diagnostics: ReadonlyArray<Diagnostic>
  readonly uses: ReadonlyArray<SymbolUse>
}

export interface CheckOptions {
  readonly host?: ts.CompilerHost
  readonly cancellationToken?: ts.CancellationToken
}

interface Ctx {
  /** Inside the body argument of a `Norn.flow` call. */
  readonly flow: boolean
  /** Directly inside an `Effect.gen` generator body (yield* allowed). */
  readonly generator: boolean
  /** Inside the policy argument of `Effect.retry`. */
  readonly retryPolicy: boolean
  /** Inside an `Effect.map` callback: a pure value expression, no effect-producing calls. */
  readonly pure: boolean
  /**
   * The expression position admits an effect (a `yield*` operand, the effect argument of an
   * admitted combinator, a flatMap/catchTag continuation result). Every other flow position
   * is data, where an effect-producing call is NORN-FLOW-VALUE.
   */
  readonly effect: boolean
  /** What a `return` in the enclosing body produces. */
  readonly ret: "none" | "flow-result" | "map-result" | "effect"
}

const TOP: Ctx = { flow: false, generator: false, retryPolicy: false, pure: false, effect: false, ret: "none" }

/** Host globals that read time, entropy, IO, or the environment. */
const AMBIENT_ROOTS = new Set([
  "Date",
  "performance",
  "crypto",
  "fetch",
  "setTimeout",
  "setInterval",
  "queueMicrotask",
  "process",
  "console",
  "globalThis",
  "navigator",
  "XMLHttpRequest",
  "WebSocket",
  "localStorage",
  "require",
  "Deno",
  "Bun",
])

const EFFECT_DIST = join(TOOL_ROOT, "node_modules", "effect", "dist") + sep
const RESOLVE_ANCHOR = join(TOOL_ROOT, "__norn_resolve_anchor__.ts")

export const COMPILER_OPTIONS: ts.CompilerOptions = {
  strict: true,
  target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  lib: ["lib.esnext.d.ts", "lib.dom.d.ts"],
  types: [],
  skipLibCheck: true,
  noEmit: true,
  allowImportingTsExtensions: true,
}

/** Refuse to run against anything but the pinned Effect and Compiler API. */
export function assertPins(profile: Profile): void {
  if (ts.version !== profile.pins.typescript.version) {
    throw new ProfileError(
      `Compiler API ${ts.version} does not match pinned typescript@${profile.pins.typescript.version}`,
    )
  }
  const effectPkg = JSON.parse(
    readFileSync(join(TOOL_ROOT, "node_modules", "effect", "package.json"), "utf8"),
  ) as { version: string }
  if (effectPkg.version !== profile.pins.effect.version) {
    throw new ProfileError(
      `effect ${effectPkg.version} does not match pinned effect@${profile.pins.effect.version}`,
    )
  }
}

export function createHost(): ts.CompilerHost {
  const host = ts.createCompilerHost(COMPILER_OPTIONS, true)
  host.resolveModuleNameLiterals = (literals, containingFile, redirected, options) =>
    literals.map((literal) => {
      const name = literal.text
      // Pin `effect` resolution to this tool's lockfile-installed copy.
      const from = name === "effect" || name.startsWith("effect/") ? RESOLVE_ANCHOR : containingFile
      return ts.resolveModuleName(name, from, options, host, undefined, redirected)
    })
  return host
}

interface Resolved {
  /** Profile identity (`effect/Effect#gen`, `@norn/authoring#Norn.flow`) if declared in a profile module. */
  readonly id?: string
  readonly namespace: boolean
  readonly target?: ts.Symbol
  readonly declaration?: ts.Declaration
}

export class ProfileChecker {
  readonly profile: Profile
  private readonly admitted = new Map<string, AdmittedSymbol>()
  private readonly rejected = new Map<string, string>()
  private readonly spellings = new Set<string>()
  private readonly bareSpellings = new Set<string>()

  constructor(profile: Profile = loadProfile()) {
    this.profile = profile
    assertPins(profile)
    for (const a of profile.admitted) {
      this.admitted.set(a.symbol, a)
      if (a.use === "method") continue
      const parts = a.probe.split(".").slice(1)
      this.spellings.add(parts.join("."))
      const last = parts[parts.length - 1]
      if (last) this.bareSpellings.add(last)
    }
    for (const r of profile.rejected) this.rejected.set(r.symbol, r.code)
  }

  /** Check source files; one Program is shared so Effect's declarations load once. */
  check(files: ReadonlyArray<string>, options: CheckOptions = {}): FileReport[] {
    const absolute = files.map((f) => resolvePath(f))
    const program = ts.createProgram({
      rootNames: [...absolute, INTRINSICS_PATH],
      options: COMPILER_OPTIONS,
      host: options.host ?? createHost(),
    })
    const checker = program.getTypeChecker()
    const walked = new Map<ts.SourceFile, FileReport>()
    const walk = (sf: ts.SourceFile): FileReport => {
      let r = walked.get(sf)
      if (!r) {
        r = new FileWalk(this, program, checker, sf).run(options.cancellationToken)
        walked.set(sf, r)
      }
      return r
    }
    return absolute.map((file) => {
      const sf = program.getSourceFile(file)
      if (!sf) throw new Error(`not in program: ${file}`)
      const own = walk(sf)
      const imports = this.importDiagnostics(sf, program, checker, walk)
      if (imports.length === 0) return own
      const diagnostics = [...own.diagnostics, ...imports].sort(
        (a, b) => a.start.line - b.start.line || a.start.column - b.start.column || a.code.localeCompare(b.code),
      )
      return { ...own, diagnostics }
    })
  }

  /** Local source modules a file imports or re-exports (not packages, libs, or the intrinsics). */
  private localImports(sf: ts.SourceFile, program: ts.Program, checker: ts.TypeChecker) {
    const out: Array<{ specifier: ts.Expression; target: ts.SourceFile }> = []
    for (const st of sf.statements) {
      const specifier =
        (ts.isImportDeclaration(st) || ts.isExportDeclaration(st)) && st.moduleSpecifier ? st.moduleSpecifier : undefined
      if (!specifier) continue
      const target = checker.getSymbolAtLocation(specifier)?.declarations?.[0]?.getSourceFile()
      if (!target || target.isDeclarationFile || program.isSourceFileFromExternalLibrary(target)) continue
      if (target.fileName.includes(`${sep}node_modules${sep}`) || target.fileName.includes("/node_modules/")) continue
      out.push({ specifier, target })
    }
    return out
  }

  /**
   * Definitions are checked as a closure: an imported local module that fails the
   * profile (directly or through its own imports) fails the importing file at the
   * import specifier with NORN-FLOW-IMPORT.
   */
  private importDiagnostics(
    root: ts.SourceFile,
    program: ts.Program,
    checker: ts.TypeChecker,
    walk: (sf: ts.SourceFile) => FileReport,
  ): Diagnostic[] {
    const firstFailure = (sf: ts.SourceFile, seen: Set<ts.SourceFile>): string | undefined => {
      if (seen.has(sf)) return undefined
      seen.add(sf)
      const r = walk(sf)
      const d = r.diagnostics[0]
      if (d) return `${r.file}:${d.start.line}:${d.start.column} ${d.code} (${d.construct})`
      for (const dep of this.localImports(sf, program, checker)) {
        const found = firstFailure(dep.target, seen)
        if (found) return found
      }
      return undefined
    }
    const out: Diagnostic[] = []
    for (const { specifier, target } of this.localImports(root, program, checker)) {
      const found = firstFailure(target, new Set([root]))
      if (!found) continue
      const start = root.getLineAndCharacterOfPosition(specifier.getStart(root))
      const end = root.getLineAndCharacterOfPosition(specifier.getEnd())
      out.push({
        code: "NORN-FLOW-IMPORT",
        start: { line: start.line + 1, column: start.character + 1 },
        end: { line: end.line + 1, column: end.character + 1 },
        construct: specifier.getText(root),
        message: `imported module fails the profile: ${found} — ${this.profile.diagnostics["NORN-FLOW-IMPORT"]?.fix ?? ""} [${this.profile.profile}]`,
      })
    }
    return out
  }

  /**
   * Resolve the profile identity of each top-level `const x = <expr>` initializer
   * in `file` (used by the export-path verification over the profile probes).
   */
  resolveInitializers(file: string): Array<{ readonly name: string; readonly id: string | undefined }> {
    const absolute = resolvePath(file)
    const program = ts.createProgram({
      rootNames: [absolute, INTRINSICS_PATH],
      options: COMPILER_OPTIONS,
      host: createHost(),
    })
    const sf = program.getSourceFile(absolute)
    if (!sf) throw new Error(`not in program: ${file}`)
    const walk = new FileWalk(this, program, program.getTypeChecker(), sf)
    const out: Array<{ name: string; id: string | undefined }> = []
    for (const st of sf.statements) {
      if (!ts.isVariableStatement(st)) continue
      for (const d of st.declarationList.declarations) {
        const init = d.initializer
        if (!init || !ts.isIdentifier(d.name)) continue
        const node = ts.isPropertyAccessExpression(init) ? init.name : init
        out.push({ name: d.name.text, id: walk.identity(node) })
      }
    }
    return out
  }

  admittedEntry(id: string): AdmittedSymbol | undefined {
    return this.admitted.get(id)
  }

  /** Shared resolved identity for syntax lowering; uses the checker's pinned provenance rules. */
  identity(program: ts.Program, node: ts.Node): string | undefined {
    return new FileWalk(this, program, program.getTypeChecker(), node.getSourceFile()).identity(node)
  }

  /** Stable code for a profile-module symbol that is not admitted. */
  codeFor(id: string): string {
    const explicit = this.rejected.get(id)
    if (explicit) return explicit
    const module = id.slice(0, id.indexOf("#"))
    for (const m of this.profile.rejectedModules) {
      if (m.module.endsWith("/") ? module.startsWith(m.module) : module === m.module) return m.code
    }
    return this.profile.defaultCode
  }

  spelledLikeAdmitted(text: string, bare: boolean): boolean {
    return bare ? this.bareSpellings.has(text) : this.spellings.has(text)
  }
}

class FileWalk {
  private readonly diagnostics: Diagnostic[] = []
  private readonly uses: SymbolUse[] = []
  private readonly jsonData = new Map<ts.VariableDeclaration, boolean>()
  private readonly boundedJsonData = new Set<ts.VariableDeclaration>()
  private jsonDataDepthExceeded = false
  private cancellationToken: ts.CancellationToken | undefined

  constructor(
    private readonly owner: ProfileChecker,
    private readonly program: ts.Program,
    private readonly checker: ts.TypeChecker,
    private readonly sf: ts.SourceFile,
  ) {}

  run(cancellationToken?: ts.CancellationToken): FileReport {
    this.cancellationToken = cancellationToken
    for (const d of ts.getPreEmitDiagnostics(this.program, this.sf, cancellationToken)) {
      if (d.category !== ts.DiagnosticCategory.Error || d.start === undefined) continue
      this.emitSpan(
        d.start,
        d.start + (d.length ?? 0),
        "NORN-FLOW-TYPE",
        ts.flattenDiagnosticMessageText(d.messageText, " "),
      )
    }
    for (const statement of this.sf.statements) this.topLevel(statement)
    const byPos = (a: { start: Position }, b: { start: Position }) =>
      a.start.line - b.start.line || a.start.column - b.start.column
    this.diagnostics.sort((a, b) => byPos(a, b) || a.code.localeCompare(b.code))
    const at = (u: SymbolUse) => u.at.split(":").map(Number) as [number, number]
    this.uses.sort((a, b) => at(a)[0] - at(b)[0] || at(a)[1] - at(b)[1])
    return {
      file: relative(TOOL_ROOT, this.sf.fileName).split(sep).join("/"),
      profile: this.owner.profile.profile,
      diagnostics: this.diagnostics,
      uses: this.uses,
    }
  }

  // ---------------------------------------------------------------- reporting

  private pos(offset: number): Position {
    const lc = this.sf.getLineAndCharacterOfPosition(offset)
    return { line: lc.line + 1, column: lc.character + 1 }
  }

  private emitSpan(start: number, end: number, code: string, detail: string): void {
    const text = this.sf.text.slice(start, end).replace(/\s+/g, " ").trim()
    const construct = text.length > 80 ? `${text.slice(0, 77)}...` : text
    const summary = this.owner.profile.diagnostics[code]
    this.diagnostics.push({
      code,
      start: this.pos(start),
      end: this.pos(end),
      construct,
      message: `${detail}${summary ? ` — ${summary.fix}` : ""} [${this.owner.profile.profile}]`,
    })
  }

  private report(node: ts.Node, code: string, detail: string): void {
    this.emitSpan(node.getStart(this.sf), node.getEnd(), code, detail)
  }

  private recordUse(node: ts.Node, entry: AdmittedSymbol): void {
    const p = this.pos(node.getStart(this.sf))
    this.uses.push({ at: `${p.line}:${p.column}`, symbol: entry.symbol, row: entry.row })
  }

  // --------------------------------------------------------------- resolution

  identity(node: ts.Node): string | undefined {
    return this.resolve(node).id
  }

  private resolve(node: ts.Node): Resolved {
    // `{ x }` names the property; the value it reads is the binding `x`.
    const direct =
      ts.isShorthandPropertyAssignment(node.parent) && node.parent.name === node
        ? this.checker.getShorthandAssignmentValueSymbol(node.parent)
        : this.checker.getSymbolAtLocation(node)
    if (!direct) return { namespace: false }
    const target = direct.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(direct) : direct
    const declaration = target.declarations?.[0]
    const namespace = (target.flags & (ts.SymbolFlags.ValueModule | ts.SymbolFlags.NamespaceModule)) !== 0
    if (!declaration) return { namespace, target }
    const file = declaration.getSourceFile().fileName
    const fqn = this.checker.getFullyQualifiedName(target)
    // Exported declarations print as `"<file>".Name`; locally declared symbols
    // exported under another name (`export { ArraySchema as Array }`) print bare.
    const quoted = /^"[^"]*"(?:\.(.*))?$/.exec(fqn)
    const qualified = quoted ? (quoted[1] ?? "") : fqn
    if (file.startsWith(EFFECT_DIST)) {
      const module = `effect/${file.slice(EFFECT_DIST.length).replace(/\.d\.ts$/, "").split(sep).join("/")}`
      return { id: `${module}#${qualified}`, namespace, target, declaration }
    }
    if (resolvePath(file) === INTRINSICS_PATH) {
      return { id: `${this.owner.profile.intrinsicsModule}#${qualified}`, namespace, target, declaration }
    }
    return { namespace, target, declaration }
  }

  private isDefaultLib(declaration: ts.Declaration | undefined): boolean {
    return declaration !== undefined && this.program.isSourceFileDefaultLibrary(declaration.getSourceFile())
  }

  private where(declaration: ts.Declaration | undefined): string {
    if (!declaration) return "an unknown declaration"
    const file = declaration.getSourceFile()
    const lc = file.getLineAndCharacterOfPosition(declaration.getStart(file))
    return `${relative(TOOL_ROOT, file.fileName).split(sep).join("/")}:${lc.line + 1}`
  }

  private rootIdentifier(expr: ts.Expression): ts.Identifier | undefined {
    let e: ts.Expression = expr
    while (ts.isPropertyAccessExpression(e) || ts.isElementAccessExpression(e) || ts.isCallExpression(e)) {
      e = e.expression
    }
    return ts.isIdentifier(e) ? e : undefined
  }

  private isAmbient(expr: ts.Expression): boolean {
    const root = this.rootIdentifier(expr)
    if (!root) return false
    const resolved = this.resolve(root)
    if (!this.isDefaultLib(resolved.declaration)) return false
    return AMBIENT_ROOTS.has(root.text) || expr.getText(this.sf) === "Math.random"
  }

  private calleeId(call: ts.CallExpression): string | undefined {
    const callee = call.expression
    const name = ts.isPropertyAccessExpression(callee) ? callee.name : ts.isIdentifier(callee) ? callee : undefined
    return name ? this.resolve(name).id : undefined
  }

  // ------------------------------------------------------------------- types

  private typeOf(node: ts.Node): ts.Type {
    return this.checker.getTypeAtLocation(node)
  }

  private every(type: ts.Type, pred: (t: ts.Type) => boolean): boolean {
    return type.isUnion() ? type.types.every(pred) : pred(type)
  }

  private isBoolean(node: ts.Node): boolean {
    return this.every(this.typeOf(node), (t) => (t.flags & ts.TypeFlags.BooleanLike) !== 0)
  }

  private isNumber(node: ts.Node): boolean {
    return this.every(this.typeOf(node), (t) => (t.flags & ts.TypeFlags.NumberLike) !== 0)
  }

  private isScalar(node: ts.Node): boolean {
    const scalar =
      ts.TypeFlags.StringLike |
      ts.TypeFlags.NumberLike |
      ts.TypeFlags.BooleanLike |
      ts.TypeFlags.EnumLike |
      ts.TypeFlags.Null
    return this.every(this.typeOf(node), (t) => (t.flags & scalar) !== 0)
  }

  // --------------------------------------------------------------- top level

  private topLevel(s: ts.Statement): void {
    if (ts.isImportDeclaration(s) || ts.isExportDeclaration(s)) return this.moduleImport(s)
    if (ts.isInterfaceDeclaration(s) || ts.isTypeAliasDeclaration(s)) return
    if (ts.isVariableStatement(s)) return this.variableStatement(s, TOP)
    if (ts.isClassDeclaration(s)) return this.classDeclaration(s)
    if (ts.isExportAssignment(s)) return this.expr(s.expression, TOP)
    if (ts.isFunctionDeclaration(s)) {
      return this.report(s, "NORN-FLOW-UNSUPPORTED-SYNTAX", "helper function declarations are not visible to the compiler")
    }
    if (ts.isExpressionStatement(s)) {
      const before = this.diagnostics.length
      this.expr(s.expression, TOP)
      if (this.diagnostics.length === before) {
        this.report(s, "NORN-FLOW-UNSUPPORTED-SYNTAX", "top-level expression statements are side effects")
      }
      return
    }
    if (!this.controlStatement(s)) {
      this.report(s, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${ts.SyntaxKind[s.kind]} is outside the definition grammar`)
    }
  }

  /**
   * Import allowlist (profile `allowedImports`): the pinned `effect` package and its subpaths,
   * the Norn intrinsics module, and relative TypeScript source modules (which the closure check
   * then requires to pass the profile). Side-effect-only imports are rejected: a definition
   * module runs nothing on import.
   */
  private moduleImport(s: ts.ImportDeclaration | ts.ExportDeclaration): void {
    const specifier = s.moduleSpecifier
    if (!specifier) return // `export { local }` without `from`
    if (!ts.isStringLiteral(specifier)) return this.report(specifier, "NORN-FLOW-MODULE", "module specifiers must be string literals")
    if (ts.isImportDeclaration(s) && !s.importClause) {
      return this.report(s, "NORN-FLOW-MODULE", `side-effect-only import of "${specifier.text}" is not admitted`)
    }
    const name = specifier.text
    const allowed = this.owner.profile.allowedImports
    const listed =
      name === this.owner.profile.intrinsicsModule ||
      allowed.packages.some((p) => (p.endsWith("/*") ? name.startsWith(p.slice(0, -1)) : name === p))
    const target = this.checker.getSymbolAtLocation(specifier)?.declarations?.[0]?.getSourceFile()
    if (listed) {
      // Listed packages must still resolve to the pinned copy / the intrinsics declaration.
      const file = target ? resolvePath(target.fileName) : undefined
      if (file === undefined) return // unresolved: already a NORN-FLOW-TYPE error
      if (name === this.owner.profile.intrinsicsModule ? file === INTRINSICS_PATH : file.startsWith(EFFECT_DIST)) return
      return this.report(specifier, "NORN-FLOW-MODULE", `"${name}" resolves to ${relative(TOOL_ROOT, file).split(sep).join("/")}, not the pinned module`)
    }
    if (name.startsWith("./") || name.startsWith("../")) {
      if (!target) return // unresolved: already a NORN-FLOW-TYPE error
      if (
        target.isDeclarationFile ||
        this.program.isSourceFileFromExternalLibrary(target) ||
        target.fileName.split(/[\\/]/).includes("node_modules") ||
        !/\.(?:ts|mts)$/.test(target.fileName)
      ) {
        return this.report(specifier, "NORN-FLOW-MODULE", `"${name}" is not a TypeScript source module the profile can check`)
      }
      return
    }
    this.report(specifier, "NORN-FLOW-MODULE", `"${name}" is outside the import allowlist (effect, effect/*, ${this.owner.profile.intrinsicsModule}, local modules)`)
  }

  /** Statements that carry a specific code wherever they appear. Returns true if handled. */
  private controlStatement(s: ts.Statement): boolean {
    if (ts.isTryStatement(s)) {
      this.report(s, "NORN-FLOW-TYPED-FAILURE-CATCH", "try/catch/finally cannot observe typed Effect failures")
      return true
    }
    if (ts.isThrowStatement(s)) {
      this.report(s, "NORN-FLOW-TYPED-FAILURE-CATCH", "throw is not a typed failure")
      return true
    }
    if (ts.isIterationStatement(s, false)) {
      this.report(s, "NORN-FLOW-LOOP", "general loops are not admitted in v1")
      return true
    }
    if (ts.isContinueStatement(s)) {
      this.report(s, "NORN-FLOW-LOOP", "loop control is not admitted")
      return true
    }
    return false
  }

  private variableStatement(s: ts.VariableStatement, ctx: Ctx): void {
    const list = s.declarationList
    if ((list.flags & ts.NodeFlags.Const) === 0) {
      return this.report(list, "NORN-FLOW-CAPTURE", "only const bindings are admitted (no let/var)")
    }
    for (const d of list.declarations) {
      if (!ts.isIdentifier(d.name)) {
        this.report(d.name, "NORN-FLOW-UNSUPPORTED-SYNTAX", "destructuring bindings are not admitted")
        continue
      }
      if (!d.initializer) {
        this.report(d, "NORN-FLOW-UNSUPPORTED-SYNTAX", "bindings need an initializer (ambient `declare` values have nothing to extract)")
        continue
      }
      if (ctx.flow && this.isEffectValue(d.initializer)) {
        this.report(d.initializer, "NORN-FLOW-UNSUPPORTED-SYNTAX", "effect values may not be stored; bind the result with yield*")
        continue
      }
      this.expr(d.initializer, { ...ctx, effect: false })
    }
  }

  /** An un-yielded call producing an effect (a flow-context admitted call or a .pipe chain over one). */
  private isEffectValue(e: ts.Expression): boolean {
    const inner = this.unwrap(e)
    if (!ts.isCallExpression(inner)) return false
    const id = this.flowRoot(inner)
    const entry = id ? this.owner.admittedEntry(id) : undefined
    return entry?.context === "flow"
  }

  private classDeclaration(s: ts.ClassDeclaration): void {
    const heritage = s.heritageClauses?.[0]?.types[0]
    if (!heritage || s.heritageClauses?.length !== 1 || s.heritageClauses[0]?.types.length !== 1) {
      return this.report(s, "NORN-FLOW-UNSUPPORTED-SYNTAX", "classes are admitted only as Data.TaggedError / Schema.Class declarations")
    }
    const before = this.diagnostics.length
    const ok = this.heritage(heritage.expression)
    if (this.diagnostics.length > before) return
    if (!ok) {
      return this.report(heritage, "NORN-FLOW-UNSUPPORTED-SYNTAX", "class must extend Data.TaggedError(tag) or Schema.Class<Self>(id)(fields)")
    }
    if (s.members.length > 0) {
      this.report(s.members[0]!, "NORN-FLOW-UNSUPPORTED-SYNTAX", "tagged error / schema classes carry no members, methods, or getters")
    }
  }

  /** Validate an `extends` expression. Returns true if it is an admitted class-heritage form. */
  private heritage(expr: ts.Expression): boolean {
    if (!ts.isCallExpression(expr)) {
      this.expr(expr, TOP)
      return false
    }
    // Schema.Class<Self>("Id")({ ...fields })
    const inner = ts.isCallExpression(expr.expression) ? expr.expression : expr
    const id = this.calleeId(inner)
    const entry = id ? this.owner.admittedEntry(id) : undefined
    if (!entry || entry.use !== "class-heritage") {
      this.expr(expr, TOP)
      return false
    }
    this.recordUse(inner.expression, entry)
    const tag = inner.arguments[0]
    if (inner.arguments.length !== 1 || !tag || !ts.isStringLiteral(tag)) {
      this.report(inner, "NORN-FLOW-LITERAL", "the tag / identifier must be one string literal")
    }
    if (inner !== expr) for (const a of expr.arguments) this.expr(a, TOP)
    else if (id === "effect/Schema#Class") {
      this.report(expr, "NORN-FLOW-UNSUPPORTED-SYNTAX", "Schema.Class requires its field struct: Schema.Class<Self>(id)(fields)")
    }
    return true
  }

  // --------------------------------------------------------------- statements

  private block(statements: ts.NodeArray<ts.Statement>, ctx: Ctx): void {
    for (const s of statements) this.statement(s, ctx, false)
  }

  private statement(s: ts.Statement, ctx: Ctx, lastInClause: boolean): void {
    if (this.controlStatement(s)) return
    if (ts.isVariableStatement(s)) return this.variableStatement(s, ctx)
    if (ts.isBlock(s)) return this.block(s.statements, ctx)
    if (ts.isEmptyStatement(s)) return
    if (ts.isReturnStatement(s)) {
      if (!s.expression) {
        if (ctx.ret === "flow-result" || ctx.ret === "map-result") {
          this.report(s, "NORN-FLOW-RESULT", "a bare `return` yields undefined; return an explicit JSON result")
        }
        return
      }
      if (ctx.generator && this.isEffectValue(s.expression)) {
        return this.report(s.expression, "NORN-FLOW-UNSUPPORTED-SYNTAX", "a flow result is a value; run the effect with `return yield* ...`")
      }
      this.expr(s.expression, { ...ctx, effect: ctx.ret === "effect" })
      return
    }
    if (ts.isBreakStatement(s)) {
      if (!lastInClause || s.label) this.report(s, "NORN-FLOW-LOOP", "break is admitted only to end a switch clause")
      return
    }
    if (ts.isExpressionStatement(s)) {
      const before = this.diagnostics.length
      // Walked as an effect position so a discarded effect reports as discarded, not as data.
      this.expr(s.expression, { ...ctx, effect: true })
      const e = s.expression
      if (this.diagnostics.length === before && !(ts.isYieldExpression(e) && e.asteriskToken)) {
        this.report(s, "NORN-FLOW-UNSUPPORTED-SYNTAX", "expression statement discards its value; yield* the effect")
      }
      return
    }
    if (ts.isIfStatement(s)) {
      this.condition(s.expression, ctx)
      this.statement(s.thenStatement, ctx, false)
      if (s.elseStatement) this.statement(s.elseStatement, ctx, false)
      return
    }
    if (ts.isSwitchStatement(s)) return this.switchStatement(s, ctx)
    this.report(s, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${ts.SyntaxKind[s.kind]} is outside the flow grammar`)
  }

  private condition(cond: ts.Expression, ctx: Ctx): void {
    const before = this.diagnostics.length
    this.expr(cond, { ...ctx, effect: false })
    if (this.diagnostics.length === before && !this.isBoolean(cond)) {
      this.report(cond, "NORN-FLOW-EXPRESSION", `condition has type ${this.checker.typeToString(this.typeOf(cond))}, not boolean (no truthiness)`)
    }
  }

  private switchStatement(s: ts.SwitchStatement, ctx: Ctx): void {
    const discType = this.typeOf(s.expression)
    const members = discType.isUnion() ? discType.types : [discType]
    if (!members.every((t) => t.isLiteral() || (t.flags & (ts.TypeFlags.BooleanLiteral | ts.TypeFlags.EnumLiteral)) !== 0)) {
      this.report(s.expression, "NORN-FLOW-ROUTE", `discriminant has type ${this.checker.typeToString(discType)}, not a literal union`)
      return
    }
    this.expr(s.expression, { ...ctx, effect: false })
    const covered = new Set<ts.Type>()
    let hasDefault = false
    const clauses = s.caseBlock.clauses
    for (const clause of clauses) {
      if (ts.isDefaultClause(clause)) hasDefault = true
      else if (
        !ts.isStringLiteral(clause.expression) &&
        !ts.isNumericLiteral(clause.expression) &&
        clause.expression.kind !== ts.SyntaxKind.TrueKeyword &&
        clause.expression.kind !== ts.SyntaxKind.FalseKeyword
      ) {
        this.report(clause.expression, "NORN-FLOW-ROUTE", "case labels must be literals")
      } else covered.add(this.typeOf(clause.expression))
      const last = clause.statements[clause.statements.length - 1]
      if (!last || !(ts.isReturnStatement(last) || ts.isBreakStatement(last))) {
        this.report(clause, "NORN-FLOW-ROUTE", "clause falls through; end it with return or break")
      }
      clause.statements.forEach((st, i) => { this.statement(st, ctx, i === clause.statements.length - 1) })
    }
    if (!hasDefault) {
      const missing = members.filter((m) => !covered.has(m))
      if (missing.length > 0) {
        this.report(s.expression, "NORN-FLOW-ROUTE", `switch is not exhaustive; missing ${missing.map((m) => this.checker.typeToString(m)).join(", ")}`)
      }
    }
  }

  // -------------------------------------------------------------- expressions

  private expr(e: ts.Expression, ctx: Ctx): void {
    // Only calls (and transparent wrappers) can occupy an effect position; the operands of
    // every other expression are data.
    if (
      ctx.effect &&
      !ts.isCallExpression(e) &&
      !ts.isParenthesizedExpression(e) &&
      !ts.isSatisfiesExpression(e) &&
      !ts.isAsExpression(e) &&
      !ts.isYieldExpression(e)
    ) {
      ctx = { ...ctx, effect: false }
    }
    switch (e.kind) {
      case ts.SyntaxKind.NumericLiteral:
        if (!Number.isFinite(Number((e as ts.NumericLiteral).text))) {
          return this.report(e, "NORN-FLOW-EXPRESSION", "non-finite numeric literal (JSON would serialize it as null)")
        }
        return
      case ts.SyntaxKind.StringLiteral:
      case ts.SyntaxKind.NoSubstitutionTemplateLiteral:
      case ts.SyntaxKind.TrueKeyword:
      case ts.SyntaxKind.FalseKeyword:
      case ts.SyntaxKind.NullKeyword:
        return
      case ts.SyntaxKind.Identifier:
        return this.identifier(e as ts.Identifier, ctx)
      case ts.SyntaxKind.PropertyAccessExpression:
        return this.propertyAccess(e as ts.PropertyAccessExpression, ctx)
      case ts.SyntaxKind.ElementAccessExpression: {
        const ea = e as ts.ElementAccessExpression
        if (ea.questionDotToken || !(ts.isStringLiteral(ea.argumentExpression) || ts.isNumericLiteral(ea.argumentExpression))) {
          return this.report(e, "NORN-FLOW-EXPRESSION", "dynamic indexing is not admitted; use a declared property path")
        }
        return this.expr(ea.expression, ctx)
      }
      case ts.SyntaxKind.CallExpression:
        return this.call(e as ts.CallExpression, ctx)
      case ts.SyntaxKind.NewExpression:
        return this.newExpression(e as ts.NewExpression, ctx, false)
      case ts.SyntaxKind.YieldExpression: {
        const y = e as ts.YieldExpression
        if (!y.asteriskToken || !ctx.generator || !y.expression) {
          return this.report(e, "NORN-FLOW-UNSUPPORTED-SYNTAX", "only yield* of an admitted effect inside Effect.gen")
        }
        return this.expr(y.expression, { ...ctx, effect: true })
      }
      case ts.SyntaxKind.ParenthesizedExpression:
        return this.expr((e as ts.ParenthesizedExpression).expression, ctx)
      case ts.SyntaxKind.SatisfiesExpression:
        return this.expr((e as ts.SatisfiesExpression).expression, ctx)
      case ts.SyntaxKind.AsExpression: {
        const a = e as ts.AsExpression
        if (ts.isConstTypeReference(a.type)) return this.expr(a.expression, ctx)
        return this.report(e, "NORN-FLOW-EXPRESSION", "a type assertion is not evidence about the runtime value")
      }
      case ts.SyntaxKind.TypeAssertionExpression:
      case ts.SyntaxKind.NonNullExpression:
        return this.report(e, "NORN-FLOW-EXPRESSION", "a type assertion is not evidence about the runtime value")
      case ts.SyntaxKind.ArrayLiteralExpression:
        for (const el of (e as ts.ArrayLiteralExpression).elements) {
          if (ts.isSpreadElement(el)) this.report(el, "NORN-FLOW-EXPRESSION", "spread is not admitted")
          else this.expr(el, ctx)
        }
        return
      case ts.SyntaxKind.ObjectLiteralExpression:
        return this.objectLiteral(e as ts.ObjectLiteralExpression, ctx)
      case ts.SyntaxKind.PrefixUnaryExpression:
        return this.prefixUnary(e as ts.PrefixUnaryExpression, ctx)
      case ts.SyntaxKind.PostfixUnaryExpression:
        return this.report(e, "NORN-FLOW-CAPTURE", "mutation is not admitted")
      case ts.SyntaxKind.BinaryExpression:
        return this.binary(e as ts.BinaryExpression, ctx)
      case ts.SyntaxKind.TemplateExpression:
        return this.report(e, "NORN-FLOW-EXPRESSION", "runtime template interpolation is not admitted in v1")
      case ts.SyntaxKind.ConditionalExpression: {
        const c = e as ts.ConditionalExpression
        this.expr(c.condition, ctx)
        if (!this.isBoolean(c.condition)) this.report(c.condition, "NORN-FLOW-EXPRESSION", "conditional needs a boolean; compare the value explicitly")
        this.expr(c.whenTrue, ctx)
        this.expr(c.whenFalse, ctx)
        return
      }
      case ts.SyntaxKind.TypeOfExpression:
      case ts.SyntaxKind.VoidExpression:
      case ts.SyntaxKind.DeleteExpression:
        return this.report(e, "NORN-FLOW-EXPRESSION", "reflection/coercion operators are not admitted")
      case ts.SyntaxKind.AwaitExpression:
        return this.report(e, "NORN-FLOW-UNSUPPORTED-CALL", "await/Promises are not admitted; use Norn.perform")
      case ts.SyntaxKind.TaggedTemplateExpression:
        return this.report(e, "NORN-FLOW-UNSUPPORTED-CALL", "tagged templates are calls the compiler cannot see")
      case ts.SyntaxKind.ArrowFunction:
      case ts.SyntaxKind.FunctionExpression:
        return this.report(e, "NORN-FLOW-CAPTURE", "arbitrary closures are not admitted; pass callbacks inline to an admitted combinator")
      default:
        return this.report(e, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${ts.SyntaxKind[e.kind]} is outside the expression grammar`)
    }
  }

  private admittedValue(node: ts.Node, r: Resolved, ctx: Ctx): void {
    const id = r.id!
    const entry = this.owner.admittedEntry(id)
    if (!entry) return this.report(node, this.owner.codeFor(id), `${id} is not admitted by the profile`)
    if (entry.use !== "value") {
      return this.report(node, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${id} must be called inline, not referenced as a value`)
    }
    if (this.contextOk(node, entry, ctx)) this.recordUse(node, entry)
  }

  private identifier(id: ts.Identifier, ctx: Ctx): void {
    if (id.text === "undefined") return this.report(id, "NORN-FLOW-EXPRESSION", "undefined is not a JSON value; use null")
    const r = this.resolve(id)
    if (!r.target) return // unresolved names are already NORN-FLOW-TYPE errors
    if (r.id !== undefined) {
      if (r.namespace) return this.report(id, "NORN-FLOW-UNSUPPORTED-SYNTAX", "a profile namespace may only qualify an admitted member")
      return this.admittedValue(id, r, ctx)
    }
    const decl = r.declaration
    if (this.isDefaultLib(decl)) {
      if (id.text === "NaN" || id.text === "Infinity") return this.report(id, "NORN-FLOW-EXPRESSION", `${id.text} is not a JSON value`)
      return this.report(id, "NORN-FLOW-AMBIENT", `${id.text} is a host global`)
    }
    if (decl && ts.isVariableDeclaration(decl) && (decl.parent.flags & ts.NodeFlags.Const) === 0) {
      return this.report(id, "NORN-FLOW-CAPTURE", `captures mutable binding declared at ${this.where(decl)}`)
    }
    if (decl && ts.isFunctionDeclaration(decl)) {
      return this.report(id, "NORN-FLOW-CAPTURE", `captures helper function declared at ${this.where(decl)}`)
    }
    if (ctx.flow) this.flowDataRef(id, decl)
  }

  /**
   * Value provenance inside a flow: a reference to a top-level binding is admitted only if
   * that binding is a JSON data const. Definition markers (Schema values, Norn.flow / Norn.fn
   * values, Tool/Toolkit values, classes) are declarations, not flow data.
   */
  private flowDataRef(node: ts.Node, decl: ts.Declaration | undefined): void {
    if (!decl) return
    if (ts.isClassDeclaration(decl)) {
      return this.report(node, "NORN-FLOW-VALUE", `${node.getText(this.sf)} is a class declared at ${this.where(decl)}, not flow data`)
    }
    if (this.isTopLevelConst(decl)) {
      this.jsonDataDepthExceeded = false
      if (!this.isJsonDataDecl(decl, new Set())) {
        return this.report(node, this.jsonDataDepthExceeded ? "NORN-FLOW-UNBOUNDED" : "NORN-FLOW-VALUE", this.jsonDataDepthExceeded ? "data provenance exceeds 128 referenced declarations" : `${node.getText(this.sf)} is bound at ${this.where(decl)} to a definition value, not a JSON data const`)
      }
    }
  }

  private isTopLevelConst(decl: ts.Declaration): decl is ts.VariableDeclaration {
    return (
      ts.isVariableDeclaration(decl) &&
      (decl.parent.flags & ts.NodeFlags.Const) !== 0 &&
      ts.isVariableStatement(decl.parent.parent) &&
      ts.isSourceFile(decl.parent.parent.parent)
    )
  }

  /** `seen` is the active recursion path (a cycle guard), not a visited set: shared references are data. */
  private isJsonDataDecl(decl: ts.VariableDeclaration, seen: Set<ts.Declaration>): boolean {
    this.cancellationToken?.throwIfCancellationRequested()
    if (seen.has(decl)) return false
    if (seen.size >= 128) { this.jsonDataDepthExceeded = true; return false }
    const known = this.jsonData.get(decl)
    if (known !== undefined) {
      if (this.boundedJsonData.has(decl)) this.jsonDataDepthExceeded = true
      return known
    }
    seen.add(decl)
    try {
      const result = decl.initializer !== undefined && this.isJsonData(decl.initializer, seen)
      this.jsonData.set(decl, result)
      if (!result && this.jsonDataDepthExceeded) this.boundedJsonData.add(decl)
      return result
    } finally {
      seen.delete(decl)
    }
  }

  /** Initializer grammar of a JSON data const: literals, arrays/objects of data, references to data consts. */
  private isJsonData(e: ts.Expression, seen: Set<ts.Declaration>): boolean {
    this.cancellationToken?.throwIfCancellationRequested()
    const ref = (decl: ts.Declaration | undefined) =>
      decl !== undefined && this.isTopLevelConst(decl) && this.isJsonDataDecl(decl, seen)
    if (ts.isParenthesizedExpression(e) || ts.isSatisfiesExpression(e)) return this.isJsonData(e.expression, seen)
    if (ts.isAsExpression(e)) return ts.isConstTypeReference(e.type) && this.isJsonData(e.expression, seen)
    if (ts.isNumericLiteral(e)) return Number.isFinite(Number(e.text))
    if (ts.isPrefixUnaryExpression(e)) {
      return e.operator === ts.SyntaxKind.MinusToken && ts.isNumericLiteral(e.operand) && this.isJsonData(e.operand, seen)
    }
    if (ts.isStringLiteral(e) || ts.isNoSubstitutionTemplateLiteral(e)) return true
    if (
      e.kind === ts.SyntaxKind.TrueKeyword ||
      e.kind === ts.SyntaxKind.FalseKeyword ||
      e.kind === ts.SyntaxKind.NullKeyword
    ) {
      return true
    }
    if (ts.isArrayLiteralExpression(e)) return e.elements.every((el) => !ts.isSpreadElement(el) && this.isJsonData(el, seen))
    if (ts.isObjectLiteralExpression(e)) {
      return e.properties.every((p) =>
        ts.isPropertyAssignment(p)
          ? !ts.isComputedPropertyName(p.name) && this.isJsonData(p.initializer, seen)
          : ts.isShorthandPropertyAssignment(p) && ref(this.resolve(p.name).declaration),
      )
    }
    if (ts.isIdentifier(e)) return ref(this.resolve(e).declaration)
    if (ts.isPropertyAccessExpression(e)) {
      const decl = this.resolve(e.name).declaration
      if (decl && this.isTopLevelConst(decl)) return ref(decl) // namespace-qualified const
      return !e.questionDotToken && this.isJsonData(e.expression, seen)
    }
    return false
  }

  private propertyAccess(pa: ts.PropertyAccessExpression, ctx: Ctx): void {
    if (pa.questionDotToken) return this.report(pa, "NORN-FLOW-EXPRESSION", "optional chaining is not admitted; null policy must be explicit")
    const r = this.resolve(pa.name)
    if (r.id !== undefined) return this.admittedValue(pa, r, ctx)
    const text = pa.getText(this.sf)
    if (this.owner.spelledLikeAdmitted(text, false)) {
      return this.report(pa, "NORN-FLOW-SYMBOL", `\`${text}\` resolves to ${this.where(r.declaration)}, not the admitted profile symbol`)
    }
    if (pa.name.text === "length" && this.every(this.typeOf(pa.expression), (t) => !!(t.flags & ts.TypeFlags.StringLike))) return this.report(pa, "NORN-FLOW-EXPRESSION", "string.length counts UTF-16 units; use Norn.byteLength(text) for UTF-8 bytes")
    const before = this.diagnostics.length
    this.expr(pa.expression, ctx)
    if (this.diagnostics.length === before && this.isDefaultLib(r.declaration) && pa.name.text !== "length") {
      this.report(pa, "NORN-FLOW-EXPRESSION", `\`.${pa.name.text}\` is a host property, not a declared data path`)
    }
    // `ns.name` through a namespace import of a local module.
    if (this.diagnostics.length === before && ctx.flow && r.declaration && (ts.isClassDeclaration(r.declaration) || this.isTopLevelConst(r.declaration))) {
      this.flowDataRef(pa, r.declaration)
    }
  }

  private objectLiteral(o: ts.ObjectLiteralExpression, ctx: Ctx): void {
    for (const p of o.properties) {
      if (ts.isPropertyAssignment(p)) {
        if (ts.isComputedPropertyName(p.name)) this.report(p.name, "NORN-FLOW-EXPRESSION", "computed keys are not admitted")
        else this.expr(p.initializer, ctx)
      } else if (ts.isShorthandPropertyAssignment(p)) {
        this.identifier(p.name, ctx)
      } else if (ts.isSpreadAssignment(p)) {
        this.report(p, "NORN-FLOW-EXPRESSION", "spread is not admitted")
      } else {
        this.report(p, "NORN-FLOW-UNSUPPORTED-SYNTAX", "methods and accessors are not admitted in data")
      }
    }
  }

  private prefixUnary(u: ts.PrefixUnaryExpression, ctx: Ctx): void {
    switch (u.operator) {
      case ts.SyntaxKind.ExclamationToken: {
        const before = this.diagnostics.length
        this.expr(u.operand, ctx)
        if (this.diagnostics.length === before && !this.isBoolean(u.operand)) {
          this.report(u, "NORN-FLOW-EXPRESSION", "! applies only to booleans (no truthiness)")
        }
        return
      }
      case ts.SyntaxKind.MinusToken:
        // A negated literal is a literal: the finite check applies to it too (`-1e400`).
        if (ts.isNumericLiteral(u.operand)) return this.expr(u.operand, ctx)
        this.expr(u.operand, ctx)
        if (!this.isNumber(u.operand)) this.report(u, "NORN-FLOW-EXPRESSION", "unary - expects a number")
        return
      case ts.SyntaxKind.PlusPlusToken:
      case ts.SyntaxKind.MinusMinusToken:
        return this.report(u, "NORN-FLOW-CAPTURE", "mutation is not admitted")
      default:
        return this.report(u, "NORN-FLOW-EXPRESSION", "coercion/arithmetic operators are not admitted")
    }
  }

  private binary(b: ts.BinaryExpression, ctx: Ctx): void {
    const op = b.operatorToken.kind
    if (op >= ts.SyntaxKind.FirstAssignment && op <= ts.SyntaxKind.LastAssignment) {
      return this.report(b, "NORN-FLOW-CAPTURE", "assignment/mutation is not admitted")
    }
    const operands = () => {
      const before = this.diagnostics.length
      this.expr(b.left, ctx)
      this.expr(b.right, ctx)
      return this.diagnostics.length === before
    }
    switch (op) {
      case ts.SyntaxKind.EqualsEqualsEqualsToken:
      case ts.SyntaxKind.ExclamationEqualsEqualsToken:
        if (operands() && !(this.isScalar(b.left) && this.isScalar(b.right))) {
          this.report(b, "NORN-FLOW-EXPRESSION", "===/!== compares scalars only (no object equality)")
        }
        return
      case ts.SyntaxKind.AmpersandAmpersandToken:
      case ts.SyntaxKind.BarBarToken:
        if (operands() && !(this.isBoolean(b.left) && this.isBoolean(b.right))) {
          this.report(b, "NORN-FLOW-EXPRESSION", "&&/|| take strict booleans (no truthiness)")
        }
        return
      case ts.SyntaxKind.LessThanToken:
      case ts.SyntaxKind.LessThanEqualsToken:
      case ts.SyntaxKind.GreaterThanToken:
      case ts.SyntaxKind.GreaterThanEqualsToken:
        if (operands() && !(this.isNumber(b.left) && this.isNumber(b.right))) {
          this.report(b, "NORN-FLOW-EXPRESSION", "ordering comparisons are qualified for numbers only")
        }
        return
      case ts.SyntaxKind.EqualsEqualsToken:
      case ts.SyntaxKind.ExclamationEqualsToken:
        return this.report(b, "NORN-FLOW-EXPRESSION", "loose equality coerces; use ===/!==")
      case ts.SyntaxKind.QuestionQuestionToken:
        operands()
        return
      case ts.SyntaxKind.PlusToken:
      case ts.SyntaxKind.MinusToken:
      case ts.SyntaxKind.AsteriskToken:
      case ts.SyntaxKind.SlashToken:
      case ts.SyntaxKind.PercentToken:
        if (operands() && !(this.isNumber(b.left) && this.isNumber(b.right)) && !(op === ts.SyntaxKind.PlusToken && this.every(this.typeOf(b.left), (t) => !!(t.flags & ts.TypeFlags.StringLike)) && this.every(this.typeOf(b.right), (t) => !!(t.flags & ts.TypeFlags.StringLike)))) {
          this.report(b, "NORN-FLOW-EXPRESSION", "expected two numbers or two strings for +; implicit coercion is not in the Norn profile")
        }
        return
      case ts.SyntaxKind.CommaToken:
        return this.report(b, "NORN-FLOW-UNSUPPORTED-SYNTAX", "comma expressions are not admitted")
      default:
        return this.report(b, "NORN-FLOW-EXPRESSION", `operator ${b.operatorToken.getText(this.sf)} (arithmetic/coercion) is not admitted in v1`)
    }
  }

  private newExpression(n: ts.NewExpression, ctx: Ctx, failArgument: boolean): void {
    const r = this.resolve(n.expression)
    const decl = r.declaration
    if (decl && ts.isClassDeclaration(decl) && this.isTaggedErrorClass(decl)) {
      if (!failArgument) {
        return this.report(n, "NORN-FLOW-UNSUPPORTED-SYNTAX", "tagged errors are constructed only as the argument of Effect.fail")
      }
      for (const a of n.arguments ?? []) this.expr(a, { ...ctx, effect: false })
      return
    }
    if (this.isAmbient(n.expression)) return this.report(n, "NORN-FLOW-AMBIENT", "host clock/IO constructors are not admitted")
    this.report(n, "NORN-FLOW-UNSUPPORTED-CALL", "arbitrary class instances are not admitted; construct Data.TaggedError declarations only")
  }

  private isTaggedErrorClass(decl: ts.ClassDeclaration): boolean {
    const heritage = decl.heritageClauses?.[0]?.types[0]?.expression
    return heritage !== undefined && ts.isCallExpression(heritage) && this.calleeId(heritage) === "effect/Data#TaggedError"
  }

  // -------------------------------------------------------------------- calls

  private contextOk(node: ts.Node, entry: AdmittedSymbol, ctx: Ctx): boolean {
    if (entry.context === "flow" && !ctx.flow) {
      this.report(node, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${entry.symbol} is admitted only inline inside a Norn.flow body (no generator escape)`)
      return false
    }
    if (entry.context === "definition" && ctx.flow) {
      this.report(node, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${entry.symbol} is a top-level definition constructor, not a flow step`)
      return false
    }
    if (entry.context === "retry-policy" && !ctx.retryPolicy) {
      this.report(node, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${entry.symbol} is admitted only inline as an Effect.retry policy`)
      return false
    }
    return true
  }

  private call(call: ts.CallExpression, ctx: Ctx): void {
    const callee = call.expression
    if (callee.kind === ts.SyntaxKind.ImportKeyword) {
      return this.report(call, "NORN-FLOW-UNSUPPORTED-CALL", "dynamic import is not admitted")
    }
    if (call.questionDotToken) return this.report(call, "NORN-FLOW-EXPRESSION", "optional calls are not admitted")
    if (ts.isCallExpression(callee)) {
      // Curried constructors (`Context.Service<...>()("id")`) report through their inner call;
      // the admitted curried forms appear only in class heritage.
      const before = this.diagnostics.length
      this.call(callee, ctx)
      if (this.diagnostics.length === before) {
        this.report(callee, "NORN-FLOW-UNSUPPORTED-SYNTAX", "curried calls are admitted only in class heritage")
      }
      return
    }
    const name = ts.isPropertyAccessExpression(callee) ? callee.name : ts.isIdentifier(callee) ? callee : undefined
    if (!name) return this.report(callee, "NORN-FLOW-UNSUPPORTED-CALL", "callee is not a statically resolved symbol")
    const r = this.resolve(name)
    if (r.id !== undefined) {
      const entry = this.owner.admittedEntry(r.id)
      if (!entry) return this.report(callee, this.owner.codeFor(r.id), `${r.id} is not admitted by the profile`)
      if (entry.use === "method") {
        if (this.effectAsData(call, ctx)) return
        this.recordUse(name, entry)
        const eff = { ...ctx, effect: true }
        if (ts.isPropertyAccessExpression(callee)) this.expr(callee.expression, eff)
        for (const a of call.arguments) this.expr(a, eff)
        return
      }
      if (entry.use !== "call") {
        return this.report(callee, "NORN-FLOW-UNSUPPORTED-SYNTAX", `${r.id} is admitted only as ${entry.use}`)
      }
      if (ctx.pure && entry.context === "flow") {
        return this.report(callee, "NORN-FLOW-UNSUPPORTED-SYNTAX", "Effect.map callbacks are pure value expressions; use Effect.flatMap to sequence an effect")
      }
      if (entry.context === "flow" && ctx.flow && !ctx.effect) {
        return this.report(call, "NORN-FLOW-VALUE", "an effect is not a data value; run it with yield* and use the result")
      }
      if (r.id === "effect/Function#pipe" && this.effectAsData(call, ctx)) return
      if (!this.contextOk(callee, entry, ctx)) return
      this.recordUse(callee, entry)
      return this.admittedCall(r.id, call, ctx)
    }
    if (ts.isPropertyAccessExpression(callee) && r.declaration && this.isDefaultLib(r.declaration)) {
      const receiver = this.typeOf(callee.expression), name = callee.name.text
      const string = this.every(receiver, (t) => !!(t.flags & ts.TypeFlags.StringLike))
      const array = this.checker.isArrayType(receiver) || this.checker.isTupleType(receiver)
      const arity = ["toLowerCase", "toUpperCase", "trim"].includes(name) ? 0 : 1
      if ((string && ["toLowerCase", "toUpperCase", "trim", "includes", "startsWith", "endsWith"].includes(name) || array && name === "includes") && call.arguments.length === arity) {
        this.expr(callee.expression, ctx)
        for (const a of call.arguments) this.expr(a, ctx)
        return
      }
    }
    const text = callee.getText(this.sf)
    if (this.owner.spelledLikeAdmitted(text, ts.isIdentifier(callee))) {
      return this.report(callee, "NORN-FLOW-SYMBOL", `\`${text}\` resolves to ${this.where(r.declaration)}, not the admitted profile symbol`)
    }
    if (this.isAmbient(callee)) return this.report(callee, "NORN-FLOW-AMBIENT", `\`${text}\` reads host time/entropy/IO`)
    this.report(callee, "NORN-FLOW-UNSUPPORTED-CALL", `\`${text}\` is a call the compiler cannot see`)
  }

  private args(call: ts.CallExpression, ctx: Ctx): void {
    for (const a of call.arguments) this.expr(a, ctx)
  }

  /** A pipe chain over an effect in a flow data position. Returns true if reported. */
  private effectAsData(call: ts.CallExpression, ctx: Ctx): boolean {
    if (!ctx.flow || ctx.effect || ctx.pure || !this.isEffectValue(call)) return false
    this.report(call, "NORN-FLOW-VALUE", "an effect is not a data value; run it with yield* and use the result")
    return true
  }

  private requireLiteral(node: ts.Expression | undefined, call: ts.CallExpression, what: string, numeric: boolean): void {
    const ok =
      node !== undefined &&
      (ts.isNumericLiteral(node) || (!numeric && (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node))))
    if (!ok) this.report(node ?? call, "NORN-FLOW-LITERAL", `${what} must be a literal`)
  }

  private callback(fn: ts.Expression | undefined, call: ts.CallExpression, ctx: Ctx, pure = false): void {
    if (!fn || !(ts.isArrowFunction(fn) || ts.isFunctionExpression(fn)) || fn.asteriskToken) {
      return this.report(fn ?? call, "NORN-FLOW-CAPTURE", "the continuation must be an inline callback literal (function-valued continuations are opaque)")
    }
    if (fn.modifiers?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword)) {
      return this.report(fn, "NORN-FLOW-UNSUPPORTED-CALL", "async callbacks are not admitted")
    }
    for (const p of fn.parameters) {
      if (!ts.isIdentifier(p.name) || p.initializer || p.dotDotDotToken) {
        this.report(p, "NORN-FLOW-UNSUPPORTED-SYNTAX", "callback parameters must be plain identifiers")
      }
    }
    const isPure = ctx.pure || pure
    const inner: Ctx = { ...ctx, generator: false, retryPolicy: false, pure: isPure, effect: false, ret: isPure ? "map-result" : "effect" }
    if (!ts.isBlock(fn.body)) return this.expr(fn.body, { ...inner, effect: !isPure })
    this.block(fn.body.statements, inner)
    if (isPure && !this.alwaysReturns(fn.body.statements)) {
      this.report(fn, "NORN-FLOW-RESULT", "a block-bodied Effect.map callback must return a value on every path")
    }
  }

  /** Every path through the statements ends in a `return` (bare returns are reported separately). */
  private alwaysReturns(statements: ReadonlyArray<ts.Statement>): boolean {
    return statements.some((s) => this.terminates(s))
  }

  private terminates(s: ts.Statement): boolean {
    if (ts.isReturnStatement(s) || ts.isThrowStatement(s)) return true
    if (ts.isBlock(s)) return this.alwaysReturns(s.statements)
    if (ts.isIfStatement(s)) return s.elseStatement !== undefined && this.terminates(s.thenStatement) && this.terminates(s.elseStatement)
    if (ts.isSwitchStatement(s)) {
      // Coverage and fall-through are NORN-FLOW-ROUTE; here every clause must end in a return.
      return s.caseBlock.clauses.every((c) => {
        const last = c.statements[c.statements.length - 1]
        return last !== undefined && this.terminates(last)
      })
    }
    return false
  }

  private admittedCall(id: string, call: ts.CallExpression, callCtx: Ctx): void {
    const a = call.arguments
    // Arguments are data unless the combinator takes an effect there (`eff`).
    const ctx: Ctx = { ...callCtx, effect: false }
    const eff: Ctx = { ...callCtx, effect: true }
    switch (id) {
      case "@norn/authoring#Norn.evaluate": {
        if (a[1]) this.expr(a[1], ctx)
        return
      }
      case "@norn/authoring#Norn.expression": {
        if (a[0]) this.expr(a[0], ctx)
        return this.callback(a[1], call, ctx, true)
      }
      case "@norn/authoring#Norn.decision": {
        if (a[0]) this.expr(a[0], ctx)
        if (a[1]) this.callback(a[1], call, ctx, true)
        return
      }
      case "@norn/authoring#Norn.flow": {
        if (a[0]) this.expr(a[0], ctx)
        const body = a[1]
        const before = this.diagnostics.length
        // Only an inline call can be a flow body; anything else is reported below as not inline.
        if (body && a.length === 2 && ts.isCallExpression(body)) this.expr(body, { ...eff, flow: true })
        // A lookalike or shadowed root is already reported (NORN-FLOW-SYMBOL) by the visit above.
        if (this.diagnostics.length > before) return
        if (!body || a.length !== 2 || !ts.isCallExpression(body) || this.flowRoot(body) !== "effect/Effect#gen") {
          this.report(body ?? call, "NORN-FLOW-UNSUPPORTED-SYNTAX", "the flow body must be an inline Effect.gen(function* () { ... }) (optionally .pipe'd)")
        }
        return
      }
      case "effect/Effect#gen": {
        const fn = a[0]
        if (a.length !== 1 || !fn || !ts.isFunctionExpression(fn) || !fn.asteriskToken || fn.parameters.length > 0) {
          return this.report(fn ?? call, "NORN-FLOW-CAPTURE", "Effect.gen takes one inline function* literal")
        }
        this.block(fn.body.statements, { ...ctx, generator: true, retryPolicy: false, pure: false, ret: "flow-result" })
        const last = fn.body.statements[fn.body.statements.length - 1]
        // A trailing bare `return` is already NORN-FLOW-RESULT.
        if (!last || !ts.isReturnStatement(last)) {
          this.report(fn, "NORN-FLOW-UNSUPPORTED-SYNTAX", "the generator must end with an explicit return of its result")
        }
        return
      }
      case "effect/Effect#map":
      case "effect/Effect#flatMap": {
        if (a.length === 2 && a[0]) this.expr(a[0], eff)
        return this.callback(a[a.length - 1], call, ctx, id === "effect/Effect#map")
      }
      case "effect/Effect#catchTag": {
        const tagIndex = a.length === 3 ? 1 : 0
        if (a.length === 3 && a[0]) this.expr(a[0], eff)
        const tag = a[tagIndex]
        if (!tag || !ts.isStringLiteral(tag)) this.report(tag ?? call, "NORN-FLOW-LITERAL", "catchTag takes one literal registered tag")
        return this.callback(a[tagIndex + 1], call, ctx)
      }
      case "effect/Effect#fail": {
        const err = a[0]
        if (!err || !ts.isNewExpression(err)) {
          return this.report(err ?? call, "NORN-FLOW-UNSUPPORTED-SYNTAX", "Effect.fail takes new <Data.TaggedError class>({ ...fields })")
        }
        return this.newExpression(err, ctx, true)
      }
      case "effect/Effect#all": {
        const tuple = a[0]
        if (!tuple || !ts.isArrayLiteralExpression(tuple)) {
          this.report(tuple ?? call, "NORN-FLOW-UNBOUNDED", "Effect.all takes a finite literal tuple")
        } else {
          for (const el of tuple.elements) {
            if (ts.isSpreadElement(el)) this.report(el, "NORN-FLOW-EXPRESSION", "spread is not admitted")
            else this.expr(el, eff)
          }
        }
        if (a[1]) this.report(a[1], "NORN-FLOW-CONCURRENCY", "Effect.all options (concurrency/mode) are not admitted; v1 is sequential")
        return
      }
      case "effect/Effect#retry": {
        if (a.length === 2 && a[0]) this.expr(a[0], eff)
        const policy = a[a.length - 1]
        if (!policy || ts.isObjectLiteralExpression(policy)) {
          return this.report(policy ?? call, "NORN-FLOW-UNSUPPORTED-SYNTAX", "use a Schedule policy: Schedule.recurs(n) or Schedule.max([delay, Schedule.recurs(n)])")
        }
        const before = this.diagnostics.length
        this.expr(policy, { ...ctx, retryPolicy: true })
        if (this.diagnostics.length === before && !this.finitePolicy(policy)) {
          this.report(policy, "NORN-FLOW-UNBOUNDED", "the retry policy must intersect a finite Schedule.recurs(n) (via Schedule.max)")
        }
        return
      }
      case "effect/Schedule#recurs": {
        const n = a[0]
        if (!n || !ts.isNumericLiteral(n) || !Number.isInteger(Number(n.text))) {
          this.report(n ?? call, "NORN-FLOW-LITERAL", "Schedule.recurs takes a non-negative integer literal")
        }
        return
      }
      case "effect/Schedule#spaced":
        return this.requireLiteral(a[0], call, "the schedule duration", false)
      case "effect/Schedule#exponential":
        this.requireLiteral(a[0], call, "the base duration", false)
        if (a[1]) this.requireLiteral(a[1], call, "the growth factor", true)
        return
      case "effect/Schedule#max": {
        const list = a[0]
        if (!list || !ts.isArrayLiteralExpression(list) || list.elements.length === 0) {
          return this.report(list ?? call, "NORN-FLOW-LITERAL", "Schedule.max takes a literal non-empty array of schedules")
        }
        return this.expr(list, ctx)
      }
      case "effect/Effect#sleep":
        return this.requireLiteral(a[0], call, "the sleep duration", false)
      case "@norn/authoring#Norn.perform": {
        const step = a[0]
        if (!step || !ts.isStringLiteral(step)) this.report(step ?? call, "NORN-FLOW-LITERAL", "the step id must be a string literal")
        const handle = a[1]
        if (!handle || !ts.isIdentifier(handle) || !this.isFnHandle(handle)) {
          this.report(handle ?? call, "NORN-FLOW-UNSUPPORTED-SYNTAX", "the Function must be a top-level const declared with Norn.fn")
        }
        if (a[2]) this.expr(a[2], ctx)
        return
      }
      case "effect/unstable/ai/Tool#make": {
        const n = a[0]
        if (!n || !ts.isStringLiteral(n)) this.report(n ?? call, "NORN-FLOW-LITERAL", "the tool name must be a string literal")
        for (const x of a.slice(1)) this.expr(x, ctx)
        return
      }
      case "effect/Function#pipe":
        return this.args(call, eff)
      default:
        return this.args(call, ctx)
    }
  }

  /**
   * Strip syntactic wrappers that do not change the runtime value: parentheses, `as`,
   * `satisfies`, `<T>` assertions, and `!`. (Assertions are reported where they are walked;
   * here they must not hide an effect from the data/effect position check.)
   */
  private unwrap(e: ts.Expression): ts.Expression {
    let inner = e
    while (
      ts.isParenthesizedExpression(inner) ||
      ts.isAsExpression(inner) ||
      ts.isSatisfiesExpression(inner) ||
      ts.isTypeAssertionExpression(inner) ||
      ts.isNonNullExpression(inner)
    ) {
      inner = inner.expression
    }
    return inner
  }

  /** Identity of the call at the root of a `.pipe` chain. */
  private flowRoot(call: ts.CallExpression): string | undefined {
    const id = this.calleeId(call)
    if (id === "effect/Pipeable#Pipeable.pipe" && ts.isPropertyAccessExpression(call.expression)) {
      const receiver = this.unwrap(call.expression.expression)
      return ts.isCallExpression(receiver) ? this.flowRoot(receiver) : undefined
    }
    if (id === "effect/Function#pipe") {
      const first = call.arguments[0] && this.unwrap(call.arguments[0])
      return first && ts.isCallExpression(first) ? this.flowRoot(first) : undefined
    }
    return id
  }

  private finitePolicy(policy: ts.Expression): boolean {
    if (!ts.isCallExpression(policy)) return false
    const id = this.calleeId(policy)
    if (id === "effect/Schedule#recurs") return true
    if (id !== "effect/Schedule#max") return false
    const list = policy.arguments[0]
    return (
      list !== undefined &&
      ts.isArrayLiteralExpression(list) &&
      list.elements.some((el) => ts.isCallExpression(el) && this.calleeId(el) === "effect/Schedule#recurs")
    )
  }

  private isFnHandle(id: ts.Identifier): boolean {
    const decl = this.resolve(id).declaration
    return (
      decl !== undefined &&
      ts.isVariableDeclaration(decl) &&
      (decl.parent.flags & ts.NodeFlags.Const) !== 0 &&
      decl.initializer !== undefined &&
      ts.isCallExpression(decl.initializer) &&
      this.calleeId(decl.initializer) === "@norn/authoring#Norn.fn"
    )
  }
}
