//! Pure OSI/`MetricFlow` interchange and ontology projection (#2627, #3301).
//! No SQL execution or independently authored semantic dataset is admitted.

use std::collections::BTreeMap;

use norn_ir::metric::{DimensionPath, MetricDefinition, MetricNorn, MetricType};
use norn_ir::osi::{
    CustomExtension, Dataset, Dialect, DialectExpression, Dimension, Expression, Field,
    LinkProjection, Metric, MetricIR, OsiVersion, Relationship, SemanticModel, Vendor,
};
use norn_ir::{Multiplicity, ObjectTypeName, PropertyType};
use serde_json::{Value, json};

use crate::entity_instance::EffectiveKind;
use crate::metric::{check_metrics_and_questions, codes, field_name};
use crate::property_types::error;
use crate::{Diagnostic, EffectiveCatalog, Span};

const ID: &str = "_norn_identity";
const FROM: &str = "_from_identity";
const TO: &str = "_to_identity";

fn gap(at: &str, message: impl Into<String>) -> Diagnostic {
    error(codes::IMPORT_GAP, message.into(), Span::pointer(at))
}

fn decode_path(error: &serde_path_to_error::Error<serde_json::Error>, at: &str) -> Diagnostic {
    let segments = error.path().iter().map(|segment| match segment {
        serde_path_to_error::Segment::Seq { index } => index.to_string(),
        serde_path_to_error::Segment::Map { key } => key.clone(),
        serde_path_to_error::Segment::Enum { variant } => variant.clone(),
        serde_path_to_error::Segment::Unknown => "?".into(),
    });
    let suffix = Span::from_segments(segments).pointer;
    gap(&format!("{at}{suffix}"), error.inner().to_string())
}

#[allow(
    clippy::result_large_err,
    reason = "retain the structured boundary diagnostic and exact source path"
)]
fn decode_flow(data: &str, at: &str) -> Result<norn_ir::metric::MetricFlowMetric, Diagnostic> {
    let mut decoder = serde_json::Deserializer::from_str(data);
    let flow =
        serde_path_to_error::deserialize(&mut decoder).map_err(|error| decode_path(&error, at))?;
    decoder.end().map_err(|error| gap(at, error.to_string()))?;
    Ok(flow)
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn expression(text: String) -> Expression {
    Expression {
        dialects: vec![DialectExpression {
            dialect: Dialect::AnsiSql,
            expression: text,
        }],
    }
}
fn field(name: String, time: bool) -> Field {
    Field {
        expression: expression(quoted(&name)),
        name,
        dimension: Some(Dimension { is_time: time }),
        label: None,
        description: None,
        ai_context: None,
        custom_extensions: vec![],
    }
}
fn path_expression(root: &str, path: &DimensionPath) -> String {
    format!("{}.{}", quoted(root), quoted(&field_name(path)))
}

fn metricflow_name(name: &str) -> String {
    if !name.contains('.') && !name.contains("__") && !name.ends_with('_') && name.len() >= 2 {
        return name.into();
    }
    let mut alias = name
        .split('.')
        .map(|component| component.replace('_', "_u"))
        .collect::<Vec<_>>()
        .join("_");
    if alias.len() < 2 {
        alias.push_str("metric");
    }
    alias
}

fn projected_filter(rendered: &str) -> Option<String> {
    rendered
        .split_once(" FILTER (WHERE ")
        .and_then(|(_, filter)| filter.strip_suffix(')'))
        .map(str::to_owned)
}

#[allow(
    clippy::too_many_lines,
    clippy::result_large_err,
    reason = "one closed expression projection with structured source diagnostics, without SQL execution"
)]
fn simple_expression(definition: &MetricDefinition) -> Result<String, Diagnostic> {
    let params = &definition.type_params;
    match definition.metric_type {
        MetricType::Simple => {
            let agg = params.metric_aggregation_params.as_ref().ok_or_else(|| {
                gap(
                    "/type_params/metric_aggregation_params",
                    "Simple metric requires aggregate parameters",
                )
            })?;
            let mut value = params.expr.as_deref().filter(|p| *p != "*").map_or_else(
                || "*".into(),
                |p| format!("{}.{}", quoted(agg.semantic_model.as_str()), quoted(p)),
            );
            if let Some(conversion) = definition
                .norn
                .currency
                .as_ref()
                .and_then(|c| c.conversion.as_ref())
            {
                value = format!(
                    "({value} * {})",
                    path_expression(agg.semantic_model.as_str(), &conversion.rate)
                );
            }
            let operator = match agg.agg {
                norn_ir::metric::MeasureAggregate::Sum => "SUM",
                norn_ir::metric::MeasureAggregate::Count
                | norn_ir::metric::MeasureAggregate::CountDistinct => "COUNT",
                norn_ir::metric::MeasureAggregate::Min => "MIN",
                norn_ir::metric::MeasureAggregate::Max => "MAX",
                norn_ir::metric::MeasureAggregate::Avg => "AVG",
            };
            let distinct = if agg.agg == norn_ir::metric::MeasureAggregate::CountDistinct {
                "DISTINCT "
            } else {
                ""
            };
            let mut result = format!("{operator}({distinct}{value})");
            let predicates: Result<Vec<_>, _> = definition
                .norn
                .default_filters
                .iter()
                .map(|filter| {
                    let value = match &filter.value {
                        norn_ir::RuntimeValue::String(text) => {
                            format!("'{}'", text.replace('\'', "''"))
                        }
                        norn_ir::RuntimeValue::Null => "NULL".into(),
                        norn_ir::RuntimeValue::Bool(value) => {
                            if *value {
                                "TRUE".into()
                            } else {
                                "FALSE".into()
                            }
                        }
                        norn_ir::RuntimeValue::Number(_) => String::from_utf8(
                            filter
                                .value
                                .canonical_json()
                                .map_err(|e| gap("/x-norn/defaultFilters", e.to_string()))?,
                        )
                        .map_err(|e| gap("/x-norn/defaultFilters", e.to_string()))?,
                        norn_ir::RuntimeValue::Array(_) | norn_ir::RuntimeValue::Object(_) => {
                            return Err(gap(
                                "/x-norn/defaultFilters",
                                "Only scalar predicate literals have an OSI projection",
                            ));
                        }
                    };
                    let op = match filter.operator {
                        norn_ir::metric::FilterOperator::Eq => "IS NOT DISTINCT FROM",
                        norn_ir::metric::FilterOperator::Ne => "IS DISTINCT FROM",
                        norn_ir::metric::FilterOperator::Lt => "<",
                        norn_ir::metric::FilterOperator::Le => "<=",
                        norn_ir::metric::FilterOperator::Gt => ">",
                        norn_ir::metric::FilterOperator::Ge => ">=",
                    };
                    Ok(format!(
                        "{} {op} {value}",
                        path_expression(agg.semantic_model.as_str(), &filter.path)
                    ))
                })
                .collect();
            let predicates = predicates?;
            if !predicates.is_empty() {
                result.push_str(" FILTER (WHERE ");
                result.push_str(&predicates.join(" AND "));
                result.push(')');
            }
            Ok(result)
        }
        MetricType::Ratio => {
            if !definition.norn.default_filters.is_empty() {
                return Err(gap(
                    "/x-norn/defaultFilters",
                    "Ratio-level filters require operand population projection",
                ));
            }
            let numerator = params
                .numerator
                .as_ref()
                .ok_or_else(|| gap("/type_params/numerator", "Missing numerator"))?;
            let denominator = params
                .denominator
                .as_ref()
                .ok_or_else(|| gap("/type_params/denominator", "Missing denominator"))?;
            Ok(format!(
                "{} / NULLIF({}, 0)",
                quoted(numerator),
                quoted(denominator)
            ))
        }
        MetricType::Cumulative | MetricType::Derived | MetricType::Conversion => Err(gap(
            "/type_params",
            "Window, derived expression and cohort conversion projections require an explicit admitted calculation profile",
        )),
    }
}

