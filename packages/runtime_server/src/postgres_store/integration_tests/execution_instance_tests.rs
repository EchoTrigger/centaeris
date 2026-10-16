use super::{reset_store, test_url, PostgresRuntimeStore, TEST_LOCK};
use crate::environment_authorization::EnvironmentAuthorization;
use postgres::{Client, NoTls};

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_execution_instance_binding_is_durable_and_rejects_reassignment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let grant: EnvironmentAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/environment_authorization/v1/valid.json"
    ))
    .unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute("ALTER TABLE public.app_core_agentrun ADD COLUMN IF NOT EXISTS environment_id text;
        INSERT INTO public.app_core_agentrun(id,session_id,environment_id) VALUES('agent_run_1','sess_1','environment_1');
        CREATE TABLE IF NOT EXISTS public.app_core_agentrunauthorization(agent_run_id text PRIMARY KEY,digest text NOT NULL);").unwrap();
    db.execute("INSERT INTO public.app_core_agentrunauthorization(agent_run_id,digest) VALUES('agent_run_1',$1) ON CONFLICT(agent_run_id) DO UPDATE SET digest=EXCLUDED.digest", &[&grant.payload.agent_run_authorization_digest]).unwrap();
    let image = format!("sha256:{}", "a".repeat(64));
    let bind = |client: &mut Client,
                host: &str,
                binding: &crate::environment_authorization::EnvironmentBinding| {
        super::super::execution_instances::bind(client, binding, "execution-one", host, &image)
    };
    let id = bind(&mut db, "host-one", &grant.payload).unwrap();
    assert_ne!(id, "execution-one");
    let mut observer = Client::connect(&url, NoTls).unwrap();
    assert_eq!(observer.query_one("SELECT instance_id FROM runtime.execution_instances WHERE execution_id='execution-one'", &[]).unwrap().get::<_,String>(0), id);
    drop(store);
    let _reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(bind(&mut db, "host-one", &grant.payload).unwrap(), id);
    assert!(bind(&mut db, "host-two", &grant.payload).is_err());
    assert!(super::super::execution_instances::bind(
        &mut db,
        &grant.payload,
        "execution-one",
        "host-one",
        &format!("sha256:{}", "b".repeat(64))
    )
    .is_err());
    let mut foreign = grant.payload.clone();
    foreign.environment_id = "foreign".into();
    assert!(bind(&mut db, "host-one", &foreign).is_err());
    for field in ["sessionId", "agentRunId", "agentRunAuthorizationDigest"] {
        let mut changed = serde_json::to_value(&grant.payload).unwrap();
        changed[field] = if field == "agentRunAuthorizationDigest" {
            format!("sha256:{}", "b".repeat(64))
        } else {
            "foreign".into()
        }
        .into();
        let changed = serde_json::from_value(changed).unwrap();
        assert!(
            super::super::execution_instances::bind(
                &mut db,
                &changed,
                "not-created",
                "host-one",
                &image
            )
            .is_err(),
            "{field}"
        );
    }
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let second = super::super::execution_instances::bind(
        &mut db,
        &grant.payload,
        "execution-two",
        "host-one",
        &image,
    )
    .unwrap();
    assert_ne!(second, id);
    assert_eq!(
        db.query_one(
            "SELECT count(DISTINCT environment_id) FROM runtime.execution_instances",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    // Independent first allocations converge without overwriting another writer.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let url = url.clone();
            let binding = grant.payload.clone();
            let image = image.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut client = Client::connect(&url, NoTls).unwrap();
                (0..64)
                    .map(|attempt| {
                        barrier.wait();
                        super::super::execution_instances::bind(
                            &mut client,
                            &binding,
                            &format!("concurrent-execution-{attempt}"),
                            "host-one",
                            &image,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    for result in ids.iter().flatten() {
        assert!(result.is_ok(), "{result:?}");
    }
    assert_eq!(ids[0], ids[1]);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.execution_instances WHERE execution_id LIKE 'concurrent-execution-%'", &[]).unwrap().get::<_,i64>(0),64);
    let mut next_run = grant.payload.clone();
    next_run.agent_run_id = "agent_run_2".into();
    db.execute(
        "INSERT INTO public.app_core_agentrun(id,session_id,environment_id) VALUES($1,$2,$3)",
        &[
            &next_run.agent_run_id,
            &next_run.session_id,
            &next_run.environment_id,
        ],
    )
    .unwrap();
    db.execute("INSERT INTO public.app_core_agentrunauthorization(agent_run_id,digest) VALUES($1,$2) ON CONFLICT(agent_run_id) DO UPDATE SET digest=EXCLUDED.digest", &[&next_run.agent_run_id,&next_run.agent_run_authorization_digest]).unwrap();
    assert!(
        super::super::execution_instances::bind(
            &mut db,
            &next_run,
            "execution-one",
            "host-one",
            &image
        )
        .is_err(),
        "same environment cannot authorize another Run to reuse an execution"
    );
    assert_ne!(
        super::super::execution_instances::bind(
            &mut db,
            &next_run,
            "next-run-execution",
            "host-one",
            &image
        )
        .unwrap(),
        id
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let competitors: Vec<_> = ["host-one", "host-two"]
        .into_iter()
        .map(|host| {
            let url = url.clone();
            let binding = grant.payload.clone();
            let image = image.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut db = Client::connect(&url, NoTls).unwrap();
                barrier.wait();
                super::super::execution_instances::bind(
                    &mut db,
                    &binding,
                    "host-race-execution",
                    host,
                    &image,
                )
            })
        })
        .collect();
    let results: Vec<_> = competitors.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.execution_instances WHERE execution_id='host-race-execution'", &[]).unwrap().get::<_,i64>(0),1);
    // A stored secondary identity collision is never a successful replay of a
    // different execution, even though both uniqueness conflicts are handled.
    let collision = super::super::execution_instances::instance_id("collision-candidate");
    db.execute("INSERT INTO runtime.execution_instances SELECT 'collision-owner',$1,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,'collision-owner',allocation_mode,profile_digest FROM runtime.execution_instances WHERE execution_id='execution-one'", &[&collision]).unwrap();
    assert_eq!(
        super::super::execution_instances::bind(
            &mut db,
            &grant.payload,
            "collision-candidate",
            "host-one",
            &image
        )
        .unwrap_err(),
        "execution instance allocation identity collision"
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.execution_instances WHERE execution_id='collision-candidate'", &[]).unwrap().get::<_,i64>(0),0);
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
