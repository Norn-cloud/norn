//! VAL-UNIT / VAL-RATE: bounded numeric dimensions, without implicit conversion.
use std::collections::{BTreeMap, BTreeSet};

use crate::{RecordType, Type, ZenTypeEnv, env::TypeEnvVersion};
use norn_ir::canonical::{CanonicalJsonError, JsonValue};
use norn_ir::{InterfaceDef, PropertyType, UnitRef, metric::DimensionPath};
use zen_expression::parser::Node;

mod algebra;
pub(super) use algebra::{
    BoundScope, Operation, admit, admit_rounding, admit_sum, destination, join,
};
pub(super) type Operand = algebra::Operand<UnitRef, String>;

pub(super) fn literal(value: &crate::Number) -> Operand {
    if value.as_str() == "0" {
        Operand::LiteralZero
    } else {
        Operand::Literal
    }
}

pub(super) fn bound_operand(
    owner: Option<&str>,
    instance: Option<Vec<String>>,
    group: &str,
) -> Operand {
    Operand::Bound(owner.zip(instance).map(|(owner, instance)| BoundScope {
        owner: owner.to_owned(),
        instance,
        group: group.to_owned(),
    }))
}

pub(super) fn decimal_binding_group(mut ty: &Type) -> Option<&str> {
    while let Type::Optional(inner) = ty {
        ty = inner;
    }
    match ty {
        Type::DecimalBinding { group } => Some(group.as_str()),
        Type::Bool
        | Type::Text { .. }
        | Type::Integer
        | Type::Decimal { .. }
        | Type::Number
        | Type::Timestamp { .. }
        | Type::Date
        | Type::Duration
        | Type::Enum { .. }
        | Type::Json
        | Type::Record(_)
        | Type::List(_)
        | Type::Optional(_) => None,
    }
}

pub(super) fn instance_path(
    node: &Node<'_>,
    resolve_alias: &impl Fn(&str) -> Option<usize>,
    innermost_scope: Option<usize>,
) -> Option<Vec<String>> {
    match node {
        Node::Parenthesized(inner) => instance_path(inner, resolve_alias, innermost_scope),
        Node::Identifier(name) => Some(vec![match resolve_alias(name) {
            Some(id) => format!("$scope:{id}"),
            None => (*name).to_owned(),
        }]),
        Node::Pointer => innermost_scope.map(|id| vec![format!("$scope:{id}")]),
        Node::Member { node, property } => {
            let Node::String(key) = property else {
                return None;
            };
            let mut path = instance_path(node, resolve_alias, innermost_scope)?;
            path.push((*key).to_owned());
            Some(path)
        }
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Array(_)
        | Node::Object(_)
        | Node::Assignments { .. }
        | Node::Closure { .. }
        | Node::Root
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Conditional { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => None,
    }
}

pub(super) fn strip_scope_operand(operand: &mut Operand, scope_id: usize) {
    let tag = format!("$scope:{scope_id}");
    if let Operand::Bound(Some(bound)) = operand
        && bound.instance.first().map(String::as_str) == Some(tag.as_str())
    {
        *operand = Operand::Bound(None);
    }
}

pub(crate) fn collect_decimal_binding_owners(
    interfaces: &BTreeMap<&str, &InterfaceDef>,
    intersections: &BTreeMap<String, BTreeSet<String>>,
    records: &BTreeMap<String, RecordType>,
) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    for (name, def) in interfaces {
        let Ok(lineage) =
            crate::env::interface_lineage(def, |parent| interfaces.get(parent.as_str()).copied())
        else {
            continue;
        };
        let mut owners: BTreeMap<&str, Option<&str>> = BTreeMap::new();
        for definition in lineage {
            for property in &definition.properties {
                let next = matches!(property.property_type, PropertyType::DecimalBinding { .. })
                    .then_some(definition.name.as_str());
                owners
                    .entry(property.id.as_str())
                    .and_modify(|existing| {
                        if *existing != next {
                            *existing = None;
                        }
                    })
                    .or_insert(next);
            }
        }
        for (field, declaring) in owners {
            if let Some(declaring) = declaring {
                out.insert(((*name).to_owned(), field.to_owned()), declaring.to_owned());
            }
        }
    }
    for (inter_name, members) in intersections {
        let mut owners: BTreeMap<&str, Option<String>> = BTreeMap::new();
        for member in members {
            let Some(record) = records.get(member) else {
                continue;
            };
            for field in record.fields.keys() {
                let next = out.get(&(member.clone(), field.clone())).cloned();
                owners
                    .entry(field.as_str())
                    .and_modify(|existing| {
                        if *existing != next {
                            *existing = None;
                        }
                    })
                    .or_insert(next);
            }
        }
        for (field, declaring) in owners {
            if let Some(declaring) = declaring {
                out.insert((inter_name.clone(), field.to_owned()), declaring);
            }
        }
    }
    out
}

