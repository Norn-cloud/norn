use tsrs_ast::{Kind, Node};
use tsrs_core::P;

/// Exit classification for a `switch` clause (ports `tools/norn-ts/src/compiler/cfg.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Terminator {
    Break,
    Return,
    Fallthrough,
    End,
}

/// Classifies how control exits `clause` (`CaseClause` or `DefaultClause`).
pub(super) fn terminator(clause: P<Node>, is_last: bool) -> Terminator {
    let stmts = clause.as_case_or_default_clause().statements.nodes();
    if let Some(&tail) = stmts.last() {
        if tail.kind() == Kind::BreakStatement {
            return Terminator::Break;
        }
        if returns(tail) {
            return Terminator::Return;
        }
    }
    if is_last {
        Terminator::End
    } else {
        Terminator::Fallthrough
    }
}

/// Returns `true` if every control-flow path through `node` terminates in a
/// `return` statement.
///
/// Mirrors `returns` in `tools/norn-ts/src/compiler/cfg.ts`.
pub(super) fn returns(node: P<Node>) -> bool {
    let kind = node.kind();
    if kind == Kind::ReturnStatement {
        return true;
    }
    if kind == Kind::Block {
        return node
            .as_block()
            .statements
            .nodes()
            .iter()
            .any(|&stmt| returns(stmt));
    }
    if kind == Kind::IfStatement {
        let if_stmt = node.as_if_statement();
        return if_stmt
            .else_statement
            .is_some_and(|else_stmt| returns(if_stmt.then_statement) && returns(else_stmt));
    }
    if kind == Kind::SwitchStatement {
        let clauses = node
            .as_switch_statement()
            .case_block
            .as_case_block()
            .clauses
            .nodes();
        if clauses.is_empty() {
            return false;
        }
        let len = clauses.len();
        return clauses.iter().enumerate().all(|(index, &clause)| {
            let is_last = index.saturating_add(1) == len;
            let exit = terminator(clause, is_last);
            match exit {
                Terminator::Return => true,
                Terminator::Fallthrough => clause
                    .as_case_or_default_clause()
                    .statements
                    .nodes()
                    .is_empty(),
                Terminator::Break | Terminator::End => false,
            }
        });
    }
    false
}
