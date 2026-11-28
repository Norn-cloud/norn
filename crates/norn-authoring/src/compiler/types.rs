//! Structural TypeScript type → closed [`ValueSchema`] lowering, artifact
//! envelopes, numeric literal exactness, and shared compiler limits/errors.
//!
//! Ports `tools/norn-ts/src/compiler/types.ts`, `builder/envelope.ts`, and
//! `exactNumber` / `checkClosure` / `diagnostic` from `compiler/index.ts`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use tsrs_ast::{Kind, Node, SourceFile, SymbolFlags, get_source_file_of_node};
use tsrs_checker::{Checker, LiteralValue, SignatureKind, Type, TypeFlags, is_tuple_type_exported};
use tsrs_compiler::Program;
use tsrs_core::P;

use crate::ProfileDiagnostic;
use crate::checker::resolve::{
    format_construct, node_end, node_start, node_text, parse_js_number, pos_of,
};

use super::canonical::{MAX_SAFE_INTEGER_I64, MIN_SAFE_INTEGER_I64, to_canonical_json};
use super::flow_source_map::SourceLocation;
use super::value_schema::{
    BudgetExceeded, ExpansionBudget, ObjectSchema, ValueSchema, merge_schemas,
};

/// Resource ceilings enforced during syntax compilation (`COMPILER_LIMITS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompilerLimits {
    /// Maximum cumulative UTF-8 source bytes across the local import closure.
    pub max_source_bytes: usize,
    /// Maximum AST node/token count across the local import closure.
    pub max_nodes: usize,
    /// Maximum expansion work units per module.
    pub max_expansion_work: usize,
    /// Maximum local source modules in the closure.
    pub max_modules: usize,
    /// Maximum AST nesting depth.
    pub max_depth: usize,
    /// Maximum accumulated output bytes.
    pub max_output_bytes: usize,
    /// Cooperative wall-clock time bound in milliseconds.
    pub max_millis: u64,
}

impl Default for CompilerLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 1_048_576,
            max_nodes: 100_000,
            max_expansion_work: 100_000,
            max_modules: 128,
            max_depth: 128,
            max_output_bytes: 8_388_608,
            max_millis: 30_000,
        }
    }
}

impl CompilerLimits {
    /// Clamps `self` so no field exceeds the default hard ceilings.
    #[must_use]
    pub fn clamped(self) -> Self {
        let max = Self::default();
        Self {
            max_source_bytes: self.max_source_bytes.min(max.max_source_bytes),
            max_nodes: self.max_nodes.min(max.max_nodes),
            max_expansion_work: self.max_expansion_work.min(max.max_expansion_work),
            max_modules: self.max_modules.min(max.max_modules),
            max_depth: self.max_depth.min(max.max_depth),
            max_output_bytes: self.max_output_bytes.min(max.max_output_bytes),
            max_millis: self.max_millis.min(max.max_millis),
        }
    }
}

/// Internal compiler error carrying a diagnostic code, message, and source node.
#[derive(Clone, Debug)]
pub(crate) struct CompileError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) node: P<Node>,
}

impl CompileError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>, node: P<Node>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            node,
        }
    }

    pub(crate) fn into_diagnostic(self) -> ProfileDiagnostic {
        diagnostic_for_node(self.node, &self.code, &self.message)
    }
}

/// Builds a [`ProfileDiagnostic`] anchored at `node` matching `diagnostic` in `index.ts`.
#[must_use]
pub(crate) fn diagnostic_for_node(node: P<Node>, code: &str, message: &str) -> ProfileDiagnostic {
    let Some(sf) = get_source_file_of_node(node) else {
        return ProfileDiagnostic {
            code: code.to_owned(),
            start: crate::Position { line: 1, column: 1 },
            end: crate::Position { line: 1, column: 1 },
            construct: String::new(),
            message: message.to_owned(),
        };
    };
    let start_off = node_start(sf, node);
    let end_off = node_end(node);
    ProfileDiagnostic {
        code: code.to_owned(),
        start: pos_of(sf, start_off),
        end: pos_of(sf, end_off),
        construct: format_construct(&node_text(sf, node)),
        message: message.to_owned(),
    }
}

