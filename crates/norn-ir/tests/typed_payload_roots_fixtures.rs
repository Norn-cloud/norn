#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Golden round-trip for every checked-in `ArtifactEnvelope` fixture.
//!
//! Each fixture must decode into the closed `ArtifactPayload` root selected by
//! its `kind`, re-encode to the same canonical JSON (`norn-canonical` Norn1
//! hash unchanged), and re-encode to the same raw `serde_json` bytes that were
//! pinned before the payload roots became a tagged enum. The raw-byte pin
//! matters because `norn-engine` hashes `serde_json::to_vec(artifact)` directly
//! for `RuleIR` definition hashes.

use norn_canonical::HashProfile;
use norn_ir::{ArtifactEnvelope, ArtifactKind};
use sha2::{Digest, Sha256};

struct Fixture {
    name: &'static str,
    source: String,
    kind: ArtifactKind,
    /// Norn1 canonical JSON hash of the source fixture.
    canonical_hash: &'static str,
    /// SHA-256 of `serde_json::to_vec(&envelope)` on the pre-enum baseline.
    raw_sha256: &'static str,
}

fn fixtures() -> Vec<Fixture> {
    let conformance: serde_json::Value = serde_json::from_str(include_str!(
        "../../../verification/archive/rules-dmn-v1/fixtures/conformance.json"
    ))
    .expect("rule conformance fixture should decode");
    let rule_artifact = serde_json::to_string(
        conformance
            .get("artifact")
            .expect("rule conformance fixture should contain an artifact"),
    )
    .expect("nested rule artifact should encode");

    vec![
        Fixture {
            name: "fixtures/ir/retail-inventory.connector.json",
            source: include_str!("../../../fixtures/ir/retail-inventory.connector.json").into(),
            kind: ArtifactKind::Connector,
            canonical_hash: "b1a8fa96a78bf57965f7ca1c781f288576a6f94c6de28897f7f634f23c92d385",
            raw_sha256: "c7e70be8b7e56664a0c8f9b23cda97c4a29269be3385b49efa89d6550fe7e681",
        },
        Fixture {
            name: "fixtures/ir/retail-replenishment.widget.json",
            source: include_str!("../../../fixtures/ir/retail-replenishment.widget.json").into(),
            kind: ArtifactKind::Widget,
            canonical_hash: "2c540e4263a1ce3348dd4c6e9de1377b55ed3a992045412a26e7f3057464d092",
            raw_sha256: "021e7fd928650433149762a9ad487ad4f2259e7c52b7cf5ad71b1ff84d2c813e",
        },
        Fixture {
            name: "fixtures/ir/retail-sku.entity.json",
            source: include_str!("../../../fixtures/ir/retail-sku.entity.json").into(),
            kind: ArtifactKind::Entity,
            canonical_hash: "65d8dc7acc7c1f2c22b10527551a1dee79ed170a70c2cbeb42375bf9ee61d612",
            raw_sha256: "df8b153b0dcbcaa9cd690f22b84691be9aece2d83d1296bcf8a404df187f58f5",
        },
        // T1-IR-1b (#2507): new fixtures, newly pinned (no existing pin moved).
        // Re-pinned by T1-ONT-2 (#2515): its computed expression now reads
        // the object through `self` (the expression-site convention).
        Fixture {
            name: "fixtures/ir/retail-sku-v2.entity.json",
            source: include_str!("../../../fixtures/ir/retail-sku-v2.entity.json").into(),
            kind: ArtifactKind::Entity,
            canonical_hash: "463acbcaef48bc08d4f241b51b287a6ecbc60fd6767ab2a0105e415b9141525e",
            raw_sha256: "a039fdac156c3dbcd16ef6d84cff07d82d33848fde24aa0e8163e5aef32c7f92",
        },
        Fixture {
            name: "fixtures/ir/retail-sku-supplier.relation.json",
            source: include_str!("../../../fixtures/ir/retail-sku-supplier.relation.json").into(),
            kind: ArtifactKind::Relation,
            canonical_hash: "1d5efb861aba212fcbd426e8f3f1e64548ecd6100ae4eff3a4ee3cc4eee4fbe4",
            raw_sha256: "77f6f8e9a58f76ec4ccaba651a246b900ae54c209f2a3c6f4597ab241a4fe0fb",
        },
        Fixture {
            name: "verification/ontology/packs/company-commerce-v0/pack.json",
            source: include_str!(
                "../../../verification/ontology/packs/company-commerce-v0/pack.json"
            )
            .into(),
            kind: ArtifactKind::BusinessPack,
            canonical_hash: "ac62a2d1cd4e3eea1e3f1132d91be9cc587aa714510c2558f2a4ff8929ded202",
            raw_sha256: "c98dc82f0972fd422410580b3506f47485d007c2d40263b8aff829dd003589ca",
        },
        Fixture {
            name: "verification/ontology/packs/quantity-units-v0/pack.json",
            source: include_str!(
                "../../../verification/ontology/packs/quantity-units-v0/pack.json"
            )
            .into(),
            kind: ArtifactKind::BusinessPack,
            canonical_hash: "264c2007efbc6b81feeb55688456b81be7701c1a8771fc0709dc5c339af382aa",
            raw_sha256: "aa1c39e6c0352843dfb630bc76af01688ff46591a375ac028ad2848e52ffb9e2",
        },
        Fixture {
            name: "verification/archive/rules-dmn-v1/fixtures/conformance.json#artifact",
            source: rule_artifact,
            kind: ArtifactKind::Rule,
            canonical_hash: "524b3f6aa2a26011d6d84f7964abf2df06c9c2436e97f1aa52734b34d922a2bc",
            raw_sha256: "2c8dbae5b4896a9dec87ce752ecdf8bb9b8d968bd682d1511cc15e41834f6001",
        },
    ]
}

#[test]
fn every_envelope_fixture_roundtrips_with_pinned_hashes() {
    for fixture in fixtures() {
        let envelope: ArtifactEnvelope = serde_json::from_str(&fixture.source)
            .unwrap_or_else(|error| panic!("{} should deserialize: {error}", fixture.name));
        assert_eq!(envelope.kind(), fixture.kind, "{} kind", fixture.name);
        assert_eq!(
            envelope.payload.kind(),
            fixture.kind,
            "{} payload root",
            fixture.name
        );

        let reserialized = serde_json::to_vec(&envelope)
            .unwrap_or_else(|error| panic!("{} should serialize: {error}", fixture.name));
        let source_hash = HashProfile::Norn1
            .hash_json(fixture.source.as_bytes())
            .expect("source fixture should canonicalize");
        let reserialized_hash = HashProfile::Norn1
            .hash_json(&reserialized)
            .expect("reserialized fixture should canonicalize");
        let raw_sha256 = format!("{:x}", Sha256::digest(&reserialized));

        assert_eq!(
            reserialized_hash, source_hash,
            "{} canonical JSON changed after round-trip",
            fixture.name
        );
        assert_eq!(
            source_hash, fixture.canonical_hash,
            "{} canonical hash drifted",
            fixture.name
        );
        assert_eq!(
            raw_sha256, fixture.raw_sha256,
            "{} raw serde_json bytes drifted",
            fixture.name
        );

        let again: ArtifactEnvelope = serde_json::from_slice(&reserialized)
            .unwrap_or_else(|error| panic!("{} should re-deserialize: {error}", fixture.name));
        assert_eq!(again, envelope, "{} typed round-trip", fixture.name);
    }
}
