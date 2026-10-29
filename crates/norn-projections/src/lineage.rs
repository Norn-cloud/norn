//! Technical lineage: the causal-chain traversal and the two-view projection
//! (T12-VIEW-2, #2718; ruling 2026-09-25 §18 "One object definition, two
//! projections").
//!
//! [`explain`] answers "why is this fact what it is?" over a
//! [`LineageInput`]: starting from one fact version it walks, breadth first,
//! to the ledger commit that wrote it, the decision that produced that
//! commit, the delivery the commit consumed (an observation, a timer and the
//! commit that scheduled it, or a command result), the fact versions the
//! commit read, and, through each provenance record, the source record, the
//! mapping version, and the upstream facts. Ontology links of the objects it
//! reaches (`technical.Mapping` `derived_from` `technical.Column`, `table`,
//! `dataset`, `source`) are followed the same way, so the chain ends at the
//! source system's tables and columns. There is no lineage store: every edge
//! is a commit, a provenance record, or an ordinary link.
//!
//! Every node has a stable id ([`norn_ir::FactRef::stable_id`],
//! [`norn_ir::LineageCommit::stable_id`], [`norn_ir::DecisionRef::stable_id`],
//! [`norn_ir::LineageObservation::stable_id`],
//! [`norn_ir::SourceRecordRef::stable_id`], [`norn_ir::object_stable_id`]),
//! and the chain lists nodes and edges in discovery order, so the same input
//! always yields the same chain.
//!
//! [`project_views`] splits one artifact set into its business view (types,
//! properties, and business links) and its technical view (technical-view
//! assets and the `backed_by`/`derived_from` bindings of business types):
//! two representations of the same canonical IR, neither a separate
//! ontology.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use norn_ir::{
    FactRef, InstanceProvenance, JsonObject, LineageCommit, LineageInput, LineageObject,
    LineageObservation, LineageRelation, LineageTrigger, LinkId, ObjectTypeDef, ObjectTypeName,
    PropertyId, SourceRecordRef, TechnicalType, UtcTimestamp, is_technical_type, object_stable_id,
};

/// What a [`LineageNode`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LineageNodeKind {
    /// One committed fact version.
    Fact,
    /// One ledger commit: a decision taken, or a direct command applied.
    Commit,
    /// The decision artifact version that produced a commit.
    Decision,
    /// One ingested observation.
    Observation,
    /// A timer or command result a commit consumed.
    Trigger,
    /// One record in an external source system.
    SourceRecord,
    /// A technical-view object (`technical.Mapping`, `technical.Column`, ...).
    Technical(TechnicalType),
    /// Any other ontology object.
    Object,
}

/// Why one node is explained by another; edges point from effect to cause.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LineageEdgeKind {
    /// A fact version → the commit that wrote it.
    WrittenBy,
    /// A commit → a fact version it read.
    Read,
    /// A commit → the decision that produced it.
    DecidedBy,
    /// A commit → the observation, timer, or result it consumed.
    Consumed,
    /// A timer → the commit that scheduled it.
    ScheduledBy,
    /// An observation or fact → the source record it came from.
    ObservedFrom,
    /// An observation or fact → the mapping version that produced it.
    MappedBy,
    /// `derivedFrom`: a fact → an upstream fact of its provenance, or an
    /// object → the target of its `derived_from` link.
    DerivedFrom,
    /// `backedBy`: an object → the target of its `backed_by` link.
    BackedBy,
    /// A source record → the `technical.Source` object of its system.
    RecordOf,
    /// Any other ontology link, by link id.
    Link(LinkId),
}

impl LineageEdgeKind {
    /// The edge's name: a fixed `camelCase` name, or the link id of a
    /// [`Self::Link`].
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::WrittenBy => "writtenBy",
            Self::Read => "read",
            Self::DecidedBy => "decidedBy",
            Self::Consumed => "consumed",
            Self::ScheduledBy => "scheduledBy",
            Self::ObservedFrom => "observedFrom",
            Self::MappedBy => "mappedBy",
            Self::DerivedFrom => LineageRelation::DerivedFrom.relation_name(),
            Self::BackedBy => LineageRelation::BackedBy.relation_name(),
            Self::RecordOf => "recordOf",
            Self::Link(link) => link.as_str(),
        }
    }
}