/// Computes a POSIX relative path from `base` to `target` (matching Node `path.relative`).
#[must_use]
pub(crate) fn relative_virtual_path(base: &str, target: &str) -> String {
    let base_parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    let target_parts: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let common = base_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = Vec::new();
    for _ in common..base_parts.len() {
        out.push("..");
    }
    if let Some(rest) = target_parts.get(common..) {
        out.extend(rest.iter().copied());
    }
    out.join("/")
}

/// Computes a [`SourceLocation`] for `node` relative to `root`.
#[must_use]
pub(crate) fn source_location_of(node: P<Node>, root: &str) -> SourceLocation {
    let Some(sf) = get_source_file_of_node(node) else {
        return SourceLocation {
            file: String::new(),
            line: 1,
            column: Some(1),
        };
    };
    let pos = pos_of(sf, node_start(sf, node));
    SourceLocation {
        file: relative_virtual_path(root, sf.file_name()),
        line: pos.line,
        column: Some(pos.column),
    }
}

/// Install-time envelope options (`EnvelopeOptions` in `builder/envelope.ts`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeOptions {
    /// Envelope scope dimensions (`orgId`, `environmentId`, etc.).
    pub scope: BTreeMap<String, String>,
    /// Repo-relative path of the authoring module.
    pub source_path: String,
    /// Optional repo-relative registry package path; when set, emits a registry envelope.
    pub registry_package_path: Option<String>,
}

/// Exact entity or rule dependency pin (`DependencyPin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyPin {
    /// Pinned artifact identifier.
    pub artifact_id: String,
    /// Pinned artifact version.
    pub version: String,
    /// Optional artifact kind override (`"entity"` or `"rule"`).
    pub kind: Option<String>,
}

/// Converts dependency pins to canonical envelope dependency entries.
#[must_use]
pub(crate) fn pinned_dependencies(pins: &[DependencyPin], default_kind: &str) -> Vec<Value> {
    pins.iter()
        .map(|pin| {
            let mut obj = Map::new();
            obj.insert(
                "artifactId".to_owned(),
                Value::String(pin.artifact_id.clone()),
            );
            obj.insert("version".to_owned(), Value::String(pin.version.clone()));
            obj.insert(
                "kind".to_owned(),
                Value::String(pin.kind.clone().unwrap_or_else(|| default_kind.to_owned())),
            );
            obj.insert("required".to_owned(), Value::Bool(true));
            Value::Object(obj)
        })
        .collect()
}

/// Metadata required to emit an `irVersion: "0.1"` artifact envelope.
#[derive(Clone, Debug)]
pub(crate) struct EnvelopeMeta {
    pub(crate) kind: String,
    pub(crate) artifact_id: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) dependencies: Vec<Value>,
}

