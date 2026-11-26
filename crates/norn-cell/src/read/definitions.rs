//! Retained definition version reads on [`StoreReader`].

use super::{StoreReader, to_i64, to_u64};
use crate::codec::hash_json;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::store::{InstalledDefinitionSet, ReceiptBody};
use crate::system::{
    DEFINITION_ACTIVE, DEFINITION_ADMISSION_RECEIPT, DEFINITION_LAYOUT, DEFINITION_REGISTRY,
    DEFINITION_VERSION, RECEIPT,
};

pub(crate) fn active_definition_version(engine: &impl SqlEngine) -> Result<Option<u64>, CellError> {
    let version: Option<i64> = engine.query_optional(
        &format!("SELECT \"version\" FROM \"{DEFINITION_ACTIVE}\" WHERE \"id\" = 1"),
        params![],
        |row| row.get(0),
    )?;
    version
        .map(|version| to_u64(version, "definitions version"))
        .transpose()
}

pub(crate) fn validate_active_definition_pointer(
    engine: &impl SqlEngine,
) -> Result<Option<u64>, CellError> {
    // One statement pins both values to the same SQLite snapshot, including
    // when another CellStore installs definitions on a separate connection.
    let (active, latest): (Option<i64>, Option<i64>) = engine.query_row(
        &format!(
            "SELECT (SELECT \"version\" FROM \"{DEFINITION_ACTIVE}\" WHERE \"id\" = 1), \
             (SELECT \"version\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\" DESC LIMIT 1)"
        ),
        params![],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let active = active
        .map(|version| to_u64(version, "definitions version"))
        .transpose()?;
    let latest = latest
        .map(|version| to_u64(version, "definitions version"))
        .transpose()?;
    match (active, latest) {
        (None, None) => Ok(None),
        (Some(active), Some(latest)) if active == latest && active > 0 => Ok(Some(active)),
        (active, latest) => Err(CellError::LedgerCorrupt {
            seq: active.or(latest).unwrap_or(0),
            reason: "active definitions pointer does not name the latest retained version"
                .to_owned(),
        }),
    }
}

pub(crate) fn definition_by_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Option<InstalledDefinitionSet>, CellError> {
    let version_sql = to_i64(version, "definitions version")?;
    let row = engine.query_optional(
        &format!(
            "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"
        ),
        params![version_sql],
        |row| {
            Ok(InstalledDefinitionSet {
                version: to_u64(row.get(0)?, "definitions version")?,
                receipt_sha256: row.get(1)?,
                admission_receipt: None,
                canonical_set: row.get(2)?,
                registry: Vec::new(),
                model_hash: row.get(3)?,
                ddl_hash: row.get(4)?,
                installed_at: to_u64(row.get(5)?, "definitions installed_at")?,
                commit_seq: to_u64(row.get(6)?, "definitions commit_seq")?,
                layout: None,
                layout_revision: 0,
            })
        },
    )?;
    let Some(mut installed) = row else {
        return Ok(None);
    };
    if definition_registry_table_exists(engine)? {
        installed.registry = definition_registry_for_version(engine, version)?;
    }
    if definition_admission_receipt_table_exists(engine)? {
        installed.admission_receipt = definition_admission_receipt_for_version(engine, version)?;
    }
    (installed.layout, installed.layout_revision) = definition_layout_for_version(engine, version)?;
    Ok(Some(installed))
}

fn definition_layout_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<(Option<String>, u64), CellError> {
    if !definition_layout_table_exists(engine)? {
        return Ok((None, 0));
    }
    definition_layout_row(engine, version)
}

fn definition_layout_row(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<(Option<String>, u64), CellError> {
    let row = engine.query_optional(
        &format!(
            "SELECT \"layout_revision\", \"sidecar\" FROM \"{DEFINITION_LAYOUT}\" WHERE \"version\" = ?1"
        ),
        params![to_i64(version, "definitions version")?],
        |row| {
            Ok((
                to_u64(row.get(0)?, "layout revision")?,
                row.get::<String>(1)?,
            ))
        },
    )?;
    Ok(row.map_or((None, 0), |(revision, sidecar)| (Some(sidecar), revision)))
}

fn definition_layout_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_LAYOUT],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_registry_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_REGISTRY],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_admission_receipt_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_ADMISSION_RECEIPT],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_admission_receipt_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Option<String>, CellError> {
    engine.query_optional(
        &format!(
            "SELECT \"receipt\" FROM \"{DEFINITION_ADMISSION_RECEIPT}\" WHERE \"version\" = ?1"
        ),
        params![to_i64(version, "definitions version")?],
        |row| row.get::<String>(0),
    )
}

