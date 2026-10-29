//! Checks over `EntityIR` v2 shared property types (T1-ONT-1a, #2511).
//!
//! `norn-ir` only decodes [`PropertyTypeDef`]s; every consistency rule is a
//! diagnostic here: the name grammar, unique names (also against the registry
//! value types a catalog may reference), resolvable and acyclic `ref`s, the
//! decimal scale range, units, enum values, bound grammar, range and order,
//! constraint applicability, and pattern compilation.
//!
//! Spans are JSON pointers into the checked definition array
//! (`/0/constraints/min`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use norn_ir::{
    DECIMAL_LITERAL_PATTERN, DecimalLiteral, DiagnosticSeverity, MarkingId,
    PROPERTY_TYPE_NAME_PATTERN, PropertyConstraints, PropertyKind, PropertyType, PropertyTypeDef,
    PropertyTypeName,
};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, Span};

/// Diagnostic codes emitted by [`check_property_type_defs`].
pub mod codes {
    /// A property type name does not follow the dotted lower-case grammar.
    pub const INVALID_NAME: &str = "property_type.validation.invalid_name";
    /// Two definitions, or a definition and a registry value type, share a name.
    pub const DUPLICATE_NAME: &str = "property_type.validation.duplicate_name";
    /// A `ref` names no local definition and no registry value type.
    pub const UNKNOWN_REF: &str = "property_type.validation.unknown_ref";
    /// A chain of `ref`s returns to the definition it started from.
    pub const REF_CYCLE: &str = "property_type.validation.ref_cycle";
    /// A `decimal` scale exceeds [`super::MAX_DECIMAL_SCALE`].
    pub const SCALE_OUT_OF_RANGE: &str = "property_type.validation.scale_out_of_range";
    /// A unit code is blank or contains whitespace.
    pub const INVALID_UNIT: &str = "property_type.validation.invalid_unit";
    /// An `enum` declares no values.
    pub const EMPTY_ENUM: &str = "property_type.validation.empty_enum";
    /// An `enum` value is blank.
    pub const INVALID_ENUM_VALUE: &str = "property_type.validation.invalid_enum_value";
    /// An `enum` value is declared twice.
    pub const DUPLICATE_ENUM_VALUE: &str = "property_type.validation.duplicate_enum_value";
    /// A constraint does not apply to the property kind.
    pub const CONSTRAINT_NOT_APPLICABLE: &str =
        "property_type.validation.constraint_not_applicable";
    /// A bound is not an exact decimal literal.
    pub const INVALID_BOUND: &str = "property_type.validation.invalid_bound";
    /// An `integer` bound has a fraction or is outside the signed 64-bit range.
    pub const BOUND_NOT_INTEGER: &str = "property_type.validation.bound_not_integer";
    /// A `decimal` bound needs more fraction digits than the declared scale.
    pub const BOUND_EXCEEDS_SCALE: &str = "property_type.validation.bound_exceeds_scale";
    /// A bound is not exactly representable as a ZEN Expression decimal
    /// (`norn_expr::Number`: a 96-bit mantissa with at most 28 fraction
    /// digits), so rules and computed properties could not evaluate it.
    pub const BOUND_OUT_OF_RANGE: &str = "property_type.validation.bound_out_of_range";
    /// `min` is greater than `max`, or `minLength` greater than `maxLength`.
    pub const INVERTED_RANGE: &str = "property_type.validation.inverted_range";
    /// `pattern` is not a valid `regex-lite` expression.
    pub const INVALID_PATTERN: &str = "property_type.validation.invalid_pattern";
}

/// Largest admitted `decimal` scale: the exact range of ZEN Expression's
/// decimal number (`rust_decimal`, 28 fraction digits), so every declared
/// value is representable where rules and computed properties evaluate it.
pub const MAX_DECIMAL_SCALE: u64 = 28;

