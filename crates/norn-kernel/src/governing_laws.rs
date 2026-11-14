//! Governing-law resolution and evaluation on the shared metered expression engine.
use crate::decision_commit::DataQualityRuleClassification;
use crate::{FactKey, TypeCatalog, cell_host::JsonObject};
use norn_expr::{EvaluationBudget, Expression, Value};
use norn_ir::{
    ArtifactEnvelope, DataQualityOutcome, DataQualityRuleRef, DataQualitySeverity, LawIR,
    LawModality, LawResponse, LawRoot, LawSeverity, ObjectTypeName,
};
use serde::{Deserialize, de::IntoDeserializer};

/// A retained governing law cannot be evaluated completely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LawEvaluationError {
    /// Inherited or effective-instance laws require B2 resolution.
    #[error("core.laws.unsupported_semantics: law {0} scope unsupported until B2")]
    UnsupportedUntilB2(String),
    /// The exact temporal/response contract is unsupported in CP2.
    #[error("core.laws.unsupported_semantics: law {0} requires unsupported semantics")]
    Unsupported(String),
    /// A retained declaration or subject cannot be resolved.
    #[error("core.laws.unresolved_scope: {0}")]
    Invalid(String),
    /// The cumulative expression budget was exhausted.
    #[error("core.laws.unsupported_semantics: governing-law evaluation budget exhausted")]
    BudgetExceeded,
    /// A native write violates a directly attached safety law.
    #[error("core.laws.violated: {}", .0.message)]
    Violated(Box<LawFailure>),
    /// LAW-LINK-1: more subjects link to one written target than a commit
    /// re-checks; the write is refused, never checked partially.
    #[error("core.compute.bound_exceeded: LAW-LINK-1: more than {limit} subjects link to {target}")]
    LinkFanOutExceeded {
        /// The written target, as `type/key`.
        target: String,
        /// The bound (`commit_check::MAX_LINK_LAW_SUBJECTS`, crate-private).
        limit: usize,
    },
}
impl LawEvaluationError {
    /// Stable Core 05 diagnostic, independent of diagnostic detail.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedUntilB2(_) | Self::Unsupported(_) | Self::BudgetExceeded => {
                "core.laws.unsupported_semantics"
            }
            Self::Invalid(_) => "core.laws.unresolved_scope",
            Self::Violated(_) => "core.laws.violated",
            Self::LinkFanOutExceeded { .. } => "core.compute.bound_exceeded",
        }
    }
}
/// One evaluated law violation; findings retain the actual law pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LawFailure {
    /// Whether the retained declaration refuses native writes.
    pub refuses_native: bool,
    /// Exact evaluation outcome retained with observation evidence.
    pub outcome: DataQualityOutcome,
    /// Exact governing declaration identity.
    pub rule_ref: DataQualityRuleRef,
    /// Declared severity, independent of proof classification.
    pub severity: DataQualitySeverity,
    /// Declared explanation.
    pub message: String,
    /// Original invariant refusal details for the existing attempt journal.
    pub invariant: Option<Box<BoundInvariantFailure>>,
    /// LAW-LINK-1: the exact version of each link target the failing instance
    /// read, by link id, and (LAW-LINK-MANY-1) each source an inverse role
    /// read, by `role/key`. Non-empty exactly when the outcome is `Linked`.
    pub links: std::collections::BTreeMap<String, norn_ir::FactRef>,
    /// DIAG-REASON-1: the stored values the law read, by path (`total`,
    /// `ledger_entry.amount`), so a refusal can say what was checked.
    pub values: std::collections::BTreeMap<String, norn_ir::RuntimeValue>,
}

/// LAW-LINK-1: one link target in the snapshot a law is checked against.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkedTarget {
    /// The target object (its concrete type and key).
    pub key: FactKey,
    /// The target's version in that snapshot.
    pub version: u64,
    /// Its values in that snapshot.
    pub values: JsonObject,
}

