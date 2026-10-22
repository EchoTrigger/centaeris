use super::retained_instance_tests::{self as retained, KEY};
use super::*;
use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;
use crate::environment_authorization::{
    ControllerBinding, EnvironmentAuthorization, EnvironmentBinding,
};
use crate::postgres_store::controller_authority::{ControllerAuthorityRequest, ControllerPurpose};
use crate::postgres_store::provider_operations::{
    ProviderCreationDispatchState, ProviderCreationRequest,
};
use sha2::{Digest, Sha256};

const CONFIG: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const REQUEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

mod creation_driver_tests;

struct Run {
    original: WorkspaceAgentRunAuthorization,
    grant: EnvironmentAuthorization,
    fence: RuntimeJobLeaseFence,
}

impl Run {
    fn authority(&self, purpose: ControllerPurpose) -> ControllerAuthorityRequest<'_> {
        ControllerAuthorityRequest {
            environment_authorization: &self.grant,
            signing_key: KEY,
            workspace_id: &self.original.workspace_id,
            lease: &self.fence,
            purpose,
        }
    }
}

fn run(store: &PostgresRuntimeStore, db: &mut Client) -> Run {
    let mut original: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    original.environment_profile = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap();
    original.environment_profile_digest = original.environment_profile.digest().unwrap();
    original.agent_run_id = "provider-run".into();
    original.id = "provider-authorization".into();
    let digest = original.digest().unwrap();
    let owner = "worker:provider-creation";
    let mut lease_digest = Sha256::new();
    lease_digest.update(b"workspace:environment-controller-lease:v1\0");
    lease_digest.update(owner.as_bytes());
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2,controller_run_id=$3,controller_epoch=1,controller_lease_owner=$4,controller_purpose='execute',controller_released=false WHERE id='environment'",
        &[&serde_json::to_string(&original.environment_profile).unwrap(),&original.environment_profile_digest,&original.agent_run_id,&owner]).unwrap();
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,'sess_1','environment','ws_1','user_1','member','running','model_1','high')", &[&original.agent_run_id]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &original.agent_run_id,
            &digest,
            &serde_json::to_string(&original).unwrap(),
            &original.signature(KEY).unwrap(),
        ],
    )
    .unwrap();
    let job_id = format!("agent_run.lifecycle:{}", original.agent_run_id);
    let mut record = job(&job_id, &format!("{job_id}:{digest}"));
    record.job_kind = "agent_run.lifecycle".into();
    record.status = RuntimeJobStatus::Running;
    record.lease_owner = Some(owner.into());
    record.lease_expires_at_ms = Some(4_000_000_000_000);
    record.session_id = Some(original.session_id.clone());
    record.payload_ref = Some(format!("record:agent_run:{}", original.agent_run_id));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some("ws_1"))
        .unwrap();
    let grant = retained::sign(EnvironmentBinding {
        schema: "workspace.environment_authorization.v1".into(),
        environment_id: "environment".into(),
        session_id: original.session_id.clone(),
        agent_run_id: original.agent_run_id.clone(),
        agent_run_authorization_digest: digest,
        execution_mode: "perRun".into(),
        controller: Some(ControllerBinding {
            controller_id: original.agent_run_id.clone(),
            controller_epoch: 1,
            controller_lease_digest: format!("sha256:{:x}", lease_digest.finalize()),
            controller_purpose: "execute".into(),
        }),
    });
    Run {
        original,
        grant,
        fence: RuntimeJobLeaseFence {
            job_id,
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner.into(),
        },
    }
}

