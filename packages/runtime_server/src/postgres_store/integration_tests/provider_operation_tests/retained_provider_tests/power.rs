use super::uses::{candidate, proof, release, successor};
use super::*;
use crate::postgres_store::provider_operations::retained::power::{
    ProviderEnvironmentPowerAcknowledgement as Ack, ProviderEnvironmentPowerAction as Action,
    ProviderEnvironmentPowerDispatchState as Dispatch, ProviderEnvironmentPowerRequest as Request,
};

fn request(id: &str, action: Action, predecessor: Option<&str>, epoch: i64) -> Request {
    Request {
        schema: "runtime.provider_environment.power.v1".into(),
        operation_id: id.into(),
        action,
        environment_id: "environment".into(),
        creation_attempt_id: "retained-attempt".into(),
        sandbox_id: "retained-sandbox".into(),
        provider_config_digest: CONFIG.into(),
        expected_controller_epoch: epoch,
        predecessor_operation_id: predecessor.map(str::to_owned),
    }
}

fn ack(request: &Request) -> Ack {
    match request.action {
        Action::Pause => Ack::Paused {
            sandbox_id: request.sandbox_id.clone(),
            changed: true,
            requested_mode: "full".into(),
        },
        Action::Resume => Ack::Resumed {
            sandbox_id: request.sandbox_id.clone(),
        },
    }
}

fn close_run(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &mut Run,
    use_: &crate::postgres_store::provider_operations::retained::RetainedProviderRunUse,
) {
    terminal(db, run);
    let epoch = run
        .grant
        .payload
        .controller
        .as_ref()
        .unwrap()
        .controller_epoch;
    store
        .confirm_retained_provider_run_cleanup(
            &run.authority(ControllerPurpose::Cleanup),
            use_,
            CONFIG,
            |value| Ok(proof(value, epoch)),
        )
        .unwrap();
    release(db, run);
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: run.fence.job_id.clone(),
            lease_owner: run.fence.lease_owner.clone(),
            output_refs: vec![],
            completed_at_ms: crate::now_ms().unwrap(),
        })
        .unwrap();
}