/// LAW-LINK-1: what a subject's one-hop, to-one links reach in the snapshot a
/// law is checked against. Hosts supply the state the commit or observation is
/// admitted on; a caller that cannot read links uses [`NoLinkTargets`], and a
/// law that reads one then refuses rather than reading `null`.
pub trait LinkTargets {
    /// The live target of `link` on the subject and its values; `None` when
    /// the subject has no target on it.
    ///
    /// # Errors
    /// The snapshot cannot be read, or the target's values cannot be proven.
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError>;

    /// LAW-LINK-MANY-1: the live objects of `source` (or a type that is one)
    /// whose forward `link` targets the subject, read through its inverse
    /// `role`, with their values; empty when none does. A caller that cannot
    /// read them refuses, so a law never sums a partial set.
    ///
    /// # Errors
    /// The snapshot cannot be read, or a source's values cannot be proven.
    fn sources(
        &mut self,
        role: &str,
        source: &ObjectTypeName,
        link: &str,
    ) -> Result<Vec<LinkedTarget>, LawEvaluationError> {
        let _ = (source, link);
        Err(LawEvaluationError::Unsupported(format!(
            "LAW-LINK-MANY-1: inverse role {role} is read by a governing law, but this caller cannot read it"
        )))
    }

    /// Whether laws that read links are left for a later pass (an observation
    /// prepared before its bundle's links are known). Such laws are skipped.
    fn defers(&self) -> bool {
        false
    }
}

/// An observation prepared before its links are known: link laws are left to
/// the pass that runs once the bundle's links are (see
/// `ObservationAuthorization::refresh_link_findings`).
pub struct DeferLinkLaws;

impl LinkTargets for DeferLinkLaws {
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError> {
        Err(LawEvaluationError::Unsupported(format!(
            "link {link} is read before the bundle's links are known"
        )))
    }

    fn defers(&self) -> bool {
        true
    }
}

/// A caller that reads no links: a governing law that reads one refuses.
pub struct NoLinkTargets;

impl LinkTargets for NoLinkTargets {
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError> {
        Err(LawEvaluationError::Unsupported(format!(
            "link {link} is read by a governing law, but this caller cannot read links"
        )))
    }
}

/// LAW-LINK-1 / LAW-LINK-MANY-1: one link a governing law reads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkRead {
    /// A declared to-one forward link of the subject (`self.ledger_entry`):
    /// one target, or `null`.
    Forward(String),
    /// An inverse role of the subject (`self.lines`): every object of
    /// `source` whose forward `link` targets the subject, as a list ordered
    /// by key. A law reads only those objects' own properties.
    Inverse {
        /// The inverse role the law reads.
        role: String,
        /// The object type that declares the forward link.
        source: ObjectTypeName,
        /// The forward link id on `source`.
        link: String,
    },
}

impl LinkRead {
    /// The member of `self` the law reads.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Forward(link) => link,
            Self::Inverse { role, .. } => role,
        }
    }
}

/// The declared forward links with inverse `role` whose target `subject`
/// reaches and that many sources may share (`from: many`): the declarations
/// an inverse read of `subject.role` resolves to. A to-one inverse role is
/// not one; the checker refuses a law that reads it.
fn inverse_declarations(
    catalog: &TypeCatalog<'_>,
    subject: &ObjectTypeName,
    role: &str,
) -> Vec<(ObjectTypeName, String)> {
    let mut found = Vec::new();
    for source in catalog.object_type_names() {
        for link in catalog.declared_links(&source) {
            if link.inverse.as_ref().is_some_and(|id| id.as_str() == role)
                && reaches(catalog, subject, &link.target)
                && link.cardinality.from == norn_ir::Multiplicity::Many
            {
                let entry = (source.clone(), link.id.as_str().to_owned());
                if !found.contains(&entry) {
                    found.push(entry);
                }
            }
        }
    }
    found
}

