//! Decider form, closed effect ADT, and tick-shaped `step` (T2-KER-1).

use std::fmt;

use norn_kernel::{
    Command, CommandId, CommitId, Decider, Decision, DependencyToken, Effect, Entropy,
    EnvelopeKind, EnvelopeLimits, FreshnessPolicy, Generation, IdempotencyKey, InputIdentity,
    LogicalTime, Observations, OwnerVersion, StepError, Timer, TimerId, World, WorldId, replay,
    step,
};
use serde::{Deserialize, Serialize};

// A small approval lifecycle: submit → (approve | timeout).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Draft,
    Submitted,
    Approved,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    status: Status,
    submitted_at: Option<LogicalTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Submit,
    Approve,
    Timeout,
    /// Emits `events` events, `commands` performs, and `timers` schedules.
    Burst {
        events: usize,
        commands: usize,
        timers: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Event {
    Submitted { at: LogicalTime },
    Approved,
    Expired,
    Noted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Cmd {
    NotifyApprover,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invalid(Status);

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "input not allowed in {:?}", self.0)
    }
}

impl std::error::Error for Invalid {}

const TIMEOUT_MS: u64 = 60_000;

fn entropy_id(entropy: &Entropy) -> String {
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&entropy.as_bytes()[..8]);
    format!("{:016x}", u64::from_le_bytes(prefix))
}

fn notify(obs: &Observations) -> Command<Cmd> {
    // Identity derives from recorded entropy, never from an ambient source.
    let id = entropy_id(&obs.entropy);
    Command {
        id: CommandId(format!("notify-{id}")),
        generation: Generation::default(),
        idempotency_key: IdempotencyKey(format!("notify-{id}")),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: obs.now.checked_add_millis(TIMEOUT_MS),
        payload: Cmd::NotifyApprover,
    }
}

struct Approval;

impl Decider for Approval {
    type State = State;
    type Input = Input;
    type Event = Event;
    type Command = Cmd;
    type Rejection = Invalid;

    fn initial_state(&self) -> State {
        State {
            status: Status::Draft,
            submitted_at: None,
        }
    }

    fn decide(
        &self,
        state: &State,
        input: &Input,
        obs: &Observations,
    ) -> Result<Decision<Event, Cmd, Input>, Invalid> {
        match (state.status, input) {
            (Status::Draft, Input::Submit) => Ok(Decision::none()
                .with_event(Event::Submitted { at: obs.now })
                .with_effect(Effect::Perform(notify(obs)))
                .with_effect(Effect::Schedule(Timer {
                    id: TimerId("expire".into()),
                    due: obs
                        .now
                        .checked_add_millis(TIMEOUT_MS)
                        .ok_or(Invalid(state.status))?,
                    payload: Input::Timeout,
                }))),
            (Status::Submitted, Input::Approve) => Ok(Decision::events(vec![Event::Approved])
                .with_effect(Effect::CancelTimer(TimerId("expire".into())))),
            (Status::Submitted, Input::Timeout) => Ok(Decision::events(vec![Event::Expired])),
            (
                _,
                Input::Burst {
                    events,
                    commands,
                    timers,
                },
            ) => {
                let mut decision = Decision::events(vec![Event::Noted; *events]);
                for _ in 0..*commands {
                    decision = decision.with_effect(Effect::Perform(notify(obs)));
                }
                for n in 0..*timers {
                    decision = decision.with_effect(Effect::Schedule(Timer {
                        id: TimerId(format!("t{n}")),
                        due: obs.now,
                        payload: Input::Timeout,
                    }));
                }
                Ok(decision)
            }
            (status, _) => Err(Invalid(status)),
        }
    }

    fn evolve(&self, state: &State, event: &Event) -> State {
        match event {
            Event::Submitted { at } => State {
                status: Status::Submitted,
                submitted_at: Some(*at),
            },
            Event::Approved => State {
                status: Status::Approved,
                ..state.clone()
            },
            Event::Expired => State {
                status: Status::Expired,
                ..state.clone()
            },
            Event::Noted => state.clone(),
        }
    }
}

fn obs(now: u64) -> Observations {
    Observations::main(LogicalTime::from_millis(now), Entropy::from_bytes([7; 32]))
}