/// Generate canonical semantic datasets/relationships from the admitted effective ontology.
/// Link datasets preserve N:M and 1:N via edge keys; x-norn retains minimum cardinality.
///
/// # Errors
/// Returns precise gaps for unavailable types, ambiguous targets or reserved-field collisions.
#[allow(
    clippy::too_many_lines,
    reason = "one deterministic effective-ontology projection including directional link keys"
)]
pub fn ontology_projection(
    catalog: &EffectiveCatalog,
) -> Result<(Vec<Dataset>, Vec<Relationship>), Vec<Diagnostic>> {
    let mut datasets = Vec::new();
    let mut relationships = Vec::new();
    let mut out = Vec::new();
    for name in catalog.object_type_names() {
        let Some(Ok(object)) = catalog.object_type_named(name) else {
            out.push(gap(
                "/datasets",
                format!("Unavailable ontology object {name}"),
            ));
            continue;
        };
        if object.property(ID).is_some() {
            out.push(gap(
                "/datasets/fields",
                format!("Reserved kernel identity field {name}.{ID}"),
            ));
            continue;
        }
        let mut fields = vec![field(ID.into(), false)];
        fields.extend(object.properties.iter().map(|p| {
            field(
                p.id.0.clone(),
                matches!(
                    p.kind,
                    EffectiveKind::Base(PropertyType::Date {} | PropertyType::Timestamp { .. })
                ),
            )
        }));
        let unique_keys = object
            .keys
            .iter()
            .filter(|key| key.match_policy.is_none())
            .map(|key| key.components.iter().map(|p| p.0.clone()).collect())
            .collect();
        datasets.push(Dataset {
            name: name.into(),
            source: format!("norn://object/{name}"),
            primary_key: vec![ID.into()],
            unique_keys,
            fields,
            norn: None,
            custom_extensions: vec![],
            description: None,
            ai_context: None,
        });
        for link in catalog.links_for_object_type(name).into_iter().flatten() {
            let Some(targets) = catalog.target_types(link.target.as_str()) else {
                out.push(gap(
                    "/relationships",
                    format!("Unavailable link target {}", link.target.as_str()),
                ));
                continue;
            };
            if targets.is_empty() {
                out.push(gap(
                    "/relationships",
                    "Link target has no concrete ontology types",
                ));
                continue;
            }
            let target = link.target.as_str();
            if catalog.object_type_named(target).is_none()
                && !datasets.iter().any(|d| d.name == target)
            {
                datasets.push(Dataset {
                    name: target.into(),
                    source: format!("norn://interface/{target}"),
                    primary_key: vec![ID.into()],
                    unique_keys: vec![],
                    fields: vec![field(ID.into(), false)],
                    norn: None,
                    custom_extensions: vec![],
                    description: None,
                    ai_context: None,
                });
            }
            let edge = format!("{name}::{}", link.id.as_str());
            let mut unique_keys = Vec::new();
            if link.cardinality.to == Multiplicity::One {
                unique_keys.push(vec![FROM.into()]);
            }
            if link.cardinality.from == Multiplicity::One {
                unique_keys.push(vec![TO.into()]);
            }
            let mut fields = vec![field(FROM.into(), false), field(TO.into(), false)];
            fields.extend(link.properties.iter().map(|p| {
                field(
                    p.id.0.clone(),
                    matches!(
                        p.kind,
                        EffectiveKind::Base(PropertyType::Date {} | PropertyType::Timestamp { .. })
                    ),
                )
            }));
            if link
                .properties
                .iter()
                .any(|p| p.id.as_str() == FROM || p.id.as_str() == TO)
            {
                out.push(gap(
                    "/datasets/fields",
                    format!("Reserved edge identity in {edge}"),
                ));
            }
            datasets.push(Dataset {
                source: format!("norn://link/{name}/{}", link.id.as_str()),
                name: edge.clone(),
                primary_key: vec![FROM.into(), TO.into()],
                unique_keys,
                fields,
                norn: Some(LinkProjection {
                    cardinality: link.cardinality,
                    role: link.id.clone(),
                    inverse: link.inverse.clone(),
                }),
                custom_extensions: vec![],
                description: None,
                ai_context: None,
            });
            for (end, column) in [(name, FROM), (target, TO)] {
                relationships.push(Relationship {
                    name: format!("{edge}::{column}"),
                    from: edge.clone(),
                    to: end.into(),
                    from_columns: vec![column.into()],
                    to_columns: vec![ID.into()],
                    ai_context: None,
                    custom_extensions: vec![],
                });
            }
        }
    }
    datasets.sort_by(|a, b| a.name.cmp(&b.name));
    relationships.sort_by(|a, b| a.name.cmp(&b.name));
    if out.is_empty() {
        Ok((datasets, relationships))
    } else {
        Err(out)
    }
}

