//! MINE-CONS-1: bounded runtime questions over source-pinned observed flows.
//! Exact certificates describe only this supplied window, never business truth.
use norn_checker::obligations::conservation::{
    Coordinate, Delta, Failure, Rational, Writer, check, discovery::discover,
};
use std::collections::BTreeSet;

/// An authorized, already selected observation, or a proposed candidate transition.
/// The adapter owns faithful exact lowering and scope/visibility enforcement.
#[derive(Debug, Clone)]
pub struct ObservedFlow {
    /// Source record/version identity within the pinned window.
    pub record: String,
    /// One exact delta per coordinate; no symbolic parameters.
    pub delta: Vec<Rational>,
}

/// This result has no admission or proof-evidence conversion.
#[derive(Debug, PartialEq, Eq)]
pub enum SuggestionStatus {
    /// Expert review and ordinary admission are required for any definition edit.
    SuggestedNotAdmitted,
}

/// A candidate plus a checked finite-observation certificate and counterexamples.
#[derive(Debug)]
pub struct ConservationSuggestion {
    /// Always suggested, including when every supplied observation balances.
    pub status: SuggestionStatus,
    /// Caller-supplied immutable question snapshot/candidate identity.
    pub window: String,
    /// Exact candidate functional, including scope and units, independent of a law.
    pub coordinates: Vec<Coordinate>,
    /// Number of initial records used for discovery (also checked as evidence).
    pub training_records: usize,
    /// All balanced observed record identities, in input order.
    pub supporting_records: Vec<String>,
    /// Every unbalanced observation and its exact partition residual.
    /// A witness is the first nonzero partition in that observation.
    pub counterexamples: Vec<(String, Failure)>,
}

