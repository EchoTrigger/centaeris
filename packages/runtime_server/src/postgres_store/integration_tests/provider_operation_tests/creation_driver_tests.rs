use super::*;
use crate::e2b_adapter::{AdapterFailure, AdapterProcess, AdapterResponse};
use crate::e2b_creation::{E2bCreationAdapter, E2bCreationDriver, E2bCreationRequest};
use serde_json::{json, Value};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

struct RecordingAdapter {
    mode: &'static str,
    calls: Mutex<Vec<Value>>,
}
impl RecordingAdapter {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            calls: Mutex::new(vec![]),
        }
    }
}
impl E2bCreationAdapter for RecordingAdapter {
    fn create(
        &self,
        request_id: &str,
        payload: Value,
        _timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        self.calls.lock().unwrap().push(payload.clone());
        match self.mode {
            "transportNotDispatched" => return Err(AdapterFailure::NotDispatched("adapter_busy")),
            "transportUnknown" => {
                return Err(AdapterFailure::Unknown("adapter_response_unavailable"))
            }
            _ => {}
        }
        let attempt = if self.mode == "wrongAttempt" {
            "unrelated-attempt"
        } else {
            payload["attemptId"].as_str().unwrap()
        };
        let mut value = json!({"schema":"runtime.e2b.adapter.response.v1","requestId":request_id,"outcome":"succeeded","result":{"attemptId":attempt,"sandboxId":"opaque-sandbox","templateId":"centaeris-core","metadata":payload["metadata"],"state":"running"}});
        match self.mode {
            "adapterNotDispatched" => {
                value["outcome"] = json!("notDispatched");
                value["result"] = json!({"attemptId":attempt});
                value["error"] = json!({"code":"laneForbidden","message":"not sent"});
            }
            "adapterUnknown" => {
                value["outcome"] = json!("unknown");
                value["result"] = json!({"attemptId":attempt});
                value["error"] = json!({"code":"providerRequestFailed","message":"unknown"});
            }
            "unknownWithId" => {
                value["outcome"] = json!("unknown");
                value["result"] = json!({"attemptId":attempt,"sandboxId":"opaque-sandbox"});
                value["error"] =
                    json!({"code":"providerRequestFailed","message":"inspection failed"});
            }
            "wrongMetadata" => {
                value["result"]["metadata"]["centaerisAttemptId"] = json!("unrelated-attempt");
            }
            "wrongRequestId" => {
                value["requestId"] = json!("unrelated-request");
            }
            _ => {}
        }
        Ok(serde_json::from_value(value).unwrap())
    }
}

