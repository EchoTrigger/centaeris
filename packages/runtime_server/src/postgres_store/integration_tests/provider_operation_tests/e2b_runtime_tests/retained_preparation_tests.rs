use super::*;
use crate::e2b_adapter::{AdapterFailure, AdapterResponse};
use crate::e2b_creation::{E2bCreationAdapter, E2bCreationDriver, E2bCreationRequest};
use serde_json::json;

mod creator_cleanup_tests;

fn retained_run(store: &PostgresRuntimeStore, db: &mut Client) -> Run {
    db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL); DELETE FROM public.app_core_agentrunenvironmentrelease;").unwrap();
    let mut profile = factory_profile();
    let EnvironmentProfile::E2b(value) = &mut profile else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    let mut run = run_with_profile(store, db, profile);
    run.grant.payload.execution_mode = "retained".into();
    run.grant = retained::sign(run.grant.payload.clone());
    run
}

struct VerifiedCreate;
impl E2bCreationAdapter for VerifiedCreate {
    fn create(
        &self,
        request_id: &str,
        payload: Value,
        _: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        Ok(
            serde_json::from_value(json!({"schema":"runtime.e2b.adapter.response.v1",
            "requestId":request_id,"outcome":"succeeded","result":{
                "attemptId":payload["attemptId"],"sandboxId":"factory-sandbox",
                "templateId":payload["templateRef"]["templateId"],
                "metadata":payload["metadata"],"state":"running"}}))
            .unwrap(),
        )
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_preparation_original_creator_without_use_never_claims_live_workspace() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = retained_run(&store, &mut db);
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "retainedReady",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    let execution = "retained-original-without-use";
    let key = runtime
        .attempt_key(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            execution,
        )
        .unwrap();
    let operation = E2bCreationDriver::new(&store, &VerifiedCreate)
        .create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &E2bCreationRequest {
                authorization: &run.original,
                execution_id: execution,
                attempt_id: &key.attempt_id,
                idempotency_key: &key.idempotency_key,
                configured_provider_config_digest: CONFIG,
                transport_timeout: Duration::from_secs(10),
            },
        )
        .unwrap();
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("factory-sandbox")
    );
    assert!(db.query_one("SELECT verified_sandbox_id IS NOT NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&key.attempt_id]).unwrap().get::<_,bool>(0));
    assert!(
        store
            .retained_provider_run_use(&run.original.agent_run_id)
            .unwrap()
            .is_none(),
        "the fault boundary is after verified creation and before original consumer binding"
    );

    let outcome = runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            execution,
        )
        .unwrap();
    assert!(matches!(outcome, E2bPrepareOutcome::Pending { .. }),
        "an original creator with no workspace preparation evidence cannot become retained-live after retry");
    assert_eq!(
        launcher.creates(),
        0,
        "loaded creation must not dispatch again"
    );
    assert!(
        launcher
            .requests()
            .iter()
            .all(|request| request["operation"] != "attach"),
        "first creation is not evidence of a previously usable retained workspace"
    );
    assert!(store
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .is_none());
    // The exact physical target was verified before the initial use could be
    // recorded. Terminal cleanup still needs positive, scoped evidence.
    super::super::retained_provider_tests::terminal(&mut db, &mut run);
    let receipt = runtime
        .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
        .unwrap();
    let crate::postgres_store::provider_operations::cleanup::ProviderRunCleanupReceipt::RetainedProvider(receipt) = receipt else {
        panic!("verified creator recovery must preserve the retained environment");
    };
    assert_eq!(receipt.scope, "runEffectsOnly");
    assert_eq!(launcher.creates(), 0);
    assert!(launcher.requests().iter().all(|request| {
        matches!(
            request["operation"].as_str(),
            Some("capabilities" | "invokeControlHelper")
        )
    }));
    assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&key.attempt_id]).unwrap().get::<_,bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_preparation_fresh_creation_has_exact_run_use_and_requires_initial_restore()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = retained_run(&store, &mut db);
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "retainedReady",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    let host = match runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            "retained-fresh-a",
        )
        .unwrap()
    {
        E2bPrepareOutcome::Prepared(host) => host,
        E2bPrepareOutcome::Pending { .. } => {
            panic!("fresh verified creation must preserve its original SDK context")
        }
    };
    let use_ = store
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .unwrap();
    assert_eq!(host.retained_use(), Some(&use_));
    assert_eq!(use_.agent_run_id, run.original.agent_run_id);
    assert_eq!(use_.authorization_digest, run.original.digest().unwrap());
    assert_eq!(use_.execution_id, "retained-fresh-a");
    assert_eq!(host.logical_execution_id(), "retained-fresh-a");
    assert_eq!(host.identity(), &use_.target.identity);
    assert!(
        !host.workspace_reused_live(),
        "fresh creation must restore its frozen Session archive"
    );
    host.runner().prepare_workspace().unwrap();
    assert_eq!(launcher.creates(), 1);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert!(launcher
        .requests()
        .iter()
        .all(|request| request["operation"] != "attach"));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_preparation_successor_preserves_target_with_independent_authority_in_memory_and_after_restart(
) {
    use super::super::retained_provider_tests::{
        terminal,
        uses::{release, successor},
    };
    use crate::postgres_store::provider_operations::cleanup::ProviderRunCleanupReceipt;

    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for restart in [false, true] {
        let (url, store, mut db) = retained::setup();
        let mut a = retained_run(&store, &mut db);
        let root = tempfile::tempdir().unwrap();
        let launcher = Arc::new(FixtureLauncher::new(
            "retainedReady",
            root.path().join("requests.jsonl"),
        ));
        let runtime = E2bRuntime::with_launcher(
            store.clone(),
            config(a.original.environment_profile.clone()),
            launcher.clone(),
        );
        let a_host = match runtime
            .prepare(
                &a.authority(ControllerPurpose::Execute),
                &a.original,
                "retained-runtime-a",
            )
            .unwrap()
        {
            E2bPrepareOutcome::Prepared(host) => host,
            E2bPrepareOutcome::Pending { .. } => panic!("fresh A must be owned"),
        };
        a_host.runner().prepare_workspace().unwrap();
        let original_creation: String = db
            .query_one(
                "SELECT row_to_json(p)::text FROM runtime.provider_creation_operations p",
                &[],
            )
            .unwrap()
            .get(0);
        let original_use = a_host.retained_use().unwrap().clone();
        terminal(&mut db, &mut a);
        let a_receipt = runtime
            .cleanup_retained(&a.authority(ControllerPurpose::Cleanup), Some(&a_host))
            .unwrap();
        assert!(matches!(
            &a_receipt,
            ProviderRunCleanupReceipt::RetainedProvider(_)
        ));
        assert_eq!(
            runtime
                .cleanup_retained(&a.authority(ControllerPurpose::Cleanup), None)
                .unwrap(),
            a_receipt,
            "a durable receipt replays without the original SDK context"
        );
        release(&mut db, &a);
        db.execute("UPDATE runtime.runtime_jobs SET status='completed',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=$1", &[&a.fence.job_id]).unwrap();
        let mut b = successor(&store, &mut db, &a);
        let restarted;
        let selected = if restart {
            restarted = E2bRuntime::with_launcher(
                PostgresRuntimeStore::new(&url).unwrap(),
                config(b.original.environment_profile.clone()),
                launcher.clone(),
            );
            &restarted
        } else {
            &runtime
        };
        let b_host = match selected
            .prepare_with_environment(
                &b.authority(ControllerPurpose::Execute),
                &b.original,
                "retained-runtime-b",
                if restart { None } else { Some(a_host.as_ref()) },
            )
            .unwrap()
        {
            E2bPrepareOutcome::Prepared(host) => host,
            E2bPrepareOutcome::Pending { .. } => {
                panic!("closed A permits a separately authorized B, restart={restart}")
            }
        };
        assert!(b_host.workspace_reused_live());
        assert_eq!(b_host.logical_execution_id(), "retained-runtime-b");
        assert_eq!(b_host.identity(), a_host.identity());
        assert_eq!(b_host.binding(), a_host.binding());
        let b_use = b_host.retained_use().unwrap();
        assert_eq!(b_use.agent_run_id, b.original.agent_run_id);
        assert_eq!(b_use.execution_id, "retained-runtime-b");
        assert_ne!(b_use.provider_use_id, original_use.provider_use_id);
        assert_ne!(
            b_use.authorization_digest,
            original_use.authorization_digest
        );
        assert!(
            matches!(
                a_host.runner().prepare_workspace(),
                Err(E2bHostError::NotDispatched(_))
            ),
            "the old A handle never acquires B's authority"
        );
        b_host.runner().prepare_workspace().unwrap();
        assert_eq!(
            db.query_one(
                "SELECT row_to_json(p)::text FROM runtime.provider_creation_operations p",
                &[]
            )
            .unwrap()
            .get::<_, String>(0),
            original_creation,
            "successor authority cannot rewrite immutable creation provenance"
        );
        assert_eq!(launcher.creates(), 1);
        assert_eq!(
            launcher
                .requests()
                .iter()
                .filter(|request| request["operation"] == "attach")
                .count(),
            usize::from(restart)
        );
        assert_eq!(
            launcher.launches.load(Ordering::SeqCst),
            if restart { 4 } else { 2 }
        );

        terminal(&mut db, &mut b);
        let b_receipt = selected
            .cleanup_retained(&b.authority(ControllerPurpose::Cleanup), Some(&b_host))
            .unwrap();
        assert!(matches!(
            &b_receipt,
            ProviderRunCleanupReceipt::RetainedProvider(_)
        ));
        assert_ne!(b_receipt.receipt_id(), a_receipt.receipt_id());
        release(&mut db, &b);
        assert!(
            db.query_one(
                "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
                &[]
            )
            .unwrap()
            .get::<_, bool>(0),
            "both Run cleanups preserve the retained physical environment"
        );
        assert!(launcher.requests().iter().all(|request| !matches!(
            request["operation"].as_str(),
            Some("kill" | "pause" | "resume")
        )));
    }
}
