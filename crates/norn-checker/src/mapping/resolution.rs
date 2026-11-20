//! SOURCE-RESOLVE-1: when several mappings (several source systems) write
//! one canonical property, the canonical value is decided by an explicitly
//! declared [`SourceAuthority`], never by whichever source wrote last.
//!
//! The [`ResolutionTable`] of an admitted set lists, for every canonical
//! property, the mappings that write it and the authority each declares
//! (a property rule's own `authority`, else its map's). Admission
//! (`admission_findings`) refuses a property that two mappings write when
//! any writer declares nothing, when the declarations do not single out who
//! decides, or when nobody may decide. The cell decides each written value
//! with [`ResolutionTable::resolve`] over the latest report of every writer,
//! so the outcome is a function of the reports, not of their arrival order.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Type;
use norn_ir::ArtifactEnvelope;
use norn_ir::mapping::{MappingIr, SourceAuthority};

use super::{MappingModel, codes};
use crate::entity_instance::{EffectiveCatalog, EffectiveObjectType};
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// Where one writer declares a property: its map's index and the property
/// rule's index within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct At {
    map: usize,
    rule: usize,
}

/// One mapping's declaration for one canonical property.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Writer {
    /// The declared authority; `None` when neither the rule nor its map
    /// declares one.
    authority: Option<SourceAuthority>,
    /// Every authority the mapping's maps declare for the property (more
    /// than one is ambiguous within one mapping).
    declared: BTreeSet<Option<SourceAuthority>>,
    at: At,
}

/// The writers of every canonical property of a set, by object type name and
/// property, each keyed by its mapping artifact id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionTable {
    fields: BTreeMap<(String, String), BTreeMap<String, Writer>>,
}

/// How one property's reports decide its canonical value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<'a, V> {
    /// The deciding writer that reported (`mapping`), and its report.
    Decided {
        /// The mapping artifact id that decided.
        mapping: &'a str,
        /// Its report.
        value: &'a V,
    },
    /// Conflict peers that all agree on `value`.
    Agreed {
        /// The agreed report.
        value: &'a V,
    },
    /// Conflict peers that disagree: the value is an explicit conflict.
    Conflict,
    /// Reports exist, but none from a writer that may decide.
    Undecided,
}

impl ResolutionTable {
    /// The table of every `mapping` artifact of `artifacts`, with targets
    /// resolved against the `entity` artifacts of `artifacts` and `base`.
    #[must_use]
    pub fn from_artifacts(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Self {
        let all: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
        let catalog = EffectiveCatalog::from_artifacts(&all);
        let mut names = BTreeMap::new();
        for artifact in &all {
            if let Some(Ok(object_type)) = catalog.object_type(&artifact.artifact_id) {
                names.insert(
                    artifact.artifact_id.clone(),
                    (
                        object_type.name.as_str().to_owned(),
                        key_properties(object_type),
                    ),
                );
            }
        }
        let mappings: Vec<(String, MappingIr)> = artifacts
            .iter()
            .filter_map(|artifact| {
                super::set::mapping_payload(artifact).map(|m| (artifact.artifact_id.clone(), m))
            })
            .collect();
        Self::of(
            mappings.iter().map(|(id, mapping)| (id.as_str(), mapping)),
            |entity| names.get(entity).cloned(),
        )
    }

    /// `type_of` gives a target entity's object type name and the
    /// properties that hold its key exactly.
    fn of<'a>(
        mappings: impl IntoIterator<Item = (&'a str, &'a MappingIr)>,
        type_of: impl Fn(&str) -> Option<(String, BTreeSet<String>)>,
    ) -> Self {
        let mut fields: BTreeMap<(String, String), BTreeMap<String, Writer>> = BTreeMap::new();
        // Whether every rule writing a property is a proven identity write.
        let mut identity: BTreeMap<(String, String), bool> = BTreeMap::new();
        for (mapping_id, mapping) in mappings {
            for (map_index, map) in mapping.maps.iter().enumerate() {
                let Some((object_type, keys)) = type_of(&map.target.entity) else {
                    continue;
                };
                for (rule, property) in map.properties.iter().enumerate() {
                    let field = (object_type.clone(), property.property.as_str().to_owned());
                    // A rule that writes the map's subject to a property
                    // holding the key exactly writes the key itself.
                    let proven = property.value == map.subject
                        && property.authority.is_none()
                        && keys.contains(property.property.as_str());
                    let all = identity.entry(field.clone()).or_insert(true);
                    *all &= proven;
                    let authority = property.authority.or(map.authority);
                    let writers = fields.entry(field).or_default();
                    let writer = writers.entry(mapping_id.to_owned()).or_insert(Writer {
                        authority,
                        declared: BTreeSet::new(),
                        at: At {
                            map: map_index,
                            rule,
                        },
                    });
                    writer.declared.insert(authority);
                }
            }
        }
        // Every report on one subject agrees on its key, so a property only
        // ever written with it needs no resolution; a property any rule
        // writes otherwise is resolved over all of its writers.
        fields.retain(|field, _| !identity.get(field).copied().unwrap_or(false));
        Self { fields }
    }