/// Build a Norn metric file, including its generated ontology projection.
///
/// # Errors
/// Returns checker diagnostics or explicit unsupported interchange semantics.
#[allow(
    clippy::too_many_lines,
    reason = "one complete canonical projection of the ontology and checked metric calculations"
)]
pub fn generate_metric_ir(
    name: &str,
    definitions: &[MetricDefinition],
    catalog: &EffectiveCatalog,
) -> Result<MetricIR, Vec<Diagnostic>> {
    let aliases: std::collections::BTreeSet<_> = definitions
        .iter()
        .map(|d| metricflow_name(&d.name))
        .collect();
    if aliases.len() != definitions.len() {
        return Err(vec![gap(
            "/metrics/name",
            "MetricFlow alias collision between namespaced metric identifiers",
        )]);
    }
    let diagnostics = check_metrics_and_questions(definitions, &[], catalog);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let (mut datasets, relationships) = ontology_projection(catalog)?;
    if datasets.is_empty() {
        return Err(vec![gap(
            "/semantic_model/0/datasets",
            "OSI semantic models require at least one ontology dataset",
        )]);
    }
    let mut metrics = Vec::new();
    for definition in definitions {
        if let Some(agg) = &definition.type_params.metric_aggregation_params {
            for (index, filter) in definition.norn.default_filters.iter().enumerate() {
                if crate::metric::projection_path(
                    agg.semantic_model.as_str(),
                    &filter.path,
                    catalog,
                )?
                .0
                {
                    return Err(vec![gap(
                        &format!("/metrics/{}/x-norn/defaultFilters/{index}", definition.name),
                        "Existential fan-out predicate requires a portable relational expression profile",
                    )]);
                }
            }
        }
        let rendered = simple_expression(definition).map_err(|d| vec![d])?;
        let mut type_params = definition.type_params.clone();
        type_params.numerator = type_params.numerator.map(|name| metricflow_name(&name));
        type_params.denominator = type_params.denominator.map(|name| metricflow_name(&name));
        let dbt = norn_ir::metric::MetricFlowMetric {
            name: metricflow_name(&definition.name),
            label: definition.label.clone(),
            metric_type: definition.metric_type,
            type_params,
            filter: projected_filter(&rendered),
        };
        let mut norn = definition.norn.clone();
        let mut semantics = norn_ir::metric::MetricPropertySemantics {
            unit: None,
            currency_source: None,
        };
        if let Some(agg) = &definition.type_params.metric_aggregation_params
            && !matches!(
                agg.agg,
                norn_ir::metric::MeasureAggregate::Count
                    | norn_ir::metric::MeasureAggregate::CountDistinct
            )
            && let Some(Ok(object)) = catalog.object_type_named(agg.semantic_model.as_str())
            && let Some(property) = definition
                .type_params
                .expr
                .as_deref()
                .and_then(|id| object.property(id))
            && let EffectiveKind::Base(PropertyType::Decimal {
                unit,
                currency_source,
                ..
            }) = &property.kind
        {
            semantics.unit.clone_from(unit);
            semantics.currency_source.clone_from(currency_source);
        }
        norn.derived = Some(semantics);
        metrics.push(Metric {
            name: definition.name.clone(),
            expression: expression(rendered),
            custom_extensions: vec![CustomExtension {
                vendor_name: Vendor::Dbt,
                data: serde_json::to_string(&dbt)
                    .map_err(|e| vec![gap("/metrics/custom_extensions", e.to_string())])?,
            }],
            norn: Some(norn),
            description: None,
            ai_context: None,
        });
        if let Some(agg) = &definition.type_params.metric_aggregation_params {
            let Some(dataset) = datasets
                .iter_mut()
                .find(|d| d.name == agg.semantic_model.as_str())
            else {
                return Err(vec![gap("/datasets", "Missing measure dataset")]);
            };
            let mut paths = definition
                .norn
                .default_filters
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>();
            paths.push(
                definition
                    .norn
                    .time_dimension
                    .clone()
                    .unwrap_or_else(|| crate::metric::dimension_path(&agg.agg_time_dimension)),
            );
            if let Some(currency) = &definition.norn.currency {
                paths.push(currency.property.clone());
                if let Some(conversion) = &currency.conversion {
                    paths.push(conversion.rate.clone());
                }
            }
            for path in paths
                .iter()
                .filter(|p| !p.links.is_empty() || field_name(p) != p.property.as_str())
            {
                let name = field_name(path);
                let (_, temporal) =
                    crate::metric::projection_path(agg.semantic_model.as_str(), path, catalog)?;
                let projected = expression(
                    path.links
                        .iter()
                        .map(|role| quoted(role.as_str()))
                        .chain(std::iter::once(quoted(path.property.as_str())))
                        .collect::<Vec<_>>()
                        .join("."),
                );
                if dataset
                    .fields
                    .iter()
                    .any(|f| f.name == name && f.expression != projected)
                {
                    return Err(vec![gap(
                        "/semantic_model/0/datasets/fields",
                        "Generated path field collides with an ontology field",
                    )]);
                }
                if !dataset.fields.iter().any(|f| f.name == name) {
                    dataset.fields.push(Field {
                        name,
                        expression: expression(
                            path.links
                                .iter()
                                .map(|role| quoted(role.as_str()))
                                .chain(std::iter::once(quoted(path.property.as_str())))
                                .collect::<Vec<_>>()
                                .join("."),
                        ),
                        dimension: Some(Dimension { is_time: temporal }),
                        label: None,
                        description: None,
                        ai_context: None,
                        custom_extensions: vec![],
                    });
                }
            }
            dataset.fields.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }
    Ok(MetricIR {
        version: OsiVersion::V011,
        dialects: vec![],
        vendors: vec![],
        semantic_model: vec![SemanticModel {
            name: name.into(),
            datasets,
            relationships,
            metrics,
            description: None,
            ai_context: None,
            custom_extensions: vec![],
        }],
    })
}

/// Strip only Norn extension members; DBT data already contains only `MetricFlow` fields.
///
/// # Errors
/// Returns a serialization error if the document cannot be encoded.
pub fn export_osi(document: &MetricIR) -> Result<Value, serde_json::Error> {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.remove("x-norn");
                for child in object.values_mut() {
                    strip(child);
                }
            }
            Value::Array(array) => {
                for child in array {
                    strip(child);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let mut value: Value = serde_json::from_str(&serde_json::to_string(document)?)?;
    strip(&mut value);
    Ok(value)
}

/// Explicit ontology and policy bindings for an external import.
#[derive(Debug, Clone, Default)]
pub struct OntologyBinding {
    /// External semantic model/dataset to Norn object type.
    pub datasets: BTreeMap<String, ObjectTypeName>,
    /// Qualified external scalar fields to ontology paths.
    pub fields: BTreeMap<String, DimensionPath>,
    /// External foreign entities to forward ontology roles.
    pub links: BTreeMap<String, norn_ir::LinkId>,
    /// Missing Norn policies keyed by metric name; no guessed owner or authorization.
    pub policies: BTreeMap<String, MetricNorn>,
}

/// Import a native file by checking its DBT calculations and re-generating the ontology.
/// Independently edited datasets, relationships or expression text are rejected.
///
/// # Errors
/// Returns checker findings and precise JSON-pointer gaps.
pub fn admit_metric_ir(
    document: &MetricIR,
    catalog: &EffectiveCatalog,
) -> Result<Vec<MetricDefinition>, Vec<Diagnostic>> {
    if document.semantic_model.len() != 1 {
        return Err(vec![gap(
            "/semantic_model",
            "Norn admission requires one bound semantic model per metric file",
        )]);
    }
    let model = document
        .semantic_model
        .first()
        .ok_or_else(|| vec![gap("/semantic_model", "Missing semantic model")])?;
    let definitions = decode_dbt_metrics(model, &OntologyBinding::default())?;
    let generated = generate_metric_ir(&model.name, &definitions, catalog)?;
    if &generated != document {
        return Err(vec![error(
            codes::ONTOLOGY_PROJECTION,
            "Metric file differs from its generated ontology/calculation projection".into(),
            Span::pointer("/semantic_model/0"),
        )]);
    }
    Ok(definitions)
}

#[allow(
    clippy::too_many_lines,
    clippy::result_large_err,
    reason = "one checked vendor-data decoding pass retaining all source diagnostics"
)]
fn decode_dbt_metrics(
    model: &SemanticModel,
    binding: &OntologyBinding,
) -> Result<Vec<MetricDefinition>, Vec<Diagnostic>> {
    let mut definitions = Vec::new();
    let mut out = Vec::new();
    let aliases: BTreeMap<_, _> = model
        .metrics
        .iter()
        .map(|metric| (metricflow_name(&metric.name), metric.name.clone()))
        .collect();
    if aliases.len() != model.metrics.len() {
        return Err(vec![gap(
            "/semantic_model/0/metrics",
            "Ambiguous MetricFlow metric aliases",
        )]);
    }
    for (index, metric) in model.metrics.iter().enumerate() {
        let at = format!("/semantic_model/0/metrics/{index}");
        let mut extensions = metric
            .custom_extensions
            .iter()
            .enumerate()
            .filter(|(_, e)| e.vendor_name == Vendor::Dbt);
        let Some((extension_index, extension)) = extensions.next() else {
            out.push(gap(&format!("{at}/expression"), "Opaque OSI expressions need a DBT calculation or the bounded aggregate import profile"));
            continue;
        };
        if extensions.next().is_some() {
            out.push(gap(
                &format!("{at}/custom_extensions"),
                "Ambiguous duplicate DBT calculation",
            ));
            continue;
        }
        let Some(policy) = metric
            .norn
            .as_ref()
            .or_else(|| binding.policies.get(&metric.name))
        else {
            out.push(gap(
                &format!("{at}/x-norn"),
                "Explicit owner, roll-up policy and ontology binding required",
            ));
            continue;
        };
        let result = (|| {
            let flow = decode_flow(&extension.data, "").map_err(|mut diagnostic| {
                let nested = diagnostic
                    .span
                    .as_ref()
                    .map_or("", |span| span.pointer.as_str());
                diagnostic.message = format!("DBT JSON {nested}: {}", diagnostic.message);
                diagnostic.span = Some(Span::pointer(format!(
                    "{at}/custom_extensions/{extension_index}/data"
                )));
                diagnostic
            })?;
            if flow.name != metricflow_name(&metric.name) {
                return Err(gap(
                    &format!("{at}/name"),
                    "OSI and DBT metric identifiers disagree",
                ));
            }
            let mut type_params = flow.type_params;
            type_params.numerator = type_params
                .numerator
                .map(|name| aliases.get(&name).cloned().unwrap_or(name));
            type_params.denominator = type_params
                .denominator
                .map(|name| aliases.get(&name).cloned().unwrap_or(name));
            let mut norn = policy.clone();
            norn.derived = None;
            let mut definition = MetricDefinition {
                name: metric.name.clone(),
                label: flow.label,
                metric_type: flow.metric_type,
                type_params,
                norn,
            };
            if flow.filter != projected_filter(&simple_expression(&definition)?) {
                return Err(gap(
                    &format!("{at}/custom_extensions/{extension_index}/data"),
                    "DBT filter must match the explicit typed predicate binding",
                ));
            }
            if let Some(agg) = definition.type_params.metric_aggregation_params.as_mut()
                && let Some(root) = binding.datasets.get(agg.semantic_model.as_str())
            {
                agg.semantic_model = root.clone();
            }
            Ok(definition)
        })();
        match result {
            Ok(definition) => definitions.push(definition),
            Err(diagnostic) => out.push(diagnostic),
        }
    }
    if out.is_empty() {
        Ok(definitions)
    } else {
        Err(out)
    }
}

