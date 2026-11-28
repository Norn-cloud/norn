use tsrs_ast::{Diagnostic, Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node};
use tsrs_checker::{Checker, Type, TypeFlags, is_tuple_type_exported};
use tsrs_core::P;
use tsrs_scanner::{get_ecma_line_and_utf16_character_of_position, get_token_pos_of_node};

use crate::BoundedVfs;
use crate::program::{resolve_profile_module, strip_quoted_module_prefix};

use super::{FileWalk, Position};

/// Host globals that read time, entropy, IO, or the environment.
const AMBIENT_ROOTS: &[&str] = &[
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
];

#[derive(Clone, Debug)]
pub(super) struct ResolvedSymbol {
    /// Profile identity (`effect/Effect#gen`, `@norn/authoring#Norn.flow`) if
    /// declared in a pinned profile module.
    pub(super) id: Option<String>,
    pub(super) namespace: bool,
    pub(super) target: Option<P<Symbol>>,
    pub(super) declaration: Option<P<Node>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TemporalKind {
    Date,
    Timestamp,
}

pub(super) fn resolve_symbol(
    checker: &mut Checker,
    intrinsics_module: &str,
    effect_dist_prefix: Option<&str>,
    vfs: &BoundedVfs,
    node: P<Node>,
) -> ResolvedSymbol {
    let direct = if let Some(parent) = node.parent()
        && parent.kind() == Kind::ShorthandPropertyAssignment
        && parent.as_shorthand_property_assignment().name() == node
    {
        checker.get_shorthand_assignment_value_symbol(Some(parent))
    } else {
        checker.get_symbol_at_location_exported(node)
    };
    let Some(direct) = direct else {
        return ResolvedSymbol {
            id: None,
            namespace: false,
            target: None,
            declaration: None,
        };
    };

    let target = if direct.flags().intersects(SymbolFlags::Alias) {
        checker.get_aliased_symbol(direct)
    } else {
        direct
    };
    let namespace = target
        .flags()
        .intersects(SymbolFlags::ValueModule | SymbolFlags::NamespaceModule);
    let Some(&declaration) = target.declarations().first() else {
        return ResolvedSymbol {
            id: None,
            namespace,
            target: Some(target),
            declaration: None,
        };
    };
    let Some(decl_sf) = get_source_file_of_node(declaration) else {
        return ResolvedSymbol {
            id: None,
            namespace,
            target: Some(target),
            declaration: Some(declaration),
        };
    };
    let declaring_file = decl_sf.file_name();
    let id = resolve_profile_module(declaring_file, intrinsics_module, effect_dist_prefix, vfs)
        .map(|module| {
            let fqn = checker.get_fully_qualified_name_exported(target);
            let qualified = strip_quoted_module_prefix(&fqn);
            format!("{module}#{qualified}")
        });
    ResolvedSymbol {
        id,
        namespace,
        target: Some(target),
        declaration: Some(declaration),
    }
}

pub(super) fn node_start(sf: P<SourceFile>, node: P<Node>) -> u32 {
    get_token_pos_of_node(node, sf, false)
}

pub(super) fn node_end(node: P<Node>) -> u32 {
    node.end().source_offset().unwrap_or(0)
}

pub(super) fn slice_text(sf: P<SourceFile>, start: u32, end: u32) -> String {
    let start_idx = usize::try_from(start).unwrap_or(usize::MAX);
    let end_idx = usize::try_from(end).unwrap_or(usize::MAX);
    sf.text().get(start_idx..end_idx).unwrap_or("").to_owned()
}

pub(super) fn node_text(sf: P<SourceFile>, node: P<Node>) -> String {
    slice_text(sf, node_start(sf, node), node_end(node))
}

pub(super) fn pos_of(sf: P<SourceFile>, offset: u32) -> Position {
    let (zero_line, zero_col) = get_ecma_line_and_utf16_character_of_position(sf.get(), offset);
    let line = u32::try_from(zero_line)
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let column = zero_col.saturating_add(1);
    Position { line, column }
}

pub(super) fn format_construct(raw: &str) -> String {
    let mut collapsed = String::with_capacity(raw.len().min(96));
    let mut in_ws = false;
    for ch in raw.trim().chars() {
        if ch.is_whitespace() {
            if !in_ws {
                collapsed.push(' ');
                in_ws = true;
            }
        } else {
            collapsed.push(ch);
            in_ws = false;
        }
    }
    if collapsed.chars().count() > 80 {
        let mut truncated: String = collapsed.chars().take(77).collect();
        truncated.push_str("...");
        truncated
    } else {
        collapsed
    }
}

pub(super) fn flatten_diagnostic_message(diag: P<Diagnostic>) -> String {
    let mut out = diag.localize();
    for &chained in diag.message_chain() {
        let sub = flatten_diagnostic_message(chained);
        if !sub.is_empty() {
            out.push(' ');
            out.push_str(&sub);
        }
    }
    out
}

pub(super) fn display_file_path(file_name: &str) -> String {
    file_name.strip_prefix('/').unwrap_or(file_name).to_owned()
}

pub(super) fn parse_js_number(text: &str) -> f64 {
    let cleaned: String = text.chars().filter(|&ch| ch != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        return parse_radix_f64(hex, 16);
    }
    if let Some(oct) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        return parse_radix_f64(oct, 8);
    }
    if let Some(bin) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        return parse_radix_f64(bin, 2);
    }
    cleaned.parse::<f64>().unwrap_or(f64::NAN)
}

