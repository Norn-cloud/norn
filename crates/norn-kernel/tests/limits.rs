//! Resource-limit profile and ingress limits at the kernel boundary (T2-LIM-1).

use std::cell::Cell;
use std::convert::Infallible;

use norn_kernel::limits::{NORN_LIMITS_PROFILE_ID, NORN_LIMITS_V1_SHA256, NORN_LIMITS_V1_VERSION};
use norn_kernel::{
    Decider, Decision, DecisionOf, Entropy, EnvelopeKind, EnvelopeLimits, IngressLimit,
    IngressLimitExceeded, LimitProfileRef, LogicalTime, Observations, ResourceLimitProfile,
    SliceLimits, SliceMeter, StepError, StepOf, check_envelope_bytes, step,
};

/// One selected slice record as a reader sees it before appending it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    canonical_bytes: u64,
    relation_depth: u32,
}

const fn record(canonical_bytes: u64, relation_depth: u32) -> Record {
    Record {
        canonical_bytes,
        relation_depth,
    }
}

/// Decides one `Noted` event per slice record. Counts `decide` calls so a test
/// can prove a refused slice never reached it.
#[derive(Default)]
struct PerRecord {
    decides: Cell<u32>,
}

impl Decider for PerRecord {
    type State = usize;
    type Input = Vec<Record>;
    type Event = ();
    type Command = ();
    type Rejection = Infallible;

    fn initial_state(&self) -> usize {
        0
    }

    fn decide(
        &self,
        _state: &usize,
        input: &Vec<Record>,
        _observations: &Observations,
    ) -> Result<DecisionOf<Self>, Infallible> {
        self.decides.set(self.decides.get().saturating_add(1));
        Ok(Decision::events(vec![(); input.len()]))
    }

    fn evolve(&self, state: &usize, _event: &()) -> usize {
        state.saturating_add(1)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TurnError {
    Ingress(IngressLimitExceeded),
    Step(StepError<Infallible>),
}

/// A host turn under `profile`: read the slice through the meter (refusing
/// while reading), then `step` under the profile's envelope.
fn turn(
    decider: &PerRecord,
    profile: &ResourceLimitProfile,
    source: &[Record],
) -> Result<StepOf<PerRecord>, TurnError> {
    let mut meter = SliceMeter::for_profile(profile);
    let mut slice = Vec::new();
    for record in source {
        meter
            .admit_record(record.canonical_bytes, record.relation_depth)
            .map_err(TurnError::Ingress)?;
        slice.push(*record);
    }
    let observations = Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32]));
    step(decider, &0, &slice, &observations, &profile.envelope).map_err(TurnError::Step)
}

#[test]
fn shipped_profile_is_pinned_and_unifies_the_envelope() {
    let profile = ResourceLimitProfile::norn_limits_v1();
    assert_eq!(profile.envelope, EnvelopeLimits::V1);
    assert_eq!(EnvelopeLimits::default(), EnvelopeLimits::V1);
    let reference = profile.reference().unwrap();
    assert_eq!(reference, LimitProfileRef::norn_limits_v1());
    assert_eq!(reference.id, NORN_LIMITS_PROFILE_ID);
    assert_eq!(reference.version, NORN_LIMITS_V1_VERSION);
    assert_eq!(reference.sha256, NORN_LIMITS_V1_SHA256);
}

#[test]
fn slice_within_limits_is_admitted_and_decided() {
    let decider = PerRecord::default();
    let profile = ResourceLimitProfile::norn_limits_v1();
    let source = [record(1_024, 0), record(2_048, 3)];
    let accepted = turn(&decider, &profile, &source).unwrap();
    assert_eq!(accepted.events.len(), 2);
    assert_eq!(decider.decides.get(), 1);
}

#[test]
fn over_depth_slice_is_refused_while_reading_with_zero_effects() {
    let decider = PerRecord::default();
    let profile = ResourceLimitProfile::norn_limits_v1();
    let source = [record(10, 0), record(10, 4), record(10, 0)];
    assert_eq!(
        turn(&decider, &profile, &source),
        Err(TurnError::Ingress(IngressLimitExceeded {
            kind: IngressLimit::SliceRelationDepth,
            limit: 3,
            actual: 4,
        }))
    );
    assert_eq!(
        decider.decides.get(),
        0,
        "a refused slice never reaches decide"
    );
}

