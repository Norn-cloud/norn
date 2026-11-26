//! The session's state on disk (DEV-CONFINED-1, HOST-REOPEN-1): the cell it
//! reopens, the lock, the session tokens and the owner-only `.norn` files, all
//! inside the project and never written through a symlink.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::CellStore;
use norn_cell::engine::NativeEngine;
use serde_json::Value;

use super::policy::{self, LocalDeveloper};
use super::{Failure, admit, checkpoint_key, load};

/// HOST-REOPEN-1: an existing cell is reopened at its active revision with
/// the model rebuilt from its retained set; nothing is written. A missing
/// or empty file waits for the first admitted revision.
#[allow(
    clippy::type_complexity,
    reason = "the reopened host and what the load did, if anything"
)]
pub(super) fn reopen(
    path: &Path,
    scope: &CellScope,
    (actor, named): (&str, &policy::NamedPrincipals),
    loads: &[PathBuf],
    actions: &policy::DevActions,
) -> Result<(Option<DevHost<NativeEngine>>, Option<load::Loaded>), Failure> {
    let unreadable = |error: String| {
        Failure::setup(
            "norn.cell.unreadable",
            format!("{}: {error}", path.display()),
        )
    };
    if !path.exists() {
        return Ok((None, None));
    }
    let engine = NativeEngine::open(path, checkpoint_key::dev_store_options(path))
        .map_err(|error| unreadable(error.to_string()))?;
    let Some(active) = CellStore::read_active_definitions(&engine)
        .map_err(|error| unreadable(error.to_string()))?
    else {
        return Ok((None, None));
    };
    let model = admit::installed_model(&active).map_err(unreadable)?;
    let store =
        CellStore::from_engine(engine, model).map_err(|error| unreadable(error.to_string()))?;
    let (store, loaded) = if loads.is_empty() {
        (store, None)
    } else {
        let (store, loaded) = load::load(store, &scope.key(), loads)
            .map_err(|message| Failure::setup("norn.dev.load_refused", message))?;
        (store, Some(loaded))
    };
    let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
        .map_err(|refusal| Failure::setup("norn.cell.scope_mismatch", refusal.message))?
        .with_authorizer(
            LocalDeveloper::for_actor(actor, Arc::clone(actions)).with_named(Arc::clone(named)),
        );
    Ok((Some(host), loaded))
}

/// One running session per project: `.norn/dev.lock` is held exclusively
/// for the life of the process.
pub(super) fn lock(state: &Path) -> Result<File, Failure> {
    let path = state.join("dev.lock");
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Never create or open the lock through a symlink.
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(_held) => {
            let running = checkpoint_key::read_private(&state.join("dev.json"), 64 * 1024)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .map_or_default(|session| {
                    format!(
                        " (pid {}, {})",
                        session.get("pid").and_then(Value::as_u64).unwrap_or(0),
                        session.get("url").and_then(Value::as_str).unwrap_or("?")
                    )
                });
            Err(Failure::setup(
                "norn.cell.locked",
                format!("norn dev is already running for this project{running}; stop it first"),
            ))
        }
    }
}

pub(super) struct Token {
    pub(super) bearer: String,
    /// The local reviewer's session token (DEV-REVIEWER-1).
    pub(super) reviewer: String,
    /// Each named principal's session token, in manifest order
    /// (DEV-PRINCIPALS-1).
    pub(super) principals: Vec<String>,
    pub(super) handle_key: [u8; 32],
}

impl Drop for Token {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.bearer);
        zeroize::Zeroize::zeroize(&mut self.reviewer);
        for principal in &mut self.principals {
            zeroize::Zeroize::zeroize(principal);
        }
        zeroize::Zeroize::zeroize(&mut self.handle_key);
    }
}