/// One node of a [`CausalChain`].
#[derive(Debug, Clone, PartialEq)]
pub struct LineageNode {
    /// The stable id.
    pub id: String,
    /// What the node is.
    pub kind: LineageNodeKind,
    /// A short human-readable label.
    pub label: String,
    /// The object type of a fact or an object node.
    pub object_type: Option<ObjectTypeName>,
    /// The natural key: a fact's or object's key, a decision's
    /// `artifactId@version`, an observation's id.
    pub key: Option<String>,
    /// When a commit or an observation was recorded.
    pub recorded_at: Option<UtcTimestamp>,
    /// The ledger events of a [`LineageNodeKind::Commit`]; empty otherwise.
    pub events: Vec<JsonObject>,
}

impl LineageNode {
    fn new(id: String, kind: LineageNodeKind, label: String) -> Self {
        Self {
            id,
            kind,
            label,
            object_type: None,
            key: None,
            recorded_at: None,
            events: Vec::new(),
        }
    }

    #[must_use]
    fn keyed(mut self, object_type: Option<&ObjectTypeName>, key: &str) -> Self {
        self.object_type = object_type.cloned();
        self.key = Some(key.to_owned());
        self
    }

    #[must_use]
    fn at(mut self, recorded_at: &UtcTimestamp) -> Self {
        self.recorded_at = Some(recorded_at.clone());
        self
    }
}

/// One edge of a [`CausalChain`], from effect to cause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineageEdge {
    /// The explained node's id.
    pub from: String,
    /// The explaining node's id.
    pub to: String,
    /// Why.
    pub kind: LineageEdgeKind,
    /// The mapping transform of a [`LineageEdgeKind::MappedBy`] edge, when
    /// the provenance record names one.
    pub detail: Option<String>,
}

/// The causal chain of one fact: every node that explains it and how, in
/// breadth-first discovery order from the root.
#[derive(Debug, Clone, PartialEq)]
pub struct CausalChain {
    /// The explained fact's stable id (the first node).
    pub root: String,
    /// Every node, in discovery order.
    pub nodes: Vec<LineageNode>,
    /// Every edge, in discovery order.
    pub edges: Vec<LineageEdge>,
}

impl CausalChain {
    /// The node with this id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&LineageNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The edges that explain node `id`, in discovery order.
    pub fn causes<'s>(&'s self, id: &'s str) -> impl Iterator<Item = &'s LineageEdge> + 's {
        self.edges.iter().filter(move |edge| edge.from == id)
    }
}

/// Why a lineage traversal failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageError {
    /// The input did not name a world.
    MissingWorld,
    /// A commit belongs to a different world from the scoped input.
    WorldMismatch {
        /// The world the input declares.
        expected: String,
        /// The world the commit declares.
        actual: String,
        /// The commit's stable id.
        commit: String,
    },
    /// No commit wrote the fact and no provenance is stored with it.
    UnknownFact {
        /// The fact's stable id.
        fact: String,
    },
    /// No version of the object was written or carries provenance.
    UnknownObject {
        /// The object's stable id.
        object: String,
    },
    /// Two commits claim to have written the same fact version.
    DuplicateWriter {
        /// The fact's stable id.
        fact: String,
    },
    /// Two commits share a world and ledger position.
    DuplicateCommit {
        /// The commit's stable id.
        commit: String,
    },
    /// Two observations share an id.
    DuplicateObservation {
        /// The observation's stable id.
        observation: String,
    },
    /// Two objects share a type and key.
    DuplicateObject {
        /// The object's stable id.
        object: String,
    },
}