/// Compiled-program size limit for a `pattern` (bytes), so one definition
/// cannot make the checker or runtime build an unbounded automaton.
pub const PATTERN_SIZE_LIMIT: usize = 1 << 20;

static NAME_GRAMMAR: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(PROPERTY_TYPE_NAME_PATTERN).ok());
static DECIMAL_GRAMMAR: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(DECIMAL_LITERAL_PATTERN).ok());

/// Check a catalog of shared property type definitions.
///
/// `registry` holds the names of registry (Norn Core) value types the catalog
/// may reference; a local definition may not reuse one of those names.
/// Returns every finding; an empty result means the catalog is valid.
#[must_use]
pub fn check_property_type_defs(
    defs: &[PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut local: BTreeMap<&PropertyTypeName, usize> = BTreeMap::new();

    for (index, def) in defs.iter().enumerate() {
        let at = |segments: &[&str]| span(index, segments);
        if !matches_grammar(&NAME_GRAMMAR, def.name.as_str()) {
            diagnostics.push(error(
                codes::INVALID_NAME,
                format!(
                    "property type name {:?} must be dot-separated segments of [a-z][a-z0-9_]*",
                    def.name.as_str()
                ),
                at(&["name"]),
            ));
        }
        if registry.contains(&def.name) {
            diagnostics.push(error(
                codes::DUPLICATE_NAME,
                format!(
                    "property type {:?} shadows the registry value type of the same name",
                    def.name.as_str()
                ),
                at(&["name"]),
            ));
        } else if let Some(first) = local.get(&def.name) {
            diagnostics.push(error(
                codes::DUPLICATE_NAME,
                format!(
                    "property type {:?} is already defined at index {first}",
                    def.name.as_str()
                ),
                at(&["name"]),
            ));
        } else {
            local.insert(&def.name, index);
        }
    }

    for (index, def) in defs.iter().enumerate() {
        let position = index.to_string();
        let base = [position.as_str()];
        check_type_members(&base, &def.property_type, &mut diagnostics);
        check_record_refs(&base, &def.property_type, defs, registry, &mut diagnostics);
        let resolved = match resolve(defs, &local, registry, index) {
            Resolution::Base(base) => Some(base),
            Resolution::Registry => None,
            Resolution::Unknown(name) => {
                if matches!(&def.property_type, PropertyType::Ref { name: direct } if direct == name)
                {
                    diagnostics.push(error(
                        codes::UNKNOWN_REF,
                        format!(
                            "ref {:?} names no property type in the catalog or registry",
                            name.as_str()
                        ),
                        span(index, &["type", "name"]),
                    ));
                }
                None
            }
            Resolution::Cycle { includes_start } => {
                if includes_start {
                    diagnostics.push(error(
                        codes::REF_CYCLE,
                        format!("ref chain from {:?} returns to itself", def.name.as_str()),
                        span(index, &["type", "name"]),
                    ));
                }
                None
            }
        };
        check_constraints(&base, &def.constraints, resolved, &mut diagnostics);
    }

    diagnostics
}

/// Check every reference inside a structured value, including cycles that
/// pass through record members. Depth is bounded before recursive consumers.
pub(crate) fn check_record_refs(
    pointer: &[&str],
    ty: &PropertyType,
    catalog: &[PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
    out: &mut Vec<Diagnostic>,
) {
    struct Walk<'a, 'o> {
        catalog: &'a [PropertyTypeDef],
        registry: &'a BTreeSet<PropertyTypeName>,
        path: BTreeSet<PropertyTypeName>,
        completed: BTreeSet<PropertyTypeName>,
        out: &'o mut Vec<Diagnostic>,
    }
    impl Walk<'_, '_> {
        fn visit(&mut self, pointer: &[&str], ty: &PropertyType, depth: usize) {
            if depth >= 64 {
                self.out.push(error(
                    codes::REF_CYCLE,
                    "structured type exceeds maximum depth 64".to_owned(),
                    span_at(pointer, &["type"]),
                ));
                return;
            }
            match ty {
                PropertyType::Ref { name } => {
                    if !self.path.insert(name.clone()) {
                        self.out.push(error(
                            codes::REF_CYCLE,
                            format!("recursive record reference {}", name.as_str()),
                            span_at(pointer, &["type", "name"]),
                        ));
                        return;
                    }
                    if !self.completed.contains(name) {
                        if let Some(def) = self.catalog.iter().find(|def| def.name == *name) {
                            self.visit(pointer, &def.property_type, depth.saturating_add(1));
                        } else if !self.registry.contains(name) {
                            self.out.push(error(
                                codes::UNKNOWN_REF,
                                format!("unknown record member type {}", name.as_str()),
                                span_at(pointer, &["type", "name"]),
                            ));
                        }
                        self.completed.insert(name.clone());
                    }
                    self.path.remove(name);
                }
                PropertyType::Record { members } => {
                    for (index, member) in members.iter().enumerate() {
                        let index = index.to_string();
                        let pointer = [pointer, &["type", "members", &index]].concat();
                        let resolved =
                            resolve_type(&member.property_type, self.catalog, self.registry);
                        if let TypeResolution::Base(base) = resolved.resolution {
                            check_constraints(&pointer, &member.constraints, Some(base), self.out);
                        }
                        self.visit(&pointer, &member.property_type, depth.saturating_add(1));
                    }
                }
                PropertyType::Text { .. }
                | PropertyType::Integer {}
                | PropertyType::Decimal { .. }
                | PropertyType::Boolean {}
                | PropertyType::Timestamp { .. }
                | PropertyType::Date {}
                | PropertyType::Duration {}
                | PropertyType::Enum { .. }
                | PropertyType::JsonExt { .. } => {}
            }
        }
    }
    if matches!(ty, PropertyType::Record { .. }) {
        Walk {
            catalog,
            registry,
            path: BTreeSet::new(),
            completed: BTreeSet::new(),
            out,
        }
        .visit(pointer, ty, 0);
    }
}

/// Metadata read through all structured members, kept separate from a
/// property's own ref chain (which controls type conformance).
pub(crate) fn structured_metadata(
    ty: &PropertyType,
    catalog: &[PropertyTypeDef],
) -> (BTreeSet<PropertyTypeName>, BTreeSet<MarkingId>) {
    let mut names = BTreeSet::new();
    let mut markings = BTreeSet::new();
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        match ty {
            PropertyType::Ref { name } => {
                if names.insert(name.clone())
                    && let Some(def) = catalog.iter().find(|def| def.name == *name)
                {
                    markings.extend(def.markings.iter().cloned());
                    pending.push(&def.property_type);
                }
            }
            PropertyType::Record { members } => {
                pending.extend(members.iter().map(|member| &member.property_type));
            }
            PropertyType::Text { .. }
            | PropertyType::Integer {}
            | PropertyType::Decimal { .. }
            | PropertyType::Boolean {}
            | PropertyType::Timestamp { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Enum { .. }
            | PropertyType::JsonExt { .. } => {}
        }
    }
    (names, markings)
}

/// What a property's type resolves to through a catalog of shared property
/// types, with the constraints every catalog definition on the `ref` chain
/// adds.
#[derive(Clone)]
pub(crate) struct ResolvedType<'a> {
    pub(crate) resolution: TypeResolution<'a>,
    /// Constraints of each catalog definition the chain passes through.
    pub(crate) chain: Vec<&'a PropertyConstraints>,
    /// Markings supplied by each shared property type in the chain.
    pub(crate) markings: Vec<&'a MarkingId>,
    /// Every name the chain follows, in order (a registry or unresolved
    /// name last).
    pub(crate) names: Vec<&'a PropertyTypeName>,
}

/// The end of a `ref` chain.
#[derive(Clone, Copy)]
pub(crate) enum TypeResolution<'a> {
    /// A local, non-`ref` type.
    Base(&'a PropertyType),
    /// A registry value type: its kind is not known here.
    Registry,
    /// The chain does not resolve; `direct` when the type itself names an
    /// unknown type (a failure deeper in the catalog is the catalog check's).
    Unresolved { direct: bool },
}

/// Follow `ref`s from `property_type` through `catalog` (first definition of
/// a name; duplicates and cycles are the catalog check's findings).
pub(crate) fn resolve_type<'a>(
    property_type: &'a PropertyType,
    catalog: &'a [PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
) -> ResolvedType<'a> {
    let mut chain = Vec::new();
    let mut markings = Vec::new();
    let mut names = Vec::new();
    let mut visited = BTreeSet::new();
    let mut current = property_type;
    loop {
        let PropertyType::Ref { name } = current else {
            return ResolvedType {
                resolution: TypeResolution::Base(current),
                chain,
                markings,
                names,
            };
        };
        names.push(name);
        let resolution = match catalog.iter().position(|def| &def.name == name) {
            Some(index) if visited.insert(index) => {
                if let Some(def) = catalog.get(index) {
                    chain.push(&def.constraints);
                    markings.extend(&def.markings);
                    current = &def.property_type;
                    continue;
                }
                TypeResolution::Unresolved { direct: false }
            }
            Some(_) => TypeResolution::Unresolved { direct: false },
            None if registry.contains(name) => TypeResolution::Registry,
            None => TypeResolution::Unresolved {
                direct: visited.is_empty(),
            },
        };
        return ResolvedType {
            resolution,
            chain,
            markings,
            names,
        };
    }
}