/// Builds a canonical `irVersion: "0.1"` artifact envelope (`envelope` in `builder/envelope.ts`).
#[must_use]
pub(crate) fn build_envelope(
    meta: EnvelopeMeta,
    payload: Value,
    options: &EnvelopeOptions,
) -> Value {
    let registry = options.registry_package_path.is_some();
    let registry_root = options
        .registry_package_path
        .as_deref()
        .map(|p| p.replace('\\', "/").trim_end_matches('/').to_owned());
    let mut scope_map = Map::new();
    if !registry {
        for (k, v) in &options.scope {
            scope_map.insert(k.clone(), Value::String(v.clone()));
        }
    }
    let mut prov = Map::new();
    prov.insert(
        "source".to_owned(),
        Value::String(if registry { "system" } else { "authoring_sdk" }.to_owned()),
    );
    let source_path = match registry_root {
        Some(root) => format!("{root}/artifacts/{}.json", meta.artifact_id),
        None => options.source_path.clone(),
    };
    prov.insert("sourcePath".to_owned(), Value::String(source_path));

    let mut env = Map::new();
    env.insert("irVersion".to_owned(), Value::String("0.1".to_owned()));
    env.insert("artifactId".to_owned(), Value::String(meta.artifact_id));
    env.insert("kind".to_owned(), Value::String(meta.kind));
    env.insert("namespace".to_owned(), Value::String(meta.namespace));
    env.insert("name".to_owned(), Value::String(meta.name));
    env.insert("version".to_owned(), Value::String(meta.version));
    env.insert(
        "lifecycle".to_owned(),
        Value::String(if registry { "active" } else { "draft" }.to_owned()),
    );
    env.insert("scope".to_owned(), Value::Object(scope_map));
    env.insert("dependencies".to_owned(), Value::Array(meta.dependencies));
    env.insert("provenance".to_owned(), Value::Object(prov));
    env.insert("payload".to_owned(), payload);
    let raw = Value::Object(env);
    to_canonical_json(&raw).unwrap_or(raw)
}

/// Checks local import closure module count, source bytes, AST node count, and nesting depth.
///
/// Ports `checkClosure` in `tools/norn-ts/src/compiler/index.ts`.
pub(crate) fn check_closure(
    program: &'static Program,
    source: P<SourceFile>,
    limits: CompilerLimits,
) -> Result<(), CompileError> {
    let local: Vec<P<SourceFile>> = program
        .get_source_files()
        .iter()
        .copied()
        .filter(|sf| {
            !sf.is_declaration_file() && !program.is_source_file_from_external_library(*sf)
        })
        .collect();
    if local.len() > limits.max_modules {
        return Err(CompileError::new(
            "NORN-FLOW-UNBOUNDED",
            "import closure exceeds compiler module bound",
            source.as_node(),
        ));
    }
    let mut bytes: usize = 0;
    let mut nodes: usize = 0;
    for sf in local {
        bytes = bytes.saturating_add(sf.text().len());
        if bytes > limits.max_source_bytes {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "import closure exceeds compiler byte bound",
                source.as_node(),
            ));
        }
        count_ast_nodes(sf.as_node(), 0, &mut nodes, limits)?;
    }
    Ok(())
}

fn count_ast_nodes(
    node: P<Node>,
    depth: usize,
    nodes: &mut usize,
    limits: CompilerLimits,
) -> Result<(), CompileError> {
    *nodes = nodes.saturating_add(1);
    if *nodes > limits.max_nodes || depth > limits.max_depth {
        return Err(CompileError::new(
            "NORN-FLOW-UNBOUNDED",
            "syntax exceeds compiler node or nesting bound",
            node,
        ));
    }
    let next_depth = depth.saturating_add(1);
    let mut error: Option<CompileError> = None;
    node.for_each_child(&mut |child| {
        if let Err(err) = count_ast_nodes(child, next_depth, nodes, limits) {
            error = Some(err);
            return true;
        }
        false
    });
    if let Some(err) = error {
        return Err(err);
    }
    Ok(())
}

