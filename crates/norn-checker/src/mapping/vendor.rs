//! Pure sparse vendor-pack and tenant discovery admission. These checks do not
//! authenticate discovery: the server must obtain it from its own perform edge.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use norn_ir::mapping::{EntitlementConditionMap, MappingIr, VendorMappingContract};
use norn_ir::source_schema::{ConstraintKind, SourceSchemaIr};

use crate::{Diagnostic, Span};

/// Explicit generic registry qualification, with no vendor parity claim.
pub const GENERIC_MAPPING_SLOT: &str = "norn.mapping.generic/1.0.0";

/// Reserved typed extension for exact vendor qualification.
pub const VENDOR_MAPPING_SLOT: &str = "norn.mapping.vendor/1.0.0";

/// Decode qualification without treating absent generic qualification as parity.
///
/// # Errors
/// A reserved extension is malformed or names an unsupported contract version.
pub fn vendor_contract(mapping: &MappingIr) -> Result<Option<VendorMappingContract>, String> {
    let Some(extensions) = &mapping.extensions else {
        return Ok(None);
    };
    for key in extensions.keys() {
        if (key.starts_with("norn.mapping.vendor/") && key != VENDOR_MAPPING_SLOT)
            || (key.starts_with("norn.mapping.generic/") && key != GENERIC_MAPPING_SLOT)
        {
            return Err("unsupported vendor mapping contract version".into());
        }
    }
    if extensions.contains_key(VENDOR_MAPPING_SLOT) && extensions.contains_key(GENERIC_MAPPING_SLOT)
    {
        return Err("mapping cannot be both generic and vendor-qualified".into());
    }
    if let Some(value) = extensions.get(GENERIC_MAPPING_SLOT) {
        serde_json::from_value::<norn_ir::mapping::GenericMappingContract>(
            serde_json::to_value(value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    }
    extensions
        .get(VENDOR_MAPPING_SLOT)
        .map(|value| {
            serde_json::from_value(serde_json::to_value(value).map_err(|error| error.to_string())?)
                .map_err(|error| format!("invalid vendor mapping contract: {error}"))
        })
        .transpose()
}

static API_REVISION: LazyLock<Option<regex_lite::Regex>> = LazyLock::new(|| {
    regex_lite::Regex::new(r"^([0-9]{4}-(0[1-9]|1[0-2])|v[1-9][0-9]*\.[0-9]+)$").ok()
});

fn exact_version(version: &str) -> bool {
    super::set::SEMVER
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(version))
        || API_REVISION
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(version))
        || ([40, 64].contains(&version.len())
            && version
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

fn contract_valid(contract: &VendorMappingContract) -> bool {
    !contract.system.trim().is_empty()
        && !contract.versions.is_empty()
        && contract
            .versions
            .iter()
            .all(|version| exact_version(version))
        && contract.versions.iter().collect::<BTreeSet<_>>().len() == contract.versions.len()
        && contract.api_terms.uri.starts_with("https://")
        && contract.api_terms.sha256.len() == 64
        && contract
            .api_terms
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

type Fields = BTreeSet<(String, String)>;

fn reads(
    fields: &mut Fields,
    dataset: &str,
    expression: &norn_ir::ZenExpression,
) -> Result<(), String> {
    let read = norn_expr::expression_reads(expression.as_str(), "input")
        .map_err(|error| format!("invalid source expression: {error:?}"))?;
    if read.whole_root {
        return Err("vendor mappings must name source fields explicitly".into());
    }
    fields.extend(
        read.fields
            .into_iter()
            .map(|field| (dataset.to_owned(), field)),
    );
    Ok(())
}

fn used_fields(mapping: &MappingIr, schema: &SourceSchemaIr) -> Result<Fields, String> {
    let mut fields = Fields::new();
    for map in &mapping.maps {
        for expression in
            std::iter::once(&map.subject)
                .chain(std::iter::once(&map.source_record))
                .chain(map.when.iter())
                .chain(map.links.iter().map(|link| &link.key))
                .chain(map.lifecycles.iter().flat_map(|lifecycle| {
                    lifecycle.state.iter().chain(lifecycle.transition.iter())
                }))
        {
            reads(&mut fields, &map.source.dataset, expression)?;
        }
        if let Some(parent) = &map.source.nested_in {
            fields.insert((parent.dataset.clone(), parent.field.clone()));
            relationship_fields(
                &mut fields,
                schema,
                &map.source.dataset,
                &parent.dataset,
                &parent.field,
            );
        }
        for property in &map.properties {
            if let Some(nested) = &property.nested {
                fields.insert((map.source.dataset.clone(), nested.field.clone()));
                relationship_fields(
                    &mut fields,
                    schema,
                    &nested.dataset,
                    &map.source.dataset,
                    &nested.field,
                );
                reads(&mut fields, &nested.dataset, &property.value)?;
                reads(&mut fields, &nested.dataset, &nested.predicate)?;
            } else {
                reads(&mut fields, &map.source.dataset, &property.value)?;
            }
        }
    }
    for (index, map) in mapping.entitlements.iter().enumerate() {
        entitlement_fields(&mut fields, map, index)?;
    }
    Ok(fields)
}

fn entitlement_fields(
    fields: &mut Fields,
    map: &norn_ir::mapping::EntitlementMap,
    index: usize,
) -> Result<(), String> {
    let aliases = std::iter::once((map.source_alias.as_str(), map.source.dataset.as_str()))
        .chain(
            map.joins
                .iter()
                .map(|join| (join.alias.as_str(), join.dataset.as_str())),
        )
        .collect::<BTreeMap<_, _>>();
    let mut path = |alias: &str, field: &str| -> Result<(), String> {
        let dataset = aliases
            .get(alias)
            .ok_or("unknown entitlement source alias")?;
        fields.insert(((*dataset).to_owned(), field.to_owned()));
        Ok(())
    };
    path(&map.source_alias, &map.source_record)?;
    path(&map.source_alias, &map.source_version)?;
    path(&map.grantee.key.alias, &map.grantee.key.field)?;
    if let EntitlementConditionMap::LinkedRecord { target_key, .. } = &map.condition {
        path(&target_key.alias, &target_key.field)?;
    }
    for join in &map.joins {
        for key in &join.on {
            path(&key.left.alias, &key.left.field)?;
            path(&join.alias, &key.right)?;
        }
        for field in join.record_id.iter().chain(&join.version) {
            path(&join.alias, field)?;
        }
    }
    for (expression, _) in super::entitlement_expressions(map, index) {
        let read = norn_expr::expression_reads(expression.as_str(), "input")
            .map_err(|error| format!("invalid entitlement expression: {error:?}"))?;
        if read.whole_root {
            return Err("vendor entitlement mappings must name source fields explicitly".into());
        }
        for field in read.fields {
            let matches = aliases
                .iter()
                .filter_map(|(alias, dataset)| {
                    field
                        .strip_prefix(&format!("{alias}_"))
                        .map(|name| ((*dataset).to_owned(), name.to_owned()))
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err("ambiguous entitlement source field".into());
            }
            fields.extend(matches);
        }
    }
    Ok(())
}

fn relationship_fields(
    fields: &mut Fields,
    schema: &SourceSchemaIr,
    child: &str,
    parent: &str,
    collection: &str,
) {
    for constraint in &schema.constraints {
        if constraint.kind == ConstraintKind::ParentChild
            && constraint.dataset == child
            && constraint.collection.as_deref() == Some(collection)
            && constraint
                .target
                .as_ref()
                .is_some_and(|target| target.dataset == parent)
        {
            fields.extend(
                constraint
                    .fields
                    .iter()
                    .map(|field| (child.to_owned(), field.clone())),
            );
            if let Some(target) = &constraint.target {
                fields.extend(
                    target
                        .fields
                        .iter()
                        .map(|field| (parent.to_owned(), field.clone())),
                );
            }
        }
    }
}

fn sparse(mapping: &MappingIr, schema: &SourceSchemaIr) -> Result<(), String> {
    if mapping.extensions.as_ref().is_some_and(|extensions| {
        extensions.keys().any(|key| {
            key != VENDOR_MAPPING_SLOT && key != norn_ir::mapping_evidence::EVIDENCE_SLOT
        })
    }) {
        return Err("vendor mapping packs cannot carry untyped extension payloads".into());
    }
    let used = used_fields(mapping, schema)?;
    let declared = schema
        .fields
        .iter()
        .map(|field| (field.dataset.clone(), field.name.clone()))
        .collect::<Fields>();
    if used != declared {
        return Err(
            "vendor source requirements must contain exactly the explicitly mapped fields".into(),
        );
    }
    let datasets = used
        .iter()
        .map(|(dataset, _)| dataset.as_str())
        .collect::<BTreeSet<_>>();
    if schema
        .datasets
        .iter()
        .map(|dataset| dataset.id.as_str())
        .collect::<BTreeSet<_>>()
        != datasets
        || !schema.profiles.is_empty()
        || !schema.code_sets.is_empty()
        || !schema.lineage.is_empty()
        || schema.extensions.is_some()
        || schema
            .datasets
            .iter()
            .any(|dataset| dataset.description.is_some())
        || schema
            .fields
            .iter()
            .any(|field| field.description.is_some())
        || mapping
            .maps
            .iter()
            .any(|map| !map.ignored_fields.is_empty())
        || schema.constraints.iter().any(|constraint| {
            constraint.kind == ConstraintKind::Check || constraint.expression.is_some()
        })
    {
        return Err("vendor mapping requirements cannot carry full schema metadata, profiles, code sets or ignored fields".into());
    }
    Ok(())
}

/// Validate the authored sparse requirements at ordinary checker admission.
#[must_use]
pub fn check_vendor_mapping(mapping: &MappingIr, schema: &SourceSchemaIr) -> Vec<Diagnostic> {
    let result = (|| {
        let Some(contract) = vendor_contract(mapping)? else {
            return Ok(());
        };
        if !contract_valid(&contract)
            || contract.system != schema.source.system
            || schema
                .source
                .version
                .as_ref()
                .is_some_and(|version| !contract.versions.contains(version))
        {
            return Err("invalid exact vendor versions, system or API-terms evidence".into());
        }
        sparse(mapping, schema)
    })();
    result
        .err()
        .into_iter()
        .map(|reason: String| {
            super::error(
                "mapping.vendor.invalid_contract",
                reason,
                Span::pointer("/extensions"),
            )
        })
        .collect()
}

/// Check trusted tenant discovery against every authored sparse requirement.
/// Extra fields remain in the tenant discovery and never enter the pack.
///
/// # Errors
/// Version, identity, type, nullability, cardinality or source identity differs.
pub fn validate_discovered_schema(
    mapping: &MappingIr,
    expected: &SourceSchemaIr,
    discovered: &SourceSchemaIr,
) -> Result<(), String> {
    let Some(contract) = vendor_contract(mapping)? else {
        return Ok(());
    };
    if !check_vendor_mapping(mapping, expected).is_empty() {
        return Err("invalid sparse vendor mapping contract".into());
    }
    if !crate::source_schema::check_source_schema(discovered).is_empty() {
        return Err("tenant discovery schema is malformed".into());
    }
    if discovered.source.system != expected.source.system
        || discovered.source.instance != expected.source.instance
    {
        return Err("tenant discovery belongs to another source".into());
    }
    if !discovered
        .source
        .version
        .as_ref()
        .is_some_and(|version| contract.versions.contains(version))
    {
        return Err("missing or unsupported exact vendor version".into());
    }
    for dataset in &expected.datasets {
        if !discovered.datasets.iter().any(|actual| {
            actual.id == dataset.id
                && actual.native_name == dataset.native_name
                && actual.namespace == dataset.namespace
                && actual.kind == dataset.kind
        }) {
            return Err(format!(
                "mapped dataset {} changed identity or cardinality",
                dataset.id
            ));
        }
    }
    for field in &expected.fields {
        if !discovered.fields.iter().any(|actual| {
            actual.dataset == field.dataset
                && actual.name == field.name
                && actual.native_type == field.native_type
                && actual.type_candidate == field.type_candidate
                && actual.required == field.required
                && actual.markings == field.markings
        }) {
            return Err(format!(
                "mapped field {}.{} is missing or incompatible",
                field.dataset, field.name
            ));
        }
    }
    for constraint in &expected.constraints {
        if !discovered.constraints.iter().any(|actual| {
            actual.kind == constraint.kind
                && actual.dataset == constraint.dataset
                && actual.fields == constraint.fields
                && actual.target == constraint.target
                && actual.collection == constraint.collection
                && actual.origin == constraint.origin
        }) {
            return Err(format!(
                "mapped identity or relationship {} changed",
                constraint.id
            ));
        }
    }
    // Newly introduced keys/relationships over mapped fields also change the
    // source contract, even if the author's required constraint still exists.
    let mapped = expected
        .fields
        .iter()
        .map(|field| (field.dataset.clone(), field.name.clone()))
        .collect::<Fields>();
    for actual in &discovered.constraints {
        if actual
            .fields
            .iter()
            .all(|field| mapped.contains(&(actual.dataset.clone(), field.clone())))
            && !expected.constraints.iter().any(|constraint| {
                actual.kind == constraint.kind
                    && actual.dataset == constraint.dataset
                    && actual.fields == constraint.fields
                    && actual.target == constraint.target
                    && actual.collection == constraint.collection
                    && actual.origin == constraint.origin
            })
        {
            return Err("mapped field identity or relationship constraints changed".into());
        }
    }
    Ok(())
}
