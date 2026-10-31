use super::*;
use crate::agent_run_authorization::SessionWorkspace;
use crate::postgres_store::cleanup_preservation::{CleanupRecovery, CleanupRecoveryEvidence};
use crate::postgres_store::cleanup_receipts::CleanupPhysicalResult;

fn preservation_fixture(
    db: &mut Client,
    cleanup: &AgentRunCleanup,
    allocation: &RetainedInstanceAllocation,
) -> CleanupRecovery {
    db.batch_execute("ALTER TABLE public.app_core_session ADD COLUMN \"workspaceBaselineKnown\" boolean NOT NULL DEFAULT true, ADD COLUMN \"workspaceGeneration\" bigint NOT NULL DEFAULT 0, ADD COLUMN \"workspaceStorageKey\" text NOT NULL DEFAULT '', ADD COLUMN \"workspaceSnapshotSha256\" text NOT NULL DEFAULT '', ADD COLUMN \"workspaceSnapshotEncoding\" text NOT NULL DEFAULT 'frameV1', ADD COLUMN \"workspaceSnapshotSizeBytes\" bigint NOT NULL DEFAULT 0, ADD COLUMN \"workspaceExpandedSizeBytes\" bigint NOT NULL DEFAULT 0, ADD COLUMN \"workspaceFileCount\" integer NOT NULL DEFAULT 0;
    DROP TABLE IF EXISTS public.app_core_sessionworkspacepreservation;
    CREATE TABLE public.app_core_sessionworkspacepreservation(id text PRIMARY KEY,session_id text,agent_run_id text,origin_agent_run_id text,environment_id text,authorization_digest text,origin_authorization_digest text,allocation_execution_id text,instance_id text,container_id text,profile_digest text,baseline jsonb,workspace jsonb,snapshot_encoding text,storage_key text,creator_lease_digest text,creator_controller_epoch bigint);").unwrap();
    let workspace = SessionWorkspace {
        generation: cleanup.authorization.session_workspace.generation + 1,
        snapshot_sha256: format!("sha256:{}", "e".repeat(64)),
        snapshot_size_bytes: 30,
        expanded_size_bytes: 10,
        file_count: 1,
    };
    let key = format!(
        "workspaces/{}/sessions/{}/snapshots/{}/{}.snapshot",
        cleanup.authorization.workspace_id,
        cleanup.authorization.session_id,
        workspace.generation,
        workspace.snapshot_sha256.strip_prefix("sha256:").unwrap()
    );
    db.execute("UPDATE public.app_core_session SET \"workspaceGeneration\"=$1,\"workspaceStorageKey\"=$2,\"workspaceSnapshotSha256\"=$3,\"workspaceSnapshotSizeBytes\"=30,\"workspaceExpandedSizeBytes\"=10,\"workspaceFileCount\"=1",&[&(workspace.generation as i64),&key,&workspace.snapshot_sha256]).unwrap();
    let receipt = format!("sha256:{}", "f".repeat(64));
    db.execute("INSERT INTO public.app_core_sessionworkspacepreservation VALUES($1,'sess_1',$2,$3,'environment',$4,$5,$6,$7,$8,$9,$10::text::jsonb,$11::text::jsonb,'frameV1',$12,'creator-old-lease',1)",&[&receipt,&cleanup.agent_run_id,&allocation.allocation_agent_run_id,&cleanup.authorization_digest,&allocation.authorization_digest,&allocation.allocation_execution_id,&allocation.instance_id,&allocation.container_id,&allocation.profile_digest,&serde_json::to_string(&cleanup.authorization.session_workspace).unwrap(),&serde_json::to_string(&workspace).unwrap(),&key]).unwrap();
    CleanupRecovery {
        expected_allocation: allocation.clone(),
        evidence: CleanupRecoveryEvidence::Preserved {
            receipt_id: receipt,
            workspace,
        },
    }
}
fn per_run(store: &PostgresRuntimeStore, db: &mut Client, id: &str) -> Run {
    let mut run = run(store, db, id, 1);
    run.grant.payload.execution_mode = "perRun".into();
    run.grant = sign(run.grant.payload);
    run
}
fn canonical_container(
    db: &mut Client,
    physical: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
    allocation: &mut RetainedInstanceAllocation,
) {
    let id = "a".repeat(64);
    db.execute(
        "UPDATE runtime.resident_sandboxes SET container_id=$1",
        &[&id],
    )
    .unwrap();
    physical.lock().unwrap()[0].container_id = id.clone();
    allocation.container_id = Some(id);
}
fn assert_responsibility(db: &mut Client, physical: &Arc<Mutex<Vec<ObservedResidentSandbox>>>) {
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_recovery_cleanup_requires_exact_durable_preservation_before_any_physical_effect() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let mut run = per_run(&store, &mut db, "proof-guarded-cleanup");
    let physical = Arc::new(Mutex::new(vec![]));
    let mut allocation = prepare(&store, &run, "proof-allocation", &physical).unwrap();
    canonical_container(&mut db, &physical, &mut allocation);
    let cleanup = terminal(&mut db, &mut run, false);
    let recovery = preservation_fixture(&mut db, &cleanup, &allocation);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &run.fence,
        host_id: "host",
    };
    for field in [
        "agent_run_id",
        "origin_agent_run_id",
        "environment_id",
        "authorization_digest",
        "origin_authorization_digest",
        "allocation_execution_id",
        "instance_id",
        "container_id",
        "profile_digest",
        "storage_key",
        "snapshot_encoding",
    ] {
        let original: String = db
            .query_one(
                &format!("SELECT {field} FROM public.app_core_sessionworkspacepreservation"),
                &[],
            )
            .unwrap()
            .get(0);
        db.execute(
            &format!("UPDATE public.app_core_sessionworkspacepreservation SET {field}='foreign'"),
            &[],
        )
        .unwrap();
        let called = AtomicBool::new(false);
        assert!(
            store
                .with_recovery_cleanup_authority(
                    &request,
                    || Ok(physical.lock().unwrap().clone()),
                    &recovery,
                    |_, _| {
                        called.store(true, Ordering::SeqCst);
                        Ok(CleanupPhysicalResult::Retained)
                    }
                )
                .is_err(),
            "receipt {field}"
        );
        assert!(!called.load(Ordering::SeqCst), "receipt {field}");
        assert_responsibility(&mut db, &physical);
        db.execute(
            &format!("UPDATE public.app_core_sessionworkspacepreservation SET {field}=$1"),
            &[&original],
        )
        .unwrap();
    }
    let mut missing = recovery.clone();
    if let CleanupRecoveryEvidence::Preserved { receipt_id, .. } = &mut missing.evidence {
        *receipt_id = format!("sha256:{}", "0".repeat(64));
    }
    assert!(store
        .with_recovery_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            &missing,
            |_, _| -> Result<CleanupPhysicalResult, String> {
                panic!("missing receipt cannot remove")
            }
        )
        .is_err());
    assert_responsibility(&mut db, &physical);
    let mut foreign = recovery.clone();
    foreign.expected_allocation.instance_id = "foreign".into();
    assert!(store
        .with_recovery_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            &foreign,
            |_, _| -> Result<CleanupPhysicalResult, String> {
                panic!("foreign target cannot remove")
            }
        )
        .is_err());
    assert_responsibility(&mut db, &physical);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_recovery_cleanup_preserved_original_survives_lost_ack_and_new_legitimate_cleanup_owner()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let origin = run(&store, &mut db, "preserved-origin", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let mut allocation = prepare(&store, &origin, "preserved-allocation", &physical).unwrap();
    canonical_container(&mut db, &physical, &mut allocation);
    let mut run = run(&store, &mut db, "preserved-replay", 2);
    run.grant.payload.execution_mode = "perRun".into();
    run.grant = sign(run.grant.payload);
    let mut cleanup = terminal(&mut db, &mut run, true);
    let recovery = preservation_fixture(&mut db, &cleanup, &allocation);
    let first = store.with_recovery_cleanup_authority(
        &CleanupAuthorityRequest {
            cleanup: &cleanup,
            signing_key: KEY,
            lease: &run.fence,
            host_id: "host",
        },
        || Ok(physical.lock().unwrap().clone()),
        &recovery,
        |_, _| -> Result<CleanupPhysicalResult, String> {
            physical.lock().unwrap().clear();
            Err("physical observation unknown".into())
        },
    );
    assert!(first.is_err());
    assert!(physical.lock().unwrap().is_empty());
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    run.fence.lease_owner = "fresh-legitimate-cleanup-owner".into();
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch = 3;
    controller.controller_lease_digest = controller_lease_digest(&run.fence.lease_owner);
    run.grant = sign(run.grant.payload);
    cleanup.environment_authorization = run.grant.clone();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=3,controller_lease_owner=$1",&[&run.fence.lease_owner]).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner=$1 WHERE job_id=$2",
        &[&run.fence.lease_owner, &run.fence.job_id],
    )
    .unwrap();
    let later_key = format!(
        "workspaces/ws_1/sessions/sess_1/snapshots/9/{}.snapshot",
        "e".repeat(64)
    );
    db.execute(
        "UPDATE public.app_core_session SET \"workspaceGeneration\"=9,\"workspaceStorageKey\"=$1",
        &[&later_key],
    )
    .unwrap();
    let receipt = store
        .with_recovery_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &run.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            &recovery,
            |target, _| {
                let CleanupTarget::Instance { allocation, .. } = target else {
                    panic!("exact original target")
                };
                assert!(
                    physical.lock().unwrap().is_empty(),
                    "exact original is independently absent"
                );
                Ok(CleanupPhysicalResult::Removed {
                    instance_id: allocation.instance_id.clone(),
                    container_id: allocation.container_id.clone().unwrap(),
                })
            },
        )
        .unwrap();
    assert!(receipt.cleanup_complete);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(db.query_one("SELECT creator_lease_digest,creator_controller_epoch FROM public.app_core_sessionworkspacepreservation",&[]).unwrap().get::<_,String>(0),"creator-old-lease");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_recovery_cleanup_rejects_unknown_or_behind_heads_and_stale_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let mut run = per_run(&store, &mut db, "preserved-head");
    let physical = Arc::new(Mutex::new(vec![]));
    let mut allocation = prepare(&store, &run, "head-allocation", &physical).unwrap();
    canonical_container(&mut db, &physical, &mut allocation);
    let cleanup = terminal(&mut db, &mut run, false);
    let recovery = preservation_fixture(&mut db, &cleanup, &allocation);
    for update in [
        "\"workspaceBaselineKnown\"=false",
        "\"workspaceGeneration\"=7",
        "\"workspaceSnapshotSha256\"='sha256:0000000000000000000000000000000000000000000000000000000000000000'",
    ] {
        db.batch_execute(&format!("UPDATE public.app_core_session SET {update}"))
            .unwrap();
        let called = AtomicBool::new(false);
        assert!(
            store
                .with_recovery_cleanup_authority(
                    &CleanupAuthorityRequest {
                        cleanup: &cleanup,
                        signing_key: KEY,
                        lease: &run.fence,
                        host_id: "host"
                    },
                    || Ok(physical.lock().unwrap().clone()),
                    &recovery,
                    |_, _| {
                        called.store(true, Ordering::SeqCst);
                        Ok(CleanupPhysicalResult::Retained)
                    }
                )
                .is_err()
        );
        assert!(!called.load(Ordering::SeqCst));
        assert_responsibility(&mut db, &physical);
        db.batch_execute("UPDATE public.app_core_session SET \"workspaceBaselineKnown\"=true,\"workspaceGeneration\"=8,\"workspaceSnapshotSha256\"='sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'").unwrap();
    }
    let mut stale = run.fence.clone();
    stale.lease_owner = "stale-owner".into();
    assert!(store
        .with_recovery_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &stale,
                host_id: "host"
            },
            || Ok(physical.lock().unwrap().clone()),
            &recovery,
            |_, _| -> Result<CleanupPhysicalResult, String> { panic!("stale lease cannot remove") }
        )
        .is_err());
    assert_responsibility(&mut db, &physical);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_recovery_cleanup_absence_requires_physical_removed_without_fabricating_a_snapshot() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let mut run = per_run(&store, &mut db, "absent-recovery");
    let physical = Arc::new(Mutex::new(vec![]));
    let mut allocation = prepare(&store, &run, "absent-allocation", &physical).unwrap();
    canonical_container(&mut db, &physical, &mut allocation);
    let cleanup = terminal(&mut db, &mut run, false);
    let recovery = CleanupRecovery {
        expected_allocation: allocation,
        evidence: CleanupRecoveryEvidence::Absent,
    };
    assert!(store
        .with_recovery_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &run.fence,
                host_id: "host"
            },
            || Ok(physical.lock().unwrap().clone()),
            &recovery,
            |_, _| Ok(CleanupPhysicalResult::Retained)
        )
        .is_err());
    assert_responsibility(&mut db, &physical);
    physical.lock().unwrap().clear();
    store
        .with_recovery_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &run.fence,
                host_id: "host",
            },
            || Ok(vec![]),
            &recovery,
            |target, _| {
                let CleanupTarget::Instance { allocation, .. } = target else {
                    panic!("exact original target")
                };
                Ok(CleanupPhysicalResult::Removed {
                    instance_id: allocation.instance_id.clone(),
                    container_id: allocation.container_id.clone().unwrap(),
                })
            },
        )
        .unwrap();
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(db.query_opt("SELECT 1 FROM information_schema.columns WHERE table_name='app_core_session' AND column_name='workspaceBaselineKnown'",&[]).unwrap().is_none(),"absence cleanup must not fabricate snapshot state");
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_recovery_cleanup_binds_manifest_encoding_and_preserves_advanced_head_identity() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let mut run = per_run(&store, &mut db, "manifest-preservation");
    let physical = Arc::new(Mutex::new(vec![]));
    let mut allocation = prepare(&store, &run, "manifest-allocation", &physical).unwrap();
    canonical_container(&mut db, &physical, &mut allocation);
    let cleanup = terminal(&mut db, &mut run, false);
    let recovery = preservation_fixture(&mut db, &cleanup, &allocation);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &run.fence,
        host_id: "host",
    };
    // Both old committed frames and new manifests reach the physical boundary
    // under current authority, without rewriting their recorded identity.
    for encoding in ["frameV1", "fileManifestV1"] {
        db.execute(
            "UPDATE public.app_core_sessionworkspacepreservation SET snapshot_encoding=$1",
            &[&encoding],
        )
        .unwrap();
        db.execute(
            "UPDATE public.app_core_session SET \"workspaceSnapshotEncoding\"=$1",
            &[&encoding],
        )
        .unwrap();
        let called = AtomicBool::new(false);
        let result = store.with_recovery_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            &recovery,
            |_, _| -> Result<CleanupPhysicalResult, String> {
                called.store(true, Ordering::SeqCst);
                Err("physical callback intentionally not dispatched".into())
            },
        );
        assert!(result.is_err());
        assert!(
            called.load(Ordering::SeqCst),
            "valid receipt encoding {encoding}"
        );
        assert_responsibility(&mut db, &physical);
    }
    db.batch_execute("UPDATE public.app_core_session SET \"workspaceSnapshotEncoding\"='frameV1'")
        .unwrap();
    let called = AtomicBool::new(false);
    assert!(store
        .with_recovery_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            &recovery,
            |_, _| -> Result<CleanupPhysicalResult, String> {
                called.store(true, Ordering::SeqCst);
                Err("encoding mismatch must not dispatch".into())
            },
        )
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
    assert_responsibility(&mut db, &physical);
    let CleanupRecoveryEvidence::Preserved { workspace, .. } = &recovery.evidence else {
        unreachable!()
    };
    let generation = workspace.generation + 1;
    let hash = format!("sha256:{}", "d".repeat(64));
    let key = format!(
        "workspaces/ws_1/sessions/sess_1/snapshots/{generation}/{}.snapshot",
        "d".repeat(64)
    );
    db.execute("UPDATE public.app_core_session SET \"workspaceGeneration\"=$1,\"workspaceSnapshotSha256\"=$2,\"workspaceStorageKey\"=$3,\"workspaceSnapshotEncoding\"='tarZstdV1'", &[&(generation as i64), &hash, &key]).unwrap();
    let receipt = store
        .with_recovery_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            &recovery,
            |target, _| {
                let CleanupTarget::Instance { allocation, .. } = target else {
                    panic!("exact original target")
                };
                physical.lock().unwrap().clear();
                Ok(CleanupPhysicalResult::Removed {
                    instance_id: allocation.instance_id.clone(),
                    container_id: allocation.container_id.clone().unwrap(),
                })
            },
        )
        .unwrap();
    assert!(receipt.cleanup_complete);
    let current = db.query_one("SELECT \"workspaceSnapshotSha256\",\"workspaceSnapshotEncoding\" FROM public.app_core_session", &[]).unwrap();
    assert_eq!(current.get::<_, String>(0), hash);
    assert_eq!(current.get::<_, String>(1), "tarZstdV1");
    assert_eq!(
        db.query_one(
            "SELECT snapshot_encoding FROM public.app_core_sessionworkspacepreservation",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "fileManifestV1"
    );
}
