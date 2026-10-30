//! The writes of one commit, inside its transaction.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{FactChange, ObjectTypeName, RuntimeValue as Value};
use norn_kernel::{EffectiveValuesError, TypeCatalog};
use norn_projections::PropertyColumn;
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, SCOPE_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, TYPE_COLUMN,
    VERSION_COLUMN, WORLD_COLUMN, quote_identifier,
};
use serde::Deserialize;

use crate::codec::{ValueOwner, canonical_json, decode, encode, hash_json};
use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::model::{ColumnRef, Model};
use crate::read::{
    ObjectRecord, StoreReader, column_list, entry_hash, next, placeholders, to_i64, to_u64,
};
use crate::store::{
    CommitBatch, Consumption, EffectRow, FactWriteRow, LinkChange, LinkedRow, OutboxCommand,
    Receipt, ReceiptBody, TimerEntry, WrittenRow,
};
use crate::system::{
    INBOX, LEDGER, LINK_CREATION, LINK_VERSIONS, MARKING_CURRENT, MARKING_OCCURRENCE,
    MARKING_SNAPSHOT, MARKING_VERSION, OUTBOX, PENDING_LINK_INDEX, PERFORM_CLAIM, RECEIPT, RUN,
    TIMER, VERSIONS, ensure_marking_subject, marking_subject,
};

struct NamingCounterAttribution {
    object_type: String,
    key_name: String,
    generated_property: String,
    declaration: norn_ir::NamingSeriesIR,
    scope: BTreeMap<norn_ir::PropertyId, String>,
    tzdb_version: String,
    period: String,
    current: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NamingSeriesCounterSeededEvent {
    op: String,
    counter: norn_kernel::cell_host::FactKey,
    object_type: ObjectTypeName,
    key_name: String,
    scope: BTreeMap<norn_ir::PropertyId, String>,
    period: String,
    current: u64,
}

fn naming_counter_attribution(
    record: &crate::read::ObjectRecord,
) -> Option<NamingCounterAttribution> {
    let object_type = record.values.get("objectType")?.as_str()?;
    let key_name = record.values.get("keyName")?.as_str()?;
    let generated_property = record.values.get("generatedProperty")?.as_str()?;
    if object_type.trim().is_empty()
        || key_name.trim().is_empty()
        || generated_property.trim().is_empty()
    {
        return None;
    }
    let declaration =
        norn_ir::runtime::decode::<norn_ir::NamingSeriesIR>(record.values.get("declaration")?)
            .ok()?;
    let scope = norn_ir::runtime::decode::<BTreeMap<norn_ir::PropertyId, String>>(
        record.values.get("scope")?,
    )
    .ok()?;
    let expected_scope = declaration
        .scope_fields
        .iter()
        .map(norn_ir::PropertyId::as_str)
        .collect::<BTreeSet<_>>();
    let actual_scope = scope
        .keys()
        .map(norn_ir::PropertyId::as_str)
        .collect::<BTreeSet<_>>();
    if expected_scope.len() != declaration.scope_fields.len() || actual_scope != expected_scope {
        return None;
    }
    let period = record.values.get("period")?.as_str()?;
    if !valid_naming_counter_period(declaration.reset_policy, period) {
        return None;
    }
    let tzdb_version = record.values.get("tzdbVersion")?.as_str()?;
    if tzdb_version.trim().is_empty() {
        return None;
    }
    let Value::Number(current) = record.values.get("current")? else {
        return None;
    };
    let current = current.to_string().parse().ok()?;
    Some(NamingCounterAttribution {
        object_type: object_type.to_owned(),
        key_name: key_name.to_owned(),
        generated_property: generated_property.to_owned(),
        declaration,
        scope,
        tzdb_version: tzdb_version.to_owned(),
        period: period.to_owned(),
        current,
    })
}

fn valid_naming_counter_period(policy: norn_ir::NamingResetPolicy, period: &str) -> bool {
    let bytes = period.as_bytes();
    match policy {
        norn_ir::NamingResetPolicy::Never => period.is_empty(),
        norn_ir::NamingResetPolicy::Year => {
            bytes.len() == 4 && bytes.iter().all(u8::is_ascii_digit)
        }
        norn_ir::NamingResetPolicy::Month => {
            bytes.len() == 7
                && bytes.get(4) == Some(&b'-')
                && bytes
                    .get(0..4)
                    .is_some_and(|year| year.iter().all(u8::is_ascii_digit))
                && bytes
                    .get(5..7)
                    .is_some_and(|month| month.iter().all(u8::is_ascii_digit))
        }
        norn_ir::NamingResetPolicy::Day => {
            bytes.len() == 10
                && bytes.get(4) == Some(&b'-')
                && bytes.get(7) == Some(&b'-')
                && bytes
                    .get(0..4)
                    .is_some_and(|year| year.iter().all(u8::is_ascii_digit))
                && bytes
                    .get(5..7)
                    .is_some_and(|month| month.iter().all(u8::is_ascii_digit))
                && bytes
                    .get(8..10)
                    .is_some_and(|day| day.iter().all(u8::is_ascii_digit))
        }
    }
}

fn naming_counter_seed_matches(
    event: &Value,
    counter_key: &str,
    attribution: &NamingCounterAttribution,
) -> bool {
    let Ok(event) = norn_ir::runtime::decode::<NamingSeriesCounterSeededEvent>(event) else {
        return false;
    };
    event.op == "naming_series_counter_seeded"
        && event.counter.object_type.as_str() == norn_kernel::NAMING_SERIES_COUNTER_TYPE
        && event.counter.key == counter_key
        && event.object_type.as_str() == attribution.object_type
        && event.key_name == attribution.key_name
        && event.scope == attribution.scope
        && event.period == attribution.period
        && event.current == attribution.current
}

fn unknown_lineage() -> BTreeSet<String> {
    BTreeSet::from(["norn.unknown-lineage".to_owned()])
}

type DestinationMarkings = BTreeMap<(String, String), BTreeSet<String>>;

/// Trusted marking lineage decoded from one stored provenance envelope.
struct TypedProvenanceMarkings {
    base: BTreeSet<String>,
    per_property: BTreeMap<String, BTreeSet<String>>,
}

/// Append the stored `columns` of one row to `names`/`params`: each value
/// from the complete effective object, checked for nullability and encoded;
/// overflow properties go into one canonical JSON object column.
fn encode_row<'c>(
    owner: &str,
    table: &str,
    columns: &'c [PropertyColumn],
    has_overflow: bool,
    row: &mut (Vec<&'c str>, Vec<SqlValue>),
    value_of: impl Fn(&str) -> Result<Value, CellError>,
) -> Result<(), CellError> {
    let mut overflow = BTreeMap::new();
    for column in columns {
        let name = column.property.as_str();
        let value = value_of(name)?;
        if value.is_null() && !column.nullable {
            return Err(CellError::MissingValue {
                owner: owner.to_owned(),
                property: name.to_owned(),
            });
        }
        let encoded = encode(owner, column, &value)?;
        if let Some(column_name) = &column.column {
            row.0.push(column_name);
            row.1.push(encoded);
        } else {
            overflow.insert(name.to_owned(), decode(table, column, &encoded)?);
        }
    }
    if has_overflow {
        row.0.push(OVERFLOW_COLUMN);
        row.1
            .push(SqlValue::Text(canonical_json(&Value::Object(overflow))?));
    }
    Ok(())
}

pub(crate) struct Writer<'a, E: SqlEngine> {
    conn: &'a E,
    model: &'a Model,
    batch: &'a CommitBatch,
    world: &'a str,
    commit_seq: u64,
    batch_output_facts: Option<BTreeSet<(String, String, String, u64)>>,
}

impl<'a, E: SqlEngine> Writer<'a, E> {
    pub(crate) fn new(
        conn: &'a E,
        model: &'a Model,
        batch: &'a CommitBatch,
        commit_seq: u64,
    ) -> Self {
        Self {
            conn,
            model,
            batch,
            world: &batch.world,
            commit_seq,
            batch_output_facts: None,
        }
    }