/// LAW-LINK-1 / LAW-LINK-MANY-1: the links of `subject` a law reads, in
/// order: a declared to-one forward link and its target's properties, or an
/// inverse role and its sources' own properties. A to-many forward link, a
/// second hop, or an inverse role more than one link declares refuses.
///
/// # Errors
/// An unparsable expression, a to-many forward link, a second hop, or an
/// ambiguous inverse role.
pub fn law_link_reads(
    catalog: &TypeCatalog<'_>,
    subject: &ObjectTypeName,
    law: &LawIR,
) -> Result<Vec<LinkRead>, LawEvaluationError> {
    let declared = |owner: &ObjectTypeName, id: &str| {
        catalog.ancestry(owner).into_iter().find_map(|definition| {
            catalog
                .declared_links(&definition.name)
                .into_iter()
                .find(|link| link.id.as_str() == id)
        })
    };
    let mut reads = std::collections::BTreeSet::new();
    let response = match &law.response {
        LawResponse::Predicate { expression } => Some(expression.as_str()),
        LawResponse::Invariant { .. }
        | LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => None,
    };
    for text in law
        .condition
        .iter()
        .map(norn_ir::ZenExpression::as_str)
        .chain(response)
    {
        let paths = norn_expr::expression_read_paths(text, "self")
            .map_err(|error| LawEvaluationError::Invalid(format!("{}: {error}", law.id)))?;
        for path in paths {
            let Some(first) = path.first() else {
                continue;
            };
            if let Some(link) = declared(subject, first) {
                let second_hop = path
                    .get(1)
                    .is_some_and(|member| declared(&link.target, member).is_some());
                if link.cardinality.to != norn_ir::Multiplicity::One || second_hop {
                    return Err(LawEvaluationError::Unsupported(format!(
                        "{}: LAW-LINK-1 reads one to-one link and its target's properties",
                        law.id
                    )));
                }
                reads.insert(LinkRead::Forward(link.id.as_str().to_owned()));
                continue;
            }
            let inverse = inverse_declarations(catalog, subject, first);
            let [(source, link)] = inverse.as_slice() else {
                if inverse.is_empty() {
                    continue;
                }
                return Err(LawEvaluationError::Unsupported(format!(
                    "{}: LAW-LINK-MANY-1: inverse role {first} is declared by more than one link",
                    law.id
                )));
            };
            let hops = norn_expr::expression_element_paths(text, "self")
                .map_err(|error| LawEvaluationError::Invalid(format!("{}: {error}", law.id)))?
                .get(std::slice::from_ref(first))
                .is_some_and(|reads| {
                    reads.iter().any(|read| {
                        read.first()
                            .is_none_or(|member| declared(source, member).is_some())
                    })
                });
            if hops
                || path
                    .get(1)
                    .is_some_and(|member| declared(source, member).is_some())
            {
                return Err(LawEvaluationError::Unsupported(format!(
                    "{}: LAW-LINK-MANY-1 reads an inverse role and its sources' own properties",
                    law.id
                )));
            }
            reads.insert(LinkRead::Inverse {
                role: first.clone(),
                source: source.clone(),
                link: link.clone(),
            });
        }
    }
    Ok(reads.into_iter().collect())
}

/// Whether any governing law of `object_type` reads a link (LAW-LINK-1).
///
/// # Errors
/// As [`TypeCatalog::governing_laws`] and [`law_link_reads`].
pub fn reads_links(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
) -> Result<bool, LawEvaluationError> {
    for artifact in catalog.governing_laws(object_type)? {
        if !law_link_reads(catalog, object_type, &decode(artifact)?)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// LAW-LINK-1 / LAW-LINK-MANY-1: the links governing laws read, once per
/// catalog. Empty for a catalog without link laws, which then pays nothing
/// for them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkLawIndex {
    /// (subject type, forward link, declared target type): a write of a
    /// target re-checks the subjects that link to it.
    pub forward: Vec<(ObjectTypeName, String, ObjectTypeName)>,
    /// (subject type, source type, forward link): a write of a source, or of
    /// its link, re-checks the subject its link reaches.
    pub inverse: Vec<(ObjectTypeName, ObjectTypeName, String)>,
}

impl LinkLawIndex {
    /// Whether no governing law reads a link.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty() && self.inverse.is_empty()
    }
}