fn request<'a>(execution: &'a str, attempt: &'a str, key: &'a str) -> ProviderCreationRequest<'a> {
    ProviderCreationRequest {
        execution_id: execution,
        attempt_id: attempt,
        idempotency_key: key,
        request_digest: REQUEST,
        configured_provider_config_digest: CONFIG,
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_creation_persists_exact_identity_and_processing_duty_before_dispatch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    assert_eq!(
        operation.dispatch_state,
        ProviderCreationDispatchState::Prepared
    );
    assert_eq!(operation.identity.environment_id, "environment");
    assert_eq!(
        operation.identity.profile_digest,
        run.original.environment_profile_digest
    );
    assert_eq!(operation.identity.provider_config_digest, CONFIG);
    assert_eq!(operation.identity.processing_job_id, run.fence.job_id);
    assert_eq!(
        operation.identity.instance_id,
        crate::postgres_store::execution_instances::instance_id("execution-provider")
    );
    assert!(operation.reported_sandbox_id.is_none());
    assert_eq!(
        store
            .prepare_provider_creation(
                &run.authority(ControllerPurpose::Execute),
                &request("execution-provider", "attempt-provider", "key-provider")
            )
            .unwrap(),
        operation
    );
    assert_eq!(
        PostgresRuntimeStore::new(&url)
            .unwrap()
            .provider_creation_operation("attempt-provider")
            .unwrap(),
        Some(operation)
    );
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.resident_sandboxes)+(SELECT count(*) FROM runtime.execution_instances)",&[]).unwrap().get::<_,i64>(0),0);
    db.batch_execute("UPDATE runtime.runtime_jobs SET status='dead_lettered',lease_owner=NULL,lease_expires_at_ms=NULL").unwrap();
    assert!(
        db.execute(
            "DELETE FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&run.fence.job_id]
        )
        .is_err(),
        "processing duty must survive retry exhaustion"
    );
    assert!(store
        .provider_creation_operation("attempt-provider")
        .unwrap()
        .is_some());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_dispatched_unknown_cannot_close_redispatch_or_change_attempt() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let prepared = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    let dispatched = store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity,
        )
        .unwrap()
        .consume();
    assert_eq!(
        dispatched.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert!(dispatched.reported_sandbox_id.is_none());
    assert!(store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity
        )
        .is_err());
    assert!(store
        .close_provider_creation_before_dispatch(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity
        )
        .is_err());
    assert!(store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request(
                "execution-replacement",
                "attempt-replacement",
                "key-replacement"
            )
        )
        .is_err());
    assert_eq!(
        PostgresRuntimeStore::new(&url)
            .unwrap()
            .provider_creation_operation("attempt-provider")
            .unwrap(),
        Some(dispatched)
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_not_dispatched_proof_is_replayable_but_never_dispatchable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let prepared = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    let closed = store
        .close_provider_creation_before_dispatch(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity,
        )
        .unwrap();
    assert_eq!(
        closed.dispatch_state,
        ProviderCreationDispatchState::NotDispatched
    );
    assert_eq!(
        store
            .close_provider_creation_before_dispatch(
                &run.authority(ControllerPurpose::Execute),
                &prepared.identity
            )
            .unwrap(),
        closed
    );
    assert!(store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity
        )
        .is_err());
    assert!(store
        .record_provider_creation_observation(&prepared.identity, "sandbox-late")
        .is_err());
    assert!(store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-next", "attempt-next", "key-next")
        )
        .is_ok());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_dispatch_requires_current_controller_and_execute_job_lease() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = run(&store, &mut db);
    let prepared = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2")
        .unwrap();
    assert!(store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity
        )
        .is_err());
    run.grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_epoch = 2;
    run.grant = retained::sign(run.grant.payload);
    let owner = "worker:provider-control";
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1,lease_purpose='control',lease_control_stage='advance'",&[&owner]).unwrap();
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_lease_owner=$1",
        &[&owner],
    )
    .unwrap();
    run.fence.lease_owner = owner.into();
    run.grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_lease_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            [
                b"workspace:environment-controller-lease:v1\0".as_slice(),
                owner.as_bytes()
            ]
            .concat()
        )
    );
    run.grant = retained::sign(run.grant.payload);
    assert!(store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity
        )
        .is_err());
    assert_eq!(
        store
            .provider_creation_operation("attempt-provider")
            .unwrap()
            .unwrap()
            .dispatch_state,
        ProviderCreationDispatchState::Prepared
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_late_candidate_identity_preserves_evidence_without_restoring_old_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
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
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2; UPDATE runtime.runtime_jobs SET lease_owner='worker:next-owner'").unwrap();
    let observed = store
        .record_provider_creation_observation(&operation.identity, "sandbox-opaque_not-docker-hex")
        .unwrap();
    assert_eq!(
        observed.reported_sandbox_id.as_deref(),
        Some("sandbox-opaque_not-docker-hex")
    );
    assert_eq!(
        observed.dispatch_state,
        ProviderCreationDispatchState::Dispatched,
        "reported identity is not Present/Ready settlement"
    );
    assert_eq!(
        store
            .record_provider_creation_observation(
                &operation.identity,
                "sandbox-opaque_not-docker-hex"
            )
            .unwrap(),
        observed
    );
    assert!(store
        .record_provider_creation_observation(&operation.identity, "sandbox-different")
        .is_err());
    assert!(store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity
        )
        .is_err());
    assert!(store
        .close_provider_creation_before_dispatch(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity
        )
        .is_err());
    assert_eq!(
        PostgresRuntimeStore::new(&url)
            .unwrap()
            .provider_creation_operation("attempt-provider")
            .unwrap(),
        Some(observed)
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_observation_requires_all_original_identity_fields() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
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
    for field in [
        "attempt",
        "execution",
        "instance",
        "environment",
        "session",
        "workspace",
        "run",
        "authorization",
        "profile",
        "config",
        "request",
        "key",
        "job",
    ] {
        let mut changed = operation.identity.clone();
        let value = match field {
            "attempt" => &mut changed.attempt_id,
            "execution" => &mut changed.execution_id,
            "instance" => &mut changed.instance_id,
            "environment" => &mut changed.environment_id,
            "session" => &mut changed.session_id,
            "workspace" => &mut changed.workspace_id,
            "run" => &mut changed.agent_run_id,
            "authorization" => &mut changed.authorization_digest,
            "profile" => &mut changed.profile_digest,
            "config" => &mut changed.provider_config_digest,
            "request" => &mut changed.request_digest,
            "key" => &mut changed.idempotency_key,
            "job" => &mut changed.processing_job_id,
            _ => unreachable!(),
        };
        value.push('x');
        assert!(
            store
                .record_provider_creation_observation(&changed, "sandbox-opaque")
                .is_err(),
            "{field}"
        );
    }
    assert!(store
        .provider_creation_operation("attempt-provider")
        .unwrap()
        .unwrap()
        .reported_sandbox_id
        .is_none());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_idempotency_key_cannot_be_rebound_to_a_different_attempt_or_request() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    store
        .close_provider_creation_before_dispatch(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let mut changed = request("execution-next", "attempt-next", "key-provider");
    changed.request_digest =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    assert!(store
        .prepare_provider_creation(&run.authority(ControllerPurpose::Execute), &changed)
        .is_err());
    let mut configured = request("execution-next", "attempt-next", "key-next");
    configured.configured_provider_config_digest =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    assert!(store
        .prepare_provider_creation(&run.authority(ControllerPurpose::Execute), &configured)
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_creation_schema_upgrade_preserves_legacy_grants_and_job_history() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let legacy = retained::run(&store, &mut db, "legacy-preserved", 1);
    let prior:Vec<(i64,i64)>=db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=16 ORDER BY version",&[]).unwrap().iter().map(|row|(row.get(0),row.get(1))).collect();
    let snapshot=db.query_one("SELECT payload::text,signature,(SELECT row_to_json(j)::text FROM runtime.runtime_jobs j WHERE j.job_id=$2) FROM public.app_core_agentrunauthorization WHERE agent_run_id=$1",&[&legacy.original.agent_run_id,&legacy.fence.job_id]).unwrap();
    let facts: (String, String, String) = (snapshot.get(0), snapshot.get(1), snapshot.get(2));
    if db
        .query_one(
            "SELECT to_regclass('runtime.provider_creation_operations') IS NOT NULL",
            &[],
        )
        .unwrap()
        .get::<_, bool>(0)
    {
        db.batch_execute("DROP TABLE runtime.provider_creation_operations; DROP FUNCTION runtime.protect_provider_creation_operation(); DELETE FROM runtime.schema_migrations WHERE version=17;").unwrap();
    }
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        17
    );
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=16 ORDER BY version",&[]).unwrap().iter().map(|row|(row.get(0),row.get(1))).collect::<Vec<(i64,i64)>>(),prior);
    let after=db.query_one("SELECT payload::text,signature,(SELECT row_to_json(j)::text FROM runtime.runtime_jobs j WHERE j.job_id=$2) FROM public.app_core_agentrunauthorization WHERE agent_run_id=$1",&[&legacy.original.agent_run_id,&legacy.fence.job_id]).unwrap();
    assert_eq!(
        (
            after.get::<_, String>(0),
            after.get::<_, String>(1),
            after.get::<_, String>(2)
        ),
        facts
    );
    assert!(reopened
        .provider_creation_operation("unallocated")
        .unwrap()
        .is_none());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_creation_current_cleanup_authority_can_close_only_the_unsent_attempt() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_purpose='cleanup'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','provider-run',false,'{\"type\":\"agent_run_interrupted\"}'); UPDATE runtime.runtime_jobs SET lease_owner=NULL;").unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1,lease_purpose='control',lease_control_stage='cleanup'",&[&run.fence.lease_owner]).unwrap();
    run.grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_epoch = 2;
    run.grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_purpose = "cleanup".into();
    run.grant = retained::sign(run.grant.payload);
    assert!(store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity
        )
        .is_err());
    assert_eq!(
        store
            .close_provider_creation_before_dispatch(
                &run.authority(ControllerPurpose::Cleanup),
                &operation.identity
            )
            .unwrap()
            .dispatch_state,
        ProviderCreationDispatchState::NotDispatched
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_creation_database_preserves_identity_state_and_report_evidence() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-provider", "attempt-provider", "key-provider"),
        )
        .unwrap();
    for change in [
        "UPDATE runtime.provider_creation_operations SET request_digest='changed'",
        "UPDATE runtime.provider_creation_operations SET processing_job_id=processing_job_id",
        "UPDATE runtime.provider_creation_operations SET reported_sandbox_id='premature'",
        "DELETE FROM runtime.provider_creation_operations",
    ] {
        assert!(db.execute(change, &[]).is_err(), "{change}");
        assert_eq!(
            store
                .provider_creation_operation("attempt-provider")
                .unwrap(),
            Some(operation.clone())
        );
    }
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let reported = store
        .record_provider_creation_observation(&operation.identity, "opaque-sandbox")
        .unwrap();
    for change in [
        "UPDATE runtime.provider_creation_operations SET dispatch_state='notDispatched'",
        "UPDATE runtime.provider_creation_operations SET reported_sandbox_id=NULL",
        "UPDATE runtime.provider_creation_operations SET reported_sandbox_id='replacement'",
    ] {
        assert!(db.execute(change, &[]).is_err(), "{change}");
    }
    assert_eq!(
        PostgresRuntimeStore::new(&url)
            .unwrap()
            .provider_creation_operation("attempt-provider")
            .unwrap(),
        Some(reported)
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_creation_reopen_rejects_missing_identity_and_transition_guards() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for change in ["ALTER TABLE runtime.provider_creation_operations ALTER COLUMN request_digest DROP NOT NULL","ALTER TABLE runtime.provider_creation_operations DROP CONSTRAINT provider_creation_operations_provider_config_key","DROP INDEX runtime.provider_creation_operations_open_key","ALTER TABLE runtime.provider_creation_operations DISABLE TRIGGER provider_creation_operation_facts","CREATE OR REPLACE FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$"] {
        let (url,_,mut db)=retained::setup();
        db.batch_execute(change).unwrap();
        let before:Vec<(i64,i64)>=db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",&[]).unwrap().iter().map(|row|(row.get(0),row.get(1))).collect();
        assert!(PostgresRuntimeStore::new(&url).is_err(),"{change}");
        assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",&[]).unwrap().iter().map(|row|(row.get(0),row.get(1))).collect::<Vec<(i64,i64)>>(),before);
    }
}