#[test]
fn provider_power_request_requires_explicit_nullable_predecessor_and_exact_public_contract() {
    let value = request("pause-a", Action::Pause, None, 2);
    value.validate().unwrap();
    let json = serde_json::to_value(&value).unwrap();
    assert!(json["predecessorOperationId"].is_null());
    assert_eq!(json["action"], "pause");
    let mut missing = json.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("predecessorOperationId");
    assert!(serde_json::from_value::<Request>(missing).is_err());
    for (field, bad) in [
        ("action", serde_json::json!("stop")),
        ("action", serde_json::json!("Pause")),
        ("schema", serde_json::json!("runtime.environment.power.v1")),
        ("expectedControllerEpoch", serde_json::json!(-1)),
        ("providerConfigDigest", serde_json::json!("invalid")),
        ("sandboxId", serde_json::json!("bad/path")),
    ] {
        let mut bad_json = json.clone();
        bad_json[field] = bad;
        assert!(serde_json::from_value::<Request>(bad_json)
            .and_then(|r| r.validate().map(|_| r).map_err(serde::de::Error::custom))
            .is_err());
    }
    let mut unknown = json;
    unknown["agentRunId"] = serde_json::json!("creator");
    assert!(serde_json::from_value::<Request>(unknown).is_err());
    assert!(request("resume", Action::Resume, None, 2)
        .validate()
        .is_err());
    assert!(request("same", Action::Pause, Some("same"), 2)
        .validate()
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_intent_precedes_effect_and_fresh_dispatch_is_single_use() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-a", Action::Pause, None, 2);
    let prepared = store
        .prepare_provider_environment_power(&req, CONFIG)
        .unwrap();
    assert_eq!(prepared.sequence, 1);
    assert_eq!(prepared.dispatch_state, Dispatch::Prepared);
    assert!(prepared.acknowledgement.is_none());
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_environment_power_operations WHERE operation_id='pause-a' AND dispatch_state='prepared'",&[]).unwrap().get::<_,i64>(0),1);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .provider_environment_power_operation("pause-a")
            .unwrap(),
        Some(prepared)
    );
    let permit = reopened
        .dispatch_provider_environment_power(&req, CONFIG)
        .unwrap()
        .unwrap();
    assert_eq!(permit.operation().dispatch_state, Dispatch::Dispatched);
    assert!(store
        .dispatch_provider_environment_power(&req, CONFIG)
        .unwrap()
        .is_none());
    let result=reopened.with_provider_environment_power_dispatch(permit,CONFIG,|target,request| {
        assert_eq!(target,&use_a.target); assert_eq!(request,&req);
        let row=db.query_one("SELECT dispatch_state,acknowledged_at_ms IS NULL FROM runtime.provider_environment_power_operations WHERE operation_id='pause-a'",&[]).unwrap();
        assert_eq!(row.get::<_,String>(0),"dispatched"); assert!(row.get::<_,bool>(1));
        let busy=db.query("SELECT id FROM public.app_core_sessionenvironment WHERE id='environment' FOR UPDATE NOWAIT",&[]).unwrap_err();
        assert_eq!(busy.code(),Some(&postgres::error::SqlState::LOCK_NOT_AVAILABLE),"provider effect retains the exact Environment pin");
        Ok(ack(request))
    }).unwrap();
    assert_eq!(result.operation_status, "acknowledged");
    assert_eq!(result.requested_mode, Some("full"));
    assert_eq!(
        result.schema,
        "runtime.provider_environment.power.result.v1"
    );
    let json = serde_json::to_value(result).unwrap();
    assert!(json.get("currentState").is_none());
    assert!(json.get("observedFull").is_none());
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_unknown_never_redispatches_or_reverses_after_restart() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-a", Action::Pause, None, 2);
    let mut calls = 0;
    assert_eq!(
        store
            .advance_provider_environment_power(&req, CONFIG, |_, _| {
                calls += 1;
                Err("private provider failure".into())
            })
            .unwrap()
            .operation_status,
        "pending"
    );
    assert_eq!(calls, 1);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let retirement=crate::postgres_store::provider_operations::retained::retirement::ProviderEnvironmentRetirementRequest {
        schema:"runtime.provider_environment.retire.v1".into(),operation_id:"retire-unknown-power".into(),environment_id:"environment".into(),
        creation_attempt_id:"retained-attempt".into(),sandbox_id:"retained-sandbox".into(),provider_config_digest:CONFIG.into(),
        expected_controller_epoch:2,acknowledge_data_loss:true,
    };
    assert!(reopened
        .retire_provider_environment(&retirement, CONFIG, |_| panic!(
            "Unknown power cannot race independent removal"
        ))
        .is_err());
    let pending = reopened
        .advance_provider_environment_power(&req, CONFIG, |_, _| {
            panic!("Unknown cannot dispatch twice")
        })
        .unwrap();
    assert_eq!(pending.operation_status, "pending");
    assert!(!serde_json::to_string(&pending).unwrap().contains("private"));
    assert!(reopened
        .advance_provider_environment_power(
            &request("resume-a", Action::Resume, Some("pause-a"), 2),
            CONFIG,
            |_, _| panic!("Unknown pause cannot be reversed")
        )
        .is_err());
    assert!(reopened
        .advance_provider_environment_power(
            &request("pause-b", Action::Pause, None, 2),
            CONFIG,
            |_, _| panic!("Unknown cannot be replaced")
        )
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_power_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_prepared_restart_continues_but_committed_ack_replays_without_effect() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-a", Action::Pause, None, 2);
    store
        .prepare_provider_environment_power(&req, CONFIG)
        .unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let result = reopened
        .advance_provider_environment_power(&req, CONFIG, |_, r| Ok(ack(r)))
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=3")
        .unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .advance_provider_environment_power(&req, CONFIG, |_, _| panic!(
                "ACK replay is evidence, never another effect"
            ))
            .unwrap(),
        result
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_cycles_fence_stale_replay_and_require_latest_predecessor() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let p1 = request("pause-a", Action::Pause, None, 2);
    let r1 = request("resume-a", Action::Resume, Some("pause-a"), 2);
    let p2 = request("pause-b", Action::Pause, Some("resume-a"), 2);
    for (req, sequence) in [(&p1, 1), (&r1, 2), (&p2, 3)] {
        let result = store
            .advance_provider_environment_power(req, CONFIG, |_, r| Ok(ack(r)))
            .unwrap();
        assert_eq!(result.sequence, sequence);
        assert_eq!(result.operation_status, "acknowledged");
    }
    assert_eq!(
        store
            .advance_provider_environment_power(&r1, CONFIG, |_, _| panic!(
                "historical resume must not wake newer pause"
            ))
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(
        store
            .advance_provider_environment_power(&p1, CONFIG, |_, _| panic!(
                "historical pause must not dispatch"
            ))
            .unwrap()
            .sequence,
        1
    );
    for req in [
        request("resume-stale", Action::Resume, Some("pause-a"), 2),
        request("pause-stale", Action::Pause, Some("resume-a"), 2),
        request("pause-no-tail", Action::Pause, None, 2),
    ] {
        assert!(store
            .advance_provider_environment_power(&req, CONFIG, |_, _| panic!(
                "non-latest predecessor cannot dispatch"
            ))
            .is_err());
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_power_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        3
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_pause_barrier_blocks_successor_until_positive_resume_ack() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, creation, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("pause-a", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    let b = successor(&store, &mut db, &a);
    assert!(store
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b-execution",
            &creation.identity.attempt_id,
            CONFIG
        )
        .is_err());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store
        .advance_provider_environment_power(
            &request("resume-a", Action::Resume, Some("pause-a"), 3),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=false")
        .unwrap();
    assert!(store
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b-execution",
            &creation.identity.attempt_id,
            CONFIG
        )
        .is_ok());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_rejects_wrong_identity_epoch_config_and_conflicting_operation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-a", Action::Pause, None, 2);
    for field in 0..5 {
        let mut bad = req.clone();
        match field {
            0 => bad.environment_id = "foreign-env".into(),
            1 => bad.creation_attempt_id = "foreign-attempt".into(),
            2 => bad.sandbox_id = "foreign-sandbox".into(),
            3 => bad.expected_controller_epoch = 1,
            _ => bad.provider_config_digest = REQUEST.into(),
        };
        assert!(store
            .prepare_provider_environment_power(&bad, CONFIG)
            .is_err());
    }
    assert!(store
        .prepare_provider_environment_power(&req, REQUEST)
        .is_err());
    store
        .prepare_provider_environment_power(&req, CONFIG)
        .unwrap();
    let mut changed = req.clone();
    changed.expected_controller_epoch = 3;
    assert!(store
        .prepare_provider_environment_power(&changed, CONFIG)
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_power_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_requires_original_and_all_consumer_cleanup_release_before_intent() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    assert!(store
        .prepare_provider_environment_power(&request("pause-a", Action::Pause, None, 1), CONFIG)
        .is_err());
    terminal(&mut db, &mut a);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .prepare_provider_environment_power(&request("pause-a", Action::Pause, None, 2), CONFIG)
        .is_err());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=false")
        .unwrap();
    store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |v| Ok(proof(v, 2)),
        )
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .prepare_provider_environment_power(&request("pause-a", Action::Pause, None, 2), CONFIG)
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_power_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_false_pause_or_foreign_ack_preserves_unknown_barrier() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for acknowledgement in [
        Ack::Paused {
            sandbox_id: "retained-sandbox".into(),
            changed: false,
            requested_mode: "full".into(),
        },
        Ack::Paused {
            sandbox_id: "retained-sandbox".into(),
            changed: true,
            requested_mode: "filesystemOnly".into(),
        },
        Ack::Paused {
            sandbox_id: "foreign".into(),
            changed: true,
            requested_mode: "full".into(),
        },
        Ack::Resumed {
            sandbox_id: "retained-sandbox".into(),
        },
    ] {
        let (_, store, mut db) = retained::setup();
        let (mut a, _, use_a) = candidate(&store, &mut db);
        close_run(&store, &mut db, &mut a, &use_a);
        let req = request("pause-a", Action::Pause, None, 2);
        assert_eq!(
            store
                .advance_provider_environment_power(&req, CONFIG, |_, _| Ok(acknowledgement))
                .unwrap()
                .operation_status,
            "pending"
        );
        assert_eq!(
            store
                .advance_provider_environment_power(&req, CONFIG, |_, _| panic!(
                    "invalid ACK never permits repeat"
                ))
                .unwrap()
                .operation_status,
            "pending"
        );
        assert!(db.query_one("SELECT acknowledged_at_ms IS NULL AND dispatch_state='dispatched' FROM runtime.provider_environment_power_operations",&[]).unwrap().get::<_,bool>(0));
    }
}

fn waiting_consumer(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    source: &Run,
    name: &str,
    environment: &str,
) -> (String, String) {
    let mut original = source.original.clone();
    original.agent_run_id = name.into();
    original.id = format!("{name}-authorization");
    let digest = original.digest().unwrap();
    let job_id = format!("agent_run.lifecycle:{name}");
    let owner = format!("worker:{name}");
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,'sess_1',$2,'ws_1','user_1','member','queued','model_1','high')",&[&name,&environment]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &name,
            &digest,
            &serde_json::to_string(&original).unwrap(),
            &original.signature(KEY).unwrap(),
        ],
    )
    .unwrap();
    let mut record = job(&job_id, &format!("{job_id}:{digest}"));
    record.job_kind = "agent_run.lifecycle".into();
    record.status = RuntimeJobStatus::Running;
    record.lease_owner = Some(owner.clone());
    record.lease_expires_at_ms = Some(4_000_000_000_000);
    record.session_id = Some("sess_1".into());
    record.payload_ref = Some(format!("record:agent_run:{name}"));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some("ws_1"))
        .unwrap();
    (job_id, owner)
}

