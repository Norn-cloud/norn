//! ENGINE-UPGRADE-1 (#1494): the refusals of a cell open whose stored engine
//! version this engine cannot serve. `norn-cell` raises them before any
//! schema statement, definition decoding or history verification touches the
//! file, so a refused open leaves the cell byte-for-byte unchanged.

/// The cell was written by a newer storage version than this engine reads:
/// an older release is opening a cell a newer one already upgraded.
pub const ENGINE_DOWNGRADE: &str = "core.engine.downgrade";

/// The cell's engine stamp is not one this engine can read (a stamp encoding
/// from a future release, or a malformed version), so it cannot order the
/// cell against itself.
pub const ENGINE_UNKNOWN_VERSION: &str = "core.engine.unknown_version";

/// The cell's storage version is older than any this engine upgrades in
/// place, or an ordered migration refused the cell's data.
pub const ENGINE_UPGRADE_UNSUPPORTED: &str = "core.engine.upgrade_unsupported";
