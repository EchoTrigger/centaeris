use super::*;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskOutputChunk, EnvironmentTaskOwner,
    EnvironmentTaskScope, EnvironmentTaskSnapshot, EnvironmentTaskState, EnvironmentTaskStream,
    ENVIRONMENT_TASK_READ_BYTES,
};
use centaeris_core::tool::DynamicToolRegistry;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

struct FakePort {
    fence: EnvironmentTaskFence,
    current: Arc<Mutex<EnvironmentTaskFence>>,
    fence_calls: AtomicUsize,
    requests: Mutex<Vec<EnvironmentTaskRequest>>,
    replies: Mutex<VecDeque<Result<EnvironmentTaskResult, String>>>,
    revoke_after_query: bool,
}
impl EnvironmentTaskPort for FakePort {
    fn environment_task_fence(&self) -> Result<EnvironmentTaskFence, String> {
        self.fence_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.fence.clone())
    }
    fn environment_task_rpc(
        &self,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        self.requests.lock().unwrap().push(request.clone());
        let fence = match &request {
            EnvironmentTaskRequest::Start { fence, .. }
            | EnvironmentTaskRequest::List { fence }
            | EnvironmentTaskRequest::Query { fence, .. }
            | EnvironmentTaskRequest::Terminate { fence, .. } => fence,
            _ => panic!("model provider attempted a controller operation"),
        };
        if fence != &*self.current.lock().unwrap() {
            return Err("task_controller_fenced".into());
        }
        if let EnvironmentTaskRequest::Query { max_bytes, .. } = &request {
            assert!((4096..=ENVIRONMENT_TASK_READ_BYTES as u32).contains(max_bytes));
            if self.revoke_after_query {
                self.current.lock().unwrap().controller_epoch += 1;
            }
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("one bounded RPC per planned operation")
    }
}
fn fence(run: &str, epoch: u64) -> EnvironmentTaskFence {
    EnvironmentTaskFence {
        environment_instance_id: "private-instance".into(),
        controller_owner: EnvironmentTaskOwner::AgentRun { run_id: run.into() },
        controller_epoch: epoch,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    }
}
fn port(
    run: &str,
    epoch: u64,
    replies: Vec<Result<EnvironmentTaskResult, String>>,
) -> Arc<FakePort> {
    let fence = fence(run, epoch);
    Arc::new(FakePort {
        current: Arc::new(Mutex::new(fence.clone())),
        fence,
        fence_calls: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        replies: Mutex::new(replies.into()),
        revoke_after_query: false,
    })
}
fn task(scope: EnvironmentTaskScope) -> EnvironmentTaskSnapshot {
    EnvironmentTaskSnapshot {
        task_id: "task-1".into(),
        operation_id: "private-operation".into(),
        scope,
        state: EnvironmentTaskState::Running,
        exit_code: None,
        cleanup_complete: false,
        output_complete: false,
    }
}
fn page(scope: EnvironmentTaskScope) -> EnvironmentTaskResult {
    EnvironmentTaskResult::Task {
        task: task(scope),
        output: vec![EnvironmentTaskOutputChunk {
            cursor: 7,
            stream: EnvironmentTaskStream::Stdout,
            data: vec![b'a', 0xff],
        }],
        earliest_cursor: 7,
        next_cursor: 9,
        gap: true,
        has_more: true,
    }
}
fn request(name: &str, id: &str, args: Value) -> DynamicToolProviderRequest {
    let registry = DynamicToolRegistry::from_contracts(retained_task_contracts()).unwrap();
    DynamicToolProviderRequest {
        tool_name: name.into(),
        tool_call_id: id.into(),
        args_json: args.to_string(),
        contract: registry.find_contract(name).unwrap(),
        cancellation_probe: None,
    }
}
fn call(port: Arc<FakePort>, name: &str, id: &str, args: Value) -> DynamicToolProviderResponse {
    let provider = RetainedTaskToolProvider::new(port);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(provider.execute(request(name, id, args)))
        .unwrap()
}
fn start_args() -> Value {
    json!({"program":"/bin/sh","args":["-c","echo hi"],"cwd":"/mnt/data","timeout_ms":0})
}

#[test]
fn model_can_read_task_identity_and_output_from_tool_content() {
    let reply = call(
        port(
            "run-a",
            1,
            vec![Ok(page(EnvironmentTaskScope::Environment))],
        ),
        "background_task_start",
        "start-1",
        start_args(),
    );
    let content: Value = serde_json::from_str(&reply.content).unwrap();
    assert_eq!(content["taskId"], "task-1");
    assert_eq!(content["nextCursor"], 9);
    assert_eq!(content["gap"], true);
    assert_eq!(content["output"][0]["text"], "a\u{fffd}");
    assert_eq!(content, reply.details);
    assert!(!reply.content.contains("private-operation"));
}

