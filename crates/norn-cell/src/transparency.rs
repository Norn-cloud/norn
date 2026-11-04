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

use crate::{
    CellError, CellStore,
    engine::{SqlEngine, params},
};

const MAX_TREE_SIZE: u64 = 1_u64 << 63;

/// Invalid checkpoint, proof encoding, or proof verification.
#[derive(Debug, thiserror::Error)]
#[error("invalid transparency proof or checkpoint")]
pub struct ProofError;

/// RFC 6962 inclusion path with checked byte decoding.
#[derive(Clone, Debug)]
pub struct InclusionProof(ct_merkle::InclusionProof<Sha256>);

impl InclusionProof {
    /// Decode concatenated SHA-256 hashes.
    ///
    /// # Errors
    /// Rejects partial hashes and paths longer than 63 hashes.
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, ProofError> {
        if bytes.len() > 63 * 32 {
            return Err(ProofError);
        }
        ct_merkle::InclusionProof::try_from_bytes(bytes)
            .map(Self)
            .map_err(|_invalid_proof| ProofError)
    }

    /// Concatenated SHA-256 hashes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// RFC 6962 consistency path with checked byte decoding.
#[derive(Clone, Debug)]
pub struct ConsistencyProof(ct_merkle::ConsistencyProof<Sha256>);

impl ConsistencyProof {
    /// Decode concatenated SHA-256 hashes.
    ///
    /// # Errors
    /// Rejects partial hashes and paths longer than 64 hashes.
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, ProofError> {
        if bytes.len() > 64 * 32 {
            return Err(ProofError);
        }
        ct_merkle::ConsistencyProof::try_from_bytes(bytes)
            .map(Self)
            .map_err(|_invalid_proof| ProofError)
    }

    /// Concatenated SHA-256 hashes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// RFC 6962 tree root and size with checked proof verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint(ct_merkle::RootHash<Sha256>);

impl Checkpoint {
    /// Decode a retained root and size without trusting their contents.
    /// Verification validates the size before invoking the Merkle library.
    #[must_use]
    pub fn new(digest: [u8; 32], num_leaves: u64) -> Self {
        Self(ct_merkle::RootHash::new(digest.into(), num_leaves))
    }

    /// Number of committed leaves claimed by this checkpoint.
    #[must_use]
    pub fn num_leaves(&self) -> u64 {
        self.0.num_leaves()
    }

    /// SHA-256 root digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Verify inclusion of a canonical receipt digest at a zero-based index.
    ///
    /// # Errors
    /// Rejects sizes above 2^63, absent indices, malformed paths and root mismatches.
    pub fn verify_inclusion<T: AsRef<[u8]>>(
        &self,
        value: &T,
        index: u64,
        proof: &InclusionProof,
    ) -> Result<(), ProofError> {
        let size = self.num_leaves();
        if size > MAX_TREE_SIZE || index >= size {
            return Err(ProofError);
        }
        // The index helper is safe only after the size and index checks above.
        let hashes = ct_merkle::indices_for_inclusion_proof(size, index).len();
        if proof.as_bytes().len() != hashes.checked_mul(32).ok_or(ProofError)? {
            return Err(ProofError);
        }
        self.0
            .verify_inclusion(value, index, &proof.0)
            .map_err(|_invalid_proof| ProofError)
    }

    /// Verify that this tree extends a retained nonempty checkpoint.
    ///
    /// # Errors
    /// Rejects sizes outside 0 < old <= new <= 2^63, malformed paths and forks.
    pub fn verify_consistency(
        &self,
        old: &Self,
        proof: &ConsistencyProof,
    ) -> Result<(), ProofError> {
        let size = self.num_leaves();
        let old_size = old.num_leaves();
        if size > MAX_TREE_SIZE || old_size == 0 || old_size > size {
            return Err(ProofError);
        }
        let additions = size.checked_sub(old_size).ok_or(ProofError)?;
        let hashes = ct_merkle::indices_for_consistency_proof(old_size, additions).len();
        if proof.as_bytes().len() != hashes.checked_mul(32).ok_or(ProofError)? {
            return Err(ProofError);
        }
        self.0
            .verify_consistency(&old.0, &proof.0)
            .map_err(|_invalid_proof| ProofError)
    }
}
pub(crate) type ReceiptLog = MemoryBackedTree<Sha256, String>;

pub(crate) fn load<E: SqlEngine>(conn: &E) -> Result<ReceiptLog, CellError> {
    let mut log = ReceiptLog::new();
    for row in conn.query(
        "SELECT receipt_hash FROM _norn_receipt ORDER BY commit_seq",
        &[],
    )? {
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
    pub(crate) fn refresh_transparency(&self) -> Result<(), CellError> {
        let mut log = self.transparency.borrow_mut();
        let from = crate::read::to_i64(log.len(), "transparency size")?;
        for row in self.conn.query(
            "SELECT receipt_hash FROM _norn_receipt WHERE commit_seq > ?1 ORDER BY commit_seq",
            params![from],
        )? {
            log.push(crate::engine::SqlRow(row).get::<String>(0)?);
        }
        Ok(())
    }

    /// Current receipt-stream commitment. Ordinary commits append in O(log n)
    /// hash work after SQL success. Opening rebuilds the O(n) memory cache
    /// from durable receipts; no network is involved. Catching up k commits
    /// from another handle costs O(k log n); the cell runtime owns commit order.
    ///
    /// # Errors
    /// SQL failures while catching up receipts from another store handle.
    pub fn transparency_checkpoint(&self) -> Result<Checkpoint, CellError> {
        self.refresh_transparency()?;
        Ok(Checkpoint(self.transparency.borrow().root()))
    }

    /// Inclusion at a zero-based receipt index in the current checkpoint.
    /// Returns None for an absent index, including the empty tree.
    ///
    /// # Errors
    /// SQL failures while catching up receipts from another store handle.
    pub fn transparency_inclusion(
        &self,
        index: usize,
    ) -> Result<Option<InclusionProof>, CellError> {
        self.refresh_transparency()?;
        let log = self.transparency.borrow();
        Ok(log
            .get(index)
            .map(|_| InclusionProof(log.prove_inclusion(index))))
    }

    /// Consistency from a previously retained nonempty tree size to the
    /// current tree. Verify against both retained roots using Norn's checked
    /// `Checkpoint::verify_consistency`. Empty prefixes carry no commitment
    /// and are refused. Equal-size proofs detect conflicting roots too.
    ///
    /// # Errors
    /// SQL failures or a tree too large for this platform.
    pub fn transparency_consistency(
        &self,
        old_size: u64,
    ) -> Result<Option<ConsistencyProof>, CellError> {
        self.refresh_transparency()?;
        let log = self.transparency.borrow();
        if old_size == 0 || old_size > log.len() {
            return Ok(None);
        }
        let additions = log
            .len()
            .checked_sub(old_size)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or(CellError::OutOfRange("transparency size"))?;
        Ok(Some(ConsistencyProof(log.prove_consistency(additions))))
    }
}
