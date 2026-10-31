//! Store-owned immutable value proofs and explicit export replay into a fresh store.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use crate::codec::{canonical_json, hash_json};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::export::ExportRecord;
use crate::export_journal::ConsumedInput;
use crate::read::{LedgerEntry, ObjectRecord, StoreReader, to_i64, to_u64};
use crate::system::{FACT_VALUES, GENESIS_HASH, INBOX, RECEIPT, RUN, STORE_FORMAT};
use crate::{CellStore, CommitOutcome, InstalledDefinitionInput, Model, Receipt};

/// The commitment preimage; absence is a tombstone, not an empty live object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FactSnapshot {
    pub world: String,
    pub root_type: String,
    pub object_type: String,
    pub lineage: Vec<String>,
    pub model_hash: String,
    pub key: String,
    pub scope: String,
    pub version: u64,
    pub values: Option<BTreeMap<String, Value>>,
}

fn historical_record(snapshot: FactSnapshot) -> Option<ObjectRecord> {
    snapshot.values.map(|values| ObjectRecord {
        object_type: snapshot.object_type,
        id: snapshot.key,
        scope: snapshot.scope,
        version: snapshot.version,
        values,
    })
}

// A projection may omit unproven fields, but each retained member must stay
// at its authenticated path; arrays cannot invent, duplicate or reorder items.
fn history_projection_is_subset(proposed: &Value, authenticated: &Value) -> bool {
    match (proposed, authenticated) {
        (Value::Object(proposed), Value::Object(authenticated)) => {
            proposed.iter().all(|(key, value)| {
                authenticated
                    .get(key)
                    .is_some_and(|actual| history_projection_is_subset(value, actual))
            })
        }
        (Value::Array(proposed), Value::Array(authenticated)) => {
            proposed.len() == authenticated.len()
                && proposed
                    .iter()
                    .zip(authenticated)
                    .all(|(value, actual)| history_projection_is_subset(value, actual))
        }
        _ => proposed == authenticated,
    }
}