#[test]
fn contracts_expose_only_four_strict_model_tools() {
    let contracts = retained_task_contracts();
    assert_eq!(
        contracts
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        [
            "background_task_start",
            "background_task_list",
            "background_task_query",
            "background_task_stop"
        ]
    );
    for contract in contracts {
        assert_eq!(contract.input_schema["additionalProperties"], false);
        for key in contract.input_schema["properties"]
            .as_object()
            .unwrap()
            .keys()
        {
            assert!(key.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }
}

#[test]
fn start_replay_has_stable_run_scoped_identity_and_preserves_initial_output() {
    let p = port(
        "run-a",
        1,
        vec![Ok(page(EnvironmentTaskScope::Environment)); 3],
    );
    let response = call(p.clone(), "background_task_start", "call-1", start_args());
    assert!(!response.is_error);
    assert_eq!(response.details["taskId"], "task-1");
    assert_eq!(response.details["output"][0]["text"], "a\u{fffd}");
    assert_eq!(response.details["output"][0]["byteLength"], 2);
    assert_eq!(response.details["output"][0]["lossy"], true);
    assert_eq!(response.details["gap"], true);
    assert_eq!(response.details["hasMore"], true);
    assert_eq!(response.details["nextCursor"], 9);
    call(p.clone(), "background_task_start", "call-1", start_args());
    call(p.clone(), "background_task_start", "call-2", start_args());
    let operations = p
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| match r {
            EnvironmentTaskRequest::Start {
                operation_id,
                scope,
                program,
                args,
                cwd,
                timeout_ms,
                ..
            } => {
                assert_eq!(*scope, EnvironmentTaskScope::Environment);
                assert_eq!(program, "/bin/sh");
                assert_eq!(args, &["-c", "echo hi"]);
                assert_eq!(cwd, "/mnt/data");
                assert_eq!(*timeout_ms, 0);
                operation_id.clone()
            }
            _ => panic!("not start"),
        })
        .collect::<Vec<_>>();
    assert_eq!(operations[0], operations[1]);
    assert_ne!(operations[0], operations[2]);
    let b = port(
        "run-b",
        2,
        vec![Ok(page(EnvironmentTaskScope::Environment))],
    );
    call(b.clone(), "background_task_start", "call-1", start_args());
    assert!(
        matches!(&b.requests.lock().unwrap()[0], EnvironmentTaskRequest::Start{operation_id,..} if operation_id != &operations[0])
    );
    let wire = response.details.to_string();
    for private in [
        "private-instance",
        "private-operation",
        "run-a",
        "controller",
        "scope",
    ] {
        assert!(!wire.contains(private));
    }
}

#[test]
fn next_controller_queries_same_task_and_old_controller_is_rejected() {
    let a = port(
        "run-a",
        1,
        vec![Ok(page(EnvironmentTaskScope::Environment))],
    );
    let started = call(a.clone(), "background_task_start", "start-a", start_args());
    let mut b = port(
        "run-b",
        2,
        vec![Ok(page(EnvironmentTaskScope::Environment))],
    );
    Arc::get_mut(&mut b).unwrap().current = a.current.clone();
    *a.current.lock().unwrap() = fence("run-b", 2);
    let queried = call(
        b,
        "background_task_query",
        "query-b",
        json!({"task_id":"task-1","cursor":7}),
    );
    assert_eq!(started.details["taskId"], queried.details["taskId"]);
    assert_eq!(queried.details["earliestCursor"], 7);
    let denied = call(
        a,
        "background_task_query",
        "old-query",
        json!({"task_id":"task-1"}),
    );
    assert!(denied.is_error);
    assert_eq!(denied.details["errorCode"], "background_task_forbidden");
}

#[test]
fn invalid_or_authority_injected_input_never_reaches_port() {
    let p = port("run-a", 1, vec![]);
    for name in [
        "background_task_start",
        "background_task_list",
        "background_task_query",
        "background_task_stop",
    ] {
        let base = match name {
            "background_task_start" => start_args(),
            "background_task_list" => json!({}),
            _ => json!({"task_id":"task-1"}),
        };
        for key in [
            "scope",
            "run_id",
            "instance_id",
            "controller_id",
            "controller_epoch",
            "fence",
            "operation_id",
            "takeover",
            "end_run",
            "max_bytes",
        ] {
            let mut args = base.clone();
            args[key] = json!("attacker-value");
            assert!(call(p.clone(), name, "call", args).is_error);
        }
    }
    for args in [
        json!({"program":""}),
        json!({"program":"sh","cwd":"/mnt/data/../etc"}),
        json!({"program":"sh","timeout_ms":86400001}),
        json!({"program":"sh","args":["a\u{0}"]}),
    ] {
        assert!(call(p.clone(), "background_task_start", "call", args).is_error);
    }
    assert_eq!(p.fence_calls.load(Ordering::SeqCst), 0);
    assert!(p.requests.lock().unwrap().is_empty());
}

#[test]
fn uncertain_start_is_not_retried_and_raw_host_error_is_hidden() {
    let p = port(
        "run-a",
        1,
        vec![Err(
            "docker private-instance token=private-value unavailable".into(),
        )],
    );
    let response = call(
        p.clone(),
        "background_task_start",
        "stable-call",
        start_args(),
    );
    assert!(response.is_error);
    assert_eq!(response.details["state"], "unknown");
    assert_eq!(response.details["errorCode"], "background_task_unknown");
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert!(!format!("{}{}", response.content, response.details).contains("private"));
}

#[test]
fn list_and_query_hide_run_scoped_control_tasks() {
    let run = EnvironmentTaskScope::Run {
        run_id: "private-run".into(),
    };
    let p = port(
        "run-a",
        1,
        vec![
            Ok(EnvironmentTaskResult::Listed {
                tasks: vec![task(EnvironmentTaskScope::Environment), task(run.clone())],
            }),
            Ok(page(run)),
        ],
    );
    let listed = call(p.clone(), "background_task_list", "list", json!({}));
    assert_eq!(listed.details["tasks"].as_array().unwrap().len(), 1);
    let query = call(
        p.clone(),
        "background_task_query",
        "query",
        json!({"task_id":"task-1"}),
    );
    assert!(query.is_error);
    assert!(!query.details.to_string().contains("private-run"));
}

#[test]
fn model_tools_cannot_use_a_maintenance_owner_as_run_authority() {
    let mut p = port(
        "run-a",
        1,
        vec![Ok(page(EnvironmentTaskScope::Environment))],
    );
    let maintenance = EnvironmentTaskFence {
        controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
            operation_id: "run-a".into(),
        },
        ..fence("run-a", 1)
    };
    let mutable = Arc::get_mut(&mut p).unwrap();
    mutable.fence = maintenance.clone();
    *mutable.current.lock().unwrap() = maintenance;
    let response = call(
        p.clone(),
        "background_task_start",
        "stable-call",
        start_args(),
    );
    assert!(response.is_error);
    assert_eq!(response.details["errorCode"], "background_task_forbidden");
    assert!(p.requests.lock().unwrap().is_empty());
}