/// Resolve a property's type to the terminal local definition's base type.
///
/// This follows the same `ref` chain as property-type checking. Registry
/// types are not included in this local catalog, so they return `None` along
/// with unknown and cyclic references; callers that need the concrete kind
/// must fail closed when resolution is absent.
#[must_use]
pub fn resolved_property_type<'a>(
    property_type: &'a PropertyType,
    catalog: &'a [PropertyTypeDef],
) -> Option<&'a PropertyType> {
    let registry = BTreeSet::new();
    match resolve_type(property_type, catalog, &registry).resolution {
        TypeResolution::Base(base) => Some(base),
        TypeResolution::Registry | TypeResolution::Unresolved { .. } => None,
    }
}

/// What a definition's type resolves to after following `ref`s.
enum Resolution<'a> {
    /// A local, non-`ref` type.
    Base(&'a PropertyType),
    /// A registry value type: its kind is not known here.
    Registry,
    /// The chain reached a name that resolves nowhere.
    Unknown(&'a PropertyTypeName),
    /// The chain loops; `includes_start` when the start is on the loop.
    Cycle { includes_start: bool },
}

fn resolve<'a>(
    defs: &'a [PropertyTypeDef],
    local: &BTreeMap<&PropertyTypeName, usize>,
    registry: &BTreeSet<PropertyTypeName>,
    start: usize,
) -> Resolution<'a> {
    let mut visited = BTreeSet::from([start]);
    let mut current = start;
    loop {
        let Some(def) = defs.get(current) else {
            return Resolution::Registry;
        };
        let PropertyType::Ref { name } = &def.property_type else {
            return Resolution::Base(&def.property_type);
        };
        match local.get(name) {
            Some(&next) => {
                if !visited.insert(next) {
                    return Resolution::Cycle {
                        includes_start: next == start,
                    };
                }
                current = next;
            }
            None if registry.contains(name) => return Resolution::Registry,
            None => return Resolution::Unknown(name),
        }
    }
}

