//! Exhaustive-finite algebraic laws for `FieldPatch` / `LinkPatchOp` pure
//! observation reduction and typed `SourceVersion` parsing/ordering (`FEED-MODE-1` / B4).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use norn_ir::feed::{
    FEED_EXTENSION_KEY, FeedContract, FeedDeclaration, FeedMode, FieldPatch, LinkPatchOp,
    SourceVersion, SourceVersionError, VersionKind, compose_field_patch, compose_link_patch,
    compose_object_patches, reduce_field, reduce_link, reduce_object,
};
use norn_ir::{RuntimeObject, RuntimeValue};
use serde_json::json;

/// Exhaustive-finite proof over the 4-element state domain
/// `{None, Some(Null), Some("a"), Some("b")}` and 4-element patch domain
/// `{Absent, ExplicitNull, Value("a"), Value("b")}` that:
/// 1. `FieldPatch::Absent` is left and right identity of `compose_field_patch`.
/// 2. `compose_field_patch` is associative.
/// 3. `reduce_field` is a monoid action homomorphism:
///    `reduce(reduce(s, p1), p2) == reduce(s, compose(p1, p2))`.
/// 4. `Absent` preserves prior state, `ExplicitNull` clears to `Null`, and
///    `Value(v)` overwrites.
#[test]
fn field_patch_monoid_and_reducer_homomorphism_exhaustive_finite() {
    let states = [
        None,
        Some(RuntimeValue::Null),
        Some(RuntimeValue::from("a")),
        Some(RuntimeValue::from("b")),
    ];
    let patches = [
        FieldPatch::Absent,
        FieldPatch::ExplicitNull,
        FieldPatch::Value(RuntimeValue::from("a")),
        FieldPatch::Value(RuntimeValue::from("b")),
    ];

    for p in &patches {
        assert_eq!(compose_field_patch(&FieldPatch::Absent, p), *p);
        assert_eq!(compose_field_patch(p, &FieldPatch::Absent), *p);
    }
    for s in &states {
        assert_eq!(
            reduce_field(s.as_ref(), &FieldPatch::Absent, RuntimeValue::Null),
            *s
        );
        assert_eq!(
            reduce_field(s.as_ref(), &FieldPatch::ExplicitNull, RuntimeValue::Null),
            Some(RuntimeValue::Null)
        );
        for v in [RuntimeValue::from("a"), RuntimeValue::from("b")] {
            assert_eq!(
                reduce_field(
                    s.as_ref(),
                    &FieldPatch::Value(v.clone()),
                    RuntimeValue::Null
                ),
                Some(v)
            );
        }
    }
    for p1 in &patches {
        for p2 in &patches {
            let p12 = compose_field_patch(p1, p2);
            for p3 in &patches {
                assert_eq!(
                    compose_field_patch(&p12, p3),
                    compose_field_patch(p1, &compose_field_patch(p2, p3))
                );
            }
            for s in &states {
                let step1 = reduce_field(s.as_ref(), p1, RuntimeValue::Null);
                let step2 = reduce_field(step1.as_ref(), p2, RuntimeValue::Null);
                let composed = reduce_field(s.as_ref(), &p12, RuntimeValue::Null);
                assert_eq!(step2, composed);
            }
        }
    }
}

/// Exhaustive-finite proof of `LinkPatchOp` monoid identity, associativity,
/// and reduction homomorphism.
#[test]
fn link_patch_monoid_and_reducer_homomorphism_exhaustive_finite() {
    let states = [None, Some("t1"), Some("t2")];
    let ops = [
        LinkPatchOp::Absent,
        LinkPatchOp::Clear,
        LinkPatchOp::Target("t1".into()),
        LinkPatchOp::Target("t2".into()),
    ];
    for op in &ops {
        assert_eq!(compose_link_patch(&LinkPatchOp::Absent, op), *op);
        assert_eq!(compose_link_patch(op, &LinkPatchOp::Absent), *op);
    }
    for op1 in &ops {
        for op2 in &ops {
            let op12 = compose_link_patch(op1, op2);
            for op3 in &ops {
                assert_eq!(
                    compose_link_patch(&op12, op3),
                    compose_link_patch(op1, &compose_link_patch(op2, op3))
                );
            }
            for s in states {
                let step1 = reduce_link(s, op1);
                let step2 = reduce_link(step1.as_deref(), op2);
                let composed = reduce_link(s, &op12);
                assert_eq!(step2, composed);
            }
        }
    }
}

/// Object-level reducer homomorphism across multi-field tri-state patches.
#[test]
fn object_patch_reducer_matches_composed_patch() {
    let prior = RuntimeObject::from([
        ("qty".to_owned(), RuntimeValue::from(100)),
        ("note".to_owned(), RuntimeValue::from("initial")),
        ("loc".to_owned(), RuntimeValue::from("north")),
    ]);
    let patch1 = BTreeMap::from([
        ("qty".to_owned(), FieldPatch::Value(RuntimeValue::from(125))),
        ("note".to_owned(), FieldPatch::Absent),
    ]);
    let patch2 = BTreeMap::from([
        ("note".to_owned(), FieldPatch::ExplicitNull),
        ("loc".to_owned(), FieldPatch::Absent),
    ]);
    let sequential = reduce_object(Some(&reduce_object(Some(&prior), &patch1)), &patch2);
    let combined = compose_object_patches(&patch1, &patch2);
    let single_shot = reduce_object(Some(&prior), &combined);
    assert_eq!(sequential, single_shot);
    assert_eq!(sequential.get("qty"), Some(&RuntimeValue::from(125)));
    assert_eq!(sequential.get("note"), Some(&RuntimeValue::Null));
    assert_eq!(sequential.get("loc"), Some(&RuntimeValue::from("north")));
}

