//! RECEIPT-STORE-1: only authenticated reconciliation populates the receipt store.
//! The store survives reopen; event restore and unavailable adapters refuse closed.

use crate::{
    CellError, CommitBatch, SqliteCell, StoreReader,
    engine::{SqlEngine, params},
    store::{GuardedError, MAIN_WORLD},
};
use norn_ir::{
    RuntimeValue,
    function::{Digest, FunctionReceipt, MemberPin, PredicatePin},
};
use norn_kernel::{
    FactKey, FactVersion,
    result_reconciliation::{
        Acceptance, CommandState, Completion, DispatchedCommand, ReceiptValidator,
        ReconcileContext, ReconcilePolicy, Reconciliation, ResultAuthorizer, ResultChecks,
        StateView, reconcile,
    },
};
use serde::{Deserialize, Serialize};

const LIMIT: usize = 1_048_576;
const EVENT: &str = "norn.function.receipt_recorded";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StoredReceipt {
    pub(crate) receipt: FunctionReceipt,
    pub(crate) acceptance: Acceptance,
    pub(crate) frontier: u64,
    pub(crate) source: u64,
}

fn refused(reason: &str) -> CellError {
    CellError::Engine(format!("action.receipt_{reason}"))
}

struct ReceiptView<'a, 'b, S: SqlEngine>(&'a StoreReader<'b, S>);
impl<S: SqlEngine> StateView for ReceiptView<'_, '_, S> {
    type Error = CellError;
    fn fact_version(&self, key: &FactKey) -> Result<FactVersion, CellError> {
        Ok(FactVersion(self.0.version(
            MAIN_WORLD,
            key.object_type.as_str(),
            &key.key,
        )?))
    }
    fn predicate_members(&self, _: &PredicatePin) -> Result<Vec<MemberPin>, CellError> {
        Err(refused("predicate_adapter_unavailable"))
    }
    fn collection_epoch(&self, _: &str) -> Result<u64, CellError> {
        Err(refused("collection_adapter_unavailable"))
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// Reconcile an authenticated performer completion and retain its receipt.
    /// The actual dispatched payload, recorded result and command state are
    /// loaded from this cell. This records evidence, never business effects.
    ///
    /// # Errors
    /// Scope, dispatch, authentication, freshness, schema or storage refusal.
    pub fn retain_function_receipt<A: ResultAuthorizer, R: ReceiptValidator>(
        &mut self,
        dispatched: &DispatchedCommand,
        completion: &Completion<'_>,
        checks: &ResultChecks<'_, A, R>,
    ) -> Result<Digest, CellError> {
        let reference = completion
            .receipt
            .content_digest()
            .map_err(|_invalid| refused("digest"))?;
        let scope = self.scope.clone();
        let now = self.now;
        let batch = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: scope.clone(),
            logical_time: now.as_millis(),
            events: vec![
                norn_ir::runtime_object! { "type" => EVENT, "reference" => reference.0.clone() },
            ],
            ..CommitBatch::default()
        };
        self.store.commit_guarded(&batch, |reader| -> Result<(), CellError> {
            if dispatched.invocation.tenant != scope || dispatched.invocation.world != MAIN_WORLD { return Err(refused("scope")); }
            let pending = reader.head_command()?.ok_or_else(|| refused("unknown"))?;
            let invocation = norn_ir::runtime::encode(&dispatched.invocation).map_err(|_invalid| refused("dispatch"))?;
            let receipt = norn_ir::runtime::encode(completion.receipt).map_err(|_invalid| refused("body"))?;
            if pending.command.command_id != dispatched.invocation.command_id
                || pending.command.generation != dispatched.generation().0
                || pending.command.command.get("payload") != Some(&invocation)
                || pending.result.as_ref() != Some(&receipt) { return Err(refused("dispatch")); }
            let command = reader.object(MAIN_WORLD, "norn.FunctionCommand", &dispatched.invocation.command_id)?.ok_or_else(|| refused("state"))?;
            let state: CommandState = norn_ir::runtime::decode(command.values.get("state").ok_or_else(|| refused("state"))?).map_err(|_invalid| refused("state"))?;
            let context = ReconcileContext { tenant: &scope, world: MAIN_WORLD, now, policy: ReconcilePolicy::DEFAULT };
            let Reconciliation::Accepted(mut acceptance) = reconcile(&context, dispatched, &state, completion, checks, &ReceiptView(reader)).map_err(|error| match error { norn_kernel::result_reconciliation::ReconcileError::View(view) => view, norn_kernel::result_reconciliation::ReconcileError::Canonical(_) => refused("reconciliation") })? else { return Err(refused("not_accepted")); };
            acceptance.reads.push(norn_kernel::FactExpectation { key: FactKey::new("norn.FunctionCommand", &dispatched.invocation.command_id), expected: FactVersion(command.version) });
            let source = reader.next_commit_seq()?;
            let frontier = source.checked_add(1).ok_or_else(|| refused("frontier"))?;
            let stored = StoredReceipt { receipt: completion.receipt.clone(), acceptance, source, frontier };
            let body = serde_json::to_string(&stored).map_err(CellError::Json)?;
            if body.len() > LIMIT { return Err(refused("limit")); }
            let retained = reader.conn.query_optional("SELECT 1 FROM _norn_function_receipt WHERE reference = ?1", params![reference.0], |row| row.get::<i64>(0))?;
            if retained.is_some() { return Err(refused("already_retained")); }
            reader.conn.execute("INSERT INTO _norn_function_receipt(reference,scope,body) VALUES (?1,?2,?3)", params![reference.0, scope, body])?;
            Ok(())
        }, &mut |_| Ok(())).map_err(|error| match error { GuardedError::Guard(error) | GuardedError::Store(error) => error })?;
        Ok(reference)
    }

