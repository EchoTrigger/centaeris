use super::*;
use crate::e2b_run_registry::E2bLiveContext;

fn prepare_requests(launcher: &FixtureLauncher) -> usize {
    launcher
        .requests()
        .iter()
        .filter(|request| {
            request["operation"] == "invokeControlHelper"
                && request["payload"]["helper"] == "prepare-workspace"
        })
        .count()
}

fn prepared_host(
    runtime: &E2bRuntime,
    run: &Run,
    execution: &str,
) -> Arc<crate::e2b_runtime::E2bPreparedHost> {
    match runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            execution,
        )
        .unwrap()
    {
        E2bPrepareOutcome::Prepared(host) => host,
        E2bPrepareOutcome::Pending { .. } => panic!("fixture must create one verified live host"),
    }
}

fn reclaim(db: &mut Client, run: &mut Run) {
    let owner = "worker:product-preparation-reclaimed";
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner=NULL WHERE job_id=$1",
        &[&run.fence.job_id],
    )
    .unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner=$1,lease_expires_at_ms=4000000000000 WHERE job_id=$2",
        &[&owner, &run.fence.job_id],
    )
    .unwrap();
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_lease_owner=$1 WHERE id=$2",
        &[&owner, &run.grant.payload.environment_id],
    )
    .unwrap();
    run.fence.lease_owner = owner.into();
    let mut lease_digest = Sha256::new();
    lease_digest.update(b"workspace:environment-controller-lease:v1\0");
    lease_digest.update(owner.as_bytes());
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch = 2;
    controller.controller_lease_digest = format!("sha256:{:x}", lease_digest.finalize());
    run.grant = retained::sign(run.grant.payload.clone());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_product_preparation_retries_only_unprepared_live_context_after_reclaim() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = run_with_profile(&store, &mut db, factory_profile());
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "productReady",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    let execution = "execution-product-preparation";
    let host = prepared_host(&runtime, &run, execution);
    let mut live = Some(E2bLiveContext::new(host));
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1 WHERE job_id=$1",
        &[&run.fence.job_id],
    )
    .unwrap();
    assert!(crate::prepare_e2b_live_host(
        &runtime,
        &mut live,
        &run.authority(ControllerPurpose::Execute),
        &run.original,
        execution,
    )
    .is_err());
    assert!(!live.as_ref().unwrap().workspace_prepared);
    assert_eq!(
        prepare_requests(&launcher),
        0,
        "expired owner cannot start preparation"
    );

    reclaim(&mut db, &mut run);
    let runner = crate::prepare_e2b_live_host(
        &runtime,
        &mut live,
        &run.authority(ControllerPurpose::Execute),
        &run.original,
        execution,
    )
    .expect("a new valid owner must prepare the preserved live host");
    assert_eq!(runner.binding().sandbox_id, "factory-sandbox");
    assert_eq!(runner.execution_id(), execution);
    assert_eq!(
        prepare_requests(&launcher),
        1,
        "live context alone is not a workspace preparation receipt"
    );
    assert!(live.as_ref().unwrap().workspace_prepared);
    let repeated = crate::prepare_e2b_live_host(
        &runtime,
        &mut live,
        &run.authority(ControllerPurpose::Execute),
        &run.original,
        execution,
    )
    .unwrap();
    assert_eq!(repeated.binding().sandbox_id, "factory-sandbox");
    assert_eq!(repeated.execution_id(), execution);
    assert_eq!(
        prepare_requests(&launcher),
        1,
        "confirmed preparation must not repeat"
    );
    assert_eq!(launcher.creates(), 1);
    assert_eq!(
        launcher.launches.load(Ordering::SeqCst),
        2,
        "reclaim must preserve the original SDK contexts"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_product_preparation_unknown_never_promotes_or_redispatches_live_context() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run_with_profile(&store, &mut db, factory_profile());
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "success",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    let execution = "execution-product-preparation-unknown";
    let host = prepared_host(&runtime, &run, execution);
    let original_runner = host.runner();
    let mut live = Some(E2bLiveContext::new(host));
    for _ in 0..2 {
        assert!(
            crate::prepare_e2b_live_host(
                &runtime,
                &mut live,
                &run.authority(ControllerPurpose::Execute),
                &run.original,
                execution,
            )
            .is_err(),
            "an unknown helper result cannot admit ordinary Core execution"
        );
        let retained = live
            .as_ref()
            .expect("uncertain live responsibility must remain owned");
        assert!(!retained.workspace_prepared);
        assert_eq!(retained.host.binding().sandbox_id, "factory-sandbox");
        assert_eq!(retained.host.identity().execution_id, execution);
        assert!(!original_runner.helper_effects_known());
    }
    assert_eq!(
        prepare_requests(&launcher),
        1,
        "unknown preparation must not be replayed"
    );
    assert_eq!(launcher.creates(), 1);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
}