/// LAW-LINK-1 / LAW-LINK-MANY-1: every link a governing law reads, by
/// subject type.
///
/// # Errors
/// As [`law_link_reads`].
pub fn link_law_index(catalog: &TypeCatalog<'_>) -> Result<LinkLawIndex, LawEvaluationError> {
    let mut index = LinkLawIndex::default();
    if catalog.law_artifacts.is_empty() {
        return Ok(index);
    }
    for subject in &catalog.object_type_names() {
        for artifact in catalog.governing_laws(subject)? {
            let law = decode(artifact)?;
            for read in law_link_reads(catalog, subject, &law)? {
                match read {
                    LinkRead::Forward(link) => {
                        let declared =
                            catalog
                                .ancestry(subject)
                                .into_iter()
                                .find_map(|definition| {
                                    catalog
                                        .declared_links(&definition.name)
                                        .into_iter()
                                        .find(|declared| declared.id.as_str() == link)
                                });
                        if let Some(declared) = declared {
                            let entry = (subject.clone(), link, declared.target.clone());
                            if !index.forward.contains(&entry) {
                                index.forward.push(entry);
                            }
                        }
                    }
                    LinkRead::Inverse { source, link, .. } => {
                        let entry = (subject.clone(), source, link);
                        if !index.inverse.contains(&entry) {
                            index.inverse.push(entry);
                        }
                    }
                }
            }
        }
    }
    Ok(index)
}

/// Whether `target` is (or extends, or implements) a link's declared target.
#[must_use]
pub fn reaches(
    catalog: &TypeCatalog<'_>,
    target: &ObjectTypeName,
    declared: &ObjectTypeName,
) -> bool {
    target == declared
        || catalog
            .ancestry(target)
            .iter()
            .any(|ty| ty.name == *declared)
        || catalog.implements(target, declared.as_str())
}

/// DIAG-REASON-1: the stored values a law read, by path: its subject's own
/// properties (`total`) and, through a to-one link, its target's
/// (`ledger_entry.amount`). A refusal discloses them as its operands.
fn law_operands(
    law: &LawIR,
    subject: &JsonObject,
    targets: &std::collections::BTreeMap<String, JsonObject>,
    sources: &std::collections::BTreeMap<String, Vec<JsonObject>>,
) -> std::collections::BTreeMap<String, norn_ir::RuntimeValue> {
    let response = if let LawResponse::Predicate { expression } = &law.response {
        Some(expression.as_str())
    } else {
        None
    };
    let mut operands = std::collections::BTreeMap::new();
    for text in law
        .condition
        .iter()
        .map(norn_ir::ZenExpression::as_str)
        .chain(response)
    {
        let Ok(paths) = norn_expr::expression_read_paths(text, "self") else {
            continue;
        };
        for path in paths {
            // An inverse role's sources are reported by what the law's
            // closures read from them, below.
            if path.first().is_some_and(|role| sources.contains_key(role)) {
                continue;
            }
            let value = match path.as_slice() {
                [property] => subject.get(property),
                [link, property] => targets.get(link).and_then(|target| target.get(property)),
                _ => None,
            };
            if let Some(value) = value {
                operands.insert(path.join("."), value.clone());
            }
        }
        // LAW-LINK-MANY-1: a property the law reads from an inverse role's
        // sources (`map(self.lines, #.amount)`) is the list of every source's
        // value, in the order the law read them (`lines.amount`).
        let Ok(elements) = norn_expr::expression_element_paths(text, "self") else {
            continue;
        };
        for (collection, reads) in elements {
            let [role] = collection.as_slice() else {
                continue;
            };
            let Some(rows) = sources.get(role) else {
                continue;
            };
            for read in reads {
                let [property] = read.as_slice() else {
                    continue;
                };
                let values = rows
                    .iter()
                    .map(|row| row.get(property).cloned().unwrap_or_default())
                    .collect();
                operands.insert(
                    format!("{role}.{property}"),
                    norn_ir::RuntimeValue::Array(values),
                );
            }
        }
    }
    operands
}

/// The expression object of stored values.
fn expr_object(values: &JsonObject) -> Result<Value, LawEvaluationError> {
    values
        .iter()
        .map(|(name, value)| crate::decision_commit::expr_value(value).map(|v| (name.clone(), v)))
        .collect::<Result<_, _>>()
        .map(Value::Object)
        .map_err(LawEvaluationError::Invalid)
}