    pub(crate) fn stored_function_receipt(
        &self,
        reference: &Digest,
    ) -> Result<Option<StoredReceipt>, CellError> {
        let row = self.store.conn.query_optional("SELECT body FROM _norn_function_receipt WHERE reference=?1 AND scope=?2 AND length(body)<=?3", params![reference.0, self.scope, i64::try_from(LIMIT).map_err(|_invalid| refused("limit"))?], |row| row.get::<String>(0))?;
        let Some(body) = row else {
            return Ok(None);
        };
        let stored: StoredReceipt = serde_json::from_str(&body).map_err(CellError::Json)?;
        if stored
            .receipt
            .content_digest()
            .map_err(|_invalid| refused("digest"))?
            != *reference
        {
            return Err(refused("digest"));
        }
        self.store.verified_head()?;
        let source = self
            .store
            .export_record(stored.source)?
            .ok_or_else(|| refused("source"))?;
        if source.batch.scope != self.scope
            || source.batch.world != MAIN_WORLD
            || !source.batch.events.iter().any(|event| {
                event.get("type").and_then(RuntimeValue::as_str) == Some(EVENT)
                    && event.get("reference").and_then(RuntimeValue::as_str)
                        == Some(reference.0.as_str())
            })
        {
            return Err(refused("source"));
        }
        Ok(Some(stored))
    }