fn parse_radix_f64(digits: &str, radix: u32) -> f64 {
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut value = 0.0_f64;
    let base = f64::from(radix);
    for ch in digits.chars() {
        let Some(digit) = ch.to_digit(radix) else {
            return f64::NAN;
        };
        value = value.mul_add(base, f64::from(digit));
    }
    value
}

pub(super) fn is_js_integer_literal(text: &str) -> bool {
    let val = parse_js_number(text);
    val.is_finite() && val.fract().abs().to_bits() == 0
}

fn root_identifier(mut expr: P<Node>) -> Option<P<Node>> {
    loop {
        let kind = expr.kind();
        if kind == Kind::PropertyAccessExpression {
            expr = expr.as_property_access_expression().expression();
        } else if kind == Kind::ElementAccessExpression {
            expr = expr.as_element_access_expression().expression();
        } else if kind == Kind::CallExpression {
            expr = expr.as_call_expression().expression();
        } else {
            break;
        }
    }
    (expr.kind() == Kind::Identifier).then_some(expr)
}

pub(super) fn list_nodes(list: Option<P<tsrs_ast::NodeList>>) -> &'static [P<Node>] {
    list.map_or(&[], |items| items.nodes())
}

pub(super) fn regular_literal_type(t: P<Type>) -> P<Type> {
    if t.flags().intersects(TypeFlags::Freshable) {
        t.as_literal_type().regular_type().unwrap_or(t)
    } else {
        t
    }
}

pub(super) fn every_type(t: P<Type>, pred: impl Fn(P<Type>) -> bool) -> bool {
    if t.is_union() {
        t.types().iter().all(|&member| pred(member))
    } else {
        pred(t)
    }
}

