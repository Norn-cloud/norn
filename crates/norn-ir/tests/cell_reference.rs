#![allow(clippy::expect_used, missing_docs, reason = "boundary properties")]

use norn_ir::{
    cell_id::CellId,
    cell_reference::{CellReference, CommitProvenance, QualifiedCommit, ReferenceNamespace},
    function::FunctionInvocation,
};
use proptest::prelude::*;

fn cell(value: u128) -> CellId {
    CellId::try_from(format!("{value:032x}")).expect("cell")
}
fn invocation() -> FunctionInvocation {
    serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.wasm.json"
    ))
    .expect("fixture")
}

proptest! {
    #[test]
    fn same_scope_local_names_do_not_alias(a in any::<u128>(), local in ".{0,80}") {
        let b = a ^ 1;
        for namespace in [ReferenceNamespace::Fact, ReferenceNamespace::Operation,
            ReferenceNamespace::Command, ReferenceNamespace::Inbox, ReferenceNamespace::FunctionReceipt] {
            let first = CellReference::new(cell(a), "main".into(), namespace, local.clone());
            let second = CellReference::new(cell(b), "main".into(), namespace, local.clone());
            prop_assert_ne!(first.content_digest().expect("digest"), second.content_digest().expect("digest"));
            prop_assert!(!first.belongs_to(&cell(b), "main"));
            prop_assert!(!first.belongs_to(&cell(a), "fork"));
        }
    }

    #[test]
    fn invocation_dispatch_binds_origin(a in any::<u128>(), commit in any::<u64>()) {
        let mut first = invocation();
        let mut second = first.clone();
        first.provenance_commit = CommitProvenance::Qualified(QualifiedCommit::new(cell(a), "main".into(), commit));
        second.provenance_commit = CommitProvenance::Qualified(QualifiedCommit::new(cell(a ^ 1), "main".into(), commit));
        prop_assert_ne!(first.content_digest().expect("dispatch"), second.content_digest().expect("dispatch"));
        prop_assert_eq!(first.input_identity().expect("inputs"), second.input_identity().expect("inputs"));
    }
}

#[test]
fn legacy_dispatch_is_byte_identical_and_unqualified() {
    let original = include_str!("../../../fixtures/function/invocation.wasm.json").trim();
    let old = invocation();
    assert!(old.provenance_commit.qualified().is_none());
    assert_eq!(
        String::from_utf8(norn_ir::canonical::canonical_json_of(&old).expect("canonical"))
            .expect("utf8"),
        original
    );
    let legacy: serde_json::Value = serde_json::from_str(original).expect("json");
    let expected = norn_canonical::domain_hash_hex(norn_ir::function::INVOCATION_DOMAIN, &legacy)
        .expect("v1 digest");
    assert_eq!(
        old.content_digest().expect("old dispatch").as_str(),
        format!("sha256:{expected}")
    );
}
