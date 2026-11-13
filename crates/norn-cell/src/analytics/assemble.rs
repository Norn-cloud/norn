//! QUERY-ENGINE-1: the cell's half of an engine answer. The cell prepares the
//! lowered question at its verified head (scope, plan contract, exclusions
//! and pins, as the exact engine does) and turns the engine's buckets into
//! the same envelope, recomputing every derived number with the exact
//! arithmetic. Within the plan's bound the envelope equals the exact
//! engine's, normalizing only its engine label.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::contribution::period_start;
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE, arithmetic as math};
use norn_ir::{Decimal, RuntimeValue};

use norn_ir::metric::TimeGrain;

use super::{
    BucketTotals, EvaluateRequest, EvaluateResponse, Grain, GroupKey, MemberRow, MembershipDigest,
    Source, TenantId, lower::lower,
};
use crate::compute::{Engine, Member, QueryContext, QueryResult, QueryRow, decimal, refused};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::read::LedgerHead;
use crate::system::LEDGER;
use crate::{CellError, CellStore};

/// The bucket value a group key stands for, as the shared projection gives it.
///
/// # Errors
/// A malformed number or a week outside the calendar.
pub fn bucket_value(group: &GroupKey) -> Result<RuntimeValue, CellError> {
    Ok(match group {
        GroupKey::Null => RuntimeValue::Null,
        GroupKey::Bool(flag) => RuntimeValue::Bool(*flag),
        GroupKey::Number(text) => RuntimeValue::Number(Decimal::parse(text)?),
        GroupKey::Text(text) => RuntimeValue::String(text.clone()),
        GroupKey::Period { grain, day } => {
            let grain = match grain {
                Grain::Day => TimeGrain::Day,
                Grain::Week => TimeGrain::Week,
                Grain::Month => TimeGrain::Month,
                Grain::Quarter => TimeGrain::Quarter,
                Grain::Year => TimeGrain::Year,
            };
            let seconds = day
                .checked_mul(86_400)
                .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
            RuntimeValue::from(period_start(seconds, grain).map_err(refused)?)
        }
        GroupKey::Tuple(members) => {
            RuntimeValue::Array(members.iter().map(bucket_value).collect::<Result<_, _>>()?)
        }
    })
}

fn key_of(value: &RuntimeValue) -> Result<String, CellError> {
    String::from_utf8(value.canonical_json()?).map_err(|e| CellError::Encoding(e.to_string()))
}

/// A question prepared at the cell's verified head for the engine.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// What to send the engine.
    pub request: EvaluateRequest,
    plan: QuestionPlan,
    context: QueryContext,
    exclusions: BTreeMap<String, String>,
    revision: LedgerHead,
    query_pin: String,
    definition_pin: String,
    /// At a knowledge cutoff: the cutoff and the receipt it is pinned to.
    known_at: Option<(crate::KnowledgeCutoff, Option<String>)>,
}

impl<E: SqlEngine> CellStore<E> {
    /// D102: prepare `plan` for the tenant's engine at the verified head. Every
    /// row of the grain is visible to the caller (the host decided the
    /// population gate for the whole type); open findings exclude their
    /// subjects exactly as the exact engine does.
    ///
    /// # Errors
    /// Scope or plan mismatch, an unsupported plan, or a store error.
    pub fn analytics_prepare(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        tenant: &TenantId,
    ) -> Result<Prepared, CellError> {
        self.prepare_at(plan, context, tenant, None)
    }