impl fmt::Display for LineageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingWorld => write!(formatter, "lineage input must name one world"),
            Self::WorldMismatch {
                expected,
                actual,
                commit,
            } => write!(
                formatter,
                "{commit} belongs to {actual}, not the input world {expected}"
            ),
            Self::UnknownFact { fact } => {
                write!(formatter, "no commit or provenance record explains {fact}")
            }
            Self::UnknownObject { object } => {
                write!(formatter, "no version of {object} is in the lineage input")
            }
            Self::DuplicateWriter { fact } => {
                write!(formatter, "two commits claim to have written {fact}")
            }
            Self::DuplicateCommit { commit } => {
                write!(formatter, "{commit} appears twice in the lineage input")
            }
            Self::DuplicateObservation { observation } => {
                write!(
                    formatter,
                    "{observation} appears twice in the lineage input"
                )
            }
            Self::DuplicateObject { object } => {
                write!(formatter, "{object} appears twice in the lineage input")
            }
        }
    }
}

impl std::error::Error for LineageError {}

/// The input, indexed by what the traversal looks up.
struct Index<'a> {
    writers: BTreeMap<&'a FactRef, &'a LineageCommit>,
    commits: BTreeMap<String, &'a LineageCommit>,
    observations: BTreeMap<&'a str, &'a LineageObservation>,
    provenance: BTreeMap<&'a FactRef, Vec<&'a InstanceProvenance>>,
    objects: BTreeMap<String, &'a LineageObject>,
}

impl<'a> Index<'a> {
    fn new(input: &'a LineageInput) -> Result<Self, LineageError> {
        if input.world.trim().is_empty() {
            return Err(LineageError::MissingWorld);
        }
        let mut index = Self {
            writers: BTreeMap::new(),
            commits: BTreeMap::new(),
            observations: BTreeMap::new(),
            provenance: BTreeMap::new(),
            objects: BTreeMap::new(),
        };
        for commit in &input.commits {
            let id = commit.stable_id();
            if commit.world != input.world {
                return Err(LineageError::WorldMismatch {
                    expected: input.world.clone(),
                    actual: commit.world.clone(),
                    commit: id,
                });
            }
            if index.commits.insert(id.clone(), commit).is_some() {
                return Err(LineageError::DuplicateCommit { commit: id });
            }
            for fact in &commit.written {
                if index.writers.insert(fact, commit).is_some() {
                    return Err(LineageError::DuplicateWriter {
                        fact: fact.stable_id(),
                    });
                }
            }
        }
        for observation in &input.observations {
            if index
                .observations
                .insert(observation.id.as_str(), observation)
                .is_some()
            {
                return Err(LineageError::DuplicateObservation {
                    observation: observation.stable_id(),
                });
            }
        }
        for instance in &input.instances {
            index
                .provenance
                .entry(&instance.fact)
                .or_default()
                .extend(instance.provenance.iter());
        }
        for object in &input.objects {
            let id = object.stable_id();
            if index.objects.insert(id.clone(), object).is_some() {
                return Err(LineageError::DuplicateObject { object: id });
            }
        }
        Ok(index)
    }

    fn knows(&self, fact: &FactRef) -> bool {
        self.writers.contains_key(fact) || self.provenance.contains_key(fact)
    }
}

/// A discovered node still to expand.
enum Pending<'a> {
    Fact(FactRef),
    Commit(&'a LineageCommit),
    Observation(&'a LineageObservation),
    Object(String),
}

/// The breadth-first walk.
struct Walk<'a> {
    index: Index<'a>,
    nodes: Vec<LineageNode>,
    seen: BTreeSet<String>,
    edges: Vec<LineageEdge>,
    seen_edges: BTreeSet<(String, String, LineageEdgeKind)>,
    queue: VecDeque<Pending<'a>>,
}

impl<'a> Walk<'a> {
    /// Record `node` unless already seen (then queue `pending` for
    /// expansion); returns its id.
    fn discover(&mut self, node: LineageNode, pending: Option<Pending<'a>>) -> String {
        let id = node.id.clone();
        if self.seen.insert(id.clone()) {
            self.nodes.push(node);
            if let Some(pending) = pending {
                self.queue.push_back(pending);
            }
        }
        id
    }