    /// Whether any property of `object_type` is resolved across writers
    /// (written by more than one mapping, or declared).
    #[must_use]
    pub fn governs(&self, object_type: &str) -> bool {
        self.fields
            .range((object_type.to_owned(), String::new())..)
            .take_while(|((t, _), _)| t == object_type)
            .any(|(_, writers)| resolved(writers))
    }

    /// The mappings that write `property` of `object_type`, with their
    /// declared authority.
    #[must_use]
    pub fn writers(
        &self,
        object_type: &str,
        property: &str,
    ) -> Vec<(&str, Option<SourceAuthority>)> {
        self.fields
            .get(&(object_type.to_owned(), property.to_owned()))
            .map(|writers| {
                writers
                    .iter()
                    .map(|(id, writer)| (id.as_str(), writer.authority))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The canonical value of `property` of `object_type` from `reports`
    /// (the latest report of each writer, by mapping artifact id). `None`
    /// when the property is not resolved across writers (one writer, no
    /// declaration): its single writer's report is the value as before.
    /// A report from a mapping that is not a declared writer never decides.
    #[must_use]
    pub fn resolve<'a, V: PartialEq>(
        &self,
        object_type: &str,
        property: &str,
        reports: &'a BTreeMap<String, V>,
    ) -> Option<Resolution<'a, V>> {
        let writers = self
            .fields
            .get(&(object_type.to_owned(), property.to_owned()))?;
        if !resolved(writers) {
            return None;
        }
        let conflict = writers
            .values()
            .any(|writer| writer.authority == Some(SourceAuthority::Conflict));
        if conflict {
            let mut values = reports
                .iter()
                .filter(|(id, _)| writers.contains_key(*id))
                .map(|(_, value)| value);
            let Some(first) = values.next() else {
                return Some(Resolution::Undecided);
            };
            return Some(if values.all(|value| value == first) {
                Resolution::Agreed { value: first }
            } else {
                Resolution::Conflict
            });
        }
        let best = reports
            .iter()
            .filter_map(|(id, value)| {
                let rank = match writers.get(id)?.authority? {
                    SourceAuthority::SystemOfRecord => 0,
                    SourceAuthority::Precedence { rank } => u64::from(rank),
                    SourceAuthority::Check | SourceAuthority::Conflict => return None,
                };
                Some((rank, id.as_str(), value))
            })
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
        Some(best.map_or(Resolution::Undecided, |(_, mapping, value)| {
            Resolution::Decided { mapping, value }
        }))
    }
}

/// Whether a property's writers need a resolution: more than one mapping
/// writes it, or one declares an authority.
fn resolved(writers: &BTreeMap<String, Writer>) -> bool {
    writers.len() > 1 || writers.values().any(|w| w.authority.is_some())
}

fn describe(authority: Option<SourceAuthority>) -> String {
    match authority {
        None => "no authority".to_owned(),
        Some(SourceAuthority::SystemOfRecord) => "system_of_record".to_owned(),
        Some(SourceAuthority::Precedence { rank }) => format!("precedence {rank}"),
        Some(SourceAuthority::Check) => "check".to_owned(),
        Some(SourceAuthority::Conflict) => "conflict".to_owned(),
    }
}

/// SOURCE-RESOLVE-1 admission over `mappings` (the candidate mapping
/// artifacts with their payloads): one finding per canonical property whose
/// writers do not explicitly decide its value.
#[must_use]
pub(super) fn admission_findings(
    mappings: &[(&ArtifactEnvelope, MappingIr)],
    model: &MappingModel,
) -> Vec<Diagnostic> {
    let table = ResolutionTable::of(
        mappings
            .iter()
            .map(|(artifact, mapping)| (artifact.artifact_id.as_str(), mapping)),
        |entity| {
            model
                .object_type_of(entity)
                .map(|name| (name.0.clone(), model.key_properties(name.as_str())))
        },
    );
    let mut out = lifecycle_overlaps(mappings, model);
    for ((object_type, property), writers) in &table.fields {
        if !resolved(writers) {
            continue;
        }
        let Some(finding) = field_finding(object_type, property, writers, model) else {
            continue;
        };
        let (code, message, mapping_id) = finding;
        let Some((artifact, _)) = mappings
            .iter()
            .find(|(artifact, _)| artifact.artifact_id == mapping_id)
        else {
            continue;
        };
        let at = writers
            .get(&mapping_id)
            .map_or(At { map: 0, rule: 0 }, |w| w.at);
        let mut finding = error(
            code,
            message,
            Span::from_segments([
                "maps",
                at.map.to_string().as_str(),
                "properties",
                at.rule.to_string().as_str(),
            ]),
        );
        if let Some(span) = finding.span.as_mut() {
            span.pointer = format!("/payload{}", span.pointer);
        }
        finding.artifact_id = Some(artifact.artifact_id.clone());
        out.push(finding);
    }
    out
}

/// SOURCE-RESOLVE-1: a lifecycle property observed by more than one mapping
/// is refused, since its snapshots are reconciled by transitions, not
/// decided by source authority yet. One finding per such property, on the
/// rule of each mapping after the first (in artifact id order).
fn lifecycle_overlaps(
    mappings: &[(&ArtifactEnvelope, MappingIr)],
    model: &MappingModel,
) -> Vec<Diagnostic> {
    let mut writers: BTreeMap<(String, String), BTreeMap<&str, At>> = BTreeMap::new();
    for (artifact, mapping) in mappings {
        for (map_index, map) in mapping.maps.iter().enumerate() {
            let Some(object_type) = model.object_type_of(&map.target.entity) else {
                continue;
            };
            for (rule, lifecycle) in map.lifecycles.iter().enumerate() {
                writers
                    .entry((
                        object_type.0.clone(),
                        lifecycle.property.as_str().to_owned(),
                    ))
                    .or_default()
                    .entry(artifact.artifact_id.as_str())
                    .or_insert(At {
                        map: map_index,
                        rule,
                    });
            }
        }
    }
    let mut out = Vec::new();
    for ((object_type, property), observed) in writers {
        let ids: Vec<&str> = observed.keys().copied().collect();
        for (id, at) in observed.iter().skip(1) {
            let mut finding = error(
                codes::RESOLUTION_LIFECYCLE_OVERLAP,
                format!(
                    "{object_type}.{property} is observed by {}; source resolution does not decide lifecycle snapshots yet, so observe it from one mapping",
                    ids.join(", ")
                ),
                Span::from_segments([
                    "maps",
                    at.map.to_string().as_str(),
                    "lifecycles",
                    at.rule.to_string().as_str(),
                ]),
            );
            if let Some(span) = finding.span.as_mut() {
                span.pointer = format!("/payload{}", span.pointer);
            }
            finding.artifact_id = Some((*id).to_owned());
            out.push(finding);
        }
    }
    out
}

/// SOURCE-RESOLVE-1: the properties of `object_type` that hold its key
/// exactly: each the one component of a key without normalization.
pub(super) fn key_properties(object_type: &EffectiveObjectType) -> BTreeSet<String> {
    object_type
        .keys
        .iter()
        .filter(|key| key.normalize.values().all(Vec::is_empty))
        .filter_map(|key| match key.components.as_slice() {
            [property] => Some(property.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// The finding for one resolved property, and the mapping it is reported
/// on; `None` when its writers decide it explicitly.
fn field_finding(
    object_type: &str,
    property: &str,
    writers: &BTreeMap<String, Writer>,
    model: &MappingModel,
) -> Option<(&'static str, String, String)> {
    let listed = || {
        writers
            .iter()
            .map(|(id, w)| format!("{id} ({})", describe(w.authority)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let first = |pred: &dyn Fn(&Writer) -> bool| {
        writers
            .iter()
            .find(|(_, w)| pred(w))
            .map_or_else(String::new, |(id, _)| id.clone())
    };
    if let Some((id, _)) = writers.iter().find(|(_, w)| w.declared.len() > 1) {
        return Some((
            codes::RESOLUTION_AMBIGUOUS,
            format!(
                "{object_type}.{property}: mapping {id} declares more than one authority for it across its maps"
            ),
            id.clone(),
        ));
    }
    if writers.len() > 1 && writers.values().any(|w| w.authority.is_none()) {
        return Some((
            codes::RESOLUTION_UNDECLARED,
            format!(
                "{object_type}.{property} is written by {}; declare who decides it (system_of_record, precedence, check or conflict) on every writer",
                listed()
            ),
            first(&|w| w.authority.is_none()),
        ));
    }
    let conflicts = writers
        .values()
        .filter(|w| w.authority == Some(SourceAuthority::Conflict))
        .count();
    if conflicts > 0 && conflicts != writers.len() {
        return Some((
            codes::RESOLUTION_MIXED,
            format!(
                "{object_type}.{property}: conflict peers cannot be mixed with other authorities ({})",
                listed()
            ),
            first(&|w| w.authority == Some(SourceAuthority::Conflict)),
        ));
    }
    if conflicts > 0 {
        let nullable = model
            .env()
            .record(object_type)
            .and_then(|record| record.fields.get(property))
            .is_some_and(|field| matches!(field.ty, Type::Optional(_)));
        return (!nullable).then(|| {
            (
                codes::RESOLUTION_CONFLICT_NOT_NULLABLE,
                format!(
                    "{object_type}.{property} is declared conflict, but it is not nullable: a disagreement stores null"
                ),
                first(&|_| true),
            )
        });
    }
    let records = writers
        .values()
        .filter(|w| w.authority == Some(SourceAuthority::SystemOfRecord))
        .count();
    let mut ranks = BTreeSet::new();
    let mut duplicate = false;
    for writer in writers.values() {
        if let Some(SourceAuthority::Precedence { rank }) = writer.authority {
            duplicate |= !ranks.insert(rank);
        }
    }
    if records > 1 || (records == 1 && !ranks.is_empty()) || duplicate {
        return Some((
            codes::RESOLUTION_AMBIGUOUS,
            format!(
                "{object_type}.{property}: the declarations do not single out who decides ({}); use one system_of_record or distinct precedence ranks",
                listed()
            ),
            first(&|w| {
                matches!(
                    w.authority,
                    Some(SourceAuthority::SystemOfRecord | SourceAuthority::Precedence { .. })
                )
            }),
        ));
    }
    if records == 0 && ranks.is_empty() {
        return Some((
            codes::RESOLUTION_NO_AUTHORITY,
            format!(
                "{object_type}.{property}: every writer is a check ({}); name a system_of_record or a precedence",
                listed()
            ),
            first(&|_| true),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Resolution, ResolutionTable, Writer};
    use norn_ir::ArtifactEnvelope;
    use norn_ir::mapping::{MappingIr, SourceAuthority};
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, BTreeSet};

    /// A mapping of `t.T` from map `maps` (each a JSON object map without
    /// its source and target).
    fn mapping(maps: &[Value]) -> MappingIr {
        let maps: Vec<Value> = maps
            .iter()
            .enumerate()
            .map(|(i, map)| {
                let mut map = map.clone();
                map["id"] = json!(format!("m{i}"));
                map["source"] = json!({"dataset": "d"});
                map["target"] = json!({"entity": "t.T.entity", "interface": "t.I.interface"});
                map["sourceRecord"] = json!("input.id");
                map
            })
            .collect();
        serde_json::from_value(json!({
            "format": "norn.mapping/1",
            "source": {"schemaId": "s", "version": "1.0.0", "sha256": "a".repeat(64)},
            "interfaces": [],
            "maps": maps,
        }))
        .expect("mapping")
    }

    fn writers_of(mappings: &[(&str, MappingIr)], property: &str) -> Vec<String> {
        let table = ResolutionTable::of(mappings.iter().map(|(id, m)| (*id, m)), |_| {
            Some(("t.T".to_owned(), BTreeSet::from(["id".to_owned()])))
        });
        table
            .writers("t.T", property)
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect()
    }

    #[test]
    fn only_proven_identity_writes_of_the_key_are_exempt() {
        let subject = |property: &str| json!({"property": property, "value": "input.id"});
        let both = [
            (
                "a",
                mapping(&[
                    json!({"subject": "input.id", "properties": [subject("id"), subject("label")]}),
                ]),
            ),
            (
                "b",
                mapping(&[
                    json!({"subject": "input.id", "properties": [subject("id"), subject("label")]}),
                ]),
            ),
        ];
        // Every writer of the key writes the subject to it: no resolution.
        assert!(writers_of(&both, "id").is_empty());
        // `label` is no key: written as the subject, it is still resolved.
        assert_eq!(writers_of(&both, "label"), ["a", "b"]);
        // One writer of the key writes something else: every writer counts.
        let mixed = [
            (
                "a",
                mapping(&[json!({"subject": "input.id", "properties": [subject("id")]})]),
            ),
            (
                "b",
                mapping(&[
                    json!({"subject": "input.id", "properties": [{"property": "id", "value": "input.other"}]}),
                ]),
            ),
        ];
        assert_eq!(writers_of(&mixed, "id"), ["a", "b"]);
    }

    #[test]
    fn two_mappings_observing_one_lifecycle_property_are_refused() {
        let envelope = |id: &str, kind: &str, payload: Value| -> ArtifactEnvelope {
            serde_json::from_value(json!({
                "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": "t",
                "name": id, "version": "1.0.0", "lifecycle": "draft", "scope": {},
                "dependencies": [], "provenance": {"source": "system"}, "payload": payload,
            }))
            .expect("envelope")
        };
        let entity = envelope(
            "t.T.entity",
            "entity",
            json!({"entityVersion": "2.0", "objectType": {
                "name": "t.T",
                "properties": [
                    {"id": "id", "type": {"kind": "text"}},
                    {"id": "status", "type": {"kind": "enum",
                        "values": [{"value": "open"}, {"value": "approved"}]}, "default": "open"},
                ],
                "identity": [{"name": "primary", "properties": ["id"]}],
                "lifecycles": [{"property": "status", "initial": ["open"], "terminal": ["approved"],
                    "transitions": [{"id": "approve", "from": ["open"], "to": "approved"}]}],
            }}),
        );
        let model =
            super::MappingModel::from_artifacts(std::slice::from_ref(&entity)).expect("model");
        let observe = mapping(&[json!({
            "subject": "input.id",
            "properties": [{"property": "id", "value": "input.id"}],
            "lifecycles": [{"property": "status", "state": "input.status"}],
        })]);
        let a = envelope(
            "a.mapping",
            "mapping",
            serde_json::to_value(&observe).expect("wire"),
        );
        let b = envelope(
            "b.mapping",
            "mapping",
            serde_json::to_value(&observe).expect("wire"),
        );
        let codes = |mappings: &[(&ArtifactEnvelope, MappingIr)]| -> Vec<(String, Option<String>)> {
            super::admission_findings(mappings, &model)
                .into_iter()
                .map(|d| (d.code.as_str().to_owned(), d.artifact_id))
                .collect()
        };
        assert!(codes(&[(&a, observe.clone())]).is_empty());
        assert_eq!(
            codes(&[(&a, observe.clone()), (&b, observe)]),
            [(
                super::codes::RESOLUTION_LIFECYCLE_OVERLAP.to_owned(),
                Some("b.mapping".to_owned())
            )]
        );
    }

    fn table(writers: &[(&str, Option<SourceAuthority>)]) -> ResolutionTable {
        let writers = writers
            .iter()
            .map(|(id, authority)| {
                (
                    (*id).to_owned(),
                    Writer {
                        authority: *authority,
                        declared: BTreeSet::from([*authority]),
                        at: super::At { map: 0, rule: 0 },
                    },
                )
            })
            .collect();
        ResolutionTable {
            fields: BTreeMap::from([(("t.T".to_owned(), "q".to_owned()), writers)]),
        }
    }

    fn reports(entries: &[(&str, i64)]) -> BTreeMap<String, i64> {
        entries.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    #[test]
    fn the_system_of_record_decides_whatever_reported_first() {
        let t = table(&[
            ("erp", Some(SourceAuthority::SystemOfRecord)),
            ("odoo", Some(SourceAuthority::Check)),
        ]);
        let both = reports(&[("odoo", 2458), ("erp", 2765)]);
        assert_eq!(
            t.resolve("t.T", "q", &both),
            Some(Resolution::Decided {
                mapping: "erp",
                value: &2765
            })
        );
        let check_only = reports(&[("odoo", 2458)]);
        assert_eq!(
            t.resolve("t.T", "q", &check_only),
            Some(Resolution::Undecided)
        );
        assert!(t.governs("t.T"));
        assert!(!t.governs("t.U"));
    }

    #[test]
    fn precedence_takes_the_best_ranked_report_and_peers_conflict() {
        let t = table(&[
            ("a", Some(SourceAuthority::Precedence { rank: 1 })),
            ("b", Some(SourceAuthority::Precedence { rank: 0 })),
        ]);
        assert_eq!(
            t.resolve("t.T", "q", &reports(&[("a", 1)])),
            Some(Resolution::Decided {
                mapping: "a",
                value: &1
            })
        );
        assert_eq!(
            t.resolve("t.T", "q", &reports(&[("a", 1), ("b", 2)])),
            Some(Resolution::Decided {
                mapping: "b",
                value: &2
            })
        );
        let peers = table(&[
            ("a", Some(SourceAuthority::Conflict)),
            ("b", Some(SourceAuthority::Conflict)),
        ]);
        assert_eq!(
            peers.resolve("t.T", "q", &reports(&[("a", 1), ("b", 1)])),
            Some(Resolution::Agreed { value: &1 })
        );
        assert_eq!(
            peers.resolve("t.T", "q", &reports(&[("a", 1), ("b", 2)])),
            Some(Resolution::Conflict)
        );
        // A single undeclared writer is not resolved at all.
        assert_eq!(
            table(&[("a", None)]).resolve("t.T", "q", &reports(&[("a", 1)])),
            None
        );
    }
}