/// The invariant outcome and owner pin from the evaluated definition snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundInvariantFailure {
    /// Exact invariant identity, message and evaluated outcome.
    pub failure: crate::InvariantFailure,
    /// Canonical version of the declaration that owns the invariant.
    pub version: String,
}
fn decode(artifact: &ArtifactEnvelope) -> Result<LawIR, LawEvaluationError> {
    let decoded: Result<LawIR, serde::de::value::Error> =
        LawIR::deserialize(artifact.payload.to_json_value().into_deserializer());
    decoded.map_err(|error| LawEvaluationError::Invalid(error.to_string()))
}
impl TypeCatalog<'_> {
    /// LAW-COMPLETE-1: never silently omit an applicable inherited/effective law.
    /// LAW-INTERFACE-1: a law scoped to exactly one type or interface governs it
    /// and every type that extends or implements it, once per governed type.
    ///
    /// # Errors
    /// Unknown subject, malformed declaration, or scope unsupported until B2.
    pub fn governing_laws(
        &self,
        object_type: &ObjectTypeName,
    ) -> Result<Vec<&ArtifactEnvelope>, LawEvaluationError> {
        if !self.contains(object_type) && !self.law_artifacts.is_empty() {
            if matches!(
                object_type.as_str(),
                crate::action_commit::ACTION_CALL_TYPE
                    | crate::action_commit::OWNED_ROWS_TYPE
                    | crate::snapshot_observe::OBSERVED_TOMBSTONE_TYPE
                    | crate::snapshot_observe::SOURCE_OWNERSHIP_TYPE
                    | crate::naming_series::NAMING_SERIES_COUNTER_TYPE
                    | "norn.PendingLinks"
            ) {
                return Ok(Vec::new());
            }
            return Err(LawEvaluationError::Invalid(object_type.as_str().to_owned()));
        }
        if !self.law_artifacts.is_empty() {
            // LAW-COMPLETE-1: unresolved interface ancestors cannot hide retained laws.
            let _ = self
                .invariants(object_type)
                .map_err(|missing| LawEvaluationError::Invalid(missing.as_str().to_owned()))?;
        }
        let is_a = |name: &str| {
            self.lineage(object_type)
                .iter()
                .any(|ty| ty.as_str() == name)
                || self.implements(object_type, name)
        };
        let mut laws = Vec::new();
        for artifact in &self.law_artifacts {
            let law = decode(artifact)?;
            if !is_a(&law.subject) && !law.scope.object_types.iter().any(|ty| is_a(ty)) {
                continue;
            }
            if !is_a(&law.subject)
                || !matches!(&law.scope.root, LawRoot::Entity(name) if *name == law.subject)
                || law.scope.object_types != [law.subject.as_str()]
            {
                return Err(LawEvaluationError::UnsupportedUntilB2(law.id));
            }
            laws.push(*artifact);
        }
        Ok(laws)
    }
}
fn supported(law: &LawIR) -> bool {
    // An exception tree has no commit-path semantics until the per-write fold
    // lands; refusing keeps a law from being evaluated without its exceptions.
    law.exceptions.is_empty()
        && law.timing.is_none()
        && law.scope.lifecycle_states.is_empty()
        && law.scope.commitments.is_empty()
        && matches!(law.modality, LawModality::Always {} | LawModality::Never {})
        && match law.response {
            LawResponse::Predicate { .. } => true,
            LawResponse::Invariant { .. } => {
                matches!(law.modality, LawModality::Always {}) && law.condition.is_none()
            }
            LawResponse::Action { .. }
            | LawResponse::Transition { .. }
            | LawResponse::TranslationPreserves { .. } => false,
        }
}