    fn edge(&mut self, from: &str, to: &str, kind: LineageEdgeKind, detail: Option<String>) {
        if self
            .seen_edges
            .insert((from.to_owned(), to.to_owned(), kind.clone()))
        {
            self.edges.push(LineageEdge {
                from: from.to_owned(),
                to: to.to_owned(),
                kind,
                detail,
            });
        }
    }

    fn expand(&mut self, pending: Pending<'a>) {
        match pending {
            Pending::Fact(fact) => self.expand_fact(&fact),
            Pending::Commit(commit) => self.expand_commit(commit),
            Pending::Observation(observation) => {
                self.provenance(&observation.stable_id(), &observation.provenance);
            }
            Pending::Object(id) => self.expand_object(&id),
        }
    }

    fn fact(&mut self, fact: &FactRef) -> String {
        let label = format!(
            "{}/{} v{}",
            fact.object_type.as_str(),
            fact.key,
            fact.version
        );
        self.discover(
            LineageNode::new(fact.stable_id(), LineageNodeKind::Fact, label)
                .keyed(Some(&fact.object_type), &fact.key),
            Some(Pending::Fact(fact.clone())),
        )
    }

    fn commit(&mut self, commit: &'a LineageCommit) -> String {
        let label = match &commit.decision {
            Some(decision) => format!(
                "commit {}/{} by {}@{}",
                commit.world, commit.seq, decision.artifact_id, decision.version
            ),
            None => format!("commit {}/{}", commit.world, commit.seq),
        };
        let mut node = LineageNode::new(commit.stable_id(), LineageNodeKind::Commit, label)
            .at(&commit.recorded_at);
        node.events.clone_from(&commit.events);
        self.discover(node, Some(Pending::Commit(commit)))
    }

    fn object(&mut self, object_type: &ObjectTypeName, key: &str) -> String {
        let kind = TechnicalType::of(object_type)
            .map_or(LineageNodeKind::Object, LineageNodeKind::Technical);
        let id = object_stable_id(object_type, key);
        let label = format!("{}/{key}", object_type.as_str());
        self.discover(
            LineageNode::new(id.clone(), kind, label).keyed(Some(object_type), key),
            Some(Pending::Object(id)),
        )
    }

    fn source_record(&mut self, source: &SourceRecordRef) -> String {
        let label = format!("{} {}", source.system, source.record);
        self.discover(
            LineageNode::new(source.stable_id(), LineageNodeKind::SourceRecord, label),
            None,
        )
    }

    fn expand_fact(&mut self, fact: &FactRef) {
        let id = fact.stable_id();
        if let Some(commit) = self.index.writers.get(fact).copied() {
            let to = self.commit(commit);
            self.edge(&id, &to, LineageEdgeKind::WrittenBy, None);
        }
        if let Some(records) = self.index.provenance.get(fact).cloned() {
            for record in records {
                self.provenance(&id, record);
            }
        }
    }