    /// Export retained receipt evidence with its authenticated recorded clock.
    /// This read is not acceptance, consumption or publication authority.
    ///
    /// # Errors
    /// Corrupt reference, source proof, JSON or host storage.
    pub fn function_receipt_evidence(
        &self,
        reference: &Digest,
    ) -> Result<Option<(FunctionReceipt, crate::Receipt)>, CellError> {
        let Some(stored) = self.stored_function_receipt(reference)? else {
            return Ok(None);
        };
        let source = self
            .store
            .read()
            .receipt_at(stored.source)?
            .ok_or_else(|| refused("source"))?;
        Ok(Some((stored.receipt, source)))
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "receipt intake scenarios"
)]
mod tests {
    use super::*;
    use crate::{CellStore, EffectRow, FactWriteRow, Model, OutboxCommand, StoreOptions};
    use norn_ir::function::{FunctionInvocation, FunctionManifest};
    use norn_kernel::result_reconciliation::{InvalidReceipt, ResultAuthzRequest, ResultDenial};
    use norn_kernel::{
        CellHost, Command, CommandId, FreshnessPolicy, Generation, IdempotencyKey, LogicalTime,
    };
    use norn_testkit::proptest::prelude::*;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    fn fixture<T: DeserializeOwned>(name: &str) -> T {
        serde_json::from_str(
            &std::fs::read_to_string(format!(
                "{}/../../fixtures/function/{name}.wasm.json",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("fixture"),
        )
        .expect("decode")
    }
    struct Checks(norn_checker::function::AdmittedFunction);
    impl ResultAuthorizer for Checks {
        fn authorize(&self, _: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
            Ok(())
        }
    }
    impl ReceiptValidator for Checks {
        fn validate(
            &self,
            dispatched: &DispatchedCommand,
            receipt: &FunctionReceipt,
        ) -> Result<(), InvalidReceipt> {
            let findings = norn_checker::function::check_function_receipt(
                receipt,
                &dispatched.invocation,
                &self.0,
            );
            if findings.is_empty() {
                Ok(())
            } else {
                Err(InvalidReceipt {
                    reason: format!("{findings:?}"),
                })
            }
        }
    }
    type Cell = SqliteCell<RuntimeValue, FunctionInvocation, FunctionReceipt>;
    fn setup(path: &std::path::Path) -> (Cell, DispatchedCommand, FunctionReceipt, Checks, Model) {
        let manifest: FunctionManifest = fixture("manifest");
        let checks =
            Checks(norn_checker::function::admit_function_manifest(&manifest).expect("manifest"));
        let mut invocation: FunctionInvocation = fixture("invocation");
        invocation.freshness = norn_ir::function::FreshnessMode::AppendOnly;
        invocation.destination = None;
        let dispatched = DispatchedCommand::pin(invocation, checks.0.execution_profile.clone())
            .expect("dispatch");
        let mut receipt: FunctionReceipt = fixture("receipt");
        receipt.dispatch = dispatched.dispatch.clone();
        receipt.input_identity = dispatched.input_identity.clone();
        receipt.dependency_token = dispatched.dependency_token.clone();
        let definition = serde_json::from_value(json!({"name":"norn.FunctionCommand","properties":[{"id":"state","type":{"kind":"json-ext"}}]})).expect("type");
        let model = Model::from_definitions(&[definition], &[]).expect("model");
        let mut store =
            CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
        let command = Command {
            id: CommandId(dispatched.invocation.command_id.clone()),
            generation: Generation(dispatched.generation().0),
            idempotency_key: IdempotencyKey("receipt-test".into()),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: dispatched.invocation.clone(),
        };
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: dispatched.invocation.tenant.clone(),
                logical_time: 1_790_000_000_000,
                writes: vec![FactWriteRow::Put {
                    object_type: "norn.FunctionCommand".into(),
                    key: command.id.0.clone(),
                    values: norn_ir::RuntimeObject::from([(
                        "state".into(),
                        norn_ir::runtime::encode(&CommandState::dispatched(&dispatched))
                            .expect("state"),
                    )]),
                }],
                effects: vec![EffectRow::Enqueue {
                    command: OutboxCommand {
                        command_id: command.id.0.clone(),
                        generation: command.generation.0,
                        idempotency_key: command.idempotency_key.0.clone(),
                        deadline: None,
                        command: norn_ir::runtime::encode(&command).expect("command"),
                    },
                }],
                ..CommitBatch::default()
            })
            .expect("enqueue");
        let result = receipt.clone();
        let mut cell = Cell::new(store, dispatched.invocation.tenant.clone(), move |_| {
            result.clone()
        })
        .expect("cell");
        assert!(cell.perform().expect("perform").is_some());
        (cell, dispatched, receipt, checks, model)
    }
    fn retain(
        cell: &mut Cell,
        dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
        checks: &Checks,
        authenticated: bool,
    ) -> Result<Digest, CellError> {
        let command = CommandId(dispatched.invocation.command_id.clone());
        cell.retain_function_receipt(
            dispatched,
            &Completion {
                receipt,
                command: &command,
                generation: Generation(dispatched.generation().0),
                authenticated_executor: authenticated
                    .then_some(receipt.provenance.executor.as_str()),
            },
            &ResultChecks {
                authorizer: checks,
                validator: checks,
            },
        )
    }
    #[test]
    fn authenticated_receipt_survives_reopen_with_recorded_time_and_source() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("cell.sqlite");
        let (mut cell, dispatched, receipt, checks, model) = setup(&path);
        cell.advance_to(LogicalTime::from_millis(1_790_000_000_100));
        let reference = retain(&mut cell, &dispatched, &receipt, &checks, true).expect("retained");
        let evidence = cell
            .function_receipt_evidence(&reference)
            .expect("read")
            .expect("evidence");
        assert!(retain(&mut cell, &dispatched, &receipt, &checks, true).is_err());
        assert_eq!(evidence.0, receipt);
        assert_eq!(evidence.1.body.logical_time, 1_790_000_000_100);
        drop(cell);
        let cell = Cell::new(
            CellStore::open(&path, model, StoreOptions::default()).expect("reopen"),
            dispatched.invocation.tenant,
            |_| panic!("no execution"),
        )
        .expect("host");
        assert_eq!(
            cell.function_receipt_evidence(&reference).expect("read"),
            Some(evidence)
        );
        assert!(
            cell.function_receipt_evidence(&Digest(format!("sha256:{}", "0".repeat(64))))
                .expect("unknown")
                .is_none()
        );
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]
        #[test]
        fn forged_or_wrong_scope_intake_never_persists(suffix in "[a-z]{1,8}", wrong_scope in any::<bool>()) {
            let dir = tempfile::tempdir().expect("dir");
            let (mut cell, mut dispatched, receipt, checks, _) = setup(&dir.path().join("cell.sqlite"));
            let before = cell.store().verified_head().expect("head");
            if wrong_scope { dispatched.invocation.tenant.push_str(&suffix); }
            prop_assert!(retain(&mut cell, &dispatched, &receipt, &checks, wrong_scope).is_err());
            prop_assert_eq!(cell.store().verified_head().expect("head"), before);
            prop_assert!(cell.function_receipt_evidence(&receipt.content_digest().expect("digest")).expect("read").is_none());
        }
    }
}