    /// ASOF-KNOWLEDGE-1 on the engine: prepare `plan` as known at `cutoff`,
    /// read at the cutoff's own clock (`context.now_ms`). The mirror answers
    /// at the cutoff's frontier; findings exclude as they stood then; a
    /// version committed under another model refuses, as the exact engine
    /// refuses it.
    ///
    /// # Errors
    /// As [`Self::analytics_prepare`], a clock that is not the cutoff's, an
    /// unproven frontier, or a loaded dataset (it has no history).
    pub fn analytics_prepare_known_at(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        tenant: &TenantId,
        cutoff: crate::KnowledgeCutoff,
    ) -> Result<Prepared, CellError> {
        self.prepare_at(plan, context, tenant, Some(cutoff))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one preparation, current or at a cutoff"
    )]
    fn prepare_at(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        tenant: &TenantId,
        cutoff: Option<crate::KnowledgeCutoff>,
    ) -> Result<Prepared, CellError> {
        self.check_question_scope(&context.scope)?;
        if cutoff.is_none()
            && matches!(
                plan.question().freshness,
                norn_ir::metric::FreshnessPolicy::AsOf { .. }
            )
        {
            return Err(refused(UNSUPPORTED_ENGINE));
        }
        crate::compute::check_question_cutoff(plan, cutoff)?;
        let bound = self.check_question_plan(plan)?;
        let mut query = lower(plan)?;
        let (revision, receipt) = match cutoff {
            Some(cutoff) => self.knowledge_frame(cutoff, context.now_ms)?,
            None => (self.verified_head()?, None),
        };
        let grain = plan.grain().as_str();
        // METRIC-GRAIN-LINK-1: the link's mirrored table and its target's root.
        if let Some(link) = &mut query.time_link {
            let unknown = |_error| refused(UNSUPPORTED_ENGINE);
            let (_, table) = self.model().link(grain, &link.link).map_err(unknown)?;
            let root = self
                .model()
                .chain(&link.target)
                .map_err(unknown)?
                .first()
                .cloned()
                .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
            link.link.clone_from(&table.table);
            link.target = root;
        }
        // BULK-LINEAGE-1: a loaded dataset is read at its current load; its
        // rows are not cell facts, so no finding excludes them.
        let (source, exclusions) = if let Some(dataset) = self.analytics_dataset(grain)? {
            if cutoff.is_some() {
                return Err(refused(UNSUPPORTED_ENGINE));
            }
            (
                Source::Dataset {
                    table: grain.to_owned(),
                    load: dataset.load,
                },
                BTreeMap::new(),
            )
        } else {
            let candidates = self.analytics_exclusion_candidates(&context.world, grain)?;
            let exclusions = match cutoff {
                Some(cutoff) => {
                    self.exclusions_known_at(&context.world, grain, candidates, cutoff)?
                }
                None => self.analytics_exclusions_of(&context.world, grain, candidates)?,
            };
            (
                Source::Mirror {
                    world: context.world.clone(),
                    table: grain.to_owned(),
                    frontier: cutoff.map_or(revision.commits, |cutoff| cutoff.frontier),
                    scope: context.scope.clone(),
                    excluded: exclusions.keys().cloned().collect(),
                    model: cutoff.map(|_| self.model().model_hash().to_owned()),
                },
                exclusions,
            )
        };
        let (query_pin, definition_pin) = self.question_pins(plan, cutoff)?;
        let request = EvaluateRequest {
            tenant: tenant.clone(),
            source,
            query,
            inline_members: u64::try_from(bound).unwrap_or(u64::MAX),
        };
        Ok(Prepared {
            request,
            plan: plan.clone(),
            context,
            exclusions,
            revision,
            query_pin,
            definition_pin,
            known_at: cutoff.map(|cutoff| (cutoff, receipt)),
        })
    }

    /// What a read of `object_type` reads, and the snapshot it is exact at:
    /// a loaded dataset's current load (`l<load>`), or the governed mirror of
    /// `scope` at the verified head without quarantined subjects (`c<commit>`).
    ///
    /// # Errors
    /// A store error.
    pub fn analytics_source(
        &self,
        world: &str,
        scope: &str,
        object_type: &str,
    ) -> Result<(Source, String), CellError> {
        if let Some(dataset) = self.analytics_dataset(object_type)? {
            let snapshot = format!("l{}", dataset.load);
            return Ok((
                Source::Dataset {
                    table: object_type.to_owned(),
                    load: dataset.load,
                },
                snapshot,
            ));
        }
        let head = self.verified_head()?.commits;
        let exclusions = self.analytics_exclusions(world, object_type)?;
        Ok((
            Source::Mirror {
                world: world.to_owned(),
                table: object_type.to_owned(),
                frontier: head,
                scope: scope.to_owned(),
                excluded: exclusions.into_keys().collect(),
                model: None,
            },
            format!("c{head}"),
        ))
    }

    /// The analytical table of the admitted type `object_type`.
    ///
    /// # Errors
    /// An unknown type.
    pub fn analytics_table(&self, object_type: &str) -> Result<super::TableSchema, CellError> {
        let contract = self.model().query_contract(object_type)?;
        super::lower::table_schema(object_type, &contract.properties)
    }

    /// DATA-QUARANTINE: the grain's current subjects with an open quarantining
    /// finding, each with its first finding, by the exact engine's own rule.
    fn analytics_exclusions(
        &self,
        world: &str,
        grain: &str,
    ) -> Result<BTreeMap<String, String>, CellError> {
        let candidates = self.analytics_exclusion_candidates(world, grain)?;
        self.analytics_exclusions_of(world, grain, candidates)
    }

    /// The grain's subjects any finding has named: those a quarantine can
    /// exclude, now or at a cutoff.
    fn analytics_exclusion_candidates(
        &self,
        world: &str,
        grain: &str,
    ) -> Result<BTreeSet<String>, CellError> {
        let model = self.model();
        let reader = self.read();
        let mut candidates = BTreeSet::new();
        let rows = self.conn.query(
            &format!("SELECT DISTINCT json_extract(event, '$.fact.subjects[0].objectType'), json_extract(event, '$.fact.subjects[0].key') FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding'"),
            params![world],
        )?;
        for row in rows {
            let row = SqlRow(row);
            let (kind, key): (Option<String>, Option<String>) = (row.get(0)?, row.get(1)?);
            if let (Some(kind), Some(key)) = (kind, key)
                && model.is_a(&kind, grain)
            {
                candidates.insert(key);
            }
        }
        if world == crate::MAIN_WORLD {
            candidates.extend(reader.source_finding_keys(grain)?);
        }
        Ok(candidates)
    }

    /// The current exclusions among `candidates`.
    fn analytics_exclusions_of(
        &self,
        world: &str,
        grain: &str,
        candidates: BTreeSet<String>,
    ) -> Result<BTreeMap<String, String>, CellError> {
        let reader = self.read();
        let mut exclusions = BTreeMap::new();
        for key in candidates {
            if let Some(record) = reader.object(world, grain, &key)?
                && let Some(finding) = reader.question_findings(world, &record, None)?.first()
            {
                exclusions.insert(key, finding.id.clone());
            }
        }
        Ok(exclusions)
    }

    /// QUERY-ENGINE-1: verify the engine's answer to `prepared` into the
    /// exact envelope. The answer must be for the prepared frontier; totals,
    /// ratios and lineage are recomputed here with the exact arithmetic.
    ///
    /// # Errors
    /// A moved head (`core.compute.stale_input`), a malformed or inconsistent
    /// response, or a sum outside the bounded profile.
    pub fn analytics_answer(
        &self,
        prepared: Prepared,
        response: EvaluateResponse,
    ) -> Result<QueryResult, CellError> {
        // A cutoff's history is immutable; a current answer is exact only at
        // the head it was prepared at.
        if prepared.known_at.is_none() && self.verified_head()? != prepared.revision {
            return Err(refused("core.compute.stale_input"));
        }
        let Prepared {
            request,
            plan,
            context,
            mut exclusions,
            revision,
            query_pin,
            definition_pin,
            known_at: _,
        } = prepared;
        let present: BTreeSet<&String> = response.excluded.iter().collect();
        exclusions.retain(|id, _| present.contains(id));
        let inline = response.population <= request.inline_members;
        let arity = plan.sums().len();
        let mut rows = BTreeMap::new();
        for bucket in response.buckets {
            let value = bucket_value(&bucket.group)?;
            let key = key_of(&value)?;
            let row = assemble_row(&bucket, value, arity, inline)?;
            if rows.insert(key, row).is_some() {
                return Err(CellError::Engine("two buckets share a key".to_owned()));
            }
        }
        let mut result = crate::compute::materialize(
            &plan,
            (context, exclusions),
            revision,
            query_pin,
            definition_pin,
            &BTreeMap::new(),
        )?;
        result.rows = rows;
        result.unlinked = response.unlinked;
        result.engine = Engine::Duckdb;
        let evidence = crate::compute::FindingEvidence {
            context: &result.context,
            exclusions: &result.exclusions,
        };
        result.lineage_ref = crate::codec::hash_json(&(
            &result.query_pin,
            &result.revision,
            evidence,
            &result.rows,
        ))?;
        Ok(result)
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// ASOF-KNOWLEDGE-1 on the engine: verify the engine's answer to a
    /// prepared cutoff into the exact engine's historical envelope.
    ///
    /// # Errors
    /// As [`Self::analytics_answer`], or a preparation without a cutoff.
    pub fn analytics_answer_known_at(
        &self,
        prepared: Prepared,
        response: EvaluateResponse,
    ) -> Result<crate::compute::HistoricalQuestionResult, CellError> {
        let (cutoff, receipt) = prepared
            .known_at
            .clone()
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
        let result = self.analytics_answer(prepared, response)?;
        Ok(crate::compute::HistoricalQuestionResult {
            world: norn_kernel::World::Historical {
                commit: norn_kernel::CommitId(
                    receipt.unwrap_or_else(|| crate::system::GENESIS_HASH.into()),
                ),
            },
            knowledge_time_ms: cutoff.recorded_at.as_millis(),
            result,
        })
    }
}

