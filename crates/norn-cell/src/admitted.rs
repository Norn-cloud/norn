//! INSTALL-SEAM-1: one admitted definition set, projected once for a cell.
//!
//! A host that installs an admitted set needs three things built from the
//! same artifacts: the cell [`Model`] (the set's object types, interfaces and
//! shared property types, the kernel's own record types and the retained
//! governing laws), the decoded actions, and the ACTOR-INDEPENDENT-1
//! transition guards of those actions. Built apart, an embedded host can serve
//! two models (one without the action-call record type, so `commit_action`
//! cannot record its call; one with a differently shaped record type, so
//! installation refuses it as not the projection of its set) or forget to
//! bind the guards, after which every transition of a type with protected
//! actors fails closed with `PermissionDenied`. [`AdmittedDefinitions`] builds
//! all three together, and [`AdmittedDefinitions::catalog`] binds the guards
//! by construction. Fail-closed semantics are unchanged: a catalog built any
//! other way without guards still refuses.

use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactPayload, EntityRoot};

use crate::error::CellError;
use crate::model::Model;

/// One admitted action, pinned by its artifact identity.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedAction {
    /// The action envelope's artifact id (`<action id>.action`).
    pub artifact_id: String,
    /// The action envelope's version.
    pub version: String,
    /// The decoded action.
    pub root: ActionRoot,
}

/// An admitted definition set as a cell serves it: its model, its actions and
/// their transition guards, all from the same artifacts.
#[derive(Debug, Clone)]
pub struct AdmittedDefinitions {
    model: Model,
    actions: Vec<AdmittedAction>,
    guards: Vec<norn_kernel::TransitionGuard>,
}

impl AdmittedDefinitions {
    /// Project `artifacts` (an admitted set, with the base artifacts its
    /// references resolve against): the model holds every `EntityIR` v2 object
    /// type, interface and property type, plus exactly the
    /// [`crate::kernel_record_types`], so it is the projection installation
    /// accepts; it retains the set's laws; the guards are those of its actions.
    ///
    /// # Errors
    ///
    /// [`CellError::Json`] when an entity or action payload does not decode,
    /// and any projection or law-retention refusal of [`Model`].
    pub fn from_envelopes(artifacts: &[ArtifactEnvelope]) -> Result<Self, CellError> {
        let (mut types, mut interfaces, mut property_types, mut actions) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for artifact in artifacts {
            match &artifact.payload {
                ArtifactPayload::Entity(entity) => {
                    if !entity.is_entity_ir_v2() {
                        continue;
                    }
                    match decode::<EntityRoot>(&artifact.payload)? {
                        EntityRoot::V2(root) => types.push(root.object_type),
                        EntityRoot::V2Interface(root) => interfaces.push(root.interface),
                        EntityRoot::V2PropertyTypes(root) => {
                            property_types.extend(root.property_types);
                        }
                        EntityRoot::V2Extension(_)
                        | EntityRoot::V2Markings(_)
                        | EntityRoot::V1(_) => {}
                    }
                }
                ArtifactPayload::Action(_) => actions.push(AdmittedAction {
                    artifact_id: artifact.artifact_id.clone(),
                    version: artifact.version.clone(),
                    root: decode(&artifact.payload)?,
                }),
                ArtifactPayload::Law(_)
                | ArtifactPayload::Policy(_)
                | ArtifactPayload::Relation(_)
                | ArtifactPayload::Rule(_)
                | ArtifactPayload::Connector(_)
                | ArtifactPayload::Widget(_)
                | ArtifactPayload::Customization(_)
                | ArtifactPayload::Setting(_)
                | ArtifactPayload::BusinessPack(_)
                | ArtifactPayload::Derived(_)
                | ArtifactPayload::SourceSchema(_)
                | ArtifactPayload::Mapping(_)
                | ArtifactPayload::DefinedConcept(_)
                | ArtifactPayload::Labels(_)
                | ArtifactPayload::Model(_)
                | ArtifactPayload::Theme(_) => {}
            }
        }
        types.extend(crate::kernel_record_types());
        let model = Model::from_definitions_with_interfaces(&types, &property_types, &interfaces)?
            .with_governing_laws(artifacts)?;
        let guards = norn_kernel::transition_guards(actions.iter().map(|action| &action.root));
        Ok(Self {
            model,
            actions,
            guards,
        })
    }

    /// The cell model: install it, and open the cell over it.
    #[must_use]
    pub const fn model(&self) -> &Model {
        &self.model
    }

    /// Every admitted action, in artifact order.
    #[must_use]
    pub fn actions(&self) -> &[AdmittedAction] {
        &self.actions
    }

    /// The admitted action `action_id` (its `actionId`, not its artifact id).
    #[must_use]
    pub fn action(&self, action_id: &str) -> Option<&AdmittedAction> {
        self.actions
            .iter()
            .find(|action| action.root.action_id == action_id)
    }

    /// ACTOR-INDEPENDENT-1: the transitions the admitted actions guard.
    #[must_use]
    pub fn guards(&self) -> &[norn_kernel::TransitionGuard] {
        &self.guards
    }

    /// The kernel catalog a governed call commits under: the model's types,
    /// interfaces, property types and retained laws, with every transition
    /// guard bound.
    #[must_use]
    pub fn catalog(&self) -> norn_kernel::TypeCatalog<'_> {
        self.model
            .type_catalog()
            .with_governing_laws(self.model.law_artifacts())
            .with_transition_guards(&self.guards)
    }
}

fn decode<T: serde::de::DeserializeOwned>(payload: &ArtifactPayload) -> Result<T, CellError> {
    Ok(serde_json::from_value(serde_json::to_value(
        payload.to_json_value(),
    )?)?)
}