#[test]
fn stop_checks_environment_scope_and_reauthorizes_termination_with_same_fence() {
    let mut p = port(
        "run-a",
        1,
        vec![Ok(page(EnvironmentTaskScope::Environment))],
    );
    Arc::get_mut(&mut p).unwrap().revoke_after_query = true;
    let response = call(
        p.clone(),
        "background_task_stop",
        "stop",
        json!({"task_id":"task-1"}),
    );
    assert!(response.is_error);
    assert_eq!(response.details["errorCode"], "background_task_forbidden");
    let calls = p.requests.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        matches!((&calls[0],&calls[1]),(EnvironmentTaskRequest::Query{fence:a,..},EnvironmentTaskRequest::Terminate{fence:b,..}) if a==b)
    );
    let denied = port(
        "run-a",
        1,
        vec![Ok(page(EnvironmentTaskScope::Run {
            run_id: "run-a".into(),
        }))],
    );
    assert!(
        call(
            denied.clone(),
            "background_task_stop",
            "stop",
            json!({"task_id":"task-1"})
        )
        .is_error
    );
    assert_eq!(denied.requests.lock().unwrap().len(), 1);
}

#[test]
fn stop_only_terminates_requested_task_and_keeps_incomplete_cleanup_visible() {
    let p = port(
        "run-a",
        1,
        vec![
            Ok(page(EnvironmentTaskScope::Environment)),
            Ok(EnvironmentTaskResult::Mutated {
                task: task(EnvironmentTaskScope::Environment),
            }),
        ],
    );
    let response = call(
        p.clone(),
        "background_task_stop",
        "stop",
        json!({"task_id":"task-1"}),
    );
    assert!(!response.is_error);
    assert_eq!(response.details["cleanupComplete"], false);
    assert!(
        matches!(&p.requests.lock().unwrap()[1],EnvironmentTaskRequest::Terminate{task_id,..} if task_id=="task-1")
    );
}