fn in_world(world: World) -> Observations {
    Observations {
        world,
        ..obs(1_000)
    }
}

#[test]
fn step_decides_then_evolves() {
    let d = Approval;
    let s0 = d.initial_state();
    let out = step(&d, &s0, &Input::Submit, &obs(1_000), &EnvelopeLimits::V1).unwrap();
    assert_eq!(
        out.events,
        [Event::Submitted {
            at: LogicalTime::from_millis(1_000)
        }]
    );
    assert_eq!(out.state.status, Status::Submitted);
    assert_eq!(out.effects.len(), 2);
    assert!(out.effects[0].is_perform());
    assert!(out.effects[1].is_timer());
    let Effect::Schedule(timer) = &out.effects[1] else {
        panic!("expected a timer")
    };
    assert_eq!(timer.due, LogicalTime::from_millis(61_000));
    // The borrowed input state is untouched.
    assert_eq!(s0, d.initial_state());
}

#[test]
fn rejection_produces_no_transition() {
    let d = Approval;
    let s0 = d.initial_state();
    let err = step(&d, &s0, &Input::Approve, &obs(1), &EnvelopeLimits::V1).unwrap_err();
    assert_eq!(err, StepError::Rejected(Invalid(Status::Draft)));
    assert_eq!(err.to_string(), "decision rejected");
    assert_eq!(
        std::error::Error::source(&err).map(ToString::to_string),
        Some("input not allowed in Draft".to_owned())
    );
    assert_eq!(s0, d.initial_state());
}

#[test]
fn replay_is_fold_of_evolve_over_step_events() {
    let d = Approval;
    let mut state = d.initial_state();
    let mut ledger = Vec::new();
    for (input, now) in [(Input::Submit, 10), (Input::Approve, 20)] {
        let out = step(&d, &state, &input, &obs(now), &EnvelopeLimits::V1).unwrap();
        ledger.extend(out.events);
        state = out.state;
    }
    assert_eq!(replay(&d, &ledger), state);
    assert_eq!(replay(&d, &[]), d.initial_state());
    assert_eq!(
        state,
        State {
            status: Status::Approved,
            submitted_at: Some(LogicalTime::from_millis(10)),
        }
    );
}

#[test]
fn timer_delivery_is_an_ordinary_input() {
    let d = Approval;
    let submitted = step(
        &d,
        &d.initial_state(),
        &Input::Submit,
        &obs(0),
        &EnvelopeLimits::V1,
    )
    .unwrap();
    // The host's `clock` delivers the scheduled payload back, unchanged, as
    // the next input — after a serialization round trip through the timer
    // table.
    let Some(Effect::Schedule(timer)) = submitted.effects.iter().find(|e| e.is_timer()) else {
        panic!("expected a scheduled timer")
    };
    let stored = serde_json::to_string(timer).unwrap();
    let due: Timer<Input> = serde_json::from_str(&stored).unwrap();
    let expired = step(
        &d,
        &submitted.state,
        &due.payload,
        &Observations::main(due.due, Entropy::from_bytes([1; 32])),
        &EnvelopeLimits::V1,
    )
    .unwrap();
    assert_eq!(expired.events, [Event::Expired]);
    assert_eq!(expired.state.status, Status::Expired);
    assert!(expired.effects.is_empty());
    // A late approval after expiry is rejected.
    let err = step(
        &d,
        &expired.state,
        &Input::Approve,
        &obs(TIMEOUT_MS),
        &EnvelopeLimits::V1,
    )
    .unwrap_err();
    assert_eq!(err, StepError::Rejected(Invalid(Status::Expired)));
}

#[test]
fn step_is_deterministic_in_its_inputs() {
    let d = Approval;
    let s0 = d.initial_state();
    let a = step(&d, &s0, &Input::Submit, &obs(5), &EnvelopeLimits::V1).unwrap();
    let b = step(&d, &s0, &Input::Submit, &obs(5), &EnvelopeLimits::V1).unwrap();
    assert_eq!(a, b);
    // Different entropy → different command identity; nothing else differs.
    let other = Observations {
        entropy: Entropy::from_bytes([9; 32]),
        ..obs(5)
    };
    let c = step(&d, &s0, &Input::Submit, &other, &EnvelopeLimits::V1).unwrap();
    assert_eq!(a.events, c.events);
    assert_ne!(a.effects, c.effects);
}

