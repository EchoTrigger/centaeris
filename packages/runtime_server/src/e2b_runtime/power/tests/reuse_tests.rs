//! Exercise receipt validation and the production lane closeout with real child IO.
use super::*;
use crate::postgres_store::provider_operations::retained::power::{
    ProviderEnvironmentPowerDispatchState, ProviderEnvironmentPowerOperation,
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Launcher {
    first_reply: &'static str,
    launches: AtomicUsize,
    effects: PathBuf,
}
impl E2bAdapterLauncher for Launcher {
    fn launch(
        &self,
        config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        assert_eq!(lane, E2bAdapterLane::Cleanup);
        let count = self.launches.fetch_add(1, Ordering::SeqCst);
        let capabilities = json!({
            "sdkVersion":"2.58.0", "lane":"cleanup", "createIdempotencyConfigured":false,
            "readOnlyAttach":true, "commandStartIdempotency":false,
            "runProcessTreeCancellation":false, "controlCommandCancellation":false,
            "commandOutputBytes":true, "commandOutputRestartRecovery":false,
            "commandOutputRecovery":"adapterLifetime", "waitBudgetMustCoverOriginalStream":true,
            "maxCommandEvidence":128, "fixedControlHelpers":[], "bulkFileTransfer":true,
            "maxTransferBytes":MAX_TRANSFER_BYTES, "transferChunkBytes":TRANSFER_CHUNK_BYTES,
            "storageQuota":false, "fullMemoryPauseVerified":false,
            "maxFileBytes":256*1024, "maxOutputBytes":64*1024
        });
        let mut command = Command::new(&config.python);
        command.args(["-u", "-c", r#"
import json,sys
capabilities=json.loads(sys.argv[1])
for raw in sys.stdin:
 r=json.loads(raw)
 if r['operation']=='capabilities': result=capabilities
 else:
  with open(sys.argv[3], 'a') as f: f.write(r['operation']+'\n')
  if sys.argv[2]=='lost': sys.exit(1)
  p=r['payload']
  if r['operation']=='pause':
   result={'sandboxId':p['sandboxId'],'requestedMode':'full','changed':sys.argv[2]!='invalid'}
  else:
   result={'sandboxId':p['sandboxId'],'templateId':p['expectedTemplateId'],'metadata':p['expectedMetadata'],'state':'running'}
 print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':r['requestId'],'outcome':'succeeded','result':result}),flush=True)
"#]).arg(capabilities.to_string())
            .arg(if count == 0 { self.first_reply } else { "valid" })
            .arg(&self.effects);
        Ok(Arc::new(SpoolingAdapterProcess::spawn(command).unwrap()))
    }
}
fn runtime(first_reply: &'static str, effects: PathBuf) -> (E2bRuntime, Arc<Launcher>) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            })
        });
    let config = E2bRuntimeConfig {
        profile: serde_json::from_str(include_str!(
            "../../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap(),
        provider_config_digest: target().identity.provider_config_digest,
        python,
        adapter_package_root: root,
        request_timeout: Duration::from_secs(5),
        running_limit: None,
        idle_pause_timeout: None,
    };
    let launcher = Arc::new(Launcher {
        first_reply,
        launches: AtomicUsize::new(0),
        effects,
    });
    (
        E2bRuntime::with_launcher(
            PostgresRuntimeStore::rejecting_http_test_store(),
            config,
            launcher.clone(),
        ),
        launcher,
    )
}
fn exchange(
    runtime: &E2bRuntime,
    action: ProviderEnvironmentPowerAction,
    commit_succeeds: bool,
) -> Result<ProviderEnvironmentPowerResult, E2bRuntimeError> {
    let target = target();
    let mut request = request(&target, action);
    request.operation_id = format!("power-{}", uuid::Uuid::new_v4());
    let mut lane = cleanup::CleanupLane::new_observed(
        runtime,
        PowerObservation::new(
            &request.operation_id,
            if action == ProviderEnvironmentPowerAction::Pause {
                "pause"
            } else {
                "resume"
            },
        ),
    );
    let receipt = dispatch_power(
        &target,
        &request,
        &binding(&target),
        "template-fixture",
        120,
        |operation, payload| lane.call_payload(operation, payload),
    );
    // Use the same public operation->result projection as Store after a fresh
    // dispatch. Persistence itself is covered by the retained power PG suite.
    let operation = ProviderEnvironmentPowerOperation {
        request,
        sequence: 1,
        dispatch_state: ProviderEnvironmentPowerDispatchState::Dispatched,
        acknowledgement: receipt.ok(),
        automatic_demand: None,
        not_dispatched_at_ms: None,
    };
    let result = if commit_succeeds {
        Ok(operation.result())
    } else {
        Err(E2bRuntimeError::Unknown("e2b_power_unconfirmed"))
    };
    finish_power_lane(lane, result)
}
fn assert_pending_discards_child(first_reply: &'static str) {
    let directory = tempfile::tempdir().unwrap();
    let effects = directory.path().join("effects");
    let (runtime, launcher) = runtime(first_reply, effects.clone());
    let first = exchange(&runtime, ProviderEnvironmentPowerAction::Pause, true).unwrap();
    assert_eq!(first.operation_status, "pending");
    let next = exchange(&runtime, ProviderEnvironmentPowerAction::Pause, true).unwrap();
    assert_eq!(next.operation_status, "acknowledged");
    assert_eq!(
        launcher.launches.load(Ordering::SeqCst),
        2,
        "pending child must be discarded"
    );
    assert_eq!(
        std::fs::read_to_string(effects)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["pause", "pause"],
        "no replay of the unconfirmed operation"
    );
}
#[test]
fn lost_power_reply_discards_child_before_next_authorized_operation() {
    assert_pending_discards_child("lost");
}
#[test]
fn invalid_power_receipt_discards_child_before_next_authorized_operation() {
    assert_pending_discards_child("invalid");
}
#[test]
fn acknowledged_power_operations_reuse_the_child() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, launcher) = runtime("valid", directory.path().join("effects"));
    for action in [
        ProviderEnvironmentPowerAction::Pause,
        ProviderEnvironmentPowerAction::Resume,
    ] {
        assert_eq!(
            exchange(&runtime, action, true).unwrap().operation_status,
            "acknowledged"
        );
    }
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
}
#[test]
fn failed_ack_commit_discards_child_even_after_valid_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, launcher) = runtime("valid", directory.path().join("effects"));
    assert!(exchange(&runtime, ProviderEnvironmentPowerAction::Pause, false).is_err());
    assert_eq!(
        exchange(&runtime, ProviderEnvironmentPowerAction::Pause, true)
            .unwrap()
            .operation_status,
        "acknowledged"
    );
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
}
