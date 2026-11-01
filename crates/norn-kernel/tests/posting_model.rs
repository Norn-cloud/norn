//! Executable reference for POST-*; see verification/posting-v1/contract.md.
//! Valuation witnesses are inputs, not an implementation of the stock fold.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "small bounded reference states and fixed witness indices"
)]

#[derive(Clone, Debug, PartialEq, Eq)]
struct Intent {
    key: u8,
    payload: u8,
    pins: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    intent: Intent,
    partition: u64,
    manifest: u64,
    before: (i128, i128),
    after: (i128, i128),
    events: Vec<(i128, i128)>,
    stock_leg: i128,
    counterpart: i128,
    time: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    stock: (i128, i128),
    version: u64,
    manifest: u64,
    time: u64,
    vouchers: Vec<u8>,
    history: Vec<(u64, usize, i128, i128)>,
    journals: Vec<(i128, i128)>,
    outbox: Vec<u8>,
    receipts: Vec<Intent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Committed,
    Replay,
    Refused,
    RolledBack,
    AckLost,
}

// Nonnegative millionths -> cents, exact half-up. The model uses small bounds.
fn cents(value: i128) -> i128 {
    (value + 5_000) / 10_000
}

impl State {
    // Faults 0..=4 are pre-COMMIT; 5 loses only the post-COMMIT ack.
    fn submit(&mut self, c: &Candidate, authorized: bool, fault: Option<usize>) -> Outcome {
        if !authorized || c.intent.pins != 1 {
            return Outcome::Refused;
        }
        if let Some(prior) = self.receipts.iter().find(|p| p.key == c.intent.key) {
            return if prior == &c.intent {
                Outcome::Replay
            } else {
                Outcome::Refused
            };
        }
        if (c.partition, c.manifest, c.before) != (self.version, self.manifest, self.stock)
            || c.time < self.time
            || c.after.0 < 0
            || c.after.1 < 0
            || (c.after.0 == 0 && c.after.1 != 0)
            || c.events.is_empty()
            || c.events.iter().map(|e| e.0).sum::<i128>() != c.after.0 - c.before.0
            || c.events.iter().map(|e| e.1).sum::<i128>() != c.after.1 - c.before.1
            || c.stock_leg != cents(c.after.1) - cents(c.before.1)
            || c.stock_leg + c.counterpart != 0
        {
            return Outcome::Refused;
        }
        let mut next = self.clone();
        next.vouchers.push(c.intent.key);
        if fault == Some(0) {
            return Outcome::RolledBack;
        }
        next.stock = c.after;
        next.version += 1;
        next.time = c.time;
        for (ordinal, &(q, v)) in c.events.iter().enumerate() {
            next.history.push((next.version, ordinal, q, v));
        }
        if fault == Some(1) {
            return Outcome::RolledBack;
        }
        next.journals.push((c.stock_leg, c.counterpart));
        if fault == Some(2) {
            return Outcome::RolledBack;
        }
        next.outbox.push(c.intent.key);
        if fault == Some(3) {
            return Outcome::RolledBack;
        }
        next.receipts.push(c.intent.clone());
        if fault == Some(4) {
            return Outcome::RolledBack;
        }
        *self = next;
        if fault == Some(5) {
            Outcome::AckLost
        } else {
            Outcome::Committed
        }
    }
}

fn witness(s: &State, key: u8, events: Vec<(i128, i128)>) -> Candidate {
    let after = (
        s.stock.0 + events.iter().map(|e| e.0).sum::<i128>(),
        s.stock.1 + events.iter().map(|e| e.1).sum::<i128>(),
    );
    let stock_leg = cents(after.1) - cents(s.stock.1);
    Candidate {
        intent: Intent {
            key,
            payload: key,
            pins: 1,
        },
        partition: s.version,
        manifest: s.manifest,
        before: s.stock,
        after,
        events,
        stock_leg,
        counterpart: -stock_leg,
        time: s.time + 1,
    }
}

fn assert_publication(s: &State) {
    let count = s.receipts.len();
    assert_eq!(s.vouchers.len(), count, "POST-ATOMIC");
    assert_eq!(s.journals.len(), count, "POST-ATOMIC");
    assert_eq!(s.outbox.len(), count, "POST-ATOMIC");
    assert_eq!(
        s.stock.0,
        s.history.iter().map(|e| e.2).sum::<i128>(),
        "POST-VALUE"
    );
    assert_eq!(
        s.stock.1,
        s.history.iter().map(|e| e.3).sum::<i128>(),
        "POST-VALUE"
    );
    assert_eq!(
        cents(s.stock.1),
        s.journals.iter().map(|j| j.0).sum::<i128>(),
        "POST-VALUE"
    );
    assert!(s.journals.iter().all(|j| j.0 + j.1 == 0), "POST-VALUE");
    assert!(
        s.history
            .windows(2)
            .all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)),
        "POST-ORDER"
    );
}

