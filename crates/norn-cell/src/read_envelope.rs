//! READ-LOCAL-1: native point reads capture value and frontier in one local snapshot.

use crate::snapshot::Main;
use crate::{CellError, SqliteCell, engine::SqlEngine};
use norn_kernel::authz::CommitBinding;
use norn_kernel::read_envelope::{
    AuthorityPin, Completeness, LocalFrontier, ReadContext, ReadEnvelope, ReadFormat,
    ReadSemantics, ReadToken,
};
use norn_kernel::{CellHost, FactKey, FactState};
use serde::{Serialize, de::DeserializeOwned};

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    E: Serialize + 'static,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    /// Wrap an already authorized local point read in snapshot evidence.
    /// The server supplies the current PDP [`CommitBinding`] after authorization;
    /// this method does not turn a supplied binding into permission to read.
    /// The existing native load profile supports main only: a fork store is
    /// refused ([`CellError::SnapshotWorldMismatch`]), never labelled `main`.
    ///
    /// # Errors
    /// Resource binding mismatch, unqualified legacy cell, or a store read failure.
    pub fn read_envelope(
        &self,
        key: &FactKey,
        authority: &CommitBinding,
    ) -> Result<ReadEnvelope<FactState>, CellError> {
        if authority.resource_kind != key.object_type.as_str() || authority.resource_id != key.key {
            return Err(CellError::Encoding(
                "read authority resource does not match the question".to_owned(),
            ));
        }
        // P5 (#1698): a `Main` snapshot, so a fork store refuses rather than
        // label its rows `main`.
        self.store().snapshot::<Main, _>(|snapshot| {
            let pin = snapshot.pin();
            let value = self.load(key)?;
            Ok(ReadEnvelope {
                token: ReadToken {
                    format: ReadFormat::V1,
                    context: ReadContext {
                        frontier: LocalFrontier {
                            cell: pin.cell.clone(),
                            world: pin.world.encode(),
                            revision: pin.revision,
                        },
                        definition_pin: pin.definitions.clone(),
                        authority_pin: AuthorityPin::from(authority),
                    },
                    completeness: Completeness::Complete,
                    semantics: ReadSemantics::Exact,
                },
                value,
            })
        })
    }
}
