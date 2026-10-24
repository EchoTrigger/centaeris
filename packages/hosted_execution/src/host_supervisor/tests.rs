use super::*;
use serde_json::json;

#[test]
fn runsc_configuration_selects_a_native_backend_and_requires_the_sealed_runner() {
    let value = json!({
        "schema":HOST_SUPERVISOR_SCHEMA,
        "socketPath":"/run/centaeris-host/control.sock",
        "controlRoot":"/var/lib/centaeris-host/control",
        "outputRoot":"/var/lib/centaeris-host/output",
        "ociRuntime":"runsc",
        "runtimePath":"/usr/bin/runsc",
        "runtimeRoot":"/run/docker/runtime-runc/moby",
        "dockerPath":"/usr/bin/docker",
        "dockerSocket":"/var/run/docker.sock",
        "controllerUid":0,
        "helperPath":"/opt/centaeris/host/execution_agent"
    });
    let config: HostSupervisorConfig = serde_json::from_value(value.clone()).unwrap();
    config.validate().unwrap();
    let mut missing = value.clone();
    missing["helperPath"] = json!(null);
    let config: HostSupervisorConfig = serde_json::from_value(missing).unwrap();
    assert_eq!(config.validate().unwrap_err(), "host_runsc_runner_required");
    let mut unsupported = value;
    unsupported["ociRuntime"] = json!("automatic");
    assert!(serde_json::from_value::<HostSupervisorConfig>(unsupported).is_err());
}

#[test]
fn managed_helper_configuration_needs_only_its_absolute_installation_path() {
    let configuration: HostSupervisorConfig = serde_json::from_value(json!({
        "schema": HOST_SUPERVISOR_SCHEMA,
        "socketPath": "/run/centaeris-host/control.sock",
        "controlRoot": "/var/lib/centaeris-host/control",
        "outputRoot": "/var/lib/centaeris-host/output",
        "ociRuntime": "runc",
        "runtimePath": "/usr/sbin/runc",
        "runtimeRoot": "/run/docker/runtime-runc/moby",
        "dockerPath": "/usr/bin/docker",
        "dockerSocket": "/var/run/docker.sock",
        "controllerUid": 0,
        "helperPath": "/usr/libexec/centaeris/execution_agent"
    }))
    .unwrap();
    configuration.validate().unwrap();
}

#[test]
fn retirement_roundtrips_the_exact_current_target_and_validates_receipts() {
    let target = HostLaunchTarget {
        environment_instance_id: format!("instance_{}", "a".repeat(64)),
        container_id: "b".repeat(64),
        environment_id: "environment".into(),
        profile_digest: format!("sha256:{}", "c".repeat(64)),
        image_digest: format!("sha256:{}", "d".repeat(64)),
    };
    let request = HostSupervisorEnvelope {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        request: HostSupervisorRequest::Retire {
            target: target.clone(),
        },
    };
    let wire = serde_json::to_value(request).unwrap();
    assert_eq!(wire["request"]["operation"], "retire");
    let parsed: HostSupervisorEnvelope = serde_json::from_value(wire).unwrap();
    parsed.validate().unwrap();
    let response = HostSupervisorResponse::Success {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        result: HostSupervisorResult::Retired {
            target: target.clone(),
        },
    };
    response.validate().unwrap();
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["result"]["kind"], "retired");
    assert_eq!(wire["result"]["target"]["containerId"], target.container_id);
    let mut malformed = target;
    malformed.environment_instance_id = "../other".into();
    assert!(HostSupervisorResponse::Success {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        result: HostSupervisorResult::Retired { target: malformed }
    }
    .validate()
    .is_err());
}
fn target() -> HostLaunchTarget {
    HostLaunchTarget {
        environment_instance_id: format!("instance_{}", "1".repeat(64)),
        container_id: "2".repeat(64),
        environment_id: "environment-a".into(),
        profile_digest: format!("sha256:{}", "3".repeat(64)),
        image_digest: format!("sha256:{}", "4".repeat(64)),
    }
}

#[test]
fn trusted_registration_is_strict_and_task_requests_cannot_choose_host_paths() {
    let envelope: HostSupervisorEnvelope = serde_json::from_value(json!({
        "schema":HOST_SUPERVISOR_SCHEMA,"request":{"operation":"register","target":target()}
    }))
    .unwrap();
    envelope.validate().unwrap();
    for field in ["runtimeRoot", "controlRoot", "capabilities", "process"] {
        let mut value = serde_json::to_value(&envelope).unwrap();
        value["request"]["target"][field] = json!("/host");
        assert!(serde_json::from_value::<HostSupervisorEnvelope>(value).is_err());
    }
    let mut wrong = envelope;
    wrong.schema = "environment.host.v0".into();
    assert_eq!(wrong.validate().unwrap_err(), "host_schema_unsupported");
}

