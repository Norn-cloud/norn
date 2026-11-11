//! LABEL-LOCALE-1 (#679): localizing never changes a digest or evidence
//! identity. Over the admitted three-systems set, arbitrary valid label
//! catalogs (an installed upstream layer plus a downstream overlay) are
//! admitted and leave the model hash, the DDL hash, every definition
//! artifact's Norn1 hash and the governing laws exactly as they were.
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "property test over the retained three-systems set"
)]
use norn_cell::Model;
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::canonical::HashProfile;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, EntityRoot, LabelCatalogRoot, LabelEntry, LabelReview,
    LabelTarget, LocaleText, ResourceLimitProfile, RuntimeValue,
};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The admitted three-systems set, as `explorer_documents.rs` selects it.
fn definitions() -> &'static [ArtifactEnvelope] {
    static SET: OnceLock<Vec<ArtifactEnvelope>> = OnceLock::new();
    SET.get_or_init(|| {
        let excluded = [
            "commerce.SalesOrder.entity",
            "core.Order.interface",
            "core.Entitlement.entity",
            "identity.BusinessUnit.entity",
            "location.Region.entity",
            "location.Space.entity",
            "party.Brand.entity",
        ];
        let mut paths: Vec<PathBuf> = ["core", "packs/canon", "packs/erpnext", "packs/shopify"]
            .into_iter()
            .flat_map(|dir| {
                std::fs::read_dir(root().join(format!("registry/{dir}/artifacts")))
                    .expect("artifact directory")
                    .map(|entry| entry.expect("entry").path())
            })
            .collect();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                serde_json::from_str(&std::fs::read_to_string(path).expect("artifact"))
                    .expect("envelope")
            })
            .filter(|a: &ArtifactEnvelope| !excluded.contains(&a.artifact_id.as_str()))
            .collect()
    })
}

fn is_base(artifact: &ArtifactEnvelope) -> bool {
    artifact.artifact_id.starts_with("core.") || artifact.artifact_id.starts_with("std.")
}

/// Admit `artifacts` (the `core.`/`std.` layer as the installed base) and
/// return the admission findings.
fn admit(artifacts: &[ArtifactEnvelope]) -> (bool, Vec<String>) {
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts.iter().cloned().partition(is_base);
    let values = |set: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
        set.iter()
            .map(|a| {
                RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("value")
            })
            .collect()
    };
    let sources = ReceiptSources::new(&values(&candidate), &values(&base)).expect("sources");
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("checked");
    let findings = checked
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{} {:?}: {}", d.code.as_str(), d.artifact_id, d.message))
        .collect();
    (checked.receipt.admitted, findings)
}

/// The model the cell builds from a set, as the host seed builds it: entity
/// roots plus governing laws read from the whole set.
fn model(artifacts: &[ArtifactEnvelope]) -> Model {
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
        )
        .expect("entity root");
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                panic!(
                    "{} is outside the three-systems profile",
                    artifact.artifact_id
                )
            }
        }
    }
    Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(artifacts)
        .expect("governing laws")
}

fn artifact_hash(artifact: &ArtifactEnvelope) -> String {
    HashProfile::Norn1
        .hash_json(serde_json::to_vec(artifact).expect("wire"))
        .expect("hash")
}

/// Every digest the cell and the definitions carry, keyed by name.
fn digests(artifacts: &[ArtifactEnvelope]) -> BTreeMap<String, String> {
    let model = model(artifacts);
    let mut out = BTreeMap::from([
        ("model".to_owned(), model.model_hash().to_owned()),
        ("ddl".to_owned(), model.ddl_hash().to_owned()),
    ]);
    for law in model.law_artifacts() {
        out.insert(format!("law:{}", law.artifact_id), artifact_hash(law));
    }
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() != ArtifactKind::Labels)
    {
        out.insert(
            format!("artifact:{}", artifact.artifact_id),
            artifact_hash(artifact),
        );
    }
    out
}

fn baseline() -> &'static BTreeMap<String, String> {
    static DIGESTS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    DIGESTS.get_or_init(|| digests(definitions()))
}

fn targets() -> &'static [LabelTarget] {
    static TARGETS: OnceLock<Vec<LabelTarget>> = OnceLock::new();
    TARGETS.get_or_init(|| {
        norn_checker::labels::declared_targets(definitions())
            .into_iter()
            .collect()
    })
}