fn yield_waiter(store: &PostgresRuntimeStore, job_id: &str, owner: &str, now: i64) {
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: job_id.into(),
            lease_owner: owner.into(),
            yielded_at_ms: now,
            run_at_ms: now + 30_000,
            transition_reason: "provider_environment_power_pending".into(),
        })
        .unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_resume_after_yield_wakes_only_bound_environment_jobs() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("pause-a", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    let (job_id, owner) = waiting_consumer(&store, &mut db, &a, "power-waiter", "environment");
    let (foreign_id, foreign_owner) =
        waiting_consumer(&store, &mut db, &a, "power-foreign", "foreign-environment");
    let now = crate::now_ms().unwrap();
    yield_waiter(&store, &job_id, &owner, now);
    yield_waiter(&store, &foreign_id, &foreign_owner, now);
    store
        .advance_provider_environment_power(
            &request("resume-a", Action::Resume, Some("pause-a"), 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    assert!(db
        .query_one(
            "SELECT run_at_ms<$2 FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&job_id, &(now + 30_000)]
        )
        .unwrap()
        .get::<_, bool>(0));
    assert_eq!(
        db.query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&foreign_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        now + 30_000
    );
    let row=db.query_one("SELECT event_id,payload_json FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested'",&[]).unwrap();
    assert_eq!(
        row.get::<_, String>(0),
        format!("runtime_job_wake:{job_id}:provider_power:resume-a")
    );
    let payload: serde_json::Value = serde_json::from_str(&row.get::<_, String>(1)).unwrap();
    assert_eq!(
        payload["schema"],
        "runtime.provider_environment.power.wake.v1"
    );
    assert_eq!(payload["sourceOperationId"], "resume-a");
    assert!(payload.get("sourceJobId").is_none());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_resume_before_yield_is_durable_and_consumed_without_recheck_delay() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("pause-a", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    let (job_id, owner) = waiting_consumer(&store, &mut db, &a, "power-waiter", "environment");
    store
        .advance_provider_environment_power(
            &request("resume-a", Action::Resume, Some("pause-a"), 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested'",&[]).unwrap().get::<_,i64>(0),1);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let now = crate::now_ms().unwrap();
    yield_waiter(&reopened, &job_id, &owner, now);
    assert_eq!(
        db.query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&job_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        now
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime_job_wake_consumed'",&[]).unwrap().get::<_,i64>(0),1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_resume_ack_replay_never_rewrites_due_or_duplicates_wake() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("pause-a", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    let (job_id, owner) = waiting_consumer(&store, &mut db, &a, "power-waiter", "environment");
    let now = crate::now_ms().unwrap();
    yield_waiter(&store, &job_id, &owner, now);
    let resume = request("resume-a", Action::Resume, Some("pause-a"), 2);
    let acknowledged = store
        .advance_provider_environment_power(&resume, CONFIG, |_, r| Ok(ack(r)))
        .unwrap();
    let payload:String=db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested'",&[]).unwrap().get(0);
    db.execute(
        "UPDATE runtime.runtime_jobs SET run_at_ms=$2 WHERE job_id=$1",
        &[&job_id, &(now + 90_000)],
    )
    .unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .advance_provider_environment_power(&resume, CONFIG, |_, _| panic!(
                "historical ACK cannot dispatch/wake"
            ))
            .unwrap(),
        acknowledged
    );
    assert_eq!(
        db.query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&job_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        now + 90_000
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested'",&[]).unwrap().get::<_,i64>(0),1);
    assert_eq!(db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested'",&[]).unwrap().get::<_,String>(0),payload);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_forward_migration_preserves_retained_bindings_receipts_and_signatures() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let original:String=db.query_one("SELECT json_build_object('creation',(SELECT to_jsonb(c)-'creation_owner_json'-'maintenance_operation_id' FROM runtime.provider_creation_operations c),'binding',(SELECT row_to_json(b) FROM runtime.provider_environment_bindings b),'use',(SELECT row_to_json(u) FROM runtime.provider_environment_run_uses u),'authorization',(SELECT row_to_json(g) FROM public.app_core_agentrunauthorization g))::text",&[]).unwrap().get(0);
    crate::postgres_store::provider_operations::retained::power::schema::restore_previous_fixture(
        &mut db,
    );
    let previous: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(previous.len(), 19);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let current:String=db.query_one("SELECT json_build_object('creation',(SELECT to_jsonb(c)-'creation_owner_json'-'maintenance_operation_id' FROM runtime.provider_creation_operations c),'binding',(SELECT row_to_json(b) FROM runtime.provider_environment_bindings b),'use',(SELECT row_to_json(u) FROM runtime.provider_environment_run_uses u),'authorization',(SELECT row_to_json(g) FROM public.app_core_agentrunauthorization g))::text",&[]).unwrap().get(0);
    assert_eq!(current, original);
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=19 ORDER BY version",&[]).unwrap().into_iter().map(|row|(row.get(0),row.get(1))).collect::<Vec<(i64,i64)>>(),previous);
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        24
    );
    assert!(reopened
        .provider_environment_power_operation("pause-a")
        .unwrap()
        .is_none());
    assert_eq!(
        reopened
            .advance_provider_environment_power(
                &request("pause-a", Action::Pause, None, 2),
                CONFIG,
                |_, r| Ok(ack(r))
            )
            .unwrap()
            .operation_status,
        "acknowledged"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_schema_rejects_tampered_views_guards_and_foreign_keys() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for tamper in ["ALTER TABLE runtime.provider_environment_power_operations DISABLE TRIGGER provider_environment_power_facts",
        "ALTER TABLE runtime.provider_environment_power_operations DROP CONSTRAINT provider_environment_power_operations_environment_id_fkey",
        "CREATE OR REPLACE VIEW runtime.provider_environment_power_admission AS SELECT DISTINCT ON (p.environment_id) p.environment_id,false AS blocked FROM runtime.provider_environment_power_operations p ORDER BY p.environment_id,p.operation_sequence DESC"] {
        let(url,_,mut db)=retained::setup();db.batch_execute(tamper).unwrap();assert!(PostgresRuntimeStore::new(&url).is_err(),"tampered maintenance facts must fail before use");
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_database_enforces_immutable_identity_dispatch_and_ack() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-a", Action::Pause, None, 2);
    store
        .prepare_provider_environment_power(&req, CONFIG)
        .unwrap();
    for statement in ["UPDATE runtime.provider_environment_power_operations SET request_json='{}'", "DELETE FROM runtime.provider_environment_power_operations", "UPDATE runtime.provider_environment_power_operations SET operation_sequence=2", "UPDATE runtime.provider_environment_power_operations SET ack_json='{}',acknowledged_at_ms=recorded_at_ms"] {assert!(db.batch_execute(statement).is_err());}
    let permit = store
        .dispatch_provider_environment_power(&req, CONFIG)
        .unwrap()
        .unwrap();
    assert!(db.batch_execute("UPDATE runtime.provider_environment_power_operations SET dispatch_state='prepared',dispatched_at_ms=NULL").is_err());
    store
        .with_provider_environment_power_dispatch(permit, CONFIG, |_, r| Ok(ack(r)))
        .unwrap();
    assert!(db
        .batch_execute("UPDATE runtime.provider_environment_power_operations SET ack_json='{}'")
        .is_err());
    assert!(db
        .batch_execute("DELETE FROM runtime.provider_environment_power_operations")
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_observations_correlate_sequential_stages_without_changing_ack() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-observed", Action::Pause, None, 2);
    let mut calls = 0;
    let (result, events) = crate::observations::power::capture_power_observations(|| {
        store.advance_provider_environment_power(&req,CONFIG,|target,current| {
        calls+=1;assert_eq!(target,&use_a.target);assert_eq!(current,&req);
        assert!(db.query_one("SELECT dispatch_state='dispatched' AND ack_json IS NULL FROM runtime.provider_environment_power_operations WHERE operation_id=$1",&[&req.operation_id]).unwrap().get::<_,bool>(0));
        std::thread::sleep(std::time::Duration::from_millis(20));Ok(ack(current))
    })
    });
    let result = result.unwrap();
    assert_eq!(result.operation_status, "acknowledged");
    assert_eq!(calls, 1);
    let expected = [
        "storePrepareElapsedMs",
        "storeDispatchCasElapsedMs",
        "storeSideEffectPinsElapsedMs",
        "storeCallbackElapsedMs",
        "storeAckCommitElapsedMs",
        "storeAdvanceElapsedMs",
    ];
    assert_eq!(
        events
            .iter()
            .map(|event| event["metric"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    for event in &events {
        assert_eq!(event["schema"], "workspace.perf.v1");
        assert_eq!(event["source"], "runtime");
        assert_eq!(event["phase"], "providerPower");
        assert_eq!(event["operationId"], req.operation_id);
        assert_eq!(event["action"], "pause");
        let elapsed = event["value"].as_f64().expect("numeric monotonic duration");
        assert!(elapsed.is_finite() && elapsed >= 0.0);
        assert!(event.get("sandboxId").is_none());
        assert!(event.get("environmentId").is_none());
        assert!(event.get("providerConfigDigest").is_none());
    }
    let callback = events
        .iter()
        .find(|event| event["metric"] == "storeCallbackElapsedMs")
        .unwrap()["value"]
        .as_f64()
        .unwrap();
    let total = events
        .iter()
        .find(|event| event["metric"] == "storeAdvanceElapsedMs")
        .unwrap()["value"]
        .as_f64()
        .unwrap();
    assert!(callback >= 15.0);
    assert!(total >= callback);
    let (replayed, replay_events) = crate::observations::power::capture_power_observations(|| {
        store.advance_provider_environment_power(&req, CONFIG, |_, _| {
            panic!("telemetry cannot grant another dispatch")
        })
    });
    assert_eq!(replayed.unwrap(), result);
    assert_eq!(
        replay_events
            .iter()
            .map(|event| event["metric"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["storePrepareElapsedMs", "storeAdvanceElapsedMs"]
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_power_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_power_observations_keep_unknown_pending_and_do_not_redispatch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let req = request("pause-observed-unknown", Action::Pause, None, 2);
    let mut calls = 0;
    let (result, events) = crate::observations::power::capture_power_observations(|| {
        store.advance_provider_environment_power(&req, CONFIG, |_, _| {
            calls += 1;
            Err("token=private-error-with-body-and-path".into())
        })
    });
    assert_eq!(result.unwrap().operation_status, "pending");
    assert_eq!(calls, 1);
    for metric in ["storeCallbackElapsedMs", "storeAckCommitElapsedMs"] {
        let event = events
            .iter()
            .find(|event| event["metric"] == metric)
            .expect("pending phase remains measured");
        assert_eq!(event["outcome"], "pending");
    }
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("private-error"));
    assert!(!serialized.contains("token="));
    assert!(!serialized.contains("retained-sandbox"));
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let (result, replay) = crate::observations::power::capture_power_observations(|| {
        reopened.advance_provider_environment_power(&req, CONFIG, |_, _| {
            panic!("observations never reopen Unknown")
        })
    });
    assert_eq!(result.unwrap().operation_status, "pending");
    assert_eq!(
        replay
            .iter()
            .map(|event| event["metric"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["storePrepareElapsedMs", "storeAdvanceElapsedMs"]
    );
    assert!(db.query_one("SELECT dispatch_state='dispatched' AND ack_json IS NULL FROM runtime.provider_environment_power_operations",&[]).unwrap().get::<_,bool>(0));
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_capacity_only_pause_ack_releases_and_resume_intent_reserves() {
    use crate::postgres_store::provider_operations::capacity;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    assert_eq!(capacity::active_count(&mut db, CONFIG).unwrap(), 1);
    let pause = request("capacity-pause", Action::Pause, None, 2);
    let prepared = store
        .prepare_provider_environment_power(&pause, CONFIG)
        .unwrap();
    assert!(prepared.acknowledgement.is_none());
    assert_eq!(
        capacity::active_count(&mut db, CONFIG).unwrap(),
        1,
        "intent alone cannot refund physical capacity"
    );
    store
        .advance_provider_environment_power(&pause, CONFIG, |_, req| Ok(ack(req)))
        .unwrap();
    assert_eq!(capacity::active_count(&mut db, CONFIG).unwrap(), 0);
    let resume = request("capacity-resume", Action::Resume, Some("capacity-pause"), 2);
    store
        .prepare_provider_environment_power_limited(&resume, CONFIG, Some(1))
        .unwrap();
    assert_eq!(
        capacity::active_count(&mut db, CONFIG).unwrap(),
        1,
        "resume is charged before SDK dispatch"
    );
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        capacity::require(&mut tx, Some(1), CONFIG).unwrap_err(),
        capacity::WAIT
    );
}

fn rotation_waiter(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    source: &Run,
    name: &str,
) -> (String, String) {
    let (job, owner) = waiting_consumer(store, db, source, name, "environment");
    let now = crate::now_ms().unwrap();
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: job.clone(),
            lease_owner: owner.clone(),
            yielded_at_ms: now,
            run_at_ms: now + 30_000,
            transition_reason: "environment_power_pending".into(),
        })
        .unwrap();
    (job, owner)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_current_demand_excludes_accepted_cancellation_before_resume() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    let (job, _) = rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    assert_eq!(page.candidates.len(), 1);
    assert!(page.candidates[0].has_demand);
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    store
        .request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let fresh = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    assert!(!fresh.candidates[0].has_demand);
    let mut sends = 0;
    assert_eq!(
        store
            .advance_provider_environment_power_guarded(&resume, CONFIG, Some(1), true, |_, r| {
                sends += 1;
                Ok(ack(r))
            })
            .unwrap_err(),
        "provider_environment_resume_demand_absent"
    );
    assert_eq!(sends, 0);
    assert!(store
        .provider_environment_power_operation(&resume.operation_id)
        .unwrap()
        .is_none());
    assert_eq!(
        db.query_one(
            "SELECT status FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&job]
        )
        .unwrap()
        .get::<_, String>(0),
        "queued"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_cancelled_prepared_resume_records_not_dispatched_without_ack() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    store
        .prepare_provider_environment_power_guarded(&resume, CONFIG, Some(1), true)
        .unwrap();
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    store
        .request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let mut sends = 0;
    let result = store
        .advance_provider_environment_power_guarded(&resume, CONFIG, Some(1), true, |_, r| {
            sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(result.operation_status, "notDispatched");
    assert_eq!(sends, 0);
    let operation = store
        .provider_environment_power_operation(&resume.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(operation.dispatch_state, Dispatch::NotDispatched);
    assert!(operation.acknowledgement.is_none());
    assert!(operation.not_dispatched_at_ms.is_some());
    assert_eq!(
        operation.automatic_demand.as_ref().unwrap().job_id,
        "agent_run.lifecycle:rotation-waiter"
    );
    assert_eq!(
        crate::postgres_store::provider_operations::capacity::active_count(&mut db, CONFIG)
            .unwrap(),
        0,
        "the explicit known-unsent terminal fact preserves the physical pause ACK"
    );
}

fn capacity_creation_waiter(store: &PostgresRuntimeStore, db: &mut Client, source: &Run) -> Run {
    let (job_id, owner) =
        waiting_consumer(store, db, source, "capacity-waiter", "capacity-environment");
    db.execute("INSERT INTO public.app_core_sessionenvironment(id,session_id,controller_run_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile,environment_profile_digest) VALUES('capacity-environment','sess_1','capacity-waiter',1,$1,'execute',false,$2::text::jsonb,$3)",&[&owner,&serde_json::to_string(&source.original.environment_profile).unwrap(),&source.original.environment_profile_digest]).unwrap();
    db.execute("UPDATE public.app_core_session SET current_environment_id='capacity-environment' WHERE id='sess_1'",&[]).unwrap();
    let original:WorkspaceAgentRunAuthorization=serde_json::from_str(&db.query_one("SELECT payload::text FROM public.app_core_agentrunauthorization WHERE agent_run_id='capacity-waiter'",&[]).unwrap().get::<_,String>(0)).unwrap();
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(owner.as_bytes());
    let grant = retained::sign(crate::environment_authorization::EnvironmentBinding {
        schema: "workspace.environment_authorization.v1".into(),
        environment_id: "capacity-environment".into(),
        session_id: "sess_1".into(),
        agent_run_id: original.agent_run_id.clone(),
        agent_run_authorization_digest: original.digest().unwrap(),
        execution_mode: "retained".into(),
        controller: Some(crate::environment_authorization::ControllerBinding {
            controller_id: original.agent_run_id.clone(),
            controller_epoch: 1,
            controller_lease_digest: format!("sha256:{:x}", hash.finalize()),
            controller_purpose: "execute".into(),
        }),
    });
    let fence = RuntimeJobLeaseFence {
        job_id: job_id.clone(),
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner.clone(),
    };
    let auth = ControllerAuthorityRequest {
        environment_authorization: &grant,
        signing_key: KEY,
        workspace_id: &original.workspace_id,
        lease: &fence,
        purpose: ControllerPurpose::Execute,
    };
    store
        .prepare_provider_creation(
            &auth,
            &ProviderCreationRequest {
                execution_id: "capacity-execution",
                attempt_id: "capacity-create",
                idempotency_key: "capacity-create-key",
                request_digest: REQUEST,
                configured_provider_config_digest: CONFIG,
            },
        )
        .unwrap();
    Run {
        original,
        grant,
        fence,
    }
}

fn capacity_release_race(release_before_yield: bool) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let waiting = capacity_creation_waiter(&store, &mut db, &a);
    let (job, owner) = (
        waiting.fence.job_id.clone(),
        waiting.fence.lease_owner.clone(),
    );
    let (question, question_owner) = waiting_consumer(
        &store,
        &mut db,
        &a,
        "question-waiter",
        "capacity-environment",
    );
    let now = crate::now_ms().unwrap();
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: question.clone(),
            lease_owner: question_owner,
            yielded_at_ms: now,
            run_at_ms: now + 30_000,
            transition_reason: "question_wait".into(),
        })
        .unwrap();
    let yield_capacity = |at| YieldRuntimeJobRequest {
        job_id: job.clone(),
        lease_owner: owner.clone(),
        yielded_at_ms: at,
        run_at_ms: at + 30_000,
        transition_reason: "e2b_environment_capacity_wait".into(),
    };
    if !release_before_yield {
        store.yield_runtime_job(yield_capacity(now)).unwrap();
    }
    store
        .advance_provider_environment_power(
            &request("capacity-release", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    if release_before_yield {
        store
            .yield_runtime_job(yield_capacity(crate::now_ms().unwrap()))
            .unwrap();
    }
    let due: i64 = db
        .query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&job],
        )
        .unwrap()
        .get(0);
    assert!(
        due < now + 30_000,
        "pause ACK must wake before the fallback deadline in either yield order"
    );
    assert_eq!(
        db.query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&question]
        )
        .unwrap()
        .get::<_, i64>(0),
        now + 30_000,
        "capacity release must not wake question waits"
    );
    let pending:i64=db.query_one("SELECT count(*) FROM runtime.runtime_events wake WHERE wake.task_id=$1 AND wake.event_type='runtime_job_wake_requested' AND NOT EXISTS(SELECT 1 FROM runtime.runtime_events consumed WHERE consumed.event_id=replace(wake.event_id,'runtime_job_wake:','runtime_job_wake_consumed:'))",&[&job]).unwrap().get(0);
    if release_before_yield {
        assert_eq!(
            pending, 0,
            "yield consumes the pre-existing wake so it cannot leak into a later wait"
        );
    }
}
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_pause_ack_wakes_capacity_wait_after_yield() {
    capacity_release_race(false);
}
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_pause_ack_before_yield_is_consumed_without_polling_delay() {
    capacity_release_race(true);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_cancelled_unsent_resume_does_not_block_other_environment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    store
        .prepare_provider_environment_power_guarded(&resume, CONFIG, Some(1), true)
        .unwrap();
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    store
        .request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let mut sends = 0;
    store
        .advance_provider_environment_power_guarded(&resume, CONFIG, Some(1), true, |_, r| {
            sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(
        sends, 0,
        "cancellation precedes any send permit or SDK callback"
    );
    let other = capacity_creation_waiter(&store, &mut db, &a);
    let creation = store
        .provider_creation_operation("capacity-create")
        .unwrap()
        .unwrap();
    assert!(store.dispatch_provider_creation_limited(&other.authority(ControllerPurpose::Execute),&creation.identity,Some(1)).is_ok(),
        "an explicitly unsent cancelled resume must not permanently consume another Environment's only running slot");
}
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_capacity_wake_after_successful_create_does_not_wake_later_question() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let other = capacity_creation_waiter(&store, &mut db, &a);
    let pause = request("capacity-release", Action::Pause, None, 2);
    store
        .advance_provider_environment_power(&pause, CONFIG, |_, r| Ok(ack(r)))
        .unwrap();
    let at:i64=db.query_one("SELECT acknowledged_at_ms FROM runtime.provider_environment_power_operations WHERE operation_id=$1",&[&pause.operation_id]).unwrap().get(0);
    let event = format!(
        "runtime_job_wake:{}:provider_capacity:{}",
        other.fence.job_id, pause.operation_id
    );
    let payload=serde_json::json!({"schema":"runtime.provider_environment.power.wake.v1","jobId":other.fence.job_id,"environmentId":"capacity-environment","sourceOperationId":pause.operation_id,"wokenAtMs":at,"transitionReason":"capacity_released"}).to_string();
    // Canonical producer input for the consumer boundary: the real pause ACK
    // above already owns the source fact. No physical acknowledgement is forged.
    db.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,'sess_1',$2,'runtime_job_wake_requested',$3,'internal',$4) ON CONFLICT(event_id) DO NOTHING",&[&event,&other.fence.job_id,&at,&payload]).unwrap();
    let creation = store
        .provider_creation_operation("capacity-create")
        .unwrap()
        .unwrap();
    store
        .dispatch_provider_creation_limited(
            &other.authority(ControllerPurpose::Execute),
            &creation.identity,
            Some(1),
        )
        .unwrap();
    let now = crate::now_ms().unwrap();
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: other.fence.job_id.clone(),
            lease_owner: other.fence.lease_owner.clone(),
            yielded_at_ms: now,
            run_at_ms: now + 30_000,
            transition_reason: "question_wait".into(),
        })
        .unwrap();
    let due: i64 = db
        .query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&other.fence.job_id],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        due,
        now + 30_000,
        "a pre-create capacity wake must not apply to a later question wait"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.runtime_events WHERE event_id=$1",
            &[&event.replacen("runtime_job_wake:", "runtime_job_wake_consumed:", 1)]
        )
        .unwrap()
        .get::<_, i64>(0),
        1,
        "the stale capacity wake is consumed exactly once"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_new_demand_resumes_after_cancelled_unsent_intent() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let cancelled = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    store
        .prepare_provider_environment_power_guarded(&cancelled, CONFIG, Some(1), true)
        .unwrap();
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    store
        .request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let mut cancelled_sends = 0;
    let ended = store
        .advance_provider_environment_power_guarded(&cancelled, CONFIG, Some(1), true, |_, r| {
            cancelled_sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(ended.operation_status, "notDispatched");
    assert_eq!(cancelled_sends, 0);
    rotation_waiter(&store, &mut db, &a, "fresh-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    assert_eq!(
        resume.predecessor_operation_id.as_deref(),
        Some(cancelled.operation_id.as_str())
    );
    let mut sends = 0;
    let acknowledged = store
        .advance_provider_environment_power_guarded(&resume, CONFIG, Some(1), true, |_, r| {
            sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(acknowledged.operation_status, "acknowledged");
    assert_eq!(sends, 1);
    let bound = store
        .provider_environment_power_operation(&resume.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        bound.automatic_demand.as_ref().unwrap().job_id,
        "agent_run.lifecycle:fresh-waiter"
    );
    let replay = store
        .advance_provider_environment_power_guarded(&cancelled, CONFIG, Some(1), false, |_, r| {
            cancelled_sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(replay.operation_status, "notDispatched");
    assert_eq!(
        cancelled_sends, 0,
        "the cancelled origin Job never creates a later send permit"
    );
    assert_eq!(
        crate::postgres_store::provider_operations::capacity::active_count(&mut db, CONFIG)
            .unwrap(),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_cancel_after_dispatch_keeps_unknown_responsibility() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    store
        .prepare_provider_environment_power_guarded(&resume, CONFIG, Some(1), true)
        .unwrap();
    let permit = store
        .dispatch_provider_environment_power_guarded(&resume, CONFIG, true)
        .unwrap()
        .unwrap();
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    store
        .request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let mut sends = 0;
    let pending = store
        .with_provider_environment_power_dispatch_guarded(permit, CONFIG, true, |_, r| {
            sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(pending.operation_status, "pending");
    assert_eq!(sends, 0);
    let current = store
        .provider_environment_power_operation(&resume.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.dispatch_state, Dispatch::Dispatched);
    assert!(current.not_dispatched_at_ms.is_none());
    assert!(current.acknowledgement.is_none());
    assert_eq!(
        crate::postgres_store::provider_operations::capacity::active_count(&mut db, CONFIG)
            .unwrap(),
        1
    );
    assert!(db.execute("UPDATE runtime.provider_environment_power_operations SET dispatch_state='notDispatched',dispatched_at_ms=NULL,not_dispatched_at_ms=recorded_at_ms WHERE operation_id=$1",&[&resume.operation_id]).is_err(),"the database cannot convert committed dispatch responsibility into an unsent fact");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_forward_migration_preserves_old_ack_and_unknown() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("old-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    store
        .advance_provider_environment_power(
            &request("old-unknown-resume", Action::Resume, Some("old-pause"), 2),
            CONFIG,
            |_, _| Err("lost SDK reply".into()),
        )
        .unwrap();
    let before:String=db.query_one("SELECT jsonb_agg(to_jsonb(p)-'automatic_demand_json'-'not_dispatched_at_ms' ORDER BY operation_sequence)::text FROM runtime.provider_environment_power_operations p",&[]).unwrap().get(0);
    let versions:Vec<(i64,i64)>=db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=23 ORDER BY version",&[]).unwrap().into_iter().map(|row|(row.get(0),row.get(1))).collect();
    crate::postgres_store::provider_operations::retained::power::automatic_resume::restore_previous_fixture(&mut db);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let after:String=db.query_one("SELECT jsonb_agg(to_jsonb(p)-'automatic_demand_json'-'not_dispatched_at_ms' ORDER BY operation_sequence)::text FROM runtime.provider_environment_power_operations p",&[]).unwrap().get(0);
    assert_eq!(
        before, after,
        "forward migration must preserve physical history byte for byte"
    );
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=23 ORDER BY version",&[]).unwrap().into_iter().map(|row|(row.get(0),row.get(1))).collect::<Vec<(i64,i64)>>(),versions);
    let acknowledged = reopened
        .provider_environment_power_operation("old-pause")
        .unwrap()
        .unwrap();
    assert!(acknowledged.acknowledgement.is_some());
    assert!(acknowledged.automatic_demand.is_none());
    let unknown = reopened
        .provider_environment_power_operation("old-unknown-resume")
        .unwrap()
        .unwrap();
    assert_eq!(unknown.dispatch_state, Dispatch::Dispatched);
    assert!(unknown.acknowledgement.is_none());
    assert!(unknown.not_dispatched_at_ms.is_none());
    assert_eq!(
        crate::postgres_store::provider_operations::capacity::active_count(&mut db, CONFIG)
            .unwrap(),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_cancel_acceptance_waits_for_sdk_dispatch_job_pin() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    store
        .prepare_provider_environment_power_guarded(&resume, CONFIG, Some(1), true)
        .unwrap();
    let permit = store
        .dispatch_provider_environment_power_guarded(&resume, CONFIG, true)
        .unwrap()
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let dispatch_store = store.clone();
    let dispatch = std::thread::spawn(move || {
        dispatch_store.with_provider_environment_power_dispatch_guarded(
            permit,
            CONFIG,
            true,
            move |_, r| {
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                Ok(ack(r))
            },
        )
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let blocker:i32=db.query_one("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND state='idle in transaction' AND query LIKE '%AND j.job_id=$2 AND g.digest=$3%'",&[]).unwrap().get(0);
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    let cancel_store = store.clone();
    let cancellation = std::thread::spawn(move || {
        cancel_store.request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut blocked = false;
    while std::time::Instant::now() < deadline {
        blocked = db
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
                &[&blocker],
            )
            .unwrap()
            .get(0);
        if blocked {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    release_tx.send(()).unwrap();
    assert_eq!(
        dispatch.join().unwrap().unwrap().operation_status,
        "acknowledged"
    );
    assert!(cancellation.join().unwrap().unwrap());
    assert!(blocked,"database lock evidence must show cancellation acceptance ordered after the SDK dispatch pin");
    let mut sends = 0;
    store
        .advance_provider_environment_power_guarded(&resume, CONFIG, Some(1), true, |_, r| {
            sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(
        sends, 0,
        "accepted cancellation cannot dispatch an already-acknowledged operation again"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_rotation_unsent_termination_wakes_existing_capacity_wait() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    store
        .advance_provider_environment_power(
            &request("rotation-pause", Action::Pause, None, 2),
            CONFIG,
            |_, r| Ok(ack(r)),
        )
        .unwrap();
    rotation_waiter(&store, &mut db, &a, "rotation-waiter");
    let page = store
        .provider_rotation_candidates(CONFIG, &a.original.environment_profile_digest, None)
        .unwrap();
    let resume = crate::postgres_store::provider_operations::retained::idle::request(
        &page.candidates[0],
        CONFIG,
        Action::Resume,
    );
    store
        .prepare_provider_environment_power_guarded(&resume, CONFIG, Some(1), true)
        .unwrap();
    let other = capacity_creation_waiter(&store, &mut db, &a);
    let now = crate::now_ms().unwrap();
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: other.fence.job_id.clone(),
            lease_owner: other.fence.lease_owner.clone(),
            yielded_at_ms: now,
            run_at_ms: now + 30_000,
            transition_reason: "e2b_environment_capacity_wait".into(),
        })
        .unwrap();
    let digest:String=db.query_one("SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='rotation-waiter'",&[]).unwrap().get(0);
    store
        .request_agent_run_cancellation(
            "rotation-waiter",
            "sess_1",
            &digest,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let mut sends = 0;
    let ended = store
        .advance_provider_environment_power_guarded(&resume, CONFIG, Some(1), true, |_, r| {
            sends += 1;
            Ok(ack(r))
        })
        .unwrap();
    assert_eq!(ended.operation_status, "notDispatched");
    assert_eq!(sends, 0);
    let due: i64 = db
        .query_one(
            "SELECT run_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&other.fence.job_id],
        )
        .unwrap()
        .get(0);
    assert!(
        due < now + 30_000,
        "a definite unsent termination releases logical capacity and wakes the same parked Job"
    );
    let payload:String=db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime_job_wake_requested'",&[&other.fence.job_id]).unwrap().get(0);
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["transitionReason"], "capacity_released");
    assert_eq!(payload["environmentId"], "capacity-environment");
    assert_eq!(
        payload["sourceOperationId"], resume.operation_id,
        "the source is the explicit unsent terminal fact, never a fabricated new pause ACK"
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_environment_observation_preserves_verified_target_without_native_allocation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    db.batch_execute("ALTER TABLE public.app_core_sessionenvironment ADD COLUMN IF NOT EXISTS controller_maintenance_operation_id text").unwrap();

    let (a, _, use_a) = candidate(&store, &mut db);
    let value = store
        .with_provider_environment_observation("sess_1", "environment", |observed| {
            assert_eq!(observed.environment_id, "environment");
            assert_eq!(observed.session_id, "sess_1");
            assert_eq!(observed.profile, a.original.environment_profile);
            assert_eq!(
                observed.profile_digest,
                a.original.environment_profile_digest
            );
            assert_eq!(observed.provider_target.as_ref(), Some(&use_a.target));
            assert!(!observed.removed);
            Ok(serde_json::to_value(&observed.profile).unwrap())
        })
        .unwrap();
    assert_eq!(
        value,
        serde_json::to_value(&a.original.environment_profile).unwrap()
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