/// Import measure-based or modern `MetricFlow` samples with explicit ontology binding.
/// Expressions, filters and time-spine/offset semantics outside the profile produce gaps.
///
/// # Errors
/// Returns exact source pointers rather than assuming SQL, keys, currencies or ownership.
#[allow(
    clippy::too_many_lines,
    reason = "one fail-closed import pass for measures, fields, join identities and governance"
)]
pub fn import_metricflow(
    input: &Value,
    binding: &OntologyBinding,
    catalog: &EffectiveCatalog,
) -> Result<MetricIR, Vec<Diagnostic>> {
    let object = input
        .as_object()
        .ok_or_else(|| vec![gap("/", "MetricFlow document must be an object")])?;
    if let Some(key) = object
        .keys()
        .find(|k| k.as_str() != "semantic_models" && k.as_str() != "metrics")
    {
        return Err(vec![gap(
            &format!("/{key}"),
            "Unsupported MetricFlow document member",
        )]);
    }
    let models_json = object
        .get("semantic_models")
        .unwrap_or(&json!([]))
        .to_string();
    let mut model_decoder = serde_json::Deserializer::from_str(&models_json);
    let models: Vec<norn_ir::metric::MetricFlowSemanticModel> =
        serde_path_to_error::deserialize(&mut model_decoder)
            .map_err(|error| vec![decode_path(&error, "/semantic_models")])?;
    model_decoder
        .end()
        .map_err(|error| vec![gap("/semantic_models", error.to_string())])?;
    let metrics = object
        .get("metrics")
        .and_then(Value::as_array)
        .ok_or_else(|| vec![gap("/metrics", "Expected a metrics array")])?;
    let mut measures = BTreeMap::new();
    for (model_index, model) in models.iter().enumerate() {
        let root = binding.datasets.get(&model.name).ok_or_else(|| {
            vec![gap(
                &format!("/semantic_models/{model_index}/name"),
                format!("Missing ontology binding for {}", model.name),
            )]
        })?;
        let Some(Ok(effective)) = catalog.object_type_named(root.as_str()) else {
            return Err(vec![gap(
                &format!("/semantic_models/{model_index}/name"),
                "Bound ontology object is unavailable",
            )]);
        };
        for (index, entity) in model.entities.iter().enumerate() {
            let at = format!("/semantic_models/{model_index}/entities/{index}");
            let expression = entity.expr.as_deref().unwrap_or(&entity.name);
            if entity.entity_type == norn_ir::metric::MetricFlowEntityType::Foreign {
                let role = binding
                    .links
                    .get(&format!("{}.{}", model.name, entity.name))
                    .ok_or_else(|| {
                        vec![gap(
                            &at,
                            "Foreign entity requires an explicit ontology link binding",
                        )]
                    })?;
                if !catalog
                    .links_for_object_type(root.as_str())
                    .into_iter()
                    .flatten()
                    .any(|link| &link.id == role && link.cardinality.to == Multiplicity::One)
                {
                    return Err(vec![gap(
                        &at,
                        "Foreign entity must bind a single-valued forward ontology link",
                    )]);
                }
            } else {
                let path = binding
                    .fields
                    .get(&format!("{}.{}", model.name, expression))
                    .ok_or_else(|| vec![gap(&at, "Entity key requires explicit field binding")])?;
                if !path.links.is_empty()
                    || (path.property.as_str() != ID
                        && !effective.keys.iter().any(|key| {
                            key.match_policy.is_none()
                                && key.components.as_slice() == [path.property.clone()]
                        }))
                {
                    return Err(vec![gap(
                        &at,
                        "Declared primary/unique/natural entity is not a proven ontology identity or key",
                    )]);
                }
            }
        }
        for (index, dimension) in model.dimensions.iter().enumerate() {
            let at = format!("/semantic_models/{model_index}/dimensions/{index}");
            let expr = dimension.expr.as_deref().unwrap_or(&dimension.name);
            let path = binding
                .fields
                .get(&format!("{}.{}", model.name, expr))
                .ok_or_else(|| {
                    vec![gap(
                        &at,
                        "Dimension requires explicit ontology field binding",
                    )]
                })?;
            let (_, temporal) = crate::metric::projection_path(root.as_str(), path, catalog)?;
            if (dimension.dimension_type == norn_ir::metric::MetricFlowDimensionType::Time)
                != temporal
                || (temporal && dimension.type_params.is_none())
            {
                return Err(vec![gap(
                    &at,
                    "Dimension type/granularity disagrees with the ontology",
                )]);
            }
        }
        for (index, measure) in model.measures.iter().enumerate() {
            if measures
                .insert(measure.name.clone(), (model, measure))
                .is_some()
            {
                return Err(vec![gap(
                    &format!("/semantic_models/{model_index}/measures/{index}/name"),
                    "Ambiguous measure name",
                )]);
            }
        }
    }
    let mut definitions = Vec::new();
    for (index, metric) in metrics.iter().enumerate() {
        let at = format!("/metrics/{index}");
        let mut metric = metric.clone();
        let name = metric
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| vec![gap(&format!("{at}/name"), "Missing metric name")])?
            .to_owned();
        let policy = binding.policies.get(&name).ok_or_else(|| {
            vec![gap(
                &format!("{at}/x-norn"),
                "Import requires explicit Norn governance",
            )]
        })?;
        let mut norn = policy.clone();
        norn.derived = None;
        if metric.get("type").is_some_and(|t| t == "simple")
            && let Some(input_measure) = metric.pointer("/type_params/measure")
        {
            if let Some(params) = metric.get("type_params").and_then(Value::as_object)
                && let Some(key) = params.keys().find(|key| key.as_str() != "measure")
            {
                return Err(vec![gap(
                    &format!("{at}/type_params/{key}"),
                    "Measure normalization cannot discard additional calculation options",
                )]);
            }
            let measure_name = input_measure
                .as_str()
                .or_else(|| input_measure.get("name").and_then(Value::as_str))
                .ok_or_else(|| {
                    vec![gap(
                        &format!("{at}/type_params/measure"),
                        "Measure input must name one bound measure",
                    )]
                })?;
            if input_measure.as_object().is_some_and(|o| o.len() != 1) {
                return Err(vec![gap(
                    &format!("{at}/type_params/measure"),
                    "Measure input filters, fill-null and time-spine options require explicit semantics",
                )]);
            }
            let (model, measure) = measures.get(measure_name).ok_or_else(|| {
                vec![gap(&format!("{at}/type_params/measure"), "Unknown measure")]
            })?;
            let root = binding
                .datasets
                .get(&model.name)
                .ok_or_else(|| vec![gap(&at, "Missing model binding")])?;
            let amount = binding
                .fields
                .get(&format!("{}.{}", model.name, measure.expr))
                .ok_or_else(|| {
                    vec![gap(
                        &format!("{at}/type_params/measure"),
                        "Measure expression requires explicit property binding",
                    )]
                })?;
            if !amount.links.is_empty() {
                return Err(vec![gap(
                    &format!("{at}/type_params/measure"),
                    "Base aggregate must bind a property at the measure object grain",
                )]);
            }
            let time_name = measure
                .agg_time_dimension
                .as_ref()
                .or(model.defaults.agg_time_dimension.as_ref())
                .ok_or_else(|| {
                    vec![gap(
                        &format!("{at}/type_params/measure"),
                        "Missing agg_time_dimension",
                    )]
                })?;
            let time = binding
                .fields
                .get(&format!("{}.{}", model.name, time_name))
                .ok_or_else(|| {
                    vec![gap(
                        &format!("{at}/type_params/measure"),
                        "Missing ontology time-field binding",
                    )]
                })?;
            if norn.time_dimension.as_ref().is_some_and(|t| t != time) {
                return Err(vec![gap(
                    &format!("{at}/x-norn/timeDimension"),
                    "Policy time binding disagrees with the measure",
                )]);
            }
            norn.time_dimension = Some(time.clone());
            if measure.non_additive_dimension.is_some() {
                return Err(vec![gap(
                    &format!("{at}/type_params/measure/non_additive_dimension"),
                    "Snapshot series import needs explicit timestamp/series binding and ordered reductions",
                )]);
            }
            metric.as_object_mut().ok_or_else(|| vec![gap(&at, "Metric must be an object")])?.insert("type_params".into(), json!({"expr":amount.property.as_str(),"metric_aggregation_params":{"semantic_model":root.as_str(),"agg":measure.agg,"agg_time_dimension":field_name(time)}}));
        }
        if metric.get("label").is_none() {
            metric
                .as_object_mut()
                .ok_or_else(|| vec![gap(&at, "Metric must be an object")])?
                .insert("label".into(), json!(name));
        }
        let flow = decode_flow(&metric.to_string(), &at).map_err(|diagnostic| vec![diagnostic])?;
        if flow.filter.is_some() {
            return Err(vec![gap(
                &format!("{at}/filter"),
                "SQL filter import requires a matching typed ontology predicate binding",
            )]);
        }
        let mut definition = MetricDefinition {
            name: flow.name,
            label: flow.label,
            metric_type: flow.metric_type,
            type_params: flow.type_params,
            norn,
        };
        if let Some(agg) = definition.type_params.metric_aggregation_params.as_mut() {
            if let Some(root) = binding.datasets.get(agg.semantic_model.as_str()) {
                let source = agg.semantic_model.0.clone();
                let time = binding
                    .fields
                    .get(&format!("{source}.{}", agg.agg_time_dimension))
                    .ok_or_else(|| {
                        vec![gap(
                            &format!(
                                "{at}/type_params/metric_aggregation_params/agg_time_dimension"
                            ),
                            "Modern aggregate requires an explicit ontology time binding",
                        )]
                    })?;
                if definition
                    .norn
                    .time_dimension
                    .as_ref()
                    .is_some_and(|t| t != time)
                {
                    return Err(vec![gap(
                        &format!("{at}/x-norn/timeDimension"),
                        "Policy and aggregate time bindings disagree",
                    )]);
                }
                definition.norn.time_dimension = Some(time.clone());
                agg.agg_time_dimension = field_name(time);
                agg.semantic_model = root.clone();
                if let Some(expr) = definition
                    .type_params
                    .expr
                    .as_mut()
                    .filter(|expr| expr.as_str() != "*")
                {
                    let amount = binding
                        .fields
                        .get(&format!("{source}.{expr}"))
                        .filter(|path| path.links.is_empty())
                        .ok_or_else(|| {
                            vec![gap(
                                &format!("{at}/type_params/expr"),
                                "Modern aggregate requires a direct ontology property binding",
                            )]
                        })?;
                    expr.clone_from(&amount.property.0);
                }
            } else if !matches!(
                catalog.object_type_named(agg.semantic_model.as_str()),
                Some(Ok(_))
            ) {
                return Err(vec![gap(
                    &format!("{at}/type_params/metric_aggregation_params/semantic_model"),
                    "Semantic model requires an explicit available ontology binding",
                )]);
            }
        }
        definitions.push(definition);
    }
    generate_metric_ir("metricflow_import", &definitions, catalog)
}

