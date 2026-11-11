//! Decode an artifact set into the typed inputs of the Frappe projection.
//! Shared by the `frappe_bundle` tests and example.

use norn_ir::{
    AccessIr, ActionRoot, ArtifactEnvelope, ArtifactKind, EntityRoot, ObjectTypeDef, PolicyRoot,
};

/// The object types, actions and access policies of an artifact set.
#[derive(Debug, Default)]
pub(crate) struct Definitions {
    pub(crate) object_types: Vec<ObjectTypeDef>,
    pub(crate) actions: Vec<ActionRoot>,
    pub(crate) access: Vec<AccessIr>,
}

/// Decode one artifact's payload as the typed root `$ty`.
macro_rules! root {
    ($artifact:expr, $ty:ty) => {
        serde_json::to_value($artifact.payload.to_json_value())
            .and_then(serde_json::from_value::<$ty>)
            .map_err(|error| format!("{}: {error}", $artifact.artifact_id))
    };
}

/// Decode `text`, a JSON array of artifact envelopes.
pub(crate) fn decode(text: &str) -> Result<Definitions, String> {
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Definitions::default();
    for artifact in &artifacts {
        let kind = artifact.kind();
        if kind == ArtifactKind::Entity {
            if let EntityRoot::V2(entity) = root!(artifact, EntityRoot)? {
                out.object_types.push(entity.object_type);
            }
        } else if kind == ArtifactKind::Action {
            out.actions.push(root!(artifact, ActionRoot)?);
        } else if kind == ArtifactKind::Policy
            && let PolicyRoot::Access(access) = root!(artifact, PolicyRoot)?
        {
            out.access.push(access);
        }
    }
    Ok(out)
}
