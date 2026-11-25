import ts from "typescript"
import { CompileError } from "./types.ts"

export type FlowStatement = ts.VariableStatement | ts.ExpressionStatement | ts.ReturnStatement
  | ts.IfStatement | ts.SwitchStatement | ts.Block | ts.EmptyStatement

export function flowStatement(node: ts.Statement): FlowStatement {
  if (ts.isVariableStatement(node) || ts.isExpressionStatement(node) || ts.isReturnStatement(node)
      || ts.isIfStatement(node) || ts.isSwitchStatement(node) || ts.isBlock(node) || ts.isEmptyStatement(node)) return node
  throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "statement has no lowering", node)
}

export function assertNever(statement: never): never {
  throw new CompileError("NORN-FLOW-UNSUPPORTED-SYNTAX", "statement has no lowering", statement)
}

type Terminator = "break" | "return" | "fallthrough" | "end"
export function terminator(clause: ts.CaseOrDefaultClause, last: boolean): Terminator {
  const tail = clause.statements.at(-1)
  if (tail && ts.isBreakStatement(tail)) return "break"
  if (tail && returns(tail)) return "return"
  return last ? "end" : "fallthrough"
}

/** The profile checks exhaustiveness of every switch before lowering it. */
export function returns(node: ts.Statement): boolean {
  if (ts.isReturnStatement(node)) return true
  if (ts.isBlock(node)) return node.statements.some(returns)
  if (ts.isIfStatement(node)) return !!node.elseStatement && returns(node.thenStatement) && returns(node.elseStatement)
  if (ts.isSwitchStatement(node)) {
    const clauses = node.caseBlock.clauses
    return clauses.length > 0 && clauses.every((clause, index) => {
      const exit = terminator(clause, index === clauses.length - 1)
      return exit === "return" || (exit === "fallthrough" && clause.statements.length === 0)
    })
  }
  return false
}

export function switchClauses(node: ts.SwitchStatement) {
  const groups: { labels: ts.Expression[]; fallback: boolean; body: readonly ts.Statement[]; terminator: Terminator }[] = []
  let labels: ts.Expression[] = [], fallback = false
  const clauses = node.caseBlock.clauses
  for (const [i, clause] of clauses.entries()) {
    if (ts.isCaseClause(clause)) labels.push(clause.expression)
    else fallback = true
    const exit = terminator(clause, i + 1 === clauses.length)
    if (exit === "fallthrough") {
      if (clause.statements.length) throw new CompileError("NORN-FLOW-ROUTE", "nonempty switch clause falls through", clause)
      continue
    }
    const body = exit === "break" ? clause.statements.slice(0, -1) : clause.statements
    groups.push({ labels, fallback, body, terminator: exit })
    labels = []
    fallback = false
  }
  return groups
}