fn coefficient_sum(text: &str) -> Result<i128, CellError> {
    let sum: i128 = text
        .parse()
        .map_err(|_e| CellError::Engine(format!("not an exact sum: {text}")))?;
    Ok(sum)
}

/// One bucket's row: the exact totals (refusing a sum whose partial sums
/// could leave the bounded profile), the ratio, and either the members or
/// their count and digest.
fn assemble_row(
    bucket: &BucketTotals,
    value: RuntimeValue,
    arity: usize,
    inline: bool,
) -> Result<QueryRow, CellError> {
    if bucket.totals.len() != arity
        || bucket.magnitude.len() != arity
        || bucket.nulls.len() != arity
    {
        return Err(CellError::Engine("operand arity".to_owned()));
    }
    let mut totals = Vec::with_capacity(arity);
    for (total, magnitude) in bucket.totals.iter().zip(&bucket.magnitude) {
        let total = coefficient_sum(total)?;
        // Every partial sum is bounded by the magnitude; past the profile's
        // bound the order-dependent exact refusal cannot be ruled out.
        if coefficient_sum(magnitude)? > math::MAX_SUM {
            return Err(refused("core.compute.bound_exceeded"));
        }
        totals.push(total);
    }
    let value_number = match totals.as_slice() {
        [sum] => Some(decimal(*sum)?),
        [_, 0] => None,
        [n, d] => Some(decimal(
            math::ratio(*n, *d).ok_or_else(|| refused("core.compute.bound_exceeded"))?,
        )?),
        _ => return Err(refused(UNSUPPORTED_ENGINE)),
    };
    let null_counts = if bucket.nulls.iter().any(|n| *n > 0) {
        bucket.nulls.clone()
    } else {
        Vec::new()
    };
    let (members, member_count, membership_digest) = if inline {
        let listed = bucket
            .members
            .as_ref()
            .ok_or_else(|| CellError::Engine("inline members missing".to_owned()))?;
        check_members(listed, bucket)?;
        let members = listed
            .iter()
            .map(|m| {
                let amounts = m
                    .amounts
                    .iter()
                    .map(|n| n.map(|v| decimal(i128::from(v))).transpose())
                    .collect::<Result<_, _>>()?;
                Ok((m.id.clone(), Member::visible(m.version, amounts)))
            })
            .collect::<Result<BTreeMap<_, _>, CellError>>()?;
        (members, None, None)
    } else {
        (
            BTreeMap::new(),
            Some(bucket.count),
            Some(bucket.digest.clone()),
        )
    };
    Ok(QueryRow {
        bucket: value,
        totals: totals.into_iter().map(decimal).collect::<Result<_, _>>()?,
        value: value_number,
        members,
        suppressed: false,
        null_counts,
        member_count,
        membership_digest,
    })
}

/// Inline members must be the bucket's members: ordered, unique, matching
/// its count, digest and totals.
fn check_members(members: &[MemberRow], bucket: &BucketTotals) -> Result<(), CellError> {
    let ordered = members
        .windows(2)
        .all(|w| matches!(w, [a, b] if a.id < b.id));
    let count = u64::try_from(members.len()).unwrap_or(u64::MAX);
    let mut sums = vec![0i128; bucket.totals.len()];
    for member in members {
        for (sum, amount) in sums.iter_mut().zip(&member.amounts) {
            *sum = sum
                .checked_add(i128::from(amount.unwrap_or(0)))
                .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
        }
    }
    let totals = bucket
        .totals
        .iter()
        .map(|t| coefficient_sum(t))
        .collect::<Result<Vec<_>, _>>()?;
    if !ordered
        || count != bucket.count
        || MembershipDigest::of(members) != bucket.digest
        || sums != totals
    {
        return Err(CellError::Engine(
            "inline members disagree with their bucket".to_owned(),
        ));
    }
    Ok(())
}