#[test]
fn host_target_rejects_path_identifiers_and_mutable_image_tags() {
    target().validate().unwrap();
    let mut value = target();
    value.container_id = "../another-container".into();
    assert!(value.validate().is_err());
    value = target();
    value.image_digest = "ubuntu:latest".into();
    assert!(value.validate().is_err());
    value = target();
    value.environment_instance_id = "../control".into();
    assert!(value.validate().is_err());
}

#[test]
fn host_configuration_keeps_all_control_roots_outside_each_other() {
    let mut config = HostSupervisorConfig {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        socket_path: "/run/centaeris-host/control.sock".into(),
        control_root: "/var/lib/centaeris-host/control".into(),
        output_root: "/var/lib/centaeris-host/output".into(),
        oci_runtime: HostOciRuntime::Runc,
        runtime_path: "/usr/bin/runc".into(),
        runtime_root: "/run/docker/runtime-runc/moby".into(),
        docker_path: "/usr/bin/docker".into(),
        docker_socket: "/var/run/docker.sock".into(),
        controller_uid: 0,
        helper_path: None,
    };
    config.validate().unwrap();
    config.output_root = format!("{}/output", config.control_root);
    assert_eq!(config.validate().unwrap_err(), "host_control_paths_overlap");
}

#[test]
fn deployment_example_uses_the_current_strict_host_contract() {
    let config: HostSupervisorConfig =
        serde_json::from_str(include_str!("../../deploy/host-supervisor.example.json")).unwrap();
    config.validate().unwrap();
}

#[test]
fn task_process_preserves_root_identity_and_all_oci_process_security() {
    let baseline = json!({"terminal":false,"user":{"uid":0,"gid":0,"additionalGids":[5]},
        "args":["/keeper"],"cwd":"/app","env":["PATH=/bin","HOME=/root"],
        "noNewPrivileges":true,"apparmorProfile":"docker-default","selinuxLabel":"label",
        "capabilities":{"bounding":["CAP_CHOWN"],"effective":["CAP_CHOWN"]},
        "rlimits":[{"type":"RLIMIT_NOFILE","hard":1024,"soft":1024}]});
    let mut expected = baseline.clone();
    expected["args"] = json!(["/bin/sh", "-c", "echo ready"]);
    expected["cwd"] = json!("/project");
    assert_eq!(
        task_process(
            &baseline,
            "/bin/sh",
            &["-c".into(), "echo ready".into()],
            "/project",
            &Default::default()
        )
        .unwrap(),
        expected
    );
    assert!(task_process(&baseline, "/bin/true", &[], "relative", &Default::default()).is_err());
    let mut unsafe_base = baseline;
    unsafe_base["noNewPrivileges"] = json!(false);
    assert!(task_process(&unsafe_base, "/bin/true", &[], "/", &Default::default()).is_err());
}

#[test]
fn oci_default_terminal_is_non_interactive_without_dropping_security() {
    let baseline = json!({"user":{"uid":0,"gid":0},"args":["/keeper"],"cwd":"/","env":[],"noNewPrivileges":true});
    let process = task_process(&baseline, "/bin/true", &[], "/", &Default::default()).unwrap();
    assert_eq!(process["user"], baseline["user"]);
    assert_eq!(process["noNewPrivileges"], true);
    assert_ne!(process["terminal"], true);
}

#[test]
fn native_task_environment_overrides_values_without_wrapping_the_program_or_identity() {
    let baseline = json!({"user":{"uid":10001,"gid":10001},"args":["/keeper"],"cwd":"/workspace","env":["PATH=/bin","PROFILE=kept"],"noNewPrivileges":true,"apparmorProfile":"docker-default"});
    let env = [
        ("PATH".into(), "/custom/bin".into()),
        ("MCP_SECRET".into(), "value with spaces".into()),
    ]
    .into();
    let process = task_process(
        &baseline,
        "/app/stdio",
        &["argument".into()],
        "/workspace",
        &env,
    )
    .unwrap();
    assert_eq!(process["args"], json!(["/app/stdio", "argument"]));
    assert_eq!(
        process["env"],
        json!([
            "MCP_SECRET=value with spaces",
            "PATH=/custom/bin",
            "PROFILE=kept"
        ])
    );
    assert_eq!(process["user"], baseline["user"]);
    assert_eq!(process["apparmorProfile"], baseline["apparmorProfile"]);
    assert_eq!(process["noNewPrivileges"], baseline["noNewPrivileges"]);
}

