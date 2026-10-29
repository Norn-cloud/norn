//! Shared UTF-8 byte budgets for new admissions and historical reads.
//!
//! These are byte counts, independent of Unicode normalization. Names leave
//! ample room for namespaced ERPNext DocTypes/actions; keys retain the 4 KiB
//! object identity budget. Validators live in the checker and kernel. Admission
//! limits govern new definitions and writes, never existing stored identities.
//! Historical reads retain their original tolerances; retained definitions use
//! the identifier policy recorded by their verified admission receipt.

/// Identifier admission profile for new definitions and writes.
pub const IDENTIFIER_ADMISSION_PROFILE: &str = "norn-identifiers/1";
/// Identifier rules of definitions admitted before byte limits were introduced.
pub const HISTORICAL_IDENTIFIER_PROFILE: &str = "norn-identifiers/legacy";
/// Original `acted_by` index read cutoff, retained for historical `ActionCall` records.
pub const HISTORICAL_ACTION_CALL_READ_BYTES: usize = 4_096;

/// Largest ordinary object key.
pub const MAX_OBJECT_KEY_BYTES: usize = 4_096;
/// Largest object type or interface name, including its namespace.
pub const MAX_OBJECT_TYPE_NAME_BYTES: usize = 256;
/// Largest action ID or definition name.
pub const MAX_ACTION_ID_BYTES: usize = 256;
/// Largest action idempotency key (#3089).
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
/// Largest link name (including inverse link names).
pub const MAX_LINK_NAME_BYTES: usize = 256;
/// Largest `actionId/idempotencyKey` ActionCall identity (#3098).
pub const MAX_ACTION_CALL_ID_BYTES: usize = MAX_ACTION_ID_BYTES + 1 + MAX_IDEMPOTENCY_KEY_BYTES;
/// Largest `objectType/key` observed-tombstone identity.
pub const MAX_TOMBSTONE_KEY_BYTES: usize = MAX_OBJECT_TYPE_NAME_BYTES + 1 + MAX_OBJECT_KEY_BYTES;
/// Largest stored object identity, including kernel-composed system keys.
pub const MAX_STORED_OBJECT_KEY_BYTES: usize = MAX_TOMBSTONE_KEY_BYTES;
