#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "golden contract tests"
)]
use norn_ir::{ArtifactEnvelope, ArtifactKind, LawIR};
use serde_json::Value;

#[test]
fn laws_decode_losslessly_and_reject_open_temporal_shapes() {
    let goldens: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("goldens");
    for golden in goldens {
        let envelope: ArtifactEnvelope = serde_json::from_value(golden.clone()).expect("envelope");
        assert_eq!(envelope.kind(), ArtifactKind::Law);
        assert_eq!(serde_json::to_value(envelope).expect("encoded"), golden);
        let law: LawIR = serde_json::from_value(golden["payload"].clone()).expect("law");
        assert_eq!(
            serde_json::to_value(law).expect("encoded law"),
            golden["payload"]
        );
        let mut invalid = golden["payload"].clone();
        invalid["modality"] = serde_json::json!({"kind":"always", "until":"true"});
        assert!(serde_json::from_value::<LawIR>(invalid).is_err());
        let mut invalid = golden["payload"].clone();
        invalid["condition"] = Value::Null;
        assert!(serde_json::from_value::<LawIR>(invalid).is_err());
    }
}
