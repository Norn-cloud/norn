//! Retained definitions: definitions a cell keeps that are not kernel
//! artifacts (Studio dashboards, agents). The IR has no artifact kind for
//! them, so the checker's admission receipt does not cover them; the host
//! admits each one with its own validator, then installs it through the
//! governed definitions install. That commit carries the kernel set and its
//! receipt unchanged, and appends exactly one revision of one retained
//! definition, in the same transaction as the new definitions version and
//! its ledger event. Revisions are never rewritten or removed: retiring
//! records a new revision whose lifecycle is `retired`.

use serde::{Deserialize, Serialize};

use crate::{
    CellError,
    engine::{SqlEngine, params},
    read::StoreReader,
};

/// The table holding every revision of every retained definition.
pub(crate) const RETAINED_DEFINITION: &str = "_norn_retained_definition";
/// The table holding server-bound author grants for retained definitions
/// (`AUTH-ATTENUATE-1`). Written only inside `SqlEngine::definition_writes`.
pub(crate) const RETAINED_AUTHORITY: &str = "_norn_retained_authority";

const RETAINED_DDL: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS "_norn_retained_definition" (
  "id" TEXT NOT NULL,
  "revision" INTEGER NOT NULL,
  "kind" TEXT NOT NULL,
  "lifecycle" TEXT NOT NULL,
  "title" TEXT NOT NULL,
  "payload" TEXT NOT NULL,
  "payload_sha256" TEXT NOT NULL,
  "signoff" TEXT,
  "definitions_version" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "recorded_at" INTEGER NOT NULL,
  PRIMARY KEY ("id", "revision")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_retained_authority" (
  "id" TEXT NOT NULL,
  "revision" INTEGER NOT NULL,
  "authority" TEXT NOT NULL,
  PRIMARY KEY ("id", "revision")
)"#,
    // One revision per id per definitions version: a kernel set install
    // carries its pack's approvals in its one version (#1469). Replaces the
    // older one-revision-per-version index on files that have it.
    r#"DROP INDEX IF EXISTS "_norn_retained_definition/version""#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "_norn_retained_definition/version_id" ON "_norn_retained_definition" ("definitions_version", "id")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_retained_definition/kind" ON "_norn_retained_definition" ("kind", "id", "revision")"#,
];

/// Create the retained-definition table in the store's initialization
/// transaction. Additive: stores opened before it existed gain an empty one.
pub(crate) fn init(engine: &impl SqlEngine) -> Result<(), CellError> {
    for statement in RETAINED_DDL {
        engine.execute_batch(statement)?;
    }
    Ok(())
}

/// Whether a retained definition is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainedLifecycle {
    /// Listed and served.
    Active,
    /// Left out of default listings; its history stays readable.
    Retired,
}

impl RetainedLifecycle {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }

    fn parse(text: &str) -> Result<Self, CellError> {
        match text {
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            other => Err(CellError::Engine(format!(
                "retained definition has unknown lifecycle {other:?}"
            ))),
        }
    }
}

/// One revision of one retained definition, as the host admitted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedDefinition {
    /// Host-declared kind (`dashboard`, `agent`).
    pub kind: String,
    /// Stable id, unique across kinds in the cell.
    pub id: String,
    /// Per-id revision, starting at 1.
    pub revision: u64,
    /// Lifecycle after this revision.
    pub lifecycle: RetainedLifecycle,
    /// The title listings show.
    pub title: String,
    /// Canonical JSON of the admitted payload.
    pub payload: String,
    /// Lower-case hex SHA-256 of `payload`.
    pub payload_sha256: String,
    /// The host-verified sign-off of this revision's plain-words restatement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signoff: Option<RetainedSignoff>,
    /// The host-verified, attenuated, revocable author grant bound to this
    /// revision at install (`AUTH-ATTENUATE-1`). Never from a client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<RetainedAuthority>,
}

/// A sign-off the host verified: the installing principal, the install
/// time, and the restatement digest the host recomputed. Never from a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedSignoff {
    /// The installing principal's subject id.
    pub signer: String,
    /// Host time of the install, in milliseconds.
    pub signed_at: u64,
    /// Lower-case hex SHA-256 of the recomputed restatement.
    pub restatement_sha256: String,
}

