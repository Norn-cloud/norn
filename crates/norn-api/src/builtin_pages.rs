//! Studio's built-in page documents: the lowered pages of `registry/studio/artifacts`, compiled
//! into every host and served on `GET /v1/definitions` (`builtins`) to each workspace that has not
//! installed a definition with the same id.
//!
//! Studio draws its record page from a definition the cell serves (#1762 C2). A workspace that
//! never installed one (a pack project, any `norn init` template, a hosted cell) still has records
//! to open, so the host serves the page itself. The workspace's own definition wins: a workspace
//! that installed `norn.studio.record.widget` is served that, and no built-in beside it.
//!
//! A built-in is not part of the workspace's definitions: it is in no revision, no receipt and no
//! definition history (`GET /v1/definitions/{id}` and `:list` read only installed ones). Its
//! identity is its id and the sha256 of the bytes this host embeds, which is the
//! `registry/studio/artifacts/<id>.json` provenance record's own digest. Both checker rings
//! (Ring-0 and the UI laws) admit every built-in alone against an empty base: the
//! `builtins_are_admitted` test holds that, so a page that stops admitting fails the build's tests,
//! never a reader.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::ToSchema;

/// Studio's record page: one page for every object type, its record a context read.
pub const RECORD_PAGE_ID: &str = "norn.studio.record.widget";

/// Every built-in page, by id, as the exact bytes this host embeds.
pub const PAGES: &[(&str, &str)] = &[(
    RECORD_PAGE_ID,
    include_str!("../../../registry/studio/artifacts/norn.studio.record.widget.json"),
)];

/// One built-in page as `GET /v1/definitions` serves it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = NornBuiltinDefinition)]
pub struct BuiltinDefinition {
    /// The page's artifact envelope, as `artifacts` carries an installed one.
    #[schema(schema_with = artifact_schema)]
    pub artifact: Box<RawValue>,
    /// The sha256 (lowercase hex) of the bytes this host embeds for the page.
    pub sha256: String,
}

fn artifact_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "One built-in artifact envelope, admitted by both checker rings when this host was built.",
        false,
    )
}

/// Each page's sha256, computed once.
fn digests() -> &'static [String] {
    static DIGESTS: OnceLock<Vec<String>> = OnceLock::new();
    DIGESTS.get_or_init(|| {
        PAGES
            .iter()
            .map(|(_, bytes)| crate::uploads::sha256_hex(bytes.as_bytes()))
            .collect()
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identified<'a> {
    #[serde(borrow)]
    artifact_id: Option<std::borrow::Cow<'a, str>>,
}

/// The built-in pages served beside `installed`: each one whose id no installed artifact has.
///
/// # Errors
/// An installed artifact that is not a JSON object, or an embedded page that is not JSON (the
/// tests decode every one, so a built host has none).
pub fn served(installed: &[Box<RawValue>]) -> Result<Vec<BuiltinDefinition>, serde_json::Error> {
    let mut ids = BTreeSet::new();
    for artifact in installed {
        if let Some(id) = serde_json::from_str::<Identified<'_>>(artifact.get())?.artifact_id {
            ids.insert(id.into_owned());
        }
    }
    PAGES
        .iter()
        .zip(digests())
        .filter(|((id, _), _)| !ids.contains(*id))
        .map(|((_, bytes), sha256)| {
            Ok(BuiltinDefinition {
                artifact: RawValue::from_string((*bytes).to_owned())?,
                sha256: sha256.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_checker::EntityArtifactSetContext;
    use norn_ir::ArtifactEnvelope;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Both rings admit each built-in alone, against an empty base and registry, with no finding.
    #[test]
    fn builtins_are_admitted() {
        for (id, bytes) in PAGES {
            let envelope: ArtifactEnvelope =
                serde_json::from_str(bytes).unwrap_or_else(|error| panic!("{id}: {error}"));
            assert_eq!(
                envelope.artifact_id.as_str(),
                *id,
                "{id} is embedded under its own id"
            );
            let findings = norn_ui_checker::check_artifact_set(
                std::slice::from_ref(&envelope),
                &EntityArtifactSetContext {
                    registry: &BTreeSet::new(),
                    base: &[],
                    tenant: None,
                },
            );
            assert!(findings.is_empty(), "{id} is not admitted: {findings:#?}");
        }
    }

    /// Every lowered Studio page is a built-in, and each built-in's bytes are the committed artifact's.
    #[test]
    fn builtins_are_the_registry_pages() {
        let dir = root().join("registry/studio/artifacts");
        let mut committed: Vec<(String, String)> = std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
            .map(|entry| {
                let path = entry.unwrap_or_else(|error| panic!("{error}")).path();
                let id = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned);
                let bytes = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                (
                    id.unwrap_or_else(|| panic!("{} has no id", path.display())),
                    bytes,
                )
            })
            .collect();
        committed.sort();
        let mut embedded: Vec<(String, String)> = PAGES
            .iter()
            .map(|(id, bytes)| ((*id).to_owned(), (*bytes).to_owned()))
            .collect();
        embedded.sort();
        assert_eq!(
            embedded, committed,
            "the embedded pages drifted from registry/studio/artifacts"
        );
    }

    fn raw(json: &str) -> Box<RawValue> {
        RawValue::from_string(json.to_owned()).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn an_installed_page_overrides_the_builtin() {
        let others = [raw(r#"{"artifactId":"canon.Party","kind":"entity"}"#)];
        let served_beside = |installed: &[Box<RawValue>]| -> Vec<String> {
            served(installed)
                .unwrap_or_else(|error| panic!("{error}"))
                .iter()
                .map(|page| page.artifact.get().to_owned())
                .collect()
        };
        let builtin = served_beside(&others);
        assert_eq!(builtin.len(), 1);
        assert!(builtin[0].contains(RECORD_PAGE_ID));
        let mut installed = others.to_vec();
        installed.push(raw(&format!(
            r#"{{"artifactId":"{RECORD_PAGE_ID}","kind":"widget"}}"#
        )));
        assert!(
            served_beside(&installed).is_empty(),
            "the workspace's own page wins"
        );
    }

    #[test]
    fn the_digest_is_the_embedded_bytes() {
        let page = served(&[]).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            page[0].sha256,
            crate::uploads::sha256_hex(PAGES[0].1.as_bytes())
        );
    }
}
