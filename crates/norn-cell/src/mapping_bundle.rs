//! OBS-EVIDENCE-1: retained mapping output is evidence, never write authority.
use crate::{CellError, CellStore, StoreReader, engine::SqlEngine};
use norn_checker::mapping::{SourceDocument, compile_mapping_artifact};
use norn_ir::{ArtifactEnvelope, RuntimeValue, mapping::MappedRecord, provenance::UtcTimestamp};
use norn_kernel::{ObservationId, Observed};
use serde::{Deserialize, Serialize};

/// Trusted source selection; every evaluation input participates in bundle identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingInput {
    /// Admitted mapping artifact ID.
    pub mapping: String,
    /// Source dataset selected by connector configuration.
    pub dataset: String,
    /// Configured connector instance.
    pub connector_instance: String,
    /// Configured partition.
    pub partition: String,
    /// Stable capture context, reused during replay.
    pub recorded_at: UtcTimestamp,
}

/// Ordinary captured evidence, including unsuccessful mapping diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingBundle {
    /// Digest of the sealed raw envelope and complete evaluation pins.
    pub digest: String,
    /// Exact sealed raw envelope, including nonconforming payloads.
    pub raw: RuntimeValue,
    /// Trusted source selection at capture.
    pub input: MappingInput,
    /// Exact retained definition version.
    pub definitions_version: u64,
    /// Digest of the retained canonical artifact set.
    pub definitions_digest: String,
    /// Source and mapping artifact identity, version and digest.
    pub artifact_pins: Vec<(String, String, String)>,
    /// Full mapping outputs, retaining provenance and declared relation outputs.
    pub records: Vec<MappedRecord>,
    /// Mapping failures retained as ordinary evidence.
    pub diagnostics: Vec<RuntimeValue>,
}

fn mismatch() -> CellError {
    CellError::MappingBundleMismatch
}
fn digest(value: &impl Serialize) -> Result<String, CellError> {
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.mapping.bundle.v1\0",
        value,
    )?)
}

impl<E: SqlEngine> StoreReader<'_, E> {
    fn mapping_bundle(
        &self,
        raw: &RuntimeValue,
        input: &MappingInput,
        version: u64,
    ) -> Result<MappingBundle, CellError> {
        self.retained_definition_model(version)?;
        let installed = self
            .installed_definitions(Some(version))?
            .ok_or_else(mismatch)?;
        let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&installed.canonical_set)?;
        let mapping = artifacts
            .iter()
            .find(|a| a.artifact_id == input.mapping)
            .ok_or_else(mismatch)?;
        let document: norn_ir::mapping::MappingIr =
            serde_json::from_value(serde_json::to_value(mapping.payload.to_json_value())?)?;
        let source = artifacts
            .iter()
            .find(|a| a.artifact_id == document.source.schema_id)
            .ok_or_else(mismatch)?;
        let schema: norn_ir::source_schema::SourceSchemaIr =
            serde_json::from_value(serde_json::to_value(source.payload.to_json_value())?)?;
        let field = |name: &str| {
            raw.get(name)
                .and_then(RuntimeValue::as_str)
                .ok_or_else(mismatch)
        };
        if field("contract")? != "norn.raw_observation/v1"
            || field("source")? != schema.source.system
            || input.connector_instance != schema.source.instance
            || input.partition.trim().is_empty()
            || !schema.datasets.iter().any(|d| d.id == input.dataset)
        {
            return Err(mismatch());
        }
        let payload = raw.get("payload").ok_or_else(mismatch)?;
        let payload_text = serde_json::to_string(payload)?;
        if norn_ir::canonical::HashProfile::Norn1.hash_json(&payload_text)?
            != field("payload_digest")?
        {
            return Err(mismatch());
        }
        let source_document = SourceDocument::from_json(&input.dataset, &payload_text, None)?;
        let (records, diagnostics) = if field("kind")? == "upsert" {
            match compile_mapping_artifact(mapping, &artifacts, &[])
                .and_then(|compiled| compiled.apply(&[source_document], &input.recorded_at))
            {
                Ok(records) => (records, Vec::new()),
                Err(diagnostics) => (
                    Vec::new(),
                    diagnostics
                        .into_iter()
                        .map(|d| {
                            norn_ir::runtime::encode(&d.to_json()).map_err(CellError::Encoding)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            }
        } else {
            (
                Vec::new(),
                vec![
                    norn_ir::runtime_object! {"code"=>norn_checker::mapping::codes::EVALUATION_FAILED, "message"=>"CP2 mapping bundle does not apply source deletes"},
                ],
            )
        };
        let artifact_pins = [source, mapping]
            .into_iter()
            .map(|a| {
                let pin = norn_checker::ArtifactRef::of(a)
                    .map_err(|e| CellError::Encoding(e.to_string()))?;
                Ok((pin.artifact_id, pin.version, pin.sha256))
            })
            .collect::<Result<Vec<_>, CellError>>()?;
        let definitions_digest = digest(&artifacts)?;
        let address = digest(&(
            digest(raw)?,
            input,
            version,
            &definitions_digest,
            &artifact_pins,
        ))?;
        Ok(MappingBundle {
            digest: address,
            raw: raw.clone(),
            input: input.clone(),
            definitions_version: version,
            definitions_digest,
            artifact_pins,
            records,
            diagnostics,
        })
    }

    /// Re-run the admitted mapping against the exact retained pins before processing.
    /// This returns evidence only; callers cannot obtain observation authority here.
    ///
    /// # Errors
    /// Changed pins, raw evidence or derived output refuse `core.boundary.bundle_mismatch`.
    pub fn recheck_mapping_bundle(&self, bundle: &MappingBundle) -> Result<(), CellError> {
        let active = self.active_definitions_version()?.ok_or_else(mismatch)?;
        if bundle.definitions_version > active
            || self
                .installed_definitions(Some(bundle.definitions_version))?
                .is_none()
        {
            return Err(mismatch());
        }
        if self.mapping_bundle(&bundle.raw, &bundle.input, bundle.definitions_version)? != *bundle {
            return Err(mismatch());
        }
        Ok(())
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Capture cell-derived mapping evidence through the existing Observed inbox.
    /// Caller-provided mapped outputs and claims are never accepted.
    ///
    /// # Errors
    /// Missing admitted pins, invalid raw framing or durable inbox failure.
    pub fn inject_mapping_bundle(
        &mut self,
        raw: &Observed<RuntimeValue>,
        input: &MappingInput,
    ) -> Result<(String, bool), CellError> {
        let version = self
            .read()
            .installed_definitions(None)?
            .ok_or_else(mismatch)?
            .version;
        let bundle = self.read().mapping_bundle(&raw.input, input, version)?;
        let id = format!("mapping-bundle:{}", bundle.digest);
        let observed = Observed {
            id: ObservationId(id.clone()),
            provenance: raw.provenance.clone(),
            input: norn_ir::runtime::encode(&bundle).map_err(CellError::Encoding)?,
        };
        let inserted = self.inject(
            &id,
            &norn_ir::runtime::encode(&observed).map_err(CellError::Encoding)?,
        )?;
        Ok((id, inserted))
    }
}