/// A server-verified, attenuated, revocable author grant bound to a retained
/// definition revision (Core 05 `AUTH-ATTENUATE-1`). Never supplied by a
/// client; never borrowed from a later triggering actor.
///
/// U2 dispatch obligation: before firing any outbox candidate or parking a
/// proposal, U2 must require that this revision is still the automation's
/// current active revision and reauthorize both the author's live grant
/// (checking revocation against `parent_grant_ids` and `author`) and the
/// concrete target row under the dispatch hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedAuthority {
    /// Stable delegation id derived from the definition id, revision, author,
    /// scope, bound action, and parent grant identities.
    pub delegation_id: String,
    /// The verified principal who authored and installed this revision.
    pub author: crate::flow_run::RunStarter,
    /// Explicit cell scope (`org/environment`) this grant is attenuated to.
    pub scope: String,
    /// The single admitted action capability this grant is attenuated to.
    pub action_id: String,
    /// The object type this grant is attenuated to.
    pub object_type: String,
    /// Digest of the bound action definition at install time.
    pub action_digest: String,
    /// Sorted, deduplicated parent grant / Cedar policy identities returned by
    /// the PDP when this revision's delegation was admitted. Retained so U2 can
    /// detect grant revocation or policy replacement under the dispatch hold.
    pub parent_grant_ids: Vec<String>,
    /// Host logical time (ms) when the author grant was verified and bound.
    pub bound_at: u64,
    /// Cell definitions version active when the grant was verified and bound.
    pub definitions_version: u64,
    /// Whether this delegation is revocable (always `true`; retiring or
    /// revising the definition revokes this revision's delegation).
    pub revocable: bool,
}

/// A retained-definition revision to install, compare-and-set on the id's
/// current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedDefinitionChange {
    /// The revision to append; its `revision` must be `expected + 1`.
    pub definition: RetainedDefinition,
    /// The id's current revision; `None` requires that the id is unused.
    pub expected_revision: Option<u64>,
}

/// A stored revision with the commit that installed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainedDefinitionRecord {
    /// The revision.
    pub definition: RetainedDefinition,
    /// The cell definitions version its install created.
    pub definitions_version: u64,
    /// The commit that installed it.
    pub commit_seq: u64,
    /// Host logical time of that commit, in milliseconds.
    pub recorded_at: u64,
}

