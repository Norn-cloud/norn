//! PACK-BIND-1: exact, complete type/unit substitution within a scoped instance.
use crate::{EffectiveCatalog, EffectiveKind};
use norn_ir::definition_catalog::{ArtifactKey, Member};
use norn_ir::mapping::InterpretationEndpoint;
use norn_ir::{ArtifactEnvelope, CompositionPackRoot, EntityInterfaceRootV2, PackInstance, PropertyType, ScopeRef};
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn payload<T: DeserializeOwned>(artifact: &ArtifactEnvelope) -> Option<T> {
    serde_json::from_value(serde_json::to_value(artifact.payload.to_json_value()).ok()?).ok()
}
fn pinned<'a>(
    pool: &[&'a ArtifactEnvelope],
    pin: &InterpretationEndpoint,
    instance: &PackInstance,
) -> Option<&'a ArtifactEnvelope> {
    let mut found = pool.iter().copied().filter(|a| {
        a.artifact_id == pin.artifact_id && a.version == pin.version
            && crate::scope_conflict(&instance.scope, &a.scope).is_none()
            && crate::ArtifactRef::of(a).is_ok_and(|actual| actual.sha256 == pin.sha256)
    });
    let artifact = found.next()?;
    found.next().is_none().then_some(artifact)
}

/// All returned members share the calling instance's exact retained path.
/// This checks substitutions only; it grants no installation or write authority.
pub(crate) fn targets(
    instance: &PackInstance,
    pool: &[&ArtifactEnvelope],
    catalog: Option<&EffectiveCatalog>,
) -> Result<Vec<ArtifactKey>, &'static str> {
    let pack = pinned(pool, &instance.pack, instance).ok_or("core.composition.unresolved_pin")?;
    let Some(root) = payload::<CompositionPackRoot>(pack) else {
        return if instance.type_unit_bindings.is_empty() {
            Ok(Vec::new())
        } else {
            Err("core.composition.mapping_gap")
        };
    };
    let parameters = parameters(&root, &pack.scope, pool)?;
    if !parameters.keys().eq(instance.type_unit_bindings.keys()) {
        return Err("core.composition.mapping_gap");
    }
    let substitutions = parameters.iter().map(|(name, interface)| {
        let binding = instance.type_unit_bindings.get(name).ok_or("core.composition.mapping_gap")?;
        Ok((interface.interface.name.as_str().to_owned(), binding.type_name.as_str().to_owned()))
    }).collect::<Result<BTreeMap<_, _>, &'static str>>()?;
    if substitutions.len() != parameters.len() { return Err("core.composition.mapping_gap"); }
    let mut out = Vec::new();
    for (name, interface) in parameters {
        let type_name = interface.interface.name.as_str().to_owned();
        let binding = instance.type_unit_bindings.get(&name).ok_or("core.composition.mapping_gap")?;
        let target = pinned(pool, &binding.target, instance).ok_or("core.composition.unresolved_pin")?;
        let effective = catalog.and_then(|c| c.object_type(&target.artifact_id))
            .and_then(Result::ok).ok_or("core.composition.mapping_gap")?;
        if effective.name != binding.type_name
            || !effective.interfaces.iter().any(|i| i.as_str() == type_name)
        {
            return Err("core.composition.mapping_gap");
        }
        let (fields, links) = contracts(interface, pool).ok_or("core.composition.mapping_gap")?;
        let concrete_links = catalog.and_then(|c| c.links_for_object_type(effective.name.as_str())).unwrap_or(&[]);
        for (id, target) in links {
            if let Some(expected) = substitutions.get(target.as_str()) {
                if !concrete_links.iter().any(|link| link.id == id && link.target.as_str() == expected.as_str()) {
                    return Err("core.composition.mapping_gap");
                }
            }
        }
        if !fields.iter().eq(binding.decimals.keys()) {
            return Err("core.composition.mapping_gap");
        }
        for (id, contract) in &binding.decimals {
            let PropertyType::Decimal { unit, currency_source, .. } = contract else {
                return Err("core.values.unit_mismatch");
            };
            if unit.is_some() == currency_source.is_some()
                || !effective.properties.iter().any(|p| p.id.as_str() == id
                    && matches!(&p.kind, EffectiveKind::Base(actual) if actual == contract))
            {
                return Err("core.values.unit_mismatch");
            }
            if let Some(currency) = currency_source {
                if !currency.links.is_empty()
                    || !effective.properties.iter().any(|p| p.id == currency.property
                        && !p.nullable
                        && matches!(&p.kind, EffectiveKind::Base(PropertyType::Enum { values })
                            if values.len() == 1))
                {
                    return Err("core.values.unit_binding_required");
                }
            }
        }
        out.push(ArtifactKey {
            artifact_id: target.artifact_id.clone(),
            version: target.version.clone(),
        });
    }
    Ok(out)
}
pub(crate) fn parameters(
    root: &CompositionPackRoot,
    scope: &ScopeRef,
    pool: &[&ArtifactEnvelope],
) -> Result<BTreeMap<String, EntityInterfaceRootV2>, &'static str> {
    if root.type_parameters.len() > 256 { return Err("core.composition.mapping_gap"); }
    let mut out = BTreeMap::new();
    for (name, source) in &root.type_parameters {
        if name.trim().is_empty() || !root.artifacts.iter().any(|a| a.artifact_id == source.artifact_id
            && a.version == source.version && a.kind == norn_ir::PackArtifactKind::Entity)
        { return Err("core.composition.mapping_gap"); }
        let Member::Type { type_name } = &source.member else { return Err("core.composition.mapping_gap"); };
        let mut sources = pool.iter().copied().filter(|a| a.artifact_id == source.artifact_id
            && a.version == source.version && crate::scope_conflict(scope, &a.scope).is_none());
        let interface: EntityInterfaceRootV2 = sources.next().and_then(payload).ok_or("core.composition.unresolved_pin")?;
        if sources.next().is_some() || interface.interface.name.as_str() != type_name { return Err("core.composition.mapping_gap"); }
        out.insert(name.clone(), interface);
    }
    Ok(out)
}
fn contracts(
    first: EntityInterfaceRootV2,
    pool: &[&ArtifactEnvelope],
) -> Option<(BTreeSet<String>, Vec<(norn_ir::LinkId, norn_ir::ObjectTypeName)>)> {
    let mut links = Vec::new();
    let mut pending = vec![first.interface];
    let mut seen = BTreeSet::new();
    let mut fields = BTreeSet::new();
    let interfaces: BTreeMap<_, _> = pool.iter().filter_map(|a| {
        payload::<EntityInterfaceRootV2>(a).map(|root| (root.interface.name.clone(), root.interface))
    }).collect();
    while let Some(interface) = pending.pop() {
        if !seen.insert(interface.name.clone()) { continue; }
        if seen.len() > 256 { return None; }
        for property in &interface.properties {
            if matches!(property.property_type, PropertyType::DecimalBinding { .. }) {
                fields.insert(property.id.as_str().to_owned());
            }
        }
        links.extend(interface.links.iter().map(|link| (link.id.clone(), link.target.clone())));
        for parent in &interface.extends {
            pending.push(interfaces.get(parent)?.clone());
        }
    }
    Some((fields, links))
}
