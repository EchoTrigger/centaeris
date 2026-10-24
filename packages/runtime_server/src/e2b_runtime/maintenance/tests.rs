use super::*;
use crate::e2b_execution_host::sdk_transport::tests::{
    process, process_with_capture_outcome, requests,
};
fn profile() -> EnvironmentProfile {
    let mut value = serde_json::to_value(crate::e2b_execution_host::tests::profile()).unwrap();
    value["workspaceInitialization"] = json!("taskImage");
    value["lifetime"] = json!("retained");
    value["user"] = json!("root");
    value["workspaceRoot"] = json!("/app");
    serde_json::from_value(value).unwrap()
}
fn auth(request: &DirectRequest) -> ProviderMaintenanceAuthorization {
    let mut value:ProviderMaintenanceAuthorization=serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json"))).unwrap();
    let encoded =
        crate::agent_run_authorization::canonical_json(serde_json::to_value(request).unwrap())
            .unwrap();
    value.payload.request_digest = format!("sha256:{:x}", Sha256::digest(encoded.as_bytes()));
    value
}
fn binding() -> E2bSandboxBinding {
    crate::e2b_execution_host::tests::binding()
}
#[test]
fn native_maintenance_command_preserves_root_cwd_argv_deadline_and_full_output_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let data = b"\0\xfffull output";
    let process = process_with_capture_outcome(&log, data, "maintenanceCommand");
    let request = DirectRequest::Command(DirectCommand {
        program: "/usr/bin/env".into(),
        args: vec![
            "--".into(),
            "FLAG=kept".into(),
            "/bin/bash".into(),
            "-lc".into(),
            "printf task".into(),
        ],
        cwd: "/app".into(),
        timeout_ms: 900000,
    });
    request.validate(&auth(&request), &[]).unwrap();
    assert_eq!(
        execute_native(
            process,
            &profile(),
            &binding(),
            &request,
            &[],
            Duration::from_secs(20)
        )
        .unwrap(),
        DirectOutput::Command {
            exit_code: 0,
            stdout_bytes_base64: base64::engine::general_purpose::STANDARD.encode(data),
            stdout_byte_length: data.len() as u64,
            stderr_bytes_base64: String::new(),
            stderr_byte_length: 0
        }
    );
    let seen = requests(&log);
    assert_eq!(
        seen.iter()
            .map(|r| r["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["attach", "runCommand", "waitCommand"]
    );
    assert_eq!(seen[1]["payload"]["user"], "root");
    assert_eq!(seen[1]["payload"]["cwd"], "/app");
    assert_eq!(seen[1]["payload"]["timeoutSeconds"], 900);
    assert_eq!(seen[2]["payload"]["timeoutSeconds"], 905);
}
#[test]
fn native_maintenance_files_use_sdk_binary_ports_without_core_atomic_or_snapshot_helpers() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let data = b"\0\xffbinary file";
    let process = process(&log, data);
    let profile = profile();
    let binding = binding();
    let write = DirectRequest::Filesystem(NativeFileFrame {
        request: NativeFileOperation::WriteFile {
            path: "/tests/result.bin".into(),
        },
        content_length: data.len() as u64,
        content_digest: format!("sha256:{:x}", Sha256::digest(data)),
    });
    write.validate(&auth(&write), data).unwrap();
    assert_eq!(
        execute_native(
            process.clone(),
            &profile,
            &binding,
            &write,
            data,
            Duration::from_secs(20)
        )
        .unwrap(),
        DirectOutput::FileWritten {
            byte_length: data.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(data))
        }
    );
    let read = DirectRequest::Filesystem(NativeFileFrame {
        request: NativeFileOperation::ReadFile {
            path: "/tests/result.bin".into(),
            max_bytes: MAX_NATIVE_READ_BYTES,
        },
        content_length: 0,
        content_digest: format!("sha256:{:x}", Sha256::digest([])),
    });
    read.validate(&auth(&read), &[]).unwrap();
    assert_eq!(
        execute_native(
            process.clone(),
            &profile,
            &binding,
            &read,
            &[],
            Duration::from_secs(20)
        )
        .unwrap(),
        DirectOutput::FileRead {
            data_base64: base64::engine::general_purpose::STANDARD.encode(data),
            byte_length: data.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(data))
        }
    );
    let list = DirectRequest::Filesystem(NativeFileFrame {
        request: NativeFileOperation::ListDirectory {
            path: "/tests".into(),
            recursive: true,
            max_entries: 100,
        },
        content_length: 0,
        content_digest: format!("sha256:{:x}", Sha256::digest([])),
    });
    list.validate(&auth(&list), &[]).unwrap();
    assert!(
        matches!(execute_native(process,&profile,&binding,&list,&[],Duration::from_secs(20)).unwrap(),DirectOutput::DirectoryListed{entries} if entries.len()==1 && entries[0].path=="/tests/result.bin")
    );
    let seen = requests(&log);
    assert_eq!(
        seen.iter()
            .map(|r| r["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "attach",
            "uploadFile",
            "attach",
            "downloadFile",
            "attach",
            "listDirectory"
        ]
    );
    assert!(seen
        .iter()
        .filter(|r| matches!(
            r["operation"].as_str(),
            Some("uploadFile" | "downloadFile" | "listDirectory")
        ))
        .all(|r| r["payload"]["user"] == "root"));
}
#[test]
fn native_maintenance_request_rejects_changed_bytes_and_unimplemented_core_atomic_fields() {
    let request = DirectRequest::Filesystem(NativeFileFrame {
        request: NativeFileOperation::WriteFile {
            path: "/tests/file".into(),
        },
        content_length: 1,
        content_digest: format!("sha256:{:x}", Sha256::digest(b"a")),
    });
    assert!(request.validate(&auth(&request), b"b").is_err());
    assert!(serde_json::from_value::<DirectRequest>(json!({"request":{"kind":"writeFile","path":"/tests/file","createOnly":true},"contentLength":0,"contentDigest":format!("sha256:{:x}",Sha256::digest([]))})).is_err());
}
