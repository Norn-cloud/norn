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

struct Bound<'a>(&'a StoredReceipt);
impl norn_kernel::receipt_binding::AcceptedReceiptStore for Bound<'_> {
    type Error = CellError;
    fn resolve(
        &self,
        _: &Digest,
    ) -> Result<norn_kernel::receipt_binding::ReceiptResolution<'_>, CellError> {
        Ok(norn_kernel::receipt_binding::ReceiptResolution::Accepted {
            acceptance: &self.0.acceptance,
            receipt: &self.0.receipt,
        })
    }
}

pub(crate) fn refused(reason: &'static str) -> CellError {
    CellError::ReceiptRefused(reason)
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
    /// RECEIPT-STORE-AUTH-1: reconcile and retain an authenticated performer completion.
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
        if dispatched.invocation.tenant != scope || dispatched.invocation.world != MAIN_WORLD {
            return Err(refused("scope"));
        }
        self.store.verified_head()?;
        let reader = self.store.read();
        let source = reader.next_commit_seq()?;
        let pending = reader.head_command()?.ok_or_else(|| refused("unknown"))?;
        let invocation = norn_ir::runtime_object! { "type" => "norn.function/v1", "invocation" => norn_ir::runtime::encode(&dispatched.invocation).map_err(|_invalid| refused("dispatch"))? };
        let receipt =
            norn_ir::runtime::encode(completion.receipt).map_err(|_invalid| refused("body"))?;
        if pending.command.command_id != dispatched.invocation.command_id
            || pending.command.generation != dispatched.generation().0
            || pending.command.command.get("payload") != Some(&invocation)
            || pending.result.as_ref() != Some(&receipt)
        {
            return Err(refused("dispatch"));
        }
        let command = reader
            .object(
                MAIN_WORLD,
                "norn.FunctionCommand",
                &dispatched.invocation.command_id,
            )?
            .ok_or_else(|| refused("state"))?;
        let state: CommandState = norn_ir::runtime::decode(
            command
                .values
                .get("state")
                .ok_or_else(|| refused("state"))?,
        )
        .map_err(|_invalid| refused("state"))?;
        let context = ReconcileContext {
            tenant: &scope,
            world: MAIN_WORLD,
            now,
            policy: ReconcilePolicy::DEFAULT,
        };
        let Reconciliation::Accepted(mut acceptance) = reconcile(
            &context,
            dispatched,
            &state,
            completion,
            checks,
            &ReceiptView(&reader),
        )
        .map_err(|error| match error {
            norn_kernel::result_reconciliation::ReconcileError::View(view) => view,
            norn_kernel::result_reconciliation::ReconcileError::Canonical(_) => {
                refused("reconciliation")
            }
        })?
        else {
            return Err(refused("not_accepted"));
        };
        acceptance.reads.push(norn_kernel::FactExpectation {
            key: FactKey::new("norn.FunctionCommand", &dispatched.invocation.command_id),
            expected: FactVersion(command.version),
        });

        let frontier = source.checked_add(1).ok_or_else(|| refused("frontier"))?;
        let stored = StoredReceipt {
            receipt: completion.receipt.clone(),
            acceptance,
            source,
            frontier,
        };
        let body = serde_json::to_string(&stored).map_err(CellError::Json)?;
        if body.len() > LIMIT {
            return Err(refused("limit"));
        }
        let authority = crate::codec::hash_json(&stored)?;
        let batch = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: scope.clone(),
            logical_time: now.as_millis(),
            events: vec![
                norn_ir::runtime_object! { "type" => EVENT, "reference" => reference.0.clone(), "authoritySha256" => authority },
            ],
            ..CommitBatch::default()
        };
        self.store.commit_guarded(&batch, |reader| -> Result<(), CellError> {
            // RECEIPT-STORE-AUTH-1: bind the complete acceptance at the same frontier.
            if reader.next_commit_seq()? != source { return Err(refused("frontier")); }
            let pending = reader.head_command()?.ok_or_else(|| refused("unknown"))?;
            if pending.command.command_id != dispatched.invocation.command_id
                || pending.command.generation != dispatched.generation().0
                || pending.command.command.get("payload") != Some(&invocation)
                || pending.result.as_ref() != Some(&receipt) { return Err(refused("dispatch")); }
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
        let row = self.store.conn.query_optional("SELECT body FROM _norn_function_receipt WHERE reference=?1 AND scope=?2 AND octet_length(body)<=?3", params![reference.0, self.scope, i64::try_from(LIMIT).map_err(|_invalid| refused("limit"))?], |row| row.get::<String>(0))?;
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
        let authority = crate::codec::hash_json(&stored)?;
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
                    && event.get("authoritySha256").and_then(RuntimeValue::as_str)
                        == Some(authority.as_str())
            })
        {
            return Err(refused("source"));
        }
        Ok(Some(stored))
    }

    pub(crate) fn receipt_for_acceptance(
        &self,
        reference: &Digest,
    ) -> Result<Option<StoredReceipt>, CellError> {
        let Some(stored) = self.stored_function_receipt(reference)? else {
            return Ok(None);
        };
        let pending = self
            .store
            .read()
            .head_command()?
            .ok_or_else(|| refused("replayed"))?;
        let payload = pending
            .command
            .command
            .get("payload")
            .ok_or_else(|| refused("dispatch"))?;
        if payload.get("type").and_then(RuntimeValue::as_str) != Some("norn.function/v1") {
            return Err(refused("dispatch"));
        }
        let invocation: norn_ir::function::FunctionInvocation = norn_ir::runtime::decode(
            payload
                .get("invocation")
                .ok_or_else(|| refused("dispatch"))?,
        )
        .map_err(|_invalid| refused("dispatch"))?;
        if invocation.tenant != self.scope
            || invocation.world != MAIN_WORLD
            || invocation
                .content_digest()
                .map_err(|_invalid| refused("dispatch"))?
                != stored.receipt.dispatch
            || pending.command.command_id != stored.receipt.command_id
            || pending.command.generation != stored.receipt.run.generation
        {
            return Err(refused("dispatch"));
        }
        Ok(Some(stored))
    }

    pub(crate) fn bind_function_receipt(
        &self,
        reference: &norn_kernel::receipt_binding::ReceiptReference,
        manifest: &Digest,
    ) -> Result<
        Option<norn_kernel::receipt_binding::ReceiptBinding<norn_ir::RuntimeObject>>,
        CellError,
    > {
        let Some(stored) = self.receipt_for_acceptance(reference.digest())? else {
            return Ok(None);
        };
        if stored.receipt.manifest != *manifest {
            return Err(refused("manifest"));
        }
        let consumed = self.store.conn.query_optional("SELECT consumed_commit_seq FROM _norn_function_receipt WHERE reference=?1 AND scope=?2", params![reference.digest().0, self.scope], |row| row.get::<Option<i64>>(0))?.flatten().is_some();
        if !crate::receipt_fence::usable(
            consumed,
            stored.frontier,
            self.store.read().next_commit_seq()?,
        ) {
            return Err(refused("replayed_or_stale"));
        }
        norn_kernel::receipt_binding::bind_receipt(&Bound(&stored), reference)
            .map(Some)
            .map_err(|_invalid| refused("binding"))
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
    type Cell = SqliteCell<norn_kernel::FactEvent, norn_ir::RuntimeObject, norn_ir::RuntimeObject>;
    fn fixture_model() -> Model {
        let definition = serde_json::from_value(json!({"name":"norn.FunctionCommand","properties":[{"id":"state","type":{"kind":"json-ext"}}]})).expect("type");
        let mut definitions = vec![definition];
        let mut target: norn_ir::ObjectTypeDef = serde_json::from_value(json!({"name":"test.ReceiptTarget", "properties":[
            {"id":"id","type":{"kind":"text"}}, {"id":"digest","type":{"kind":"text"},"default":""},
            {"id":"intent","type":{"kind":"text"},"default":""}, {"id":"confidence","type":{"kind":"text"},"default":""},
            {"id":"status","type":{"kind":"enum","values":[{"value":"proposed"},{"value":"published"}]},"default":"proposed"}
        ],"identity":[{"name":"primary","properties":["id"]}],"lifecycles":[{"property":"status","initial":["proposed"],"terminal":["published"],"transitions":[{"id":"publish","from":["proposed"],"to":"published","guards":["self.intent == \"order.modify\""]}]}]})).expect("target");
        definitions.push(target.clone());
        target.name = norn_ir::ObjectTypeName("norn.ActionCall".into());
        target.lifecycles.clear();
        target.identity.clear();
        target.properties.clear();
        let names = [
            "actionId",
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "actor",
            "turn",
            "transition",
            "from",
            "to",
            "rowOperations",
        ];
        target.properties = names
            .into_iter()
            .map(|id| {
                serde_json::from_value(
                    json!({"id":id,"type":{"kind":"json-ext"},"constraints":{"nullable":true}}),
                )
                .expect("property")
            })
            .collect();
        definitions.push(target);
        Model::from_definitions(&definitions, &[]).expect("model")
    }
    fn tombstone_dependency(store: &mut CellStore, scope: &str) {
        for write in [
            FactWriteRow::Put {
                object_type: "test.ReceiptTarget".into(),
                key: "ghost".into(),
                values: norn_ir::RuntimeObject::from([(
                    "id".into(),
                    RuntimeValue::String("ghost".into()),
                )]),
            },
            FactWriteRow::Delete {
                object_type: "test.ReceiptTarget".into(),
                key: "ghost".into(),
            },
        ] {
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: scope.to_owned(),
                    logical_time: 1_790_000_000_000,
                    writes: vec![write],
                    ..CommitBatch::default()
                })
                .expect("tombstone");
        }
    }
    fn setup(path: &std::path::Path) -> (Cell, DispatchedCommand, FunctionReceipt, Checks, Model) {
        setup_with_pin(path, false)
    }
    fn setup_with_pin(
        path: &std::path::Path,
        pin_absence: bool,
    ) -> (Cell, DispatchedCommand, FunctionReceipt, Checks, Model) {
        setup_with_destination(path, pin_absence, true)
    }
    fn setup_with_destination(
        path: &std::path::Path,
        pin_absence: bool,
        has_destination: bool,
    ) -> (Cell, DispatchedCommand, FunctionReceipt, Checks, Model) {
        let manifest: FunctionManifest = fixture("manifest");
        let checks =
            Checks(norn_checker::function::admit_function_manifest(&manifest).expect("manifest"));
        let mut invocation: FunctionInvocation = fixture("invocation");
        invocation.freshness = if pin_absence || !has_destination {
            norn_ir::function::FreshnessMode::RelevantState
        } else {
            norn_ir::function::FreshnessMode::DestinationCas
        };
        invocation.destination = has_destination.then(|| norn_ir::function::Destination {
            object_type: "test.ReceiptTarget".into(),
            key: "target-1".into(),
            expected_owner_version: 1,
        });
        invocation.dependency.facts.clear();
        invocation.dependency.predicates.clear();
        if pin_absence {
            invocation
                .dependency
                .facts
                .push(norn_ir::function::FactPin {
                    object_type: "test.ReceiptTarget".into(),
                    key: "ghost".into(),
                    version: 0,
                });
        }

        let dispatched = DispatchedCommand::pin(invocation, checks.0.execution_profile.clone())
            .expect("dispatch");
        let mut receipt: FunctionReceipt = fixture("receipt");
        receipt.dispatch = dispatched.dispatch.clone();
        receipt.input_identity = dispatched.input_identity.clone();
        receipt.dependency_token = dispatched.dependency_token.clone();
        let model = fixture_model();
        let mut store =
            CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
        let command = Command {
            id: CommandId(dispatched.invocation.command_id.clone()),
            generation: Generation(dispatched.generation().0),
            idempotency_key: IdempotencyKey("receipt-test".into()),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: norn_ir::runtime_object! { "type" => "norn.function/v1", "invocation" => norn_ir::runtime::encode(&dispatched.invocation).expect("invocation") }.as_object().expect("envelope").clone(),
        };
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: dispatched.invocation.tenant.clone(),
                logical_time: 1_790_000_000_000,
                writes: vec![
                    FactWriteRow::Put {
                        object_type: "norn.FunctionCommand".into(),
                        key: command.id.0.clone(),
                        values: norn_ir::RuntimeObject::from([(
                            "state".into(),
                            norn_ir::runtime::encode(&CommandState::dispatched(&dispatched))
                                .expect("state"),
                        )]),
                    },
                    FactWriteRow::Put {
                        object_type: "test.ReceiptTarget".into(),
                        key: "target-1".into(),
                        values: norn_ir::RuntimeObject::from([(
                            "id".into(),
                            RuntimeValue::String("target-1".into()),
                        )]),
                    },
                ],
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
        if pin_absence {
            tombstone_dependency(&mut store, &dispatched.invocation.tenant);
        }
        let result = receipt.clone();
        let mut cell = Cell::new(store, dispatched.invocation.tenant.clone(), move |_| {
            norn_ir::runtime::encode(&result)
                .expect("receipt")
                .as_object()
                .expect("object")
                .clone()
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
    fn deleted_dependency_refuses_a_never_written_freshness_pin() {
        let dir = tempfile::tempdir().expect("dir");
        let (mut cell, dispatched, receipt, checks, _) =
            setup_with_pin(&dir.path().join("cell.sqlite"), true);
        let before = cell.store().verified_head().expect("head");
        assert_eq!(
            cell.store()
                .read()
                .version(MAIN_WORLD, "test.ReceiptTarget", "ghost")
                .expect("version"),
            2
        );
        assert!(retain(&mut cell, &dispatched, &receipt, &checks, true).is_err());
        assert_eq!(cell.store().verified_head().expect("head"), before);
        assert!(
            cell.function_receipt_evidence(&receipt.content_digest().expect("digest"))
                .expect("evidence")
                .is_none()
        );
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
    #[test]
    fn intervening_commit_refuses_without_consuming_receipt() {
        use norn_kernel::{CellCommit, Refusable, receipt_binding::ReceiptReference};
        let dir = tempfile::tempdir().expect("dir");
        let (mut cell, dispatched, receipt, checks, _) = setup(&dir.path().join("cell.sqlite"));
        let digest = retain(&mut cell, &dispatched, &receipt, &checks, true).expect("retain");
        let reference =
            ReceiptReference::parse(&RuntimeValue::String(digest.0.clone())).expect("reference");
        assert!(
            cell.resolve_accepted_receipt(&reference, &receipt.manifest)
                .expect("usable")
                .is_some()
        );
        let before = cell.store().verified_head().expect("head");
        cell.commit(CellCommit::default())
            .expect("unrelated commit");
        let after = cell.store().verified_head().expect("head");
        assert_ne!(after, before);
        let refusal = cell
            .resolve_accepted_receipt(&reference, &receipt.manifest)
            .expect_err("stale")
            .refusal();
        assert_eq!(refusal.code, "action.receipt_refused");
        assert_eq!(refusal.category, norn_kernel::RefusalCategory::Precondition);
        assert!(refusal.message.contains("replayed_or_stale"));
        assert!(
            cell.commit_accepted_receipt(CellCommit::default(), &reference)
                .is_err()
        );
        assert_eq!(cell.store().verified_head().expect("head"), after);
        assert!(
            cell.function_receipt_evidence(&digest)
                .expect("retained")
                .is_some()
        );
        let consumed = cell
            .store
            .conn
            .query_optional(
                "SELECT consumed_commit_seq FROM _norn_function_receipt WHERE reference=?1",
                params![digest.0],
                |row| row.get::<Option<i64>>(0),
            )
            .expect("consumption")
            .flatten();
        assert_eq!(consumed, None);
    }
    #[test]
    fn consumption_is_atomic_single_use_and_refusal_preserves_evidence() {
        use norn_kernel::receipt_binding::ReceiptReference;
        use norn_kernel::{CellCommit, FactMutation, FactWrite};
        let dir = tempfile::tempdir().expect("dir");
        let (mut cell, dispatched, receipt, checks, _) = setup(&dir.path().join("cell.sqlite"));
        let digest = retain(&mut cell, &dispatched, &receipt, &checks, true).expect("retain");
        let reference =
            ReceiptReference::parse(&RuntimeValue::String(digest.0)).expect("reference");
        assert!(
            cell.resolve_accepted_receipt(&reference, &receipt.manifest)
                .expect("resolve")
                .is_some()
        );
        assert!(
            cell.resolve_accepted_receipt(
                &reference,
                &Digest(format!("sha256:{}", "0".repeat(64)))
            )
            .is_err()
        );
        let before = cell.store().verified_head().expect("head");
        let invalid = CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("unknown.Type", "bad"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: norn_ir::RuntimeObject::new(),
                },
            }],
            ..CellCommit::default()
        };
        assert!(cell.commit_accepted_receipt(invalid, &reference).is_err());
        assert_eq!(cell.store().verified_head().expect("head"), before);
        assert!(
            cell.resolve_accepted_receipt(&reference, &receipt.manifest)
                .expect("still usable")
                .is_some()
        );
        cell.commit_accepted_receipt(CellCommit::default(), &reference)
            .expect("consume");
        assert!(
            cell.resolve_accepted_receipt(&reference, &receipt.manifest)
                .is_err()
        );
        let after = cell.store().verified_head().expect("head");
        assert!(
            cell.commit_accepted_receipt(CellCommit::default(), &reference)
                .is_err()
        );
        assert_eq!(cell.store().verified_head().expect("head"), after);
        assert!(
            cell.store()
                .read()
                .head_command()
                .expect("outbox")
                .is_none()
        );
    }
    struct ActionAllow;
    impl norn_kernel::ActionAuthorizer for ActionAllow {
        fn authorize(
            &self,
            _: &norn_kernel::ActionAuthzRequest<'_>,
        ) -> Result<(), norn_kernel::ActionDenial> {
            Ok(())
        }
    }
    fn exercise_receipt_action(member: &str, accepts: bool) {
        exercise_receipt_action_with_destination(member, accepts, true);
    }
    fn exercise_receipt_action_with_destination(member: &str, accepts: bool, has_destination: bool) {
        use norn_kernel::{
            ActionCall, ActionOutcome, DecisionTurn, TypeCatalog, World, commit_action,
        };
        let dir = tempfile::tempdir().expect("dir");
        let (mut cell, dispatched, receipt, checks, model) =
            setup_with_destination(&dir.path().join("cell.sqlite"), false, has_destination);
        let digest = retain(&mut cell, &dispatched, &receipt, &checks, true).expect("retain");
        let action: norn_ir::ActionRoot = serde_json::from_value(json!({
            "actionId":"test.accept", "title":"Accept", "scopeRequired":["orgId"], "idempotent":false,"ledgered":true,"ledgerCategory":"test",
            "inputSchema":{"type":"object","properties":{"id":{"type":"string"},"receipt":{"type":"string"}},"required":["id","receipt"],"additionalProperties":false},
            "outputSchema":{"type":"object"},"preconditions":[], "writes":[{"entityKind":"test.ReceiptTarget","change":"modified"}],
            "effectProgram":{"kind":"fire_transition","objectIdField":"id","objectKind":"test.ReceiptTarget","entityArtifactId":"test.target","entityArtifactVersion":"0.1.0","lifecycle":"status","transition":"publish", "acceptReceipt":{"referenceField":"receipt","manifestDigest":receipt.manifest.0,"digestProperty":"digest","resultBindings":{"intent":"intent","confidence":member}}}
        })).expect("action");
        let catalog = TypeCatalog::new(model.object_types());
        let mut request = ActionCall {
            action: &action,
            action_artifact_id: "test.accept",
            action_version: "0.1.0",
            scope: std::collections::BTreeMap::from([(
                "orgId".into(),
                dispatched.invocation.tenant,
            )]),
            actor: "reviewer:test".into(),
            input: norn_ir::RuntimeObject::from([
                ("id".into(), RuntimeValue::String("target-1".into())),
                ("receipt".into(), RuntimeValue::String(digest.0.clone())),
            ]),
            idempotency_key: "accept-1".into(),
            object_type: None,
            turn: DecisionTurn {
                id: "publish-turn".into(),
                now: LogicalTime::from_millis(1_790_000_000_000),
                world: World::Main,
                consumes: None,
            },
        };
        let before = cell.store().verified_head().expect("head");
        request.input.insert(
            "receipt".into(),
            norn_ir::runtime::encode(&receipt).expect("body"),
        );
        assert!(commit_action(&mut cell, &catalog, &request, &ActionAllow).is_err());
        assert_eq!(cell.store().verified_head().expect("head"), before);
        request
            .input
            .insert("receipt".into(), RuntimeValue::String(digest.0));
        if !accepts {
            let error =
                commit_action(&mut cell, &catalog, &request, &ActionAllow).expect_err("refusal");
            if !has_destination {
                assert!(matches!(error, norn_kernel::ActionRefusal::InvalidInput { ref field } if field == "receipt"));
            } else if member == "confidence" {
                assert!(
                    matches!(error, norn_kernel::ActionRefusal::Decision(norn_kernel::DecisionRefusal::InvalidOutput { ref reason }) if reason.contains("receipt result property confidence"))
                );
            }
            assert_eq!(cell.store().verified_head().expect("head"), before);
            assert!(
                cell.resolve_accepted_receipt(
                    &norn_kernel::receipt_binding::ReceiptReference::parse(
                        request.input.get("receipt").expect("reference")
                    )
                    .expect("reference"),
                    &receipt.manifest
                )
                .expect("unconsumed")
                .is_some()
            );
            return;
        }
        assert!(matches!(
            commit_action(&mut cell, &catalog, &request, &ActionAllow).expect("publish"),
            ActionOutcome::Committed { .. }
        ));
        assert_eq!(
            cell.load(&FactKey::new("test.ReceiptTarget", "target-1"))
                .expect("target")
                .values
                .expect("values")
                .get("status")
                .and_then(RuntimeValue::as_str),
            Some("published")
        );
        assert_receipt_retries(&mut cell, &catalog, &mut request);
    }
    fn assert_receipt_retries(
        cell: &mut Cell,
        catalog: &norn_kernel::TypeCatalog<'_>,
        request: &mut norn_kernel::ActionCall<'_>,
    ) {
        use norn_kernel::{ActionOutcome, commit_action};
        let published = cell.store().verified_head().expect("head");
        for world in [
            norn_kernel::World::Fork {
                id: norn_kernel::WorldId("scenario".into()),
            },
            norn_kernel::World::Historical {
                commit: norn_kernel::CommitId("history".into()),
            },
        ] {
            request.turn.world = world;
            assert!(norn_kernel::commit_action(cell, catalog, request, &ActionAllow).is_err());
            assert_eq!(cell.store().verified_head().expect("head"), published);
        }
        request.turn.world = norn_kernel::World::Main;
        assert!(matches!(
            commit_action(cell, catalog, request, &ActionAllow).expect("retry"),
            ActionOutcome::Replayed { .. }
        ));
        assert_eq!(cell.store().verified_head().expect("head"), published);
        request.idempotency_key = "accept-2".into();
        assert!(commit_action(cell, catalog, request, &ActionAllow).is_err());
        assert_eq!(cell.store().verified_head().expect("head"), published);
    }
    #[test]
    fn destinationless_authenticated_receipt_refuses_without_consumption() {
        exercise_receipt_action_with_destination("intent", false, false);
    }
    #[test]
    fn receipt_action_binds_before_guard_and_exact_retry_reproduces_answer() {
        exercise_receipt_action("intent", true);
    }
    #[test]
    fn wrong_typed_result_refuses_without_consumption() {
        exercise_receipt_action("confidence", false);
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]
        #[test]
        fn wrong_type_or_missing_members_never_publish(member in prop_oneof![Just("confidence".to_owned()), "missing_[a-z]{1,16}"]) {
            exercise_receipt_action(&member, false);
        }
        #[test]
        fn receipt_preconditions_preserve_safe_detail_and_storage_errors_stay_redacted(
            reason in prop_oneof![Just("unknown"), Just("replayed_or_stale"), Just("manifest"), Just("binding")],
            storage_detail in "[a-zA-Z0-9_/]{1,128}"
        ) {
            use norn_kernel::Refusable;
            let expected = crate::host::receipt_commit_error(refused(reason)).refusal();
            prop_assert_eq!(expected.code, "action.receipt_refused");
            prop_assert_eq!(expected.category, norn_kernel::RefusalCategory::Precondition);
            prop_assert!(expected.message.contains(reason));
            let storage = crate::host::receipt_commit_error(CellError::Engine(storage_detail)).refusal();
            prop_assert_eq!(storage.category, norn_kernel::RefusalCategory::Internal);
            prop_assert_eq!(storage.message, norn_kernel::INTERNAL_MESSAGE);
        }
        #[test]
        fn consumed_or_different_frontier_never_admits(consumed in any::<bool>(), authenticated in any::<u64>(), current in any::<u64>()) {
            let admitted = crate::receipt_fence::usable(consumed, authenticated, current);
            prop_assert_eq!(admitted, !consumed && authenticated == current);
            prop_assert!(!crate::receipt_fence::usable(true, authenticated, authenticated));
        }
        #[test]
        fn stored_acceptance_or_frontier_tampering_refuses_without_writes(which in 0u8..3, suffix in "[a-z]{1,8}") {
            let dir = tempfile::tempdir().expect("dir");
            let (mut cell, dispatched, receipt, checks, _) = setup(&dir.path().join("cell.sqlite"));
            let reference = retain(&mut cell, &dispatched, &receipt, &checks, true).expect("retain");
            let mut stored = cell.stored_function_receipt(&reference).expect("read").expect("stored");
            let before = cell.store().verified_head().expect("head");
            match which {
                0 => { prop_assert!(!stored.acceptance.reads.is_empty()); stored.acceptance.reads.clear(); },
                1 => { stored.acceptance.record.command.0.push_str(&suffix); },
                _ => { stored.frontier = stored.frontier.wrapping_add(1); },
            }
            let body = serde_json::to_string(&stored).expect("body");
            cell.store.conn.execute("UPDATE _norn_function_receipt SET body=?1 WHERE reference=?2", params![body, reference.0]).expect("tamper");
            prop_assert!(cell.function_receipt_evidence(&reference).is_err());
            let reference = norn_kernel::receipt_binding::ReceiptReference::parse(&RuntimeValue::String(reference.0)).expect("reference");
            prop_assert!(cell.resolve_accepted_receipt(&reference, &receipt.manifest).is_err());
            prop_assert_eq!(cell.store().verified_head().expect("head"), before);
        }
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

/// Claim only inside the accepting action transaction; rollback covers effects.
pub(crate) fn claim<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    scope: &str,
    reference: &Digest,
    stored: &StoredReceipt,
) -> Result<(), CellError> {
    let current = reader.next_commit_seq()?;
    let (retained, consumed) = reader.conn.query_optional("SELECT body, consumed_commit_seq FROM _norn_function_receipt WHERE reference=?1 AND scope=?2", params![reference.0, scope], |row| Ok((row.get::<String>(0)?, row.get::<Option<i64>>(1)?)))?.ok_or_else(|| refused("unknown"))?;
    let body = serde_json::to_string(stored).map_err(CellError::Json)?;
    if retained != body
        || !crate::receipt_fence::usable(consumed.is_some(), stored.frontier, current)
    {
        return Err(refused("replayed_or_stale"));
    }
    reader.conn.execute("UPDATE _norn_function_receipt SET consumed_commit_seq=?1 WHERE reference=?2 AND scope=?3 AND body=?4 AND consumed_commit_seq IS NULL", params![i64::try_from(current).map_err(|_invalid| refused("frontier"))?, reference.0, scope, body])?;
    Ok(())
}
