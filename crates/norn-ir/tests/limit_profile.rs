//! `norn.limits/1`: canonical bytes and golden content hash (T2-LIM-1).

use norn_ir::{
    EnvelopeLimits, LimitProfileRef, NORN_LIMITS_PROFILE_ID, NORN_LIMITS_V1_SHA256,
    NORN_LIMITS_V1_VERSION, ResourceLimitProfile,
};

#[test]
fn norn_limits_v1_hash_matches_golden() {
    let profile = ResourceLimitProfile::norn_limits_v1();
    let hash = profile.content_sha256().unwrap();
    assert_eq!(
        hash, NORN_LIMITS_V1_SHA256,
        "norn.limits/1 bytes changed: publish a new profile version instead of editing v1"
    );
    assert_eq!(
        profile.reference().unwrap(),
        LimitProfileRef::norn_limits_v1()
    );
}

#[test]
fn norn_limits_v1_values_are_the_ruling_targets() {
    let p = ResourceLimitProfile::norn_limits_v1();
    assert_eq!(p.id, NORN_LIMITS_PROFILE_ID);
    assert_eq!(p.version, NORN_LIMITS_V1_VERSION);
    assert_eq!(p.decide.cpu_p99_micros, 2_000);
    assert_eq!(p.decide.fuel_envelope_micros, 5_000);
    assert_eq!(p.turn.cpu_p99_micros, 10_000);
    assert_eq!(p.slice.max_canonical_bytes, 256 * 1024);
    assert_eq!(p.slice.max_records, 1_000);
    assert_eq!(p.slice.max_relation_depth, 3);
    assert_eq!(p.slice.qualification_ceiling_bytes, 1024 * 1024);
    assert_eq!(p.memory.max_turn_working_bytes, 16 * 1024 * 1024);
    assert_eq!(p.memory.live_footprint_target_bytes, 64 * 1024 * 1024);
    assert_eq!(p.envelope, EnvelopeLimits::V1);
    assert_eq!(p.envelope.max_output_bytes, 32 * 1024);
    assert_eq!(
        (
            p.envelope.max_events,
            p.envelope.max_commands,
            p.envelope.max_timers
        ),
        (32, 32, 32)
    );
    assert_eq!(p.helper.cpu_p99_micros, 100);
    assert_eq!(p.helper.aggregate_micros_per_decision, 500);
    assert_eq!(p.helper.max_guest_memory_bytes, 1024 * 1024);
    assert!(p.turn.fuel_units >= p.decide.fuel_units);
}

/// The hand-built canonical value is exactly the serde document in Norn1
/// canonical form, so the hash commits to the wire shape.
#[test]
fn canonical_bytes_equal_canonicalized_serde_json() {
    let profile = ResourceLimitProfile::norn_limits_v1();
    let serde_bytes = serde_json::to_vec(&profile).unwrap();
    let canonical = norn_canonical::canonicalize_json(&serde_bytes).unwrap();
    assert_eq!(profile.canonical_json().unwrap(), canonical);

    let round_trip: ResourceLimitProfile = serde_json::from_slice(&serde_bytes).unwrap();
    assert_eq!(round_trip, profile);
}

#[test]
fn any_changed_value_changes_the_hash() {
    let base = ResourceLimitProfile::norn_limits_v1();
    let base_hash = base.content_sha256().unwrap();
    let mut variants = Vec::new();
    let mut v = base.clone();
    v.version = 2;
    variants.push(v);
    let mut v = base.clone();
    v.slice.max_records = 1_001;
    variants.push(v);
    let mut v = base.clone();
    v.envelope.max_output_bytes += 1;
    variants.push(v);
    let mut v = base;
    v.turn.fuel_units += 1;
    variants.push(v);
    for variant in variants {
        assert_ne!(variant.content_sha256().unwrap(), base_hash);
    }
}

#[test]
fn profile_rejects_unknown_fields() {
    let mut value = serde_json::to_value(ResourceLimitProfile::norn_limits_v1()).unwrap();
    value["slice"]["max_rows"] = serde_json::json!(5);
    assert!(serde_json::from_value::<ResourceLimitProfile>(value).is_err());
}

/// The checked-in canonical bytes of `norn.limits/1`, so non-Rust hosts can
/// verify the same hash without this crate.
#[test]
fn norn_limits_v1_canonical_bytes_match_golden_file() {
    let golden = include_bytes!("golden/norn.limits.v1.canonical.json");
    let bytes = ResourceLimitProfile::norn_limits_v1()
        .canonical_json()
        .unwrap();
    assert_eq!(bytes.as_slice(), golden.as_slice());
    assert_eq!(norn_canonical::canonicalize_json(golden).unwrap(), bytes);
}