/// Discover one nonzero anchored conservation candidate from a training prefix,
/// then independently check it on every supplied observation. No state is mutated.
/// The full input is checked: a holdout failure is reported, never discarded.
///
/// The caller must pin and retain the supplied window and its faithful lowering
/// (as for other runtime question results). This API does not fetch or persist data.
/// Bounded to 256 records, 64 coordinates, 4096 identity bytes, and the discovery
/// kernel's 65536-coefficient / 1000000-elimination-work ceilings. These bounds
/// cap time and allocation independently of runtime admission budgets.
///
/// # Errors
/// Empty/vacuous input, duplicate identities, malformed exact deltas, unsupported
/// arithmetic or work ceilings, or no feasible nonzero anchored candidate. An
/// error is no proposal and conveys no verification or admission authority.
pub fn suggest_conservation(
    window: &str,
    coordinates: &[Coordinate],
    observations: &[ObservedFlow],
    training_records: usize,
    anchors: &[Option<Rational>],
) -> Result<ConservationSuggestion, Failure> {
    if coordinates.len() > 64 || observations.len() > 256
        || coordinates.iter().any(|c| c.scope.len() > 64)
    {
        return Err(Failure::Malformed);
    }
    let identity_bytes = coordinates.iter().fold(window.len(), |size, c| {
        c.scope.iter().fold(
            size.saturating_add(c.id.len()).saturating_add(c.unit.code.len()),
            |size, (k, v)| size.saturating_add(k.len()).saturating_add(v.len()),
        )
    });
    let identity_bytes = observations.iter().fold(identity_bytes, |size, o| {
        size.saturating_add(o.record.len())
    });
    if window.is_empty()
        || coordinates.is_empty()
        || coordinates.len() > 64
        || observations.is_empty()
        || observations.len() > 256
        || training_records == 0
        || training_records > observations.len()
        || identity_bytes > 4096
    {
        return Err(Failure::Malformed);
    }
    let mut ids = BTreeSet::new();
    let zero = Rational::new(0, 1).ok_or(Failure::Malformed)?;
    let mut informative = false;
    for observation in observations {
        if observation.record.is_empty()
            || !ids.insert(&observation.record)
            || observation.delta.len() != coordinates.len()
        {
            return Err(Failure::Malformed);
        }
    }
    let writers: Vec<_> = observations
        .iter()
        .map(|o| Writer {
            id: o.record.clone(),
            delta: Delta::Affine(o.delta.iter().map(|v| vec![*v]).collect()),
        })
        .collect();
    let training = writers.get(..training_records).ok_or(Failure::Malformed)?;
    for observation in observations.iter().take(training_records) {
        informative |= observation.delta.iter().any(|q| *q != zero);
    }
    if !informative {
        return Err(Failure::Unknown("no nonzero training transitions".into()));
    }
    let weights = discover(coordinates, training, anchors)?;
    let candidate: Vec<_> = coordinates
        .iter()
        .zip(&weights)
        .map(|(c, weight)| Coordinate { weight: *weight, ..c.clone() })
        .collect();
    let mut result = ConservationSuggestion {
        status: SuggestionStatus::SuggestedNotAdmitted,
        window: window.into(),
        coordinates: candidate,
        training_records,
        supporting_records: Vec::new(),
        counterexamples: Vec::new(),
    };
    for writer in &writers {
        match check(&result.coordinates, std::slice::from_ref(writer), &weights) {
            Ok(()) => result.supporting_records.push(writer.id.clone()),
            Err(witness @ Failure::Unbalanced { .. }) => {
                result.counterexamples.push((writer.id.clone(), witness));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "exact mining fixtures")]
mod tests {
    use super::*;
    use norn_ir::{UnitRef, UnitSystem};

    fn q(n: i64, d: i64) -> Rational { Rational::new(n, d).unwrap() }
    fn fixture(count: usize) -> (Vec<Coordinate>, Vec<ObservedFlow>) {
        let coordinates = ["input", "output"].into_iter().map(|id| Coordinate {
            id: id.into(), scope: [("book".into(), "primary".into())].into(),
            unit: UnitRef { system: UnitSystem::Ucum, code: "g".into() },
            weight: q(1, 1),
        }).collect();
        let observations = (0..count).map(|i| ObservedFlow {
            record: format!("flow:{i}@1"), delta: vec![q(-2, 1), q(1, 1)],
        }).collect();
        (coordinates, observations)
    }
    #[test]
    fn true_candidate_is_only_suggested_with_full_evidence() {
        let (c, o) = fixture(4);
        let result = suggest_conservation("revision:42", &c, &o, 2, &[None, Some(q(1, 1))]).unwrap();
        assert_eq!(result.status, SuggestionStatus::SuggestedNotAdmitted);
        assert_eq!(result.window, "revision:42");
        assert_eq!(result.coordinates[0].weight, q(1, 2));
        assert_eq!(result.supporting_records, o.iter().map(|v| v.record.clone()).collect::<Vec<_>>());
        assert!(result.counterexamples.is_empty());
        assert_eq!(c[0].weight, q(1, 1)); // Discovery cannot edit the input functional.
    }
    #[test]
    fn near_miss_retains_exact_counterexample() {
        let (c, mut o) = fixture(4);
        o[3].delta[1] = q(3, 2);
        let result = suggest_conservation("revision:42", &c, &o, 2, &[None, Some(q(1, 1))]).unwrap();
        assert_eq!(result.supporting_records.len(), 3);
        assert_eq!(result.counterexamples.len(), 1);
        assert_eq!(result.counterexamples[0].0, o[3].record);
        assert!(matches!(&result.counterexamples[0].1, Failure::Unbalanced { residual, .. } if *residual == q(1, 2)));
        assert_eq!(result.status, SuggestionStatus::SuggestedNotAdmitted);
    }
    #[test]
    fn no_signal_and_invalid_or_excessive_windows_produce_no_proposal() {
        let (c, mut o) = fixture(2);
        o[1].delta[1] = q(2, 1);
        assert!(suggest_conservation("r", &c, &o, 2, &[None, Some(q(1, 1))]).is_err());
        o[0].delta.fill(q(0, 1));
        assert!(suggest_conservation("r", &c, &o, 1, &[None, Some(q(1, 1))]).is_err());
        o[1].record = o[0].record.clone();
        assert!(suggest_conservation("r", &c, &o, 1, &[None, Some(q(1, 1))]).is_err());
        let (c, o) = fixture(257);
        assert!(suggest_conservation("r", &c, &o, 1, &[None, Some(q(1, 1))]).is_err());
        let (mut c, o) = fixture(2);
        c[1].unit.code = "mg".into();
        assert!(suggest_conservation("r", &c, &o, 1, &[None, Some(q(1, 1))]).is_err());
    }
    #[test]
    fn fixture_cost() {
        let (c, o) = fixture(256);
        let start = std::time::Instant::now();
        let result = suggest_conservation("r", &c, &o, 128, &[None, Some(q(1, 1))]).unwrap();
        assert_eq!(result.supporting_records.len(), 256);
        eprintln!("MINE-CONS-1: 256 records x 2 coordinates; 128 training; {:?}; <=512 input coefficients", start.elapsed());
    }
}
