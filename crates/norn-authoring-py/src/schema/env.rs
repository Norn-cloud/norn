//! Static module evaluation environment and intermediate AST declaration records
//! for zero-execution Python-to-`EntityIR`/`LawIR` lowering.

use std::collections::{BTreeMap, BTreeSet};

use ruff_python_ast::{Expr, ExprLambda, StmtFunctionDef};

use crate::canonical::DependencyPin;
use crate::error::{CODE_SCHEMA, CODE_UNBOUNDED, PyAuthoringError, SourceSpan};
use crate::expr::{ZenExpr, ZenLiteral};
use crate::schema::packs::{PackManifest, SecretRef};
use crate::schema::types::{
    FieldDefault, FieldMeta, FnRef, InvariantClassification, KeySpec, LawModality, LawSeverity,
    LinkSpec, MarkingDecl, MarkingsCatalog, MaskSpec, PropertyTypeSpec, RoleKind, ScopeRootKind,
    UnitRef,
};

/// Maximum total structural value nodes allocated or cloned during static module evaluation.
pub(crate) const MAX_EVAL_VALUE_NODES: usize = 50_000;
/// Maximum expression evaluation nesting depth during static module evaluation.
pub(crate) const MAX_EVAL_DEPTH: usize = 128;
/// Maximum `Concept` inheritance chain depth walked during field resolution.
pub(crate) const MAX_INHERITANCE_DEPTH: usize = 64;

/// Reference to a Python callback AST node (`def` or `lambda`) inside the parsed module.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CallbackRef<'a> {
    Func(&'a StmtFunctionDef),
    Lambda(&'a ExprLambda),
}

/// Scalar member value on a declared Python `StrEnum` or `Enum`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EnumMemberVal {
    Str(String),
    Int(String),
}

impl EnumMemberVal {
    pub(crate) fn as_str_value(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value.as_str()),
            Self::Int(_) => None,
        }
    }

    pub(crate) fn to_zen_literal(&self) -> ZenLiteral {
        match self {
            Self::Str(value) => ZenLiteral::Str(value.clone()),
            Self::Int(digits) => ZenLiteral::Int(digits.clone()),
        }
    }
}

/// Declared Python `StrEnum` or `Enum` class captured during static module evaluation.
#[derive(Clone, Debug)]
pub(crate) struct EnumDecl {
    pub(crate) py_name: String,
    pub(crate) is_str_enum: bool,
    /// Ordered `(member_name, value)` pairs preserving declaration order.
    pub(crate) members: Vec<(String, EnumMemberVal)>,
}

impl EnumDecl {
    pub(crate) fn member(&self, name: &str) -> Option<&EnumMemberVal> {
        self.members
            .iter()
            .find(|(m, _)| m == name)
            .map(|(_, val)| val)
    }

    pub(crate) fn str_values(&self) -> Vec<String> {
        self.members
            .iter()
            .filter_map(|(_, val)| val.as_str_value().map(str::to_owned))
            .collect()
    }
}

/// Evaluated lifecycle transition AST before callback lowering.
#[derive(Clone, Debug)]
pub(crate) struct TransitionAst<'a> {
    pub(crate) id: String,
    pub(crate) from_states: Vec<String>,
    pub(crate) to_state: String,
    pub(crate) guards: Vec<CallbackRef<'a>>,
    pub(crate) description: Option<String>,
    pub(crate) guard_refusal_code: Option<String>,
}

/// Evaluated lifecycle AST before callback lowering.
#[derive(Clone, Debug)]
pub(crate) struct LifecycleAst<'a> {
    pub(crate) property: String,
    pub(crate) initial: Vec<String>,
    pub(crate) transitions: Vec<TransitionAst<'a>>,
    pub(crate) terminal: Vec<String>,
    pub(crate) immutable: Vec<String>,
    pub(crate) description: Option<String>,
}

/// Evaluated `@invariant(...)` method declaration on a `Concept` or interface.
#[derive(Clone, Debug)]
pub(crate) struct InvariantAst<'a> {
    pub(crate) id: String,
    pub(crate) message: String,
    pub(crate) func: &'a StmtFunctionDef,
    pub(crate) description: Option<String>,
    pub(crate) classification: InvariantClassification,
}

