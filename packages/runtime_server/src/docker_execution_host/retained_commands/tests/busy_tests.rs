use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn busy_query_keeps_the_registered_task_output_and_original_cursor() {
    let mut starts = 0;
    let mut queries = Vec::new();
    let result = run_command(
        &fence(),
        "tool-1",
        command(),
        None,
        "/bin",
        |request| match request {
            EnvironmentTaskRequest::Start { .. } => {
                starts += 1;
                let mut response = started();
                if let EnvironmentTaskResult::Task {
                    output,
                    next_cursor,
                    ..
                } = &mut response
                {
                    output.push(EnvironmentTaskOutputChunk {
                        cursor: 0,
                        stream: EnvironmentTaskStream::Stdout,
                        data: b"head".to_vec(),
                    });
                    *next_cursor = 1;
                }
                Ok(response)
            }
            EnvironmentTaskRequest::Input { .. } => Ok(EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            }),
            EnvironmentTaskRequest::Query {
                ref task_id,
                cursor,
                ..
            } => {
                assert_eq!(task_id, "task-1");
                assert_eq!(cursor, 1);
                queries.push(serde_json::to_value(&request).unwrap());
                if queries.len() < 3 {
                    return Err("environment_host_busy".into());
                }
                let mut response = finished(false);
                if let EnvironmentTaskResult::Task {
                    output,
                    next_cursor,
                    ..
                } = &mut response
                {
                    output[0].cursor = 1;
                    output[0].data = b"tail".to_vec();
                    *next_cursor = 2;
                }
                Ok(response)
            }
            _ => panic!("busy Query cannot restart, input again or terminate the command"),
        },
    )
    .unwrap();
    assert_eq!(result.process.stdout, "headtail");
    assert_eq!(starts, 1);
    assert!(queries.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn busy_query_returns_to_the_cancellation_probe_before_requerying() {
    let rejected = Arc::new(AtomicBool::new(false));
    let mut terminated = false;
    let mut stops = Vec::new();
    let mut starts = 0;
    let probe_rejected = rejected.clone();
    let probe = move || {
        Ok(probe_rejected
            .load(Ordering::Relaxed)
            .then(|| "agent_run_cancel_requested".into()))
    };
    let result = run_command(
        &fence(),
        "tool-1",
        command(),
        Some(&probe),
        "/bin",
        |request| match request {
            EnvironmentTaskRequest::Start { .. } => {
                starts += 1;
                Ok(started())
            }
            EnvironmentTaskRequest::Input { .. } => Ok(EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            }),
            EnvironmentTaskRequest::Query { .. } if !rejected.swap(true, Ordering::Relaxed) => {
                Err("environment_host_busy".into())
            }
            EnvironmentTaskRequest::Terminate { ref task_id, .. } => {
                assert_eq!(task_id, "task-1");
                stops.push(serde_json::to_value(&request).unwrap());
                if stops.len() == 1 {
                    return Err("environment_host_busy".into());
                }
                terminated = true;
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Query { .. } => {
                assert!(terminated, "a busy response must not bypass cancellation");
                Ok(finished(false))
            }
            _ => panic!("only the original registered task may be observed or stopped"),
        },
    )
    .unwrap();
    assert_eq!(starts, 1);
    assert_eq!(stops.len(), 2);
    assert_eq!(stops[0], stops[1]);
    assert_eq!(result.failure_kind, ExecutionHostFailureKind::Cancelled);
}

#[test]
fn recovered_busy_query_never_starts_or_inputs_a_second_command() {
    let owner = fence();
    let original = owner
        .task_id(&operation(&owner, "tool-1", "start"))
        .unwrap();
    let mut queries = Vec::new();
    let result = recover_command(&owner, "tool-1", command(), None, "/bin", |request| {
        assert!(matches!(&request, EnvironmentTaskRequest::Query { task_id, cursor: 0, .. } if task_id == &original));
        queries.push(serde_json::to_value(request).unwrap());
        if queries.len() < 3 { return Err("environment_host_busy".into()); }
        let mut response = finished(false);
        if let EnvironmentTaskResult::Task { task, .. } = &mut response { task.task_id = original.clone(); }
        Ok(response)
    }).unwrap();
    assert_eq!(result.process.stdout, "hello");
    assert_eq!(queries.len(), 3);
    assert!(queries.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn rejected_input_and_eof_repeat_only_the_same_unaccepted_operation_and_body() {
    let mut data = Vec::new();
    let mut eof = Vec::new();
    let mut starts = 0;
    let result = run_stdin_fixture(
        &fence(),
        "tool-1",
        b"original-bytes",
        100,
        "/bin",
        |request| match &request {
            EnvironmentTaskRequest::Start { .. } => {
                starts += 1;
                Ok(started())
            }
            EnvironmentTaskRequest::Input { close, .. } => {
                let seen = if *close { &mut eof } else { &mut data };
                seen.push(serde_json::to_value(&request).unwrap());
                if seen.len() < 3 {
                    Err("environment_host_busy".into())
                } else {
                    Ok(EnvironmentTaskResult::Mutated {
                        task: snapshot(false),
                    })
                }
            }
            EnvironmentTaskRequest::Query { .. } => {
                assert_eq!(eof.len(), 3);
                Ok(finished(false))
            }
            _ => panic!("definitive busy needs neither a receipt read nor a new Start"),
        },
    )
    .unwrap();
    assert_eq!(result, b"hello");
    assert_eq!(starts, 1);
    assert!(data.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(eof.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn unknown_input_after_busy_uses_only_the_original_receipt_even_when_receipt_query_is_busy() {
    let mut input_calls = 0;
    let mut receipts = Vec::new();
    let mut closes = 0;
    let result = run_stdin_fixture(
        &fence(),
        "tool-1",
        b"original-bytes",
        100,
        "/bin",
        |request| match &request {
            EnvironmentTaskRequest::Start { .. } => Ok(started()),
            EnvironmentTaskRequest::Input { close: false, .. } => {
                input_calls += 1;
                assert!(
                    input_calls <= 2,
                    "unknown input must not be dispatched again"
                );
                Err(if input_calls == 1 {
                    "environment_host_busy"
                } else {
                    "environment_host_transport_unavailable"
                }
                .into())
            }
            EnvironmentTaskRequest::QueryInputReceipt { .. } => {
                receipts.push(serde_json::to_value(&request).unwrap());
                if receipts.len() == 1 {
                    Err("environment_host_busy".into())
                } else {
                    Ok(EnvironmentTaskResult::Mutated {
                        task: snapshot(false),
                    })
                }
            }
            EnvironmentTaskRequest::Input { close: true, .. } => {
                closes += 1;
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Query { .. } => Ok(finished(false)),
            _ => panic!("unknown input allows only the original receipt read"),
        },
    )
    .unwrap();
    assert_eq!(result, b"hello");
    assert_eq!((input_calls, closes), (2, 1));
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0], receipts[1]);
}

#[test]
fn continuous_query_busy_obeys_the_original_deadline_without_real_sleep() {
    for recovering in [false, true] {
        let now = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let owner = fence();
        let expected_task = if recovering {
            owner
                .task_id(&operation(&owner, "tool-1", "start"))
                .unwrap()
        } else {
            "task-1".into()
        };
        let (mut starts, mut queries, mut terminations) = (0, 0, 0);
        let result = run_command_with_io(
            &owner,
            "tool-1",
            command(),
            None,
            "/bin",
            if recovering {
                CommandMode::Recover
            } else {
                CommandMode::Execute(&[])
            },
            CommandIo {
                rpc: |request| match request {
                    EnvironmentTaskRequest::Start { .. } => {
                        starts += 1;
                        Ok(started())
                    }
                    EnvironmentTaskRequest::Input { close: true, .. } => {
                        Ok(EnvironmentTaskResult::Mutated {
                            task: snapshot(false),
                        })
                    }
                    EnvironmentTaskRequest::Query {
                        task_id, cursor, ..
                    } => {
                        assert_eq!(task_id, expected_task);
                        assert_eq!(cursor, 0);
                        queries += 1;
                        Err("environment_host_busy".into())
                    }
                    EnvironmentTaskRequest::Terminate { task_id, .. } => {
                        assert_eq!(task_id, expected_task);
                        terminations += 1;
                        Err("environment_host_busy".into())
                    }
                    _ => panic!("query contention must not dispatch a second tool"),
                },
                now: || now + elapsed.get(),
                delay: |_| elapsed.set(elapsed.get() + Duration::from_secs(1)),
            },
        );
        assert!(result.unwrap_err().is_cancellation_indeterminate());
        assert_eq!(starts, usize::from(!recovering));
        assert!(queries > 1 && queries <= 6);
        assert_eq!(terminations, 1);
        assert_eq!(elapsed.get(), Duration::from_secs(6));
    }
}

#[test]
fn continuous_input_busy_does_not_reset_the_command_deadline_or_send_later_bytes() {
    let now = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let mut inputs = Vec::new();
    let mut terminations = 0;
    let result = run_command_with_io(
        &fence(),
        "tool-1",
        command(),
        None,
        "/bin",
        CommandMode::Execute(&[b'x'; 5000]),
        CommandIo {
            rpc: |request| match &request {
                EnvironmentTaskRequest::Start { .. } => Ok(started()),
                EnvironmentTaskRequest::Input {
                    close: false, data, ..
                } => {
                    assert_eq!(data.len(), 4096);
                    inputs.push(serde_json::to_value(&request).unwrap());
                    Err("environment_host_busy".into())
                }
                EnvironmentTaskRequest::Terminate { task_id, .. } => {
                    assert_eq!(task_id, "task-1");
                    terminations += 1;
                    Ok(EnvironmentTaskResult::Mutated {
                        task: snapshot(false),
                    })
                }
                _ => panic!("busy input must not send another chunk, EOF or output Query"),
            },
            now: || now + elapsed.get(),
            delay: |_| elapsed.set(elapsed.get() + Duration::from_secs(1)),
        },
    );
    assert!(result.unwrap_err().is_cancellation_indeterminate());
    assert_eq!(terminations, 1);
    assert_eq!(elapsed.get(), Duration::from_secs(6));
    assert!(inputs.len() > 1 && inputs.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn cancellation_during_busy_input_stops_the_original_task_without_more_input() {
    let rejected = Arc::new(AtomicBool::new(false));
    let probe_rejected = rejected.clone();
    let probe = move || {
        Ok(probe_rejected
            .load(Ordering::Relaxed)
            .then(|| "agent_run_cancel_requested".into()))
    };
    let mut inputs = 0;
    let mut terminations = 0;
    let result = run_command_with_io(
        &fence(),
        "tool-1",
        command(),
        Some(&probe),
        "/bin",
        CommandMode::Execute(b"original-input"),
        CommandIo {
            rpc: |request| match request {
                EnvironmentTaskRequest::Start { .. } => Ok(started()),
                EnvironmentTaskRequest::Input { close: false, .. } => {
                    inputs += 1;
                    rejected.store(true, Ordering::Relaxed);
                    Err("environment_host_busy".into())
                }
                EnvironmentTaskRequest::Terminate { task_id, .. } => {
                    assert_eq!(task_id, "task-1");
                    terminations += 1;
                    Ok(EnvironmentTaskResult::Mutated {
                        task: snapshot(false),
                    })
                }
                EnvironmentTaskRequest::Query { .. } => Ok(finished(false)),
                _ => panic!("cancelled input must not send EOF"),
            },
            now: Instant::now,
            delay: |_| {},
        },
    )
    .unwrap();
    assert_eq!((inputs, terminations), (1, 1));
    assert_eq!(result.failure_kind, ExecutionHostFailureKind::Cancelled);
}

#[test]
fn hook_start_busy_retries_only_the_identical_unaccepted_start_before_sending_input() {
    let now = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let mut starts = Vec::new();
    let mut writes = 0;
    let mut closes = 0;
    let result = run_command_with_io(
        &fence(),
        "tool-1",
        command(),
        None,
        "/bin",
        CommandMode::Execute(b"hook-event\n"),
        CommandIo {
            rpc: |request| match &request {
                EnvironmentTaskRequest::Start { .. } => {
                    starts.push(serde_json::to_value(&request).unwrap());
                    if starts.len() < 4 {
                        Err("environment_host_busy".into())
                    } else {
                        Ok(started())
                    }
                }
                EnvironmentTaskRequest::Input { data, close, .. } => {
                    assert_eq!(starts.len(), 4);
                    if *close {
                        assert!(data.is_empty());
                        closes += 1;
                    } else {
                        assert_eq!(data, b"hook-event\n");
                        writes += 1;
                    }
                    Ok(EnvironmentTaskResult::Mutated {
                        task: snapshot(false),
                    })
                }
                EnvironmentTaskRequest::Query { .. } => Ok(finished(false)),
                _ => panic!("unaccepted busy hook cannot signal another process"),
            },
            now: || now + elapsed.get(),
            delay: |_| elapsed.set(elapsed.get() + Duration::from_millis(250)),
        },
    )
    .unwrap();
    assert_eq!(result.process.stdout, "hello");
    assert!(starts.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!((writes, closes), (1, 1));
}

#[test]
fn hook_start_busy_exhaustion_is_bounded_and_proves_that_execution_never_started() {
    let now = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let mut calls = 0;
    let error = run_command_with_io(
        &fence(),
        "tool-1",
        command(),
        None,
        "/bin",
        CommandMode::Execute(b"hook-event\n"),
        CommandIo {
            rpc: |request| {
                assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                calls += 1;
                Err("environment_host_busy".into())
            },
            now: || now + elapsed.get(),
            delay: |_| elapsed.set(elapsed.get() + Duration::from_millis(250)),
        },
    )
    .unwrap_err();
    assert!(
        matches!(&error, ExecutionError::CapacityRejected { stage: ExecutionOperationStage::Start, reason } if reason == "environment_host_busy")
    );
    assert_eq!(error.execution_started(), Some(false));
    assert_eq!(elapsed.get(), Duration::from_secs(1));
    assert!(calls > 1 && calls <= 5);
    assert_eq!(
        super::super::super::lifecycle_hook_command_failure(error),
        "lifecycle_hook_start_busy"
    );
}

#[test]
fn hook_start_unknown_after_busy_is_never_reissued_or_classified_as_unaccepted() {
    let now = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let mut calls = 0;
    let error = run_command_with_io(
        &fence(),
        "tool-1",
        command(),
        None,
        "/bin",
        CommandMode::Execute(b"hook-event\n"),
        CommandIo {
            rpc: |request| {
                assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                calls += 1;
                Err(if calls == 1 {
                    "environment_host_busy"
                } else {
                    "environment_host_transport_unavailable"
                }
                .into())
            },
            now: || now + elapsed.get(),
            delay: |_| elapsed.set(elapsed.get() + Duration::from_millis(250)),
        },
    )
    .unwrap_err();
    assert_eq!(calls, 2);
    assert_eq!(error.execution_started(), None);
    assert_eq!(
        super::super::super::lifecycle_hook_command_failure(error),
        "lifecycle_hook_outcome_unknown"
    );
}

#[test]
fn hook_start_busy_classification_cannot_be_forged_by_an_input_or_uncertain_ack() {
    for error in [
        ExecutionError::OperationIndeterminate {
            stage: ExecutionOperationStage::Start,
            reason: "environment_host_busy".into(),
        },
        ExecutionError::CapacityRejected {
            stage: ExecutionOperationStage::Input,
            reason: "environment_host_busy".into(),
        },
        ExecutionError::CapacityRejected {
            stage: ExecutionOperationStage::Start,
            reason: "host_busy".into(),
        },
        ExecutionError::CapacityRejected {
            stage: ExecutionOperationStage::Start,
            reason: "environment_host_busy: detail".into(),
        },
    ] {
        assert_eq!(
            super::super::super::lifecycle_hook_command_failure(error),
            "lifecycle_hook_outcome_unknown"
        );
    }
}