/// Per-kind member checks of one type (`scale`, `unit`, enum `values`); `base`
/// is the JSON-pointer prefix of the object holding `type`.
pub(crate) fn check_type_members(
    base: &[&str],
    property_type: &PropertyType,
    out: &mut Vec<Diagnostic>,
) {
    match property_type {
        PropertyType::Record { members } => {
            if members.is_empty() {
                out.push(error(
                    "property_type.validation.empty_record",
                    "record must declare members".to_owned(),
                    span_at(base, &["type", "members"]),
                ));
            }
            let mut seen = BTreeSet::new();
            for (index, member) in members.iter().enumerate() {
                let position = index.to_string();
                let mut pointer = base.to_vec();
                pointer.extend(["type", "members", &position]);
                if !matches_grammar(&NAME_GRAMMAR, member.id.as_str())
                    || member.id.as_str().contains('.')
                {
                    out.push(error(
                        "property_type.validation.invalid_member",
                        "record member must match [a-z][a-z0-9_]*".to_owned(),
                        span_at(&pointer, &["id"]),
                    ));
                }
                if !seen.insert(&member.id) {
                    out.push(error(
                        "property_type.validation.duplicate_member",
                        format!("duplicate record member {}", member.id.as_str()),
                        span_at(&pointer, &["id"]),
                    ));
                }
                check_type_members(&pointer, &member.property_type, out);
            }
        }
        PropertyType::Decimal { scale, unit, .. } => {
            if scale.0 > MAX_DECIMAL_SCALE {
                out.push(error(
                    codes::SCALE_OUT_OF_RANGE,
                    format!(
                        "decimal scale {} exceeds the maximum of {MAX_DECIMAL_SCALE}",
                        scale.0
                    ),
                    span_at(base, &["type", "scale"]),
                ));
            }
            if let Some(unit) = unit
                && (unit.code.is_empty() || unit.code.chars().any(char::is_whitespace))
            {
                out.push(error(
                    codes::INVALID_UNIT,
                    format!(
                        "unit code {:?} must be non-blank without whitespace",
                        unit.code
                    ),
                    span_at(base, &["type", "unit", "code"]),
                ));
            }
        }
        PropertyType::Enum { values } => {
            if values.is_empty() {
                out.push(error(
                    codes::EMPTY_ENUM,
                    "enum declares no values".to_owned(),
                    span_at(base, &["type", "values"]),
                ));
            }
            let mut seen = BTreeSet::new();
            for (position, value) in values.iter().enumerate() {
                let position = position.to_string();
                let at = span_at(base, &["type", "values", &position, "value"]);
                if value.value.trim().is_empty() {
                    out.push(error(
                        codes::INVALID_ENUM_VALUE,
                        "enum value must be non-blank".to_owned(),
                        at,
                    ));
                } else if !seen.insert(value.value.as_str()) {
                    out.push(error(
                        codes::DUPLICATE_ENUM_VALUE,
                        format!("enum value {:?} is declared twice", value.value),
                        at,
                    ));
                }
            }
        }
        PropertyType::Text { .. }
        | PropertyType::Integer {}
        | PropertyType::Boolean {}
        | PropertyType::Timestamp { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => {}
    }
}

/// Constraint applicability, bounds, and text constraints against the
/// resolved base type (`None` when it is a registry type or unresolvable);
/// `base` is the JSON-pointer prefix of the object holding `constraints`.
pub(crate) fn check_constraints(
    base_pointer: &[&str],
    constraints: &PropertyConstraints,
    base: Option<&PropertyType>,
    out: &mut Vec<Diagnostic>,
) {
    let kind = base.map(PropertyType::kind);
    let numeric = matches!(
        kind,
        None | Some(PropertyKind::Integer | PropertyKind::Decimal)
    );

    let mut bounds = [None, None];
    for (slot, (member, bound)) in bounds
        .iter_mut()
        .zip([("min", &constraints.min), ("max", &constraints.max)])
    {
        let Some(bound) = bound else { continue };
        let at = span_at(base_pointer, &["constraints", member]);
        if !numeric {
            out.push(not_applicable(member, kind, at));
            continue;
        }
        let Some(parsed) = Decimal::parse(bound) else {
            out.push(error(
                codes::INVALID_BOUND,
                format!(
                    "{member} {:?} must be an exact decimal literal such as \"-12.50\"",
                    bound.as_str()
                ),
                at,
            ));
            continue;
        };
        match base {
            Some(PropertyType::Integer {})
                if !parsed.fraction.is_empty() || bound.as_str().parse::<i64>().is_err() =>
            {
                out.push(error(
                    codes::BOUND_NOT_INTEGER,
                    format!(
                        "{member} {:?} must be a signed 64-bit integer without a fraction",
                        bound.as_str()
                    ),
                    at,
                ));
                continue;
            }
            Some(PropertyType::Decimal { scale, .. })
                if u64::try_from(parsed.fraction.len()).map_or(true, |digits| digits > scale.0) =>
            {
                out.push(error(
                    codes::BOUND_EXCEEDS_SCALE,
                    format!(
                        "{member} {:?} has more fraction digits than the declared scale {}",
                        bound.as_str(),
                        scale.0
                    ),
                    at,
                ));
                continue;
            }
            _ => {}
        }
        // The engine's own constructor decides representability, so the range
        // is not restated here.
        if let Err(range_error) = bound.as_str().parse::<norn_expr::Number>() {
            out.push(error(
                codes::BOUND_OUT_OF_RANGE,
                format!(
                    "{member} {:?} is outside the ZEN Expression decimal range: {range_error}",
                    bound.as_str()
                ),
                at,
            ));
            continue;
        }
        *slot = Some(parsed);
    }
    if let [Some(min), Some(max)] = &bounds
        && min > max
    {
        out.push(error(
            codes::INVERTED_RANGE,
            "min is greater than max".to_owned(),
            span_at(base_pointer, &["constraints", "min"]),
        ));
    }

    check_text_constraints(base_pointer, constraints, kind, out);
}

/// `minLength`/`maxLength`/`pattern`: applicability, order, and compilation.
fn check_text_constraints(
    base: &[&str],
    constraints: &PropertyConstraints,
    kind: Option<PropertyKind>,
    out: &mut Vec<Diagnostic>,
) {
    let textual = matches!(kind, None | Some(PropertyKind::Text));
    for (member, present) in [
        ("minLength", constraints.min_length.is_some()),
        ("maxLength", constraints.max_length.is_some()),
        ("pattern", constraints.pattern.is_some()),
    ] {
        if present && !textual {
            out.push(not_applicable(
                member,
                kind,
                span_at(base, &["constraints", member]),
            ));
        }
    }
    if textual {
        if let (Some(min), Some(max)) = (constraints.min_length, constraints.max_length)
            && min > max
        {
            out.push(error(
                codes::INVERTED_RANGE,
                "minLength is greater than maxLength".to_owned(),
                span_at(base, &["constraints", "minLength"]),
            ));
        }
        if let Some(pattern) = &constraints.pattern
            && let Err(compile_error) = regex_lite::RegexBuilder::new(pattern)
                .size_limit(PATTERN_SIZE_LIMIT)
                .build()
        {
            out.push(error(
                codes::INVALID_PATTERN,
                format!("pattern {pattern:?} does not compile: {compile_error}"),
                span_at(base, &["constraints", "pattern"]),
            ));
        }
    }
}

fn not_applicable(member: &str, kind: Option<PropertyKind>, at: Span) -> Diagnostic {
    let kind = kind.map_or("unknown", PropertyKind::as_str);
    error(
        codes::CONSTRAINT_NOT_APPLICABLE,
        format!("constraint {member} does not apply to a {kind} property"),
        at,
    )
}

/// An exact decimal literal split into sign and digit strings, ordered by
/// numeric value.
///
/// Hand-rolled on purpose: bounds are arbitrary-precision literals that only
/// need grammar and order, and fixed-width decimal crates (`rust_decimal`)
/// would reject valid literals beyond 28 digits.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Decimal<'a> {
    negative: bool,
    /// Integer digits without leading zeros (`"0"` for zero).
    integer: &'a str,
    /// Fraction digits as written (may be empty or end in zeros).
    fraction: &'a str,
}

