//! READ-LOCAL-1: native point reads capture value and frontier in one local snapshot.

use crate::{CellError, SqliteCell, engine::SqlEngine};
use norn_kernel::authz::CommitBinding;
use norn_kernel::read_envelope::{
    AuthorityPin, Completeness, DefinitionPin, LocalFrontier, ReadContext, ReadEnvelope,
    ReadFormat, ReadSemantics, ReadToken,
};
use norn_kernel::{CellHost, FactKey, FactState};
use serde::{Serialize, de::DeserializeOwned};

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    E: Serialize,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    /// Wrap an already authorized local point read in snapshot evidence.
    /// The server supplies the current PDP [`CommitBinding`] after authorization;
    /// this method does not turn a supplied binding into permission to read.
    /// The existing native load profile supports main only, never a caller-labelled fork.
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
        let store = self.store();
        store.conn.transaction(|| {
            let cell = store.cell_id()?.ok_or_else(|| {
                CellError::Encoding(
                    "unqualified history cannot issue a portable read token".to_owned(),
                )
            })?;
            let head = store.verified_head()?;
            let installed = store.read().installed_definitions(None)?;
            if let Some(definitions) = &installed
                && definitions.model_hash != store.model().model_hash()
            {
                return Err(CellError::SchemaMismatch {
                    expected: store.model().model_hash().to_owned(),
                    found: definitions.model_hash.clone(),
                });
            }
            let definition_pin = DefinitionPin {
                version: installed
                    .as_ref()
                    .map_or(0, |definitions| definitions.version),
                model_hash: store.model().model_hash().to_owned(),
                admission_receipt: installed.map(|definitions| definitions.receipt_sha256),
            };
            let value = self.load(key)?;
            Ok(ReadEnvelope {
                token: ReadToken {
                    format: ReadFormat::V1,
                    context: ReadContext {
                        frontier: LocalFrontier {
                            cell,
                            world: crate::MAIN_WORLD.to_owned(),
                            revision: head.commits,
                        },
                        definition_pin,
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
