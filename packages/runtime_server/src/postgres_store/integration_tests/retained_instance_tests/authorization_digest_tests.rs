use super::*;
use crate::environment_authorization::digest_observation;

fn canonical_bytes(value: &impl serde::Serialize) -> String {
    crate::agent_run_authorization::canonical_json(serde_json::to_value(value).unwrap()).unwrap()
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_authority_rechecks_current_state_and_preserves_digest_bytes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = run(&store, &mut db, "digest-authority", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &current, "digest-execution", &physical).unwrap();
    let profile = current.original.environment_profile.digest().unwrap();
    let environment_bytes = canonical_bytes(&current.grant.payload).len();
    let profile_bytes = canonical_bytes(&current.original.environment_profile).len();
    let signed = serde_json::to_string(&current.grant).unwrap();
    for call in 0..2 {
        let (result, work) = digest_observation::measure(|| {
            store.with_retained_instance_operation(
                &current.request("digest-execution", &profile),
                &allocation,
                |actual| Ok(actual.instance_id.clone()),
            )
        });
        assert_eq!(result.unwrap(), allocation.instance_id);
        assert!(work.environment_calls > 0 && work.profile_calls > 0);
        assert_eq!(
            work.environment_bytes,
            work.environment_calls * environment_bytes
        );
        assert_eq!(work.profile_bytes, work.profile_calls * profile_bytes);
        assert_eq!(serde_json::to_string(&current.grant).unwrap(), signed);
        eprintln!("controller digest characterization call={call} work={work:?}");
    }
    for (change, restore, expected) in [
        ("UPDATE public.app_core_sessionenvironment SET environment_profile=jsonb_set(environment_profile,'{user}','\"1:1\"'::jsonb)", "UPDATE public.app_core_sessionenvironment SET environment_profile=(SELECT payload->'environmentProfile' FROM public.app_core_agentrunauthorization WHERE agent_run_id='digest-authority')", "environment_profile_binding_rejected"),
        ("UPDATE public.app_core_sessionenvironment SET environment_profile_digest='sha256:'||repeat('0',64)", "UPDATE public.app_core_sessionenvironment SET environment_profile_digest=(SELECT payload->>'environmentProfileDigest' FROM public.app_core_agentrunauthorization WHERE agent_run_id='digest-authority')", "environment_profile_binding_rejected"),
        ("UPDATE public.app_core_workspacemembership SET role='revoked'", "UPDATE public.app_core_workspacemembership SET role='member'", "retained membership rejected"),
        ("UPDATE public.app_core_sessionenvironment SET controller_epoch=2", "UPDATE public.app_core_sessionenvironment SET controller_epoch=1", "retained controller fence rejected"),
        ("UPDATE runtime.runtime_jobs SET lease_owner='reclaimed'", "UPDATE runtime.runtime_jobs SET lease_owner='owner-digest-authority'", "retained job fence rejected"),
        ("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','digest-authority',false,'{\"type\":\"agent_run_completed\"}')", "DELETE FROM public.app_core_sessionevent", "retained Core terminal execution rejected"),
    ] {
        db.batch_execute(change).unwrap();
        let (rejected, _): (Result<(), String>, _) = digest_observation::measure(|| {
            store.with_retained_instance_operation(
                &current.request("digest-execution", &profile), &allocation,
                |_| panic!("changed current authority reached the operation"),
            )
        });
        assert_eq!(rejected.unwrap_err(), expected);
        db.batch_execute(restore).unwrap();
    }
    let mut forged = current.grant.clone();
    forged.signature = format!("hmac-sha256:{}", "0".repeat(64));
    let mut request = current.request("digest-execution", &profile);
    request.environment_authorization = &forged;
    assert_eq!(
        store
            .with_retained_instance_operation::<()>(&request, &allocation, |_| panic!(
                "forged signature reached operation"
            ))
            .unwrap_err(),
        "environment_authorization_signature_invalid"
    );
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_authority_hashes_each_binding_once_per_call() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = run(&store, &mut db, "digest-work", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &current, "digest-work-execution", &physical).unwrap();
    let profile = current.original.environment_profile.digest().unwrap();
    let expected = digest_observation::DigestWork {
        environment_calls: 1,
        environment_bytes: canonical_bytes(&current.grant.payload).len(),
        profile_calls: 1,
        profile_bytes: canonical_bytes(&current.original.environment_profile).len(),
    };
    for call in 0..2 {
        let (result, work) = digest_observation::measure(|| {
            store.with_retained_instance_operation(
                &current.request("digest-work-execution", &profile),
                &allocation,
                |_| Ok(()),
            )
        });
        result.unwrap();
        eprintln!("controller digest work call={call} actual={work:?} expected={expected:?}");
        assert_eq!(
            work, expected,
            "each independent call must verify fresh bytes once"
        );
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_authority_rejects_equal_but_invalid_original_profile() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = run(&store, &mut db, "invalid-original-profile", 1);
    let mut original = current.original.clone();
    original.environment_profile.user = "invalid-user".into();
    // Even a signed original and exactly equal persisted profile must pass the
    // original grant's full validation before its profile result can be reused.
    let profile_json = canonical_bytes(&original.environment_profile);
    original.environment_profile_digest =
        format!("sha256:{:x}", Sha256::digest(profile_json.as_bytes()));
    let original_json = canonical_bytes(&original);
    let digest = format!("sha256:{:x}", Sha256::digest(original_json.as_bytes()));
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(b"workspace:agent-run-authorization:v1\0");
    mac.update(digest.as_bytes());
    let signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    db.execute("UPDATE public.app_core_agentrunauthorization SET digest=$2,payload=$3::text::jsonb,signature=$4 WHERE agent_run_id=$1", &[&original.agent_run_id,&digest,&original_json,&signature]).unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2", &[&profile_json,&original.environment_profile_digest]).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET idempotency_key=$2 WHERE job_id=$1",
        &[
            &current.fence.job_id,
            &format!("{}:{digest}", current.fence.job_id),
        ],
    )
    .unwrap();
    let mut payload = current.grant.payload.clone();
    payload.agent_run_authorization_digest = digest;
    let grant = sign(payload);
    let mut request = current.request(
        "invalid-profile-execution",
        &original.environment_profile_digest,
    );
    request.environment_authorization = &grant;
    let rejected = store.prepare_retained_instance(
        &request,
        limits(&current),
        || panic!("invalid original profile reached inventory"),
        |_, _| panic!("invalid original profile published a fact"),
        |_| panic!("invalid original profile reached physical work"),
    );
    assert_eq!(rejected.unwrap_err(), "environment_profile_user_invalid");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_authority_rejects_malformed_binding_before_lookup() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = run(&store, &mut db, "malformed-binding", 1);
    let profile = current.original.environment_profile.digest().unwrap();
    for (field, expected) in [
        ("schema", "environment_authorization_schema_or_mode_invalid"),
        ("session", "environment_authorization_identity_invalid"),
        ("digest", "environment_authorization_digest_invalid"),
        ("controller", "environment_controller_binding_invalid"),
    ] {
        let mut grant = current.grant.clone();
        grant.payload.agent_run_id = "unrecorded-run".into();
        grant.payload.controller.as_mut().unwrap().controller_id = "unrecorded-run".into();
        match field {
            "schema" => grant.payload.schema = "unsupported".into(),
            "session" => grant.payload.session_id = "bad session".into(),
            "digest" => grant.payload.agent_run_authorization_digest = "invalid".into(),
            "controller" => grant.payload.controller.as_mut().unwrap().controller_epoch = 0,
            _ => unreachable!(),
        }
        let mut request = current.request("malformed-execution", &profile);
        request.environment_authorization = &grant;
        let rejected = store.prepare_retained_instance(
            &request,
            limits(&current),
            || panic!("malformed binding reached inventory"),
            |_, _| panic!("malformed binding published a fact"),
            |_| panic!("malformed binding reached physical work"),
        );
        assert_eq!(rejected.unwrap_err(), expected, "{field}");
    }
}