fn envelope(id: &str, root: &LabelCatalogRoot) -> ArtifactEnvelope {
    let (namespace, name) = id.split_once('.').expect("qualified id");
    serde_json::from_value(serde_json::json!({
        "irVersion": "0.1", "artifactId": id, "kind": "labels",
        "namespace": namespace, "name": name, "version": "1.0.0", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": root,
    }))
    .expect("labels envelope")
}

fn text() -> impl Strategy<Value = String> {
    // Arabic, Latin and digits, with interior spaces and Arabic punctuation;
    // never blank and never a bidi control.
    "[A-Za-z\u{0621}-\u{064A}0-9][A-Za-z\u{0621}-\u{064A}\u{0660}-\u{0669}0-9 \u{060C}-]{0,23}"
}

fn locales() -> impl Strategy<Value = BTreeSet<&'static str>> {
    prop::collection::btree_set(prop::sample::select(vec!["en", "ar", "ar-EG", "fr"]), 1..4)
}

fn entry(target: LabelTarget) -> impl Strategy<Value = LabelEntry> {
    (locales(), any::<bool>(), any::<bool>())
        .prop_flat_map(move |(tags, describe, proposed)| {
            let tags: Vec<String> = tags.into_iter().map(String::from).collect();
            let n = tags.len();
            (
                Just(target.clone()),
                Just(tags),
                prop::collection::vec(text(), n),
                prop::collection::vec(text(), n),
                Just(describe),
                Just(proposed),
            )
        })
        .prop_map(
            |(target, tags, labels, descriptions, describe, proposed)| LabelEntry {
                target,
                label: LocaleText(tags.iter().cloned().zip(labels).collect()),
                description: describe
                    .then(|| LocaleText(tags.iter().cloned().zip(descriptions).collect())),
                // Machine-proposed Arabic is marked for review.
                review: tags
                    .iter()
                    .filter(|tag| proposed && tag.starts_with("ar"))
                    .map(|tag| (tag.clone(), LabelReview::NeedsReview))
                    .collect(),
            },
        )
}

/// An upstream catalog and a downstream overlay; they may label the same
/// targets in the same locales (the overlay wins), each labels a target once.
fn layers() -> impl Strategy<Value = (LabelCatalogRoot, LabelCatalogRoot)> {
    let catalog = || {
        prop::sample::subsequence(targets().to_vec(), 0..=targets().len().min(24))
            .prop_flat_map(|chosen| chosen.into_iter().map(entry).collect::<Vec<_>>())
            .prop_map(|entries| LabelCatalogRoot {
                labels_version: norn_ir::IrVersion01::V0_1,
                entries,
                required_locales: Vec::new(),
                extensions: None,
            })
    };
    (catalog(), catalog())
}

#[test]
fn the_label_universe_covers_every_definition_kind() {
    let kinds: BTreeSet<&str> = targets()
        .iter()
        .map(|target| match target {
            LabelTarget::ObjectType { .. } => "objectType",
            LabelTarget::Property { .. } => "property",
            LabelTarget::EnumValue { .. } => "enumValue",
            LabelTarget::Action { .. } => "action",
            LabelTarget::Rule { .. } => "rule",
            LabelTarget::Law { .. } => "law",
            LabelTarget::Page { .. } => "page",
        })
        .collect();
    for kind in ["objectType", "property", "enumValue", "law"] {
        assert!(
            kinds.contains(kind),
            "the fixture declares no {kind} target: {kinds:?}"
        );
    }
    let (admitted, findings) = admit(definitions());
    assert!(admitted, "{findings:#?}");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// LABEL-LOCALE-1: adding, changing or overlaying labels is admitted and
    /// changes no model, DDL, artifact or law digest.
    #[test]
    fn labels_never_change_a_digest((upstream, overlay) in layers()) {
        let mut labelled = definitions().to_vec();
        labelled.push(envelope("core.labels", &upstream));
        labelled.push(envelope("tenant.labels", &overlay));
        let (admitted, findings) = admit(&labelled);
        prop_assert!(admitted, "{:#?}", findings);
        prop_assert_eq!(&digests(&labelled), baseline());
    }
}