fn materialize_historical_values(
    model: &Model,
    object_type: &norn_ir::ObjectTypeName,
    stored: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, CellError> {
    let catalog = norn_kernel::TypeCatalog::new(model.object_types())
        .with_interfaces(model.interfaces())
        .with_property_types(model.property_types());
    let retained = stored
        .iter()
        .filter(|(property, _)| model.column(object_type.as_str(), property).is_ok())
        .map(|(property, value)| (property.clone(), value.clone()))
        .collect();
    catalog
        .effective_object_values(object_type, &retained)
        .map_err(|_invalid| unproven_event())
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Resolve the last committed incarnation at an explicit ledger cursor.
    /// The indexed lookup includes deletions; it cannot resurrect an older live
    /// value through a tombstone. The selected immutable preimage is authenticated
    /// and charged before decoding. Authorization belongs to the exporting host.
    ///
    /// # Errors
    /// Invalid cursor, corrupt/missing proof, or engine failure.
    pub fn object_at_commit_with_budget(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        commit_seq: u64,
        budget: &mut crate::reactive::DecodeBudget,
    ) -> Result<crate::reactive::BudgetedRead, CellError> {
        let root = self.root_type(object_type).unwrap_or(object_type);
        let version: Option<i64> = self.conn.query_optional(
            &format!("SELECT \"version\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"root_type\" = ?2 AND \"id\" = ?3 AND \"commit_seq\" <= ?4 ORDER BY \"commit_seq\" DESC, \"version\" DESC LIMIT 1"),
            params![world, root, key, to_i64(commit_seq, "historical cursor")?],
            |row| row.get(0),
        )?;
        // A corrupt index can hide the true latest incarnation beyond the cursor.
        // Authenticate the next greater version independently of that filter.
        let next: Option<i64> = self.conn.query_optional(
            &format!("SELECT \"version\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"root_type\" = ?2 AND \"id\" = ?3 AND \"version\" > ?4 ORDER BY \"version\" ASC LIMIT 1"),
            params![world, root, key, version.unwrap_or(0)],
            |row| row.get(0),
        )?;
        if let Some(next) = next {
            let Some(receipt) = self.receipt_for_fact_version_with_budget(
                world, root, key, to_u64(next, "next historical version")?, budget,
            )? else {
                return Ok(crate::reactive::BudgetedRead::Exhausted);
            };
            if receipt.body.commit_seq <= commit_seq {
                return Err(unproven_snapshot());
            }
        }
        let Some(version) = version else {
            return Ok(crate::reactive::BudgetedRead::Missing);
        };
        let version = to_u64(version, "historical version")?;
        let Some(receipt) =
            self.receipt_for_fact_version_with_budget(world, root, key, version, budget)?
        else {
            return Ok(crate::reactive::BudgetedRead::Exhausted);
        };
        if receipt.body.commit_seq > commit_seq {
            return Err(unproven_snapshot());
        }
        let Some(snapshot) = self.proven_snapshot_with_budget(world, root, key, version, budget)?
        else {
            return Ok(crate::reactive::BudgetedRead::Exhausted);
        };
        if !snapshot.lineage.iter().any(|name| name == object_type) {
            return Ok(crate::reactive::BudgetedRead::Missing);
        }
        Ok(match historical_record(snapshot) {
            Some(record) => crate::reactive::BudgetedRead::Found(record),
            None => crate::reactive::BudgetedRead::Missing,
        })
    }

    fn proven_snapshot(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<FactSnapshot, CellError> {
        let root = self.root_type(object_type).unwrap_or(object_type);
        let row: Option<(String, String)> = self.conn.query_optional(
            &format!("SELECT \"snapshot\", \"hash\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND (\"root_type\" = ?2 OR \"object_type\" = ?2) AND \"id\" = ?3 AND \"version\" = ?4"),
            params![world, root, key, to_i64(version, "fact version")?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (encoded, hash) = row.ok_or_else(unproven_snapshot)?;
        decode_snapshot(world, object_type, key, version, &encoded, &hash)
    }

    /// Authenticate the receipt that emitted one exact committed fact version.
    /// The existing snapshot primary key locates its commit without scanning
    /// ledger events. Both the immutable snapshot and the receipt's hashed
    /// world/write span must agree with the requested identity and version.
    ///
    /// # Errors
    /// Missing/corrupt snapshot, lookup or receipt; engine failures.
    pub fn receipt_for_fact_version(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<Receipt, CellError> {
        let root = self.root_type(object_type).unwrap_or(object_type);
        let row: Option<(String, String, i64)> = self.conn.query_optional(
            &format!("SELECT \"snapshot\", \"hash\", \"commit_seq\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"root_type\" = ?2 AND \"id\" = ?3 AND \"version\" = ?4"),
            params![world, root, key, to_i64(version, "fact version")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (encoded, hash, commit_seq) = row.ok_or_else(unproven_snapshot)?;
        let snapshot = decode_snapshot(world, object_type, key, version, &encoded, &hash)?;
        if snapshot.root_type != root {
            return Err(unproven_snapshot());
        }
        let receipt = self
            .receipt_at(to_u64(commit_seq, "fact commit seq")?)?
            .ok_or_else(unproven_snapshot)?;
        if hash_json(&receipt.body)? != receipt.hash
            || receipt.body.world != world
            || !receipt.body.written.iter().any(|row| {
                row.object_type == snapshot.object_type && row.key == key && row.version == version
            })
        {
            return Err(unproven_snapshot());
        }
        Ok(receipt)
    }

    /// Authenticate one exact fact's emitting receipt within a shared read budget.
    /// Indexed length metadata admits both proof preimages before either is
    /// materialized. `None` means the row or byte budget cannot admit them.
    ///
    /// # Errors
    /// Missing/corrupt snapshot, lookup or receipt; engine failures.
    pub fn receipt_for_fact_version_with_budget(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
        budget: &mut crate::reactive::DecodeBudget,
    ) -> Result<Option<Receipt>, CellError> {
        let root = self.root_type(object_type).unwrap_or(object_type);
        let bytes: Option<i64> = self.conn.query_optional(
            &format!("SELECT octet_length(f.\"snapshot\") + octet_length(f.\"hash\") + COALESCE(octet_length(r.\"world\"), 0) + COALESCE(octet_length(r.\"idempotency_key\"), 0) + COALESCE(octet_length(r.\"request_hash\"), 0) + octet_length(r.\"receipt\") + octet_length(r.\"receipt_hash\") FROM \"{FACT_VALUES}\" f JOIN \"{RECEIPT}\" r ON r.\"commit_seq\" = f.\"commit_seq\" WHERE f.\"world\" = ?1 AND f.\"root_type\" = ?2 AND f.\"id\" = ?3 AND f.\"version\" = ?4"),
            params![world, root, key, to_i64(version, "fact version")?],
            |row| row.get(0),
        )?;
        let bytes = to_u64(
            bytes.ok_or_else(unproven_snapshot)?,
            "fact receipt proof bytes",
        )?
        .saturating_add(128);
        if !budget.charge_rows(2) || !budget.charge(bytes) {
            return Ok(None);
        }
        self.receipt_for_fact_version(world, object_type, key, version)
            .map(Some)
    }

    pub(crate) fn retained_fact_scope(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<String, CellError> {
        Ok(self
            .proven_snapshot(world, object_type, key, version)?
            .scope)
    }

    pub(crate) fn verify_value_history(&self) -> Result<(), CellError> {
        for receipt in self.receipt_stream() {
            let receipt = receipt?;
            for row in receipt.body.written {
                self.proven_snapshot(&receipt.body.world, &row.object_type, &row.key, row.version)?;
            }
        }
        Ok(())
    }

    /// Read one exact historical version only after verifying its immutable proof.
    /// A proven tombstone returns `None`; missing or corrupted proofs refuse.
    ///
    /// # Errors
    /// Missing/mismatched proof, unknown type, or SQL failure.
    pub fn object_at_version(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<Option<ObjectRecord>, CellError> {
        let snapshot = self.proven_snapshot(world, object_type, key, version)?;
        Ok(historical_record(snapshot))
    }

    /// Budgeted exact-version read for historical projections.
    /// The commitment preimage is admitted by stored byte length before decoding.
    ///
    /// # Errors
    /// Missing/corrupt proof or engine failure.
    pub fn object_at_version_with_budget(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
        budget: &mut crate::reactive::DecodeBudget,
    ) -> Result<crate::reactive::BudgetedRead, CellError> {
        Ok(
            match self.proven_snapshot_with_budget(world, object_type, key, version, budget)? {
                Some(snapshot) => match historical_record(snapshot) {
                    Some(record) => crate::reactive::BudgetedRead::Found(record),
                    None => crate::reactive::BudgetedRead::Missing,
                },
                None => crate::reactive::BudgetedRead::Exhausted,
            },
        )
    }

    fn proven_snapshot_with_budget(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
        budget: &mut crate::reactive::DecodeBudget,
    ) -> Result<Option<FactSnapshot>, CellError> {
        let root = self.root_type(object_type).unwrap_or(object_type);
        let length: Option<i64> = self.conn.query_optional(&format!("SELECT octet_length(\"snapshot\") + octet_length(\"hash\") FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND (\"root_type\" = ?2 OR \"object_type\" = ?2) AND \"id\" = ?3 AND \"version\" = ?4"), params![world, root, key, to_i64(version, "fact version")?], |row| row.get(0))?;
        let length = length.ok_or_else(unproven_event)?;
        if !budget.charge_rows(1) || !budget.charge(to_u64(length, "historical snapshot bytes")?) {
            return Ok(None);
        }
        self.proven_snapshot(world, object_type, key, version)
            .map(Some)
    }

    /// Authenticate a complete snapshot against engine-materialized committed values.
    ///
    /// # Errors
    /// Missing proof or snapshot mismatch; no caller value becomes a commitment.
    pub fn verify_fact_snapshot(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        version: u64,
        values: &BTreeMap<String, Value>,
    ) -> Result<(), CellError> {
        if version == 0 {
            return if values.is_empty() {
                Ok(())
            } else {
                Err(CellError::HistoricalValueUnproven(
                    "nonempty absent snapshot".to_owned(),
                ))
            };
        }
        let snapshot = self.proven_snapshot(world, object_type, key, version)?;
        if snapshot
            .values
            .as_ref()
            .map_or(!values.is_empty(), |stored| stored != values)
        {
            return Err(CellError::HistoricalValueUnproven(
                "historical snapshot does not match its value commitment".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_modified_before(
        &self,
        world: &str,
        key: &norn_kernel::FactKey,
        before_version: u64,
        after_version: u64,
        before: &BTreeMap<String, Value>,
        commit_seq: Option<u64>,
    ) -> Result<(), CellError> {
        if before_version == 0 {
            return self.verify_fact_snapshot(
                world,
                key.object_type.as_str(),
                &key.key,
                before_version,
                before,
            );
        }
        let snapshot =
            self.proven_snapshot(world, key.object_type.as_str(), &key.key, before_version)?;
        if snapshot.values.as_ref() == Some(before) {
            return Ok(());
        }
        let after =
            self.proven_snapshot(world, key.object_type.as_str(), &key.key, after_version)?;
        if snapshot.object_type != after.object_type {
            return Err(unproven_event());
        }
        // Replay each admitted schema projection: a newly added default is
        // materialized once, and changing that default later preserves the
        // already stored value. Neither caller values nor today's schema can
        // substitute for the immutable values and intervening pinned models.
        let version_at = |version| {
            self.receipt_for_fact_version(world, key.object_type.as_str(), &key.key, version)
                .map(|receipt| receipt.body.definitions_version)
        };
        let first = version_at(before_version)?;
        // At append time this commit has materialized values but no receipt
        // yet. The preceding hashed receipt pins its active definitions; the
        // exact emission-to-commit check below still binds the after snapshot.
        let last = if let Some(seq) = commit_seq {
            self.definitions_version_at(seq)?.unwrap_or(0)
        } else {
            version_at(after_version)?
        };
        if first >= last
            || (first != 0
                && self
                    .installed_definitions(Some(first))?
                    .is_none_or(|installed| installed.model_hash != snapshot.model_hash))
        {
            return Err(unproven_event());
        }
        let object_type = norn_ir::ObjectTypeName(after.object_type);
        let mut materialized = snapshot.values.ok_or_else(unproven_event)?;
        let mut previous_hash = snapshot.model_hash;
        for version in first.saturating_add(1)..=last {
            let installed = self
                .installed_definitions(Some(version))?
                .ok_or_else(unproven_event)?;
            if version == last && installed.model_hash != after.model_hash {
                return Err(unproven_event());
            }
            if installed.model_hash == previous_hash {
                continue;
            }
            let historical_model;
            let model = if self.model().model_hash() == installed.model_hash {
                self.model()
            } else {
                let record = crate::export_journal::read_record(self.conn, installed.commit_seq)?
                    .ok_or_else(unproven_event)?;
                let installation = record.definition_install.ok_or_else(unproven_event)?;
                historical_model = Model::from_snapshot(&installation.model)?;
                if installation.installed != installed
                    || historical_model.model_hash() != installed.model_hash
                    || record.model_hash != installed.model_hash
                {
                    return Err(unproven_event());
                }
                &historical_model
            };
            materialized = materialize_historical_values(model, &object_type, &materialized)?;
            previous_hash = installed.model_hash;
        }
        if materialized != *before {
            return Err(unproven_event());
        }
        Ok(())
    }

    /// Verify value snapshots without attributing an unversioned event to a commit.
    /// Created and Set require the commit-bound historical accessor instead.
    ///
    /// # Errors
    /// Malformed event or unproven historical values.
    pub fn verify_event_snapshots(&self, world: &str, value: &Value) -> Result<(), CellError> {
        self.verify_event_values(world, None, value)
    }

    /// Verify fact payloads against independent engine-materialized emissions.
    /// No event payload is used to create a value commitment.
    pub(crate) fn verify_event_values(
        &self,
        world: &str,
        commit_seq: Option<u64>,
        value: &Value,
    ) -> Result<(), CellError> {
        let event = if value.get("op").and_then(Value::as_str) == Some("observed_fact") {
            value.get("event").ok_or_else(unproven_event)?
        } else {
            value
        };
        if !matches!(
            event.get("op").and_then(Value::as_str),
            Some("created" | "set" | "modified")
        ) {
            return Ok(());
        }
        if [value, event]
            .into_iter()
            .any(|v| v.get("world").is_some_and(|v| v.as_str() != Some(world)))
        {
            return Err(unproven_event());
        }
        let mut canonical = event.clone();
        if let Some(object) = canonical.as_object_mut() {
            object.remove("causality");
            object.remove("world");
        }
        let decoded: norn_kernel::FactEvent =
            norn_ir::runtime::decode(&canonical).map_err(|_invalid| unproven_event())?;
        match decoded {
            norn_kernel::FactEvent::Modified {
                key,
                before_version,
                after_version,
                before,
                after,
                changed_properties,
            } => {
                let changed = before
                    .keys()
                    .chain(after.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter(|property| before.get(*property) != after.get(*property))
                    .map(|property| norn_ir::PropertyId(property.clone()))
                    .collect::<Vec<_>>();
                if changed_properties != changed
                    || before_version.checked_next() != Some(after_version)
                {
                    return Err(unproven_event());
                }
                self.verify_fact_snapshot(
                    world,
                    key.object_type.as_str(),
                    &key.key,
                    after_version.0,
                    &after,
                )?;
                self.verify_modified_before(
                    world,
                    &key,
                    before_version.0,
                    after_version.0,
                    &before,
                    commit_seq,
                )?;
                if let Some(seq) = commit_seq {
                    let emissions = self.event_emissions(world, seq, &key)?;
                    if !emissions
                        .iter()
                        .any(|snapshot| snapshot.version == after_version.0)
                    {
                        return Err(unproven_event());
                    }
                }
            }
            norn_kernel::FactEvent::Created { key, values } => {
                let seq = commit_seq.ok_or_else(unproven_event)?;
                let snapshot = self.unique_event_emission(world, seq, &key, |snapshot| {
                    self.created_values_proven(snapshot, &values)
                })?;
                self.verify_prior_absence(&snapshot)?;
            }
            norn_kernel::FactEvent::Set {
                key,
                property,
                value,
            } => {
                let seq = commit_seq.ok_or_else(unproven_event)?;
                self.unique_event_emission(world, seq, &key, |snapshot| {
                    snapshot
                        .values
                        .as_ref()
                        .and_then(|values| values.get(property.as_str()))
                        == Some(&value)
                })?;
            }
            norn_kernel::FactEvent::CommandEnqueued { .. }
            | norn_kernel::FactEvent::NamingSeriesAllocated { .. }
            | norn_kernel::FactEvent::NamingSeriesCounterSeeded { .. }
            | norn_kernel::FactEvent::Deleted { .. }
            | norn_kernel::FactEvent::Transitioned { .. }
            | norn_kernel::FactEvent::InvariantRefused { .. } => return Err(unproven_event()),
        }
        Ok(())
    }

    fn verify_prior_absence(&self, snapshot: &FactSnapshot) -> Result<(), CellError> {
        if snapshot.version > 1 {
            let prior = self.proven_snapshot(
                &snapshot.world,
                &snapshot.root_type,
                &snapshot.key,
                snapshot.version.saturating_sub(1),
            )?;
            if prior.values.is_some() || prior.scope != snapshot.scope {
                return Err(unproven_event());
            }
        }
        Ok(())
    }

    pub(crate) fn verify_durable_event(
        &self,
        world: &str,
        commit_seq: u64,
        value: &Value,
    ) -> Result<(), CellError> {
        let event = if value.get("op").and_then(Value::as_str) == Some("observed_fact") {
            value.get("event").unwrap_or(value)
        } else {
            value
        };
        if matches!(
            event.get("op").and_then(Value::as_str),
            Some("created" | "modified")
        ) {
            self.verify_event_values(world, Some(commit_seq), value)?;
        }
        Ok(())
    }

    // Each event kind consumes one matching immutable mutation emission. A
    // granular Set and its aggregate Modified event are separate contracts.
    pub(crate) fn verify_fact_event_multiset(
        &self,
        world: &str,
        commit_seq: u64,
        events: &[Value],
    ) -> Result<(), CellError> {
        let mut claimed = std::collections::BTreeSet::new();
        for value in events {
            let event = if value.get("op").and_then(Value::as_str) == Some("observed_fact") {
                value.get("event").ok_or_else(unproven_event)?
            } else {
                value
            };
            let Some(op @ ("created" | "set" | "modified" | "deleted" | "transitioned")) =
                event.get("op").and_then(Value::as_str)
            else {
                continue;
            };
            let key: norn_kernel::FactKey =
                norn_ir::runtime::decode(event.get("key").ok_or_else(unproven_event)?)
                    .map_err(|_invalid| unproven_event())?;
            let root = self
                .root_type(key.object_type.as_str())
                .unwrap_or(key.object_type.as_str());
            let mut candidates = Vec::new();
            for snapshot in self.event_emissions(world, commit_seq, &key)? {
                if self.native_event_matches_emission(event, op, &snapshot)? {
                    candidates.push((
                        op.to_owned(),
                        root.to_owned(),
                        key.key.clone(),
                        snapshot.version,
                        event
                            .get("property")
                            .or_else(|| event.get("lifecycle"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ));
                }
            }
            // Unproven granular records remain opaque; valid native events
            // cannot repeat a claim on an already used mutation emission.
            if !candidates.is_empty() && !candidates.into_iter().any(|claim| claimed.insert(claim))
            {
                return Err(unproven_event());
            }
        }
        Ok(())
    }

    fn native_event_matches_emission(
        &self,
        event: &Value,
        op: &str,
        snapshot: &FactSnapshot,
    ) -> Result<bool, CellError> {
        let matches = match op {
            "created" => event
                .get("values")
                .and_then(Value::as_object)
                .is_some_and(|values| self.created_values_proven(snapshot, values)),
            "modified" => {
                event
                    .get("afterVersion")
                    .and_then(Value::as_i64)
                    .and_then(|v| u64::try_from(v).ok())
                    == Some(snapshot.version)
            }
            "set" => {
                event.get("value").is_some()
                    && event
                        .get("property")
                        .and_then(Value::as_str)
                        .and_then(|property| {
                            snapshot
                                .values
                                .as_ref()
                                .and_then(|values| values.get(property))
                        })
                        == event.get("value")
            }
            "deleted" | "transitioned" if snapshot.version > 1 => {
                let prior = self.proven_snapshot(
                    &snapshot.world,
                    &snapshot.root_type,
                    &snapshot.key,
                    snapshot.version.saturating_sub(1),
                )?;
                if op == "deleted" {
                    snapshot.values.is_none() && prior.values.is_some()
                } else {
                    event
                        .get("lifecycle")
                        .and_then(Value::as_str)
                        .is_some_and(|property| {
                            event.get("to").is_some()
                                && snapshot
                                    .values
                                    .as_ref()
                                    .and_then(|values| values.get(property))
                                    == event.get("to")
                                && event.get("from").is_some()
                                && prior
                                    .values
                                    .as_ref()
                                    .and_then(|values| values.get(property))
                                    == event.get("from")
                        })
                }
            }
            _ => false,
        };
        Ok(matches)
    }

    fn created_values_proven(
        &self,
        snapshot: &FactSnapshot,
        supplied: &BTreeMap<String, Value>,
    ) -> bool {
        let Some(stored) = &snapshot.values else {
            return false;
        };
        if stored == supplied {
            return true;
        }
        if snapshot.model_hash != self.model().model_hash() {
            return false;
        }
        let definitions = self.model().object_types().cloned().collect::<Vec<_>>();
        let catalog = norn_kernel::TypeCatalog::new(&definitions)
            .with_interfaces(self.model().interfaces())
            .with_property_types(self.model().property_types());
        // Use the writer's effective-value contract, including defaults, nullable
        // fields and scalar normalization. Event bytes still prove nothing until
        // the complete normalized value matches an independent snapshot.
        let Ok(effective) = catalog.effective_object_values(
            &norn_ir::ObjectTypeName(snapshot.object_type.clone()),
            supplied,
        ) else {
            return false;
        };
        if stored.len() != effective.len() {
            return false;
        }
        // Creation during observation reconciliation uses declared initial
        // lifecycle states before the final source state is materialized.
        effective.iter().all(|(property, value)| {
            stored.get(property).is_some_and(|actual| {
                actual == value
                    || value.as_str().is_some_and(|state| {
                        self.model()
                            .lifecycle(&snapshot.object_type, property)
                            .is_ok_and(|declaration| {
                                declaration.is_some_and(|declaration| {
                                    declaration.initial.iter().any(|initial| initial == state)
                                })
                            })
                    })
            })
        })
    }

    fn event_emissions(
        &self,
        world: &str,
        commit_seq: u64,
        key: &norn_kernel::FactKey,
    ) -> Result<Vec<FactSnapshot>, CellError> {
        let root = self
            .root_type(key.object_type.as_str())
            .unwrap_or(key.object_type.as_str());
        let rows = self.conn.query(&format!("SELECT \"version\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"commit_seq\" = ?2 AND \"root_type\" = ?3 AND \"id\" = ?4 ORDER BY \"version\" LIMIT 4097"), params![world, to_i64(commit_seq, "commit seq")?, root, &key.key])?;
        if rows.len() > 4096 {
            return Err(unproven_event());
        }
        let receipt = self.receipt_at(commit_seq)?;
        rows.into_iter()
            .map(|row| {
                let snapshot = self.proven_snapshot(
                    world,
                    key.object_type.as_str(),
                    &key.key,
                    to_u64(SqlRow(row).get(0)?, "fact version")?,
                )?;
                // During append, the new receipt is not written yet. On every
                // read, its hashed write span also binds the emission identity.
                if receipt.as_ref().is_some_and(|receipt| {
                    receipt.body.world != world
                        || !receipt.body.written.iter().any(|row| {
                            row.key == snapshot.key
                                && row.version == snapshot.version
                                && snapshot.lineage.contains(&row.object_type)
                        })
                }) {
                    return Err(unproven_event());
                }
                Ok(snapshot)
            })
            .collect()
    }

    fn unique_event_emission(
        &self,
        world: &str,
        commit_seq: u64,
        key: &norn_kernel::FactKey,
        matches: impl Fn(&FactSnapshot) -> bool,
    ) -> Result<FactSnapshot, CellError> {
        let mut matching = self
            .event_emissions(world, commit_seq, key)?
            .into_iter()
            .filter(matches);
        let snapshot = matching.next().ok_or_else(unproven_event)?;
        if matching.next().is_some() {
            return Err(unproven_event());
        }
        Ok(snapshot)
    }

    /// The only accessor for ledger-derived payloads before disclosure or rule lineage.
    /// It returns independently proven values or verified derivations. `None` masks
    /// an unprovable payload, including untyped finding messages and opaque events.
    /// Raw ledger/export records remain durability data, never a reveal capability.
    ///
    /// # Errors
    /// Corrupt commitments, malformed facts, or storage errors.
    #[allow(
        clippy::too_many_lines,
        reason = "dispatches historical proof contracts before one disclosure boundary"
    )]
    pub fn verified_history_event(&self, entry: &LedgerEntry) -> Result<Option<Value>, CellError> {
        if entry.seq == 0 {
            return Ok(None);
        }
        let Some(stored) = self
            .ledger_page(entry.seq.saturating_sub(1), 1)?
            .into_iter()
            .find(|stored| stored.seq == entry.seq)
        else {
            return Ok(None);
        };
        if stored.world != entry.world
            || stored.commit_seq != entry.commit_seq
            || stored.hash != entry.hash
            || crate::read::entry_hash(
                stored.seq,
                stored.commit_seq,
                &stored.world,
                &stored.event,
                &stored.prev_hash,
            )? != stored.hash
        {
            return Ok(None);
        }
        let authenticated = self.project_stored_history_event(&stored)?;
        if entry.event == stored.event {
            return Ok(authenticated);
        }
        // A caller may remove unproven wrapper metadata, but cannot introduce
        // a different business occurrence under an authentic sequence/hash.
        let proposed = self.project_stored_history_event(entry)?;
        let (Some(authenticated), Some(proposed)) = (authenticated, proposed) else {
            return Ok(None);
        };
        Ok(history_projection_is_subset(&proposed, &authenticated).then_some(proposed))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "dispatches independent historical producer proofs"
    )]
    fn project_stored_history_event(
        &self,
        entry: &LedgerEntry,
    ) -> Result<Option<Value>, CellError> {
        match self.verify_event_values(&entry.world, Some(entry.commit_seq), &entry.event) {
            Ok(()) => {}
            Err(CellError::HistoricalValueUnproven(_unproven)) => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some(receipt) = self.receipt_at(entry.commit_seq)? else {
            return Ok(None);
        };
        if receipt.body.world != entry.world
            || entry.seq < receipt.body.first_event_seq
            || entry.seq.saturating_sub(receipt.body.first_event_seq) >= receipt.body.event_count
        {
            return Ok(None);
        }
        if entry.event.get("type").and_then(Value::as_str) == Some("marking.declassified") {
            let Some(object_type) = entry.event.get("objectType").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(key) = entry.event.get("key").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(version) = entry
                .event
                .get("expectedVersion")
                .and_then(Value::as_i64)
                .and_then(|version| u64::try_from(version).ok())
            else {
                return Ok(None);
            };
            self.proven_snapshot(&entry.world, object_type, key, version)?;
            // The typed writer authenticates this audit event at admission. Free
            // text (actor/reason) still has no independent value lineage.
            return Ok(Some(
                norn_ir::runtime_object! { "type" => "marking.declassified", "objectType" => object_type, "key" => key, "expectedVersion" => version },
            ));
        }
        if entry.event.get("type").and_then(Value::as_str) == Some("definitions.installed") {
            let Some(installed) =
                self.installed_definitions(Some(receipt.body.definitions_version))?
            else {
                return Ok(None);
            };
            if installed.commit_seq != entry.commit_seq {
                return Ok(None);
            }
            // Typed installs start at 1 and advance by exactly one. Derive the
            // predecessor from the receipt-bound retained version, never from
            // user-emittable event metadata (including its diff).
            let previous_version = installed
                .version
                .checked_sub(1)
                .filter(|version| *version != 0);
            return Ok(Some(
                norn_ir::runtime_object! { "type" => "definitions.installed", "version" => installed.version, "previousVersion" => previous_version, "receiptSha256" => installed.receipt_sha256, "registry" => installed.registry },
            ));
        }
        let op = entry.event.get("op").and_then(Value::as_str);
        if matches!(
            op,
            Some("naming_series_allocated" | "naming_series_counter_seeded")
        ) {
            return self.verified_naming_event(entry, receipt.body.logical_time);
        }
        if matches!(op, Some("transitioned" | "observed_transition")) {
            return self.verified_transition_event(entry);
        }
        if matches!(
            op,
            Some("created" | "set" | "modified" | "deleted" | "observed_fact")
        ) {
            return self.verified_native_event(entry);
        }
        let Some(sources) = self.history_sources(entry, op == Some("data_quality_finding"))? else {
            return Ok(None);
        };
        if entry.event.get("type").and_then(Value::as_str) == Some("seed.loaded") {
            return self.verified_seed_event(entry, &receipt);
        }
        if op == Some("data_quality_finding") {
            return self.verified_finding_event(entry, &sources);
        }

        // Opaque payloads require classified committed emissions. Consumed input
        // byte equality alone is not disclosure authority.
        // Every non-contract value must be independently present in a verified
        // receipt emission. A same-key public row alone never authenticates text.
        if payload_from_sources(&entry.event, &sources, true) {
            return Ok(Some(entry.event.clone()));
        }
        // Preserve individually authenticated members while omitting unrelated
        // text. A bare protocol discriminator carries no evidence by itself.
        let Some(object) = entry.event.as_object() else {
            return Ok(None);
        };
        let projected: BTreeMap<_, _> = object
            .iter()
            .filter(|(name, value)| {
                payload_from_sources(
                    &Value::Object(BTreeMap::from([((*name).clone(), (*value).clone())])),
                    &sources,
                    true,
                )
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if projected.len() > 1 && (projected.contains_key("op") || projected.contains_key("type")) {
            return Ok(Some(Value::Object(projected)));
        }
        Ok(None)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the two naming event projections share one committed counter proof"
    )]
    fn verified_naming_event(
        &self,
        entry: &LedgerEntry,
        logical_time: u64,
    ) -> Result<Option<Value>, CellError> {
        let event = &entry.event;
        let allocated = event.get("op").and_then(Value::as_str) == Some("naming_series_allocated");
        let Some(raw_counter) = event.get("counter") else {
            return Ok(None);
        };
        let Ok(counter) = norn_ir::runtime::decode::<norn_kernel::FactKey>(raw_counter) else {
            return Ok(None);
        };
        if counter.object_type.as_str() != norn_kernel::NAMING_SERIES_COUNTER_TYPE {
            return Ok(None);
        }
        let emissions = self.event_emissions(&entry.world, entry.commit_seq, &counter)?;
        let [snapshot] = emissions.as_slice() else {
            return Ok(None);
        };
        let Some(values) = &snapshot.values else {
            return Ok(None);
        };
        let fields: &[&str] = if allocated {
            let Some(raw_key) = event.get("key") else {
                return Ok(None);
            };
            let Ok(key) = norn_ir::runtime::decode::<norn_kernel::FactKey>(raw_key) else {
                return Ok(None);
            };
            if self
                .event_emissions(&entry.world, entry.commit_seq, &key)?
                .is_empty()
                || values.get("objectType").and_then(Value::as_str)
                    != Some(key.object_type.as_str())
                || values.get("current") != event.get("value")
                || event.get("epochMillis")
                    != Some(&norn_ir::runtime::encode(&logical_time).map_err(CellError::Encoding)?)
                || values.get("tzdbVersion") != event.get("tzdbVersion")
            {
                return Ok(None);
            }
            let Some(declaration) = values.get("declaration") else {
                return Ok(None);
            };
            if ["pattern", "timezone", "resetPolicy", "gapPolicy"]
                .iter()
                .any(|field| {
                    declaration.get(field).is_none() || declaration.get(field) != event.get(field)
                })
            {
                return Ok(None);
            }
            &[
                "op",
                "key",
                "counter",
                "pattern",
                "timezone",
                "epochMillis",
                "tzdbVersion",
                "scope",
                "period",
                "value",
                "resetPolicy",
                "gapPolicy",
            ]
        } else {
            if ["objectType", "keyName", "current"]
                .iter()
                .any(|field| values.get(*field).is_none() || values.get(*field) != event.get(field))
            {
                return Ok(None);
            }
            &[
                "op",
                "counter",
                "objectType",
                "keyName",
                "scope",
                "period",
                "current",
            ]
        };
        if ["scope", "period"]
            .iter()
            .any(|field| values.get(*field).is_none() || values.get(*field) != event.get(field))
        {
            return Ok(None);
        }
        Ok(Some(Value::Object(
            fields
                .iter()
                .filter_map(|field| {
                    event
                        .get(field)
                        .map(|value| ((*field).to_owned(), value.clone()))
                })
                .collect(),
        )))
    }

    fn verified_transition_event(&self, entry: &LedgerEntry) -> Result<Option<Value>, CellError> {
        let native = entry.rule_input();
        let Ok(norn_kernel::FactEvent::Transitioned {
            key,
            lifecycle,
            transition,
            from,
            to,
        }) = norn_ir::runtime::decode(&native)
        else {
            return Ok(None);
        };
        if !self.model().has_type(key.object_type.as_str()) {
            return Ok(None);
        }
        let Some(declaration) = self
            .model()
            .lifecycle(key.object_type.as_str(), lifecycle.as_str())?
        else {
            return Ok(None);
        };
        if !declaration
            .transitions
            .iter()
            .any(|edge| edge.id == transition && edge.from.contains(&from) && edge.to == to)
        {
            return Ok(None);
        }
        let emissions = self.event_emissions(&entry.world, entry.commit_seq, &key)?;
        let mut proven = 0_usize;
        for snapshot in &emissions {
            if snapshot.model_hash != self.model().model_hash()
                || snapshot
                    .values
                    .as_ref()
                    .and_then(|values| values.get(lifecycle.as_str()))
                    .and_then(Value::as_str)
                    != Some(to.as_str())
                || snapshot.version <= 1
            {
                continue;
            }
            let prior = self.proven_snapshot(
                &entry.world,
                key.object_type.as_str(),
                &key.key,
                snapshot.version.saturating_sub(1),
            )?;
            if prior.model_hash == snapshot.model_hash
                && prior.scope == snapshot.scope
                && prior
                    .values
                    .as_ref()
                    .and_then(|values| values.get(lifecycle.as_str()))
                    .and_then(Value::as_str)
                    == Some(from.as_str())
            {
                proven = proven.saturating_add(1);
            }
        }
        if proven != 1 {
            return Ok(None);
        }
        // State/transition strings come from the verified immutable emission
        // and its pinned declaration; source metadata requires its own proof.
        let mut projected = if entry.event.get("op").and_then(Value::as_str)
            == Some("observed_transition")
        {
            norn_ir::runtime_object! { "op" => "observed_transition", "key" => entry.event.get("key"), "property" => lifecycle.as_str(), "transition" => transition.as_str(), "from" => from, "to" => to }
        } else {
            native
        };
        if let Some(provenance) = entry.event.get("provenance")
            && let Some(sources) = self.history_sources(entry, true)?
            && sources
                .iter()
                .any(|source| contains_value(source, provenance))
            && let Some(object) = projected.as_object_mut()
        {
            object.insert("provenance".to_owned(), provenance.clone());
        }
        Ok(Some(projected))
    }

    fn verified_native_event(&self, entry: &LedgerEntry) -> Result<Option<Value>, CellError> {
        let op = entry.event.get("op").and_then(Value::as_str);
        let native = if op == Some("observed_fact") {
            entry.event.get("event").ok_or_else(unproven_event)?
        } else {
            &entry.event
        };
        let mut canonical = native.clone();
        if let Some(object) = canonical.as_object_mut() {
            object.remove("causality");
            object.remove("world");
        }
        let decoded: norn_kernel::FactEvent =
            norn_ir::runtime::decode(&canonical).map_err(|_invalid| unproven_event())?;
        if let norn_kernel::FactEvent::Deleted { key } = &decoded {
            let emissions = self.event_emissions(&entry.world, entry.commit_seq, key)?;
            let mut proven = 0_usize;
            for snapshot in emissions
                .iter()
                .filter(|snapshot| snapshot.values.is_none() && snapshot.version > 1)
            {
                let prior = self.proven_snapshot(
                    &entry.world,
                    key.object_type.as_str(),
                    &key.key,
                    snapshot.version.saturating_sub(1),
                )?;
                if prior.values.is_some() && prior.scope == snapshot.scope {
                    proven = proven.saturating_add(1);
                }
            }
            if proven != 1 {
                return Ok(None);
            }
        }
        if matches!(decoded, norn_kernel::FactEvent::Transitioned { .. }) {
            return Ok(None);
        }
        let canonical = norn_ir::runtime::encode(&decoded).map_err(CellError::Encoding)?;
        let mut projected = if op == Some("observed_fact") {
            norn_ir::runtime_object! { "op" => "observed_fact", "event" => canonical }
        } else {
            canonical
        };
        if let Some(provenance) = entry.event.get("provenance")
            && let Some(sources) = self.history_sources(entry, true)?
            && sources
                .iter()
                .any(|source| contains_value(source, provenance))
            && let Some(object) = projected.as_object_mut()
        {
            object.insert("provenance".to_owned(), provenance.clone());
        }
        Ok(Some(projected))
    }

    fn history_sources(
        &self,
        entry: &LedgerEntry,
        include_inputs: bool,
    ) -> Result<Option<Vec<Value>>, CellError> {
        // Evidence reads are bounded independently of the size of the tiny
        // event being projected. Lengths are admitted before any preimage decode.
        let bytes: Option<i64> = self.conn.query_optional(&format!("SELECT COALESCE(SUM(octet_length(\"snapshot\") + octet_length(\"hash\")), 0) FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"commit_seq\" = ?2"), params![&entry.world, to_i64(entry.commit_seq, "commit seq")?], |row| row.get(0))?;
        if bytes.is_none_or(|bytes| bytes > 1024 * 1024) {
            return Ok(None);
        }
        let Some(receipt) = self.receipt_at(entry.commit_seq)? else {
            return Ok(None);
        };
        if receipt.body.written.len() > 4096 {
            return Ok(None);
        }
        let mut sources = Vec::new();
        for row in &receipt.body.written {
            let snapshot =
                self.proven_snapshot(&entry.world, &row.object_type, &row.key, row.version)?;
            sources.push(norn_ir::runtime::encode(&snapshot).map_err(CellError::Encoding)?);
            if let Some(encoded) = snapshot
                .values
                .as_ref()
                .and_then(|values| values.get("observationJson"))
                .and_then(Value::as_str)
                && let Ok(decoded) = serde_json::from_str::<Value>(encoded)
            {
                sources.push(decoded);
            }
        }
        self.history_producer_sources(entry, &receipt, &mut sources, include_inputs)?;
        Ok(Some(sources))
    }

    fn history_producer_sources(
        &self,
        entry: &LedgerEntry,
        receipt: &crate::store::Receipt,
        sources: &mut Vec<Value>,
        include_inputs: bool,
    ) -> Result<(), CellError> {
        let length = crate::export::record_decode_bytes(self.conn, entry.commit_seq)?;
        if length.is_none_or(|length| length > 1024 * 1024) {
            return Ok(());
        }
        let row: Option<(String, String)> = self.conn.query_optional(
            "SELECT hash, record FROM _norn_export_journal WHERE commit_seq = ?1",
            params![to_i64(entry.commit_seq, "commit seq")?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let Some((hash, encoded)) = row else {
            return Ok(());
        };
        let record = crate::payload_storage::decode_export(self.conn, &encoded, entry.commit_seq)
            .map_err(|_invalid| unproven_event())?;
        if record.hash()? != hash
            || &record.receipt != receipt
            || hash_json(&record.batch)? != receipt.body.request_hash
        {
            return Err(unproven_event());
        }
        // Delivery identity is the typed receipt contract, not input payload.
        // Preserve these headers even when all opaque input values are omitted.
        sources
            .push(norn_ir::runtime::encode(&receipt.body.consumed).map_err(CellError::Encoding)?);
        // Consumed preimages may prove typed producer metadata, never opaque
        // public payloads. Actual queued effects are independent authored data.
        // Never use batch.events, entries, or event-derived fields as evidence.
        if include_inputs && let Some(input) = record.consumed_input {
            if let Some(observed) = input.get("observed")
                && let Some(payload) = observed.get("input")
            {
                sources.push(norn_ir::runtime_object! { "payload" => payload });
            }
            if input.get("kind").and_then(Value::as_str) == Some("result") {
                let result = input.get("result");
                sources.push(norn_ir::runtime_object! {
                    "commandId" => input.get("command").and_then(|command| command.get("commandId")),
                    "generation" => input.get("command").and_then(|command| command.get("generation")),
                    "result" => result.and_then(|result| result.get("input")).or(result),
                });
            }
            sources.push(input);
        }
        sources.push(norn_ir::runtime::encode(&record.batch.effects).map_err(CellError::Encoding)?);
        // An accepted Function classification is derived from its independently
        // committed reconciliation state, with the receipt identity preserved.
        for row in &receipt.body.written {
            if row.object_type != "norn.FunctionCommand" {
                continue;
            }
            let snapshot =
                self.proven_snapshot(&entry.world, &row.object_type, &row.key, row.version)?;
            if let Some(accepted) = snapshot
                .values
                .as_ref()
                .and_then(|values| values.get("state"))
                .and_then(|state| state.get("accepted"))
                && accepted.as_object().is_some()
            {
                sources.push(norn_ir::runtime_object! { "commandId" => row.key, "classification" => "accepted", "receiptDigest" => accepted.get("receipt"), "generation" => accepted.get("generation") });
            }
        }
        Ok(())
    }

    fn verified_seed_event(
        &self,
        entry: &LedgerEntry,
        receipt: &Receipt,
    ) -> Result<Option<Value>, CellError> {
        let written = &receipt.body.written;
        if written.is_empty() {
            return Ok(None);
        }
        let mut objects = Vec::with_capacity(written.len());
        // history_sources has admitted this receipt's existing row/byte budgets.
        // Resolve each snapshot by receipt identity: its mixed source list also
        // contains decoded observation payloads, which are not snapshot proofs.
        for row in written {
            let snapshot =
                self.proven_snapshot(&entry.world, &row.object_type, &row.key, row.version)?;
            if row.version != 1
                || snapshot.values.is_none()
                || snapshot.object_type != row.object_type
            {
                return Ok(None);
            }
            objects.push(norn_ir::runtime_object! {
                "objectType" => row.object_type, "key" => row.key,
            });
        }
        let objects = Value::Array(objects);
        if entry.event.get("objects") != Some(&objects) {
            return Ok(None);
        }
        Ok(Some(norn_ir::runtime_object! {
            "type" => "seed.loaded", "objects" => objects,
        }))
    }

    fn verified_finding_event(
        &self,
        entry: &LedgerEntry,
        sources: &[Value],
    ) -> Result<Option<Value>, CellError> {
        let Some(raw) = entry.event.get("fact") else {
            return Ok(None);
        };
        // A complete finding can itself be a typed, committed factual value.
        // Match the entire object, never its dedupe id or a readable subject.
        if sources.iter().any(|source| contains_value(source, raw)) {
            return Ok(Some(
                norn_ir::runtime_object! { "op" => "data_quality_finding", "fact" => raw },
            ));
        }
        let Ok(finding) = norn_ir::runtime::decode::<norn_ir::DataQualityFinding>(raw) else {
            return Ok(None);
        };
        let Some(subject) = finding
            .subjects
            .first()
            .filter(|_| finding.subjects.len() == 1)
        else {
            return Ok(None);
        };
        let Some(digest) = &finding.evidence_digest else {
            return Ok(None);
        };
        // Provenance is data too: source, mapping, transform, fields and observation
        // ids must occur in an independently committed producer preimage.
        if finding.provenance.iter().any(|lineage| {
            norn_ir::runtime::encode(&lineage.provenance).map_or(true, |value| {
                !sources.iter().any(|source| contains_value(source, &value))
            })
        }) {
            return Ok(None);
        }
        let key = norn_kernel::FactKey::new(subject.object_type.as_str(), subject.key.clone());
        let emissions = self.event_emissions(&entry.world, entry.commit_seq, &key)?;
        let mut candidates = emissions.into_iter().filter(|snapshot| {
            snapshot.values.as_ref().is_some_and(|values| {
                hash_json(values).is_ok_and(|hash| format!("sha256:{hash}") == *digest)
            })
        });
        let Some(snapshot) = candidates.next() else {
            return Ok(None);
        };
        if candidates.next().is_some() || snapshot.model_hash != self.model().model_hash() {
            return Ok(None);
        }
        let Some(values) = snapshot.values else {
            return Ok(None);
        };
        let definitions = self.model().object_types().cloned().collect::<Vec<_>>();
        let catalog =
            norn_kernel::TypeCatalog::new(&definitions).with_interfaces(self.model().interfaces());
        if !norn_kernel::snapshot_observe::verify_finding_values(&catalog, &key, &values, &finding)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        Self::verified_finding_metadata(&finding, sources)
    }
    fn verified_finding_metadata(
        finding: &norn_ir::DataQualityFinding,
        sources: &[Value],
    ) -> Result<Option<Value>, CellError> {
        // Excluded identity metadata cannot be used to smuggle arbitrary text.
        if finding.provenance.iter().any(|lineage| {
            lineage.observation_ids.iter().any(|id| {
                !sources
                    .iter()
                    .any(|source| contains_value(source, &Value::from(id.clone())))
            })
        }) {
            return Ok(None);
        }
        let digest = finding
            .evidence_digest
            .as_ref()
            .ok_or_else(unproven_event)?;
        let canonical = norn_ir::DataQualityFinding::v2(
            finding.rule_ref.clone(),
            finding.subjects.clone(),
            finding.severity,
            finding.message.clone(),
            finding.outcome.clone(),
            finding.provenance.clone(),
            finding.first_seen.clone(),
            finding.last_seen.clone(),
            finding.status,
        )?
        .with_evidence_digest(digest.clone())?;
        if canonical.id != finding.id || canonical.dedupe_key != finding.dedupe_key {
            return Ok(None);
        }
        let mut projection = canonical;
        // Timestamps/status are identity-excluded and not evidence; retain them
        // only if their exact values are present in the proven producer preimage.
        if !sources
            .iter()
            .any(|source| contains_value(source, &Value::from(projection.first_seen.0.clone())))
            || !sources
                .iter()
                .any(|source| contains_value(source, &Value::from(projection.last_seen.0.clone())))
        {
            return Ok(None);
        }
        projection.status = norn_ir::DataQualityFindingStatus::Open;
        Ok(Some(
            norn_ir::runtime_object! { "op" => "data_quality_finding", "fact" => norn_ir::runtime::encode(&projection).map_err(CellError::Encoding)? },
        ))
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Rebuild into an empty host engine by replaying a complete verified export prefix.
    /// Accepts only the current format; pre-alpha stores must be recreated. The caller supplies the trusted final export
    /// hash and publishes the engine only after success. Definitions are replayed at
    /// each retained version, never replaced by today's model. Re-rooted current-format journal
    /// records and value proofs are generated by the normal writer, not imported.
    ///
    /// # Errors
    /// Unprovable/incomplete history, changed replay results, or store failure.
    #[allow(
        clippy::too_many_lines,
        reason = "verified replay keeps definition, input, receipt and journal checks together"
    )]
    pub fn rebuild_export_history(
        engine: E,
        records: &[ExportRecord],
        expected_head_hash: &str,
    ) -> Result<Self, CellError> {
        let refused = || {
            CellError::HistoricalValueUnproven(
                "export replay did not prove the complete history".to_owned(),
            )
        };
        let first = records.first().ok_or_else(refused)?;
        let initial = first
            .initial_model
            .as_ref()
            .or_else(|| {
                first
                    .definition_install
                    .as_ref()
                    .map(|install| &install.model)
            })
            .ok_or_else(refused)?;
        let mut previous = GENESIS_HASH.to_owned();
        for (index, record) in records.iter().enumerate() {
            if record.format != crate::export::EXPORT_FORMAT
                || record.store_format != STORE_FORMAT
                || record.receipt.body.commit_seq
                    != u64::try_from(index)
                        .map_err(|_range| refused())?
                        .saturating_add(1)
                || record.previous != previous
            {
                return Err(refused());
            }
            previous = record.hash()?;
        }
        if previous != expected_head_hash {
            return Err(refused());
        }
        let mut store = Self::from_engine(engine, Model::from_snapshot(initial)?)?;
        if store.export_commit_count()? != 0 {
            return Err(refused());
        }
        for record in records {
            for (key, value) in &record.bindings {
                store.bind(key, value)?;
            }
            if let Some(install) = &record.definition_install {
                let installed = store.install_definitions(
                    Model::from_snapshot(&install.model)?,
                    InstalledDefinitionInput {
                        canonical_set: install.installed.canonical_set.clone(),
                        registry: install.installed.registry.clone(),
                        receipt_sha256: install.installed.receipt_sha256.clone(),
                        admission_receipt: install.installed.admission_receipt.clone(),
                        readmission: install.readmission.clone(),
                        installed_at: install.installed.installed_at,
                        diff: install.diff.clone(),
                        layout: install.installed.layout.clone(),
                    },
                    Some(install.installed.version.saturating_sub(1)),
                    &record.batch.scope,
                )?;
                if installed != install.installed {
                    return Err(refused());
                }
            } else {
                if record.model_hash != store.model().model_hash()
                    || record.ddl_hash != store.model().ddl_hash()
                    || record.ddl_sql != store.model().ddl_sql()
                {
                    return Err(refused());
                }
                restore_input(&mut store, record)?;
                let ((), outcome) = store
                    .commit_guarded_submitted(
                        &record.batch,
                        record.quantity_input.as_ref(),
                        |_reader| Ok::<(), CellError>(()),
                        &mut |_point| Ok(()),
                    )
                    .map_err(|error| match error {
                        crate::store::GuardedError::Guard(error)
                        | crate::store::GuardedError::Store(error) => error,
                    })?;
                if !matches!(outcome, CommitOutcome::Committed(_))
                    || outcome.receipt() != &record.receipt
                {
                    return Err(refused());
                }
            }
            let rebuilt = store
                .export_record(record.receipt.body.commit_seq)?
                .ok_or_else(refused)?;
            // Format and previous export hash change in a big-bang rebuild. Every
            // replay-relevant field must otherwise remain exactly the same.
            let mut expected = record.clone();
            STORE_FORMAT.clone_into(&mut expected.store_format);
            expected.previous.clone_from(&rebuilt.previous);
            if rebuilt != expected {
                return Err(refused());
            }
        }
        store.read().verify_ledger()?;
        Ok(store)
    }
}

pub(crate) fn restore_input<E: SqlEngine>(
    store: &mut CellStore<E>,
    record: &ExportRecord,
) -> Result<(), CellError> {
    let Some(input) = &record.consumed_input else {
        return Ok(());
    };
    let seq = record.receipt.body.commit_seq;
    let mismatch = || CellError::HistoricalValueUnproven(format!("consumed input at commit {seq}"));
    let input = decode_input(input).map_err(|_invalid| mismatch())?;
    match input {
        ConsumedInput::Observation {
            position,
            observation_id,
            observed,
        } => {
            if position == 0
                || record.batch.consumes
                    != Some(crate::Consumption::Observation {
                        observation_id: observation_id.clone(),
                    })
            {
                return Err(mismatch());
            }
            // inject() assigns a new position; the export must retain the old
            // position even when unconsumed deliveries left gaps in the inbox.
            store.conn.execute(
                &format!("INSERT INTO \"{INBOX}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") VALUES (?1, ?2, ?3, NULL)"),
                params![to_i64(position, "position")?, observation_id, canonical_json(&observed)?],
            ).map_err(|_invalid| mismatch())?;
        }
        ConsumedInput::Timer { commit_seq, timer } => {
            let expected = crate::PendingTimer { commit_seq, timer };
            if store
                .read()
                .pending_timer(&expected.timer.timer_id)?
                .as_ref()
                != Some(&expected)
            {
                return Err(mismatch());
            }
        }
        ConsumedInput::Result {
            position,
            commit_seq,
            command,
            result,
            runs,
        } => {
            let head = store.read().head_command()?.ok_or_else(mismatch)?;
            if head.position != position
                || head.commit_seq != commit_seq
                || head.command != command
                || head.result.is_some()
                || result.is_none()
            {
                return Err(mismatch());
            }
            // Older generation runs can survive command replacement without
            // ever having been consumed. Recover the recorded history, without IO.
            let existing = store.read().runs(&command.command_id)?;
            if existing.iter().any(|run| !runs.contains(run)) {
                return Err(mismatch());
            }
            restore_run_history(store, record, &command, &runs, &existing)?;
            let current: Vec<_> = runs
                .iter()
                .filter(|run| run.generation == command.generation)
                .collect();
            if current.len() != 1
                || current.first().is_none_or(|run| {
                    run.attempt != 1
                        || run.consumed_commit_seq.is_some()
                        || Some(&run.outcome) != result.as_ref()
                })
            {
                return Err(mismatch());
            }
            let world = crate::payload_storage::receipt_world(&store.conn, head.commit_seq)?;
            let encoded_result = crate::payload_storage::encode_result(
                &store.conn,
                &record.batch.scope,
                &world,
                result.as_ref().ok_or_else(mismatch)?,
            )?;
            store.conn.execute(
                "UPDATE \"_norn_outbox\" SET \"result\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3",
                params![encoded_result, command.command_id, to_i64(command.generation, "generation")?],
            )?;
        }
    }
    // This compares the kind, consumption reference, full payload, order and
    // run history using the same preimage codec as the normal commit journal.
    if crate::export_journal::consumed_input(&store.read(), record.batch.consumes.as_ref())?
        .as_ref()
        != record.consumed_input.as_ref()
    {
        return Err(mismatch());
    }
    Ok(())
}

fn restore_run_history<E: SqlEngine>(
    store: &mut CellStore<E>,
    record: &ExportRecord,
    command: &crate::OutboxCommand,
    runs: &[crate::RunRecord],
    existing: &[crate::RunRecord],
) -> Result<(), CellError> {
    let seq = record.receipt.body.commit_seq;
    let mismatch = || CellError::HistoricalValueUnproven(format!("consumed input at commit {seq}"));
    for run in runs {
        if run.command_id != command.command_id
            || run.generation > command.generation
            || run.attempt == 0
        {
            return Err(mismatch());
        }
        if existing.contains(run) {
            continue;
        }
        if run.consumed_commit_seq.is_some() {
            return Err(mismatch());
        }
        let world =
            crate::payload_storage::command_world(&store.conn, &run.command_id, run.generation)?
                .ok_or_else(mismatch)?;
        let encoded_outcome = crate::payload_storage::encode_result(
            &store.conn,
            &record.batch.scope,
            &world,
            &run.outcome,
        )?;
        store.conn.execute(
            &format!("INSERT INTO \"{RUN}\" (\"command_id\", \"generation\", \"attempt\", \"outcome\", \"consumed_commit_seq\") VALUES (?1, ?2, ?3, ?4, NULL)"),
            params![run.command_id, to_i64(run.generation, "generation")?, to_i64(run.attempt, "attempt")?, encoded_outcome],
        ).map_err(|_invalid| mismatch())?;
    }
    Ok(())
}

// Decode the discriminator separately: internally tagged serde enums buffer
// numeric values, which would round exact runtime decimals in these preimages.
fn decode_input(input: &norn_ir::RuntimeValue) -> Result<ConsumedInput, String> {
    match input.get("kind").and_then(norn_ir::RuntimeValue::as_str) {
        Some("observation") => {
            let row: crate::PendingObservation = norn_ir::runtime::decode(input)?;
            Ok(ConsumedInput::Observation {
                position: row.position,
                observation_id: row.observation_id,
                observed: row.observed,
            })
        }
        Some("timer") => {
            let row: crate::PendingTimer = norn_ir::runtime::decode(input)?;
            Ok(ConsumedInput::Timer {
                commit_seq: row.commit_seq,
                timer: row.timer,
            })
        }
        Some("result") => {
            let row: crate::PendingCommand = norn_ir::runtime::decode(input)?;
            let runs = norn_ir::runtime::decode(input.get("runs").ok_or("missing runs")?)?;
            Ok(ConsumedInput::Result {
                position: row.position,
                commit_seq: row.commit_seq,
                command: row.command,
                result: row.result,
                runs,
            })
        }
        _ => Err("unknown consumed input kind".to_owned()),
    }
}

fn decode_snapshot(
    world: &str,
    object_type: &str,
    key: &str,
    version: u64,
    encoded: &str,
    hash: &str,
) -> Result<FactSnapshot, CellError> {
    let snapshot: FactSnapshot =
        serde_json::from_str(encoded).map_err(|_invalid| unproven_snapshot())?;
    if snapshot.world != world
        || !snapshot.lineage.iter().any(|name| name == object_type)
        || snapshot.key != key
        || snapshot.version != version
        || hash_json(&snapshot)? != hash
    {
        return Err(unproven_snapshot());
    }
    Ok(snapshot)
}

fn unproven_snapshot() -> CellError {
    CellError::HistoricalValueUnproven(
        "historical value commitment is missing or invalid".to_owned(),
    )
}

fn unproven_event() -> CellError {
    CellError::HistoricalValueUnproven(
        "event payload has no unique committed value proof".to_owned(),
    )
}

fn contains_value(source: &Value, value: &Value) -> bool {
    source == value
        || match source {
            Value::Object(object) => object.values().any(|child| contains_value(child, value)),
            Value::Array(array) => array.iter().any(|child| contains_value(child, value)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
        }
}

fn payload_from_sources(value: &Value, sources: &[Value], root: bool) -> bool {
    if sources.iter().any(|source| contains_value(source, value)) {
        return true;
    }
    let Value::Object(object) = value else {
        return false;
    };
    object.iter().all(|(name, value)| {
        (root
            && matches!(name.as_str(), "op" | "type")
            && value.as_str().is_some_and(|tag| {
                matches!(
                    tag,
                    "observation.recorded"
                        | "function_receipt"
                        | "observed_out_of_band"
                        | "command.result"
                        | "command.enqueued"
                        | "timer.fired"
                        | "command_enqueued"
                        | "naming_series_allocated"
                        | "naming_series_counter_seeded"
                        | "observed_transition"
                        | "observed_deleted"
                        | "observed_recreated"
                )
            }))
            || sources
                .iter()
                .any(|source| contains_member(source, name, value))
    })
}

fn contains_member(source: &Value, name: &str, value: &Value) -> bool {
    match source {
        Value::Object(object) => {
            object.get(name) == Some(value)
                || object
                    .values()
                    .any(|child| contains_member(child, name, value))
        }
        Value::Array(array) => array
            .iter()
            .any(|child| contains_member(child, name, value)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}