/// Fresh session tokens: the developer's, the reviewer's and one for each of
/// `named` principals, plus the handle key.
pub(super) fn token(named: usize) -> Result<Token, Failure> {
    let random = |bytes: &mut [u8]| {
        getrandom::fill(bytes)
            .map_err(|error| Failure::setup("norn.dev.io", format!("no randomness: {error}")))
    };
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                let _written = write!(hex, "{byte:02x}");
                hex
            })
    };
    let mut bytes = zeroize::Zeroizing::new([0_u8; 96]);
    random(&mut *bytes)?;
    let (bearers, key) = bytes.split_at(64);
    let (bearer, reviewer) = bearers.split_at(32);
    let mut handle_key = [0_u8; 32];
    handle_key.copy_from_slice(key);
    let principals = (0..named)
        .map(|_| {
            let mut bearer = zeroize::Zeroizing::new([0_u8; 32]);
            random(&mut *bearer)?;
            Ok(hex(&*bearer))
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    Ok(Token {
        bearer: hex(bearer),
        reviewer: hex(reviewer),
        principals,
        handle_key,
    })
}

pub(super) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// The project's `.norn` state directory, created owner-only. A symlinked
/// `.norn` refuses: the session token must never land outside the project.
pub(super) fn private_dir(root: &Path) -> Result<PathBuf, Failure> {
    let state = root.join(".norn");
    if is_symlink(&state) {
        return Err(Failure::setup(
            "norn.dev.unsafe_state",
            format!(
                "{} is a symlink; norn dev keeps its session token only in a real .norn directory",
                state.display()
            ),
        ));
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(&state)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", state.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| Failure::setup("norn.dev.io", format!("{}: {error}", state.display())),
        )?;
    }
    Ok(state)
}

/// The manifest's cell, which must be a relative path inside the project:
/// no absolute path, no `..`, and no symlinked directory leading out of it.
/// `norn dev` grants everything to the local developer, so it must never be
/// pointable at another cell on the machine.
pub(super) fn cell_path(root: &Path, cell: &str) -> Result<PathBuf, Failure> {
    use std::path::Component;
    let outside = || {
        Failure::setup(
            "norn.project.cell_outside",
            format!("norn.json cell {cell:?} must be a relative path inside the project"),
        )
    };
    let relative = Path::new(cell);
    let mut parts = relative.components();
    let valid_parts = !relative.as_os_str().is_empty()
        && !matches!(cell.rsplit(['/', '\\']).next(), Some("" | "." | ".."))
        && matches!(parts.next_back(), Some(Component::Normal(_)))
        && parts.all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
    if !valid_parts {
        return Err(outside());
    }
    let path = root.join(relative);
    let parent = path.parent().ok_or_else(outside)?;
    // Nothing is created until the deepest directory that already exists is
    // known to be inside the project: a symlinked ancestor leads out of it.
    let existing = parent
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or_else(outside)?;
    let within = |directory: &Path| -> Result<bool, Failure> {
        directory
            .canonicalize()
            .map(|real| real.starts_with(root))
            .map_err(|error| {
                Failure::setup("norn.dev.io", format!("{}: {error}", directory.display()))
            })
    };
    if !within(existing)? {
        return Err(outside());
    }
    std::fs::create_dir_all(parent)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", parent.display())))?;
    if !within(parent)? || is_symlink(&path) {
        return Err(outside());
    }
    Ok(path)
}

/// Write a session file readable by the owner only. Atomically replaces via
/// a sibling `O_EXCL`/`O_NOFOLLOW` mode-0600 file so concurrent readers never
/// observe `ENOENT`/partial JSON and older descriptors keep the old inode.
pub(super) fn write_private(path: &Path, text: &str) -> Result<(), Failure> {
    if is_symlink(path) {
        return Err(Failure::setup(
            "norn.dev.unsafe_state",
            format!(
                "{} is a symlink; norn dev refuses to write a session file through it",
                path.display()
            ),
        ));
    }
    checkpoint_key::write_atomic_private(path, text)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", path.display())))
}