fn creation_request(run: &Run) -> E2bCreationRequest<'_> {
    E2bCreationRequest {
        authorization: &run.original,
        execution_id: "execution-provider",
        attempt_id: "attempt-provider",
        idempotency_key: "key-provider",
        configured_provider_config_digest: CONFIG,
        transport_timeout: Duration::from_secs(20),
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_sends_exact_payload_once_and_reopens_without_send() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("success");
    let request = creation_request(&run);
    let operation = E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
        .unwrap();
    assert_eq!(
        operation.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("opaque-sandbox")
    );
    assert_eq!(
        db.query_one("SELECT verified_sandbox_id FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&operation.identity.attempt_id]).unwrap().get::<_, Option<String>>(0).as_deref(),
        Some("opaque-sandbox"),
        "the actual successful create inspection must survive VM expiry before cleanup"
    );
    let payload = adapter.calls.lock().unwrap()[0].clone();
    assert_eq!(payload["attemptId"], "attempt-provider");
    assert_eq!(payload["idempotencyKey"], "key-provider");
    assert_eq!(
        payload["metadata"]["centaerisAttemptId"],
        "attempt-provider"
    );
    assert_eq!(payload["metadata"]["environmentId"], "environment");
    assert_eq!(
        payload["metadata"]["instanceId"],
        operation.identity.instance_id
    );
    assert_eq!(
        payload["metadata"]["profileDigest"],
        run.original.environment_profile_digest
    );
    assert_eq!(
        payload["templateRef"]["buildId"],
        "e8e0d749-b6a4-4985-a50c-a239c77536f1"
    );
    assert_eq!(payload["timeoutSeconds"], 3600);
    assert_eq!(payload["onTimeout"], "kill");
    assert_eq!(payload["allowInternetAccess"], false);
    assert_eq!(payload.as_object().unwrap().len(), 7);
    let canonical = crate::agent_run_authorization::canonical_json(payload).unwrap();
    assert_eq!(
        operation.identity.request_digest,
        format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
    );
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        E2bCreationDriver::new(&reopened, &adapter)
            .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
            .unwrap(),
        operation
    );
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.resident_sandboxes)+(SELECT count(*) FROM runtime.execution_instances)",&[]).unwrap().get::<_,i64>(0),0);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_concurrent_callers_get_only_one_send() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = Arc::new(run(&store, &mut db));
    let adapter = Arc::new(RecordingAdapter::new("success"));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let run = run.clone();
            let adapter = adapter.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                E2bCreationDriver::new(&store, adapter.as_ref()).create_or_reopen(
                    &run.authority(ControllerPurpose::Execute),
                    &creation_request(&run),
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(results.iter().any(Result::is_ok));
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .provider_creation_operation("attempt-provider")
            .unwrap()
            .unwrap()
            .dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_all_post_cas_failures_preserve_dispatch_and_never_retry() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mode in [
        "transportNotDispatched",
        "transportUnknown",
        "adapterNotDispatched",
        "adapterUnknown",
        "wrongAttempt",
        "wrongRequestId",
    ] {
        let (url, store, mut db) = retained::setup();
        let run = run(&store, &mut db);
        let adapter = RecordingAdapter::new(mode);
        let request = creation_request(&run);
        let _ = E2bCreationDriver::new(&store, &adapter)
            .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request);
        let operation = store
            .provider_creation_operation("attempt-provider")
            .unwrap()
            .unwrap();
        assert_eq!(
            operation.dispatch_state,
            ProviderCreationDispatchState::Dispatched,
            "{mode}"
        );
        assert!(operation.reported_sandbox_id.is_none(), "{mode}");
        let reopened = PostgresRuntimeStore::new(&url).unwrap();
        assert_eq!(
            E2bCreationDriver::new(&reopened, &adapter)
                .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
                .unwrap(),
            operation
        );
        assert_eq!(adapter.calls.lock().unwrap().len(), 1, "{mode}");
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_late_candidate_does_not_restore_old_execution_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("adapterUnknown");
    let operation = E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &creation_request(&run),
        )
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2; UPDATE runtime.runtime_jobs SET lease_owner='worker:next-owner'").unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let driver = E2bCreationDriver::new(&reopened, &adapter);
    let candidate = driver
        .record_late_candidate(&operation.identity, "opaque-late-sandbox")
        .unwrap();
    assert_eq!(
        candidate.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert_eq!(
        candidate.reported_sandbox_id.as_deref(),
        Some("opaque-late-sandbox")
    );
    assert!(reopened
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity
        )
        .is_err());
    assert_eq!(
        driver
            .create_or_reopen(
                &run.authority(ControllerPurpose::Execute),
                &creation_request(&run)
            )
            .unwrap(),
        candidate
    );
    let mut changed = creation_request(&run);
    changed.idempotency_key = "other-key";
    assert!(driver
        .create_or_reopen(&run.authority(ControllerPurpose::Execute), &changed)
        .is_err());
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_preflight_closes_unsent_only_and_rejects_changed_loaded_identity() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("success");
    let driver = E2bCreationDriver::new(&store, &adapter);
    let mut invalid = creation_request(&run);
    invalid.transport_timeout = Duration::ZERO;
    assert!(driver
        .create_or_reopen(&run.authority(ControllerPurpose::Execute), &invalid)
        .is_err());
    let closed = store
        .provider_creation_operation("attempt-provider")
        .unwrap()
        .unwrap();
    assert_eq!(
        closed.dispatch_state,
        ProviderCreationDispatchState::NotDispatched
    );
    assert_eq!(
        driver
            .create_or_reopen(
                &run.authority(ControllerPurpose::Execute),
                &creation_request(&run)
            )
            .unwrap(),
        closed
    );
    for field in ["execution", "key", "configuration", "workspace"] {
        let mut changed = creation_request(&run);
        let mut foreign = run.original.clone();
        match field {
            "execution" => changed.execution_id = "another-execution",
            "key" => changed.idempotency_key = "another-key",
            "configuration" => changed.configured_provider_config_digest = REQUEST,
            "workspace" => {
                foreign.workspace_id = "another-workspace".into();
                changed.authorization = &foreign;
            }
            _ => unreachable!(),
        }
        assert!(
            driver
                .create_or_reopen(&run.authority(ControllerPurpose::Execute), &changed)
                .is_err(),
            "{field}"
        );
    }
    assert!(adapter.calls.lock().unwrap().is_empty());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_retains_candidate_when_sdk_inspection_failed() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("unknownWithId");
    let operation = E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &creation_request(&run),
        )
        .unwrap();
    assert_eq!(
        operation.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("opaque-sandbox")
    );
    assert!(db.query_one("SELECT verified_sandbox_id FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&operation.identity.attempt_id]).unwrap().get::<_, Option<String>>(0).is_none(),
        "a locator returned with Unknown must not authorize execution or deletion");
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_preserves_matching_attempt_locator_despite_metadata_contradiction()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("wrongMetadata");
    let request = creation_request(&run);
    assert!(E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
        .is_err());
    let operation = store
        .provider_creation_operation("attempt-provider")
        .unwrap()
        .unwrap();
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("opaque-sandbox"),
        "a contradictory inspection must not erase the resource locator"
    );
    assert_eq!(
        operation.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let driver = E2bCreationDriver::new(&reopened, &adapter);
    assert_eq!(
        driver
            .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
            .unwrap(),
        operation
    );
    let mut changed = creation_request(&run);
    changed.configured_provider_config_digest = REQUEST;
    assert!(driver
        .create_or_reopen(&run.authority(ControllerPurpose::Execute), &changed)
        .is_err());
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_validates_provider_key_before_preparation_and_accepts_255_bytes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("success");
    let driver = E2bCreationDriver::new(&store, &adapter);
    for invalid in [
        String::new(),
        "UPPER".into(),
        "key_name".into(),
        "key.name".into(),
        "key:name".into(),
        "é".into(),
        "a".repeat(256),
    ] {
        let mut request = creation_request(&run);
        request.idempotency_key = &invalid;
        assert!(
            driver
                .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
                .is_err(),
            "invalid provider key was accepted"
        );
        assert!(store
            .provider_creation_operation("attempt-provider")
            .unwrap()
            .is_none());
        assert!(adapter.calls.lock().unwrap().is_empty());
    }
    let valid = "a".repeat(255);
    let mut request = creation_request(&run);
    request.idempotency_key = &valid;
    assert!(driver
        .create_or_reopen(&run.authority(ControllerPurpose::Execute), &request)
        .is_ok());
    assert_eq!(adapter.calls.lock().unwrap()[0]["idempotencyKey"], valid);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_rejects_lifetime_grant_mismatch_before_preparing_or_sending() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = run(&store, &mut db);
    let crate::agent_run_authorization::EnvironmentProfile::E2b(profile) =
        &mut run.original.environment_profile
    else {
        unreachable!()
    };
    profile.lifetime = "retained".into();
    run.original.environment_profile_digest = run.original.environment_profile.digest().unwrap();
    run.grant.payload.agent_run_authorization_digest = run.original.digest().unwrap();
    run.grant = retained::sign(run.grant.payload);
    let adapter = RecordingAdapter::new("success");
    assert!(matches!(
        E2bCreationDriver::new(&store, &adapter).create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &creation_request(&run)
        ),
        Err(crate::e2b_creation::E2bCreationError::Rejected(
            "e2b_creation_authorization_invalid"
        ))
    ));
    assert!(store
        .provider_creation_operation("attempt-provider")
        .unwrap()
        .is_none());
    assert!(adapter.calls.lock().unwrap().is_empty());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_accepts_explicit_retained_policy_and_preserves_verified_target() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut profile: crate::agent_run_authorization::EnvironmentProfile =
        serde_json::from_str(include_str!(
            "../../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
    let crate::agent_run_authorization::EnvironmentProfile::E2b(value) = &mut profile else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    let mut run = run_with_profile(&store, &mut db, profile);
    run.grant.payload.execution_mode = "retained".into();
    run.grant = retained::sign(run.grant.payload.clone());
    let adapter = RecordingAdapter::new("success");
    let operation = E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &creation_request(&run),
        )
        .unwrap();
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("opaque-sandbox")
    );
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
    let target = db.query_one("SELECT p.verified_sandbox_id,b.creation_attempt_id FROM runtime.provider_creation_operations p JOIN runtime.provider_environment_bindings b ON b.creation_attempt_id=p.attempt_id WHERE p.attempt_id=$1", &[&operation.identity.attempt_id]).unwrap();
    assert_eq!(
        target.get::<_, Option<String>>(0).as_deref(),
        Some("opaque-sandbox")
    );
    assert_eq!(target.get::<_, String>(1), operation.identity.attempt_id);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_run_uses",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "the Driver owns physical creation evidence; Runtime separately binds the first consumer"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_preserves_candidate_on_record_failure_without_second_send() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let adapter = RecordingAdapter::new("success");
    db.batch_execute("ALTER TABLE runtime.provider_creation_operations ADD CONSTRAINT fixture_reject_report CHECK(reported_sandbox_id IS NULL)").unwrap();
    let error = E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &creation_request(&run),
        )
        .unwrap_err();
    let crate::e2b_creation::E2bCreationError::CandidateNotRecorded {
        identity,
        sandbox_id,
    } = error
    else {
        panic!("known candidate must survive a failed evidence commit")
    };
    assert_eq!(sandbox_id, "opaque-sandbox");
    assert_eq!(
        store
            .provider_creation_operation("attempt-provider")
            .unwrap()
            .unwrap()
            .dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    db.batch_execute(
        "ALTER TABLE runtime.provider_creation_operations DROP CONSTRAINT fixture_reject_report",
    )
    .unwrap();
    let driver = E2bCreationDriver::new(&store, &adapter);
    let candidate = driver
        .record_late_candidate(&identity, &sandbox_id)
        .unwrap();
    assert_eq!(
        driver
            .create_or_reopen(
                &run.authority(ControllerPurpose::Execute),
                &creation_request(&run)
            )
            .unwrap(),
        candidate
    );
    assert_eq!(adapter.calls.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_creation_driver_runs_real_sdk_loopback_http_through_ipc_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let directory = tempfile::tempdir().unwrap();
    let requests = directory.path().join("requests.json");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            root.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            })
        });
    let mut command = std::process::Command::new(python);
    command.args(["-u", "-c"]);
    command.arg(r#"
import json, sys
from pathlib import Path
root=Path(sys.argv[1])
sys.path.insert(0,str(root/'packages/e2b_adapter/tests'))
sys.path.insert(0,str(root/'packages/e2b_adapter'))
from test_sdk_transport_acceptance import provider_fixture
from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
with provider_fixture() as fixture:
 # This fixture must expose the actual template bound by this signed profile.
 # The generic Python fixture otherwise reports its own template-fixture ID.
 original_sandbox=fixture.sandbox
 fixture.sandbox=lambda:{**original_sandbox(), 'templateID':'centaeris-core'}
 adapter=Adapter(AdapterConfig(api_key='owned-loopback-fixture',domain='fixture.invalid',api_url=fixture.api_url,sandbox_url=fixture.api_url,idempotency_supported=False))
 for line in sys.stdin:
  response=adapter.handle(json.loads(line))
  Path(sys.argv[2]).write_text(json.dumps(fixture.requests))
  print(json.dumps(response),flush=True)
"#);
    command.arg(&root).arg(&requests);
    let adapter = AdapterProcess::spawn(command).unwrap();
    let operation = E2bCreationDriver::new(&store, &adapter)
        .create_or_reopen(
            &run.authority(ControllerPurpose::Execute),
            &creation_request(&run),
        )
        .unwrap();
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("sandbox-fixture")
    );
    let recorded: Vec<Value> = serde_json::from_slice(&std::fs::read(&requests).unwrap()).unwrap();
    assert_eq!(
        recorded
            .iter()
            .filter(|entry| entry["method"] == "POST")
            .count(),
        1
    );
    let sent = &recorded[0];
    assert_eq!(sent["path"], "/v2/sandboxes");
    assert_eq!(sent["headers"]["idempotency-key"], "key-provider");
    assert_eq!(
        sent["body"]["metadata"]["centaerisAttemptId"],
        "attempt-provider"
    );
    assert_eq!(sent["body"]["allow_internet_access"], false);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        E2bCreationDriver::new(&reopened, &adapter)
            .create_or_reopen(
                &run.authority(ControllerPurpose::Execute),
                &creation_request(&run)
            )
            .unwrap(),
        operation
    );
    assert_eq!(
        serde_json::from_slice::<Vec<Value>>(&std::fs::read(&requests).unwrap())
            .unwrap()
            .len(),
        recorded.len()
    );
}