fn definition_registry_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Vec<String>, CellError> {
    definition_registry_row(engine, version).map(|(registry, _)| registry)
}

fn definition_registry_row(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<(Vec<String>, bool), CellError> {
    let registry = engine.query_optional(
        &format!("SELECT \"registry\" FROM \"{DEFINITION_REGISTRY}\" WHERE \"version\" = ?1"),
        params![to_i64(version, "definitions version")?],
        |row| row.get::<String>(0),
    )?;
    let Some(registry) = registry else {
        return Ok((Vec::new(), false));
    };
    Ok((serde_json::from_str(&registry)?, true))
}

fn definition_from_row(row: &SqlRow) -> Result<InstalledDefinitionSet, CellError> {
    Ok(InstalledDefinitionSet {
        version: to_u64(row.get(0)?, "definitions version")?,
        receipt_sha256: row.get(1)?,
        admission_receipt: None,
        canonical_set: row.get(2)?,
        registry: Vec::new(),
        model_hash: row.get(3)?,
        ddl_hash: row.get(4)?,
        installed_at: to_u64(row.get(5)?, "definitions installed_at")?,
        commit_seq: to_u64(row.get(6)?, "definitions commit_seq")?,
        layout: None,
        layout_revision: 0,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The active retained definition version without loading its payload.
    ///
    /// # Errors
    /// SQL errors or a dangling active-version pointer.
    pub fn active_definitions_version(&self) -> Result<Option<u64>, CellError> {
        self.check_live()?;
        validate_active_definition_pointer(self.conn)
    }

    /// ADMIT-CACHE-1 (#1533): the active definitions' identity, its version
    /// and admission receipt digest, without loading its payload: what a host
    /// keys the work it does once per definitions version by.
    ///
    /// # Errors
    /// SQL errors or a dangling active-version pointer.
    pub fn active_definitions_identity(&self) -> Result<Option<(u64, String)>, CellError> {
        self.check_live()?;
        let Some(version) = validate_active_definition_pointer(self.conn)? else {
            return Ok(None);
        };
        let digest: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT \"receipt_sha256\" FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"
            ),
            params![to_i64(version, "definitions version")?],
            |row| row.get(0),
        )?;
        let Some(digest) = digest else {
            return Err(CellError::LedgerCorrupt {
                seq: version,
                reason: "active definitions pointer has no retained version".to_owned(),
            });
        };
        Ok(Some((version, digest)))
    }

    /// Whether a retained definition set is installed at `version`, without
    /// loading it: exactly `installed_definitions(Some(version)).is_some()`
    /// for any version at or below the active one.
    ///
    /// # Errors
    /// SQL errors.
    pub(crate) fn definitions_version_retained(&self, version: u64) -> Result<bool, CellError> {
        Ok(self
            .conn
            .query_optional(
                &format!("SELECT 1 FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"),
                params![to_i64(version, "definitions version")?],
                |row| row.get::<i64>(0),
            )?
            .is_some())
    }

    /// The retained definition set at `version`, or the active set when
    /// `version` is `None`.
    ///
    /// # Errors
    ///
    /// SQL, decoding, or a dangling active-version pointer.
    pub fn installed_definitions(
        &self,
        version: Option<u64>,
    ) -> Result<Option<InstalledDefinitionSet>, CellError> {
        self.check_live()?;
        let version = if let Some(version) = version {
            version
        } else {
            let Some(version) = validate_active_definition_pointer(self.conn)? else {
                return Ok(None);
            };
            version
        };
        let installed = definition_by_version(self.conn, version)?;
        if installed.is_none()
            && version == validate_active_definition_pointer(self.conn)?.unwrap_or(0)
        {
            return Err(CellError::LedgerCorrupt {
                seq: version,
                reason: "active definitions pointer has no retained version".to_owned(),
            });
        }
        Ok(installed)
    }

    /// The active definition version in force at `commit_seq`, derived from
    /// the retained version rows rather than from user-emittable ledger data.
    ///
    /// # Errors
    ///
    /// SQL, decoding, sequence-range, or receipt-integrity errors.
    pub fn definitions_version_at(&self, commit_seq: u64) -> Result<Option<u64>, CellError> {
        self.check_history("definitions version history")?;
        if commit_seq == 0 {
            return Ok(None);
        }
        let row: Option<(i64, String, String)> = self.conn.query_optional(
            &format!(
                "SELECT \"commit_seq\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" <= ?1 ORDER BY \"commit_seq\" DESC LIMIT 1"
            ),
            params![to_i64(commit_seq, "commit_seq")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let Some((stored_seq, serialized, stored_hash)) = row else {
            return Ok(None);
        };
        let stored_seq = to_u64(stored_seq, "commit_seq")?;
        let body: ReceiptBody = serde_json::from_str(&serialized)?;
        if body.commit_seq != stored_seq || stored_hash != hash_json(&body)? {
            return Err(CellError::LedgerCorrupt {
                seq: stored_seq,
                reason: "definitions-version receipt does not verify".to_owned(),
            });
        }
        Ok((body.definitions_version != 0).then_some(body.definitions_version))
    }

    /// All retained definition versions in ascending order.
    ///
    /// # Errors
    ///
    /// SQL or decoding errors.
    pub fn installed_definition_versions(&self) -> Result<Vec<InstalledDefinitionSet>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\""
            ),
            params![],
        )?;
        decode_definition_rows(self.conn, rows, None)
    }

    /// Read at most `limit` retained definition versions strictly after
    /// `after_version` (unfiltered when `after_version == 0`), charging exact
    /// SQL rows read (including schema probes and layout/registry rows) to `budget`.
    pub(crate) fn installed_definition_versions_after(
        &self,
        after_version: u64,
        limit: usize,
        budget: &mut crate::scrub::RowBudget,
    ) -> Result<Vec<InstalledDefinitionSet>, CellError> {
        let limit = i64::try_from(limit)
            .unwrap_or(crate::engine::MAX_SAFE_INTEGER)
            .min(crate::engine::MAX_SAFE_INTEGER);
        let rows = if after_version == 0 {
            self.conn.query(
                &format!(
                    "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\" LIMIT ?1"
                ),
                params![limit],
            )?
        } else {
            self.conn.query(
                &format!(
                    "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" WHERE \"version\" > ?1 ORDER BY \"version\" LIMIT ?2"
                ),
                params![to_i64(after_version, "definitions version")?, limit],
            )?
        };
        budget.charge(rows.len());
        decode_definition_rows(self.conn, rows, Some(budget))
    }

    /// O(1) latest retained definition version at or before `through` for
    /// `Phase::Seed` pin verification (`ORDER BY commit_seq DESC LIMIT 1`, `<= 1` row).
    pub(crate) fn latest_definition_version_through(&self, through: u64) -> Result<u64, CellError> {
        let max_version: Option<i64> = self.conn.query_optional(
            &format!(
                "SELECT \"version\" FROM \"{DEFINITION_VERSION}\" WHERE \"commit_seq\" <= ?1 ORDER BY \"commit_seq\" DESC LIMIT 1"
            ),
            params![to_i64(through, "definitions commit_seq")?],
            |row| row.get(0),
        )?;
        max_version
            .map(|version| to_u64(version, "definitions max version"))
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

fn decode_definition_rows(
    conn: &impl SqlEngine,
    rows: Vec<Vec<crate::engine::SqlValue>>,
    mut budget: Option<&mut crate::scrub::RowBudget>,
) -> Result<Vec<InstalledDefinitionSet>, CellError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let has_registry_table = definition_registry_table_exists(conn)?;
    let has_admission_receipt_table = definition_admission_receipt_table_exists(conn)?;
    let has_layout_table = definition_layout_table_exists(conn)?;
    if let Some(budget) = budget.as_deref_mut() {
        budget.charge(
            usize::from(has_registry_table)
                .saturating_add(usize::from(has_admission_receipt_table))
                .saturating_add(usize::from(has_layout_table)),
        );
    }
    let mut out = Vec::with_capacity(rows.len());
    for values in rows {
        let mut installed = definition_from_row(&SqlRow(values))?;
        if has_registry_table {
            let (registry, found) = definition_registry_row(conn, installed.version)?;
            installed.registry = registry;
            if let Some(budget) = budget.as_deref_mut() {
                budget.charge(usize::from(found));
            }
        }
        if has_admission_receipt_table {
            installed.admission_receipt =
                definition_admission_receipt_for_version(conn, installed.version)?;
            if let Some(budget) = budget.as_deref_mut() {
                budget.charge(usize::from(installed.admission_receipt.is_some()));
            }
        }
        if has_layout_table {
            (installed.layout, installed.layout_revision) =
                definition_layout_row(conn, installed.version)?;
            if let Some(budget) = budget.as_deref_mut() {
                budget.charge(usize::from(installed.layout.is_some()));
            }
        }
        out.push(installed);
    }
    Ok(out)
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// PRIVACY-4 / S2 (#1547, #1559): verify that no grouped, filtered, measure
    /// or time path of `plan` (including every lowered sum's metric default
    /// filters) reads marked or unclassified values in `world`. When
    /// `historical` is true, any current, past or deleted version of a
    /// referenced property that holds a label or lacks immutable classification
    /// evidence is refused.
    ///
    /// Pass `has_marking_catalog = true` when the caller already knows the
    /// active definition catalog declares markings; when `false`, the guard
    /// still resolves the retained definition catalog, model property markings,
    /// and stored markings (`Self::stores_markings`) before taking the fast
    /// path.
    ///
    /// # Errors
    /// Unknown type, malformed retained definitions, or a SQL-engine error.
    pub fn check_question_markings(
        &self,
        plan: &norn_checker::metric::compute::QuestionPlan,
        world: &str,
        historical: bool,
        has_marking_catalog: bool,
    ) -> Result<Option<super::objects::MarkedQuestionPath>, CellError> {
        self.check_live()?;
        if !has_marking_catalog
            && !self.model_declares_markings()
            && !self.stores_markings()?
            && !self.has_retained_marking_catalog()?
        {
            return Ok(None);
        }
        let question = plan.question();
        let grain = plan.grain().as_str();
        for sum in plan.sums() {
            let measure_ptr = format!("/metric/{}/measure", sum.name);
            if let Some(hit) = self.check_property_marking(
                world,
                grain,
                sum.property.id.as_str(),
                &measure_ptr,
                historical,
            )? {
                return Ok(Some(hit));
            }
            let (time_type, time_prop) = match &sum.time_link {
                Some(link) => (link.target_type.as_str(), link.property.id.as_str()),
                None => (grain, sum.time_property.as_str()),
            };
            let time_ptr = format!("/metric/{}/time", sum.name);
            if let Some(hit) =
                self.check_property_marking(world, time_type, time_prop, &time_ptr, historical)?
            {
                return Ok(Some(hit));
            }
        }
        let paths = question
            .dimensions
            .iter()
            .enumerate()
            .map(|(index, dimension)| (format!("/question/groupBy/{index}"), &dimension.path))
            .chain(
                question
                    .filters
                    .iter()
                    .enumerate()
                    .map(|(index, filter)| (format!("/question/filter/{index}"), &filter.path)),
            )
            .chain(plan.sums().iter().flat_map(|sum| {
                sum.filters.iter().enumerate().map(move |(index, filter)| {
                    (
                        format!("/metric/{}/norn/defaultFilters/{index}", sum.name),
                        &filter.path,
                    )
                })
            }));
        for (pointer, path) in paths {
            let property = path.property.as_str();
            let types: Vec<String> = if path.links.is_empty() {
                vec![grain.to_owned()]
            } else {
                self.model
                    .object_types()
                    .map(|definition| definition.name.as_str().to_owned())
                    .filter(|name| {
                        self.model
                            .effective_property_types(name)
                            .is_ok_and(|properties| properties.iter().any(|(id, _)| id == property))
                    })
                    .collect()
            };
            for object_type in types {
                if let Some(hit) = self.check_property_marking(
                    world,
                    &object_type,
                    property,
                    &pointer,
                    historical,
                )? {
                    return Ok(Some(hit));
                }
            }
        }
        Ok(None)
    }

    fn check_property_marking(
        &self,
        world: &str,
        object_type: &str,
        property: &str,
        pointer: &str,
        historical: bool,
    ) -> Result<Option<super::objects::MarkedQuestionPath>, CellError> {
        if historical
            && (self.property_history_marked(world, object_type, property)?
                || self.property_history_unclassified(world, object_type, property)?)
        {
            return Ok(Some(super::objects::MarkedQuestionPath::History {
                pointer: pointer.to_owned(),
            }));
        }
        let labels = self.property_marking_summary(world, object_type, property)?;
        if !labels.is_some_and(|labels| labels.is_empty()) {
            return Ok(Some(super::objects::MarkedQuestionPath::Live {
                pointer: pointer.to_owned(),
            }));
        }
        Ok(None)
    }
}