pub(crate) fn check_v1_canonical_compatibility(
    records: &BTreeMap<String, RecordType>,
    variables: &BTreeMap<String, Type>,
    owners: &BTreeMap<(String, String), String>,
) -> Result<(), CanonicalJsonError> {
    let is_rate = |mut ty: &Type| {
        while let Type::List(inner) | Type::Optional(inner) = ty {
            ty = inner;
        }
        matches!(
            ty,
            Type::Decimal {
                per_unit: Some(_),
                ..
            }
        )
    };
    let has_rate = variables.values().any(is_rate)
        || records
            .values()
            .flat_map(|record| record.fields.values())
            .any(|field| is_rate(&field.ty));
    if has_rate {
        return Err(CanonicalJsonError::Serialize {
            message: "Rate requires norn.expr.env/2".to_owned(),
        });
    }
    let is_binding = |mut ty: &Type| {
        while let Type::List(inner) | Type::Optional(inner) = ty {
            ty = inner;
        }
        matches!(ty, Type::DecimalBinding { .. })
    };
    let has_binding = !owners.is_empty()
        || variables.values().any(is_binding)
        || records
            .values()
            .flat_map(|record| record.fields.values())
            .any(|field| is_binding(&field.ty));
    if has_binding {
        return Err(CanonicalJsonError::Serialize {
            message: "DecimalBinding requires norn.expr.env/2".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn with_decimal_binding_owners(
    mut value: JsonValue,
    version: TypeEnvVersion,
    owners: &BTreeMap<(String, String), String>,
) -> JsonValue {
    if version != TypeEnvVersion::V3 || owners.is_empty() {
        return value;
    }
    let mut by_record: BTreeMap<String, BTreeMap<String, JsonValue>> = BTreeMap::new();
    for ((record, field), owner) in owners {
        by_record
            .entry(record.clone())
            .or_default()
            .insert(field.clone(), JsonValue::String(owner.clone()));
    }
    let serialized = JsonValue::Object(
        by_record
            .into_iter()
            .map(|(record, fields)| (record, JsonValue::Object(fields)))
            .collect(),
    );
    if let JsonValue::Object(map) = &mut value {
        map.insert("decimalBindingOwners".to_owned(), serialized);
    }
    value
}

pub(super) fn exact_type(mut ty: &Type) -> bool {
    while let Type::Optional(inner) | Type::List(inner) = ty {
        ty = inner;
    }
    matches!(ty, Type::Decimal { .. } | Type::DecimalBinding { .. })
}

pub(super) fn expected_exact_type<'a>(ty: &'a Type, env: &'a ZenTypeEnv) -> bool {
    let mut pending = vec![ty];
    let mut visited = BTreeSet::new();
    while let Some(ty) = pending.pop() {
        if exact_type(ty) {
            return true;
        }
        let mut names = Vec::new();
        ty.records(&mut names);
        for name in names {
            if visited.insert(name.clone())
                && let Some(record) = env.record(&name)
            {
                pending.extend(record.fields.values().map(|field| &field.ty));
            }
        }
    }
    false
}

pub(crate) fn decimal_destination(mut ty: &Type) -> Option<PropertyType> {
    while let Type::Optional(inner) = ty {
        ty = inner;
    }
    let Type::Decimal {
        scale,
        unit,
        rounding,
        currency_source,
        per_unit,
    } = ty
    else {
        return None;
    };
    Some(PropertyType::Decimal {
        additivity: norn_ir::PropertyAdditivity::Undeclared {},
        scale: norn_ir::WholeNumber(*scale),
        unit: unit.clone(),
        rounding: Some(*rounding),
        currency_source: currency_source.clone(),
        per_unit: per_unit.clone(),
        unit_source: Vec::new(),
    })
}

pub(crate) fn validate_exact_environment(env: &ZenTypeEnv, prefix: &str) -> Result<(), String> {
    for (name, record) in env
        .records()
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
    {
        for field in record.fields.values() {
            let mut ty = &field.ty;
            while let Type::Optional(inner) | Type::List(inner) = ty {
                ty = inner;
            }
            if let Type::Decimal {
                unit,
                currency_source,
                per_unit,
                ..
            } = ty
                && matches!(
                    declared(
                        env,
                        Some(name),
                        unit.as_ref(),
                        currency_source.as_ref(),
                        per_unit.as_ref()
                    ),
                    Operand::Unresolved
                )
            {
                return Err("exact currency requires a fixed singleton currencySource and compatible Decimal bindings".into());
            }
        }
    }
    Ok(())
}

pub(super) fn declared(
    env: &ZenTypeEnv,
    owner: Option<&str>,
    unit: Option<&UnitRef>,
    source: Option<&DimensionPath>,
    per: Option<&UnitRef>,
) -> Operand {
    match (unit, source, per) {
        (Some(u), None, None) => Operand::Quantity(u.clone()),
        (None, None, None) => Operand::Dimensionless,
        (None, Some(path), per) => currency(env, owner, path).map_or(Operand::Unresolved, |c| {
            per.map_or_else(
                || Operand::Money(c.clone()),
                |u| Operand::Rate(c.clone(), u.clone()),
            )
        }),
        _ => Operand::Unresolved,
    }
}
fn currency(env: &ZenTypeEnv, owner: Option<&str>, path: &DimensionPath) -> Option<String> {
    fn record_currency<'a>(
        env: &'a ZenTypeEnv,
        mut record: &'a str,
        path: &DimensionPath,
    ) -> Option<String> {
        for link in &path.links {
            let Type::Record(next) = &env.record(record)?.fields.get(link.as_str())?.ty else {
                return None;
            };
            record = next;
        }
        let Type::Enum { values } = &env.record(record)?.fields.get(path.property.as_str())?.ty
        else {
            return None;
        };
        let [code] = values.as_slice() else {
            return None;
        };
        (code.len() == 3 && code.bytes().all(|c| c.is_ascii_uppercase())).then(|| code.clone())
    }
    if let Some(owner) = owner {
        return record_currency(env, owner, path);
    }
    let mut candidates = Vec::new();
    if path.links.is_empty()
        && let Some(Type::Enum { values }) = env.variables().get(path.property.as_str())
    {
        let [code] = values.as_slice() else {
            return None;
        };
        candidates.push(code.clone());
    }
    for name in ["input", "self"] {
        if let Some(Type::Record(record)) = env.variables().get(name)
            && let Some(c) = record_currency(env, record, path)
        {
            candidates.push(c);
        }
    }
    let first = candidates.first()?;
    candidates.iter().all(|c| c == first).then(|| first.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grams() -> UnitRef {
        UnitRef {
            system: norn_ir::UnitSystem::Ucum,
            code: "g".into(),
        }
    }
    #[test]
    fn val_rate_matches_currency_and_denominator_and_refuses_other_compounds() {
        let quantity = Operand::Quantity(grams());
        let usd = Operand::Money("USD".into());
        let rate = Operand::Rate("USD".into(), grams());
        assert_eq!(admit(Operation::Multiply, &rate, &quantity).unwrap().0, usd);
        assert_eq!(admit(Operation::Divide, &usd, &quantity).unwrap().0, rate);
        assert!(admit(Operation::Multiply, &rate, &rate).is_none());
        assert!(admit(Operation::Add, &rate, &Operand::Rate("EUR".into(), grams())).is_none());
        assert!(
            admit(
                Operation::Multiply,
                &rate,
                &Operand::Quantity(UnitRef {
                    code: "mg".into(),
                    ..grams()
                })
            )
            .is_none()
        );
        assert!(admit(Operation::Add, &quantity, &Operand::Dimensionless).is_none());
        assert_eq!(
            admit(Operation::Compare, &quantity, &Operand::Literal)
                .unwrap()
                .1,
            Some(grams())
        );
        assert_eq!(
            admit(Operation::Compare, &quantity, &Operand::LiteralZero)
                .unwrap()
                .1,
            Some(grams())
        );
        assert!(
            admit(
                Operation::Compare,
                &Operand::Unresolved,
                &Operand::LiteralZero
            )
            .is_none()
        );
        assert!(admit_sum(&Operand::Unresolved).is_none());
        assert!(admit_rounding(&Operand::Unresolved).is_none());
        assert_eq!(admit_sum(&quantity), Some(quantity.clone()));
        assert_eq!(admit_sum(&usd), Some(usd.clone()));
        assert_eq!(admit_sum(&rate), Some(rate.clone()));
        assert_eq!(admit_rounding(&quantity), Some(quantity));
        assert_eq!(admit_rounding(&usd), Some(usd));
        assert_eq!(admit_rounding(&rate), Some(rate));
        assert_eq!(
            admit_rounding(&Operand::Dimensionless),
            Some(Operand::Dimensionless)
        );
        assert_eq!(admit_rounding(&Operand::Literal), Some(Operand::Literal));
        assert_eq!(
            admit_rounding(&Operand::LiteralZero),
            Some(Operand::LiteralZero)
        );
        assert_eq!(admit_rounding(&Operand::Legacy), Some(Operand::Legacy));
    }

    #[test]
    fn bound_scope_enforces_owner_instance_group_sum_and_rounding() {
        let quantity = Operand::Quantity(grams());
        let bound_conv_self = Operand::Bound(Some(BoundScope {
            owner: "core.Conversion".into(),
            instance: vec!["self".into()],
            group: "conversion".into(),
        }));
        let bound_conv_other_instance = Operand::Bound(Some(BoundScope {
            owner: "core.Conversion".into(),
            instance: vec!["self".into(), "other".into()],
            group: "conversion".into(),
        }));
        let bound_conv_other_owner = Operand::Bound(Some(BoundScope {
            owner: "other.Conversion".into(),
            instance: vec!["self".into()],
            group: "conversion".into(),
        }));
        let bound_conv_other_group = Operand::Bound(Some(BoundScope {
            owner: "core.Conversion".into(),
            instance: vec!["self".into()],
            group: "volume".into(),
        }));
        let bound_unknown = Operand::Bound(None);

        assert_eq!(
            admit(Operation::Add, &bound_conv_self, &bound_conv_self),
            Some((bound_conv_self.clone(), None))
        );
        assert_eq!(
            admit(Operation::Subtract, &bound_conv_self, &bound_conv_self),
            Some((bound_conv_self.clone(), None))
        );
        assert_eq!(
            admit(Operation::Compare, &bound_conv_self, &bound_conv_self),
            Some((Operand::Dimensionless, None))
        );
        assert_eq!(
            join(&bound_conv_self, &bound_conv_self),
            Some(bound_conv_self.clone())
        );
        assert!(destination(&bound_conv_self, &bound_conv_self));
        assert!(admit(Operation::Multiply, &bound_conv_self, &bound_conv_self).is_none());
        assert!(admit(Operation::Divide, &bound_conv_self, &bound_conv_self).is_none());

        for bound in [&bound_conv_self, &bound_unknown] {
            assert_eq!(
                admit(Operation::Compare, bound, &Operand::LiteralZero),
                Some((Operand::Dimensionless, None))
            );
            assert_eq!(
                admit(Operation::Compare, &Operand::LiteralZero, bound),
                Some((Operand::Dimensionless, None))
            );
            assert!(admit(Operation::Compare, bound, &Operand::Literal).is_none());
            assert!(admit(Operation::Compare, bound, &Operand::Dimensionless).is_none());
            assert!(admit(Operation::Compare, bound, &quantity).is_none());
            assert!(admit(Operation::Add, bound, &Operand::LiteralZero).is_none());
            assert!(admit(Operation::Subtract, bound, &Operand::LiteralZero).is_none());
            assert!(admit(Operation::Multiply, bound, &Operand::LiteralZero).is_none());
            assert!(admit(Operation::Divide, bound, &Operand::Literal).is_none());
            assert!(join(bound, &Operand::LiteralZero).is_none());
            assert!(join(&Operand::LiteralZero, bound).is_none());
            assert!(!destination(&Operand::LiteralZero, bound));
            assert!(!destination(&Operand::Literal, bound));
            assert!(!destination(bound, &Operand::Dimensionless));
            assert!(!destination(bound, &Operand::Legacy));
            assert!(!destination(bound, &quantity));
            assert!(admit_sum(bound).is_none());
            assert!(admit_rounding(bound).is_none());
        }
        for (a, b) in [
            (&bound_conv_self, &bound_conv_other_instance),
            (&bound_conv_self, &bound_conv_other_owner),
            (&bound_conv_self, &bound_conv_other_group),
            (&bound_conv_self, &bound_unknown),
            (&bound_unknown, &bound_unknown),
        ] {
            for op in [
                Operation::Add,
                Operation::Subtract,
                Operation::Multiply,
                Operation::Divide,
                Operation::Compare,
            ] {
                assert!(
                    admit(op, a, b).is_none(),
                    "{op:?} must refuse {a:?} & {b:?}"
                );
            }
            assert!(join(a, b).is_none());
            assert!(!destination(a, b));
        }
    }
}