impl<'a> Decimal<'a> {
    fn parse(literal: &'a DecimalLiteral) -> Option<Self> {
        Self::parse_str(literal.as_str())
    }

    /// Parse an exact decimal literal ([`DECIMAL_LITERAL_PATTERN`]).
    pub(crate) fn parse_str(text: &'a str) -> Option<Self> {
        if !matches_grammar(&DECIMAL_GRAMMAR, text) {
            return None;
        }
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text), |rest| (true, rest));
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let mut decimal = Self {
            negative,
            integer,
            fraction,
        };
        // Negative zero is zero.
        decimal.negative &= !decimal.is_zero();
        Some(decimal)
    }

    /// Fraction digits as written, including trailing zeros.
    pub(crate) const fn fraction(&self) -> &'a str {
        self.fraction
    }

    fn significant_fraction(&self) -> &'a str {
        self.fraction.trim_end_matches('0')
    }

    fn is_zero(&self) -> bool {
        self.integer == "0" && self.significant_fraction().is_empty()
    }

    fn cmp_magnitude(&self, other: &Self) -> std::cmp::Ordering {
        // Integer parts have no leading zeros, so length orders them first;
        // trailing-zero-free fraction digit strings order lexicographically.
        self.integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(other.integer))
            .then_with(|| {
                self.significant_fraction()
                    .cmp(other.significant_fraction())
            })
    }
}