fn invariant_binding<'a>(
    catalog: &TypeCatalog<'_>,
    subject: &ObjectTypeName,
    law: &'a LawIR,
) -> Result<Option<&'a str>, LawEvaluationError> {
    let LawResponse::Invariant {
        object_type,
        invariant_id,
    } = &law.response
    else {
        return Ok(None);
    };
    let owners = |name: &str| {
        catalog
            .contract_invariant_owners(name, invariant_id)
            .map_err(|missing| LawEvaluationError::Invalid(missing.as_str().to_owned()))
    };
    let declared = owners(&law.subject)?;
    if object_type != &law.subject || declared.len() != 1 || owners(subject.as_str())? != declared {
        return Err(LawEvaluationError::Invalid(format!(
            "{}: invariant reference is missing, ambiguous or outside its retained subject",
            law.id
        )));
    }
    Ok(Some(invariant_id))
}
fn pin(artifact: &ArtifactEnvelope, law: &LawIR) -> Result<DataQualityRuleRef, LawEvaluationError> {
    let bytes = norn_ir::canonical::canonical_json_of(law)
        .map_err(|e| LawEvaluationError::Invalid(e.to_string()))?;
    Ok(DataQualityRuleRef::Law {
        artifact_id: artifact.artifact_id.clone(),
        law_id: law.id.clone(),
        version: format!(
            "sha256:{}",
            norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
        ),
    })
}

fn law_failure(
    artifact: &ArtifactEnvelope,
    law: &LawIR,
    outcome: DataQualityOutcome,
    invariant: Option<Box<BoundInvariantFailure>>,
) -> Result<LawFailure, LawEvaluationError> {
    Ok(LawFailure {
        refuses_native: true,
        outcome,
        rule_ref: pin(artifact, law)?,
        severity: match law.severity {
            LawSeverity::Error => DataQualitySeverity::Error,
            LawSeverity::Warning => DataQualitySeverity::Warning,
        },
        message: format!("{}: {}", law.id, law.rationale),
        invariant,
        links: std::collections::BTreeMap::new(),
        values: std::collections::BTreeMap::new(),
    })
}

fn bind_invariant_failure(
    catalog: &TypeCatalog<'_>,
    failure: crate::InvariantFailure,
) -> Result<Box<BoundInvariantFailure>, LawEvaluationError> {
    let version = catalog
        .invariant_owner_version(&failure.owner)
        .map_err(|error| LawEvaluationError::Invalid(error.to_string()))?
        .ok_or_else(|| LawEvaluationError::Invalid("invariant owner is unavailable".into()))?;
    Ok(Box::new(BoundInvariantFailure { failure, version }))
}

/// LAW-COMMIT-1: preserve the governing law pin when the shared integrity
/// evaluator has already found the referenced invariant violation.
pub(crate) fn invariant_failure(
    catalog: &TypeCatalog<'_>,
    subject: &ObjectTypeName,
    failure: &crate::commit_check::InvariantFailure,
) -> Result<Option<LawFailure>, LawEvaluationError> {
    if failure.finding_outcome.is_some() {
        return Ok(None);
    }
    for artifact in catalog.governing_laws(subject)? {
        let law = decode(artifact)?;
        if !matches!(&law.response, LawResponse::Invariant { invariant_id, .. }
            if invariant_id == failure.id.as_str())
            || law.severity != LawSeverity::Error
        {
            continue;
        }
        if !supported(&law) {
            return Err(LawEvaluationError::Unsupported(law.id));
        }
        invariant_binding(catalog, subject, &law)?;
        return law_failure(
            artifact,
            &law,
            failure.outcome.clone(),
            Some(bind_invariant_failure(catalog, failure.clone())?),
        )
        .map(Some);
    }
    Ok(None)
}

/// LAW-COMMIT-1 / OBS-EVIDENCE-1: display severity never relaxes governing authority.
pub(crate) fn classify(
    catalog: &TypeCatalog<'_>,
    rule_ref: &DataQualityRuleRef,
) -> DataQualityRuleClassification {
    for artifact in &catalog.law_artifacts {
        let Ok(law) = decode(artifact) else {
            continue;
        };
        if !supported(&law) || pin(artifact, &law).as_ref() != Ok(rule_ref) {
            continue;
        }
        let governs = |subject: &ObjectTypeName| {
            catalog.governing_laws(subject).is_ok_and(|laws| {
                laws.iter()
                    .any(|retained| std::ptr::eq(*retained, *artifact))
            }) && invariant_binding(catalog, subject, &law).is_ok()
        };
        if catalog.object_type_names().iter().any(governs) {
            return DataQualityRuleClassification::Integrity;
        }
    }
    DataQualityRuleClassification::Unverified
}
/// LAW-COMMIT-1 / OBS-EVIDENCE-1: evaluate retained direct laws, never caller payloads.
/// A law that reads a link refuses here (LAW-LINK-1 needs [`evaluate_linked`]).
///
/// # Errors
/// Incomplete scope, unsupported semantics, invalid context or exhausted fuel.
pub fn evaluate(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    remaining: &mut u64,
) -> Result<Vec<LawFailure>, LawEvaluationError> {
    evaluate_linked(catalog, key, values, &mut NoLinkTargets, remaining)
}