    fn expand_commit(&mut self, commit: &'a LineageCommit) {
        let id = commit.stable_id();
        if let Some(decision) = &commit.decision {
            let label = format!("decision {}@{}", decision.artifact_id, decision.version);
            let key = format!("{}@{}", decision.artifact_id, decision.version);
            let to = self.discover(
                LineageNode::new(decision.stable_id(), LineageNodeKind::Decision, label)
                    .keyed(None, &key),
                None,
            );
            self.edge(&id, &to, LineageEdgeKind::DecidedBy, None);
        }
        match &commit.consumed {
            Some(LineageTrigger::Observation { id: observation }) => {
                let to = match self.index.observations.get(observation.as_str()).copied() {
                    Some(found) => self.discover(
                        LineageNode::new(
                            found.stable_id(),
                            LineageNodeKind::Observation,
                            format!("observation {observation}"),
                        )
                        .keyed(None, observation)
                        .at(&found.provenance.recorded_at),
                        Some(Pending::Observation(found)),
                    ),
                    None => self.discover(
                        LineageNode::new(
                            format!("observation:{observation}"),
                            LineageNodeKind::Observation,
                            format!("observation {observation}"),
                        )
                        .keyed(None, observation),
                        None,
                    ),
                };
                self.edge(&id, &to, LineageEdgeKind::Consumed, None);
            }
            Some(LineageTrigger::Timer {
                timer_id,
                incarnation,
            }) => {
                let timer = self.discover(
                    LineageNode::new(
                        format!("timer:{}/{timer_id}@{incarnation}", commit.world),
                        LineageNodeKind::Trigger,
                        format!("timer {timer_id}"),
                    ),
                    None,
                );
                self.edge(&id, &timer, LineageEdgeKind::Consumed, None);
                let scheduler_id = norn_ir::commit_stable_id(&commit.world, *incarnation);
                if let Some(scheduler) = self.index.commits.get(&scheduler_id).copied() {
                    let to = self.commit(scheduler);
                    self.edge(&timer, &to, LineageEdgeKind::ScheduledBy, None);
                }
            }
            Some(LineageTrigger::Result {
                command_id,
                generation,
            }) => {
                let to = self.discover(
                    LineageNode::new(
                        format!("result:{}/{command_id}@g{generation}", commit.world),
                        LineageNodeKind::Trigger,
                        format!("result of {command_id}"),
                    ),
                    None,
                );
                self.edge(&id, &to, LineageEdgeKind::Consumed, None);
            }
            None => {}
        }
        for read in &commit.reads {
            let to = self.fact(read);
            self.edge(&id, &to, LineageEdgeKind::Read, None);
        }
    }

    fn provenance(&mut self, from: &str, record: &InstanceProvenance) {
        let source = self.source_record(&record.source);
        self.edge(from, &source, LineageEdgeKind::ObservedFrom, None);
        let system = self.object(
            &ObjectTypeName(TechnicalType::Source.type_name().to_owned()),
            &record.source.system,
        );
        self.edge(&source, &system, LineageEdgeKind::RecordOf, None);
        let mapping = self.object(
            &ObjectTypeName(TechnicalType::Mapping.type_name().to_owned()),
            record.mapping_version.as_str(),
        );
        self.edge(
            from,
            &mapping,
            LineageEdgeKind::MappedBy,
            record.transform.clone(),
        );
        for upstream in &record.upstream {
            let to = self.fact(upstream);
            self.edge(from, &to, LineageEdgeKind::DerivedFrom, None);
        }
    }

    fn expand_object(&mut self, id: &str) {
        let Some(object) = self.index.objects.get(id).copied() else {
            return;
        };
        for link in &object.links {
            let to = self.object(&link.target_type, &link.target_key);
            let kind = match LineageRelation::of(&link.link) {
                Some(LineageRelation::BackedBy) => LineageEdgeKind::BackedBy,
                Some(LineageRelation::DerivedFrom) => LineageEdgeKind::DerivedFrom,
                None => LineageEdgeKind::Link(link.link.clone()),
            };
            self.edge(id, &to, kind, None);
        }
    }
}

/// The causal chain of fact version `fact`. See the module docs.
///
/// # Errors
///
/// [`LineageError::UnknownFact`] when no commit wrote `fact` and no
/// provenance is stored with it; a `Duplicate*` [`LineageError`] when the
/// input names one commit, observation, object, or written fact twice.
/// [`LineageError::MissingWorld`] or [`LineageError::WorldMismatch`] when
/// the input does not explicitly contain exactly one world.
pub fn explain(input: &LineageInput, fact: &FactRef) -> Result<CausalChain, LineageError> {
    explain_index(Index::new(input)?, fact)
}

fn explain_index(index: Index<'_>, fact: &FactRef) -> Result<CausalChain, LineageError> {
    if !index.knows(fact) {
        return Err(LineageError::UnknownFact {
            fact: fact.stable_id(),
        });
    }
    let mut walk = Walk {
        index,
        nodes: Vec::new(),
        seen: BTreeSet::new(),
        edges: Vec::new(),
        seen_edges: BTreeSet::new(),
        queue: VecDeque::new(),
    };
    let root = walk.fact(fact);
    while let Some(next) = walk.queue.pop_front() {
        walk.expand(next);
    }
    Ok(CausalChain {
        root,
        nodes: walk.nodes,
        edges: walk.edges,
    })
}