/// Parses a TypeScript numeric literal into an exact JSON [`Number`], refusing
/// non-finite, unsafe-integer, or precision-losing decimals (`exactNumber` in `index.ts`).
pub(crate) fn exact_number(
    sf: P<SourceFile>,
    node: P<Node>,
    started: Instant,
    max_millis: u64,
    negate: bool,
) -> Result<Number, CompileError> {
    let check_time = || -> Result<(), CompileError> {
        if started.elapsed() >= Duration::from_millis(max_millis) {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "numeric normalization exceeds cooperative time bound",
                node,
            ));
        }
        Ok(())
    };
    check_time()?;
    let raw = node_text(sf, node);
    let source: String = raw.chars().filter(|&ch| ch != '_').collect();
    let value = parse_js_number(&source);
    check_time()?;
    let is_int = value.is_finite() && value.fract().abs().to_bits() == 0;
    let is_safe_int = is_int && value.abs() <= 9_007_199_254_740_991.0;
    if !value.is_finite() || (is_int && !is_safe_int) {
        return Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "numeric literal has no exact JSON/IR representation",
            node,
        ));
    }
    let final_val = if negate { -value } else { value };
    if source.starts_with("0x")
        || source.starts_with("0X")
        || source.starts_with("0b")
        || source.starts_with("0B")
        || source.starts_with("0o")
        || source.starts_with("0O")
    {
        return f64_to_json_number(final_val, node);
    }
    let written = normalize_decimal_str(&source, node, &check_time)?;
    let encoded_str = value.to_string();
    let encoded = normalize_decimal_str(&encoded_str, node, &check_time)?;
    if written != encoded {
        return Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "numeric literal loses precision in canonical JSON",
            node,
        ));
    }
    f64_to_json_number(final_val, node)
}

fn f64_to_json_number(val: f64, node: P<Node>) -> Result<Number, CompileError> {
    if !val.is_finite() {
        return Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "numeric literal has no exact JSON/IR representation",
            node,
        ));
    }
    if val.abs().to_bits() == 0 {
        return Ok(Number::from(0_i64));
    }
    if val.fract().abs().to_bits() == 0
        && val >= -9_007_199_254_740_991.0
        && val <= 9_007_199_254_740_991.0
        && let Ok(i) = format!("{val:.0}").parse::<i64>()
        && (MIN_SAFE_INTEGER_I64..=MAX_SAFE_INTEGER_I64).contains(&i)
    {
        return Ok(Number::from(i));
    }
    Number::from_f64(val).ok_or_else(|| {
        CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "numeric literal has no exact JSON/IR representation",
            node,
        )
    })
}

fn normalize_decimal_str(
    text: &str,
    node: P<Node>,
    check_time: &impl Fn() -> Result<(), CompileError>,
) -> Result<(String, i64), CompileError> {
    let (mantissa, exp_part) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (text, None),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "invalid decimal numeric literal",
            node,
        ));
    }
    let exp: i64 = match exp_part {
        None => 0,
        Some(e) => {
            let valid = e
                .strip_prefix(['+', '-'])
                .unwrap_or(e)
                .bytes()
                .all(|b| b.is_ascii_digit());
            if !valid {
                return Err(CompileError::new(
                    "NORN-FLOW-EXPRESSION",
                    "invalid decimal numeric literal",
                    node,
                ));
            }
            e.parse::<i64>().unwrap_or(0)
        }
    };
    let mut digits = String::with_capacity(int_part.len().max(1).saturating_add(frac_part.len()));
    if int_part.is_empty() {
        digits.push('0');
    } else {
        digits.push_str(int_part);
    }
    digits.push_str(frac_part);
    let bytes = digits.as_bytes();
    let mut first: usize = 0;
    let mut end: usize = bytes.len();
    while first < end && bytes.get(first).copied() == Some(b'0') {
        if (first & 4095) == 0 {
            check_time()?;
        }
        first = first.saturating_add(1);
    }
    if first == end {
        return Ok(("0".to_owned(), 0));
    }
    while end > first && bytes.get(end.saturating_sub(1)).copied() == Some(b'0') {
        if (end & 4095) == 0 {
            check_time()?;
        }
        end = end.saturating_sub(1);
    }
    check_time()?;
    let trimmed = digits.get(first..end).unwrap_or("0").to_owned();
    let frac_len = i64::try_from(frac_part.len()).unwrap_or(i64::MAX);
    let trailing = i64::try_from(bytes.len().saturating_sub(end)).unwrap_or(i64::MAX);
    let adjusted_exp = exp.saturating_sub(frac_len).saturating_add(trailing);
    Ok((trimmed, adjusted_exp))
}

