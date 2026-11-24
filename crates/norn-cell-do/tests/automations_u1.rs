//! AUTOMATION-1 (Unit 1): author-grant authority (`AUTH-ATTENUATE-1`),
//! transactional trigger outbox (`AUTOMATION-OUTBOX-1`), and reserved
//! automation idempotency key namespace (`AUTOMATION-KEY-1`) end to end
//! through the hosted fetch dispatch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "automation U1 integration tests"
)]

#[path = "support/step_cell.rs"]
mod step_cell;

mod cases {
    use std::path::Path;

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};
    use norn_api::automation_definitions::{
        automation_approved_key, automation_run_key, automation_triggers_for_commit,
        pending_automation_continuation_count, pending_automation_trigger_count,
        pending_automation_triggers,
    };
    use norn_api::provision::PROVISION_PATH;
    use norn_api::scope::CellScope;
    use norn_cell::StoreOptions;
    use norn_cell::engine::{NativeEngine, SqlEngine};
    use norn_cell::retained::RetainedLifecycle;
    use norn_cell_do::base;
    use norn_cell_do::policy::DEMO_GRANTS;
    use norn_cell_do::principal::TokenPrincipals;
    use norn_cell_do::server::{Request, Response};
    use norn_cell_do::tenant::Tenant;
    use norn_ir::RuntimeValue;
    use serde_json::{Value, json};

    use super::step_cell::{
        APPROVER, Cell, FLOW_ID, MELT, MELT_ACTION, OPERATOR, SCOPE, call, call_with, cell,
        cell_on, melt_body,
    };

    const ISSUER: &str = "https://studio-preview.example";
    const NOW: u64 = 1_800_000_000;
    const POLICY: &str = "constitution.melt.access.policy";
    const HANDLE_KEY: [u8; 32] = [7; 32];
    const TENANT_QUERY: &str = "orgId=org_a&environmentId=prod";

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    fn jwks() -> String {
        json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "alg": "EdDSA",
                "kid": "k1",
                "x": URL_SAFE_NO_PAD.encode(signing_key().verifying_key().as_bytes())
            }]
        })
        .to_string()
    }

    fn user_token(role: &str) -> String {
        let claims = json!({
            "iss": ISSUER,
            "aud": "norn-host",
            "sub": format!("user-{role}-of-org_a"),
            "iat": NOW.saturating_sub(10),
            "exp": NOW.saturating_add(290),
            "norn": { "kind": "user", "org": "org_a", "role": role, "amr": "password" }
        });
        let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
        let body = URL_SAFE_NO_PAD.encode(claims.to_string());
        let sig = signing_key().sign(format!("{head}.{body}").as_bytes());
        format!(
            "Bearer {head}.{body}.{}",
            URL_SAFE_NO_PAD.encode(sig.to_bytes())
        )
    }

    fn tenant_scope() -> CellScope {
        CellScope {
            org_id: "org_a".into(),
            environment_id: "prod".into(),
        }
    }

    fn melt_artifacts_with_policy(roles: &[&str]) -> Vec<Value> {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../norn-cell/tests/fixtures/constitution.json"
        ))
        .unwrap();
        let access = json!({ "artifactId": POLICY, "version": "1.0.0" });
        let mut artifacts: Vec<Value> = fixture["withLoss"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(|mut artifact| {
                artifact["scope"] = json!({ "orgId": "org_a", "environmentId": "prod" });
                if artifact["kind"] == "action" {
                    artifact["payload"]
                        .as_object_mut()
                        .unwrap()
                        .insert("access".into(), access.clone());
                }
                artifact
            })
            .collect();
        artifacts.push(json!({
            "artifactId": POLICY,
            "dependencies": [],
            "irVersion": "0.1",
            "kind": "policy",
            "lifecycle": "active",
            "name": "Melt access",
            "namespace": "constitution",
            "payload": {
                "accessVersion": "0.1",
                "policyId": "constitution.melt.access",
                "rules": [{
                    "ruleId": "callers",
                    "actions": [MELT_ACTION],
                    "resourceType": MELT,
                    "scope": { "dimensions": ["org", "environment"] },
                    "allOf": [{ "role": { "anyOf": roles } }]
                }]
            },
            "provenance": { "source": "authoring_sdk", "sourcePath": "tests/automations_u1.rs" },
            "scope": { "orgId": "org_a", "environmentId": "prod" },
            "version": "1.0.0"
        }));
        artifacts
    }

    struct TenantCell {
        tenant: Tenant<NativeEngine>,
        tokens: TokenPrincipals,
    }

    impl TenantCell {
        fn call(&mut self, method: &str, path: &str, auth: Option<&str>, body: &str) -> Response {
            self.tenant.handle(
                &self.tokens,
                &Request {
                    method,
                    path,
                    query: TENANT_QUERY,
                    authorization: auth,
                    now: NOW,
                    body,
                },
            )
        }
    }

    fn provisioned_tenant(path: &Path, roles: &[&str]) -> TenantCell {
        provisioned_tenant_with_artifacts(path, &melt_artifacts_with_policy(roles))
    }

    fn provisioned_tenant_with_artifacts(path: &Path, artifacts: &[Value]) -> TenantCell {
        let file = path.to_owned();
        let tenant = Tenant::open(
            tenant_scope(),
            &DEMO_GRANTS,
            Box::new(move || {
                NativeEngine::open(&file, StoreOptions::default())
                    .map_err(|error| error.to_string())
            }),
            Some(&HANDLE_KEY),
        )
        .unwrap();
        let tokens =
            TokenPrincipals::new(ISSUER, &jwks(), tenant_scope(), tenant.caller(), NOW).unwrap();
        let mut cell = TenantCell { tenant, tokens };
        let body = json!({
            "baseSha256": base::trusted_base().unwrap().sha256(),
            "artifacts": artifacts,
        })
        .to_string();
        let response = cell.call("POST", PROVISION_PATH, Some(&user_token("owner")), &body);
        assert_eq!(response.status, 200, "{}", response.body);
        cell
    }

    fn automation_payload(title: &str, when: &Value) -> Value {
        json!({
            "title": title,
            "enabled": true,
            "objectType": MELT,
            "when": when,
            "then": {
                "actionId": MELT_ACTION,
                "mode": "auto",
                "input": { "input_fine_g": "12", "output_fine_g": "10" }
            }
        })
    }

    fn install_automation<E: SqlEngine>(cell: &Cell<E>, id: &str, payload: &Value) {
        let mut body = json!({ "kind": "automation", "id": id, "payload": payload });
        let (status, admitted) = call(
            cell,
            "POST",
            "/v1/definitions:admit",
            OPERATOR,
            &body.to_string(),
        );
        assert_eq!(status, 200, "{admitted}");
        assert_eq!(admitted["admitted"], json!(true), "{admitted}");
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        let (status, installed) = call(
            cell,
            "POST",
            "/v1/definitions:install",
            OPERATOR,
            &body.to_string(),
        );
        assert_eq!(status, 200, "{installed}");
        assert_eq!(installed["installed"], json!(true), "{installed}");
    }

    fn install_approval<E: SqlEngine>(cell: &Cell<E>) {
        let mut body = json!({
            "kind": "approval",
            "id": "ops.melt_approval",
            "payload": {
                "action": MELT_ACTION,
                "decider": { "roles": ["admin"] },
                "question": "Approve melt conversion?",
                "risk": "high"
            }
        });
        let (status, admitted) = call(
            cell,
            "POST",
            "/v1/definitions:admit",
            OPERATOR,
            &body.to_string(),
        );
        assert_eq!(status, 200, "{admitted}");
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        let (status, installed) = call(
            cell,
            "POST",
            "/v1/definitions:install",
            OPERATOR,
            &body.to_string(),
        );
        assert_eq!(status, 200, "{installed}");
    }

    #[test]
    fn only_automation_installs_record_authority_in_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install-audit.sqlite");
        let automation_id = "ops.audit_melt";
        {
            let engine = NativeEngine::open(&path, StoreOptions::default()).unwrap();
            let cell = cell_on(engine, true);
            install_automation(
                &cell,
                automation_id,
                &automation_payload(
                    "Audit melt",
                    &json!({ "kind": "event", "event": "created" }),
                ),
            );
        }

        let engine = NativeEngine::open(&path, StoreOptions::default()).unwrap();
        let install_event = |id: &str| -> Value {
            let event: String = engine
                .query_row(
                    "SELECT event FROM _norn_ledger
                     WHERE json_extract(event, '$.type') = 'definitions.installed'
                       AND json_extract(event, '$.diff.retained.id') = ?1",
                    &[id.into()],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&event).unwrap()
        };
        let flow = install_event(FLOW_ID);
        assert!(
            !flow["diff"]["retained"]
                .as_object()
                .unwrap()
                .contains_key("authority")
        );
        let automation = install_event(automation_id);
        let authority = &automation["diff"]["retained"]["authority"];
        assert!(authority.is_object(), "{automation}");
        assert_eq!(authority["actionId"], MELT_ACTION);
    }

    /// 1. Trigger recorded in the same commit as the write, and replay dedupes.
    #[test]
    fn trigger_recorded_in_same_commit_and_replay_dedupes() {
        let cell = cell();
        install_automation(
            &cell,
            "ops.on_melt_created",
            &automation_payload(
                "On melt created",
                &json!({ "kind": "event", "event": "created" }),
            ),
        );
        install_automation(
            &cell,
            "ops.on_melt_action",
            &automation_payload(
                "On melt convert action",
                &json!({ "kind": "event", "event": "action", "actionId": MELT_ACTION }),
            ),
        );
        install_automation(
            &cell,
            "ops.on_melt_updated",
            &automation_payload(
                "On melt updated",
                &json!({ "kind": "event", "event": "updated" }),
            ),
        );

        let body = melt_body("melt-1", "12", "10", "k-direct-1");
        let (status, committed) = call(
            &cell,
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            OPERATOR,
            &body,
        );
        assert_eq!(status, 200, "{committed}");
        assert_eq!(committed["outcome"], json!("committed"), "{committed}");
        let commit_seq = committed["receipt"]["commitSeq"].as_u64().unwrap();

        let recorded = automation_triggers_for_commit(&cell.host, commit_seq).unwrap();
        assert_eq!(recorded.len(), 2, "{recorded:?}");
        assert_eq!(recorded[0].key.automation_id, "ops.on_melt_action");
        assert_eq!(recorded[1].key.automation_id, "ops.on_melt_created");
        for candidate in &recorded {
            assert_eq!(candidate.key.revision, 1);
            assert_eq!(candidate.key.trigger_commit_seq, commit_seq);
            assert_eq!(candidate.key.object_type, MELT);
            assert_eq!(candidate.key.object_id, "melt-1");
            assert_eq!(candidate.key.object_version, 1);
            assert_eq!(candidate.action_id.as_deref(), Some(MELT_ACTION));
            assert_eq!(candidate.before_values, None);
            let after = candidate.after_values.as_ref().unwrap();
            assert_eq!(
                after.get("loss_fine_g"),
                Some(&RuntimeValue::from_json_text("2").unwrap())
            );
        }

        // Exact retry replays the original receipt and adds no duplicate candidate.
        let (status, replayed) = call(
            &cell,
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            OPERATOR,
            &body,
        );
        assert_eq!(status, 200, "{replayed}");
        assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
        assert_eq!(
            pending_automation_trigger_count(&cell.host).unwrap(),
            2,
            "replay must not record duplicate candidates"
        );
    }

    /// 2. Crash or reopen after commit loses no candidate, and a refused/aborted
    ///    write records none.
    #[test]
    fn committed_candidate_survives_reopen_and_aborted_write_records_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automations-u1.sqlite");

        let commit_seq = {
            let engine = NativeEngine::open(&path, StoreOptions::default()).unwrap();
            let cell = cell_on(engine, true);
            install_automation(
                &cell,
                "ops.durable_watch",
                &automation_payload(
                    "Durable watch",
                    &json!({ "kind": "event", "event": "created" }),
                ),
            );

            // A call that violates the action precondition aborts before commit and records 0 candidates.
            let (bad_status, bad_body) = call(
                &cell,
                "POST",
                &format!("/v1/actions/{MELT_ACTION}"),
                OPERATOR,
                &melt_body("melt-bad", "10", "12", "k-bad-1"),
            );
            assert_ne!(bad_status, 200, "{bad_body}");
            assert_eq!(pending_automation_trigger_count(&cell.host).unwrap(), 0);

            // A committed write records its candidate inside the same SQLite transaction.
            let (ok_status, committed) = call(
                &cell,
                "POST",
                &format!("/v1/actions/{MELT_ACTION}"),
                OPERATOR,
                &melt_body("melt-ok", "20", "17", "k-ok-1"),
            );
            assert_eq!(ok_status, 200, "{committed}");
            committed["receipt"]["commitSeq"].as_u64().unwrap()
        };

        // Reopen the SQLite database from disk as a fresh process after a crash.
        let reopened_engine = NativeEngine::open(&path, StoreOptions::default()).unwrap();
        let reopened = cell_on(reopened_engine, false);
        let pending = pending_automation_triggers(&reopened.host, 16).unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].key.automation_id, "ops.durable_watch");
        assert_eq!(pending[0].key.trigger_commit_seq, commit_seq);
        assert_eq!(pending[0].key.object_id, "melt-ok");
        assert_eq!(pending[0].key.object_version, 1);
        assert_eq!(pending[0].action_id.as_deref(), Some(MELT_ACTION));
    }

    /// 3. Bound flow call records trigger candidates in the committing write.
    #[test]
    fn bound_flow_action_step_records_trigger_candidates() {
        let cell = cell();
        install_automation(
            &cell,
            "ops.flow_watch_created",
            &automation_payload(
                "Watch flow created",
                &json!({ "kind": "event", "event": "created" }),
            ),
        );
        install_automation(
            &cell,
            "ops.flow_watch_action",
            &automation_payload(
                "Watch flow action",
                &json!({ "kind": "event", "event": "action", "actionId": MELT_ACTION }),
            ),
        );

        let (status, opened) = call(
            &cell,
            "POST",
            "/v1/runs",
            OPERATOR,
            &json!({ "flow": FLOW_ID, "runId": "flow-auto-1" }).to_string(),
        );
        assert_eq!(status, 201, "{opened}");
        assert_eq!(pending_automation_trigger_count(&cell.host).unwrap(), 0);

        let pending_query = format!("{SCOPE}&state=pending");
        let (_, page) = call_with(&cell, "GET", "/v1/decisions", &pending_query, APPROVER, "");
        let decision_id = page["items"][0]["id"].as_str().unwrap().to_owned();
        let (status, decided) = call(
            &cell,
            "POST",
            &format!("/v1/decisions/{decision_id}:decide"),
            APPROVER,
            &json!({ "choice": "approve", "idempotencyKey": "k-flow-approve" }).to_string(),
        );
        assert_eq!(status, 200, "{decided}");
        assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");

        let candidates = pending_automation_triggers(&cell.host, 16).unwrap();
        assert_eq!(candidates.len(), 2, "{candidates:?}");
        assert_eq!(candidates[0].key.automation_id, "ops.flow_watch_action");
        assert_eq!(candidates[1].key.automation_id, "ops.flow_watch_created");
        assert_eq!(
            candidates[0].key.trigger_commit_seq,
            candidates[1].key.trigger_commit_seq
        );
        for candidate in &candidates {
            assert_eq!(candidate.key.object_type, MELT);
            assert_eq!(candidate.key.object_id, "melt-7");
            assert_eq!(candidate.key.object_version, 1);
            assert_eq!(candidate.action_id.as_deref(), Some(MELT_ACTION));
        }
    }

    /// 4. Approved proposal parks with 0 candidates and records its candidate
    ///    atomically when the approved call commits.
    #[test]
    fn approved_proposal_records_trigger_candidate_only_on_commit() {
        let cell = cell();
        install_approval(&cell);
        install_automation(
            &cell,
            "ops.approval_watch",
            &automation_payload(
                "Watch approved",
                &json!({ "kind": "event", "event": "created" }),
            ),
        );

        let body = melt_body("melt-7", "12", "10", "k-approved-1");
        let (status, parked) = call(
            &cell,
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            OPERATOR,
            &body,
        );
        assert_eq!(status, 202, "{parked}");
        assert_eq!(pending_automation_trigger_count(&cell.host).unwrap(), 0);

        let decision_id = parked["decision"]["id"].as_str().unwrap().to_owned();
        let (status, decided) = call(
            &cell,
            "POST",
            &format!("/v1/decisions/{decision_id}:decide"),
            APPROVER,
            &json!({ "choice": "approve", "idempotencyKey": "k-decide-1" }).to_string(),
        );
        assert_eq!(status, 200, "{decided}");
        assert_eq!(decided["call"]["state"], json!("not_executed"), "{decided}");
        assert_eq!(
            pending_automation_trigger_count(&cell.host).unwrap(),
            0,
            "approving before commit must not record a candidate"
        );

        let (status, committed) = call(
            &cell,
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            OPERATOR,
            &body,
        );
        assert_eq!(status, 200, "{committed}");
        assert_eq!(committed["outcome"], json!("committed"), "{committed}");
        let commit_seq = committed["receipt"]["commitSeq"].as_u64().unwrap();

        let candidates = automation_triggers_for_commit(&cell.host, commit_seq).unwrap();
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].key.automation_id, "ops.approval_watch");
        assert_eq!(candidates[0].key.object_id, "melt-7");
        assert_eq!(candidates[0].key.object_version, 1);
        assert_eq!(candidates[0].action_id.as_deref(), Some(MELT_ACTION));
    }

    /// 5. Author without the grant for `then.actionId` is refused at admission
    ///    and install; an author with the grant installs a revocable, server-bound
    ///    authority record.
    #[test]
    fn author_without_grant_is_refused_at_admission_and_install() {
        let dir = tempfile::tempdir().unwrap();
        let mut cell = provisioned_tenant(&dir.path().join("authz.sqlite"), &["owner"]);
        let payload = automation_payload(
            "Owner-only automation",
            &json!({ "kind": "event", "event": "created" }),
        );
        let body = json!({
            "kind": "automation",
            "id": "ops.owner_auto",
            "payload": payload,
        });

        // `admin` has WriteDefinitions, but lacks the Cedar grant for MELT_ACTION -> 403.
        let admin_admit = cell.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("admin")),
            &body.to_string(),
        );
        assert_eq!(admin_admit.status, 403, "{}", admin_admit.body);

        // `owner` holds both WriteDefinitions and the MELT_ACTION grant -> 200 admitted.
        let owner_admit = cell.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("owner")),
            &body.to_string(),
        );
        assert_eq!(owner_admit.status, 200, "{}", owner_admit.body);
        let admitted: Value = serde_json::from_str(&owner_admit.body).unwrap();
        assert_eq!(admitted["admitted"], json!(true), "{admitted}");
        let digest = admitted["restatementSha256"].clone();

        // `admin` cannot install even with a valid restatement sign-off -> 403.
        let mut signed_body = body.clone();
        signed_body["signoff"] = json!({ "restatementSha256": digest });
        let admin_install = cell.call(
            "POST",
            "/v1/definitions:install",
            Some(&user_token("admin")),
            &signed_body.to_string(),
        );
        assert_eq!(admin_install.status, 403, "{}", admin_install.body);

        // Unsigned install refuses with definition.signoff_required.
        let unsigned = cell.call(
            "POST",
            "/v1/definitions:install",
            Some(&user_token("owner")),
            &body.to_string(),
        );
        assert_eq!(unsigned.status, 422, "{}", unsigned.body);

        // Signed install by `owner` succeeds and binds `RetainedAuthority` to `owner`.
        let owner_install = cell.call(
            "POST",
            "/v1/definitions:install",
            Some(&user_token("owner")),
            &signed_body.to_string(),
        );
        assert_eq!(owner_install.status, 200, "{}", owner_install.body);

        let host = cell.tenant.serving_host().unwrap();
        let stored = host.retained_definition("ops.owner_auto").unwrap().unwrap();
        let authority = stored.definition.authority.expect("server-bound authority");
        assert_eq!(authority.author.subject, "user:user-owner-of-org_a");
        assert_eq!(authority.action_id, MELT_ACTION);
        assert_eq!(authority.object_type, MELT);
        assert!(authority.revocable);
        assert_ne!(authority.delegation_id, "");
        assert_ne!(authority.action_digest, "");
        assert_eq!(
            authority.parent_grant_ids.len(),
            1,
            "{:?}",
            authority.parent_grant_ids
        );
        assert!(
            authority.parent_grant_ids[0].contains(POLICY)
                && authority.parent_grant_ids[0].contains("callers"),
            "{:?}",
            authority.parent_grant_ids
        );
    }

    /// 6a. An author lacking the action grant cannot borrow a future trigger
    /// actor's grant to admit an automation.
    #[test]
    fn author_cannot_borrow_future_trigger_actors_grant() {
        let dir = tempfile::tempdir().unwrap();
        let mut member_only =
            provisioned_tenant(&dir.path().join("member-only.sqlite"), &["member"]);
        let def = json!({
            "kind": "automation",
            "id": "ops.no_borrowing",
            "payload": automation_payload(
                "No borrowing",
                &json!({ "kind": "event", "event": "created" }),
            ),
        });
        for role in ["owner", "admin"] {
            let refused = member_only.call(
                "POST",
                "/v1/definitions:admit",
                Some(&user_token(role)),
                &def.to_string(),
            );
            assert_eq!(refused.status, 403, "{role}: {}", refused.body);
        }
    }

    /// 6b. Trigger actor's grant is never bound onto the automation; bound
    /// authority stays on the installing author, and retiring revokes it.
    #[test]
    fn trigger_actors_grant_is_never_used_and_retire_revokes_authority() {
        let dir = tempfile::tempdir().unwrap();
        let mut shared =
            provisioned_tenant(&dir.path().join("shared.sqlite"), &["owner", "member"]);
        let def = json!({
            "kind": "automation",
            "id": "ops.no_borrowing",
            "payload": automation_payload(
                "No borrowing",
                &json!({ "kind": "event", "event": "created" }),
            ),
        });
        let admit = shared.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("owner")),
            &def.to_string(),
        );
        assert_eq!(admit.status, 200, "{}", admit.body);
        let digest =
            serde_json::from_str::<Value>(&admit.body).unwrap()["restatementSha256"].clone();
        let mut install_body = def;
        install_body["signoff"] = json!({ "restatementSha256": digest });
        let installed = shared.call(
            "POST",
            "/v1/definitions:install",
            Some(&user_token("owner")),
            &install_body.to_string(),
        );
        assert_eq!(installed.status, 200, "{}", installed.body);

        let trigger_write = shared.call(
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            Some(&user_token("member")),
            &melt_body("melt-by-member", "15", "12", "k-member-1"),
        );
        assert_eq!(trigger_write.status, 200, "{}", trigger_write.body);
        let trigger_commit_seq =
            serde_json::from_str::<Value>(&trigger_write.body).unwrap()["receipt"]["commitSeq"]
                .as_u64()
                .unwrap();

        let host = shared.tenant.serving_host().unwrap();
        let stored = host
            .retained_definition("ops.no_borrowing")
            .unwrap()
            .unwrap();
        assert_eq!(
            stored
                .definition
                .authority
                .as_ref()
                .map(|a| a.author.subject.as_str()),
            Some("user:user-owner-of-org_a"),
            "authority must stay bound to installing author, never trigger actor"
        );
        assert_eq!(pending_automation_trigger_count(host).unwrap(), 1);

        let retired = shared.call(
            "POST",
            "/v1/definitions/ops.no_borrowing:retire",
            Some(&user_token("owner")),
            &json!({ "expectedVersion": 1 }).to_string(),
        );
        assert_eq!(retired.status, 200, "{}", retired.body);
        let host = shared.tenant.serving_host().unwrap();
        let after_retire = host
            .retained_definition("ops.no_borrowing")
            .unwrap()
            .unwrap();
        assert_eq!(
            after_retire.definition.lifecycle,
            RetainedLifecycle::Retired
        );
        assert_eq!(after_retire.definition.authority, None);
        assert_eq!(
            pending_automation_trigger_count(host).unwrap(),
            0,
            "retiring an automation must void its pending outbox candidates in the same transaction"
        );
        let audited = automation_triggers_for_commit(host, trigger_commit_seq).unwrap();
        assert_eq!(audited.len(), 1, "{audited:?}");
        assert!(
            audited[0].voided_commit_seq.is_some(),
            "voided candidate must record its retire commit_seq"
        );

        let second_write = shared.call(
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            Some(&user_token("member")),
            &melt_body("melt-after-retire", "15", "12", "k-member-2"),
        );
        assert_eq!(second_write.status, 200, "{}", second_write.body);
        assert_eq!(
            pending_automation_trigger_count(shared.tenant.serving_host().unwrap()).unwrap(),
            0,
            "retired automation must neither keep pending candidates nor record new ones"
        );
    }

    /// 6c. Conditional policy rules (requiring a resource/context predicate the
    /// admission-time PDP check cannot establish) fail closed: there is no
    /// role-conjunct fallback outside Cedar.
    #[test]
    fn conditional_grant_is_refused_at_admission_without_role_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut artifacts = melt_artifacts_with_policy(&["owner"]);
        let policy = artifacts
            .iter_mut()
            .find(|artifact| artifact["kind"] == "policy")
            .unwrap();
        policy["payload"]["rules"][0]["allOf"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "resource": { "name": "id", "op": "eq", "value": "m" } }));
        let mut cell =
            provisioned_tenant_with_artifacts(&dir.path().join("conditional.sqlite"), &artifacts);
        let body = json!({
            "kind": "automation",
            "id": "ops.conditional_denied",
            "payload": automation_payload(
                "Conditional denied",
                &json!({ "kind": "event", "event": "created" }),
            ),
        });
        let refused = cell.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("owner")),
            &body.to_string(),
        );
        assert_eq!(
            refused.status, 403,
            "conditional rule must not bypass Cedar via role fallback: {}",
            refused.body
        );
    }

    /// 6d. Watching a supertype requires an explicit Cedar grant across every
    /// subtype the automation rule can match; granting only the supertype
    /// refuses, while granting supertype and subtype binds both parent grant ids.
    #[test]
    fn supertype_automation_requires_author_grant_on_every_subtype() {
        const SPECIAL_MELT: &str = "constitution.SpecialMelt";
        let special_entity = json!({
            "artifactId": "constitution.SpecialMelt.entity",
            "dependencies": [
                {
                    "artifactId": "constitution.Melt.entity",
                    "kind": "entity",
                    "required": true,
                    "version": "1.0.0"
                }
            ],
            "irVersion": "0.1",
            "kind": "entity",
            "lifecycle": "draft",
            "name": "Special fine-metal conversion",
            "namespace": "constitution",
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": SPECIAL_MELT,
                    "extends": MELT,
                    "properties": []
                }
            },
            "provenance": { "source": "authoring_sdk", "sourcePath": "tests/automations_u1.rs" },
            "scope": { "orgId": "org_a", "environmentId": "prod" },
            "version": "1.0.0"
        });

        let dir = tempfile::tempdir().unwrap();
        let mut parent_only_artifacts = melt_artifacts_with_policy(&["owner"]);
        parent_only_artifacts.push(special_entity.clone());
        let mut parent_only = provisioned_tenant_with_artifacts(
            &dir.path().join("parent-only.sqlite"),
            &parent_only_artifacts,
        );
        let watch_parent = json!({
            "kind": "automation",
            "id": "ops.watch_parent",
            "payload": automation_payload(
                "Watch parent",
                &json!({ "kind": "event", "event": "created" }),
            ),
        });
        let refused_parent = parent_only.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("owner")),
            &watch_parent.to_string(),
        );
        assert_eq!(
            refused_parent.status, 403,
            "missing subtype grant must refuse supertype automation: {}",
            refused_parent.body
        );

        let mut both_artifacts = melt_artifacts_with_policy(&["owner"]);
        both_artifacts.push(special_entity);
        let policy = both_artifacts
            .iter_mut()
            .find(|artifact| artifact["kind"] == "policy")
            .unwrap();
        policy["payload"]["rules"] = json!([
            {
                "ruleId": "melt_parent",
                "actions": [MELT_ACTION],
                "resourceType": MELT,
                "scope": { "dimensions": ["org", "environment"] },
                "allOf": [{ "role": { "anyOf": ["owner"] } }]
            },
            {
                "ruleId": "melt_special",
                "actions": [MELT_ACTION],
                "resourceType": SPECIAL_MELT,
                "scope": { "dimensions": ["org", "environment"] },
                "allOf": [{ "role": { "anyOf": ["owner"] } }]
            }
        ]);
        let mut both =
            provisioned_tenant_with_artifacts(&dir.path().join("both.sqlite"), &both_artifacts);
        let admitted = both.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("owner")),
            &watch_parent.to_string(),
        );
        assert_eq!(admitted.status, 200, "{}", admitted.body);
        let digest =
            serde_json::from_str::<Value>(&admitted.body).unwrap()["restatementSha256"].clone();
        let mut signed = watch_parent;
        signed["signoff"] = json!({ "restatementSha256": digest });
        let installed = both.call(
            "POST",
            "/v1/definitions:install",
            Some(&user_token("owner")),
            &signed.to_string(),
        );
        assert_eq!(installed.status, 200, "{}", installed.body);
        let stored = both
            .tenant
            .serving_host()
            .unwrap()
            .retained_definition("ops.watch_parent")
            .unwrap()
            .unwrap();
        let authority = stored.definition.authority.expect("bound authority");
        assert_eq!(
            authority.parent_grant_ids.len(),
            2,
            "{:?}",
            authority.parent_grant_ids
        );
        assert!(
            authority
                .parent_grant_ids
                .iter()
                .any(|id| id.contains("melt_parent")),
            "{:?}",
            authority.parent_grant_ids
        );
        assert!(
            authority
                .parent_grant_ids
                .iter()
                .any(|id| id.contains("melt_special")),
            "{:?}",
            authority.parent_grant_ids
        );
    }

    /// 7. Public action route unconditionally refuses automation-namespace
    ///    idempotency keys, both ungated and under an active approval gate.
    #[test]
    fn public_route_refuses_automation_namespace_idempotency_keys() {
        let cell = cell();
        install_approval(&cell);
        install_automation(
            &cell,
            "ops.key_watch",
            &automation_payload("Key watch", &json!({ "kind": "event", "event": "created" })),
        );

        let run_key = automation_run_key("ops.key_watch", 1, 5, MELT, "melt-1", 1).unwrap();
        let approved_key = automation_approved_key("dec_1").unwrap();
        for reserved in [
            "norn.automation.run.deadbeef",
            "norn.automation/custom",
            run_key.as_str(),
            approved_key.as_str(),
        ] {
            let (status, refusal) = call(
                &cell,
                "POST",
                &format!("/v1/actions/{MELT_ACTION}"),
                OPERATOR,
                &melt_body("melt-reserved", "12", "10", reserved),
            );
            assert_eq!(status, 400, "{reserved}: {refusal}");
            assert_eq!(refusal["code"], json!("request.invalid"), "{refusal}");
            assert_eq!(
                refusal["diagnostics"][0]["span"]["pointer"],
                json!("/idempotencyKey"),
                "{refusal}"
            );
        }

        // Nothing committed, nothing parked, and zero candidates or continuations recorded.
        let (get_status, _) = call(
            &cell,
            "GET",
            &format!("/v1/objects/{MELT}/melt-reserved"),
            OPERATOR,
            "",
        );
        assert_eq!(get_status, 404);
        let pending_query = format!("{SCOPE}&state=pending");
        let (_, page) = call_with(&cell, "GET", "/v1/decisions", &pending_query, APPROVER, "");
        assert_eq!(page["total"], json!(0), "{page}");
        assert_eq!(pending_automation_trigger_count(&cell.host).unwrap(), 0);
        assert_eq!(
            pending_automation_continuation_count(&cell.host).unwrap(),
            0
        );
    }

    /// 8. Hidden automations never alter or refuse business writes and never
    ///    leak their existence in ordinary action responses or refusals.
    #[test]
    fn hidden_automations_are_never_disclosed_in_business_write_responses_or_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let mut cell = provisioned_tenant(
            &dir.path().join("no-disclosure.sqlite"),
            &["owner", "member"],
        );
        let def = json!({
            "kind": "automation",
            "id": "ops.hidden_watch",
            "payload": automation_payload(
                "Hidden watch",
                &json!({ "kind": "event", "event": "created" }),
            ),
        });
        let admit = cell.call(
            "POST",
            "/v1/definitions:admit",
            Some(&user_token("owner")),
            &def.to_string(),
        );
        assert_eq!(admit.status, 200, "{}", admit.body);
        let digest =
            serde_json::from_str::<Value>(&admit.body).unwrap()["restatementSha256"].clone();
        let mut signed = def;
        signed["signoff"] = json!({ "restatementSha256": digest });
        let installed = cell.call(
            "POST",
            "/v1/definitions:install",
            Some(&user_token("owner")),
            &signed.to_string(),
        );
        assert_eq!(installed.status, 200, "{}", installed.body);

        // A member without definition visibility commits cleanly and sees no automation fields.
        let ok = cell.call(
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            Some(&user_token("member")),
            &melt_body("melt-member-ok", "15", "12", "k-member-ok"),
        );
        assert_eq!(ok.status, 200, "{}", ok.body);
        assert!(
            !ok.body.to_ascii_lowercase().contains("automation"),
            "ordinary write response must not reveal automations: {}",
            ok.body
        );

        // A failing write returns its ordinary business refusal with no automation code or detail,
        // and leaves no extra candidates or continuation rows behind.
        let bad = cell.call(
            "POST",
            &format!("/v1/actions/{MELT_ACTION}"),
            Some(&user_token("member")),
            &melt_body("melt-member-bad", "10", "15", "k-member-bad"),
        );
        assert_ne!(bad.status, 200, "{}", bad.body);
        assert_ne!(bad.status, 429, "{}", bad.body);
        assert!(
            !bad.body.to_ascii_lowercase().contains("automation"),
            "ordinary write refusal must not reveal automations: {}",
            bad.body
        );
        let host = cell.tenant.serving_host().unwrap();
        assert_eq!(pending_automation_trigger_count(host).unwrap(), 1);
        assert_eq!(pending_automation_continuation_count(host).unwrap(), 0);
    }
}
