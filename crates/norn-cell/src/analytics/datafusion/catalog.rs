//! D137: a tenant's transactional catalog. One SQLite database per tenant
//! holds every record a reader trusts: the mirror's model, schemas, links,
//! frontiers and segment files; each dataset's load, files and lineage; each
//! store's receipt. Parquet files are immutable objects named by their
//! digest; a file becomes visible only when a catalog commit names it, so the
//! SQLite commit is the one publication point (a crash before it leaves an
//! unnamed object, never a half-read state).

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use transparency_sha2::{Digest as _, Sha256};

use super::super::hex;
use super::{Outcome, engine_error, refusal};

fn sql<T>(value: rusqlite::Result<T>) -> Outcome<T> {
    value.map_err(|error| engine_error(error.to_string()))
}

fn io<T>(value: std::io::Result<T>) -> Outcome<T> {
    value.map_err(|error| engine_error(error.to_string()))
}

/// One mirror segment: the rows of one sync batch of one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Segment {
    /// Its order among the table's segments of its world.
    pub seq: u64,
    /// Its object.
    pub path: PathBuf,
    /// Its digest.
    pub sha256: String,
}

/// One mirrored link edge (METRIC-GRAIN-LINK-1): its state after its last change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Edge {
    /// The source object.
    pub source: String,
    /// The target object.
    pub target: String,
    /// Whether the edge is live.
    pub live: bool,
    /// The commit of its last change.
    pub commit: u64,
}

/// A tenant's catalog.
pub(in crate::analytics) struct Catalog {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS segment (
  tbl TEXT NOT NULL, world TEXT NOT NULL, seq INTEGER NOT NULL,
  path TEXT NOT NULL, sha256 TEXT NOT NULL, PRIMARY KEY (tbl, world, seq));
CREATE TABLE IF NOT EXISTS edge (
  link TEXT NOT NULL, world TEXT NOT NULL, source TEXT NOT NULL, target TEXT NOT NULL,
  live INTEGER NOT NULL, commit_seq INTEGER NOT NULL, PRIMARY KEY (link, world, source, target));
";

impl Catalog {
    /// Open (creating) the catalog at `path`: WAL, every commit synced.
    pub(super) fn open(path: &Path) -> Outcome<Self> {
        let conn = sql(Connection::open(path))?;
        sql(conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;",
        ))?;
        sql(conn.execute_batch(SCHEMA))?;
        Ok(Self { conn })
    }

    /// One read of `key`.
    pub(super) fn meta(&self, key: &str) -> Outcome<Option<String>> {
        get(&self.conn, key)
    }

    /// Run `f` in one write transaction: everything it writes commits
    /// together, or nothing does.
    pub(super) fn write<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Outcome<T>) -> Outcome<T> {
        let tx = sql(self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let value = f(&tx)?;
        sql(tx.commit())?;
        Ok(value)
    }

    /// The segments of `table` in `world`, in order.
    pub(super) fn segments(&self, table: &str, world: &str) -> Outcome<Vec<Segment>> {
        let mut statement = sql(self
            .conn
            .prepare("SELECT seq, path, sha256 FROM segment WHERE tbl = ?1 AND world = ?2 ORDER BY seq"))?;
        let rows = sql(statement.query_map(params![table, world], |row| {
            Ok(Segment {
                seq: row.get::<_, i64>(0).map(|seq| u64::try_from(seq).unwrap_or(0))?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                sha256: row.get(2)?,
            })
        }))?;
        sql(rows.collect())
    }

    /// The edges of `link` in `world`.
    pub(super) fn edges(&self, link: &str, world: &str) -> Outcome<Vec<Edge>> {
        let mut statement = sql(self.conn.prepare(
            "SELECT source, target, live, commit_seq FROM edge WHERE link = ?1 AND world = ?2 ORDER BY source, target",
        ))?;
        let rows = sql(statement.query_map(params![link, world], |row| {
            Ok(Edge {
                source: row.get(0)?,
                target: row.get(1)?,
                live: row.get::<_, i64>(2)? != 0,
                commit: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
            })
        }))?;
        sql(rows.collect())
    }
}

/// `key`'s value in `conn`.
pub(super) fn get(conn: &Connection, key: &str) -> Outcome<Option<String>> {
    sql(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0))
        .optional())
}

/// Set `key` within a write transaction.
pub(super) fn set(tx: &Transaction<'_>, key: &str, value: &str) -> Outcome<()> {
    sql(tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    ))
    .map(drop)
}

/// A frontier record as a number (absent: 0).
pub(super) fn frontier(conn: &Connection, world: &str) -> Outcome<u64> {
    Ok(get(conn, &format!("frontier:{world}"))?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0))
}