/// Memoization and expansion budget for lowering structural `tsrs` types to [`ValueSchema`].
pub(crate) struct SchemaBudget {
    memo: HashMap<P<Type>, (ValueSchema, usize)>,
}

impl SchemaBudget {
    pub(crate) fn new() -> Self {
        Self {
            memo: HashMap::new(),
        }
    }

    /// Lowers a structural TypeScript `type_` to a closed [`ValueSchema`] (`valueSchema` in `types.ts`).
    pub(crate) fn value_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        ty: P<Type>,
        node: P<Node>,
        active: &HashSet<P<Type>>,
    ) -> Result<ValueSchema, CompileError> {
        expansion
            .check()
            .map_err(|BudgetExceeded(msg)| CompileError::new("NORN-FLOW-UNBOUNDED", msg, node))?;
        if active.contains(&ty) {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "recursive Function schema",
                node,
            ));
        }
        if let Some((known, height)) = self.memo.get(&ty) {
            if active.len().saturating_add(*height) >= 32 {
                return Err(CompileError::new(
                    "NORN-FLOW-UNBOUNDED",
                    "Function schema exceeds nesting bound",
                    node,
                ));
            }
            return Ok(known.clone());
        }
        let result = self.build_value_schema(checker, expansion, ty, node, active)?;
        expansion
            .assert_schema(&result)
            .map_err(|BudgetExceeded(msg)| CompileError::new("NORN-FLOW-UNBOUNDED", msg, node))?;
        let height = result.height();
        if active.len().saturating_add(height) >= 32 {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "Function schema exceeds nesting bound",
                node,
            ));
        }
        self.memo.insert(ty, (result.clone(), height));
        Ok(result)
    }

    fn build_value_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        ty: P<Type>,
        node: P<Node>,
        active: &HashSet<P<Type>>,
    ) -> Result<ValueSchema, CompileError> {
        if active.contains(&ty) || active.len() >= 32 {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "recursive or deeply nested Function types are not closed schemas",
                node,
            ));
        }
        let mut next = active.clone();
        next.insert(ty);
        if ty.is_union() {
            let members = ty.types();
            if members
                .iter()
                .all(|&m| m.flags().intersects(TypeFlags::StringLiteral))
            {
                let mut vals = Vec::with_capacity(members.len());
                for &m in members {
                    if let Some(LiteralValue::String(s)) = m.as_literal_type().value() {
                        vals.push(s.to_owned());
                    }
                }
                return Ok(ValueSchema::text_enum(vals));
            }
            if members
                .iter()
                .all(|&m| m.flags().intersects(TypeFlags::BooleanLike))
            {
                return Ok(ValueSchema::boolean());
            }
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "Function schemas do not admit general unions",
                node,
            ));
        }
        let flags = ty.flags();
        if flags.intersects(TypeFlags::StringLike) {
            if flags.intersects(TypeFlags::StringLiteral)
                && let Some(LiteralValue::String(s)) = ty.as_literal_type().value()
            {
                return Ok(ValueSchema::text_enum(vec![s.to_owned()]));
            }
            return Ok(ValueSchema::text());
        }
        if flags.intersects(TypeFlags::NumberLike) {
            return Ok(ValueSchema::number());
        }
        if flags.intersects(TypeFlags::BooleanLike) {
            return Ok(ValueSchema::boolean());
        }
        if flags.intersects(TypeFlags::Null) {
            return Ok(ValueSchema::null_value());
        }
        if checker.is_array_type_exported(ty) || is_tuple_type_exported(ty) {
            let args = checker.get_type_arguments_exported(ty);
            let Some(&first) = args.first() else {
                return Err(CompileError::new(
                    "NORN-FLOW-VALUE",
                    "an array needs a closed item type",
                    node,
                ));
            };
            let mut items = self.value_schema(checker, expansion, first, node, &next)?;
            for &arg in args.iter().skip(1) {
                let next_item = self.value_schema(checker, expansion, arg, node, &next)?;
                items = merge_schemas(&items, &next_item, "$")
                    .map_err(|msg| CompileError::new("NORN-FLOW-VALUE", msg, node))?;
                expansion
                    .assert_schema(&items)
                    .map_err(|BudgetExceeded(msg)| {
                        CompileError::new("NORN-FLOW-UNBOUNDED", msg, node)
                    })?;
            }
            return Ok(ValueSchema::array(items));
        }
        if !flags.intersects(TypeFlags::Object)
            || !checker
                .get_signatures_of_type_exported(ty, SignatureKind::Call)
                .is_empty()
            || !checker.get_index_infos_of_type_exported(ty).is_empty()
        {
            let ty_str = checker.type_to_string_exported(ty);
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                format!("type {ty_str} is outside the closed Function schema subset"),
                node,
            ));
        }
        self.build_object_value_schema(checker, expansion, ty, node, &next)
    }

    fn build_object_value_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        ty: P<Type>,
        node: P<Node>,
        next: &HashSet<P<Type>>,
    ) -> Result<ValueSchema, CompileError> {
        let props = checker.get_properties_of_type_exported(ty);
        let mut fields = Vec::with_capacity(props.len());
        let mut optional = Vec::new();
        for &prop in props {
            let decl = prop
                .value_declaration()
                .or_else(|| prop.declarations().first().copied())
                .unwrap_or(node);
            let mut member = checker
                .get_type_of_symbol_at_location(prop, Some(decl))
                .unwrap_or_else(|| checker.get_type_of_symbol_exported(prop));
            let prop_name = prop.name().to_owned();
            if prop.flags().intersects(SymbolFlags::Optional) {
                optional.push(prop_name.clone());
                if member.is_union() {
                    let present: Vec<P<Type>> = member
                        .types()
                        .iter()
                        .copied()
                        .filter(|&t| !t.flags().intersects(TypeFlags::Undefined))
                        .collect();
                    if present.len() != 1 {
                        return Err(CompileError::new(
                            "NORN-FLOW-VALUE",
                            "optional fields must have one closed type",
                            decl,
                        ));
                    }
                    if let Some(&single) = present.first() {
                        member = single;
                    }
                }
            }
            let field_schema = self.value_schema(checker, expansion, member, decl, next)?;
            fields.push((prop_name, field_schema));
        }
        Ok(ValueSchema::object(ObjectSchema::record_ordered(
            fields, &optional,
        )))
    }

    /// Joins union callback result branches through closed schema merge (`resultSchema` in `types.ts`).
    pub(crate) fn result_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        ty: P<Type>,
        node: P<Node>,
    ) -> Result<ValueSchema, CompileError> {
        let empty = HashSet::new();
        if !ty.is_union() {
            return self.value_schema(checker, expansion, ty, node, &empty);
        }
        let mut branches = Vec::with_capacity(ty.types().len());
        for &branch in ty.types() {
            branches.push(self.value_schema(checker, expansion, branch, node, &empty)?);
        }
        let Some(first) = branches.first().cloned() else {
            return Ok(ValueSchema::null_value());
        };
        let mut schema = first;
        for branch in branches.iter().skip(1) {
            schema = merge_schemas(&schema, branch, "$").map_err(|msg| {
                CompileError::new(
                    "NORN-FLOW-VALUE",
                    format!("callback results have no closed shared shape: {msg}"),
                    node,
                )
            })?;
            expansion
                .assert_schema(&schema)
                .map_err(|BudgetExceeded(msg)| {
                    CompileError::new("NORN-FLOW-UNBOUNDED", msg, node)
                })?;
        }
        Ok(schema)
    }
}

/// Checks whether a `Node` is a `StringLiteral` or `NoSubstitutionTemplateLiteral`.
#[must_use]
pub(crate) fn is_string_literal_like(node: P<Node>) -> bool {
    let kind = node.kind();
    kind == Kind::StringLiteral || kind == Kind::NoSubstitutionTemplateLiteral
}