/// The link reads one law evaluation bound: each read's pin, and the values
/// of every to-one target and inverse-role source, by link or role.
#[derive(Default)]
struct BoundLinks {
    pinned: std::collections::BTreeMap<String, norn_ir::FactRef>,
    targets: std::collections::BTreeMap<String, JsonObject>,
    sources: std::collections::BTreeMap<String, Vec<JsonObject>>,
}

/// LAW-LINK-1 / LAW-LINK-MANY-1: bind each link a law reads into the
/// subject's expression object: a to-one target (or `null`), or an inverse
/// role's sources as a list ordered by key, each pinned at the version read.
fn bind_links(
    key: &FactKey,
    reads: Vec<LinkRead>,
    links: &mut dyn LinkTargets,
    object: &mut std::collections::BTreeMap<String, Value>,
) -> Result<BoundLinks, LawEvaluationError> {
    let mut bound = BoundLinks::default();
    for read in reads {
        match read {
            LinkRead::Forward(link) => {
                let value = match links.target(&link)? {
                    Some(target) => {
                        bound.targets.insert(link.clone(), target.values.clone());
                        bound.pinned.insert(
                            link.clone(),
                            norn_ir::FactRef::new(
                                target.key.object_type.as_str(),
                                target.key.key.clone(),
                                target.version,
                            ),
                        );
                        expr_object(&target.values)?
                    }
                    None => Value::Null,
                };
                object.insert(link, value);
            }
            LinkRead::Inverse { role, source, link } => {
                let mut found = links.sources(&role, &source, &link)?;
                if found.len() > crate::commit_check::MAX_LINK_LAW_SUBJECTS {
                    return Err(LawEvaluationError::LinkFanOutExceeded {
                        target: format!("{}/{}", key.object_type.as_str(), key.key),
                        limit: crate::commit_check::MAX_LINK_LAW_SUBJECTS,
                    });
                }
                found.sort_by(|left, right| left.key.key.cmp(&right.key.key));
                found.dedup_by(|left, right| left.key == right.key);
                let mut items = Vec::with_capacity(found.len());
                let mut rows = Vec::with_capacity(found.len());
                for item in found {
                    // Each source is pinned at the exact version read, by
                    // `role/key`, so a retained finding replays the same set.
                    bound.pinned.insert(
                        format!("{role}/{}", item.key.key),
                        norn_ir::FactRef::new(
                            item.key.object_type.as_str(),
                            item.key.key.clone(),
                            item.version,
                        ),
                    );
                    items.push(expr_object(&item.values)?);
                    rows.push(item.values);
                }
                bound.sources.insert(role.clone(), rows);
                object.insert(role, Value::Array(items));
            }
        }
    }
    Ok(bound)
}