impl PartialOrd for Decimal<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.negative, other.negative) {
            (false, true) => std::cmp::Ordering::Greater,
            (true, false) => std::cmp::Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }
}

pub(crate) fn matches_grammar(grammar: &LazyLock<Option<regex_lite::Regex>>, text: &str) -> bool {
    // The grammars are crate constants and always compile; fail closed if not.
    grammar.as_ref().is_some_and(|regex| regex.is_match(text))
}

fn span(index: usize, segments: &[&str]) -> Span {
    let index = index.to_string();
    span_at(&[index.as_str()], segments)
}

/// A JSON-pointer span: `base` followed by `segments`.
pub(crate) fn span_at(base: &[&str], segments: &[&str]) -> Span {
    Span::from_segments(base.iter().chain(segments).copied())
}

pub(crate) fn error(code: &str, message: String, at: Span) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(at),
        artifact_id: None,
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(text: &str) -> DecimalLiteral {
        DecimalLiteral(text.to_owned())
    }

    #[test]
    fn decimal_literals_order_by_value() {
        let ordered = [
            "-100", "-12.5", "-12.49", "-0.001", "0", "0.0001", "0.1", "0.12", "1", "9.99", "10",
        ];
        let literals: Vec<DecimalLiteral> = ordered.iter().map(|text| decimal(text)).collect();
        let parsed: Vec<Decimal<'_>> = literals
            .iter()
            .map(|literal| Decimal::parse(literal).expect("valid literal"))
            .collect();
        for pair in parsed.windows(2) {
            if let [lower, upper] = pair {
                assert!(lower < upper, "{lower:?} < {upper:?}");
            }
        }
        let zero = decimal("0");
        let negative_zero = decimal("-0.000");
        assert_eq!(
            Decimal::parse(&zero)
                .map(|value| value.cmp(&Decimal::parse(&negative_zero).expect("valid"))),
            Some(std::cmp::Ordering::Equal)
        );
        let a = decimal("1.50");
        let b = decimal("1.5");
        assert_eq!(
            Decimal::parse(&a)
                .expect("valid")
                .cmp(&Decimal::parse(&b).expect("valid")),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn decimal_grammar_rejects_non_literals() {
        for text in ["", "-", "01", "1.", ".5", "+1", "1e3", "1.2.3", " 1", "NaN"] {
            assert!(
                Decimal::parse(&decimal(text)).is_none(),
                "{text:?} is rejected"
            );
        }
    }
}