/// Clear every mirror record (segments, edges, frontiers, pins).
pub(super) fn clear_mirror(tx: &Transaction<'_>) -> Outcome<()> {
    sql(tx.execute_batch(
        "DELETE FROM segment; DELETE FROM edge; \
         DELETE FROM meta WHERE key LIKE 'frontier:%' OR key LIKE 'pin:%' OR key LIKE 'schema:%' \
         OR key = 'model_pin' OR key = 'links';",
    ))
}

/// Record one segment within a write transaction.
pub(super) fn add_segment(
    tx: &Transaction<'_>,
    table: &str,
    world: &str,
    segment: &Segment,
) -> Outcome<()> {
    sql(tx.execute(
        "INSERT INTO segment (tbl, world, seq, path, sha256) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            table,
            world,
            i64::try_from(segment.seq).map_err(|_e| engine_error("a segment number"))?,
            segment.path.to_string_lossy(),
            segment.sha256
        ],
    ))
    .map(drop)
}

/// The next segment number of `table` in `world`.
pub(super) fn next_segment(tx: &Transaction<'_>, table: &str, world: &str) -> Outcome<u64> {
    let last: Option<i64> = sql(tx.query_row(
        "SELECT max(seq) FROM segment WHERE tbl = ?1 AND world = ?2",
        params![table, world],
        |row| row.get(0),
    ))?;
    Ok(last
        .and_then(|seq| u64::try_from(seq).ok())
        .map_or(1, |seq| seq.saturating_add(1)))
}

/// Upsert one edge within a write transaction.
pub(super) fn put_edge(tx: &Transaction<'_>, link: &str, world: &str, edge: &Edge) -> Outcome<()> {
    sql(tx.execute(
        "INSERT OR REPLACE INTO edge (link, world, source, target, live, commit_seq) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            link,
            world,
            edge.source,
            edge.target,
            i64::from(edge.live),
            i64::try_from(edge.commit).map_err(|_e| engine_error("an edge commit"))?
        ],
    ))
    .map(drop)
}

/// MANIFEST-02: create `dir` if missing and make it this process's alone.
pub(super) fn private_dir(dir: &Path) -> Outcome<()> {
    io(std::fs::create_dir_all(dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        io(std::fs::set_permissions(
            dir,
            std::fs::Permissions::from_mode(0o700),
        ))?;
    }
    Ok(())
}

/// MANIFEST-02: create `path`, which must not exist, for this process alone.
pub(super) fn create_new(path: &Path) -> Outcome<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    io(options.open(path))
}

/// MANIFEST-02: make a written file read-only before it is hashed.
pub(super) fn seal(path: &Path) -> Outcome<()> {
    let failed = |error: std::io::Error| refusal("bulk.unreadable", error.to_string());
    let mut permissions = std::fs::metadata(path).map_err(failed)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions).map_err(failed)
}

/// The SHA-256 and length of a file.
pub(super) fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok((hex(&hasher.finalize()), bytes))
}

/// A kept object must hash to its digest before it is read.
pub(super) fn verify_object(path: &Path, sha256: &str) -> Outcome<()> {
    let (digest, _bytes) =
        sha256_file(path).map_err(|error| refusal("bulk.unreadable", error.to_string()))?;
    if digest == sha256 {
        Ok(())
    } else {
        Err(refusal(
            "bulk.digest_mismatch",
            "a kept dataset file is not the one its manifest records",
        ))
    }
}

/// Make the directory entry of everything renamed or linked into `dir` durable.
pub(super) fn sync_dir(dir: &Path) -> Outcome<()> {
    io(std::fs::File::open(dir).and_then(|dir| dir.sync_all()))
}

/// Install a finished, synced file as the object of its digest in
/// `objects`: sealed, hashed, then linked without clobbering (an object
/// already there must be the same bytes), and the directory synced. The
/// staged name is removed. Nothing names the object until a catalog commit.
pub(super) fn install(staged: &Path, objects: &Path) -> Outcome<(PathBuf, String, u64)> {
    seal(staged)?;
    let (sha256, bytes) = io(sha256_file(staged))?;
    let target = objects.join(format!("{sha256}.parquet"));
    match std::fs::hard_link(staged, &target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_object(&target, &sha256)?;
        }
        Err(error) => return Err(engine_error(error.to_string())),
    }
    io(std::fs::remove_file(staged))?;
    sync_dir(objects)?;
    Ok((target, sha256, bytes))
}

/// A path the scan may read: a regular file (never a link) inside `dir`.
pub(super) fn confined(path: &Path, dir: &Path) -> Outcome<PathBuf> {
    let escape = || refusal("analytics.invalid", "a file outside the tenant's objects");
    let metadata = std::fs::symlink_metadata(path).map_err(|_missing| {
        refusal(
            super::DATASET_COLD,
            "the dataset's files are not kept by this engine",
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(escape());
    }
    let (file, root) = (
        io(std::fs::canonicalize(path))?,
        io(std::fs::canonicalize(dir))?,
    );
    if file.starts_with(&root) {
        Ok(file)
    } else {
        Err(escape())
    }
}
