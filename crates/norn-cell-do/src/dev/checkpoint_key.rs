//! OPEN-CHECKPOINT-1 (#1551): the dev host's per-cell open-checkpoint key,
//! kept beside the cell in `.norn/checkpoint.key` (owner-only, outside the
//! cell file). Without a readable key the cell opens from genesis.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_cell::{CheckpointKey, StoreOptions};
use zeroize::Zeroizing;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Open options carrying the dev host's checkpoint key beside `cell_path`.
pub(super) fn dev_store_options(cell_path: &Path) -> StoreOptions {
    StoreOptions {
        checkpoint_key: dev_checkpoint_key(&cell_path.with_file_name("checkpoint.key")),
        ..StoreOptions::default()
    }
}

/// Atomically create a new owner-only (`0o600`, `O_NOFOLLOW`) sibling temp
/// file and rename it onto `path` so concurrent readers never observe
/// `ENOENT` or partial JSON, and older descriptors keep reading the old inode.
pub(super) fn write_atomic_private(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session");
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.tmp.{}.{seq}", std::process::id()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = options
        .open(&tmp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ignored = std::fs::remove_file(&tmp);
    }
    result
}

/// Read a bounded regular file under `.norn` without following symlinks or
/// blocking on a FIFO (DEV-CONFINED-1).
pub(super) fn read_private(path: &Path, limit: u64) -> std::io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds host bound",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn dev_checkpoint_key(path: &Path) -> Option<CheckpointKey> {
    if let Ok(text) = read_private(path, 128).map(Zeroizing::new) {
        return CheckpointKey::from_hex(&text).ok();
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    getrandom::fill(&mut *key).ok()?;
    let mut text = Zeroizing::new(String::with_capacity(65));
    for byte in key.iter() {
        let _written = write!(&mut *text, "{byte:02x}");
    }
    text.push('\n');
    super::write_private(path, &text).ok()?;
    Some(CheckpointKey::from_bytes(*key))
}