/// Declared field annotation & optional class-body default inside a `Concept` class.
#[derive(Clone, Debug)]
pub(crate) struct ConceptFieldAst<'a> {
    pub(crate) name: String,
    pub(crate) annotation: &'a Expr,
    pub(crate) default: Option<FieldDefault>,
    pub(crate) span: SourceSpan,
}

/// Declared `Concept` or interface class captured during Phase 1 static evaluation.
#[derive(Clone, Debug)]
pub(crate) struct ConceptDecl<'a> {
    pub(crate) py_name: String,
    pub(crate) norn_name: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) is_interface: bool,
    pub(crate) role: Option<RoleKind>,
    pub(crate) description: Option<String>,
    pub(crate) extends: Option<String>,
    pub(crate) implements: Vec<String>,
    pub(crate) interface_extends: Vec<String>,
    pub(crate) markings: Vec<String>,
    pub(crate) identity: Vec<KeySpec>,
    pub(crate) dependencies: Vec<DependencyPin>,
    pub(crate) base_py_names: Vec<String>,
    pub(crate) fields: Vec<ConceptFieldAst<'a>>,
    pub(crate) lifecycles: Vec<LifecycleAst<'a>>,
    pub(crate) invariants: Vec<InvariantAst<'a>>,
    pub(crate) span: SourceSpan,
}

/// Declared `@law(...)` function captured during Phase 1 static evaluation.
#[derive(Clone, Debug)]
pub(crate) struct LawDeclAst<'a> {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) owner: String,
    pub(crate) subject_py_name: String,
    pub(crate) rationale: String,
    pub(crate) predicate: &'a StmtFunctionDef,
    pub(crate) severity: LawSeverity,
    pub(crate) modality: LawModality,
    pub(crate) scope_root_kind: ScopeRootKind,
    pub(crate) scope_root_id: Option<String>,
    pub(crate) dependencies: Vec<DependencyPin>,
    pub(crate) span: SourceSpan,
}

