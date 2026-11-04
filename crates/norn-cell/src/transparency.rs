//! RFC 6962 commitments to canonical committed receipt digests (#727).
//!
//! A retained root detects later forks and truncation. These unsigned local
//! checkpoints cannot detect a complete pre-anchor rewrite or isolated-view
//! equivocation. Cell identity must accompany roots out of band. External
//! witnessing and a separate admission-evidence stream remain future work.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ct_merkle::mem_backed_tree::MemoryBackedTree;
use serde::Serialize;
use transparency_sha2::Sha256;

use crate::{CellError, CellStore, engine::SqlEngine};

/// RFC 6962 inclusion path (concatenated SHA-256 hashes).
pub type InclusionProof = ct_merkle::InclusionProof<Sha256>;
/// RFC 6962 consistency path (concatenated SHA-256 hashes).
pub type ConsistencyProof = ct_merkle::ConsistencyProof<Sha256>;
/// RFC 6962 tree root and size; exposes library proof verification methods.
pub type Checkpoint = ct_merkle::RootHash<Sha256>;
pub(crate) type ReceiptLog = MemoryBackedTree<Sha256, String>;

pub(crate) fn load<E: SqlEngine>(conn: &E) -> Result<ReceiptLog, CellError> {
    let mut log = ReceiptLog::new();
    for row in conn.query("SELECT receipt_hash FROM _norn_receipt ORDER BY commit_seq", &[])? {
        log.push(crate::engine::SqlRow(row).get::<String>(0)?);
    }
    Ok(log)
}

/// Deterministic unsigned RFC 6962 `get-sth` JSON. Timestamp zero makes no
/// time claim; a witness must supply its own signed time and log identity.
///
/// # Errors
/// Returns a JSON serialization error.
pub fn checkpoint_json(checkpoint: &Checkpoint) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct TreeHead {
        tree_size: u64,
        timestamp: u64,
        sha256_root_hash: String,
    }
    serde_json::to_string(&TreeHead {
        tree_size: checkpoint.num_leaves(),
        timestamp: 0,
        sha256_root_hash: STANDARD.encode(checkpoint.as_bytes()),
    })
}

impl<E: SqlEngine> CellStore<E> {
    /// Current receipt-stream commitment. Ordinary commits append in O(log n)
    /// hash work after SQL success. Opening rebuilds the O(n) memory cache
    /// from durable receipts; no network is involved.
    pub fn transparency_checkpoint(&self) -> Checkpoint {
        self.transparency.root()
    }

    /// Inclusion at a zero-based receipt index in the current checkpoint.
    /// Returns None for an absent index, including the empty tree.
    pub fn transparency_inclusion(&self, index: usize) -> Option<InclusionProof> {
        self.transparency.get(index)?;
        Some(self.transparency.prove_inclusion(index))
    }

    /// Consistency from a previously retained nonempty tree size to the
    /// current tree. Verify against both retained roots using the library's
    /// `Checkpoint::verify_consistency`. Empty prefixes carry no commitment
    /// and are refused. Equal-size proofs detect conflicting roots too.
    pub fn transparency_consistency(&self, old_size: u64) -> Option<ConsistencyProof> {
        if old_size == 0 || old_size > self.transparency.len() {
            return None;
        }
        let additions = usize::try_from(self.transparency.len() - old_size).ok()?;
        Some(self.transparency.prove_consistency(additions))
    }
}
