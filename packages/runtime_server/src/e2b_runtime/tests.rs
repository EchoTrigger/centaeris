use super::*;
use serde_json::json;
use std::fs;

fn profile() -> EnvironmentProfile {
    let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap();
    let EnvironmentProfile::E2b(value) = &mut profile else {
        unreachable!()
    };
    value.workspace_root = "/mnt/data".into();
    value.user = "agent".into();
    profile
}

fn paths() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let python = root.path().join("python-fixture");
    fs::write(&python, b"fixture").unwrap();
    let package = root.path().join("package");
    fs::create_dir_all(package.join("centaeris_e2b_adapter")).unwrap();
    fs::write(package.join("centaeris_e2b_adapter/__init__.py"), b"").unwrap();
    fs::write(package.join("centaeris_e2b_adapter/__main__.py"), b"").unwrap();
    (root, python, package)
}

fn configured_digest() -> String {
    let EnvironmentProfile::E2b(profile) = profile() else {
        unreachable!()
    };
    profile.provider_config_digest
}

#[test]
fn configuration_keeps_exact_provider_profile_and_selected_absolute_paths() {
    let (_root, python, package) = paths();
    let expected = profile();
    let config = E2bRuntimeConfig::new(
        expected.clone(),
        configured_digest(),
        python.clone(),
        package.clone(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(config.profile(), &expected);
    assert_eq!(config.python, python.canonicalize().unwrap());
    assert_eq!(config.adapter_package_root, package.canonicalize().unwrap());
}

#[test]
fn configuration_rejects_unavailable_paths_and_unsupported_profile_before_launch() {
    let (_root, python, package) = paths();
    for field in [
        "workspace",
        "user",
        "digest",
        "relativePython",
        "relativePackage",
        "missingModule",
        "timeout",
    ] {
        let mut selected = profile();
        let EnvironmentProfile::E2b(value) = &mut selected else {
            unreachable!()
        };
        match field {
            "workspace" => value.workspace_root = "/another".into(),
            "user" => value.user = "root".into(),
            _ => {}
        }
        let result = E2bRuntimeConfig::new(
            selected,
            if field == "digest" {
                format!("sha256:{}", "d".repeat(64))
            } else {
                configured_digest()
            },
            if field == "relativePython" {
                PathBuf::from("python")
            } else {
                python.clone()
            },
            if field == "relativePackage" {
                PathBuf::from("package")
            } else if field == "missingModule" {
                package.join("absent")
            } else {
                package.clone()
            },
            if field == "timeout" {
                Duration::ZERO
            } else {
                Duration::from_secs(20)
            },
        );
        assert!(result.is_err(), "{field}");
    }
}

#[test]
fn business_attempt_key_is_stable_and_binds_every_immutable_identity() {
    let fields = [
        "environment",
        "grant-digest",
        "profile-digest",
        "provider-digest",
        "execution",
    ];
    let key = make_attempt_key(fields[0], fields[1], fields[2], fields[3], fields[4]);
    assert!(!key.attempt_id.is_empty());
    assert!(key.attempt_id.len() <= 128);
    assert!(!key.idempotency_key.is_empty());
    assert!(key.idempotency_key.len() <= 255);
    assert!(key
        .idempotency_key
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'));
    assert_eq!(
        key,
        make_attempt_key(fields[0], fields[1], fields[2], fields[3], fields[4])
    );
    for changed in 0..fields.len() {
        let mut alternate = fields;
        alternate[changed] = "changed";
        assert_ne!(
            key,
            make_attempt_key(
                alternate[0],
                alternate[1],
                alternate[2],
                alternate[3],
                alternate[4]
            ),
            "identity field {changed}"
        );
    }
}

#[test]
fn installed_adapter_launch_imports_only_selected_package_under_python_isolation() {
    let (root, _, package) = paths();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            })
        });
    fs::write(package.join("centaeris_e2b_adapter/__main__.py"), r#"import json,pathlib,sys
for line in sys.stdin:
 request=json.loads(line)
 result={"selectedPackage":str(pathlib.Path(__file__).parent.parent.resolve()),"isolated":sys.flags.isolated,"lane":sys.argv[-1]}
 print(json.dumps({"schema":"runtime.e2b.adapter.response.v1","requestId":request["requestId"],"outcome":"succeeded","result":result}),flush=True)
"#).unwrap();
    let config = E2bRuntimeConfig::new(
        profile(),
        configured_digest(),
        python.canonicalize().unwrap(),
        package.clone(),
        Duration::from_secs(10),
    )
    .unwrap();
    for (lane, expected) in [
        (E2bAdapterLane::Ordinary, "ordinary"),
        (E2bAdapterLane::Control, "control"),
    ] {
        let process = InstalledAdapterLauncher.launch(&config, lane).unwrap();
        let result = process
            .call(
                "launch-fixture",
                "capabilities",
                json!({}),
                Duration::from_secs(5),
            )
            .unwrap()
            .result
            .unwrap();
        assert_eq!(result["isolated"], 1);
        assert_eq!(result["lane"], expected);
        assert_eq!(
            result["selectedPackage"].as_str().unwrap(),
            package.canonicalize().unwrap().to_str().unwrap()
        );
        process.close().unwrap();
    }
    drop(root);
}