#[test]
fn forged_tool_identity_and_invalid_task_cursor_never_obtain_authority() {
    let p = port("run-a", 1, vec![]);
    for field in ["name", "provider", "dynamic", "schema", "call"] {
        let mut request = request("background_task_list", "call", json!({}));
        match field {
            "name" => request.contract.name = "background_task_start".into(),
            "provider" => request.contract.provider_id = Some("forged".into()),
            "dynamic" => request.contract.dynamic = false,
            "schema" => request.contract.schema_hash = None,
            _ => request.tool_call_id.clear(),
        }
        assert!(execute_task(request, p.as_ref()).is_error);
    }
    for args in [
        json!({"task_id":""}),
        json!({"task_id":"../private"}),
        json!({"task_id":"task-1","cursor":-1}),
    ] {
        assert!(call(p.clone(), "background_task_query", "call", args).is_error);
    }
    assert_eq!(p.fence_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn valid_utf8_status_and_empty_output_keep_exact_cursor_and_loss_flags() {
    let mut snapshot = task(EnvironmentTaskScope::Environment);
    snapshot.state = EnvironmentTaskState::Exited;
    snapshot.exit_code = Some(7);
    snapshot.cleanup_complete = true;
    snapshot.output_complete = true;
    let first = EnvironmentTaskResult::Task {
        task: snapshot.clone(),
        output: vec![EnvironmentTaskOutputChunk {
            cursor: 10,
            stream: EnvironmentTaskStream::Stderr,
            data: "完成".as_bytes().into(),
        }],
        earliest_cursor: 10,
        next_cursor: 16,
        gap: false,
        has_more: false,
    };
    let second = EnvironmentTaskResult::Task {
        task: snapshot,
        output: vec![],
        earliest_cursor: 16,
        next_cursor: 16,
        gap: true,
        has_more: false,
    };
    let p = port("run-a", 1, vec![Ok(first), Ok(second)]);
    let response = call(
        p.clone(),
        "background_task_query",
        "first",
        json!({"task_id":"task-1","cursor":10}),
    );
    assert_eq!(
        response.details["output"][0],
        json!({"cursor":10,"stream":"stderr","text":"完成","byteLength":6,"lossy":false})
    );
    assert_eq!(response.details["state"], "exited");
    assert_eq!(response.details["exitCode"], 7);
    assert_eq!(response.details["cleanupComplete"], true);
    assert_eq!(response.details["outputComplete"], true);
    assert_eq!(response.details["gap"], false);
    let empty = call(
        p,
        "background_task_query",
        "second",
        json!({"task_id":"task-1","cursor":16}),
    );
    assert_eq!(empty.details["output"], json!([]));
    assert_eq!(empty.details["gap"], true);
    assert_eq!(empty.details["nextCursor"], 16);
    assert_eq!(empty.details["hasMore"], false);
}

#[test]
fn start_defaults_are_bounded_and_wrong_reply_cannot_leak_control_identity() {
    let p = port(
        "run-a",
        1,
        vec![Ok(EnvironmentTaskResult::TakenOver {
            fence: fence("private-controller", 77),
        })],
    );
    let response = call(
        p.clone(),
        "background_task_start",
        "call",
        json!({"program":"python3"}),
    );
    assert!(response.is_error);
    assert_eq!(response.details["state"], "unknown");
    assert!(!response.details.to_string().contains("private"));
    assert!(
        matches!(&p.requests.lock().unwrap()[0], EnvironmentTaskRequest::Start { args,cwd,timeout_ms,.. } if args.is_empty() && cwd=="/mnt/data" && *timeout_ms==0)
    );
    let p = port(
        "run-a",
        1,
        vec![Ok(page(EnvironmentTaskScope::Environment))],
    );
    let wrong = call(
        p,
        "background_task_query",
        "call",
        json!({"task_id":"task-other"}),
    );
    assert!(wrong.is_error);
    assert!(wrong.details.get("output").is_none());
}

#[test]
fn retired_operation_and_history_refusals_are_actionable_without_becoming_unknown() {
    for (error, code, phrase) in [
        (
            "task_history_capacity",
            "background_task_history_capacity",
            "Finish this Run",
        ),
        (
            "task_operation_replay_expired",
            "background_task_replay_expired",
            "do not retry",
        ),
        (
            "task_run_closed",
            "background_task_run_closed",
            "cannot admit",
        ),
        (
            "task_journal_capacity",
            "background_task_capacity",
            "capacity",
        ),
    ] {
        let response = host_failure(error, true);
        assert_eq!(response.details["errorCode"], code);
        assert!(response.is_error);
        assert_ne!(response.details["state"], "unknown");
        assert!(response.content.contains(phrase));
    }
}
