use super::*;
mod cleanup_tests;
mod product_preparation_tests;
mod retained_preparation_tests;
use crate::agent_run_authorization::EnvironmentProfile;
use crate::e2b_adapter::bulk::SpoolingAdapterProcess;
use crate::e2b_execution_host::E2bHostError;
use crate::e2b_runtime::{
    E2bAdapterLane, E2bAdapterLauncher, E2bPendingReason, E2bPrepareOutcome, E2bRuntime,
    E2bRuntimeConfig, E2bRuntimeError,
};
use hosted_execution::host_supervisor::TrustedHelperKind;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn python() -> PathBuf {
    std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(if cfg!(windows) {
                    ".venv/Scripts/python.exe"
                } else {
                    ".venv/bin/python"
                })
        })
        .canonicalize()
        .unwrap()
}

fn factory_profile() -> EnvironmentProfile {
    let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(
        "../../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap();
    let EnvironmentProfile::E2b(value) = &mut profile else {
        unreachable!()
    };
    value.workspace_root = "/mnt/data".into();
    value.user = "agent".into();
    profile
}

fn config(profile: EnvironmentProfile) -> E2bRuntimeConfig {
    E2bRuntimeConfig::new(
        profile,
        CONFIG.into(),
        python(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../e2b_adapter")
            .canonicalize()
            .unwrap(),
        Duration::from_secs(10),
    )
    .unwrap()
}

struct FixtureLauncher {
    mode: &'static str,
    log: PathBuf,
    launches: AtomicUsize,
    observation: Option<Value>,
}

impl FixtureLauncher {
    fn new(mode: &'static str, log: PathBuf) -> Self {
        Self {
            mode,
            log,
            launches: AtomicUsize::new(0),
            observation: None,
        }
    }

    fn with_observation(mut self, observation: Value) -> Self {
        self.observation = Some(observation);
        self
    }

    fn requests(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn creates(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request["operation"] == "create")
            .count()
    }
}

impl E2bAdapterLauncher for FixtureLauncher {
    fn launch(
        &self,
        _config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let mut command = Command::new(python());
        command.args(["-I","-u","-c",r#"import base64,json,sys
mode,log,lane,encoded_observation=sys.argv[1:]
observation=json.loads(encoded_observation)
for line in sys.stdin:
 request=json.loads(line)
 with open(log,"a",encoding="utf-8") as output:
  output.write(json.dumps(request)+"\n")
 operation,payload=request["operation"],request["payload"]
 outcome="succeeded"
 if operation=="capabilities":
  result={"sdkVersion":"2.58.0","lane":lane,"createIdempotencyConfigured":False,"readOnlyAttach":True,"commandStartIdempotency":False,"runProcessTreeCancellation":False,"controlCommandCancellation":False,"commandOutputBytes":True,"commandOutputRestartRecovery":False,"commandOutputRecovery":"adapterLifetime","waitBudgetMustCoverOriginalStream":True,"maxCommandEvidence":128,"fixedControlHelpers":["prepare-workspace","agent-process-state","quiesce-agent-processes"] if lane=="control" else [],"bulkFileTransfer":True,"maxTransferBytes":134217728,"transferChunkBytes":65536,"storageQuota":False,"fullMemoryPauseVerified":False,"maxFileBytes":262144,"maxOutputBytes":65536}
 elif operation=="create":
  result={"attemptId":payload["attemptId"],"sandboxId":"factory-sandbox","templateId":payload["templateRef"]["templateId"],"metadata":payload["metadata"],"state":"running"}
  if mode=="unknown":
   outcome="unknown"
   result={"attemptId":payload["attemptId"],"sandboxId":"factory-sandbox"}
  elif mode=="wrongMetadata":
   result["metadata"]["profileDigest"]="unrelated-profile"
 elif operation=="attach" and mode=="retainedReady":
  result={"sandboxId":payload["sandboxId"],"templateId":payload["expectedTemplateId"],"metadata":payload["expectedMetadata"],"state":"running"}
 elif operation=="invokeControlHelper" and mode in ("productReady","retainedReady"):
  stdout=json.dumps({"activeProcessCount":0}) if mode=="retainedReady" and payload["helper"]=="agent-process-state" else ""
  result={"sandboxId":"factory-sandbox","pid":31,"exitCode":0,"stdout":stdout,"stderr":"","stdoutBytesBase64":base64.b64encode(stdout.encode()).decode(),"stderrBytesBase64":"","stdoutByteLength":len(stdout),"stderrByteLength":0,"helper":payload["helper"]}
 elif operation=="inspect":
  result=observation
  if mode=="inspectUnknown":
   outcome="unknown"
   result={"sandboxId":payload["sandboxId"]}
 elif operation=="kill":
  result={"sandboxId":payload["sandboxId"],"deleteAcknowledged":True,"providerReportedKilled":mode!="notFound"}
  if mode=="ackFalse":
   result["deleteAcknowledged"]=False
  elif mode=="missingProviderResult":
   result.pop("providerReportedKilled")
  if mode=="deleteUnknown":
   outcome="unknown"
   result={"sandboxId":payload["sandboxId"]}
 else:
  outcome="unknown"
  result={"sandboxId":"factory-sandbox"}
 response={"schema":"runtime.e2b.adapter.response.v1","requestId":request["requestId"],"outcome":outcome,"result":result}
 if outcome=="unknown":
  response["error"]={"code":"providerRequestFailed","message":"Fixture response is unknown"}
 print(json.dumps(response),flush=True)
"#]);
        command.arg(self.mode).arg(&self.log).arg(match lane {
            E2bAdapterLane::Ordinary => "ordinary",
            E2bAdapterLane::Control => "control",
            E2bAdapterLane::Cleanup => "cleanup",
        });
        command.arg(serde_json::to_string(&self.observation).unwrap());
        SpoolingAdapterProcess::spawn(command)
            .map(Arc::new)
            .map_err(|_| E2bRuntimeError::Unavailable("fixture_launch_failed"))
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_fresh_create_retains_original_context_and_rebinds_current_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = run_with_profile(&store, &mut db, factory_profile());
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
    let host = match runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            "execution-factory",
        )
        .unwrap()
    {
        E2bPrepareOutcome::Prepared(host) => host,
        E2bPrepareOutcome::Pending { .. } => {
            panic!("fresh verified create must prepare one owned host")
        }
    };
    assert_eq!(launcher.creates(), 1);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert_eq!(host.binding().sandbox_id, "factory-sandbox");
    assert_eq!(host.identity().execution_id, "execution-factory");
    assert_eq!(db.query_one("SELECT verified_sandbox_id FROM runtime.provider_creation_operations WHERE attempt_id=$1",&[&host.identity().attempt_id]).unwrap().get::<_,Option<String>>(0).as_deref(),Some("factory-sandbox"));
    let runner = host.runner();
    assert!(runner.helper_effects_known());
    assert!(matches!(
        runner.invoke_helper(TrustedHelperKind::FileSystem, b"fixture", 64),
        Err(E2bHostError::Unknown(_))
    ));
    assert!(!runner.helper_effects_known());

    let owner = "worker:factory-reclaimed";
    db.batch_execute("UPDATE runtime.runtime_jobs SET lease_owner=NULL")
        .unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1", &[&owner])
        .unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_lease_owner=$1",&[&owner]).unwrap();
    run.fence.lease_owner = owner.into();
    let mut lease_digest = Sha256::new();
    lease_digest.update(b"workspace:environment-controller-lease:v1\0");
    lease_digest.update(owner.as_bytes());
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch = 2;
    controller.controller_lease_digest = format!("sha256:{:x}", lease_digest.finalize());
    run.grant = retained::sign(run.grant.payload.clone());
    host.rebind(&run.authority(ControllerPurpose::Execute))
        .unwrap();
    assert!(
        !host.runner().helper_effects_known(),
        "rebind cannot erase an uncertain mutating helper"
    );
    assert!(matches!(
        runtime
            .prepare(
                &run.authority(ControllerPurpose::Execute),
                &run.original,
                "execution-factory"
            )
            .unwrap(),
        E2bPrepareOutcome::Pending {
            reason: E2bPendingReason::OwnedContextUnavailable,
            ..
        }
    ));
    assert_eq!(launcher.creates(), 1);
    assert_eq!(
        launcher.launches.load(Ordering::SeqCst),
        2,
        "reopening cannot silently build a fresh mutable context"
    );
    assert!(launcher.requests().iter().all(|request| !matches!(
        request["operation"].as_str(),
        Some("attach" | "connect" | "kill" | "pause")
    )));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_unknown_reopens_without_dispatch_or_claiming_clean_context() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run_with_profile(&store, &mut db, factory_profile());
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "unknown",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    let operation = match runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            "execution-factory",
        )
        .unwrap()
    {
        E2bPrepareOutcome::Pending {
            operation,
            reason: E2bPendingReason::CreationUnconfirmed,
        } => operation,
        _ => panic!("create with unknown inspection must retain responsibility"),
    };
    assert_eq!(
        operation.reported_sandbox_id.as_deref(),
        Some("factory-sandbox")
    );
    assert!(db.query_one("SELECT verified_sandbox_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",&[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
    drop(runtime);
    let reopened = E2bRuntime::with_launcher(
        PostgresRuntimeStore::new(&url).unwrap(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    assert!(matches!(
        reopened
            .prepare(
                &run.authority(ControllerPurpose::Execute),
                &run.original,
                "execution-factory"
            )
            .unwrap(),
        E2bPrepareOutcome::Pending { .. }
    ));
    assert_eq!(launcher.creates(), 1);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert_eq!(
        store
            .provider_creation_operation(&operation.identity.attempt_id)
            .unwrap(),
        Some(*operation)
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_rejects_profile_mismatch_before_launch_and_preserves_bad_response_locator()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run_with_profile(&store, &mut db, factory_profile());
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "wrongMetadata",
        root.path().join("requests.jsonl"),
    ));
    let mut mismatched = run.original.environment_profile.clone();
    let EnvironmentProfile::E2b(value) = &mut mismatched else {
        unreachable!()
    };
    value.input_budget_bytes -= 1;
    let mismatched_runtime =
        E2bRuntime::with_launcher(store.clone(), config(mismatched), launcher.clone());
    assert!(mismatched_runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            "execution-factory"
        )
        .is_err());
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    assert!(runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            "execution-factory"
        )
        .is_err());
    assert_eq!(launcher.creates(), 1);
    let row=db.query_one("SELECT reported_sandbox_id,verified_sandbox_id FROM runtime.provider_creation_operations",&[]).unwrap();
    assert_eq!(
        row.get::<_, Option<String>>(0).as_deref(),
        Some("factory-sandbox")
    );
    assert!(row.get::<_, Option<String>>(1).is_none());
}
