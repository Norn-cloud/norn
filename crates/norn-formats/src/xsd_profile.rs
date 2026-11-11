//! Typed source semantics, separate from the executable `EntityIR` projection.
use crate::xsd::XsdNode;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(crate) enum OccurrenceLimit {
    Finite(String),
    Unbounded,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Particle {
    pub node: String,
    pub kind: String,
    pub min_occurs: String,
    pub max_occurs: OccurrenceLimit,
    pub nillable: Option<bool>,
    pub parent: Option<String>,
    pub name: Option<String>,
    pub reference: Option<String>,
    pub type_name: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Facet {
    pub node: String,
    pub parent: Option<String>,
    pub kind: String,
    // Decimal and integer values remain lexical strings, never f64.
    pub lexical_value: String,
    pub fixed: Option<bool>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnumerationBinding {
    pub type_node: String,
    pub restriction_node: String,
    pub value_nodes: Vec<String>,
    pub lexical_values: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct XsdProfile {
    pub particles: Vec<Particle>,
    pub facets: Vec<Facet>,
    pub enumeration_bindings: Vec<EnumerationBinding>,
    pub qualified_references: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}
fn bool_value(value: &str) -> Result<bool, String> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!("invalid XSD boolean {value}")),
    }
}
fn optional_bool(node: &XsdNode, key: &str) -> Result<Option<bool>, String> {
    node.attributes
        .get(key)
        .map(|value| bool_value(value))
        .transpose()
}
fn whole(value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|error| format!("invalid or out-of-profile occurrence {value}: {error}"))
}
/// A particle's `minOccurs` and `maxOccurs` (`None` for `unbounded`), parsed
/// numerically in this one place: `"00"`, `"+0"` and `"0"` all mean zero.
pub(crate) fn occurrences(node: &XsdNode) -> Result<(u64, Option<u64>), String> {
    let min = node
        .attributes
        .get("minOccurs")
        .map_or(Ok(1), |v| whole(v))?;
    let max = match node.attr("maxOccurs") {
        "" => Some(1),
        "unbounded" => None,
        value => Some(whole(value)?),
    };
    if max.is_some_and(|max| min > max) {
        return Err(format!("minOccurs exceeds maxOccurs: {}", node.id));
    }
    Ok((min, max))
}

pub(crate) fn describe(nodes: &[XsdNode]) -> Result<XsdProfile, String> {
    let mut profile = XsdProfile {
        particles: Vec::new(),
        facets: Vec::new(),
        enumeration_bindings: Vec::new(),
        qualified_references: BTreeMap::new(),
    };
    for node in crate::xsd::semantic_nodes(nodes) {
        let mut references = BTreeMap::new();
        for key in [
            "ref",
            "type",
            "base",
            "itemType",
            "substitutionGroup",
            "memberTypes",
        ] {
            if let Some(value) = node.attributes.get(key) {
                let values = value
                    .split_whitespace()
                    .map(|value| node.qname(value))
                    .collect::<Result<Vec<_>, _>>()?;
                if values.is_empty() || (key != "memberTypes" && values.len() != 1) {
                    return Err(format!("invalid QName list {}@{key}", node.id));
                }
                references.insert(key.to_owned(), values);
            }
        }
        if !references.is_empty() {
            profile
                .qualified_references
                .insert(node.id.clone(), references);
        }
        if ["element", "sequence", "choice", "all", "any", "group"]
            .iter()
            .any(|kind| node.is(kind))
        {
            let (min, max) = occurrences(node)?;
            let max = max.map_or(OccurrenceLimit::Unbounded, |max| {
                OccurrenceLimit::Finite(max.to_string())
            });
            let kind = node.name.rsplit('}').next().unwrap_or_default().to_owned();
            profile.particles.push(Particle {
                node: node.id.clone(),
                kind,
                min_occurs: min.to_string(),
                max_occurs: max,
                nillable: optional_bool(node, "nillable")?,
                parent: node.parent.clone(),
                name: node.attributes.get("name").cloned(),
                reference: node
                    .attributes
                    .get("ref")
                    .map(|v| node.qname(v))
                    .transpose()?,
                type_name: node
                    .attributes
                    .get("type")
                    .map(|v| node.qname(v))
                    .transpose()?,
            });
        }
        if [
            "enumeration",
            "pattern",
            "whiteSpace",
            "length",
            "minLength",
            "maxLength",
            "minInclusive",
            "maxInclusive",
            "minExclusive",
            "maxExclusive",
            "totalDigits",
            "fractionDigits",
            "assertion",
            "explicitTimezone",
        ]
        .iter()
        .any(|kind| node.is(kind))
        {
            profile.facets.push(Facet {
                node: node.id.clone(),
                parent: node.parent.clone(),
                kind: node.name.rsplit('}').next().unwrap_or_default().to_owned(),
                lexical_value: node.attributes.get("value").cloned().unwrap_or_default(),
                fixed: optional_bool(node, "fixed")?,
            });
        }
    }
    profile.enumeration_bindings = enumeration_bindings(nodes, &profile.facets);
    Ok(profile)
}

fn enumeration_bindings(nodes: &[XsdNode], facets: &[Facet]) -> Vec<EnumerationBinding> {
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let mut bindings: BTreeMap<String, EnumerationBinding> = BTreeMap::new();
    for facet in facets.iter().filter(|facet| facet.kind == "enumeration") {
        let Some(restriction) = facet.parent.as_deref() else {
            continue;
        };
        let mut parent = by_id
            .get(restriction)
            .and_then(|node| node.parent.as_deref());
        while let Some(id) = parent {
            let Some(node) = by_id.get(id) else {
                break;
            };
            if node.is("simpleType") || node.is("complexType") {
                let binding =
                    bindings
                        .entry(restriction.to_owned())
                        .or_insert_with(|| EnumerationBinding {
                            type_node: node.id.clone(),
                            restriction_node: restriction.to_owned(),
                            value_nodes: Vec::new(),
                            lexical_values: Vec::new(),
                        });
                binding.value_nodes.push(facet.node.clone());
                binding.lexical_values.push(facet.lexical_value.clone());
                break;
            }
            parent = node.parent.as_deref();
        }
    }
    bindings.into_values().collect()
}