/// Typed `SourceVersion` parsing and ordering for `Uint64`, `Int64`, `Rfc3339`,
/// `Lexicographical`, and `Opaque`.
#[test]
fn source_version_parsing_and_typed_ordering() {
    let u99 = SourceVersion::parse(VersionKind::Uint64, "99").expect("99");
    let u100 = SourceVersion::parse(VersionKind::Uint64, "100").expect("100");
    assert_eq!(u99.try_cmp(&u100), Ok(Ordering::Less));
    assert!(SourceVersion::parse(VersionKind::Uint64, "-1").is_err());
    assert!(SourceVersion::parse(VersionKind::Uint64, "+1").is_err());
    assert!(SourceVersion::parse(VersionKind::Uint64, "abc").is_err());

    let neg10 = SourceVersion::parse(VersionKind::Int64, "-10").expect("-10");
    let neg2 = SourceVersion::parse(VersionKind::Int64, "-2").expect("-2");
    let pos2 = SourceVersion::parse(VersionKind::Int64, "2").expect("2");
    assert_eq!(neg10.try_cmp(&neg2), Ok(Ordering::Less));
    assert_eq!(neg2.try_cmp(&pos2), Ok(Ordering::Less));
    assert!(SourceVersion::parse(VersionKind::Int64, "+2").is_err());

    // 10:00+02:00 == 08:00Z < 08:00:00.5Z < 03:01-05:00 (= 08:01Z)
    let plus2 =
        SourceVersion::parse(VersionKind::Rfc3339, "2026-11-12T10:00:00+02:00").expect("t1");
    let utc = SourceVersion::parse(VersionKind::Rfc3339, "2026-11-12T08:00:00Z").expect("t2");
    let utc_half =
        SourceVersion::parse(VersionKind::Rfc3339, "2026-11-12T08:00:00.5Z").expect("t3");
    let utc_frac =
        SourceVersion::parse(VersionKind::Rfc3339, "2026-11-12T08:00:00.500000000Z").expect("t4");
    let minus5 =
        SourceVersion::parse(VersionKind::Rfc3339, "2026-11-12T03:01:00-05:00").expect("t5");
    assert_eq!(plus2.try_cmp(&utc), Ok(Ordering::Equal));
    assert_eq!(utc.try_cmp(&utc_half), Ok(Ordering::Less));
    assert_eq!(utc_half.try_cmp(&utc_frac), Ok(Ordering::Equal));
    assert_eq!(utc_half.try_cmp(&minus5), Ok(Ordering::Less));

    // Leap year validation and missing offset refusal.
    assert!(SourceVersion::parse(VersionKind::Rfc3339, "2024-02-29T00:00:00Z").is_ok());
    assert!(SourceVersion::parse(VersionKind::Rfc3339, "2025-02-29T00:00:00Z").is_err());
    assert!(SourceVersion::parse(VersionKind::Rfc3339, "2026-11-12 08:00:00").is_err());

    // Opaque refuses ordering; mismatched kinds refuse comparison.
    let op_a = SourceVersion::parse(VersionKind::Opaque, "tok-a").expect("opaque");
    let op_b = SourceVersion::parse(VersionKind::Opaque, "tok-b").expect("opaque");
    assert_eq!(
        op_a.try_cmp(&op_b),
        Err(SourceVersionError::UnorderableOpaque)
    );
    assert_eq!(
        u99.try_cmp(&utc),
        Err(SourceVersionError::IncompatibleKinds {
            left: VersionKind::Uint64,
            right: VersionKind::Rfc3339,
        })
    );
}

/// `FeedContract` omits default fields on the wire and round-trips `Patch` +
/// typed `VersionKind` declarations cleanly.
#[test]
fn feed_declaration_defaults_and_roundtrip() {
    let default_contract = FeedContract::default();
    assert_eq!(
        serde_json::to_value(default_contract).expect("json"),
        json!({})
    );

    let ext = BTreeMap::from([(
        FEED_EXTENSION_KEY.to_owned(),
        json!({
            "feeds": {
                "Warehouse Lot Balance": {
                    "mode": "patch",
                    "versionKind": "rfc3339"
                }
            }
        }),
    )]);
    let decl = FeedDeclaration::from_extensions(Some(&ext))
        .expect("parse")
        .expect("present");
    assert_eq!(decl.mode_for("Warehouse Lot Balance"), FeedMode::Patch);
    assert_eq!(decl.mode_for("Item"), FeedMode::FullSnapshot);
    assert_eq!(
        decl.feeds
            .get("Warehouse Lot Balance")
            .map(|c| c.version_kind),
        Some(VersionKind::Rfc3339)
    );
}