#[test]
fn bare_program_is_resolved_only_by_the_container_process_path() {
    let baseline = json!({"user":{"uid":10001,"gid":10001},"cwd":"/","env":["PATH=/custom/bin"],"noNewPrivileges":true});
    let process = task_process(
        &baseline,
        "node",
        &["literal ; argument".into()],
        "/workspace",
        &Default::default(),
    )
    .unwrap();
    assert_eq!(process["args"], json!(["node", "literal ; argument"]));
    assert_eq!(process["env"], baseline["env"]);
    for invalid in [
        "",
        "../node",
        "./node",
        "bin/node",
        "bad\nprogram",
        "node\0",
    ] {
        assert!(task_process(&baseline, invalid, &[], "/workspace", &Default::default()).is_err());
    }
    assert!(task_process(
        &baseline,
        &"x".repeat(4097),
        &[],
        "/workspace",
        &Default::default()
    )
    .is_err());
}

#[test]
fn workspace_helper_context_is_bound_to_profile_identity_and_independent_of_keeper_cwd() {
    let mut baseline = json!({"cwd":"/", "user":{"uid":10001,"gid":10001}, "env":["CENTAERIS_WORKSPACE_ROOT=/workspace","CENTAERIS_WORKSPACE_USER=10001:10001"]});
    assert_eq!(
        workspace_context(&baseline).unwrap(),
        ("/workspace".into(), "10001:10001".into())
    );
    baseline["env"][1] = json!("CENTAERIS_WORKSPACE_USER=0:0");
    assert!(workspace_context(&baseline).is_err());
    baseline["env"] = json!([
        "CENTAERIS_WORKSPACE_ROOT=/workspace",
        "CENTAERIS_WORKSPACE_ROOT=/escape",
        "CENTAERIS_WORKSPACE_USER=10001:10001"
    ]);
    assert!(workspace_context(&baseline).is_err());
}

#[test]
fn task_routing_rejects_path_identifiers_before_reading_registration() {
    let value = json!({"schema":HOST_SUPERVISOR_SCHEMA,"request":{"operation":"task","envelope":{
        "schema":centaeris_core::execution::ENVIRONMENT_TASK_SCHEMA,"request":{"operation":"list","fence":{
            "environmentInstanceId":"../elsewhere","controllerOwner":{"kind":"agentRun","runId":"run"},
            "controllerEpoch":1,"controllerPurpose":"execute"}}}}});
    let envelope: HostSupervisorEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(envelope.validate().unwrap_err(), "host_target_invalid");
}

#[test]
fn leader_exit_does_not_destroy_a_background_task_domain() {
    use centaeris_core::execution::*;
    let mut task = EnvironmentTaskSnapshot {
        task_id: "task".into(),
        operation_id: "1:start".into(),
        scope: EnvironmentTaskScope::Environment,
        state: EnvironmentTaskState::Running,
        exit_code: None,
        cleanup_complete: false,
        output_complete: false,
    };
    physical_snapshot(&mut task, Some(Some(0)), false, false);
    assert_eq!(task.state, EnvironmentTaskState::Exited);
    assert_eq!(task.exit_code, Some(0));
    assert!(!task.cleanup_complete);
    assert!(!task.output_complete);
    physical_snapshot(&mut task, Some(Some(0)), true, false);
    assert!(task.output_complete);
    assert!(!task.cleanup_complete);
    physical_snapshot(&mut task, Some(Some(0)), true, true);
    assert!(task.cleanup_complete);
}

#[test]
fn task_projection_keeps_attested_workspace_identity_separate_from_request_environment() {
    let baseline = json!({
        "user": {"uid": 10001, "gid": 10001, "additionalGids": []},
        "args": ["/keeper"], "cwd": "/", "terminal": false,
        "noNewPrivileges": true,
        "env": ["PATH=/usr/bin:/bin", "CENTAERIS_WORKSPACE_ROOT=/mnt/data",
            "CENTAERIS_WORKSPACE_USER=10001:10001"]
    });
    let frozen = baseline.clone();
    let requested_environment = [
        (
            "CENTAERIS_WORKSPACE_ROOT".into(),
            "/tmp/request-selected".into(),
        ),
        ("CENTAERIS_WORKSPACE_USER".into(), "0:0".into()),
    ]
    .into();
    let args = ["literal ; argument".into(), "$(not-a-shell)".into()];
    let projected = task_process(
        &baseline,
        "/bin/printf",
        &args,
        "/mnt/data",
        &requested_environment,
    )
    .unwrap();
    assert_eq!(
        projected["args"],
        json!(["/bin/printf", "literal ; argument", "$(not-a-shell)"])
    );
    assert_eq!(projected["cwd"], "/mnt/data");
    assert_eq!(projected["user"], frozen["user"]);
    assert_eq!(projected["noNewPrivileges"], true);
    assert_eq!(baseline, frozen);
    assert_eq!(
        workspace_context(&baseline).unwrap(),
        ("/mnt/data".into(), "10001:10001".into())
    );
}
