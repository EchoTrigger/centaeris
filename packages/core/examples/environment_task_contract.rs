//! Emit typed Core environment-task examples consumed by non-Rust hosts.
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskEnvelope, EnvironmentTaskFence,
    EnvironmentTaskOutputChunk, EnvironmentTaskOwner, EnvironmentTaskRequest,
    EnvironmentTaskResponse, EnvironmentTaskResult, EnvironmentTaskScope, EnvironmentTaskSnapshot,
    EnvironmentTaskState, EnvironmentTaskStream, ENVIRONMENT_TASK_READ_BYTES,
    ENVIRONMENT_TASK_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/workspace/fixtures/environment_tasks/v1/maintenance.json")
}

fn exchange(request: EnvironmentTaskRequest, result: EnvironmentTaskResult) -> Value {
    let envelope = EnvironmentTaskEnvelope {
        schema: ENVIRONMENT_TASK_SCHEMA.into(),
        request,
    };
    envelope.validate().expect("valid fixture envelope");
    envelope
        .request
        .validate_authority()
        .expect("valid fixture authority");
    let response = EnvironmentTaskResponse::from_result(Ok(result));
    response.validate().expect("valid fixture response");
    let request_value = serde_json::to_value(&envelope).expect("serialize request");
    let response_value = serde_json::to_value(&response).expect("serialize response");
    assert_eq!(
        serde_json::from_value::<EnvironmentTaskEnvelope>(request_value.clone()).unwrap(),
        envelope
    );
    assert_eq!(
        serde_json::from_value::<EnvironmentTaskResponse>(response_value.clone()).unwrap(),
        response
    );
    json!({"request": request_value, "response": response_value})
}

fn task_result(
    task: EnvironmentTaskSnapshot,
    output: Vec<EnvironmentTaskOutputChunk>,
    next_cursor: u64,
    has_more: bool,
) -> EnvironmentTaskResult {
    EnvironmentTaskResult::Task {
        task,
        output,
        earliest_cursor: 0,
        next_cursor,
        gap: false,
        has_more,
    }
}

fn corpus() -> Value {
    let operation = "maintenance-00000000000000000000000000000001";
    let fence = EnvironmentTaskFence {
        environment_instance_id: "instance".into(),
        controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
            operation_id: operation.into(),
        },
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    fence.validate().expect("valid fixture fence");
    let scope = EnvironmentTaskScope::Operation {
        operation_id: operation.into(),
    };
    let start_id = fence.operation_id("execute");
    let task_id = fence.task_id(&start_id).expect("Core-owned task identity");
    let running = EnvironmentTaskSnapshot {
        task_id: task_id.clone(),
        operation_id: start_id.clone(),
        scope: scope.clone(),
        state: EnvironmentTaskState::Running,
        exit_code: None,
        cleanup_complete: false,
        output_complete: false,
    };
    let exited = EnvironmentTaskSnapshot {
        state: EnvironmentTaskState::Exited,
        exit_code: Some(0),
        cleanup_complete: true,
        output_complete: true,
        ..running.clone()
    };
    let first = EnvironmentTaskOutputChunk {
        cursor: 0,
        stream: EnvironmentTaskStream::Stdout,
        data: b"o".to_vec(),
    };
    let last = EnvironmentTaskOutputChunk {
        cursor: 1,
        stream: EnvironmentTaskStream::Stdout,
        data: b"k".to_vec(),
    };
    let start_request = EnvironmentTaskRequest::Start {
        fence: fence.clone(),
        operation_id: start_id,
        scope,
        program: "/bin/bash".into(),
        args: vec!["-lc".into(), "printf ok".into()],
        env: BTreeMap::new(),
        cwd: "/app".into(),
        timeout_ms: 5000,
    };
    let start = exchange(
        start_request.clone(),
        task_result(running, vec![], 0, false),
    );
    let mut command = start["request"]["request"].clone();
    let projection = command.as_object_mut().expect("Start object");
    for field in ["operation", "fence", "operationId", "scope"] {
        projection.remove(field);
    }
    json!({
        "schema": "centaeris.environmentTaskContract.v1",
        "command": command,
        "takeover": exchange(EnvironmentTaskRequest::Takeover {
            environment_instance_id: fence.environment_instance_id.clone(),
            controller_owner: fence.controller_owner.clone(),
            controller_epoch: fence.controller_epoch,
            controller_purpose: fence.controller_purpose.clone(),
        }, EnvironmentTaskResult::TakenOver { fence: fence.clone() }),
        "start": start,
        "replayedStart": exchange(start_request, task_result(
            exited.clone(), vec![first.clone(), last.clone()], 2, false)),
        "queryPages": [
            exchange(EnvironmentTaskRequest::Query {
                fence: fence.clone(), task_id: task_id.clone(), cursor: 0,
                max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
            }, task_result(exited.clone(), vec![first], 1, true)),
            exchange(EnvironmentTaskRequest::Query {
                fence: fence.clone(), task_id: task_id.clone(), cursor: 1,
                max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
            }, task_result(exited, vec![last], 2, false)),
        ],
        "endOperation": exchange(EnvironmentTaskRequest::EndOperation {
            fence: fence.clone(), owner_operation_id: operation.into(),
            operation_id: fence.operation_id("close"),
        }, EnvironmentTaskResult::OperationEnded {
            task_ids: vec![task_id], cleanup_complete: true,
        }),
    })
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => println!("{}", serde_json::to_string_pretty(&corpus()).unwrap()),
        [check] if check == "--check" => check_fixture(),
        _ => panic!("usage: environment_task_contract [--check]"),
    }
}

fn check_fixture() {
    let checked_in: Value = serde_json::from_slice(
        &std::fs::read(fixture_path()).expect("checked-in environment task corpus"),
    )
    .expect("environment task corpus");
    assert_eq!(
        corpus(),
        checked_in,
        "regenerate with cargo run -p centaeris-core --example environment_task_contract"
    );
}

#[test]
fn checked_in_corpus_matches_core_construction_and_validation() {
    check_fixture();
}

#[test]
fn task_requests_reject_kind_discriminator_alias() {
    let corpus = corpus();
    for case in ["takeover", "start", "replayedStart", "endOperation"]
        .into_iter()
        .map(|name| &corpus[name])
        .chain(corpus["queryPages"].as_array().unwrap())
    {
        let mut envelope = case["request"].clone();
        let request = envelope["request"].as_object_mut().unwrap();
        let operation = request.remove("operation").unwrap();
        request.insert("kind".into(), operation);
        assert!(serde_json::from_value::<EnvironmentTaskEnvelope>(envelope).is_err());
    }
}