    const fn reader(&self) -> StoreReader<'a, E> {
        StoreReader::new(self.conn, self.model)
    }

    fn commit_seq_sql(&self) -> Result<i64, CellError> {
        to_i64(self.commit_seq, "commit_seq")
    }

    /// Consume the delivery, apply the whole-object writes (returning their
    /// new versions), then the fact changes.
    #[allow(
        clippy::too_many_lines,
        reason = "one transaction resolves value, occurrence and recomputed lineage before capturing its output snapshots"
    )]
    pub(crate) fn apply_writes(&mut self) -> Result<Vec<WrittenRow>, CellError> {
        self.verify_mutation_scopes()?;
        self.verify_derived_properties()?;
        let mut inherited = self.read_lineage_markings(&self.batch.marking_reads)?;
        inherited
            .extend(self.read_lineage_markings_at(&self.batch.marking_historical_reads, false)?);
        for read in &self.batch.marking_reference_reads {
            if read.expected.0 != 0 {
                inherited.extend(self.reference_lineage_markings(read)?);
            }
        }
        for (read, properties) in &self.batch.marking_property_reads {
            inherited.extend(self.property_lineage_markings(read, properties)?);
        }
        let occurrence_inherited = self.occurrence_lineage_markings(&inherited)?;
        let destination_markings = self.value_lineage_markings()?;
        self.batch_output_facts = Some(self.emitted_fact_versions()?);
        if let Some(consumption) = &self.batch.consumes {
            self.consume(consumption)?;
        }
        let mut written = Vec::with_capacity(self.batch.writes.len());
        let mut records = Vec::new();
        let mut changed_properties = BTreeMap::new();
        for write in &self.batch.writes {
            let write_inherited = self.destination_value_lineage(
                &inherited,
                &destination_markings,
                write.target().0,
                write.target().1,
            )?;
            let changed = self.write_changed_properties(write)?;
            let derived = self.derived_properties(write);
            let lineage_properties = changed.union(&derived).cloned().collect();
            let row = self.write(write)?;
            Self::record_changed_properties(
                &mut changed_properties,
                write.target().0,
                write.target().1,
                &changed,
            );
            if changed.is_empty() && matches!(write, FactWriteRow::Put { .. }) {
                // A no-op still emits an observable version and fact event.
                // Preserved values keep their labels; explicitly recomputed
                // outputs also refresh their provenance when values are equal.
                self.apply_occurrence_lineage_to_version(
                    &row.object_type,
                    &row.key,
                    row.version,
                    &occurrence_inherited,
                )?;
                if !derived.is_empty() {
                    self.apply_read_lineage_to_version(
                        &row.object_type,
                        &row.key,
                        row.version,
                        &derived,
                        &write_inherited,
                    )?;
                }
            } else {
                self.apply_read_lineage_to_version(
                    &row.object_type,
                    &row.key,
                    row.version,
                    &lineage_properties,
                    &write_inherited,
                )?;
            }
            if !changed.is_empty() || !derived.is_empty() {
                self.propagate_computed_markings(write.target().0, write.target().1)?;
            }
            if !changed.is_empty()
                && let Some(record) =
                    self.reader()
                        .object(self.world, &row.object_type, &row.key)?
            {
                records.push(record);
            }
            self.capture_value_commitment(&row)?;
            written.push(row);
        }
        for fact in &self.batch.facts {
            let (object_type, key) = Self::fact_target(fact);
            let write_inherited = self.destination_value_lineage(
                &inherited,
                &destination_markings,
                object_type,
                key,
            )?;
            let changed = self.fact_changed_properties(fact)?;
            let concrete_before = self.live(object_type, key)?;
            self.apply_fact(fact)?;
            Self::record_changed_properties(&mut changed_properties, object_type, key, &changed);
            let row = self.canonical_written_row(object_type, key, concrete_before)?;
            if changed.is_empty() && matches!(fact, FactChange::Set { .. }) {
                self.apply_occurrence_lineage_to_version(
                    &row.object_type,
                    &row.key,
                    row.version,
                    &occurrence_inherited,
                )?;
            } else {
                self.apply_read_lineage_to_version(
                    &row.object_type,
                    &row.key,
                    row.version,
                    &changed,
                    &write_inherited,
                )?;
            }
            if !changed.is_empty() {
                self.propagate_computed_markings(&row.object_type, key)?;
                if let Some(record) = self.reader().object(self.world, &row.object_type, key)? {
                    records.push(record);
                }
            }
            self.capture_value_commitment(&row)?;
            written.push(row);
        }
        // All snapshots were captured before later mutations.
        self.finish_write_markings(&written, &records, &changed_properties)?;
        let pending_records = self
            .batch
            .writes
            .iter()
            .map(FactWriteRow::target)
            .chain(self.batch.facts.iter().map(Self::fact_target))
            .filter(|(object_type, _)| *object_type == "norn.PendingLinks")
            .map(|(_, key)| key)
            .collect::<BTreeSet<_>>();
        for key in pending_records {
            self.index_pending_links(key)?;
        }
        Ok(written)
    }

    fn derived_properties(&self, write: &FactWriteRow) -> BTreeSet<String> {
        self.batch
            .marking_derived_properties
            .iter()
            .filter(|(key, _)| {
                key.object_type.as_str() == write.target().0 && key.key == write.target().1
            })
            .flat_map(|(_, properties)| properties.iter().cloned())
            .collect()
    }

    fn verify_derived_properties(&self) -> Result<(), CellError> {
        for (key, properties) in &self.batch.marking_derived_properties {
            let written = self.batch.writes.iter().find_map(|write| match write {
                FactWriteRow::Put {
                    object_type,
                    key: id,
                    values,
                } if object_type == key.object_type.as_str() && id == &key.key => Some(values),
                FactWriteRow::Put { .. } | FactWriteRow::Delete { .. } => None,
            });
            if written.is_none_or(|values| {
                properties
                    .iter()
                    .any(|property| !values.contains_key(property))
            }) {
                return Err(CellError::HistoricalValueUnproven(
                    "derived-property lineage must name properties present in an exact written destination".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn verify_mutation_scopes(&self) -> Result<(), CellError> {
        if !self.batch.writes.is_empty()
            || !self.batch.facts.is_empty()
            || !self.batch.marking_declassifications.is_empty()
        {
            let bound: Option<String> = self.conn.query_optional(
                &format!(
                    "SELECT value FROM \"{}\" WHERE key = ?1",
                    crate::system::META
                ),
                params!["binding:scope"],
                |row| row.get(0),
            )?;
            if let Some(found) = bound
                && found != self.batch.scope
            {
                return Err(CellError::BindingMismatch {
                    key: "scope".to_owned(),
                    expected: self.batch.scope.clone(),
                    found,
                });
            }
        }
        for (object_type, key) in self
            .batch
            .writes
            .iter()
            .map(FactWriteRow::target)
            .chain(self.batch.facts.iter().map(Self::fact_target))
            .chain(
                self.batch
                    .marking_declassifications
                    .iter()
                    .map(|change| (change.object_type.as_str(), change.key.as_str())),
            )
        {
            let retained_scope =
                if let Some(record) = self.reader().object(self.world, object_type, key)? {
                    Some(record.scope)
                } else {
                    let version = self.reader().high_water(self.world, object_type, key)?;
                    if version == 0 {
                        None
                    } else {
                        Some(self.reader().retained_fact_scope(
                            self.world,
                            self.reader().root_type(object_type)?,
                            key,
                            version,
                        )?)
                    }
                };
            if retained_scope.is_some_and(|scope| scope != self.batch.scope) {
                return Err(CellError::HistoricalValueUnproven(
                    "mutation scope differs from retained fact scope".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn canonical_written_row(
        &self,
        object_type: &str,
        key: &str,
        concrete_before: Option<String>,
    ) -> Result<WrittenRow, CellError> {
        let (object_type, version) = match self.reader().root_row(self.world, object_type, key)? {
            Some((concrete, _, version)) => (concrete, version),
            None => (
                concrete_before.unwrap_or_else(|| object_type.to_owned()),
                self.reader().high_water(self.world, object_type, key)?,
            ),
        };
        Ok(WrittenRow {
            object_type,
            key: key.to_owned(),
            version,
        })
    }

    fn finish_write_markings(
        &self,
        written: &[WrittenRow],
        records: &[ObjectRecord],
        changed_properties: &BTreeMap<(String, String), BTreeSet<String>>,
    ) -> Result<(), CellError> {
        self.apply_mapping_markings(written)?;
        self.close_emitted_markings(written)?;
        self.apply_fact_value_provenance_markings(changed_properties)?;
        self.apply_system_lineage_markings(records)?;
        self.close_emitted_markings(written)?;
        self.apply_marking_declassifications()?;
        Ok(())
    }

    fn close_emitted_markings(&self, written: &[WrittenRow]) -> Result<(), CellError> {
        for row in written {
            // Late receipt/system attribution must also flow through later
            // versions emitted in this batch, before declassification.
            if row.version > 1
                && let Some(previous) = self.previous_markings(
                    &row.object_type,
                    &row.key,
                    row.version.saturating_sub(1),
                )?
            {
                for (property, labels) in previous {
                    if !labels.is_empty() {
                        self.extend_marking_snapshot(
                            &row.object_type,
                            &row.key,
                            &property,
                            row.version,
                            &labels.into_iter().collect(),
                        )?;
                    }
                }
            }
            self.propagate_computed_markings_at(&row.object_type, &row.key, row.version)?;
        }
        Ok(())
    }

    // This derived index shares the record's transaction, including raw fact
    // changes, replay, deletion and rollback. No history/backfill path exists.
    fn index_pending_links(&self, key: &str) -> Result<(), CellError> {
        self.conn.execute(
            &format!("DELETE FROM \"{PENDING_LINK_INDEX}\" WHERE world = ?1 AND record_key = ?2"),
            params![self.world, key],
        )?;
        let Some(record) = self.reader().object(self.world, "norn.PendingLinks", key)? else {
            return Ok(());
        };
        let Some(Value::Array(links)) = record.values.get("links") else {
            return Err(CellError::Encoding(
                "pending links must be an array".to_owned(),
            ));
        };
        if links.len() != 1 {
            return Err(CellError::Encoding(
                "pending records require exactly one intent".to_owned(),
            ));
        }
        let catalog = TypeCatalog::new(self.model.object_types());
        let model = norn_kernel::CommitModel::new(&catalog);
        let mut sources = std::collections::BTreeSet::new();
        for entry in links {
            let field = |name| {
                entry
                    .get(name)
                    .and_then(Value::as_str)
                    .ok_or_else(|| CellError::Encoding(format!("pending link is missing {name}")))
            };
            sources.insert((
                model.canonical(&norn_kernel::FactKey::new(
                    field("sourceType")?,
                    field("sourceKey")?,
                )),
                model.canonical(&norn_kernel::FactKey::new(
                    field("targetType")?,
                    field("targetKey")?,
                )),
            ));
        }
        for (source, target) in sources {
            self.conn.execute(
                &format!("INSERT INTO \"{PENDING_LINK_INDEX}\" (world, source_type, source_key, record_key, target_type, target_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
                params![self.world, source.object_type.as_str(), source.key, key, target.object_type.as_str(), target.key],
            )?;
        }
        Ok(())
    }

    /// Quantity state joins the same atomic commit as ordinary writes.
    pub(crate) fn apply_quantity(
        &self,
    ) -> Result<Option<norn_kernel::quantity::QuantityResult>, CellError> {
        self.batch
            .quantity
            .as_ref()
            .map(|request| crate::quantity::apply(self.conn, self.batch, request, self.commit_seq))
            .transpose()
    }

    fn consume(&self, consumption: &Consumption) -> Result<(), CellError> {
        if !self.reader().is_pending(consumption)? {
            return Err(CellError::NotPending(consumption.clone()));
        }
        let commit_seq = self.commit_seq_sql()?;
        match consumption {
            Consumption::Timer { timer_id, .. } => {
                self.conn.execute(
                    &format!("DELETE FROM \"{TIMER}\" WHERE \"timer_id\" = ?1"),
                    params![timer_id],
                )?;
            }
            Consumption::Result {
                command_id,
                generation,
            } => {
                let generation = to_i64(*generation, "generation")?;
                self.conn.execute(
                    &format!(
                        "UPDATE \"{OUTBOX}\" SET \"consumed_commit_seq\" = ?1 WHERE \"command_id\" = ?2"
                    ),
                    params![commit_seq, command_id],
                )?;
                self.conn.execute(
                    &format!(
                        "UPDATE \"{RUN}\" SET \"consumed_commit_seq\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3 AND \"consumed_commit_seq\" IS NULL"
                    ),
                    params![commit_seq, command_id, generation],
                )?;
            }
            Consumption::Observation { observation_id } => {
                self.conn.execute(
                    &format!(
                        "UPDATE \"{INBOX}\" SET \"consumed_commit_seq\" = ?1 WHERE \"observation_id\" = ?2"
                    ),
                    params![commit_seq, observation_id],
                )?;
            }
        }
        Ok(())
    }

    fn write(&mut self, write: &FactWriteRow) -> Result<WrittenRow, CellError> {
        let (object_type, key) = write.target();
        let current = self.reader().version(self.world, object_type, key)?;
        let version = next(current, "version")?;
        let live = self.live(object_type, key)?;
        let mut concrete_type = live.clone().unwrap_or_else(|| object_type.to_owned());
        match write {
            FactWriteRow::Put { values, .. } => match live {
                Some(concrete) if concrete != object_type => {
                    return Err(CellError::ObjectExists {
                        object_type: object_type.to_owned(),
                        id: key.to_owned(),
                        world: self.world.to_owned(),
                    });
                }
                Some(_) => self.replace(object_type, key, values, version)?,
                None => self.create(object_type, key, values)?,
            },
            FactWriteRow::Delete { .. } => {
                if let Some(concrete) = live {
                    self.delete(object_type, &concrete, key)?;
                } else {
                    if self
                        .reader()
                        .root_row(self.world, object_type, key)?
                        .is_some()
                    {
                        // A sibling shares the high-water identity but is not
                        // an absent target. Match canonical Delete's refusal.
                        return Err(CellError::ObjectMissing {
                            object_type: object_type.to_owned(),
                            id: key.to_owned(),
                            world: self.world.to_owned(),
                        });
                    }
                    // An idempotent delete still emits a new high-water
                    // version. Retain its lineage even without a live row.
                    concrete_type = self
                        .retained_snapshot_type(object_type, key, current)?
                        .unwrap_or_else(|| object_type.to_owned());
                    self.persist_marking_snapshot(&concrete_type, key, version)?;
                    self.record_version(&concrete_type, key, version)?;
                    self.clear_current_markings(&concrete_type, key)?;
                }
            }
        }
        Ok(WrittenRow {
            object_type: concrete_type,
            key: key.to_owned(),
            version,
        })
    }

    fn apply_fact(&mut self, fact: &FactChange) -> Result<(), CellError> {
        match fact {
            FactChange::Create {
                object_type,
                key,
                values,
            } => {
                let object_type = object_type.as_str();
                if self.live(object_type, key)?.is_some() {
                    return Err(CellError::ObjectExists {
                        object_type: object_type.to_owned(),
                        id: key.clone(),
                        world: self.world.to_owned(),
                    });
                }
                self.create(object_type, key, values)
            }
            FactChange::Set {
                object_type,
                key,
                property,
                value,
            } => self.set(object_type.as_str(), key, property.as_str(), value),
            FactChange::Delete { object_type, key } => {
                let concrete = self.existing(object_type.as_str(), key)?;
                self.delete(object_type.as_str(), &concrete, key)
            }
            FactChange::Transition {
                object_type,
                key,
                lifecycle,
                transition,
            } => self.transition(
                object_type.as_str(),
                key,
                lifecycle.as_str(),
                transition.as_str(),
            ),
        }
    }

    /// The concrete type of live object `key` if it is an `object_type`.
    fn live(&self, object_type: &str, key: &str) -> Result<Option<String>, CellError> {
        Ok(self
            .reader()
            .root_row(self.world, object_type, key)?
            .map(|(concrete, _, _)| concrete)
            .filter(|concrete| self.model.is_a(concrete, object_type)))
    }

    /// The concrete type of existing object `key`, which must be an
    /// `object_type`.
    fn existing(&self, object_type: &str, key: &str) -> Result<String, CellError> {
        self.live(object_type, key)?
            .ok_or_else(|| CellError::ObjectMissing {
                object_type: object_type.to_owned(),
                id: key.to_owned(),
                world: self.world.to_owned(),
            })
    }

    /// Resolve a deleted ancestor's exact immutable subtype snapshot through
    /// indexed per-type lookups. Missing or ambiguous attribution stays unknown.
    fn retained_snapshot_type(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<Option<String>, CellError> {
        if version == 0 {
            return Ok(None);
        }
        let mut concrete = None;
        for definition in self
            .model
            .object_types()
            .filter(|definition| self.model.is_a(definition.name.as_str(), object_type))
        {
            let name = definition.name.as_str();
            if self
                .reader()
                .effective_markings(self.world, name, key, Some(version))?
                .is_some()
            {
                if concrete.is_some() {
                    return Ok(None);
                }
                concrete = Some(name.to_owned());
            }
        }
        Ok(concrete)
    }

    fn previous_markings(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<Option<BTreeMap<String, Vec<String>>>, CellError> {
        if let Some(snapshot) =
            self.reader()
                .effective_markings(self.world, object_type, key, Some(version))?
        {
            return Ok(Some(snapshot));
        }
        let Some(retained_type) =
            self.retained_snapshot_type(self.reader().root_type(object_type)?, key, version)?
        else {
            return Ok(None);
        };
        let Some(snapshot) =
            self.reader()
                .effective_markings(self.world, &retained_type, key, Some(version))?
        else {
            return Ok(None);
        };
        // A changed concrete schema cannot prove which new fields carry the
        // prior incarnation's classified values. Carry its complete union.
        let labels = snapshot
            .into_values()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        Ok(Some(
            self.model
                .effective_property_markings(object_type)?
                .into_keys()
                .map(|property| (property, labels.clone()))
                .collect(),
        ))
    }

    fn check_properties(
        &self,
        object_type: &str,
        values: &BTreeMap<String, Value>,
    ) -> Result<(), CellError> {
        for name in values.keys() {
            self.model.column(object_type, name)?;
        }
        Ok(())
    }

    fn effective_values(
        &self,
        object_type: &str,
        values: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, CellError> {
        let catalog = TypeCatalog::new(self.model.object_types())
            .with_interfaces(self.model.interfaces())
            .with_property_types(self.model.property_types());
        catalog
            .effective_object_values(&ObjectTypeName(object_type.to_owned()), values)
            .map_err(|error| match error {
                EffectiveValuesError::Runtime(error) => CellError::Value(error),
                EffectiveValuesError::ValueType { property, expected } => CellError::ValueType {
                    owner: object_type.to_owned(),
                    property,
                    expected,
                },
            })
    }

    /// Insert a new object of `object_type`; its version continues after the
    /// highest version its id ever had in the type hierarchy.
    fn create(
        &mut self,
        object_type: &str,
        key: &str,
        values: &BTreeMap<String, Value>,
    ) -> Result<(), CellError> {
        self.check_properties(object_type, values)?;
        let values = self.effective_values(object_type, values)?;
        let version = next(
            self.reader().high_water(self.world, object_type, key)?,
            "version",
        )?;
        self.record_version(object_type, key, version)?;
        for table in self.model.tables(object_type)? {
            let mut row: (Vec<&str>, Vec<SqlValue>) = (
                vec![WORLD_COLUMN, ID_COLUMN],
                vec![
                    SqlValue::Text(self.world.to_owned()),
                    SqlValue::Text(key.to_owned()),
                ],
            );
            if table.is_root() {
                row.0.extend([TYPE_COLUMN, SCOPE_COLUMN, VERSION_COLUMN]);
                row.1.extend([
                    SqlValue::Text(object_type.to_owned()),
                    SqlValue::Text(self.batch.scope.clone()),
                    SqlValue::Integer(to_i64(version, "version")?),
                ]);
            } else {
                // Subtype tables repeat the scope for their scoped key indexes.
                row.0.push(SCOPE_COLUMN);
                row.1.push(SqlValue::Text(self.batch.scope.clone()));
            }
            encode_row(
                object_type,
                &table.table,
                &table.columns,
                table.has_overflow,
                &mut row,
                |name| Ok(values.get(name).cloned().unwrap_or(Value::Null)),
            )?;
            self.conn.execute(
                &format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    quote_identifier(&table.table),
                    column_list(&row.0),
                    placeholders(row.0.len())
                ),
                &row.1,
            )?;
        }
        self.persist_marking_snapshot(object_type, key, version)
    }

    /// Replace every stored property of live object `key` and set its
    /// version.
    fn replace(
        &mut self,
        object_type: &str,
        key: &str,
        values: &BTreeMap<String, Value>,
        version: u64,
    ) -> Result<(), CellError> {
        self.check_properties(object_type, values)?;
        let values = self.effective_values(object_type, values)?;
        self.record_version(object_type, key, version)?;
        for table in self.model.tables(object_type)? {
            let mut row: (Vec<&str>, Vec<SqlValue>) = (Vec::new(), Vec::new());
            if table.is_root() {
                row.0.push(VERSION_COLUMN);
                row.1.push(SqlValue::Integer(to_i64(version, "version")?));
            }
            encode_row(
                object_type,
                &table.table,
                &table.columns,
                table.has_overflow,
                &mut row,
                |name| Ok(values.get(name).cloned().unwrap_or(Value::Null)),
            )?;
            if row.0.is_empty() {
                continue;
            }
            let assignments: Vec<String> = row
                .0
                .iter()
                .map(|column| format!("{} = ?", quote_identifier(column)))
                .collect();
            row.1.push(SqlValue::Text(self.world.to_owned()));
            row.1.push(SqlValue::Text(key.to_owned()));
            self.conn.execute(
                &format!(
                    "UPDATE {} SET {} WHERE {} = ? AND {} = ?",
                    quote_identifier(&table.table),
                    assignments.join(", "),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(ID_COLUMN),
                ),
                &row.1,
            )?;
        }
        self.persist_marking_snapshot(object_type, key, version)
    }

    /// Delete live object `key` (of `concrete` type, addressed as
    /// `object_type`): its rows and links cascade, and the hierarchy's version
    /// high-water mark moves one past its live version.
    fn delete(&mut self, object_type: &str, concrete: &str, key: &str) -> Result<(), CellError> {
        let version = next(
            self.reader().version(self.world, object_type, key)?,
            "version",
        )?;
        let tables = self.model.tables(concrete)?;
        let root = tables
            .first()
            .ok_or_else(|| CellError::UnknownObjectType(concrete.to_owned()))?;
        // Preserve the immutable classification of the deleted version before
        // clearing current state. Recreated keys inherit this exact tombstone.
        self.persist_marking_snapshot(concrete, key, version)?;
        // Subtype rows and links go with the root row (ON DELETE CASCADE).
        self.conn.execute(
            &format!(
                "DELETE FROM {} WHERE {} = ?1 AND {} = ?2",
                quote_identifier(&root.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            params![self.world, key],
        )?;
        self.record_version(concrete, key, version)?;
        self.clear_current_markings(concrete, key)
    }

    fn clear_current_markings(&self, concrete: &str, key: &str) -> Result<(), CellError> {
        let subject = marking_subject(self.conn, self.world, concrete, key)?;
        self.conn.execute(
            &format!(
                "DELETE FROM \"{MARKING_CURRENT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3"
            ),
            params![self.world, concrete, subject],
        )?;
        self.conn.execute(
            &format!(
                "DELETE FROM \"{MARKING_SNAPSHOT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"id\" = ?3 AND \"is_current\" = 1"
            ),
            params![self.world, concrete, key],
        )?;
        Ok(())
    }

    /// Record `version` as the highest version `key` has had in
    /// `object_type`'s hierarchy, preserving the row locator used by reads.
    fn record_version(&self, object_type: &str, key: &str, version: u64) -> Result<(), CellError> {
        let root = self.reader().root_type(object_type)?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{VERSIONS}\" (\"world\", \"root_type\", \"id\", \"version\") VALUES (?1, ?2, ?3, ?4) ON CONFLICT (\"world\", \"root_type\", \"id\") DO UPDATE SET \"version\" = excluded.\"version\""
            ),
            params![self.world, root, key, to_i64(version, "version")?],
        )?;
        Ok(())
    }

    /// Preserve the previous labels, add declarations from the effective
    /// model, and snapshot the result for this exact fact version. Labels are
    /// monotone on writes; only the dedicated declassification flow removes
    /// them from the current table.
    fn persist_marking_snapshot(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<(), CellError> {
        let subject = ensure_marking_subject(self.conn, self.world, object_type, key)?;
        let fact_version = version;
        let mut markings = self.current_markings(object_type, key)?;
        let declared_markings = self.model.effective_property_markings(object_type)?;

        // Declassification changes only current visibility. A later fact
        // write begins a new version and inherits the immutable labels of the
        // version it replaces, even if the old current snapshot was cleared.
        if let Some(previous_version) = version.checked_sub(1).filter(|previous| *previous != 0) {
            match self.previous_markings(object_type, key, previous_version)? {
                Some(previous) => {
                    for (property, labels) in previous {
                        if declared_markings.contains_key(&property) {
                            markings.entry(property).or_default().extend(labels);
                        }
                    }
                }
                None => {
                    // A missing historical snapshot cannot prove that the
                    // previous fact was unclassified. Keep uncertainty on all
                    // effective properties rather than laundering it away.
                    for property in declared_markings.keys() {
                        markings
                            .entry(property.clone())
                            .or_default()
                            .insert("norn.unknown-lineage".to_owned());
                    }
                }
            }
        }

        for (property, declared) in declared_markings {
            markings.entry(property).or_default().extend(declared);
        }
        // Explicitly record public occurrences too, so a missing slot is
        // distinguishable from a trusted empty classification.
        markings
            .entry(MARKING_OCCURRENCE.to_owned())
            .or_default()
            .extend(self.model.object_markings(object_type)?);
        if let Some(previous_version) = version.checked_sub(1).filter(|previous| *previous != 0) {
            let labels = self.previous_occurrence_markings(object_type, key, previous_version)?;
            if !labels.is_empty() {
                markings
                    .entry(MARKING_OCCURRENCE.to_owned())
                    .or_default()
                    .extend(labels);
            }
        }
        for (property, labels) in markings {
            let labels = labels.into_iter().collect::<Vec<_>>();
            let encoded = serde_json::to_string(&labels).map_err(CellError::Json)?;
            let version = to_i64(version, "marking snapshot version")?;
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{MARKING_CURRENT}\" (\"world\", \"object_type\", \"subject\", \"property\", \"version\", \"markings\") VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (\"world\", \"object_type\", \"subject\", \"property\") DO UPDATE SET \"version\" = excluded.\"version\", \"markings\" = excluded.\"markings\""
                ),
                params![self.world, object_type, subject, property, version, encoded],
            )?;
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{MARKING_VERSION}\" (\"world\", \"object_type\", \"subject\", \"property\", \"version\", \"markings\") VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                ),
                params![self.world, object_type, subject, property, version, encoded],
            )?;
        }
        let version = to_i64(version, "marking snapshot version")?;
        self.conn.execute(
            &format!(
                "DELETE FROM \"{MARKING_SNAPSHOT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"id\" = ?3 AND \"is_current\" = 1"
            ),
            params![self.world, object_type, key],
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{MARKING_SNAPSHOT}\" (\"world\", \"object_type\", \"id\", \"version\", \"is_current\") VALUES (?1, ?2, ?3, ?4, 1)"
            ),
            params![self.world, object_type, key, version],
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{MARKING_SNAPSHOT}\" (\"world\", \"object_type\", \"id\", \"version\", \"is_current\") VALUES (?1, ?2, ?3, ?4, 0)"
            ),
            params![self.world, object_type, key, version],
        )?;
        // Classify each exact version before another mutation can replace its
        // provenance. Both whole-object writes and canonical fact changes use
        // this boundary; later batch propagation only adds classifications.
        // A tombstone snapshot has no new live values. Reclassifying the
        // still-live prior row here would rewrite an older immutable version.
        if self
            .reader()
            .root_row(self.world, object_type, key)?
            .is_some_and(|(_, _, live_version)| live_version == fact_version)
        {
            self.apply_record_value_provenance_markings(object_type, key, true)?;
        }
        Ok(())
    }

    fn current_markings(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, CellError> {
        let subject = marking_subject(self.conn, self.world, object_type, key)?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"property\", \"markings\" FROM \"{MARKING_CURRENT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3 ORDER BY \"property\""
            ),
            params![self.world, object_type, subject],
        )?;
        rows.into_iter()
            .map(|row| {
                let row = SqlRow(row);
                let property: String = row.get(0)?;
                let encoded: String = row.get(1)?;
                let labels: Vec<String> =
                    serde_json::from_str(&encoded).map_err(CellError::Json)?;
                Ok((property, labels.into_iter().collect()))
            })
            .collect()
    }

    /// Apply trusted mapping provenance to the snapshots just created by this
    /// commit. The event is not scanned for arbitrary metadata: only exact
    /// `observation.recorded` mapped-property provenance for a `Put` in this
    /// batch can extend labels.
    fn apply_mapping_markings(&self, written: &[WrittenRow]) -> Result<(), CellError> {
        let targets: BTreeSet<(String, String)> = self
            .batch
            .writes
            .iter()
            .filter_map(|write| match write {
                FactWriteRow::Put {
                    object_type, key, ..
                } => Some((object_type.clone(), key.clone())),
                FactWriteRow::Delete { .. } => None,
            })
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        for event in &self.batch.events {
            if event.get("type").and_then(Value::as_str) != Some("observation.recorded") {
                continue;
            }
            let Some(mapped) = event.get("mapped").and_then(Value::as_array) else {
                continue;
            };
            for output in mapped {
                let Some(object_type) = output.get("objectType").and_then(Value::as_str) else {
                    continue;
                };
                let Some(key) = output.get("key").and_then(Value::as_str) else {
                    continue;
                };
                if !targets.contains(&(object_type.to_owned(), key.to_owned())) {
                    continue;
                }
                let Some(properties) = output.get("propertyProvenance").and_then(Value::as_object)
                else {
                    continue;
                };
                for (property, provenance) in properties {
                    let mut labels = match provenance.get("markings") {
                        None => BTreeSet::new(),
                        Some(Value::Array(labels)) => labels
                            .iter()
                            .map(|label| match label.as_str() {
                                Some(label) => Ok(label.to_owned()),
                                None => Err(()),
                            })
                            .collect::<Result<BTreeSet<_>, _>>()
                            .unwrap_or_else(|()| unknown_lineage()),
                        Some(_) => unknown_lineage(),
                    };
                    labels.extend(self.provenance_upstream_markings(provenance)?);
                    if labels.is_empty() {
                        continue;
                    }
                    for row in written
                        .iter()
                        .filter(|row| row.object_type == object_type && row.key == key)
                    {
                        self.extend_marking_snapshot(
                            object_type,
                            key,
                            property,
                            row.version,
                            &labels,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply only canonical typed instance provenance stored with ordinary
    /// fact values. Mapping event provenance is handled above; this path
    /// covers quality findings and other facts written with the same envelope.
    fn apply_fact_value_provenance_markings(
        &self,
        changed: &BTreeMap<(String, String), BTreeSet<String>>,
    ) -> Result<(), CellError> {
        for (object_type, key) in changed.keys() {
            self.apply_record_value_provenance_markings(object_type, key, false)?;
        }
        Ok(())
    }

    fn apply_record_value_provenance_markings(
        &self,
        object_type: &str,
        key: &str,
        pending_batch_lineage: bool,
    ) -> Result<(), CellError> {
        let Some(record) = self.reader().object(self.world, object_type, key)? else {
            return Ok(());
        };
        let Some(provenance) = record.values.get("provenance") else {
            return Ok(());
        };
        let Some(provenance) = self.decode_typed_provenance(provenance, pending_batch_lineage)?
        else {
            return Ok(());
        };
        // A Set of the envelope can classify values that were not themselves
        // changed. Instance provenance protects the complete record; the
        // per-property envelope additionally identifies its own targets.
        let mut targets = record.values.keys().cloned().collect::<BTreeSet<_>>();
        targets.extend(provenance.per_property.keys().cloned());
        for property in targets {
            let mut labels = provenance.base.clone();
            if let Some(property_labels) = provenance.per_property.get(&property) {
                labels.extend(property_labels.iter().cloned());
            }
            if !labels.is_empty() {
                self.extend_marking_snapshot(
                    &record.object_type,
                    key,
                    &property,
                    record.version,
                    &labels,
                )?;
            }
        }
        Ok(())
    }

    /// Decode a typed `InstanceProvenance` or per-property map stored in the
    /// fact's `provenance` value. Arbitrary JSON metadata is ignored;
    /// malformed objects that claim the typed envelope fail closed.
    fn decode_typed_provenance(
        &self,
        value: &Value,
        pending_batch_lineage: bool,
    ) -> Result<Option<TypedProvenanceMarkings>, CellError> {
        if let Some(values) = value.as_array() {
            let has_typed_entries = values.iter().any(|entry| {
                Self::looks_like_instance_provenance(entry)
                    || entry
                        .as_object()
                        .is_some_and(|object| object.contains_key("provenance"))
            });
            if has_typed_entries {
                let mut labels = BTreeSet::new();
                for entry in values {
                    let entry_labels = if Self::looks_like_instance_provenance(entry) {
                        self.instance_provenance_markings(entry, pending_batch_lineage)?
                    } else {
                        self.typed_lineage_entry_markings(entry, pending_batch_lineage)?
                    };
                    labels.extend(entry_labels);
                }
                return Ok(Some(TypedProvenanceMarkings {
                    base: labels,
                    per_property: BTreeMap::new(),
                }));
            }
        }
        if Self::looks_like_instance_provenance(value) {
            let base = self.instance_provenance_markings(value, pending_batch_lineage)?;
            let Some(object) = value.as_object() else {
                return Ok(Some(TypedProvenanceMarkings {
                    base: unknown_lineage(),
                    per_property: BTreeMap::new(),
                }));
            };
            let mut per_property = BTreeMap::new();
            if let Some(raw) = object.get("propertyProvenance") {
                let Some(entries) = raw.as_object() else {
                    return Ok(Some(TypedProvenanceMarkings {
                        base: unknown_lineage(),
                        per_property: BTreeMap::new(),
                    }));
                };
                for (property, provenance) in entries {
                    let labels = if Self::looks_like_instance_provenance(provenance) {
                        self.instance_provenance_markings(provenance, pending_batch_lineage)?
                    } else {
                        unknown_lineage()
                    };
                    per_property.insert(property.clone(), labels);
                }
            }
            return Ok(Some(TypedProvenanceMarkings { base, per_property }));
        }

        let Some(object) = value.as_object() else {
            return Ok(None);
        };
        let wrapped = if object.contains_key("propertyProvenance") {
            let Some(entries) = object.get("propertyProvenance").and_then(Value::as_object) else {
                return Ok(Some(TypedProvenanceMarkings {
                    base: unknown_lineage(),
                    per_property: BTreeMap::new(),
                }));
            };
            Some(entries)
        } else {
            None
        };
        let per_property = wrapped.or_else(|| {
            object
                .values()
                .any(Self::looks_like_instance_provenance)
                .then_some(object)
        });
        let Some(per_property) = per_property else {
            return Ok(None);
        };

        let mut labels = BTreeMap::new();
        for (property, provenance) in per_property {
            let property_labels = if Self::looks_like_instance_provenance(provenance) {
                self.instance_provenance_markings(provenance, pending_batch_lineage)?
            } else {
                unknown_lineage()
            };
            labels.insert(property.clone(), property_labels);
        }
        Ok(Some(TypedProvenanceMarkings {
            base: BTreeSet::new(),
            per_property: labels,
        }))
    }

    fn typed_lineage_entry_markings(
        &self,
        value: &Value,
        pending_batch_lineage: bool,
    ) -> Result<BTreeSet<String>, CellError> {
        let Some(object) = value.as_object() else {
            return Ok(unknown_lineage());
        };
        if object
            .keys()
            .any(|key| key != "provenance" && key != "observationIds")
        {
            return Ok(unknown_lineage());
        }
        if let Some(ids) = object.get("observationIds") {
            let Some(ids) = ids.as_array() else {
                return Ok(unknown_lineage());
            };
            if ids.iter().any(|id| id.as_str().is_none()) {
                return Ok(unknown_lineage());
            }
        }
        let Some(provenance) = object.get("provenance") else {
            return Ok(unknown_lineage());
        };
        if !Self::looks_like_instance_provenance(provenance) {
            return Ok(unknown_lineage());
        }
        self.instance_provenance_markings(provenance, pending_batch_lineage)
    }

    fn looks_like_instance_provenance(value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        object.contains_key("source")
            && (object.contains_key("mappingVersion")
                || object.contains_key("recordedAt")
                || object.contains_key("sourceFields")
                || object.contains_key("markings")
                || object.contains_key("upstream"))
    }

    fn instance_provenance_markings(
        &self,
        value: &Value,
        pending_batch_lineage: bool,
    ) -> Result<BTreeSet<String>, CellError> {
        let Some(object) = value.as_object() else {
            return Ok(unknown_lineage());
        };
        let mut legacy_fields = object.clone();
        let raw_markings = legacy_fields.remove("markings");
        let raw_source_fields = legacy_fields.remove("sourceFields");
        legacy_fields.remove("propertyProvenance");
        let legacy_value = Value::Object(legacy_fields);
        let Ok(provenance) = norn_ir::runtime::decode::<norn_ir::InstanceProvenance>(&legacy_value)
        else {
            return Ok(unknown_lineage());
        };

        let mut labels = BTreeSet::new();
        if let Some(markings) = raw_markings {
            let Some(markings) = markings.as_array() else {
                return Ok(unknown_lineage());
            };
            for marking in markings {
                let Some(marking) = marking.as_str() else {
                    return Ok(unknown_lineage());
                };
                labels.insert(marking.to_owned());
            }
        }
        if let Some(source_fields) = raw_source_fields {
            let Some(source_fields) = source_fields.as_array() else {
                return Ok(unknown_lineage());
            };
            if source_fields.iter().any(|field| field.as_str().is_none()) {
                return Ok(unknown_lineage());
            }
        }
        for upstream in provenance.upstream {
            // A same-batch output may still gain mapping/read/system labels
            // after this snapshot is persisted. Keep that uncertainty only
            // when the exact hierarchy identity and version are emitted in
            // this batch; an unrelated root with the same key or a preexisting
            // historical version is independently resolvable.
            if pending_batch_lineage {
                let root_type = self.reader().root_type(upstream.object_type.as_str())?;
                let identity = (
                    self.world.to_owned(),
                    root_type.to_owned(),
                    upstream.key.clone(),
                    upstream.version,
                );
                if self
                    .batch_output_facts
                    .as_ref()
                    .is_some_and(|outputs| outputs.contains(&identity))
                {
                    labels.extend(unknown_lineage());
                }
            }
            if upstream.version == 0 {
                labels.extend(unknown_lineage());
                continue;
            }
            labels.extend(self.labels_for_reference(
                upstream.object_type.as_str(),
                &upstream.key,
                upstream.version,
                false,
            )?);
        }
        Ok(labels)
    }

    /// Resolve the immutable classifications of the kernel's read set before
    /// this commit mutates any of its source objects.
    fn occurrence_lineage_markings(
        &self,
        inherited: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, CellError> {
        let mut labels = inherited.clone();
        if let Some(reads) = &self.batch.marking_occurrence_reads {
            labels.extend(self.read_lineage_markings(reads)?);
        }
        Ok(labels)
    }

    fn value_lineage_markings(&self) -> Result<DestinationMarkings, CellError> {
        let mut destinations = DestinationMarkings::new();
        for read in &self.batch.marking_value_reads {
            let root = self.reader().root_type(read.key.object_type.as_str())?;
            let destination = self.batch.writes.iter().any(|write| {
                let (object_type, key) = write.target();
                key == read.key.key.as_str()
                    && self
                        .reader()
                        .root_type(object_type)
                        .is_ok_and(|actual| actual == root)
            });
            if !destination
                || read.expected.0 == 0
                || self.reader().version(
                    self.world,
                    read.key.object_type.as_str(),
                    &read.key.key,
                )? != read.expected.0
            {
                return Err(CellError::HistoricalValueUnproven(
                    "value-only lineage must name an exact current written destination".to_owned(),
                ));
            }
            destinations
                .entry((root.to_owned(), read.key.key.clone()))
                .or_default()
                .extend(self.labels_for_reference(
                    read.key.object_type.as_str(),
                    &read.key.key,
                    read.expected.0,
                    true,
                )?);
        }
        Ok(destinations)
    }

    // A destination CAS preserves that destination's old values. It is not
    // a value read by sibling outputs (for example an atomic protocol cursor).
    // Explicit decision reads remain in `inherited` for every emitted output.
    fn destination_value_lineage(
        &self,
        inherited: &BTreeSet<String>,
        destinations: &DestinationMarkings,
        object_type: &str,
        key: &str,
    ) -> Result<BTreeSet<String>, CellError> {
        let mut labels = inherited.clone();
        let root = self.reader().root_type(object_type)?;
        if let Some(destination) = destinations.get(&(root.to_owned(), key.to_owned())) {
            labels.extend(destination.iter().cloned());
        }
        Ok(labels)
    }

    fn read_lineage_markings(
        &self,
        reads: &[norn_kernel::FactExpectation],
    ) -> Result<BTreeSet<String>, CellError> {
        self.read_lineage_markings_at(reads, true)
    }

    fn reference_lineage_markings(
        &self,
        read: &norn_kernel::FactExpectation,
    ) -> Result<BTreeSet<String>, CellError> {
        let object_type = read.key.object_type.as_str();
        let version = Some(read.expected.0);
        let reader = self.reader();
        let mut labels = reader
            .fact_occurrence_markings(self.world, object_type, &read.key.key, version)?
            .unwrap_or_else(unknown_lineage);
        // Identity components can themselves be classified. A reference uses
        // that identity even when it never reads contact or business values.
        let Some(snapshot) =
            reader.effective_markings(self.world, object_type, &read.key.key, version)?
        else {
            labels.extend(unknown_lineage());
            return Ok(labels);
        };
        let mut properties = self.model.identity_properties(object_type)?;
        if properties.is_empty() {
            // Without declared identity components, every field can contribute
            // to the opaque key. Match the read-side identity visibility rule.
            properties.extend(snapshot.keys().cloned());
        }
        for property in properties {
            labels.extend(
                snapshot
                    .get(&property)
                    .cloned()
                    .map_or_else(unknown_lineage, |marks| marks.into_iter().collect()),
            );
        }
        Ok(labels)
    }

    fn property_lineage_markings(
        &self,
        read: &norn_kernel::FactExpectation,
        properties: &[String],
    ) -> Result<BTreeSet<String>, CellError> {
        if read.expected.0 == 0 {
            return Ok(unknown_lineage());
        }
        let mut labels = self.reference_lineage_markings(read)?;
        let reader = self.reader();
        let object_type = read.key.object_type.as_str();
        let version = if reader.version(self.world, object_type, &read.key.key)? == read.expected.0
        {
            None
        } else {
            Some(read.expected.0)
        };
        let Some(snapshot) =
            reader.effective_markings(self.world, object_type, &read.key.key, version)?
        else {
            labels.extend(unknown_lineage());
            return Ok(labels);
        };
        for property in properties {
            labels.extend(
                snapshot
                    .get(property)
                    .cloned()
                    .unwrap_or_else(|| vec!["norn.unknown-lineage".to_owned()]),
            );
        }
        Ok(labels)
    }

    fn read_lineage_markings_at(
        &self,
        reads: &[norn_kernel::FactExpectation],
        current_if_exact: bool,
    ) -> Result<BTreeSet<String>, CellError> {
        let mut inherited = BTreeSet::new();
        for seq in &self.batch.marking_events {
            let entry = seq
                .checked_sub(1)
                .map(|after| self.reader().ledger_page(after, 1))
                .transpose()?
                .and_then(|entries| entries.into_iter().find(|entry| entry.seq == *seq));
            let labels = match entry {
                Some(entry) if entry.world == self.world => {
                    self.reader().fact_event_markings(&entry)?
                }
                _ => None,
            };
            inherited.extend(labels.unwrap_or_else(unknown_lineage));
        }
        for read in reads {
            let version = read.expected.0;
            if version == 0 {
                continue;
            }
            inherited.extend(self.labels_for_reference(
                read.key.object_type.as_str(),
                &read.key.key,
                version,
                current_if_exact,
            )?);
        }
        Ok(inherited)
    }

    /// Apply read/event labels to one emitted snapshot. A deletion
    /// still exposes its occurrence, so its complete snapshot inherits
    /// the decision inputs even though no stored value property changed.
    fn apply_read_lineage_to_version(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
        properties: &BTreeSet<String>,
        inherited: &BTreeSet<String>,
    ) -> Result<(), CellError> {
        if inherited.is_empty() {
            return Ok(());
        }
        let all_properties;
        let properties = if properties.is_empty() {
            all_properties = self
                .model
                .effective_property_markings(object_type)?
                .into_keys()
                .collect::<BTreeSet<_>>();
            &all_properties
        } else {
            properties
        };
        if properties.is_empty() {
            return self.apply_occurrence_lineage_to_version(object_type, key, version, inherited);
        }
        for property in properties {
            self.extend_marking_snapshot(object_type, key, property, version, inherited)?;
        }
        Ok(())
    }

    fn apply_occurrence_lineage_to_version(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
        inherited: &BTreeSet<String>,
    ) -> Result<(), CellError> {
        if inherited.is_empty() {
            return Ok(());
        }
        let mut labels = self
            .reader()
            .fact_occurrence_markings(self.world, object_type, key, Some(version))?
            .ok_or_else(|| {
                CellError::Engine("emitted version lacks complete marking snapshot".to_owned())
            })?;
        labels.extend(inherited.iter().cloned());
        let subject = marking_subject(self.conn, self.world, object_type, key)?;
        let encoded = serde_json::to_string(&labels).map_err(CellError::Json)?;
        let version = to_i64(version, "marking snapshot version")?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{MARKING_VERSION}\" (\"world\", \"object_type\", \"subject\", \"property\", \"version\", \"markings\") VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (\"world\", \"object_type\", \"subject\", \"property\", \"version\") DO UPDATE SET \"markings\" = excluded.\"markings\""
            ),
            params![self.world, object_type, subject, MARKING_OCCURRENCE, version, encoded],
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{MARKING_CURRENT}\" (\"world\", \"object_type\", \"subject\", \"property\", \"version\", \"markings\") VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (\"world\", \"object_type\", \"subject\", \"property\") DO UPDATE SET \"version\" = excluded.\"version\", \"markings\" = excluded.\"markings\""
            ),
            params![self.world, object_type, subject, MARKING_OCCURRENCE, version, encoded],
        )?;
        Ok(())
    }

    fn previous_occurrence_markings(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<BTreeSet<String>, CellError> {
        if let Some(labels) =
            self.reader()
                .fact_occurrence_markings(self.world, object_type, key, Some(version))?
        {
            return Ok(labels);
        }
        let Some(previous_type) =
            self.retained_snapshot_type(self.reader().root_type(object_type)?, key, version)?
        else {
            return Ok(unknown_lineage());
        };
        Ok(self
            .reader()
            .fact_occurrence_markings(self.world, &previous_type, key, Some(version))?
            .unwrap_or_else(unknown_lineage))
    }

    fn materialized_property_value(
        &self,
        object_type: &str,
        property: &str,
        value: &Value,
    ) -> Result<Value, CellError> {
        let column = self.model.column(object_type, property)?;
        let encoded = encode(object_type, column.column, value)?;
        crate::codec::decode_stored_value(
            crate::codec::DecodeContext {
                model: self.model,
                owner: ValueOwner::Object { object_type },
            },
            &column.table.table,
            column.column,
            &encoded,
        )
    }

    fn write_changed_properties(
        &self,
        write: &FactWriteRow,
    ) -> Result<BTreeSet<String>, CellError> {
        let FactWriteRow::Put {
            object_type,
            key,
            values,
        } = write
        else {
            return Ok(BTreeSet::new());
        };
        let incoming = self
            .effective_values(object_type, values)?
            .into_iter()
            .map(|(property, value)| {
                self.materialized_property_value(object_type, &property, &value)
                    .map(|value| (property, value))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let existing = self.reader().object(self.world, object_type, key)?;
        Ok(incoming
            .iter()
            .filter(|(property, value)| {
                match existing
                    .as_ref()
                    .and_then(|record| record.values.get(*property))
                {
                    Some(previous) => previous != *value,
                    None => !value.is_null(),
                }
            })
            .map(|(property, _)| property.clone())
            .collect())
    }

    fn fact_target(fact: &FactChange) -> (&str, &str) {
        match fact {
            FactChange::Create {
                object_type, key, ..
            }
            | FactChange::Set {
                object_type, key, ..
            }
            | FactChange::Delete { object_type, key }
            | FactChange::Transition {
                object_type, key, ..
            } => (object_type.as_str(), key),
        }
    }

    fn fact_changed_properties(&self, fact: &FactChange) -> Result<BTreeSet<String>, CellError> {
        match fact {
            FactChange::Create {
                object_type,
                values,
                ..
            } => Ok(self
                .effective_values(object_type.as_str(), values)?
                .into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(property, _)| property)
                .collect()),
            FactChange::Set {
                object_type,
                key,
                property,
                value,
            } => {
                let existing = self
                    .reader()
                    .object(self.world, object_type.as_str(), key)?;
                let incoming = self.materialized_property_value(
                    object_type.as_str(),
                    property.as_str(),
                    value,
                )?;
                if existing
                    .as_ref()
                    .and_then(|row| row.values.get(property.as_str()))
                    == Some(&incoming)
                {
                    Ok(BTreeSet::new())
                } else {
                    Ok(BTreeSet::from([property.0.clone()]))
                }
            }
            FactChange::Transition { lifecycle, .. } => Ok(BTreeSet::from([lifecycle.0.clone()])),
            FactChange::Delete { .. } => Ok(BTreeSet::new()),
        }
    }

    fn record_changed_properties(
        changed: &mut BTreeMap<(String, String), BTreeSet<String>>,
        object_type: &str,
        key: &str,
        properties: &BTreeSet<String>,
    ) {
        if !properties.is_empty() {
            changed
                .entry((object_type.to_owned(), key.to_owned()))
                .or_default()
                .extend(properties.iter().cloned());
        }
    }

    /// Calculate every exact object version this commit will emit before any
    /// mutation runs. Fact references can then distinguish future same-batch
    /// outputs from valid preexisting historical versions.
    fn emitted_fact_versions(&self) -> Result<BTreeSet<(String, String, String, u64)>, CellError> {
        let mut versions = BTreeMap::<(String, String), u64>::new();
        let mut emitted = BTreeSet::new();
        let mut record = |object_type: &str, key: &str| -> Result<(), CellError> {
            let root_type = self.reader().root_type(object_type)?.to_owned();
            let identity = (root_type.clone(), key.to_owned());
            let current = match versions.get(&identity) {
                Some(current) => *current,
                None => self.reader().high_water(self.world, object_type, key)?,
            };
            let version = next(current, "version")?;
            versions.insert(identity, version);
            emitted.insert((self.world.to_owned(), root_type, key.to_owned(), version));
            Ok(())
        };
        for write in &self.batch.writes {
            record(write.target().0, write.target().1)?;
        }
        for fact in &self.batch.facts {
            let (object_type, key) = Self::fact_target(fact);
            record(object_type, key)?;
        }
        Ok(emitted)
    }

    fn labels_for_reference(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
        current_if_exact: bool,
    ) -> Result<BTreeSet<String>, CellError> {
        let reader = self.reader();
        let concrete_type = reader
            .root_row(self.world, object_type, key)?
            .map_or_else(|| object_type.to_owned(), |(concrete, _, _)| concrete);
        let actual = reader.version(self.world, &concrete_type, key)?;
        let selected_version = if current_if_exact && actual == version {
            None
        } else {
            Some(version)
        };
        let Some(snapshot) =
            reader.effective_markings(self.world, &concrete_type, key, selected_version)?
        else {
            return Ok(unknown_lineage());
        };
        let mut labels = snapshot.into_values().flatten().collect::<BTreeSet<_>>();
        labels.extend(
            reader
                .fact_occurrence_markings(self.world, &concrete_type, key, selected_version)?
                .unwrap_or_else(unknown_lineage),
        );
        Ok(labels)
    }

    fn provenance_upstream_markings(
        &self,
        provenance: &Value,
    ) -> Result<BTreeSet<String>, CellError> {
        let Some(upstream) = provenance.get("upstream") else {
            return Ok(BTreeSet::new());
        };
        let Some(upstream) = upstream.as_array() else {
            return Ok(unknown_lineage());
        };
        let mut markings = BTreeSet::new();
        for fact in upstream {
            let Some(object_type) = fact.get("objectType").and_then(Value::as_str) else {
                markings.extend(unknown_lineage());
                continue;
            };
            let Some(key) = fact.get("key").and_then(Value::as_str) else {
                markings.extend(unknown_lineage());
                continue;
            };
            let version = fact
                .get("version")
                .and_then(Value::as_i64)
                .and_then(|version| u64::try_from(version).ok());
            let Some(version) = version.filter(|version| *version > 0) else {
                markings.extend(unknown_lineage());
                continue;
            };
            // A FactRef names immutable lineage, so preserve the exact
            // historical snapshot even when a later declassification changed
            // the source object's current projection.
            markings.extend(self.labels_for_reference(object_type, key, version, false)?);
        }
        Ok(markings)
    }

    /// Carry runtime mapping labels through computed properties using the
    /// checker-owned AST dependency metadata. This keeps cell code independent
    /// of expression parsing and fails closed for missing dependency rows.
    fn propagate_computed_markings(&self, object_type: &str, key: &str) -> Result<(), CellError> {
        let Some(concrete) = self.live(object_type, key)? else {
            return Ok(());
        };
        let object_type = concrete.as_str();
        let version = self.reader().version(self.world, object_type, key)?;
        self.propagate_computed_markings_at(object_type, key, version)
    }

    fn propagate_computed_markings_at(
        &self,
        object_type: &str,
        key: &str,
        version: u64,
    ) -> Result<(), CellError> {
        let dependencies = self.model.computed_property_inputs(object_type)?;
        if dependencies.is_empty() {
            return Ok(());
        }
        if version == 0 {
            return Err(CellError::ObjectMissing {
                object_type: object_type.to_owned(),
                id: key.to_owned(),
                world: self.world.to_owned(),
            });
        }
        let initial = self
            .reader()
            .effective_markings(self.world, object_type, key, Some(version))?
            .ok_or_else(|| {
                CellError::Engine("emitted version lacks complete marking snapshot".to_owned())
            })?
            .into_iter()
            .map(|(property, labels)| (property, labels.into_iter().collect::<BTreeSet<_>>()))
            .collect::<BTreeMap<_, _>>();
        let mut effective = initial.clone();
        let changed_properties = self
            .model
            .close_computed_markings(object_type, &mut effective)?;
        for property in changed_properties {
            let additions = effective
                .get(&property)
                .into_iter()
                .flatten()
                .filter(|marking| {
                    !initial
                        .get(&property)
                        .is_some_and(|labels| labels.contains(*marking))
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            if !additions.is_empty() {
                self.extend_marking_snapshot(object_type, key, &property, version, &additions)?;
            }
        }
        Ok(())
    }

    /// System records can contain raw JSON payloads and therefore need
    /// subject lineage even though their fixed schemas have no tenant
    /// markings. Action calls inherit target labels at enqueue; command tasks
    /// inherit the attributed call's immutable snapshot. Unscoped rule and
    /// observation detail is unknown.
    fn capture_value_commitment(&self, row: &WrittenRow) -> Result<(), CellError> {
        let root = self.reader().root_type(&row.object_type)?.to_owned();
        let record = self
            .reader()
            .object(self.world, &row.object_type, &row.key)?;
        let snapshot = crate::history::FactSnapshot {
            world: self.world.to_owned(),
            root_type: root.clone(),
            object_type: row.object_type.clone(),
            lineage: self.model.chain(&row.object_type)?.to_vec(),
            model_hash: self.model.model_hash().to_owned(),
            key: row.key.clone(),
            scope: match &record {
                Some(record) => record.scope.clone(),
                None if row.version == 1 => self.batch.scope.clone(),
                None => self.reader().retained_fact_scope(
                    self.world,
                    &root,
                    &row.key,
                    row.version.saturating_sub(1),
                )?,
            },
            version: row.version,
            values: record.map(|record| record.values),
        };
        let hash = hash_json(&snapshot)?;
        self.conn.execute(&format!("INSERT INTO \"{}\" (\"world\", \"root_type\", \"object_type\", \"id\", \"version\", \"commit_seq\", \"snapshot\", \"hash\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", crate::system::FACT_VALUES), params![self.world, root, row.object_type, row.key, to_i64(row.version, "fact version")?, self.commit_seq_sql()?, canonical_json(&snapshot)?, hash])?;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the explicit system-record classification rules are reviewed together at the durable trust boundary"
    )]
    fn apply_system_lineage_markings(&self, records: &[ObjectRecord]) -> Result<(), CellError> {
        // Caller write order does not impose dependency order. Classify
        // source metadata first, then calls, then their attributed tasks.
        let sources = records.iter().filter(|record| {
            record.object_type != norn_kernel::ACTION_CALL_TYPE
                && record.object_type != "norn.CommandTask"
        });
        let calls = records
            .iter()
            .filter(|record| record.object_type == norn_kernel::ACTION_CALL_TYPE);
        let tasks = records
            .iter()
            .filter(|record| record.object_type == "norn.CommandTask");
        for record in sources.chain(calls).chain(tasks) {
            let object_type = &record.object_type;
            let key = &record.id;
            if !(object_type == norn_kernel::ACTION_CALL_TYPE
                || object_type == "norn.CommandTask"
                || object_type == "norn.RuleRun"
                || object_type == "norn.ObservationHead"
                || object_type == norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE
                || object_type == "norn.ConnectorPollState"
                || object_type == "norn.ConnectorPollApplyCursor"
                || object_type == "norn.ConnectorPollConflict"
                || object_type == "norn.ScimResource"
                || object_type == "norn.FunctionCommand"
                || object_type == "norn.SweepCursor"
                || object_type == "norn.PendingLinks"
                || object_type == norn_kernel::NAMING_SERIES_COUNTER_TYPE)
            {
                continue;
            }
            let labels = match object_type.as_str() {
                norn_kernel::ACTION_CALL_TYPE => record
                    .values
                    .get("object")
                    .and_then(Value::as_object)
                    .and_then(|object| {
                        Some((
                            object.get("objectType")?.as_str()?,
                            object.get("key")?.as_str()?,
                        ))
                    })
                    .map(|(target_type, target_key)| self.target_markings(target_type, target_key))
                    .transpose()?
                    .unwrap_or_else(unknown_lineage),
                "norn.CommandTask" => self.command_task_markings(record)?,
                // A trigger/read set authenticates its own inputs, not arbitrary
                // caller-supplied siblings in the complete RuleRun detail.
                "norn.RuleRun" => unknown_lineage(),
                "norn.FunctionCommand" => {
                    // Reconciliation state contains only typed generation,
                    // count and receipt metadata. Dispatch and result inputs
                    // have already classified it in this transaction.
                    // Unattributed or malformed state remains concealed.
                    let state = record.values.get("state").and_then(|state| {
                        norn_ir::runtime::decode::<
                            norn_kernel::result_reconciliation::CommandState,
                        >(state)
                        .ok()
                    });
                    let dispatch = state.as_ref().is_some_and(|state| {
                        if state.cancelled || state.accepted.is_some() {
                            return false;
                        }
                        self.batch.effects.iter().any(|effect| {
                            let EffectRow::Enqueue { command } = effect else {
                                return false;
                            };
                            command.command_id == record.id
                                && command.generation == state.generation.0
                                && command
                                    .command
                                    .get("payload")
                                    .and_then(|payload| payload.get("type"))
                                    .and_then(Value::as_str)
                                    == Some("norn.function/v1")
                        })
                    });
                    if state.is_some()
                        && (!self.batch.marking_reads.is_empty()
                            || !self.batch.marking_historical_reads.is_empty()
                            || dispatch)
                    {
                        BTreeSet::new()
                    } else {
                        unknown_lineage()
                    }
                }
                norn_kernel::NAMING_SERIES_COUNTER_TYPE => {
                    self.naming_counter_markings(key, record)?
                }
                norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE => {
                    // New mapping deletions carry typed source-field and
                    // marking provenance. Their attached summary also has a
                    // full-catalog reveal guard; generic tombstone object
                    // reads are hidden by the shared API projection because
                    // the synthetic key itself embeds the subject identity.
                    let provenance = record.values.get("provenance").and_then(Value::as_object);
                    match provenance {
                        Some(provenance)
                            if provenance.contains_key("sourceFields")
                                || provenance.contains_key("markings") =>
                        {
                            match provenance.get("markings") {
                                None => BTreeSet::new(),
                                Some(Value::Array(values)) => {
                                    let mut labels = BTreeSet::new();
                                    for value in values {
                                        let Some(label) = value.as_str() else {
                                            labels.extend(unknown_lineage());
                                            break;
                                        };
                                        labels.insert(label.to_owned());
                                    }
                                    labels
                                }
                                Some(_) => unknown_lineage(),
                            }
                        }
                        Some(_) => unknown_lineage(),
                        // A legacy untyped provenance has no marker evidence;
                        // the attached summary still requires reveal of every
                        // catalogued marking before it is returned.
                        None => BTreeSet::new(),
                    }
                }
                // Retained observation and staging payloads contain source
                // JSON or identifiers without complete field attribution.
                "norn.ObservationHead"
                | "norn.ConnectorPollState"
                | "norn.ConnectorPollApplyCursor"
                | "norn.ConnectorPollConflict"
                | "norn.ScimResource"
                | "norn.SweepCursor"
                | "norn.PendingLinks" => unknown_lineage(),
                _ => BTreeSet::new(),
            };
            let version = record.version;
            for property in record.values.keys() {
                if !labels.is_empty() {
                    self.extend_marking_snapshot(object_type, key, property, version, &labels)?;
                }
            }
            self.close_emitted_markings(&[WrittenRow {
                object_type: object_type.clone(),
                key: key.clone(),
                version,
            }])?;
        }
        Ok(())
    }

    fn naming_counter_markings(
        &self,
        counter_key: &str,
        record: &crate::read::ObjectRecord,
    ) -> Result<BTreeSet<String>, CellError> {
        let Some(attribution) = naming_counter_attribution(record) else {
            return Ok(unknown_lineage());
        };
        let Some(mut labels) = self.model.naming_counter_markings(
            &attribution.object_type,
            &attribution.key_name,
            &attribution.generated_property,
            &attribution.declaration,
        ) else {
            return Ok(unknown_lineage());
        };
        let mut matched_event = false;
        for event in &self.batch.events {
            if naming_counter_seed_matches(event, counter_key, &attribution) {
                matched_event = true;
                continue;
            }
            let Ok(event) = norn_ir::runtime::decode::<norn_kernel::FactEvent>(event) else {
                continue;
            };
            if let Some(target_labels) =
                self.naming_counter_allocation_target_markings(counter_key, &attribution, &event)?
            {
                matched_event = true;
                labels.extend(target_labels);
            }
        }
        if matched_event {
            Ok(labels)
        } else {
            Ok(unknown_lineage())
        }
    }

    fn naming_counter_allocation_target_markings(
        &self,
        counter_key: &str,
        attribution: &NamingCounterAttribution,
        event: &norn_kernel::FactEvent,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        let norn_kernel::FactEvent::NamingSeriesAllocated {
            key,
            counter,
            pattern,
            timezone,
            scope,
            period,
            value,
            tzdb_version,
            reset_policy,
            gap_policy,
            ..
        } = event
        else {
            return Ok(None);
        };
        let target_type = key.object_type.as_str();
        let target_key = key.key.as_str();
        if counter.object_type.as_str() != norn_kernel::NAMING_SERIES_COUNTER_TYPE
            || counter.key != counter_key
            || !self.model.is_a(target_type, &attribution.object_type)
            || pattern != &attribution.declaration.pattern
            || timezone != &attribution.declaration.timezone
            || tzdb_version != &attribution.tzdb_version
            || scope != &attribution.scope
            || period != &attribution.period
            || *value != attribution.current
            || *reset_policy != attribution.declaration.reset_policy
            || *gap_policy != attribution.declaration.gap_policy
        {
            return Ok(None);
        }
        let target_was_written = self.batch.writes.iter().any(|write| {
            matches!(write, FactWriteRow::Put { object_type, key, .. }
                if object_type == target_type && key == target_key)
        });
        if !target_was_written {
            return Ok(None);
        }
        let Some(target) = self.reader().object(self.world, target_type, target_key)? else {
            return Ok(None);
        };
        if !self.model.is_a(&target.object_type, target_type)
            || target
                .values
                .get(&attribution.generated_property)
                .and_then(Value::as_str)
                != Some(target_key)
        {
            return Ok(None);
        }
        let target_scope = attribution.declaration.scope_fields.iter().try_fold(
            BTreeMap::new(),
            |mut scope_values, property| {
                let value = target.values.get(property.as_str())?.as_str()?;
                scope_values.insert(property.clone(), value.to_owned());
                Some(scope_values)
            },
        );
        if target_scope.as_ref() != Some(scope) {
            return Ok(None);
        }
        self.target_markings(target_type, target_key).map(Some)
    }

    fn target_markings(&self, object_type: &str, key: &str) -> Result<BTreeSet<String>, CellError> {
        let concrete = match self.live(object_type, key) {
            Ok(Some(concrete)) => concrete,
            Ok(None) | Err(CellError::UnknownObjectType(_)) => return Ok(unknown_lineage()),
            Err(error) => return Err(error),
        };
        match self
            .reader()
            .effective_markings(self.world, &concrete, key, None)
        {
            Ok(Some(snapshot)) => {
                let mut labels = snapshot.into_values().flatten().collect::<BTreeSet<_>>();
                labels.extend(
                    self.reader()
                        .fact_occurrence_markings(self.world, &concrete, key, None)?
                        .unwrap_or_else(unknown_lineage),
                );
                Ok(labels)
            }
            Ok(None) | Err(CellError::UnknownObjectType(_)) => Ok(unknown_lineage()),
            Err(error) => Err(error),
        }
    }

    fn command_task_markings(
        &self,
        task: &crate::read::ObjectRecord,
    ) -> Result<BTreeSet<String>, CellError> {
        let Some(true) = task.values.get("attributed").and_then(Value::as_bool) else {
            return Ok(unknown_lineage());
        };
        let Some(action_call_id) = task
            .values
            .get("actionCall")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(unknown_lineage());
        };
        let Some(action_id) = task
            .values
            .get("actionId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(unknown_lineage());
        };
        let Some(target_type) = task
            .values
            .get("objectType")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(unknown_lineage());
        };
        let Some(target_key) = task
            .values
            .get("objectKey")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(unknown_lineage());
        };
        let source =
            match self
                .reader()
                .object(self.world, norn_kernel::ACTION_CALL_TYPE, action_call_id)
            {
                Ok(Some(source)) => source,
                Ok(None) | Err(CellError::UnknownObjectType(_)) => return Ok(unknown_lineage()),
                Err(error) => return Err(error),
            };
        let source_target = source
            .values
            .get("object")
            .and_then(Value::as_object)
            .and_then(|object| {
                Some((
                    object.get("objectType")?.as_str()?,
                    object.get("key")?.as_str()?,
                ))
            });
        if source.values.get("actionId").and_then(Value::as_str) != Some(action_id)
            || source_target != Some((target_type, target_key))
        {
            return Ok(unknown_lineage());
        }
        // The command task may be written after the target's current labels
        // change. Pin the ActionCall fact version and read its immutable
        // snapshot rather than looking through to the target again.
        self.labels_for_reference(
            norn_kernel::ACTION_CALL_TYPE,
            action_call_id,
            source.version,
            false,
        )
    }

    fn extend_marking_snapshot(
        &self,
        object_type: &str,
        key: &str,
        property: &str,
        version: u64,
        additions: &BTreeSet<String>,
    ) -> Result<(), CellError> {
        let subject = marking_subject(self.conn, self.world, object_type, key)?;
        let current: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT \"markings\" FROM \"{MARKING_VERSION}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3 AND \"property\" = ?4 AND \"version\" = ?5"
            ),
            params![self.world, object_type, subject, property, to_i64(version, "marking snapshot version")?],
            |row| row.get(0),
        )?;
        let mut labels: BTreeSet<String> = current
            .map(|encoded| serde_json::from_str(&encoded).map_err(CellError::Json))
            .transpose()?
            .ok_or_else(|| {
                CellError::MarkingDeclassificationInvalid(format!(
                    "mapping provenance names unknown property `{object_type}.{property}`"
                ))
            })?;
        labels.extend(additions.iter().cloned());
        let labels = labels.into_iter().collect::<Vec<_>>();
        let encoded = serde_json::to_string(&labels).map_err(CellError::Json)?;
        let version = to_i64(version, "marking snapshot version")?;
        self.conn.execute(
            &format!(
                "UPDATE \"{MARKING_CURRENT}\" SET \"markings\" = ?1 WHERE \"world\" = ?2 AND \"object_type\" = ?3 AND \"subject\" = ?4 AND \"property\" = ?5 AND \"version\" = ?6"
            ),
            params![encoded, self.world, object_type, subject, property, version],
        )?;
        self.conn.execute(
            &format!(
                "UPDATE \"{MARKING_VERSION}\" SET \"markings\" = ?1 WHERE \"world\" = ?2 AND \"object_type\" = ?3 AND \"subject\" = ?4 AND \"property\" = ?5 AND \"version\" = ?6"
            ),
            params![encoded, self.world, object_type, subject, property, version],
        )?;
        Ok(())
    }

    fn apply_marking_declassifications(&self) -> Result<(), CellError> {
        let canonical_events = self
            .batch
            .marking_declassifications
            .iter()
            .map(crate::store::MarkingDeclassification::ledger_event)
            .collect::<Result<Vec<_>, _>>()?;
        let mut identities = BTreeSet::new();
        for change in &self.batch.marking_declassifications {
            let root = self.reader().root_type(&change.object_type)?;
            if !identities.insert((
                root.to_owned(),
                &change.key,
                &change.property,
                change.expected_version,
            )) {
                return Err(CellError::MarkingDeclassificationInvalid(
                    "a declassification event does not match an authorized typed change one-to-one"
                        .to_owned(),
                ));
            }
        }
        let audit_events = self
            .batch
            .events
            .iter()
            .filter(|event| {
                event.get("type").and_then(Value::as_str) == Some("marking.declassified")
            })
            .collect::<Vec<_>>();
        if audit_events.len() != canonical_events.len()
            || canonical_events.iter().any(|event| {
                audit_events
                    .iter()
                    .filter(|actual| **actual == event)
                    .count()
                    != 1
            })
        {
            return Err(CellError::MarkingDeclassificationInvalid(
                "a declassification event does not match an authorized typed change one-to-one"
                    .to_owned(),
            ));
        }
        for (change, event) in self
            .batch
            .marking_declassifications
            .iter()
            .zip(&canonical_events)
        {
            self.apply_marking_declassification(change, event)?;
        }
        Ok(())
    }

    fn apply_marking_declassification(
        &self,
        change: &crate::store::MarkingDeclassification,
        event: &Value,
    ) -> Result<(), CellError> {
        if change.property == MARKING_OCCURRENCE {
            return Err(CellError::MarkingDeclassificationInvalid(
                "fact occurrence lineage is not a property and cannot be declassified".to_owned(),
            ));
        }
        if change.reason.trim().is_empty()
            || change.actor.trim().is_empty()
            || change.markings.is_empty()
            || change
                .markings
                .iter()
                .any(|marking| marking == "norn.unknown-lineage")
        {
            return Err(CellError::MarkingDeclassificationInvalid(
                "actor, reason, and declared markings are required; unknown lineage cannot be declassified".to_owned(),
            ));
        }
        if !self.batch.events.contains(event) {
            return Err(CellError::MarkingDeclassificationInvalid(
                "the canonical declassification event is missing from this commit".to_owned(),
            ));
        }
        let actual = self
            .reader()
            .version(self.world, &change.object_type, &change.key)?;
        if actual != change.expected_version {
            return Err(CellError::MarkingVersionConflict {
                object_type: change.object_type.clone(),
                key: change.key.clone(),
                expected: change.expected_version,
                actual: (actual != 0).then_some(actual),
            });
        }
        let subject = marking_subject(self.conn, self.world, &change.object_type, &change.key)?;
        let current: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT \"markings\" FROM \"{MARKING_CURRENT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3 AND \"property\" = ?4 AND \"version\" = ?5"
            ),
            params![
                self.world,
                change.object_type,
                subject,
                change.property,
                to_i64(change.expected_version, "marking snapshot version")?,
            ],
            |row| row.get(0),
        )?;
        let mut labels: BTreeSet<String> = current
            .map(|encoded| serde_json::from_str(&encoded).map_err(CellError::Json))
            .transpose()?
            .ok_or_else(|| {
                CellError::MarkingDeclassificationInvalid(
                    "the current property marking snapshot is unavailable".to_owned(),
                )
            })?;
        let distinct: BTreeSet<_> = change.markings.iter().collect();
        if distinct.len() != change.markings.len()
            || change
                .markings
                .iter()
                .any(|marking| !labels.contains(marking))
        {
            return Err(CellError::MarkingDeclassificationInvalid(
                "a requested marking is duplicated or is not effective on the property".to_owned(),
            ));
        }
        for marking in &change.markings {
            labels.remove(marking);
        }
        let labels = serde_json::to_string(&labels.into_iter().collect::<Vec<_>>())
            .map_err(CellError::Json)?;
        self.conn.execute(
            &format!(
                "UPDATE \"{MARKING_CURRENT}\" SET \"markings\" = ?1 WHERE \"world\" = ?2 AND \"object_type\" = ?3 AND \"subject\" = ?4 AND \"property\" = ?5 AND \"version\" = ?6"
            ),
            params![
                labels,
                self.world,
                change.object_type,
                subject,
                change.property,
                to_i64(change.expected_version, "marking snapshot version")?,
            ],
        )?;
        Ok(())
    }

    fn set(
        &mut self,
        object_type: &str,
        key: &str,
        property: &str,
        value: &Value,
    ) -> Result<(), CellError> {
        let concrete = self.existing(object_type, key)?;
        if self.model.lifecycle(&concrete, property)?.is_some() {
            return Err(CellError::LifecycleWrite {
                object_type: object_type.to_owned(),
                property: property.to_owned(),
            });
        }
        let column = self.model.column(object_type, property)?;
        self.write_value(&concrete, column, key, value)?;
        let version = self.bump_version(object_type, key)?;
        self.persist_marking_snapshot(&concrete, key, version)
    }

    fn transition(
        &mut self,
        object_type: &str,
        key: &str,
        lifecycle: &str,
        transition: &str,
    ) -> Result<(), CellError> {
        let concrete = self.existing(object_type, key)?;
        let unknown = || CellError::UnknownTransition {
            object_type: object_type.to_owned(),
            lifecycle: lifecycle.to_owned(),
            transition: transition.to_owned(),
        };
        let declared = self
            .model
            .lifecycle(&concrete, lifecycle)?
            .ok_or_else(unknown)?;
        let step = declared
            .transitions
            .iter()
            .find(|candidate| candidate.id.as_str() == transition)
            .ok_or_else(unknown)?;
        let column = self.model.column(&concrete, lifecycle)?;
        let current = self.read_value(&concrete, column, key)?;
        let state = current.as_str().unwrap_or_default().to_owned();
        if !step.from.contains(&state) {
            return Err(CellError::TransitionNotAllowed {
                transition: transition.to_owned(),
                state,
            });
        }
        self.write_value(&concrete, column, key, &Value::String(step.to.clone()))?;
        let version = self.bump_version(object_type, key)?;
        self.persist_marking_snapshot(&concrete, key, version)
    }

    fn read_value(
        &self,
        object_type: &str,
        column: ColumnRef<'_>,
        key: &str,
    ) -> Result<Value, CellError> {
        let values = self.reader().read_row(
            column.table,
            self.world,
            key,
            ValueOwner::Object { object_type },
        )?;
        Ok(values
            .get(column.column.property.as_str())
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn write_value(
        &self,
        owner: &str,
        column: ColumnRef<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(), CellError> {
        let encoded = encode(owner, column.column, value)?;
        let (target, written) = if let Some(name) = &column.column.column {
            (name.as_str(), encoded)
        } else {
            // Read-modify-write the overflow object.
            let mut overflow: BTreeMap<String, Value> = self
                .reader()
                .read_row(
                    column.table,
                    self.world,
                    key,
                    ValueOwner::Object { object_type: owner },
                )?
                .into_iter()
                .filter(|(name, _)| {
                    column.table.columns.iter().any(|candidate| {
                        candidate.column.is_none() && candidate.property.as_str() == name.as_str()
                    })
                })
                .collect();
            overflow.insert(
                column.column.property.as_str().to_owned(),
                decode(&column.table.table, column.column, &encoded)?,
            );
            (
                OVERFLOW_COLUMN,
                SqlValue::Text(canonical_json(&Value::Object(overflow))?),
            )
        };
        self.conn.execute(
            &format!(
                "UPDATE {} SET {} = ?1 WHERE {} = ?2 AND {} = ?3",
                quote_identifier(&column.table.table),
                quote_identifier(target),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            params![written, self.world, key],
        )?;
        Ok(())
    }

    fn bump_version(&self, object_type: &str, key: &str) -> Result<u64, CellError> {
        let tables = self.model.tables(object_type)?;
        let root = tables
            .first()
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
        let version = next(
            self.reader().version(self.world, object_type, key)?,
            "version",
        )?;
        self.conn.execute(
            &format!(
                "UPDATE {} SET {} = ?1 WHERE {} = ?2 AND {} = ?3",
                quote_identifier(&root.table),
                quote_identifier(VERSION_COLUMN),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            params![to_i64(version, "version")?, self.world, key],
        )?;
        self.record_version(object_type, key, version)?;
        Ok(version)
    }

    /// Apply the batch's link changes in order. Every change bumps the
    /// link's version (`_norn_link_version`), an add replaces an existing
    /// link's values, and a remove of an absent link only bumps its version.
    #[allow(
        clippy::too_many_lines,
        reason = "link state, version and original creation evidence are written atomically"
    )]
    pub(crate) fn apply_links(&mut self) -> Result<Vec<LinkedRow>, CellError> {
        let mut linked = Vec::with_capacity(self.batch.links.len());
        for change in &self.batch.links {
            let (object_type, link, source, target, target_type, values) = match change {
                LinkChange::Add {
                    object_type,
                    link,
                    source,
                    target,
                    values,
                    target_type,
                } => (object_type, link, source, target, target_type, Some(values)),
                LinkChange::Remove {
                    object_type,
                    link,
                    source,
                    target,
                    target_type,
                } => (object_type, link, source, target, target_type, None),
            };
            let (declaration, table) = self.model.link(object_type, link)?;
            let target_type = target_type
                .clone()
                .unwrap_or_else(|| declaration.target.as_str().to_owned());
            let table_name = table.table.clone();
            let previous_version = self.reader().link_version(
                self.world,
                object_type,
                link,
                source,
                &target_type,
                target,
            )?;
            let version = next(previous_version, "link version")?;
            let live_identity = self
                .conn
                .query_optional(
                    &format!(
                        "SELECT v.\"version\", v.\"target_type\" FROM {} AS p LEFT JOIN \"{LINK_VERSIONS}\" AS v ON v.\"world\" = p.{} AND v.\"link_table\" = ?4 AND v.\"source\" = p.{} AND v.\"target\" = p.{} WHERE p.{} = ?1 AND p.{} = ?2 AND p.{} = ?3",
                        quote_identifier(&table_name),
                        quote_identifier(WORLD_COLUMN),
                        quote_identifier(SOURCE_COLUMN),
                        quote_identifier(TARGET_COLUMN),
                        quote_identifier(WORLD_COLUMN),
                        quote_identifier(SOURCE_COLUMN),
                        quote_identifier(TARGET_COLUMN),
                    ),
                    params![self.world, source, target, table_name],
                    |row| Ok((row.get::<Option<i64>>(0)?, row.get::<Option<String>>(1)?)),
                )?;
            let replaces_same_edge = live_identity.as_ref().is_some_and(|(_, previous_target)| {
                // Interface targets can share a raw key across unrelated type
                // roots. A changed typed endpoint starts a new incarnation;
                // hierarchy aliases still address the same object identity.
                previous_target.as_ref().is_some_and(|previous_target| {
                    self.reader().canonical_type(previous_target)
                        == self.reader().canonical_type(&target_type)
                })
            });
            let creation = if values.is_none() {
                None
            } else if live_identity
                .as_ref()
                .is_some_and(|(version, target)| version.is_none() || target.is_none())
            {
                // Older files may contain live rows without typed version
                // evidence. Their original creation cannot be established.
                Some((None, None))
            } else if replaces_same_edge {
                // A legacy version-one live row has an unambiguous first-add
                // receipt. Later versions do not distinguish replacements
                // from remove/re-add, so preserve unknown evidence explicitly.
                self.conn
                    .query_optional(
                        &format!(
                            "SELECT \"created_commit\", \"creation_version\" FROM \"{LINK_CREATION}\" WHERE \"world\" = ?1 AND \"link_table\" = ?2 AND \"source\" = ?3 AND \"target\" = ?4"
                        ),
                        params![self.world, table_name, source, target],
                        |row| Ok((row.get::<Option<i64>>(0)?, row.get::<Option<i64>>(1)?)),
                    )?
                    .or_else(|| Some((
                        None,
                        (live_identity.and_then(|(version, _)| version) == Some(1)).then_some(1),
                    )))
            } else {
                Some((
                    Some(self.commit_seq_sql()?),
                    Some(to_i64(version, "link version")?),
                ))
            };
            self.conn.execute(
                &format!(
                    "DELETE FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3",
                    quote_identifier(&table_name),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(SOURCE_COLUMN),
                    quote_identifier(TARGET_COLUMN),
                ),
                params![self.world, source, target],
            )?;
            if let Some(values) = values {
                self.add_link(object_type, link, source, target, values)?;
            }
            // Bumped from the canonical version (#3018): the highest over
            // the source hierarchy's tables of this link id.
            self.conn.execute(
                &format!(
                    "DELETE FROM \"{LINK_VERSIONS}\" WHERE \"world\" = ?1 AND \"link_table\" = ?2 AND \"source\" = ?3 AND \"target\" = ?4"
                ),
                params![self.world, table_name, source, target],
            )?;
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{LINK_VERSIONS}\" (\"world\", \"link_table\", \"source\", \"target\", \"link\", \"source_type\", \"target_type\", \"version\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ),
                params![
                    self.world,
                    table_name,
                    source,
                    target,
                    link,
                    object_type,
                    target_type,
                    to_i64(version, "link version")?
                ],
            )?;
            self.record_link_creation(&table_name, source, target, creation)?;
            linked.push(LinkedRow {
                object_type: object_type.clone(),
                link: link.clone(),
                source: source.clone(),
                target_type,
                target: target.clone(),
                version,
            });
        }
        Ok(linked)
    }

    fn record_link_creation(
        &self,
        table: &str,
        source: &str,
        target: &str,
        creation: Option<(Option<i64>, Option<i64>)>,
    ) -> Result<(), CellError> {
        self.conn.execute(
            &format!(
                "DELETE FROM \"{LINK_CREATION}\" WHERE \"world\" = ?1 AND \"link_table\" = ?2 AND \"source\" = ?3 AND \"target\" = ?4"
            ),
            params![self.world, table, source, target],
        )?;
        if let Some((commit, version)) = creation {
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{LINK_CREATION}\" (\"world\", \"link_table\", \"source\", \"target\", \"created_commit\", \"creation_version\") VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                ),
                params![self.world, table, source, target, commit, version],
            )?;
        }
        Ok(())
    }

    fn add_link(
        &self,
        object_type: &str,
        link: &str,
        source: &str,
        target: &str,
        values: &BTreeMap<String, Value>,
    ) -> Result<(), CellError> {
        self.existing(object_type, source)?;
        let (declaration, table) = self.model.link(object_type, link)?;
        let owner = format!("{object_type}.{link}");
        if let Some(name) = values.keys().find(|name| {
            !table
                .columns
                .iter()
                .any(|column| column.property.as_str() == name.as_str())
        }) {
            return Err(CellError::UnknownProperty {
                object_type: owner,
                property: (*name).clone(),
            });
        }
        let mut row: (Vec<&str>, Vec<SqlValue>) = (
            vec![WORLD_COLUMN, SOURCE_COLUMN, TARGET_COLUMN],
            vec![
                SqlValue::Text(self.world.to_owned()),
                SqlValue::Text(source.to_owned()),
                SqlValue::Text(target.to_owned()),
            ],
        );
        encode_row(
            &owner,
            &table.table,
            &table.columns,
            table.has_overflow,
            &mut row,
            |name| match values.get(name) {
                Some(value) => Ok(value.clone()),
                None => declaration
                    .properties
                    .iter()
                    .find(|property| property.id.as_str() == name)
                    .and_then(|property| {
                        property.default.as_ref().map(|default| (property, default))
                    })
                    .map(|(property, default)| {
                        let effective = norn_checker::EffectiveProperty::from_declared_property(
                            property,
                            self.model.property_types(),
                        );
                        effective
                            .runtime_default_literal(&default.0)
                            .map(|value| effective.canonical_runtime_value(&value))
                    })
                    .transpose()
                    .map(|value| value.unwrap_or(Value::Null))
                    .map_err(CellError::Value),
            },
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote_identifier(&table.table),
                column_list(&row.0),
                placeholders(row.0.len())
            ),
            &row.1,
        )?;
        Ok(())
    }

    /// Append the batch's events to the hash chain. Returns the first event's
    /// sequence number and the head hash after the append.
    pub(crate) fn append_events(&mut self) -> Result<(u64, String), CellError> {
        for event in &self.batch.events {
            self.reader()
                .verify_durable_event(self.world, self.commit_seq, event)?;
        }
        self.reader().verify_fact_event_multiset(
            self.world,
            self.commit_seq,
            &self.batch.events,
        )?;
        let (mut seq, mut prev) = self.reader().ledger_tail()?;
        let first = next(seq, "seq")?;
        let commit_seq = self.commit_seq_sql()?;
        for event in &self.batch.events {
            seq = next(seq, "seq")?;
            let hash = entry_hash(seq, self.commit_seq, self.world, event, &prev)?;
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{LEDGER}\" (\"seq\", \"commit_seq\", \"world\", \"event\", \"prev_hash\", \"hash\") VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                ),
                params![
                    to_i64(seq, "seq")?,
                    commit_seq,
                    self.world,
                    crate::payload_storage::encode_event(self.conn, seq, &self.batch.scope, self.world, event)?,
                    prev,
                    hash
                ],
            )?;
            prev = hash;
        }
        Ok((first, prev))
    }

    /// Apply the outbox and timer effects, in order.
    pub(crate) fn apply_effects(&mut self) -> Result<(), CellError> {
        for effect in &self.batch.effects {
            match effect {
                EffectRow::Enqueue { command } => self.enqueue(command)?,
                EffectRow::ClaimPerform {
                    command_id,
                    generation,
                } => self.claim_perform(command_id, *generation)?,
                EffectRow::Schedule { timer } => self.schedule(timer)?,
                EffectRow::CancelTimer { timer_id } => {
                    self.conn.execute(
                        &format!("DELETE FROM \"{TIMER}\" WHERE \"timer_id\" = ?1"),
                        params![timer_id],
                    )?;
                }
            }
        }
        Ok(())
    }

    fn claim_perform(&self, command_id: &str, generation: u64) -> Result<(), CellError> {
        let generation_sql = to_i64(generation, "generation")?;
        let head = StoreReader::new(self.conn, self.model).head_command()?;
        let is_head = head.is_some_and(|head| {
            head.command.command_id == command_id
                && head.command.generation == generation
                && head.result.is_none()
        });
        if !is_head {
            return Err(CellError::PerformClaimNotPending {
                command_id: command_id.to_owned(),
                generation,
            });
        }
        let known: Option<i64> = self
            .conn
            .query_optional(
                &format!(
                    "SELECT 1 FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
                ),
                params![command_id, generation_sql],
                |row| row.get::<i64>(0),
            )?;
        if known.is_some() {
            return Err(CellError::PerformAlreadyClaimed {
                command_id: command_id.to_owned(),
                generation,
            });
        }
        self.conn.execute(
            &format!(
                "INSERT INTO \"{PERFORM_CLAIM}\" (\"command_id\", \"generation\", \"commit_seq\") VALUES (?1, ?2, ?3)"
            ),
            params![command_id, generation_sql, self.commit_seq_sql()?],
        )?;
        Ok(())
    }

    fn enqueue(&self, command: &OutboxCommand) -> Result<(), CellError> {
        let commit_seq = self.commit_seq_sql()?;
        let generation = to_i64(command.generation, "generation")?;
        let deadline = command
            .deadline
            .map(|deadline| to_i64(deadline, "deadline"))
            .transpose()?;
        let serialized = canonical_json(&command.command)?;
        let known: Option<(i64, Option<i64>)> = self.conn.query_optional(
            &format!(
                "SELECT \"generation\", \"consumed_commit_seq\" FROM \"{OUTBOX}\" WHERE \"command_id\" = ?1"
            ),
            params![command.command_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let position = match known {
            // A re-issue at the same or an older generation never re-executes.
            Some((known, _)) if known >= generation => return Ok(()),
            // A newer generation of a pending command replaces it in place.
            Some((_, None)) => None,
            // New, or a newer generation of a consumed command: appended.
            Some((_, Some(_))) | None => {
                let maximum = self.conn.query_row(
                    &format!("SELECT COALESCE(MAX(\"position\"), 0) FROM \"{OUTBOX}\""),
                    params![],
                    |row| row.get::<i64>(0),
                )?;
                // Increment only after crossing the SQL/JS seam: SQLite's
                // 2^53 + 1 would otherwise round back to 2^53 on the DO host.
                Some(to_i64(
                    next(to_u64(maximum, "outbox position")?, "outbox position")?,
                    "outbox position",
                )?)
            }
        };
        self.conn.execute(
            &format!(
                "DELETE FROM \"{OUTBOX}\" WHERE \"command_id\" = ?1 AND \"consumed_commit_seq\" IS NOT NULL"
            ),
            params![command.command_id],
        )?;
        if let Some(position) = position {
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{OUTBOX}\" (\"command_id\", \"position\", \"generation\", \"commit_seq\", \"idempotency_key\", \"deadline\", \"command\", \"result\", \"consumed_commit_seq\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL)"
                ),
                params![
                    command.command_id,
                    position,
                    generation,
                    commit_seq,
                    command.idempotency_key,
                    deadline,
                    serialized
                ],
            )?;
        } else {
            self.conn.execute(
                &format!(
                    "UPDATE \"{OUTBOX}\" SET \"generation\" = ?1, \"commit_seq\" = ?2, \"idempotency_key\" = ?3, \"deadline\" = ?4, \"command\" = ?5, \"result\" = NULL WHERE \"command_id\" = ?6"
                ),
                params![
                    generation,
                    commit_seq,
                    command.idempotency_key,
                    deadline,
                    serialized,
                    command.command_id
                ],
            )?;
        }
        crate::payload_storage::remember_command(
            self.conn,
            &command.command_id,
            command.generation,
            self.commit_seq,
        )?;
        Ok(())
    }

    fn schedule(&self, timer: &TimerEntry) -> Result<(), CellError> {
        self.conn.execute(
            &format!("DELETE FROM \"{TIMER}\" WHERE \"timer_id\" = ?1"),
            params![timer.timer_id],
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{TIMER}\" (\"timer_id\", \"due\", \"timer\", \"commit_seq\") VALUES (?1, ?2, ?3, ?4)"
            ),
            params![
                timer.timer_id,
                to_i64(timer.due, "due")?,
                canonical_json(&timer.timer)?,
                self.commit_seq_sql()?
            ],
        )?;
        Ok(())
    }

    pub(crate) fn write_receipt(&mut self, body: ReceiptBody) -> Result<Receipt, CellError> {
        let body_json = serde_json::to_value(&body)?;
        let hash = hash_json(&body_json)?;
        self.conn.execute(
            &format!(
                "INSERT INTO \"{RECEIPT}\" (\"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\") VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![
                to_i64(body.commit_seq, "commit_seq")?,
                body.world,
                body.idempotency_key,
                body.request_hash,
                canonical_json(&body_json)?,
                hash
            ],
        )?;
        Ok(Receipt { body, hash })
    }
}
