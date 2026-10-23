use super::*;
use crate::e2b_execution_host::authority::PostgresE2bDispatchAuthority;
use crate::e2b_execution_host::{E2bDispatchAuthority, E2bDispatchResult, E2bHostError};
use crate::postgres_store::provider_operations::cleanup::*;
use crate::postgres_store::provider_operations::{
    ProviderCreationIdentity, ProviderCreationOperation,
};
use std::collections::BTreeMap;

fn terminal_cleanup(db: &mut Client, run: &mut Run) {
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_purpose='cleanup'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','provider-run',false,'{\"type\":\"agent_run_interrupted\"}'); UPDATE runtime.runtime_jobs SET lease_owner=NULL;").unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1,lease_purpose='control',lease_control_stage='cleanup'", &[&run.fence.lease_owner]).unwrap();
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch = 2;
    controller.controller_purpose = "cleanup".into();
    run.grant = retained::sign(run.grant.payload.clone());
}

fn dispatched(store: &PostgresRuntimeStore, db: &mut Client) -> (Run, ProviderCreationOperation) {
    let mut run = run(store, db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let operation = store
        .record_provider_creation_observation(&operation.identity, "sandbox-opaque")
        .unwrap();
    terminal_cleanup(db, &mut run);
    (run, operation)
}

fn observation(identity: &ProviderCreationIdentity) -> ProviderSandboxObservation {
    ProviderSandboxObservation {
        sandbox_id: "sandbox-opaque".into(),
        template_id: "centaeris-core".into(),
        metadata: BTreeMap::from([
            ("centaerisAttemptId".into(), identity.attempt_id.clone()),
            ("environmentId".into(), identity.environment_id.clone()),
            ("instanceId".into(), identity.instance_id.clone()),
            ("executionId".into(), identity.execution_id.clone()),
            ("agentRunId".into(), identity.agent_run_id.clone()),
            ("sessionId".into(), identity.session_id.clone()),
            ("workspaceId".into(), identity.workspace_id.clone()),
            ("profileDigest".into(), identity.profile_digest.clone()),
            (
                "authorizationDigest".into(),
                identity.authorization_digest.clone(),
            ),
            ("processingJobId".into(), identity.processing_job_id.clone()),
        ]),
    }
}

fn execution_candidate(
    store: &PostgresRuntimeStore,
    db: &mut Client,
) -> (Run, ProviderCreationOperation) {
    let run = run(store, db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let operation = store
        .record_provider_creation_observation(&operation.identity, "sandbox-opaque")
        .unwrap();
    (run, operation)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_execute_verification_survives_restart_and_expiry_before_cleanup() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut run, operation) = execution_candidate(&store, &mut db);
    assert!(matches!(
        store.with_provider_execution_dispatch(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
            CONFIG,
            |_| -> Result<(), String> { panic!("candidate alone must not authorize an action") }
        ),
        Err(ProviderExecutionDispatchError::Rejected(_))
    ));
    let target = store
        .record_provider_execution_target(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
            CONFIG,
            &observation(&operation.identity),
        )
        .unwrap();
    let verified_at: i64 = db
        .query_one(
            "SELECT verified_at_ms FROM runtime.provider_creation_operations WHERE attempt_id=$1",
            &[&operation.identity.attempt_id],
        )
        .unwrap()
        .get(0);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .record_provider_execution_target(
                &run.authority(ControllerPurpose::Execute),
                &operation.identity,
                CONFIG,
                &observation(&operation.identity)
            )
            .unwrap(),
        target
    );
    assert_eq!(
        db.query_one(
            "SELECT verified_at_ms FROM runtime.provider_creation_operations WHERE attempt_id=$1",
            &[&operation.identity.attempt_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        verified_at
    );
    assert_eq!(reopened.with_provider_execution_dispatch(&run.authority(ControllerPurpose::Execute), &operation.identity, CONFIG, |actual| -> Result<u32, String> {
        assert_eq!(actual, &target);
        assert_eq!(db.query_one("SELECT verified_sandbox_id FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&operation.identity.attempt_id]).unwrap().get::<_,String>(0), target.sandbox_id, "binding is committed before physical start");
        for query in ["SELECT id FROM public.app_core_sessionenvironment WHERE id='environment' FOR UPDATE NOWAIT", "SELECT job_id FROM runtime.runtime_jobs WHERE job_id='agent_run.lifecycle:provider-run' FOR UPDATE NOWAIT"] {
            let mut competing = db.transaction().unwrap();
            assert_eq!(competing.query(query, &[]).unwrap_err().code(), Some(&postgres::error::SqlState::LOCK_NOT_AVAILABLE), "current authority remains pinned across start");
        }
        Ok(17)
    }).unwrap(), 17);
    terminal_cleanup(&mut db, &mut run);
    assert_eq!(
        reopened
            .verify_provider_cleanup_target(
                &run.authority(ControllerPurpose::Cleanup),
                &operation.identity,
                CONFIG,
                |_, _| panic!("expired VM cannot provide another GET; durable binding must replay")
            )
            .unwrap(),
        target
    );
    assert_eq!(
        reopened
            .remove_provider_creation(
                &run.authority(ControllerPurpose::Cleanup),
                &operation.identity,
                CONFIG,
                |target| Ok(ProviderRemovalEvidence::NotFound {
                    sandbox_id: target.sandbox_id.clone()
                })
            )
            .unwrap()
            .kind,
        ProviderSettlementKind::Removed
    );
    assert_eq!(
        reopened
            .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap()
            .operation_count,
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_execute_verification_rejects_stale_control_and_mismatched_observations() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = execution_candidate(&store, &mut db);
    for field in [
        "sandbox", "template", "metadata", "extra", "identity", "config",
    ] {
        let mut info = observation(&operation.identity);
        let mut identity = operation.identity.clone();
        match field {
            "sandbox" => info.sandbox_id.push('x'),
            "template" => info.template_id.push('x'),
            "metadata" => {
                info.metadata.remove("profileDigest");
            }
            "extra" => {
                info.metadata.insert("foreign".into(), "unbound".into());
            }
            "identity" => identity.request_digest.push('0'),
            _ => {}
        }
        assert!(
            store
                .record_provider_execution_target(
                    &run.authority(ControllerPurpose::Execute),
                    &identity,
                    if field == "config" { REQUEST } else { CONFIG },
                    &info
                )
                .is_err(),
            "{field}"
        );
    }
    assert!(store
        .record_provider_execution_target(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            &observation(&operation.identity)
        )
        .is_err());
    for mutation in ["UPDATE public.app_core_sessionenvironment SET controller_epoch=2", "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1", "UPDATE runtime.runtime_jobs SET lease_owner=NULL; UPDATE runtime.runtime_jobs SET lease_purpose='control',lease_control_stage='advance',lease_owner='worker:provider-creation'"] {
        db.batch_execute(mutation).unwrap();
        assert!(store.record_provider_execution_target(&run.authority(ControllerPurpose::Execute), &operation.identity, CONFIG, &observation(&operation.identity)).is_err());
        db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=1; UPDATE runtime.runtime_jobs SET lease_owner=NULL; UPDATE runtime.runtime_jobs SET lease_purpose='execute',lease_control_stage=NULL,lease_owner='worker:provider-creation',lease_expires_at_ms=4000000000000").unwrap();
    }
    assert!(db.query_one("SELECT verified_sandbox_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
    store
        .record_provider_execution_target(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
            CONFIG,
            &observation(&operation.identity),
        )
        .unwrap();
    for mutation in ["UPDATE public.app_core_sessionenvironment SET controller_epoch=2", "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1", "UPDATE runtime.runtime_jobs SET lease_owner=NULL; UPDATE runtime.runtime_jobs SET lease_purpose='control',lease_control_stage='advance',lease_owner='worker:provider-creation'"] {
        db.batch_execute(mutation).unwrap();
        assert!(matches!(store.with_provider_execution_dispatch(&run.authority(ControllerPurpose::Execute), &operation.identity, CONFIG, |_| -> Result<(), String> { panic!("invalid authority must not reach physical start") }), Err(ProviderExecutionDispatchError::Rejected(_))));
        db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=1; UPDATE runtime.runtime_jobs SET lease_owner=NULL; UPDATE runtime.runtime_jobs SET lease_purpose='execute',lease_control_stage=NULL,lease_owner='worker:provider-creation',lease_expires_at_ms=4000000000000").unwrap();
    }
    db.batch_execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+1000").unwrap();
    assert!(
        matches!(
            store.with_provider_execution_dispatch(
                &run.authority(ControllerPurpose::Execute),
                &operation.identity,
                CONFIG,
                |_| -> Result<(), String> {
                    std::thread::sleep(Duration::from_millis(1100));
                    Ok(())
                }
            ),
            Err(ProviderExecutionDispatchError::AfterDispatch(_))
        ),
        "a start followed by lease expiry must remain Unknown rather than look undispatched"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_execute_dispatch_authority_uses_current_target_and_preserves_unknown() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = execution_candidate(&store, &mut db);
    store
        .record_provider_execution_target(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
            CONFIG,
            &observation(&operation.identity),
        )
        .unwrap();
    let authority = PostgresE2bDispatchAuthority::new(
        store.clone(),
        &run.authority(ControllerPurpose::Execute),
        operation.identity.clone(),
        CONFIG,
        "sandbox-opaque",
    )
    .unwrap();
    let mut calls = 0;
    assert!(matches!(
        authority.dispatch(&mut || {
            calls += 1;
            Ok(E2bDispatchResult::Process(17))
        }),
        Ok(E2bDispatchResult::Process(17))
    ));
    assert_eq!(calls, 1, "one guarded invocation sends exactly once");

    let wrong_sandbox = PostgresE2bDispatchAuthority::new(
        store.clone(),
        &run.authority(ControllerPurpose::Execute),
        operation.identity.clone(),
        CONFIG,
        "another-sandbox",
    )
    .unwrap();
    assert!(matches!(
        wrong_sandbox.dispatch(&mut || panic!("wrong sandbox cannot start")),
        Err(E2bHostError::NotDispatched(_))
    ));
    assert!(PostgresE2bDispatchAuthority::new(
        store.clone(),
        &run.authority(ControllerPurpose::Cleanup),
        operation.identity.clone(),
        CONFIG,
        "sandbox-opaque",
    )
    .is_err());
    assert!(PostgresE2bDispatchAuthority::new(
        store.clone(),
        &run.authority(ControllerPurpose::Execute),
        operation.identity.clone(),
        REQUEST,
        "sandbox-opaque",
    )
    .is_err());
    for expected in [
        E2bHostError::NotDispatched("local_preflight_rejected"),
        E2bHostError::Unknown("provider_reply_lost"),
    ] {
        assert!(matches!(
            authority.dispatch(&mut || Err(expected)),
            Err(actual) if actual == expected
        ));
    }
    db.batch_execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1")
        .unwrap();
    assert!(matches!(
        authority.dispatch(&mut || panic!("expired lease cannot start")),
        Err(E2bHostError::NotDispatched(_))
    ));
    db.batch_execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+1000").unwrap();
    let mut started = false;
    assert!(matches!(
        authority.dispatch(&mut || {
            started = true;
            std::thread::sleep(Duration::from_millis(1100));
            Ok(E2bDispatchResult::Process(18))
        }),
        Err(E2bHostError::Unknown(_))
    ));
    assert!(started, "post-start expiry cannot be reported as no effect");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_verifies_target_durably_before_exact_removal() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (run, operation) = dispatched(&store, &mut db);
    let target = store
        .verify_provider_cleanup_target(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |identity, id| {
                assert_eq!(id, "sandbox-opaque");
                Ok(observation(identity))
            },
        )
        .unwrap();
    assert_eq!(target.sandbox_id, "sandbox-opaque");
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let settled = reopened
        .remove_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |actual| {
                assert_eq!(actual, &target);
                Ok(ProviderRemovalEvidence::Deleted {
                    sandbox_id: actual.sandbox_id.clone(),
                })
            },
        )
        .unwrap();
    assert_eq!(settled.kind, ProviderSettlementKind::Removed);
    assert_eq!(
        reopened
            .remove_provider_creation(
                &run.authority(ControllerPurpose::Cleanup),
                &operation.identity,
                CONFIG,
                |_| panic!("settled replay must not delete again")
            )
            .unwrap(),
        settled
    );
    assert_eq!(
        store
            .provider_creation_operation("attempt-provider")
            .unwrap()
            .unwrap()
            .dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_rejects_unverified_target_metadata_and_stale_authority_before_actions()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = dispatched(&store, &mut db);
    assert!(store
        .remove_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |_| panic!("candidate is not deletion authority")
        )
        .is_err());
    for field in ["sandbox", "template", "metadata", "extra"] {
        assert!(
            store
                .verify_provider_cleanup_target(
                    &run.authority(ControllerPurpose::Cleanup),
                    &operation.identity,
                    CONFIG,
                    |identity, _| {
                        let mut info = observation(identity);
                        match field {
                            "sandbox" => info.sandbox_id.push('x'),
                            "template" => info.template_id.push('x'),
                            "extra" => {
                                info.metadata.insert("foreign".into(), "unbound".into());
                            }
                            _ => {
                                info.metadata.remove("profileDigest");
                            }
                        }
                        Ok(info)
                    }
                )
                .is_err(),
            "{field}"
        );
    }
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=3")
        .unwrap();
    assert!(store
        .verify_provider_cleanup_target(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |_, _| panic!("old controller cannot inspect")
        )
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_unknown_and_lost_commit_keep_responsibility_until_exact_not_found() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (run, operation) = dispatched(&store, &mut db);
    store
        .verify_provider_cleanup_target(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |identity, _| Ok(observation(identity)),
        )
        .unwrap();
    assert!(store
        .remove_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |_| Err("transport_unknown".into())
        )
        .is_err());
    assert!(store
        .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
        .is_err());
    // Physical deletion can succeed while authority expires before DB settlement.
    db.batch_execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+1000").unwrap();
    assert!(store
        .remove_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |target| {
                std::thread::sleep(Duration::from_millis(1100));
                Ok(ProviderRemovalEvidence::Deleted {
                    sandbox_id: target.sandbox_id.clone(),
                })
            }
        )
        .is_err());
    db.batch_execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=4000000000000")
        .unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let settled = reopened
        .remove_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |target| {
                Ok(ProviderRemovalEvidence::NotFound {
                    sandbox_id: target.sandbox_id.clone(),
                })
            },
        )
        .unwrap();
    assert_eq!(settled.kind, ProviderSettlementKind::Removed);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_unsent_and_removed_settlements_form_one_replayable_v2_receipt() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let mut run = run(&store, &mut db);
    let unsent = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-unsent", "attempt-unsent", "key-unsent"),
        )
        .unwrap();
    store
        .close_provider_creation_before_dispatch(
            &run.authority(ControllerPurpose::Execute),
            &unsent.identity,
        )
        .unwrap();
    terminal_cleanup(&mut db, &mut run);
    let settled = store
        .settle_provider_creation_not_dispatched(
            &run.authority(ControllerPurpose::Cleanup),
            &unsent.identity,
        )
        .unwrap();
    assert_eq!(settled.kind, ProviderSettlementKind::NotDispatched);
    let receipt = store
        .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
        .unwrap();
    assert_eq!(receipt.schema, "runtime.agent_run.cleanup_receipt.v2");
    assert_eq!(receipt.operation_count, 1);
    let query = ProviderCleanupReceiptQuery {
        schema: "runtime.agent_run.cleanup_receipt.get.v2".into(),
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        cleanup_receipt_id: receipt.receipt_id.clone(),
    };
    assert_eq!(
        PostgresRuntimeStore::new(&url)
            .unwrap()
            .get_provider_cleanup_receipt(&query)
            .unwrap(),
        Some(receipt.clone().into())
    );
    assert_eq!(
        store
            .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        receipt
    );
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=3")
        .unwrap();
    run.grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_epoch = 3;
    run.grant = retained::sign(run.grant.payload.clone());
    assert_eq!(
        store
            .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        receipt,
        "older immutable evidence survives a new current cleanup epoch"
    );
}

fn release_table(db: &mut Client) {
    db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL); DELETE FROM public.app_core_agentrunenvironmentrelease;").unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_unknown_cannot_hide_behind_legacy_receipts_or_no_effects() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, _) = dispatched(&store, &mut db);
    release_table(&mut db);
    let digest = run.original.digest().unwrap();
    db.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,cleanup_complete,file_protection,recorded_at_ms) VALUES('fake-legacy',$1,'environment',$2,2,'neverAllocated',true,'deferred',1)", &[&run.original.agent_run_id, &digest]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunenvironmentrelease VALUES($1,'environment',$2,2)",
        &[&run.original.agent_run_id, &digest],
    )
    .unwrap();
    assert!(
        store
            .verify_lifecycle_completion(&run.fence.job_id)
            .is_err(),
        "Unknown provider creation cannot complete using a legacy receipt"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_journal_defeats_no_effects_even_without_current_controller() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, _) = dispatched(&store, &mut db);
    db.batch_execute("DELETE FROM public.app_core_sessionevent; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let control = crate::contract::AgentRunControl {
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        authorization_signature: run.original.signature(KEY).unwrap(),
        authorization: run.original.clone(),
    };
    assert!(store.confirm_no_effects(&control, &run.fence.job_id, &run.fence.lease_owner).unwrap().is_none(), "a provider attempt remains a durable effect even without Core history or controller ownership");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_v2_and_release_gate_completion_and_next_per_run() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = dispatched(&store, &mut db);
    release_table(&mut db);
    store
        .verify_provider_cleanup_target(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |identity, _| Ok(observation(identity)),
        )
        .unwrap();
    store
        .remove_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |target| {
                Ok(ProviderRemovalEvidence::Deleted {
                    sandbox_id: target.sandbox_id.clone(),
                })
            },
        )
        .unwrap();
    assert!(
        store
            .verify_lifecycle_completion(&run.fence.job_id)
            .is_err(),
        "settlement alone does not release business ownership"
    );
    let receipt = store
        .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
        .unwrap();
    assert!(
        store
            .verify_lifecycle_completion(&run.fence.job_id)
            .is_err(),
        "cleanup without release must remain pending"
    );
    db.execute(
        "INSERT INTO public.app_core_agentrunenvironmentrelease VALUES($1,'environment',$2,$3)",
        &[
            &run.original.agent_run_id,
            &receipt.authorization_digest,
            &receipt.controller_epoch,
        ],
    )
    .unwrap();
    assert!(store.verify_lifecycle_completion(&run.fence.job_id).is_ok());
    // The next business owner receives a fresh grant and Job; no Docker allocation is manufactured.
    let mut next = run.original.clone();
    next.agent_run_id = "provider-next-run".into();
    next.id = "provider-next-authorization".into();
    let digest = next.digest().unwrap();
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,'sess_1','environment','ws_1','user_1','member','running','model_1','high')", &[&next.agent_run_id]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &next.agent_run_id,
            &digest,
            &serde_json::to_string(&next).unwrap(),
            &next.signature(KEY).unwrap(),
        ],
    )
    .unwrap();
    let job_id = format!("agent_run.lifecycle:{}", next.agent_run_id);
    let mut next_job = job(&job_id, &format!("{job_id}:{digest}"));
    next_job.job_kind = "agent_run.lifecycle".into();
    next_job.status = RuntimeJobStatus::Running;
    next_job.lease_owner = Some(run.fence.lease_owner.clone());
    next_job.lease_expires_at_ms = Some(4_000_000_000_000);
    next_job.session_id = Some(next.session_id.clone());
    next_job.payload_ref = Some(format!("record:agent_run:{}", next.agent_run_id));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: next_job }, Some("ws_1"))
        .unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=$1,controller_epoch=3,controller_purpose='execute'", &[&next.agent_run_id]).unwrap();
    let mut payload = run.grant.payload.clone();
    payload.agent_run_id = next.agent_run_id.clone();
    payload.agent_run_authorization_digest = digest;
    let controller = payload.controller.as_mut().unwrap();
    controller.controller_id = next.agent_run_id.clone();
    controller.controller_epoch = 3;
    controller.controller_purpose = "execute".into();
    let next = Run {
        original: next,
        grant: retained::sign(payload),
        fence: RuntimeJobLeaseFence {
            job_id,
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: run.fence.lease_owner.clone(),
        },
    };
    assert!(store
        .prepare_provider_creation(
            &next.authority(ControllerPurpose::Execute),
            &request("execution-next", "attempt-next", "key-next")
        )
        .is_ok());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_cleanup_schema_upgrade_preserves_unknown_and_original_signed_facts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (run, operation) = dispatched(&store, &mut db);
    let original: String = db.query_one("SELECT row_to_json(g)::text FROM public.app_core_agentrunauthorization g WHERE agent_run_id=$1", &[&run.original.agent_run_id]).unwrap().get(0);
    crate::postgres_store::provider_operations::cleanup::restore_previous_fixture(&mut db);
    let history: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(history.last().unwrap().0, 17);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .provider_creation_operation(&operation.identity.attempt_id)
            .unwrap(),
        Some(operation)
    );
    assert_eq!(db.query_one("SELECT row_to_json(g)::text FROM public.app_core_agentrunauthorization g WHERE agent_run_id=$1", &[&run.original.agent_run_id]).unwrap().get::<_,String>(0), original);
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=17 ORDER BY version", &[]).unwrap().iter().map(|row| (row.get(0),row.get(1))).collect::<Vec<(i64,i64)>>(), history);
    assert_eq!(db.batch_execute("UPDATE runtime.provider_creation_operations SET settlement_id='malformed',settlement_kind=NULL,settlement_controller_epoch=2,settled_at_ms=0").unwrap_err().code(), Some(&postgres::error::SqlState::CHECK_VIOLATION));
    assert!(reopened
        .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
        .is_err());
}
