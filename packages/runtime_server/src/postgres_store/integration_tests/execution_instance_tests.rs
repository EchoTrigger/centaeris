use super::{reset_store, test_url, PostgresRuntimeStore, TEST_LOCK};
use postgres::{Client, NoTls};

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_execution_instance_binding_is_durable_and_rejects_reassignment() {
    use super::retained_instance_tests::{self as fixture, Run};
    use std::sync::{Arc, Mutex};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let a = fixture::run(&store, &mut db, "instance-run", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let first = fixture::prepare(&store, &a, "execution-one", &physical).unwrap();
    assert_ne!(first.instance_id, "execution-one");
    let original = db.query_one("SELECT row_to_json(i)::text FROM runtime.execution_instances i WHERE execution_id='execution-one'", &[]).unwrap().get::<_,String>(0);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let replay = fixture::prepare(&reopened, &a, "execution-one", &physical).unwrap();
    assert_eq!(replay.instance_id, first.instance_id);
    assert!(replay.reused);
    let profile = a.original.environment_profile.digest().unwrap();
    for wrong in ["host", "image"] {
        let mut request = a.request("not-created", &profile);
        if wrong == "host" {
            request.host_id = "foreign-host";
        } else {
            request.image_digest =
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        }
        assert!(reopened
            .prepare_retained_instance(
                &request,
                fixture::limits(&a),
                || Ok(physical.lock().unwrap().clone()),
                |_, _| panic!("reassignment reached Core acceptance"),
                |_| panic!("reassignment reached physical create")
            )
            .is_err());
    }
    for field in [
        "environmentId",
        "sessionId",
        "agentRunId",
        "agentRunAuthorizationDigest",
    ] {
        let mut value = serde_json::to_value(&a.grant.payload).unwrap();
        value[field] = if field == "agentRunAuthorizationDigest" {
            format!("sha256:{}", "b".repeat(64))
        } else {
            "foreign".into()
        }
        .into();
        if field == "agentRunId" {
            value["controllerId"] = "foreign".into();
        }
        let grant = fixture::sign(serde_json::from_value(value).unwrap());
        let foreign = Run {
            original: a.original.clone(),
            grant,
            fence: a.fence.clone(),
        };
        assert!(
            fixture::prepare(&reopened, &foreign, "not-created", &physical).is_err(),
            "{field}"
        );
    }
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let store = reopened.clone();
            let run = Run {
                original: a.original.clone(),
                grant: a.grant.clone(),
                fence: a.fence.clone(),
            };
            let physical = physical.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                (0..4)
                    .map(|attempt| {
                        barrier.wait();
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(5);
                        loop {
                            let result = fixture::prepare(
                                &store,
                                &run,
                                &format!("concurrent-{attempt}"),
                                &physical,
                            );
                            if result.as_ref().is_err_and(|e| {
                                e == crate::resident_capacity::RESIDENT_CAPACITY_WAIT
                                    || e == crate::resident_capacity::HOST_PREPARATION_WAIT
                            }) && std::time::Instant::now() < deadline
                            {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                                continue;
                            }
                            break result.map(|a| a.instance_id);
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(results.iter().flatten().all(Result::is_ok), "{results:?}");
    assert_eq!(results[0], results[1]);
    assert!(results[0]
        .iter()
        .all(|r| r.as_ref().unwrap() == &first.instance_id));
    assert_eq!(physical.lock().unwrap().len(), 1);
    db.batch_execute("UPDATE public.app_core_agentrun SET status='completed'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let b = fixture::run(&reopened, &mut db, "next-run", 2);
    assert!(
        fixture::prepare(&reopened, &b, "execution-one", &physical).is_err(),
        "an execution fact cannot be reassigned to a later Run"
    );
    let next = fixture::prepare(&reopened, &b, "next-run-execution", &physical).unwrap();
    assert_eq!(
        next.instance_id, first.instance_id,
        "new Run reuses the common physical instance through a new association"
    );
    assert_eq!(db.query_one("SELECT row_to_json(i)::text FROM runtime.execution_instances i WHERE execution_id='execution-one'", &[]).unwrap().get::<_,String>(0), original);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_instance_forward_migration_preserves_reservations_and_rejects_bad_constraints() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let _store = PostgresRuntimeStore::new(&url).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::retained_instance_tests::downgrade_to_eight(&mut db);
    db.batch_execute("INSERT INTO runtime.resident_sandboxes VALUES('host','execution','run','tenant',NULL,'reserved',1,1,1,1,1,1);
        DROP TABLE runtime.execution_instances; DELETE FROM runtime.schema_migrations WHERE version=8;").unwrap();
    let history = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1)))
        .collect::<Vec<_>>();
    let _upgraded = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        db.query_one(
            "SELECT state FROM runtime.resident_sandboxes WHERE execution_id='execution'",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "reserved"
    );
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=7 ORDER BY version", &[]).unwrap().into_iter().map(|r| (r.get::<_, i64>(0),r.get::<_, i64>(1))).collect::<Vec<_>>(),history);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    db.batch_execute("DROP INDEX runtime.execution_instances_anchor_instance_key")
        .unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("allocation uniqueness index missing"));
}