#[test]
fn cp1_feasibility_witness() {
    let mut s = State::default();
    // Lane c's FIFO oracle: 2 @ 10 + 3 @ 20, then deliver 4 at cost 60.
    let receipt = witness(
        &s,
        1,
        vec![(2_000_000, 20_000_000), (3_000_000, 60_000_000)],
    );
    assert_eq!(s.submit(&receipt, true, None), Outcome::Committed);
    assert_eq!(s.journals, vec![(8_000, -8_000)]);
    let delivery = witness(&s, 2, vec![(-4_000_000, -60_000_000)]);
    assert_eq!(s.submit(&delivery, true, None), Outcome::Committed);
    assert_eq!(s.stock, (1_000_000, 20_000_000));
    assert_eq!(s.journals[1], (-6_000, 6_000));
    assert_publication(&s);
}

#[test]
fn post_atomic_every_precommit_fault_preserves_all_business_state() {
    for fault in 0..=4 {
        let mut s = State::default();
        let c = witness(&s, 1, vec![(1, 5_000), (2, 20_000)]);
        let before = s.clone();
        assert_eq!(s.submit(&c, true, Some(fault)), Outcome::RolledBack);
        assert_eq!(s, before, "POST-ATOMIC fault {fault}");
        assert_eq!(s.submit(&c, true, None), Outcome::Committed);
        assert_publication(&s);
    }
}

#[test]
fn lost_ack_after_commit_is_a_committed_retry() {
    let mut s = State::default();
    let c = witness(&s, 1, vec![(1, 5_000)]);
    assert_eq!(s.submit(&c, true, Some(5)), Outcome::AckLost);
    let committed = s.clone();
    assert_eq!(s.submit(&c, true, None), Outcome::Replay);
    assert_eq!(s, committed, "POST-IDENTITY");
    for conflict in [
        Intent {
            payload: 2,
            ..c.intent.clone()
        },
        Intent {
            pins: 2,
            ..c.intent.clone()
        },
    ] {
        assert_eq!(
            s.submit(
                &Candidate {
                    intent: conflict,
                    ..c.clone()
                },
                true,
                None
            ),
            Outcome::Refused
        );
        assert_eq!(s, committed);
    }
}

#[test]
fn post_value_rejects_balanced_misvaluation_and_event_mismatch() {
    let mut s = State::default();
    let c = witness(&s, 1, vec![(1, 5_000)]);
    for bad in [
        Candidate {
            stock_leg: 2,
            counterpart: -2,
            ..c.clone()
        },
        Candidate {
            events: vec![(2, 5_000)],
            ..c.clone()
        },
        Candidate {
            events: vec![(1, 4_999)],
            ..c.clone()
        },
        Candidate {
            counterpart: 0,
            ..c
        },
    ] {
        assert_eq!(s.submit(&bad, true, None), Outcome::Refused);
        assert_eq!(s, State::default(), "POST-VALUE refusal is atomic");
    }
    // Aggregate before rounding: two half-cents produce one cent, not two.
    let c = witness(&s, 1, vec![(1, 5_000), (1, 5_000)]);
    assert_eq!(s.submit(&c, true, None), Outcome::Committed);
    assert_eq!(s.journals, vec![(1, -1)]);
    assert_publication(&s);
}

#[test]
fn post_order_empty_partition_and_manifest_races_are_fenced() {
    for reverse in [false, true] {
        let mut s = State::default();
        let a = witness(&s, 1, vec![(1, 5_000)]);
        let b = witness(&s, 2, vec![(2, 15_000)]);
        let (first, second) = if reverse { (b, a) } else { (a, b) };
        assert_eq!(s.submit(&first, true, None), Outcome::Committed);
        let committed = s.clone();
        assert_eq!(s.submit(&second, true, None), Outcome::Refused);
        assert_eq!(s, committed);
        let stale_manifest = witness(&s, 3, vec![(1, 0)]);
        s.manifest += 1;
        let edited = s.clone();
        assert_eq!(s.submit(&stale_manifest, true, None), Outcome::Refused);
        assert_eq!(s, edited);
        assert_publication(&s);
    }
}

#[test]
fn post_authority_checks_current_authority_even_for_replay() {
    let mut s = State::default();
    let c = witness(&s, 1, vec![(1, 0)]);
    assert_eq!(s.submit(&c, false, None), Outcome::Refused);
    assert_eq!(s, State::default());
    let unpinned = Candidate {
        intent: Intent {
            pins: 2,
            ..c.intent.clone()
        },
        ..c.clone()
    };
    assert_eq!(s.submit(&unpinned, true, None), Outcome::Refused);
    assert_eq!(s, State::default());
    assert_eq!(s.submit(&c, true, None), Outcome::Committed);
    let committed = s.clone();
    assert_eq!(s.submit(&c, false, None), Outcome::Refused);
    assert_eq!(s, committed, "POST-AUTHORITY");
}
