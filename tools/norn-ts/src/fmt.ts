import { lstatSync, readFileSync, readdirSync, writeFileSync } from "node:fs"
import { extname, join, relative, resolve } from "node:path"
import ts from "typescript"
import { COMPILER_OPTIONS } from "./check.ts"
import { ProjectError, readProject } from "./project.ts"

export const FORMAT_SETTINGS: ts.FormatCodeSettings = {
  indentSize: 2, tabSize: 2, convertTabsToSpaces: true, newLineCharacter: "\n",
  semicolons: ts.SemicolonPreference.Remove,
  insertSpaceAfterCommaDelimiter: true, insertSpaceAfterSemicolonInForStatements: true,
  insertSpaceBeforeAndAfterBinaryOperators: true, insertSpaceAfterKeywordsInControlFlowStatements: true,
  insertSpaceAfterOpeningAndBeforeClosingNonemptyBraces: true,
  insertSpaceAfterOpeningAndBeforeClosingNonemptyBrackets: false,
  insertSpaceAfterOpeningAndBeforeClosingNonemptyParenthesis: false,
  placeOpenBraceOnNewLineForFunctions: false, placeOpenBraceOnNewLineForControlBlocks: false,
}

function edits(text: string, changes: readonly ts.TextChange[]): string {
  for (const change of [...changes].sort((a, b) => b.span.start - a.span.start)) {
    text = text.slice(0, change.span.start) + change.newText + text.slice(change.span.start + change.span.length)
  }
  return text
}

let currentFile = ""
let currentText = ""
let version = 0
const host: ts.LanguageServiceHost = {
  getCompilationSettings: () => COMPILER_OPTIONS,
  getScriptFileNames: () => currentFile ? [currentFile] : [],
  getScriptVersion: path => path === currentFile ? String(version) : "0",
  getProjectVersion: () => String(version),
  getScriptSnapshot: path => {
    const source = path === currentFile ? currentText : ts.sys.readFile(path)
    return source === undefined ? undefined : ts.ScriptSnapshot.fromString(source)
  },
  getCurrentDirectory: () => process.cwd(), getDefaultLibFileName: options => ts.getDefaultLibFilePath(options),
  fileExists: ts.sys.fileExists, readFile: ts.sys.readFile, readDirectory: ts.sys.readDirectory,
}
// Reuse the pinned library declarations across files and formatting passes.
const service = ts.createLanguageService(host)

/** FMT-SEMANTIC-1: formatting never executes source or deletes unused imports. */
export function formatSource(file: string, text: string): string {
  currentFile = file
  currentText = text
  version++
  for (const change of service.organizeImports({ type: "file", fileName: file, skipDestructiveCodeActions: true }, FORMAT_SETTINGS, {})) {
    if (resolve(change.fileName) === resolve(file)) currentText = edits(currentText, change.textChanges)
  }
  version++
  return edits(currentText, service.getFormattingEditsForDocument(file, FORMAT_SETTINGS)).replace(/\r\n/g, "\n").replace(/\n*$/, "\n")
}

function collect(path: string, files: Set<string>): void {
  const stat = lstatSync(path)
  if (stat.isSymbolicLink()) return
  if (stat.isDirectory()) {
    for (const entry of readdirSync(path).sort()) {
      if (!["node_modules", ".norn", ".git"].includes(entry)) collect(join(path, entry), files)
    }
  } else if (stat.isFile() && [".ts", ".tsx"].includes(extname(path))) files.add(resolve(path))
}

export function fmt(args: readonly string[]): number {
  const paths: string[] = []
  for (const arg of args) {
    if (arg === "--check" || arg === "--json") continue
    if (arg.startsWith("-")) throw new ProjectError("cli.format.usage", "Unknown formatter option; use norn fmt [path...] [--check] [--json].", 2)
    paths.push(resolve(arg))
  }
  const files = new Set<string>()
  try {
    if (!paths.length) {
      const project = readProject()
      paths.push(...project.manifest.sources.map(path => resolve(project.root, path)))
    }
    for (const path of paths) collect(path, files)
    const changed: string[] = []
    for (const file of [...files].sort()) {
      const before = readFileSync(file, "utf8")
      const after = formatSource(file, before)
      if (after === before) continue
      changed.push(relative(process.cwd(), file))
      if (!args.includes("--check")) writeFileSync(file, after)
    }
    process.stdout.write(args.includes("--json") ? `${JSON.stringify({ changed, check: args.includes("--check") })}\n` : changed.map(path => `${path}\n`).join(""))
    return args.includes("--check") && changed.length ? 1 : 0
  } catch (error) {
    if (error instanceof ProjectError) throw error
    throw new ProjectError("cli.format.io", `Cannot format sources; check paths and permissions. ${String(error)}`, 3)
  }
}