#[test]
fn historical_world_is_read_only() {
    let d = Approval;
    let world = World::Historical {
        commit: CommitId("c1".into()),
    };
    let err = step(
        &d,
        &d.initial_state(),
        &Input::Submit,
        &in_world(world),
        &EnvelopeLimits::V1,
    )
    .unwrap_err();
    assert_eq!(err, StepError::ReadOnlyWorld);
}

#[test]
fn only_main_performs() {
    let d = Approval;
    let fork = in_world(World::Fork {
        id: WorldId("what-if".into()),
    });
    let err = step(
        &d,
        &d.initial_state(),
        &Input::Submit,
        &fork,
        &EnvelopeLimits::V1,
    )
    .unwrap_err();
    assert_eq!(err, StepError::PerformOutsideMain { count: 1 });

    // A fork may still commit events and manage timers.
    let submitted = State {
        status: Status::Submitted,
        submitted_at: Some(LogicalTime::EPOCH),
    };
    let out = step(&d, &submitted, &Input::Approve, &fork, &EnvelopeLimits::V1).unwrap();
    assert_eq!(out.state.status, Status::Approved);
    assert_eq!(out.effects, [Effect::CancelTimer(TimerId("expire".into()))]);
}

#[test]
fn envelope_limits_are_enforced_per_kind() {
    let d = Approval;
    let s0 = d.initial_state();
    let limits = EnvelopeLimits {
        max_events: 2,
        max_commands: 1,
        max_timers: 3,
        ..EnvelopeLimits::V1
    };
    let burst = |events, commands, timers| Input::Burst {
        events,
        commands,
        timers,
    };
    assert!(step(&d, &s0, &burst(2, 1, 3), &obs(0), &limits).is_ok());
    for (input, kind, limit, actual) in [
        (burst(3, 0, 0), EnvelopeKind::Events, 2, 3),
        (burst(0, 2, 0), EnvelopeKind::Commands, 1, 2),
        (burst(0, 0, 4), EnvelopeKind::Timers, 3, 4),
    ] {
        let err = step(&d, &s0, &input, &obs(0), &limits).unwrap_err();
        assert_eq!(
            err,
            StepError::EnvelopeExceeded {
                kind,
                limit,
                actual
            }
        );
    }
    let err = step(
        &d,
        &s0,
        &burst(33, 0, 0),
        &obs(0),
        &EnvelopeLimits::default(),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "decision emitted 33 events, limit is 32");
}

#[test]
fn effects_are_plain_serializable_data() {
    let effects: Vec<Effect<Cmd, Input>> = vec![
        Effect::Perform(Command {
            id: CommandId("c1".into()),
            generation: Generation(3),
            idempotency_key: IdempotencyKey("k1".into()),
            freshness: FreshnessPolicy::RelevantState {
                token: DependencyToken("slice:v9".into()),
            },
            deadline: None,
            payload: Cmd::NotifyApprover,
        }),
        Effect::Schedule(Timer {
            id: TimerId("t1".into()),
            due: LogicalTime::from_millis(42),
            payload: Input::Timeout,
        }),
        Effect::CancelTimer(TimerId("t1".into())),
    ];
    let json = serde_json::to_value(&effects).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"perform": {
                "id": "c1",
                "generation": 3,
                "idempotency_key": "k1",
                "freshness": {"relevant_state": {"token": "slice:v9"}},
                "deadline": null,
                "payload": "NotifyApprover"
            }},
            {"schedule": {"id": "t1", "due": 42, "payload": "Timeout"}},
            {"cancel_timer": "t1"}
        ])
    );
    let back: Vec<Effect<Cmd, Input>> = serde_json::from_value(json).unwrap();
    assert_eq!(back, effects);

    let decision: Decision<Event, Cmd, Input> = Decision::events(vec![Event::Approved])
        .with_effect(Effect::CancelTimer(TimerId("t".into())));
    let text = serde_json::to_string(&decision).unwrap();
    assert_eq!(
        serde_json::from_str::<Decision<Event, Cmd, Input>>(&text).unwrap(),
        decision
    );
}