/// Closed static value domain for module-level and class-body declarative expressions.
#[derive(Clone, Debug)]
pub(crate) enum EvalValue<'a> {
    None,
    Bool(bool),
    Int(String),
    Decimal(String),
    Str(String),
    Tuple(Vec<Self>),
    List(Vec<Self>),
    Dict(BTreeMap<String, Self>),
    ModuleNamespace(&'static str),
    BuiltinSymbol(&'static str),
    EnumClass(String),
    EnumMember(Box<EnumMemberVal>),
    ConceptClass(String),
    ProtocolClass,
    UnitRef(Box<UnitRef>),
    DependencyPin(Box<DependencyPin>),
    KeySpec(Box<KeySpec>),
    Marking(String),
    FieldMeta(Box<FieldMeta>),
    FieldDefault(Box<FieldDefault>),
    PropertyTypeSpec(Box<PropertyTypeSpec>),
    LinkSpec(Box<LinkSpec>),
    MaskSpec(MaskSpec),
    MarkingDecl(Box<MarkingDecl>),
    MarkingsCatalog(Box<MarkingsCatalog>),
    LawDecl(String),
    FnRef(Box<FnRef>),
    SecretRef(Box<SecretRef>),
    PackManifest(Box<PackManifest>),
    DeploymentSpec,
    TransitionSpec(Box<TransitionAst<'a>>),
    LifecycleSpec(Box<LifecycleAst<'a>>),
    ZenExpr(Box<ZenExpr>),
    Callback(CallbackRef<'a>),
    OpaqueObject,
}

impl EvalValue<'_> {
    /// Counts structural nodes in this value without recursion, stopping early once `cap` is
    /// exceeded so exponential binding graphs cannot cause unbounded cloning.
    pub(crate) fn bounded_node_count(&self, cap: usize) -> usize {
        let mut total: usize = 0;
        let mut stack: Vec<&Self> = vec![self];
        while let Some(val) = stack.pop() {
            total = total.saturating_add(1);
            if total > cap {
                return total;
            }
            match val {
                Self::Tuple(items) | Self::List(items) => {
                    for item in items {
                        stack.push(item);
                    }
                }
                Self::Dict(map) => {
                    for entry in map.values() {
                        total = total.saturating_add(1);
                        if total > cap {
                            return total;
                        }
                        stack.push(entry);
                    }
                }
                Self::MarkingsCatalog(cat) => {
                    total = total.saturating_add(cat.markings.len());
                }
                Self::PackManifest(pack) => {
                    total = total.saturating_add(
                        pack.requires
                            .len()
                            .saturating_add(pack.entities.len())
                            .saturating_add(pack.laws.len())
                            .saturating_add(pack.markings.len())
                            .saturating_add(pack.functions.len()),
                    );
                }
                Self::LifecycleSpec(lc) => {
                    total = total.saturating_add(lc.transitions.len());
                }
                Self::None
                | Self::Bool(_)
                | Self::Int(_)
                | Self::Decimal(_)
                | Self::Str(_)
                | Self::ModuleNamespace(_)
                | Self::BuiltinSymbol(_)
                | Self::EnumClass(_)
                | Self::EnumMember(_)
                | Self::ConceptClass(_)
                | Self::ProtocolClass
                | Self::UnitRef(_)
                | Self::DependencyPin(_)
                | Self::KeySpec(_)
                | Self::Marking(_)
                | Self::FieldMeta(_)
                | Self::FieldDefault(_)
                | Self::PropertyTypeSpec(_)
                | Self::LinkSpec(_)
                | Self::MaskSpec(_)
                | Self::MarkingDecl(_)
                | Self::LawDecl(_)
                | Self::FnRef(_)
                | Self::SecretRef(_)
                | Self::DeploymentSpec
                | Self::TransitionSpec(_)
                | Self::ZenExpr(_)
                | Self::Callback(_)
                | Self::OpaqueObject => {}
            }
            if total > cap {
                return total;
            }
        }
        total
    }
}

/// Ordered top-level artifact handle recorded during Phase 1 for deterministic Phase 2 emission.
#[derive(Clone, Debug)]
pub(crate) enum ArtifactEntry {
    MarkingsCatalog { binding_name: String },
    Concept { py_name: String },
    Law { binding_name: String },
}

/// Module-wide symbol table built during Phase 1 static AST evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModuleEnv<'a> {
    pub(crate) bindings: BTreeMap<String, EvalValue<'a>>,
    pub(crate) enums: BTreeMap<String, EnumDecl>,
    pub(crate) concepts: BTreeMap<String, ConceptDecl<'a>>,
    pub(crate) laws: BTreeMap<String, LawDeclAst<'a>>,
    pub(crate) emission_order: Vec<ArtifactEntry>,
    pub(crate) eval_nodes_used: usize,
}

impl<'a> ModuleEnv<'a> {
    /// Seeds built-in Python type/constant names exposed in Layer 2's restricted builtins.
    pub(crate) fn new() -> Self {
        let mut env = Self::default();
        for builtin in ["bool", "dict", "frozenset", "int", "list", "str", "tuple"] {
            env.bindings
                .insert(builtin.to_owned(), EvalValue::BuiltinSymbol(builtin));
        }
        env
    }

    /// Returns the remaining static evaluation value-node budget before failing closed.
    pub(crate) fn remaining_eval_nodes(&self) -> usize {
        MAX_EVAL_VALUE_NODES.saturating_sub(self.eval_nodes_used)
    }

    /// Charges `nodes` against [`MAX_EVAL_VALUE_NODES`], failing closed with [`CODE_UNBOUNDED`].
    pub(crate) fn charge_eval_nodes(
        &mut self,
        nodes: usize,
        span: &SourceSpan,
    ) -> Result<(), PyAuthoringError> {
        self.eval_nodes_used = self.eval_nodes_used.saturating_add(nodes);
        if self.eval_nodes_used > MAX_EVAL_VALUE_NODES {
            return Err(PyAuthoringError::spanned(
                CODE_UNBOUNDED,
                format!("static evaluation exceeded {MAX_EVAL_VALUE_NODES} value node budget"),
                span.clone(),
            ));
        }
        Ok(())
    }

    /// Collects all declared field names for `concept_py_name`, walking base `Concept`
    /// classes in MRO-like parent-first order (matching `Concept.declared_field_names`)
    /// with cycle detection and [`MAX_INHERITANCE_DEPTH`] enforcement.
    pub(crate) fn declared_field_names(
        &self,
        concept_py_name: &str,
        span: &SourceSpan,
    ) -> Result<Vec<String>, PyAuthoringError> {
        let mut names = Vec::new();
        let mut visited = BTreeSet::new();
        let mut in_progress = Vec::new();
        self.collect_fields_recursive(
            concept_py_name,
            0,
            &mut visited,
            &mut in_progress,
            &mut names,
            span,
        )?;
        Ok(names)
    }

    fn collect_fields_recursive(
        &self,
        concept_py_name: &str,
        depth: usize,
        visited: &mut BTreeSet<String>,
        in_progress: &mut Vec<String>,
        out: &mut Vec<String>,
        span: &SourceSpan,
    ) -> Result<(), PyAuthoringError> {
        if in_progress.iter().any(|active| active == concept_py_name) {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("cyclic Concept inheritance detected involving '{concept_py_name}'"),
                span.clone(),
            ));
        }
        if depth > MAX_INHERITANCE_DEPTH {
            return Err(PyAuthoringError::spanned(
                CODE_UNBOUNDED,
                format!("Concept inheritance depth exceeds maximum ({MAX_INHERITANCE_DEPTH})"),
                span.clone(),
            ));
        }
        if visited.contains(concept_py_name) {
            return Ok(());
        }
        let Some(concept) = self.concepts.get(concept_py_name) else {
            return Ok(());
        };
        in_progress.push(concept_py_name.to_owned());
        let next_depth = depth.saturating_add(1);
        for base in &concept.base_py_names {
            self.collect_fields_recursive(base, next_depth, visited, in_progress, out, span)?;
        }
        let _popped = in_progress.pop();
        visited.insert(concept_py_name.to_owned());
        for field in &concept.fields {
            if !out.iter().any(|existing| existing == &field.name) {
                out.push(field.name.clone());
            }
        }
        Ok(())
    }

    /// Collects all compile-time scalar and enum-member constants for callback lowering.
    pub(crate) fn callback_constants(&self) -> BTreeMap<String, ZenLiteral> {
        let mut constants = BTreeMap::new();
        for (enum_name, decl) in &self.enums {
            for (member_name, val) in &decl.members {
                constants.insert(format!("{enum_name}.{member_name}"), val.to_zen_literal());
            }
        }
        for (name, val) in &self.bindings {
            match val {
                EvalValue::None => {
                    constants.insert(name.clone(), ZenLiteral::Null);
                }
                EvalValue::Bool(b) => {
                    constants.insert(name.clone(), ZenLiteral::Bool(*b));
                }
                EvalValue::Int(digits) => {
                    constants.insert(name.clone(), ZenLiteral::Int(digits.clone()));
                }
                EvalValue::Decimal(formatted) => {
                    constants.insert(name.clone(), ZenLiteral::Decimal(formatted.clone()));
                }
                EvalValue::Str(text) => {
                    constants.insert(name.clone(), ZenLiteral::Str(text.clone()));
                }
                EvalValue::EnumMember(member) => {
                    constants.insert(name.clone(), member.to_zen_literal());
                }
                EvalValue::ZenExpr(expr) => {
                    if let ZenExpr::Literal(lit) = expr.as_ref() {
                        constants.insert(name.clone(), lit.clone());
                    }
                }
                EvalValue::Tuple(_)
                | EvalValue::List(_)
                | EvalValue::Dict(_)
                | EvalValue::ModuleNamespace(_)
                | EvalValue::BuiltinSymbol(_)
                | EvalValue::EnumClass(_)
                | EvalValue::ConceptClass(_)
                | EvalValue::ProtocolClass
                | EvalValue::UnitRef(_)
                | EvalValue::DependencyPin(_)
                | EvalValue::KeySpec(_)
                | EvalValue::Marking(_)
                | EvalValue::FieldMeta(_)
                | EvalValue::FieldDefault(_)
                | EvalValue::PropertyTypeSpec(_)
                | EvalValue::LinkSpec(_)
                | EvalValue::MaskSpec(_)
                | EvalValue::MarkingDecl(_)
                | EvalValue::MarkingsCatalog(_)
                | EvalValue::LawDecl(_)
                | EvalValue::FnRef(_)
                | EvalValue::SecretRef(_)
                | EvalValue::PackManifest(_)
                | EvalValue::DeploymentSpec
                | EvalValue::TransitionSpec(_)
                | EvalValue::LifecycleSpec(_)
                | EvalValue::Callback(_)
                | EvalValue::OpaqueObject => {}
            }
        }
        constants
    }
}
