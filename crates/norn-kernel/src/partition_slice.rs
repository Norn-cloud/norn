//! Complete declared collection reads carried into decision preparation.

use norn_ir::DecisionOutput;

use crate::{
    CellHost, DecisionRefusal, DecisionTurn, EnvelopeLimits, FactKey, FactSlice,
    LinkReadExpectation, PreparedDecision, SliceLimits, SliceMeter, TypeCatalog,
    prepare_decision_with_limits,
};

/// A captured fact slice and its complete partition-membership expectations.
/// Fields stay private so collection readers cannot accidentally discard fences
/// while preparing a decision. Loading confers no authorization.
#[derive(Debug, Clone)]
pub struct PartitionSlice {
    facts: FactSlice,
    partitions: Vec<LinkReadExpectation>,
}

impl PartitionSlice {
    /// Fact versions and values supplied to the admitted evaluator.
    #[must_use]
    pub const fn facts(&self) -> &FactSlice {
        &self.facts
    }

    /// FENCE-COMPLETE: exact captured populations, including empty partitions.
    /// Targets identify members; source and extra point facts are not members.
    /// Evaluate these targets from `facts()`, then use this slice's `prepare`.
    #[must_use]
    pub fn partition_reads(&self) -> &[LinkReadExpectation] {
        &self.partitions
    }

    /// Resolve ordinary `RuleIR` outputs, retaining every captured membership
    /// expectation in the same commit as its facts and effects.
    ///
    /// # Errors
    /// The existing decision admission refusal. The caller must still use its
    /// admitted authority path to commit this proposal.
    pub fn prepare<X>(
        &self,
        catalog: &TypeCatalog<'_>,
        turn: &DecisionTurn,
        output: &DecisionOutput,
        limits: &EnvelopeLimits,
    ) -> Result<PreparedDecision, DecisionRefusal<X>> {
        let mut prepared =
            prepare_decision_with_limits(catalog, &self.facts, turn, output, limits)?;
        prepared.link_reads.clone_from(&self.partitions);
        Ok(prepared)
    }
}

/// FENCE-BOUNDS: load point reads and complete partitions through bounded host reads.
/// Empty partitions are fenced too. Host list reads must be complete; `None`
/// (unsupported, incomplete or over bound) refuses before evaluation.
///
/// # Errors
/// A host failure, exceeded slice bound, malformed membership snapshot or
/// missing member. Nothing is committed. Authorization remains the caller's
/// existing admitted read/commit authority, as with `load_slice`.
pub fn load_partition_slice<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    points: impl IntoIterator<Item = FactKey>,
    partitions: &[(FactKey, String)],
    limits: SliceLimits,
) -> Result<PartitionSlice, DecisionRefusal<H::Error>> {
    let mut slice = PartitionSlice {
        facts: FactSlice::new(),
        partitions: Vec::new(),
    };
    let mut meter = SliceMeter::new(limits);
    for key in points {
        capture_fact(host, &key, &mut slice.facts, &mut meter, limits)?;
    }
    let model = crate::CommitModel::new(catalog);
    for (source, link) in partitions {
        capture_fact(host, source, &mut slice.facts, &mut meter, limits)?;
        let max_targets = usize::try_from(limits.max_records.saturating_sub(meter.records()))
            .unwrap_or(usize::MAX);
        let targets = host
            .read_link_targets(
                source,
                link,
                max_targets,
                limits.max_canonical_bytes.saturating_sub(meter.bytes()),
            )
            .map_err(DecisionRefusal::Host)?
            .ok_or_else(|| unavailable("complete bounded partition snapshot unavailable"))?;
        if targets.len() > max_targets
            || targets.iter().any(|target| {
                target.key.source != *source
                    || target.key.link != *link
                    || target.version == crate::FactVersion::ABSENT
            })
            || targets.windows(2).any(|pair| {
                matches!(pair, [a, b]
                if model.canonical_link(&a.key) >= model.canonical_link(&b.key))
            })
        {
            return Err(unavailable("non-canonical partition membership snapshot"));
        }
        for target in &targets {
            capture_fact(
                host,
                &target.key.target,
                &mut slice.facts,
                &mut meter,
                limits,
            )?;
            if !slice
                .facts
                .get(&target.key.target)
                .is_some_and(crate::FactState::exists)
            {
                return Err(unavailable("partition member is absent"));
            }
        }
        let read = LinkReadExpectation {
            source: source.clone(),
            link: link.clone(),
            targets,
        };
        if let Some(previous) = slice
            .partitions
            .iter()
            .find(|previous| previous.source == *source && previous.link == *link)
        {
            if previous != &read {
                return Err(unavailable("partition changed while loading the slice"));
            }
        } else {
            let bytes = norn_ir::runtime::encode(&read)
                .map_err(|reason| unavailable(&reason))?
                .canonical_json()
                .map_err(|error| unavailable(&error.to_string()))?;
            meter
                .admit_record(u64::try_from(bytes.len()).unwrap_or(u64::MAX), 1)
                .map_err(|error| unavailable(&error.to_string()))?;
            slice.partitions.push(read);
        }
    }
    Ok(slice)
}

fn capture_fact<H: CellHost + ?Sized>(
    host: &H,
    key: &FactKey,
    facts: &mut FactSlice,
    meter: &mut SliceMeter,
    limits: SliceLimits,
) -> Result<(), DecisionRefusal<H::Error>> {
    if facts.contains_key(key) {
        return Ok(());
    }
    let state = host
        .load_bounded(
            key,
            limits.max_canonical_bytes.saturating_sub(meter.bytes()),
        )
        .map_err(DecisionRefusal::Host)?
        .ok_or_else(|| unavailable("complete bounded fact snapshot unavailable"))?
        .state;
    let bytes = norn_ir::runtime::encode(&(key, &state))
        .map_err(|reason| unavailable(&reason))?
        .canonical_json()
        .map_err(|error| unavailable(&error.to_string()))?;
    meter
        .admit_record(u64::try_from(bytes.len()).unwrap_or(u64::MAX), 0)
        .map_err(|error| unavailable(&error.to_string()))?;
    facts.insert(key.clone(), state);
    Ok(())
}

fn unavailable<X>(reason: &str) -> DecisionRefusal<X> {
    DecisionRefusal::InvalidOutput {
        reason: reason.to_owned(),
    }
}