#[test]
fn ingress_rejects_unknown_fields() {
    let bad = serde_json::json!({
        "schedule": {"id": "t", "due": 1, "payload": "Timeout", "callback": "x"}
    });
    assert!(serde_json::from_value::<Effect<Cmd, Input>>(bad).is_err());
    let mut bad = serde_json::to_value(obs(1)).unwrap();
    bad["wall_clock"] = serde_json::json!(5);
    assert!(serde_json::from_value::<Observations>(bad).is_err());
}

#[test]
fn observations_round_trip() {
    let observations = in_world(World::Fork {
        id: WorldId("w".into()),
    });
    let json = serde_json::to_value(&observations).unwrap();
    assert_eq!(json["world"], serde_json::json!({"fork": {"id": "w"}}));
    assert_eq!(json["now"], serde_json::json!(1_000));
    let back: Observations = serde_json::from_value(json).unwrap();
    assert_eq!(back, observations);
    assert_eq!(
        serde_json::to_value(World::Main).unwrap(),
        serde_json::json!("main")
    );
}

#[test]
fn world_capabilities() {
    let fork = World::Fork {
        id: WorldId("f".into()),
    };
    let hist = World::Historical {
        commit: CommitId("c".into()),
    };
    assert!(World::Main.may_perform() && World::Main.is_writable());
    assert!(!fork.may_perform() && fork.is_writable());
    assert!(!hist.may_perform() && !hist.is_writable());
}

#[test]
fn counters_do_not_overflow() {
    assert_eq!(
        LogicalTime::from_millis(u64::MAX).checked_add_millis(1),
        None
    );
    assert_eq!(
        LogicalTime::from_millis(1).checked_add_millis(2),
        Some(LogicalTime::from_millis(3))
    );
    assert_eq!(Generation(u64::MAX).checked_next(), None);
    assert_eq!(Generation(1).checked_next(), Some(Generation(2)));
}

#[test]
fn destination_cas_pins_immutable_input_identity() {
    let cas = FreshnessPolicy::DestinationCas {
        inputs: InputIdentity("sha256:inputs-a".into()),
        expected: OwnerVersion(4),
    };
    let json = serde_json::to_value(&cas).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"destination_cas": {"inputs": "sha256:inputs-a", "expected": 4}})
    );
    assert_eq!(
        serde_json::from_value::<FreshnessPolicy>(json).unwrap(),
        cas
    );
    // Destination CAS alone is not a valid policy: the input identity is
    // mandatory, so a reconciler can never accept a result on the version
    // check only.
    let version_only = serde_json::json!({"destination_cas": {"expected": 4}});
    assert!(serde_json::from_value::<FreshnessPolicy>(version_only).is_err());
    // Same destination version, different inputs: distinct policies.
    let other = FreshnessPolicy::DestinationCas {
        inputs: InputIdentity("sha256:inputs-b".into()),
        expected: OwnerVersion(4),
    };
    assert_ne!(cas, other);
}

#[test]
fn envelope_limits_serialize_as_fixed_width_integers() {
    let limits = EnvelopeLimits::V1;
    let json = serde_json::to_value(limits).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "max_output_bytes": 32_768,
            "max_events": 32,
            "max_commands": 32,
            "max_timers": 32
        })
    );
    assert_eq!(
        serde_json::from_value::<EnvelopeLimits>(json).unwrap(),
        limits
    );
    let max = EnvelopeLimits {
        max_output_bytes: u64::MAX,
        max_events: u32::MAX,
        max_commands: 0,
        max_timers: 1,
    };
    let text = serde_json::to_string(&max).unwrap();
    assert_eq!(serde_json::from_str::<EnvelopeLimits>(&text).unwrap(), max);
    // Values wider than u32 are rejected on every host, native or wasm32.
    let too_wide = serde_json::json!({
        "max_output_bytes": 0,
        "max_events": 4_294_967_296_u64,
        "max_commands": 0,
        "max_timers": 0
    });
    assert!(serde_json::from_value::<EnvelopeLimits>(too_wide).is_err());
}