/// LAW-COMMIT-1 / LAW-LINK-1: evaluate retained direct laws over the subject's
/// values and, for a law that reads a to-one link, its target's values in the
/// same snapshot (`self.<link>` is `null` when there is no target).
///
/// # Errors
/// Incomplete scope, unsupported semantics, invalid context, an unreadable
/// link target, or exhausted fuel.
pub fn evaluate_linked(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    links: &mut dyn LinkTargets,
    remaining: &mut u64,
) -> Result<Vec<LawFailure>, LawEvaluationError> {
    let artifacts = catalog.governing_laws(&key.object_type)?;
    if artifacts.is_empty() {
        return Ok(Vec::new());
    }
    let Value::Object(subject) = expr_object(values)? else {
        return Err(LawEvaluationError::Invalid("subject values".into()));
    };
    let mut failures = Vec::new();
    for artifact in artifacts {
        let law = decode(artifact)?;
        if !supported(&law) {
            return Err(LawEvaluationError::Unsupported(law.id));
        }
        let reads = law_link_reads(catalog, &key.object_type, &law)?;
        if !reads.is_empty() && links.defers() {
            continue;
        }
        let mut object = subject.clone();
        let bound = bind_links(key, reads, links, &mut object)?;
        let context = Value::object([("self", Value::Object(object))]);
        let invariant_id = invariant_binding(catalog, &key.object_type, &law)?;
        let mut invariant_failure = None;
        let activation = match &law.condition {
            Some(expr) => boolean(expr.as_str(), &context, remaining)?,
            None => Ok(true),
        };
        let result = match activation {
            Ok(false) => continue,
            Err(outcome) => Err(outcome),
            Ok(true) => match &law.response {
                LawResponse::Predicate { expression } => {
                    boolean(expression.as_str(), &context, remaining)?.and_then(|value| {
                        if value == matches!(law.modality, LawModality::Always {}) {
                            Ok(())
                        } else {
                            Err(DataQualityOutcome::NotSatisfied)
                        }
                    })
                }
                LawResponse::Invariant { .. } => {
                    let id =
                        invariant_id.ok_or_else(|| LawEvaluationError::Invalid(law.id.clone()))?;
                    let failures = crate::commit_check::evaluate_named_invariant(
                        catalog, key, values, remaining, id,
                    )
                    .map_err(|error| match error {
                        crate::commit_check::InvariantEvaluationError::BudgetExceeded => {
                            LawEvaluationError::BudgetExceeded
                        }
                        other @ (crate::commit_check::InvariantEvaluationError::InvalidEffectiveValues(_)
| crate::commit_check::InvariantEvaluationError::MissingInterface(_)) => LawEvaluationError::Invalid(format!("{other:?}")),
                    })?;
                    if let Some(failure) = failures.into_iter().next() {
                        let outcome = failure.outcome.clone();
                        invariant_failure = Some(bind_invariant_failure(catalog, failure)?);
                        Err(outcome)
                    } else {
                        Ok(())
                    }
                }
                LawResponse::Action { .. }
                | LawResponse::Transition { .. }
                | LawResponse::TranslationPreserves { .. } => {
                    return Err(LawEvaluationError::Unsupported(law.id));
                }
            },
        };
        if let Err(outcome) = result {
            let mut failure = law_failure(artifact, &law, outcome, invariant_failure)?;
            failure.values = law_operands(&law, values, &bound.targets, &bound.sources);
            failure.links = bound.pinned;
            failures.push(failure);
        }
    }
    Ok(failures)
}
fn boolean(
    text: &str,
    context: &Value,
    remaining: &mut u64,
) -> Result<Result<bool, DataQualityOutcome>, LawEvaluationError> {
    if *remaining == 0 {
        return Err(LawEvaluationError::BudgetExceeded);
    }
    *remaining = remaining.saturating_sub(1);
    let expression = match Expression::compile(text) {
        Ok(expr) => expr,
        Err(e) => {
            return Ok(Err(DataQualityOutcome::EvaluationFailed {
                reason: e.to_string(),
            }));
        }
    };
    let attempt = expression.evaluate_metered(context, EvaluationBudget::new(*remaining));
    *remaining = remaining.saturating_sub(attempt.units_used);
    Ok(match attempt.result {
        Err(norn_expr::ExprError::BudgetExhausted { .. }) => {
            return Err(LawEvaluationError::BudgetExceeded);
        }
        Err(e) => Err(DataQualityOutcome::EvaluationFailed {
            reason: e.to_string(),
        }),
        Ok(result) => match result.value() {
            Value::Bool(value) => Ok(*value),
            other @ (Value::Null
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)
            | Value::Object(_)) => Err(DataQualityOutcome::NonBoolean {
                actual: format!("{other:?}"),
            }),
        },
    })
}