/// Import external OSI DBT definitions, or a bounded ANSI aggregate sample.
/// The result always regenerates datasets/relationships from the ontology.
///
/// # Errors
/// Opaque SQL, unsupported dialects or conflicting relationship/key bindings report gaps.
#[allow(
    clippy::too_many_lines,
    reason = "one fail-closed import pass for the native projection and bounded external expressions"
)]
pub fn import_osi(
    document: &MetricIR,
    binding: &OntologyBinding,
    catalog: &EffectiveCatalog,
) -> Result<MetricIR, Vec<Diagnostic>> {
    let model = document
        .semantic_model
        .first()
        .filter(|_| document.semantic_model.len() == 1)
        .ok_or_else(|| {
            vec![gap(
                "/semantic_model",
                "Import requires one explicitly bound semantic model",
            )]
        })?;
    // Near-identity native export import: reconstruct policies from explicit bindings,
    // then compare the *entire* stripped projection before admitting it.
    if model
        .datasets
        .iter()
        .any(|dataset| dataset.source.starts_with("norn://"))
    {
        let definitions = decode_dbt_metrics(model, binding)?;
        let generated = generate_metric_ir(&model.name, &definitions, catalog)?;
        if export_osi(&generated).map_err(|e| vec![gap("/semantic_model", e.to_string())])?
            != export_osi(document).map_err(|e| vec![gap("/semantic_model", e.to_string())])?
        {
            return Err(vec![gap(
                "/semantic_model/0",
                "Exported ontology/calculation projection differs from the bound ontology",
            )]);
        }
        return Ok(generated);
    }
    // Imported table names and SQL sources never become a second ontology.
    for (index, dataset) in model.datasets.iter().enumerate() {
        if !binding.datasets.contains_key(&dataset.name) {
            return Err(vec![gap(
                &format!("/semantic_model/0/datasets/{index}/name"),
                "Dataset needs an explicit ontology binding",
            )]);
        }
    }
    if !model.relationships.is_empty() {
        return Err(vec![gap(
            "/semantic_model/0/relationships",
            "External join columns require explicit ontology link/key binding; no relationships are authored by import",
        )]);
    }
    let mut definitions = Vec::new();
    for (index, metric) in model.metrics.iter().enumerate() {
        if metric
            .custom_extensions
            .iter()
            .any(|e| e.vendor_name == Vendor::Dbt)
        {
            let single = SemanticModel {
                metrics: vec![metric.clone()],
                ..model.clone()
            };
            let unbound = OntologyBinding {
                policies: binding.policies.clone(),
                ..Default::default()
            };
            let original = decode_dbt_metrics(&single, &unbound)?;
            let projected = original
                .first()
                .ok_or_else(|| vec![gap("/metrics", "Missing DBT metric")])?;
            if expression(simple_expression(projected).map_err(|d| vec![d])?) != metric.expression {
                return Err(vec![gap(
                    &format!("/semantic_model/0/metrics/{index}/expression"),
                    "OSI expression and DBT calculation disagree",
                )]);
            }
            definitions.extend(decode_dbt_metrics(&single, binding)?);
            continue;
        }
        let at = format!("/semantic_model/0/metrics/{index}");
        let dialect = metric
            .expression
            .dialects
            .first()
            .filter(|_| metric.expression.dialects.len() == 1)
            .filter(|e| e.dialect == Dialect::AnsiSql)
            .ok_or_else(|| {
                vec![gap(
                    &format!("{at}/expression"),
                    "Import profile requires exactly one ANSI_SQL expression",
                )]
            })?;
        // This bounded recognizer is intentionally not a SQL parser or execution engine.
        let grammar = regex_lite::Regex::new(r"^\s*(SUM|COUNT|AVG|MIN|MAX)\(\s*([a-zA-Z_][a-zA-Z0-9_]*)\.([a-zA-Z_][a-zA-Z0-9_]*)\s*\)\s*$").map_err(|e| vec![gap(&at, e.to_string())])?;
        let captures = grammar.captures(&dialect.expression).ok_or_else(|| vec![gap(&format!("{at}/expression/dialects/0/expression"), "Opaque expression: only a single bound dataset.property aggregate is imported without DBT metadata")])?;
        let dataset = captures.get(2).map_or("", |m| m.as_str());
        let property = captures.get(3).map_or("", |m| m.as_str());
        let root = binding
            .datasets
            .get(dataset)
            .ok_or_else(|| vec![gap(&format!("{at}/expression"), "Missing dataset binding")])?;
        let amount = binding
            .fields
            .get(&format!("{dataset}.{property}"))
            .filter(|p| p.links.is_empty())
            .ok_or_else(|| {
                vec![gap(
                    &format!("{at}/expression"),
                    "Missing direct property binding",
                )]
            })?;
        let policy = binding.policies.get(&metric.name).ok_or_else(|| {
            vec![gap(
                &format!("{at}/x-norn"),
                "Missing explicit owner/governance/time policy",
            )]
        })?;
        let time = policy.time_dimension.as_ref().ok_or_else(|| {
            vec![gap(
                &format!("{at}/x-norn/timeDimension"),
                "OSI has no agg_time_dimension; explicit ontology time binding required",
            )]
        })?;
        let agg = match captures.get(1).map(|m| m.as_str()) {
            Some("SUM") => norn_ir::metric::MeasureAggregate::Sum,
            Some("COUNT") => norn_ir::metric::MeasureAggregate::Count,
            Some("AVG") => norn_ir::metric::MeasureAggregate::Avg,
            Some("MIN") => norn_ir::metric::MeasureAggregate::Min,
            Some("MAX") => norn_ir::metric::MeasureAggregate::Max,
            Some(_) | None => return Err(vec![gap(&at, "Unsupported aggregate")]),
        };
        definitions.push(MetricDefinition {
            name: metric.name.clone(),
            label: metric.name.clone(),
            metric_type: MetricType::Simple,
            type_params: norn_ir::metric::MetricTypeParams {
                expr: Some(amount.property.0.clone()),
                metric_aggregation_params: Some(norn_ir::metric::MetricAggregationParams {
                    semantic_model: root.clone(),
                    agg,
                    agg_time_dimension: field_name(time),
                    non_additive_dimension: None,
                }),
                ..Default::default()
            },
            norn: {
                let mut norn = policy.clone();
                norn.derived = None;
                norn
            },
        });
    }
    generate_metric_ir(&model.name, &definitions, catalog)
}