#[test]
fn configuration_accepts_explicit_retained_without_reinterpreting_per_run() {
    let (_root, python, package) = paths();
    let mut selected = profile();
    let EnvironmentProfile::E2b(value) = &mut selected else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    let configured = E2bRuntimeConfig::new(
        selected.clone(),
        configured_digest(),
        python,
        package,
        Duration::from_secs(20),
    )
    .expect("an explicitly frozen retained profile must have its own lifecycle route");
    assert_eq!(configured.profile(), &selected);
    let EnvironmentProfile::E2b(original) = profile() else {
        unreachable!()
    };
    assert_eq!(
        original.lifetime, "perRun",
        "existing profile policy must not be reinterpreted"
    );
}

#[test]
fn adapter_transport_timings_measure_each_launch_and_validated_handshake_without_retry() {
    use crate::observations::{capture_timings, Context};
    let (root, _, package) = paths();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            })
        });
    fs::write(package.join("centaeris_e2b_adapter/__main__.py"), r#"import json,pathlib,sys
lane=sys.argv[-1]
for line in sys.stdin:
 request=json.loads(line)
 with pathlib.Path(__file__).with_name('requests').open('a') as count:
  count.write(request['operation']+'\n')
 result={"sdkVersion":"2.58.0","lane":lane,"createIdempotencyConfigured":False,"readOnlyAttach":True,
 "commandStartIdempotency":False,"runProcessTreeCancellation":False,"controlCommandCancellation":False,
 "commandOutputBytes":True,"commandOutputRestartRecovery":False,"commandOutputRecovery":"adapterLifetime",
 "waitBudgetMustCoverOriginalStream":True,"maxCommandEvidence":128,
 "fixedControlHelpers":["agent-process-state","prepare-workspace","quiesce-agent-processes"] if lane=='control' else [],
 "bulkFileTransfer":True,"maxTransferBytes":134217728,"transferChunkBytes":65536,"storageQuota":False,
 "fullMemoryPauseVerified":False,"maxFileBytes":262144,"maxOutputBytes":65536}
 print(json.dumps({"schema":"runtime.e2b.adapter.response.v1","requestId":request["requestId"],"outcome":"succeeded","result":result}),flush=True)
"#).unwrap();
    let config = E2bRuntimeConfig::new(
        profile(),
        configured_digest(),
        python.canonicalize().unwrap(),
        package.clone(),
        Duration::from_secs(10),
    )
    .unwrap();
    for (lane, spawn_phase, handshake_phase) in [
        (
            E2bAdapterLane::Ordinary,
            "e2bAdapterSpawnOrdinary",
            "e2bAdapterHandshakeOrdinary",
        ),
        (
            E2bAdapterLane::Control,
            "e2bAdapterSpawnControl",
            "e2bAdapterHandshakeControl",
        ),
        (
            E2bAdapterLane::Cleanup,
            "e2bAdapterSpawnCleanup",
            "e2bAdapterHandshakeCleanup",
        ),
    ] {
        let (_, records) = capture_timings(true, || {
            let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:handshake"}"#);
            let process = InstalledAdapterLauncher.launch(&config, lane).unwrap();
            require_capabilities(&process, lane, Duration::from_secs(10)).unwrap();
            let wrong_lane = if lane == E2bAdapterLane::Control {
                E2bAdapterLane::Ordinary
            } else {
                E2bAdapterLane::Control
            };
            assert_eq!(
                require_capabilities(&process, wrong_lane, Duration::from_secs(10)),
                Err(E2bRuntimeError::Rejected(
                    "e2b_runtime_capabilities_unsupported"
                ))
            );
            process.close().unwrap();
        });
        assert_eq!(records.len(), 5);
        assert_eq!(records[0]["phase"], spawn_phase);
        assert_eq!(records[0]["outcome"], "ok");
        assert_eq!(records[1]["phase"], "e2bAdapterCallCapabilities");
        assert_eq!(records[2]["phase"], handshake_phase);
        assert_eq!(records[2]["outcome"], "ok");
        assert_eq!(records[3]["phase"], "e2bAdapterCallCapabilities");
        assert_eq!(
            records[3]["outcome"], "ok",
            "transport success is independent of contract validation"
        );
        assert_eq!(records[4]["outcome"], "error");
        for record in records {
            assert_eq!(record["jobId"], "agent_run.lifecycle:handshake");
            assert!(record["elapsedMs"].as_f64().unwrap() >= 0.0);
        }
    }
    assert_eq!(
        fs::read_to_string(package.join("centaeris_e2b_adapter/requests"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    drop(root);
}

#[test]
fn adapter_transport_timings_preserve_failed_spawn_and_disabled_observation() {
    use crate::observations::capture_timings;
    let (_root, python, package) = paths();
    let config = E2bRuntimeConfig::new(
        profile(),
        configured_digest(),
        python,
        package,
        Duration::from_secs(10),
    )
    .unwrap();
    for enabled in [false, true] {
        let (result, records) = capture_timings(enabled, || {
            InstalledAdapterLauncher.launch(&config, E2bAdapterLane::Ordinary)
        });
        assert!(matches!(
            result,
            Err(E2bRuntimeError::Unavailable(
                "e2b_runtime_adapter_unavailable"
            ))
        ));
        if enabled {
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["phase"], "e2bAdapterSpawnOrdinary");
            assert_eq!(records[0]["outcome"], "error");
            assert!(records[0].get("jobId").is_none());
        } else {
            assert!(records.is_empty());
        }
    }
}

#[test]
fn running_limit_is_opt_in_bounded_and_requires_retained_lifetime() {
    let (_root, python, package) = paths();
    let configured = |selected| {
        E2bRuntimeConfig::new(
            selected,
            configured_digest(),
            python.clone(),
            package.clone(),
            Duration::from_secs(20),
        )
        .unwrap()
    };
    assert_eq!(configured(profile()).running_limit, None);
    assert!(configured(profile()).with_running_limit(Some(1)).is_err());
    let mut retained = profile();
    let EnvironmentProfile::E2b(value) = &mut retained else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    for invalid in [0, 65, u32::MAX] {
        assert!(configured(retained.clone())
            .with_running_limit(Some(invalid))
            .is_err());
    }
    for valid in [1, 64] {
        let selected = configured(retained.clone())
            .with_running_limit(Some(valid))
            .unwrap();
        assert_eq!(selected.running_limit, Some(valid));
        assert_eq!(selected.profile(), &retained);
    }
}

#[test]
fn idle_rotation_requires_explicit_retained_running_budget_and_positive_grace() {
    let (_root, python, package) = paths();
    let mut retained = profile();
    let EnvironmentProfile::E2b(value) = &mut retained else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    let configured = || {
        E2bRuntimeConfig::new(
            retained.clone(),
            configured_digest(),
            python.clone(),
            package.clone(),
            Duration::from_secs(20),
        )
        .unwrap()
    };
    assert!(
        configured().idle_pause_timeout.is_none(),
        "ordinary configuration performs no automatic scans"
    );
    assert!(configured()
        .with_idle_pause_timeout(Some(Duration::from_millis(1)))
        .is_err());
    assert!(configured()
        .with_running_limit(Some(1))
        .unwrap()
        .with_idle_pause_timeout(Some(Duration::ZERO))
        .is_err());
    let selected = configured()
        .with_running_limit(Some(1))
        .unwrap()
        .with_idle_pause_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    assert_eq!(
        selected.idle_pause_timeout,
        Some(Duration::from_millis(100))
    );
}