/// The causal chain of the latest version of object `key` of type
/// `object_type`: the highest version any commit wrote or any provenance
/// record is stored with, within the explicit world of `input`.
///
/// # Errors
///
/// [`LineageError::UnknownObject`] when no version of the object is in the
/// input; otherwise as [`explain`].
pub fn explain_latest(
    input: &LineageInput,
    object_type: &ObjectTypeName,
    key: &str,
) -> Result<CausalChain, LineageError> {
    let index = Index::new(input)?;
    let latest = input
        .commits
        .iter()
        .flat_map(|commit| commit.written.iter())
        .chain(input.instances.iter().map(|instance| &instance.fact))
        .filter(|fact| fact.object_type == *object_type && fact.key == key)
        .max_by_key(|fact| fact.version)
        .cloned()
        .ok_or_else(|| LineageError::UnknownObject {
            object: object_stable_id(object_type, key),
        })?;
    explain_index(index, &latest)
}

/// One link in a view: its id and target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewLink {
    /// The link id.
    pub id: LinkId,
    /// The target type.
    pub target: ObjectTypeName,
}

/// One business type: what it means, without where its facts come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BusinessType {
    /// The type name.
    pub name: ObjectTypeName,
    /// Its declared property ids, in declaration order.
    pub properties: Vec<PropertyId>,
    /// Its business links (every link except `backed_by`/`derived_from`).
    pub links: Vec<ViewLink>,
}

/// One technical-view asset type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TechnicalAsset {
    /// The type name.
    pub name: ObjectTypeName,
    /// The vocabulary type it is, when it is one of the nine.
    pub kind: Option<TechnicalType>,
    /// Its links, lineage links included.
    pub links: Vec<ViewLink>,
}

/// One lineage binding of a business type to the technical view.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineageBinding {
    /// The business type.
    pub business_type: ObjectTypeName,
    /// The relation family.
    pub relation: LineageRelation,
    /// The technical-view target type.
    pub target: ObjectTypeName,
}

/// The business view of an artifact set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct BusinessView {
    /// Every business type, in declaration order.
    pub types: Vec<BusinessType>,
}

/// The technical view of an artifact set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TechnicalView {
    /// Every technical-view type, in declaration order.
    pub assets: Vec<TechnicalAsset>,
    /// Every `backed_by`/`derived_from` link of a business type, in
    /// declaration order.
    pub bindings: Vec<LineageBinding>,
}

/// The two projections of one artifact set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TwoViews {
    /// What the types mean.
    pub business: BusinessView,
    /// Where their facts come from.
    pub technical: TechnicalView,
}

/// Project the business and technical views of `defs`. A type in the
/// `technical` namespace is a technical asset; every other type is a
/// business type whose lineage links become technical-view bindings.
#[must_use]
pub fn project_views(defs: &[ObjectTypeDef]) -> TwoViews {
    let mut views = TwoViews::default();
    for def in defs {
        let links = def.links.iter().map(|link| ViewLink {
            id: link.id.clone(),
            target: link.target.clone(),
        });
        if is_technical_type(&def.name) {
            views.technical.assets.push(TechnicalAsset {
                name: def.name.clone(),
                kind: TechnicalType::of(&def.name),
                links: links.collect(),
            });
            continue;
        }
        let mut business = Vec::new();
        for link in links {
            match LineageRelation::of(&link.id) {
                Some(relation) => views.technical.bindings.push(LineageBinding {
                    business_type: def.name.clone(),
                    relation,
                    target: link.target,
                }),
                None => business.push(link),
            }
        }
        views.business.types.push(BusinessType {
            name: def.name.clone(),
            properties: def
                .properties
                .iter()
                .map(|property| property.id.clone())
                .collect(),
            links: business,
        });
    }
    views
}
