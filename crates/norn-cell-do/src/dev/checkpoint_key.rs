//! OPEN-CHECKPOINT-1 (#1551): the dev host's per-cell open-checkpoint key,
//! kept beside the cell in `.norn/checkpoint.key` (owner-only, outside the
//! cell file). Without a readable key the cell opens from genesis.

use std::fmt::Write as _;
use std::path::Path;

use norn_cell::{CheckpointKey, StoreOptions};

/// Open options carrying the dev host's checkpoint key beside `cell_path`.
pub(super) fn dev_store_options(cell_path: &Path) -> StoreOptions {
    StoreOptions {
        checkpoint_key: dev_checkpoint_key(&cell_path.with_file_name("checkpoint.key")),
        ..StoreOptions::default()
    }
}

fn dev_checkpoint_key(path: &Path) -> Option<CheckpointKey> {
    if let Ok(text) = std::fs::read_to_string(path) {
        return CheckpointKey::from_hex(&text).ok();
    }
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).ok()?;
    let text = key
        .iter()
        .fold(String::with_capacity(65), |mut text, byte| {
            let _written = write!(text, "{byte:02x}");
            text
        });
    super::write_private(path, &format!("{text}\n")).ok()?;
    Some(CheckpointKey::from_bytes(key))
}
