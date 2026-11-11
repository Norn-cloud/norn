//! Conservative scalar projection. The source graph remains authoritative for
//! particles, inheritance, nil/presence, whitespace and lexical constraints.
use crate::{
    type_tables::{TableInput, TypeTable},
    xsd::XsdNode,
};
use norn_ir::source_schema::{NormalizedType, SourceField};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub(crate) const SCALARS: &str = include_str!("../tables/norn.formats.xsd.scalar_types.json");
pub(crate) fn property(
    index: usize,
    field: &SourceField,
    node: &XsdNode,
    nodes: &BTreeMap<&str, &XsdNode>,
    table: &mut TypeTable,
) -> Result<(Value, crate::contract::TypeDecision), String> {
    let mut scalar = true;
    let mut parent = Some(node.id.as_str());
    while let Some(id) = parent {
        let ancestor = nodes.get(id).ok_or("particle parent is missing")?;
        if ancestor.is("choice")
            || ancestor.is("any")
            || (!ancestor.attr("maxOccurs").is_empty() && ancestor.attr("maxOccurs") != "1")
        {
            scalar = false;
        }
        if ancestor.is("complexType") {
            break;
        }
        parent = ancestor.parent.as_deref();
    }
    let row = table.lookup(&[(
        "scalar",
        TableInput::Text(if scalar {
            field.native_type.clone()
        } else {
            String::new()
        }),
    )])?;
    let kind = match row.text("kind") {
        "text" => "text",
        "boolean" => "boolean",
        _ => "json-ext",
    };
    Ok((
        json!({"id":format!("f{index}"),"type":{"kind":kind},"constraints":{"nullable":true},"description":format!("{}; source field {}. Native lexical constraints, nil and presence remain separate source evidence; not enforced by this technical projection.",field.native_type,field.name)}),
        crate::contract::TypeDecision {
            table: row.table,
            row: row.row,
        },
    ))
}
pub(crate) fn resolve_fields(
    nodes: &[XsdNode],
    datasets: &mut [crate::xsd::DiscoveredDataset],
    repeating: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let by_id = nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect::<BTreeMap<_, _>>();
    let mut globals = BTreeMap::new();
    for node in crate::xsd::semantic_nodes(nodes) {
        if !node.is("element") || node.attr("name").is_empty() {
            continue;
        }
        let Some(root) = node.parent.as_deref().and_then(|id| by_id.get(id)) else {
            continue;
        };
        if !root.is("schema") {
            continue;
        }
        let key = format!("{{{}}}{}", root.attr("targetNamespace"), node.attr("name"));
        if globals.insert(key, node).is_some() {
            return Err("duplicate global element".into());
        }
    }
    // Global simple and complex types, for simple-content resolution.
    let mut types: BTreeMap<String, &XsdNode> = BTreeMap::new();
    for node in crate::xsd::semantic_nodes(nodes) {
        if !(node.is("simpleType") || node.is("complexType")) || node.attr("name").is_empty() {
            continue;
        }
        let Some(root) = node.parent.as_deref().and_then(|id| by_id.get(id)) else {
            continue;
        };
        if root.is("schema") {
            let name = format!("{{{}}}{}", root.attr("targetNamespace"), node.attr("name"));
            if types.insert(name.clone(), node).is_some() {
                return Err(format!("duplicate global type {name}"));
            }
        }
    }
    let mut children: BTreeMap<&str, Vec<&XsdNode>> = BTreeMap::new();
    for node in nodes {
        if let Some(parent) = node.parent.as_deref() {
            children.entry(parent).or_default().push(node);
        }
    }
    let types = TypeIndex { types, children };
    for (_, _, fields) in datasets {
        for (field, node_id) in fields {
            let node = by_id
                .get(node_id.as_str())
                .ok_or("field source identity is missing")?;
            if !node.attr("ref").is_empty() {
                let name = node.qname(node.attr("ref"))?;
                if let Some(def) = globals.get(&name)
                    && !def.attr("type").is_empty()
                {
                    field.native_type = def.qname(def.attr("type"))?;
                }
            }
            // A field that can occur more than once is a list, never a scalar.
            let base = if repeating.contains(&field.name) {
                None
            } else {
                builtin(&field.native_type, &types, 0)?
            };
            field.type_candidate.normalized = match base.as_deref() {
                Some(
                    "string" | "normalizedString" | "token" | "anyURI" | "language" | "Name"
                    | "NCName" | "NMTOKEN" | "ID" | "IDREF",
                ) => NormalizedType::String,
                Some("boolean") => NormalizedType::Boolean,
                Some("date") => NormalizedType::Date,
                Some("dateTime") => NormalizedType::Datetime,
                Some("time") => NormalizedType::Time,
                // Unbounded numeric lexical values remain strings in the runtime
                // projection; never silently narrow to Norn's fixed decimal scale.
                _ => NormalizedType::Json,
            };
        }
    }
    Ok(())
}

/// Global type definitions and each node's children, for simple-content
/// resolution.
struct TypeIndex<'a> {
    types: BTreeMap<String, &'a XsdNode>,
    children: BTreeMap<&'a str, Vec<&'a XsdNode>>,
}

const XSD_NS: &str = "{http://www.w3.org/2001/XMLSchema}";

/// The XML Schema built-in a value of `qname` ultimately has: through simple
/// type restrictions and a complex type's simple content (extension or
/// restriction). `None` for element-only content, lists, unions and unknown
/// types. Bounded, so a cyclic definition cannot loop.
fn builtin(qname: &str, index: &TypeIndex<'_>, depth: u8) -> Result<Option<String>, String> {
    if let Some(local) = qname.strip_prefix(XSD_NS) {
        return Ok(Some(local.to_owned()));
    }
    if depth > 32 {
        return Ok(None);
    }
    let Some(node) = index.types.get(qname) else {
        return Ok(None);
    };
    let children = |id: &str| index.children.get(id).map_or(&[][..], Vec::as_slice);
    let mut frontier = children(&node.id);
    if node.is("complexType") {
        let Some(content) = frontier.iter().find(|n| n.is("simpleContent")) else {
            return Ok(None);
        };
        frontier = children(&content.id);
    }
    for step in frontier {
        if (step.is("restriction") || step.is("extension")) && !step.attr("base").is_empty() {
            return builtin(
                &step.qname(step.attr("base"))?,
                index,
                depth.saturating_add(1),
            );
        }
    }
    Ok(None)
}