impl FileWalk<'_> {
    pub(super) fn resolve(&mut self, node: P<Node>) -> ResolvedSymbol {
        resolve_symbol(
            self.checker,
            &self.owner.profile.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            node,
        )
    }

    pub(super) fn is_default_lib(&self, declaration: Option<P<Node>>) -> bool {
        let Some(decl) = declaration else {
            return false;
        };
        let Some(sf) = get_source_file_of_node(decl) else {
            return false;
        };
        self.program.is_source_file_default_library(sf.path())
    }

    pub(super) fn where_declared(&self, declaration: Option<P<Node>>) -> String {
        let Some(decl) = declaration else {
            return "an unknown declaration".to_owned();
        };
        let Some(file) = get_source_file_of_node(decl) else {
            return "an unknown declaration".to_owned();
        };
        let p = pos_of(file, node_start(file, decl));
        format!("{}:{}", display_file_path(file.file_name()), p.line)
    }

    pub(super) fn is_ambient(&mut self, expr: P<Node>) -> bool {
        let Some(root) = root_identifier(expr) else {
            return false;
        };
        let resolved = self.resolve(root);
        if !self.is_default_lib(resolved.declaration) {
            return false;
        }
        AMBIENT_ROOTS.contains(&root.text()) || node_text(self.sf, expr) == "Math.random"
    }

    pub(super) fn callee_id(&mut self, call: P<Node>) -> Option<String> {
        let callee = call.as_call_expression().expression();
        let name = if callee.kind() == Kind::PropertyAccessExpression {
            callee.as_property_access_expression().name()
        } else if callee.kind() == Kind::Identifier {
            callee
        } else {
            return None;
        };
        self.resolve(name).id
    }

    pub(super) fn type_of(&mut self, node: P<Node>) -> P<Type> {
        self.checker.get_type_at_location(node)
    }

    pub(super) fn is_boolean(&mut self, node: P<Node>) -> bool {
        let t = self.type_of(node);
        every_type(t, |member| {
            member.flags().intersects(TypeFlags::BooleanLike)
        })
    }

    pub(super) fn is_string(&mut self, node: P<Node>) -> bool {
        let t = self.type_of(node);
        every_type(t, |member| member.flags().intersects(TypeFlags::StringLike))
    }

    pub(super) fn is_number(&mut self, node: P<Node>) -> bool {
        let t = self.type_of(node);
        every_type(t, |member| member.flags().intersects(TypeFlags::NumberLike))
    }

    pub(super) fn is_scalar(&mut self, node: P<Node>) -> bool {
        let scalar = TypeFlags::StringLike
            | TypeFlags::NumberLike
            | TypeFlags::BooleanLike
            | TypeFlags::EnumLike
            | TypeFlags::Null;
        let t = self.type_of(node);
        every_type(t, |member| member.flags().intersects(scalar))
    }

    pub(super) fn is_nullable_null_check(&mut self, left: P<Node>, right: P<Node>) -> bool {
        self.is_null_against_nullable(left, right) || self.is_null_against_nullable(right, left)
    }

    fn is_null_against_nullable(&mut self, null_side: P<Node>, other_side: P<Node>) -> bool {
        if null_side.kind() != Kind::NullKeyword {
            return false;
        }
        let t = self.type_of(other_side);
        t.is_union()
            && t.types()
                .iter()
                .any(|&member| member.flags().intersects(TypeFlags::Null))
    }

    pub(super) fn is_array_or_tuple(&self, t: P<Type>) -> bool {
        self.checker.is_array_type_exported(t) || is_tuple_type_exported(t)
    }

    pub(super) fn same_parsed_temporal(&mut self, left: P<Node>, right: P<Node>) -> bool {
        let Some(left_kind) = self.parsed_temporal_kind(left) else {
            return false;
        };
        self.parsed_temporal_kind(right) == Some(left_kind)
    }

    fn parsed_temporal_kind(&mut self, mut node: P<Node>) -> Option<TemporalKind> {
        while node.kind() == Kind::ParenthesizedExpression {
            node = node.as_parenthesized_expression().expression();
        }
        if node.kind() != Kind::CallExpression {
            return None;
        }
        let id = self.callee_id(node)?;
        match id.as_str() {
            "@norn/authoring#Norn.parseDate" | "@norn/authoring#Norn.addDays" => {
                Some(TemporalKind::Date)
            }
            "@norn/authoring#Norn.parseTimestamp"
            | "@norn/authoring#Norn.addSeconds"
            | "@norn/authoring#Norn.resolveLocalTimestamp" => Some(TemporalKind::Timestamp),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_js_integer_literal;

    #[test]
    fn js_integer_literal_is_exact_without_epsilon() {
        assert!(is_js_integer_literal("0"));
        assert!(is_js_integer_literal("-0"));
        assert!(is_js_integer_literal("42"));
        assert!(is_js_integer_literal("-42"));
        assert!(is_js_integer_literal("1e2"));
        assert!(is_js_integer_literal("0x10"));
        assert!(is_js_integer_literal("1_000"));

        assert!(!is_js_integer_literal("1e-17"));
        assert!(!is_js_integer_literal("1e-300"));
        assert!(!is_js_integer_literal("5e-324"));
        assert!(!is_js_integer_literal("1.5"));
        assert!(!is_js_integer_literal("1e400"));
    }
}
