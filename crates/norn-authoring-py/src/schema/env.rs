//! Static module evaluation environment and intermediate AST declaration records
//! for zero-execution Python-to-`EntityIR`/`LawIR` lowering.

use std::collections::BTreeMap;

use ruff_python_ast::{Expr, ExprLambda, StmtFunctionDef};

use crate::canonical::DependencyPin;
use crate::error::SourceSpan;
use crate::expr::{ZenExpr, ZenLiteral};
use crate::schema::packs::{PackManifest, SecretRef};
use crate::schema::types::{
    FieldDefault, FieldMeta, FnRef, InvariantClassification, KeySpec, LawModality, LawSeverity,
    LinkSpec, MarkingDecl, MarkingsCatalog, MaskSpec, PropertyTypeSpec, RoleKind, ScopeRootKind,
    UnitRef,
};

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

    /// Collects all declared field names for `concept_py_name`, walking base `Concept`
    /// classes in MRO-like parent-first order (matching `Concept.declared_field_names`).
    pub(crate) fn declared_field_names(&self, concept_py_name: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut visited = Vec::new();
        self.collect_fields_recursive(concept_py_name, &mut visited, &mut names);
        names
    }

    fn collect_fields_recursive(
        &self,
        concept_py_name: &str,
        visited: &mut Vec<String>,
        out: &mut Vec<String>,
    ) {
        if visited.iter().any(|seen| seen == concept_py_name) {
            return;
        }
        visited.push(concept_py_name.to_owned());
        let Some(concept) = self.concepts.get(concept_py_name) else {
            return;
        };
        for base in &concept.base_py_names {
            self.collect_fields_recursive(base, visited, out);
        }
        for field in &concept.fields {
            if !out.iter().any(|existing| existing == &field.name) {
                out.push(field.name.clone());
            }
        }
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