#[test]
fn over_record_slice_is_refused_at_the_first_excess_record() {
    let decider = PerRecord::default();
    let profile = ResourceLimitProfile::norn_limits_v1();
    let source = vec![record(1, 0); 1_001];
    assert_eq!(
        turn(&decider, &profile, &source),
        Err(TurnError::Ingress(IngressLimitExceeded {
            kind: IngressLimit::SliceRecords,
            limit: 1_000,
            actual: 1_001,
        }))
    );
    assert_eq!(decider.decides.get(), 0);
    // Exactly at the cap is admitted.
    assert!(
        turn(&decider, &profile, &source[..1_000]).is_err_and(|error| matches!(
            error,
            TurnError::Step(StepError::EnvelopeExceeded { .. })
        ))
    );
}

#[test]
fn over_byte_slice_is_refused_before_it_is_assembled() {
    let decider = PerRecord::default();
    let profile = ResourceLimitProfile::norn_limits_v1();
    let source = [record(200 * 1024, 0), record(56 * 1024, 1), record(1, 0)];
    assert_eq!(
        turn(&decider, &profile, &source),
        Err(TurnError::Ingress(IngressLimitExceeded {
            kind: IngressLimit::SliceBytes,
            limit: 256 * 1024,
            actual: 256 * 1024 + 1,
        }))
    );
    assert_eq!(decider.decides.get(), 0);
}

#[test]
fn slice_meter_charges_nothing_on_refusal_and_stays_refused() {
    let limits = SliceLimits {
        max_canonical_bytes: 10,
        max_records: 2,
        max_relation_depth: 1,
        qualification_ceiling_bytes: 10,
    };
    let mut meter = SliceMeter::new(limits);
    meter.admit_record(6, 1).unwrap();
    let refused = meter.admit_record(5, 0).unwrap_err();
    assert_eq!(refused.kind, IngressLimit::SliceBytes);
    assert_eq!((meter.bytes(), meter.records()), (6, 1));
    assert_eq!(meter.refused(), Some(refused));
    // Sticky: a record that would fit is still refused.
    assert_eq!(meter.admit_record(1, 0), Err(refused));
    assert_eq!((meter.bytes(), meter.records()), (6, 1));
    // Saturating: a huge record cannot wrap the counter.
    let mut meter = SliceMeter::new(limits);
    let huge = meter.admit_record(u64::MAX, 0).unwrap_err();
    assert_eq!(
        (huge.kind, huge.actual),
        (IngressLimit::SliceBytes, u64::MAX)
    );
}

#[test]
fn over_envelope_decision_is_refused_by_step_with_zero_effects() {
    let decider = PerRecord::default();
    let profile = ResourceLimitProfile::norn_limits_v1();
    // 33 tiny records: the slice is fine, the decision emits 33 events.
    let source = vec![record(1, 0); 33];
    assert_eq!(
        turn(&decider, &profile, &source),
        Err(TurnError::Step(StepError::EnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: 32,
            actual: 33,
        }))
    );
}

#[test]
fn envelope_bytes_are_checked_against_the_profile() {
    let limits = ResourceLimitProfile::norn_limits_v1().envelope;
    assert_eq!(check_envelope_bytes(&limits, 32 * 1024), Ok(()));
    assert_eq!(
        check_envelope_bytes(&limits, 32 * 1024 + 1),
        Err(IngressLimitExceeded {
            kind: IngressLimit::EnvelopeBytes,
            limit: 32 * 1024,
            actual: 32 * 1024 + 1,
        })
    );
}

#[test]
fn ingress_refusal_serializes_as_typed_data() {
    let refusal = IngressLimitExceeded {
        kind: IngressLimit::SliceRecords,
        limit: 1_000,
        actual: 1_001,
    };
    let json = serde_json::to_value(refusal).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"kind": "slice_records", "limit": 1000, "actual": 1001})
    );
    assert_eq!(
        serde_json::from_value::<IngressLimitExceeded>(json).unwrap(),
        refusal
    );
}
