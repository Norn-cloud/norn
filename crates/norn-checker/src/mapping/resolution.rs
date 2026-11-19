//! SOURCE-RESOLVE-1: when several mappings (several source systems) write
//! one canonical property, the canonical value is decided by an explicitly
//! declared [`SourceAuthority`], never by whichever source wrote last.
//!
//! The [`ResolutionTable`] of an admitted set lists, for every canonical
//! property, the mappings that write it and the authority each declares
//! (a property rule's own `authority`, else its map's). Admission
//! ([`admission_findings`]) refuses a property that two mappings write when
//! any writer declares nothing, when the declarations do not single out who
//! decides, or when nobody may decide. The cell decides each written value
//! with [`ResolutionTable::resolve`] over the latest report of every writer,
//! so the outcome is a function of the reports, not of their arrival order.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Type;
use norn_ir::ArtifactEnvelope;
use norn_ir::mapping::{MappingIr, SourceAuthority};

use super::{MappingModel, codes};
use crate::entity_artifacts::{Decoded, decode_v2};
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
        let mut names = BTreeMap::new();
        for artifact in base.iter().chain(artifacts) {
            if let Decoded::Root(root) = decode_v2(artifact)
                && let norn_ir::EntityRoot::V2(root) = *root
            {
                names.insert(artifact.artifact_id.clone(), root.object_type.name.0);
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

    fn of<'a>(
        mappings: impl IntoIterator<Item = (&'a str, &'a MappingIr)>,
        type_of: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let mut fields: BTreeMap<(String, String), BTreeMap<String, Writer>> = BTreeMap::new();
        for (mapping_id, mapping) in mappings {
            for (map_index, map) in mapping.maps.iter().enumerate() {
                let Some(object_type) = type_of(&map.target.entity) else {
                    continue;
                };
                for (rule, property) in map.properties.iter().enumerate() {
                    // A rule whose value is the map's subject writes the key
                    // itself: every report on one subject agrees on it.
                    if property.value == map.subject && property.authority.is_none() {
                        continue;
                    }
                    let authority = property.authority.or(map.authority);
                    let writers = fields
                        .entry((object_type.clone(), property.property.as_str().to_owned()))
                        .or_default();
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
        |entity| model.object_type_of(entity).map(|name| name.0.clone()),
    );
    let mut out = Vec::new();
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
    use norn_ir::mapping::SourceAuthority;
    use std::collections::{BTreeMap, BTreeSet};

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