/// Check `change` against the id's current revision. Runs inside the install
/// preflight, so the transaction re-checks it.
pub(crate) fn preflight(
    engine: &impl SqlEngine,
    change: &RetainedDefinitionChange,
) -> Result<(), CellError> {
    let definition = &change.definition;
    let actual = current_revision(engine, &definition.id)?;
    let next = change.expected_revision.unwrap_or(0).checked_add(1);
    if actual != change.expected_revision || next != Some(definition.revision) {
        return Err(CellError::RetainedDefinitionConflict {
            id: definition.id.clone(),
            expected: change.expected_revision,
            actual,
        });
    }
    if let Some(current) = actual.map(|revision| at(engine, &definition.id, revision)) {
        let current =
            current?.ok_or_else(|| CellError::Engine("retained revision vanished".into()))?;
        if current.definition.kind != definition.kind {
            return Err(CellError::RetainedDefinitionConflict {
                id: definition.id.clone(),
                expected: change.expected_revision,
                actual,
            });
        }
    }
    if payload_sha256(&definition.payload) != definition.payload_sha256 {
        return Err(CellError::Engine(
            "retained definition payload digest mismatch".to_owned(),
        ));
    }
    if definition.kind == "automation" && definition.lifecycle == RetainedLifecycle::Active {
        let valid = definition.authority.as_ref().is_some_and(|authority| {
            authority.revocable
                && !authority.delegation_id.trim().is_empty()
                && !authority.author.subject.trim().is_empty()
                && !authority.scope.trim().is_empty()
                && !authority.action_id.trim().is_empty()
                && !authority.object_type.trim().is_empty()
                && !authority.action_digest.trim().is_empty()
                && !authority.parent_grant_ids.is_empty()
                && authority
                    .parent_grant_ids
                    .iter()
                    .all(|grant_id| !grant_id.trim().is_empty())
        });
        if !valid {
            return Err(CellError::Engine(
                "active automation definition requires a server-bound revocable author grant"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

/// Append `definition` as installed by `definitions_version` at `commit_seq`,
/// maintaining the transactional automation trigger projection when `definition`
/// is an automation.
pub(crate) fn insert(
    engine: &impl SqlEngine,
    model: &crate::Model,
    definition: &RetainedDefinition,
    definitions_version: u64,
    commit_seq: u64,
    recorded_at: u64,
) -> Result<(), CellError> {
    let signoff = definition
        .signoff
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(CellError::Json)?;
    engine.execute(
        &format!(
            "INSERT INTO \"{RETAINED_DEFINITION}\" (\"id\", \"revision\", \"kind\", \"lifecycle\", \"title\", \"payload\", \"payload_sha256\", \"definitions_version\", \"commit_seq\", \"recorded_at\", \"signoff\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
        ),
        params![
            definition.id,
            to_i64(definition.revision)?,
            definition.kind,
            definition.lifecycle.as_str(),
            definition.title,
            definition.payload,
            definition.payload_sha256,
            to_i64(definitions_version)?,
            to_i64(commit_seq)?,
            to_i64(recorded_at)?,
            signoff,
        ],
    )?;
    if let Some(authority) = &definition.authority {
        let encoded = serde_json::to_string(authority).map_err(CellError::Json)?;
        engine.execute(
            &format!(
                "INSERT INTO \"{RETAINED_AUTHORITY}\" (\"id\", \"revision\", \"authority\") VALUES (?1, ?2, ?3)"
            ),
            params![definition.id, to_i64(definition.revision)?, encoded],
        )?;
    }
    if definition.kind == "automation" {
        crate::automation_outbox::sync_trigger_index(engine, model, definition)?;
    }
    Ok(())
}

/// Lower-case hex SHA-256 of a canonical payload.
#[must_use]
pub fn payload_sha256(payload: &str) -> String {
    use transparency_sha2::{Digest, Sha256};
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(payload.as_bytes()) {
        for nibble in [byte >> 4, byte & 0x0f] {
            // Every nibble is below 16, so a hex digit always exists.
            hex.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    hex
}

fn to_i64(value: u64) -> Result<i64, CellError> {
    crate::read::to_i64(value, "retained definition")
}

fn to_u64(value: i64) -> Result<u64, CellError> {
    crate::read::to_u64(value, "retained definition")
}

fn current_revision(engine: &impl SqlEngine, id: &str) -> Result<Option<u64>, CellError> {
    let revision: Option<Option<i64>> = engine.query_optional(
        &format!("SELECT MAX(\"revision\") FROM \"{RETAINED_DEFINITION}\" WHERE \"id\" = ?1"),
        params![id],
        |row| row.get(0),
    )?;
    revision.flatten().map(to_u64).transpose()
}

const COLUMNS: &str = "\"d\".\"id\", \"d\".\"revision\", \"d\".\"kind\", \"d\".\"lifecycle\", \"d\".\"title\", \"d\".\"payload\", \"d\".\"payload_sha256\", \"d\".\"definitions_version\", \"d\".\"commit_seq\", \"d\".\"recorded_at\", \"d\".\"signoff\", \"a\".\"authority\"";

type Row = (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

fn record(row: Row) -> Result<RetainedDefinitionRecord, CellError> {
    let (
        id,
        revision,
        kind,
        lifecycle,
        title,
        payload,
        payload_sha256,
        version,
        seq,
        at,
        signoff,
        authority,
    ) = row;
    let signoff = signoff
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(CellError::Json)?;
    let authority = authority
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(CellError::Json)?;
    Ok(RetainedDefinitionRecord {
        definition: RetainedDefinition {
            kind,
            id,
            revision: to_u64(revision)?,
            lifecycle: RetainedLifecycle::parse(&lifecycle)?,
            title,
            payload,
            payload_sha256,
            signoff,
            authority,
        },
        definitions_version: to_u64(version)?,
        commit_seq: to_u64(seq)?,
        recorded_at: to_u64(at)?,
    })
}

macro_rules! row {
    ($row:expr) => {
        Ok((
            $row.get(0)?,
            $row.get(1)?,
            $row.get(2)?,
            $row.get(3)?,
            $row.get(4)?,
            $row.get(5)?,
            $row.get(6)?,
            $row.get(7)?,
            $row.get(8)?,
            $row.get(9)?,
            $row.get(10)?,
            $row.get(11)?,
        ))
    };
}

fn at(
    engine: &impl SqlEngine,
    id: &str,
    revision: u64,
) -> Result<Option<RetainedDefinitionRecord>, CellError> {
    let row: Option<Row> = engine.query_optional(
        &format!(
            "SELECT {COLUMNS} FROM \"{RETAINED_DEFINITION}\" AS \"d\" \
             LEFT JOIN \"{RETAINED_AUTHORITY}\" AS \"a\" ON \"a\".\"id\" = \"d\".\"id\" AND \"a\".\"revision\" = \"d\".\"revision\" \
             WHERE \"d\".\"id\" = ?1 AND \"d\".\"revision\" = ?2"
        ),
        params![id, to_i64(revision)?],
        |row| row!(row),
    )?;
    row.map(record).transpose()
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Revision `revision` of retained definition `id`, or its latest when
    /// `revision` is `None`.
    ///
    /// # Errors
    /// Store errors.
    pub fn retained_definition(
        &self,
        id: &str,
        revision: Option<u64>,
    ) -> Result<Option<RetainedDefinitionRecord>, CellError> {
        self.check_live()?;
        let revision = match revision {
            Some(revision) => Some(revision),
            None => current_revision(self.conn, id)?,
        };
        revision.map_or(Ok(None), |revision| at(self.conn, id, revision))
    }

    /// The latest revision of retained definition `id` installed at or before
    /// `definitions_version`, for `STEP-PIN-1` run-start compatibility checks.
    ///
    /// # Errors
    /// Store errors.
    pub fn retained_definition_at_version(
        &self,
        id: &str,
        definitions_version: u64,
    ) -> Result<Option<RetainedDefinitionRecord>, CellError> {
        self.check_live()?;
        let revision = self.conn.query_optional(
            &format!(
                "SELECT MAX(\"revision\") FROM \"{RETAINED_DEFINITION}\" \
                 WHERE \"id\" = ?1 AND \"definitions_version\" <= ?2"
            ),
            params![id, to_i64(definitions_version)?],
            |row| row.get::<Option<i64>>(0),
        )?;
        match revision.flatten() {
            Some(rev) => at(self.conn, id, to_u64(rev)?),
            None => Ok(None),
        }
    }

    /// The latest revision of each retained definition of `kind`, ordered by
    /// id, strictly after `after`, at most `limit`; retired ones only when
    /// `include_retired`.
    ///
    /// # Errors
    /// Store errors.
    pub fn retained_definitions(
        &self,
        kind: &str,
        include_retired: bool,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<RetainedDefinitionRecord>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT {COLUMNS} FROM \"{RETAINED_DEFINITION}\" AS \"d\" \
                 LEFT JOIN \"{RETAINED_AUTHORITY}\" AS \"a\" ON \"a\".\"id\" = \"d\".\"id\" AND \"a\".\"revision\" = \"d\".\"revision\" \
                 WHERE \"d\".\"kind\" = ?1 AND \"d\".\"id\" > ?2 \
                 AND \"d\".\"revision\" = (SELECT MAX(\"revision\") FROM \"{RETAINED_DEFINITION}\" WHERE \"id\" = \"d\".\"id\") \
                 AND (?3 OR \"d\".\"lifecycle\" = 'active') ORDER BY \"d\".\"id\" LIMIT ?4"
            ),
            params![kind, after.unwrap_or(""), include_retired, i64::from(limit)],
        )?;
        rows.into_iter()
            .map(|values| {
                let row = crate::engine::SqlRow(values);
                let decoded: Result<Row, CellError> = row!(row);
                record(decoded?)
            })
            .collect()
    }
}
