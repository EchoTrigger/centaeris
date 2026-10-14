use centaeris_core::execution::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

fn unknown(reason: impl Into<String>) -> ExecutionError {
    ExecutionError::CancellationIndeterminate {
        reason: reason.into(),
    }
}

fn operation(fence: &EnvironmentTaskFence, id: &str, action: &str) -> String {
    let mut hash = Sha256::new();
    for part in ["retained-command-v1", &fence.controller_id, id, action] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("command_{:x}", hash.finalize())
}

pub(super) fn run_file_helper(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    input: &[u8],
    output_limit: usize,
    path: &str,
    rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
) -> Result<Vec<u8>, String> {
    let request = ExecutionCommandRequest {
        program: super::AGENT_BINARY.into(),
        args: vec!["filesystem-once".into()],
        cwd: WORKSPACE_DATA_ROOT.into(),
        env: Default::default(),
        timeout_ms: 30_000,
        policy: ExecutionPolicy::workspace_write_no_network(WORKSPACE_DATA_ROOT),
    };
    let output = run_command_with_input(fence, operation_id, request, None, path, input, rpc)
        .map_err(|e| e.to_string())?;
    if output.process.exit_code != Some(0)
        || !output.process.runtime_diagnostics.is_empty()
        || output.process.stdout_decode.raw_byte_length > output_limit
        || output.process.stdout.len() > output_limit
    {
        return Err("retained filesystem response incomplete or oversized".into());
    }
    Ok(output.process.stdout.into_bytes())
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    total: usize,
}
impl Capture {
    fn append(&mut self, bytes: &[u8]) -> Result<(), ExecutionError> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| unknown("command_output_count_overflow"))?;
        let remaining = (8 * 1024 * 1024usize).saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        Ok(())
    }
}

pub(super) fn run_command(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    request: ExecutionCommandRequest,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    command_path: &str,
    rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
) -> Result<ExecutionHostCommandOutput, ExecutionError> {
    run_command_with_input(
        fence,
        operation_id,
        request,
        cancellation_probe,
        command_path,
        &[],
        rpc,
    )
}

fn run_command_with_input(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    request: ExecutionCommandRequest,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    command_path: &str,
    input: &[u8],
    mut rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
) -> Result<ExecutionHostCommandOutput, ExecutionError> {
    if operation_id.is_empty()
        || fence.controller_purpose != EnvironmentTaskControllerPurpose::Execute
    {
        return Err(ExecutionError::Denied {
            reason: "retained command identity required".into(),
        });
    }
    let mut args = vec![
        "-i".into(),
        format!("HOME={WORKSPACE_HOME}"),
        format!("PATH={command_path}"),
        "TMPDIR=/tmp".into(),
    ];
    for (key, default) in [("LANG", "C.UTF-8"), ("LC_ALL", "C.UTF-8"), ("TERM", "dumb")] {
        args.push(format!(
            "{key}={}",
            request.env.get(key).map(String::as_str).unwrap_or(default)
        ));
    }
    args.extend([
        "/usr/bin/timeout".into(),
        "--signal=TERM".into(),
        "--kill-after=1s".into(),
        super::coreutils_timeout_duration(request.timeout_ms),
        request.program,
    ]);
    args.extend(request.args);
    let scope = EnvironmentTaskScope::Run {
        run_id: fence.controller_id.clone(),
    };
    let start_id = operation(fence, operation_id, "start");
    let first = rpc(EnvironmentTaskRequest::Start {
        fence: fence.clone(),
        operation_id: start_id.clone(),
        scope: scope.clone(),
        program: "/usr/bin/env".into(),
        args,
        cwd: WORKSPACE_DATA_ROOT.into(),
        timeout_ms: request.timeout_ms.saturating_add(3000),
    })
    .map_err(unknown)?;
    let (id, running) = match &first {
        EnvironmentTaskResult::Task { task, .. }
            if task.scope == scope && task.operation_id == start_id =>
        {
            (
                task.task_id.clone(),
                task.state == EnvironmentTaskState::Running,
            )
        }
        _ => return Err(unknown("command_start_unconfirmed")),
    };
    if running {
        for (index, chunk) in input.chunks(4096).enumerate() {
            let response = rpc(EnvironmentTaskRequest::Input {
                fence: fence.clone(),
                task_id: id.clone(),
                operation_id: operation(fence, operation_id, &format!("input-{index}")),
                data: chunk.to_vec(),
                close: false,
            })
            .map_err(unknown)?;
            if !matches!(response, EnvironmentTaskResult::Mutated { ref task }
                if task.task_id == id && task.scope == scope && task.operation_id == start_id)
            {
                return Err(unknown("command_input_unconfirmed"));
            }
        }
        // A fast process can exit before stdin is closed. Its confirmed terminal
        // query below resolves that race without replaying the command.
        let closed = rpc(EnvironmentTaskRequest::Input {
            fence: fence.clone(),
            task_id: id.clone(),
            operation_id: operation(fence, operation_id, "close"),
            data: Vec::new(),
            close: true,
        });
        if !input.is_empty() {
            closed.map_err(unknown)?;
        }
    } else if !input.is_empty() {
        return Err(unknown("command_exited_before_input"));
    }
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.saturating_add(5000));
    let mut first = Some(first);
    let mut cancelled = false;
    let mut cancel_deadline = None;
    let mut cursor = 0;
    let (mut stdout, mut stderr) = (Capture::default(), Capture::default());
    let exit_code = loop {
        let cancellation_requested = if cancelled {
            false
        } else if let Some(probe) = cancellation_probe {
            probe().map_err(unknown)?.is_some()
        } else {
            false
        };
        if cancellation_requested {
            rpc(EnvironmentTaskRequest::Terminate {
                fence: fence.clone(),
                task_id: id.clone(),
                operation_id: operation(fence, operation_id, "cancel"),
            })
            .map_err(unknown)?;
            cancelled = true;
            cancel_deadline = Some(Instant::now() + Duration::from_secs(5));
        }
        let response = match first.take() {
            Some(result) => result,
            None => rpc(EnvironmentTaskRequest::Query {
                fence: fence.clone(),
                task_id: id.clone(),
                cursor,
                max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
            })
            .map_err(unknown)?,
        };
        let EnvironmentTaskResult::Task {
            task,
            output,
            earliest_cursor,
            next_cursor,
            gap,
            has_more,
        } = response
        else {
            return Err(unknown("command_query_unconfirmed"));
        };
        if task.task_id != id || task.scope != scope || task.operation_id != start_id {
            return Err(unknown("command_task_identity_changed"));
        }
        if gap || earliest_cursor > cursor {
            return Err(unknown("command_output_gap"));
        }
        let mut bytes = 0usize;
        for chunk in output {
            bytes = bytes
                .checked_add(chunk.data.len())
                .ok_or_else(|| unknown("command_output_page_invalid"))?;
            if chunk.cursor != cursor || bytes > ENVIRONMENT_TASK_READ_BYTES {
                return Err(unknown("command_output_page_invalid"));
            }
            match chunk.stream {
                EnvironmentTaskStream::Stdout => stdout.append(&chunk.data)?,
                EnvironmentTaskStream::Stderr => stderr.append(&chunk.data)?,
            }
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| unknown("command_output_cursor_overflow"))?;
        }
        if next_cursor != cursor || (has_more && bytes == 0) {
            return Err(unknown("command_output_page_invalid"));
        }
        if task.state == EnvironmentTaskState::Unknown {
            return Err(unknown("command_process_outcome_unknown"));
        }
        if task.state == EnvironmentTaskState::Exited
            && task.cleanup_complete
            && task.output_complete
            && !has_more
        {
            break task.exit_code;
        }
        if Instant::now() >= deadline || cancel_deadline.is_some_and(|at| Instant::now() >= at) {
            // A failed acknowledgement is not proof of task termination. EndRun
            // cleanup still owns this registered process after this caller exits.
            let _ = rpc(EnvironmentTaskRequest::Terminate {
                fence: fence.clone(),
                task_id: id.clone(),
                operation_id: operation(fence, operation_id, "deadline"),
            });
            return Err(unknown("command_cleanup_unconfirmed"));
        }
        if !has_more {
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let (mut out, mut err) = (
        decode_process_output(&stdout.bytes),
        decode_process_output(&stderr.bytes),
    );
    let mut diagnostics = Vec::new();
    for (name, capture, decoded) in [("stdout", &stdout, &mut out), ("stderr", &stderr, &mut err)] {
        decoded.summary.raw_byte_length = capture.total;
        if capture.total > capture.bytes.len() {
            decoded.text.push_str("\n[output truncated at 8 MiB]");
            diagnostics.push(RuntimeOutputDiagnostic {source:"execution_host".into(),stream:name.into(),severity:"warning".into(),code:"output_truncated".into(),message:"Output exceeded the per-stream retention limit".into(),details:Some(serde_json::json!({"retainedBytes":capture.bytes.len(),"rawBytes":capture.total}))});
        }
    }
    let timed_out = !cancelled && exit_code == Some(124);
    let failure_kind = if cancelled {
        ExecutionHostFailureKind::Cancelled
    } else {
        classify_execution_host_failure(exit_code, timed_out, &out.text, &err.text)
    };
    if timed_out {
        err.text = format!(
            "sandboxed process timed out after {}ms\n{}",
            request.timeout_ms, err.text
        );
    }
    Ok(ExecutionHostCommandOutput {
        process: ExecutionProcessOutput {
            exit_code: if timed_out || cancelled {
                None
            } else {
                exit_code
            },
            stdout: out.text,
            stderr: err.text,
            stdout_decode: out.summary,
            stderr_decode: err.summary,
            timed_out,
            attempt: ExecutionAttempt {
                transition_reason: super::OciRuntime::Runc.transition_reason().into(),
                policy: super::policy_summary(&request.policy),
            },
            runtime_diagnostics: diagnostics,
        },
        failure_kind,
        input_state_changes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    #[test]
    fn file_helper_delivers_exact_stdin_and_requires_bounded_complete_output() {
        let input = vec![b'x'; 5000];
        let mut received = Vec::new();
        let mut closed = false;
        let result = run_file_helper(&fence(), "tool-1", &input, 10, "/usr/bin", |r| match r {
            EnvironmentTaskRequest::Start {
                program,
                args,
                scope,
                ..
            } => {
                assert_eq!(program, "/usr/bin/env");
                assert!(args.iter().any(|a| a == "filesystem-once"));
                assert_eq!(
                    scope,
                    EnvironmentTaskScope::Run {
                        run_id: "run-a".into()
                    }
                );
                Ok(started())
            }
            EnvironmentTaskRequest::Input { data, close, .. } => {
                received.extend(data);
                closed = close;
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Query { .. } => {
                assert!(closed);
                Ok(finished(false))
            }
            _ => panic!("unexpected operation"),
        })
        .unwrap();
        assert_eq!(received, input);
        assert_eq!(result, b"hello");
        let result = run_file_helper(&fence(), "tool-1", b"", 4, "/usr/bin", |_| {
            Ok(finished(false))
        });
        assert!(
            result.is_err(),
            "oversized file response cannot become partial success"
        );
    }

    fn fence() -> EnvironmentTaskFence {
        EnvironmentTaskFence {
            environment_instance_id: "instance-1".into(),
            controller_id: "run-a".into(),
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        }
    }
    fn command() -> ExecutionCommandRequest {
        ExecutionCommandRequest {
            program: "/bin/bash".into(),
            args: vec!["-c".into(), "printf hello".into()],
            cwd: WORKSPACE_DATA_ROOT.into(),
            env: HashMap::new(),
            timeout_ms: 1000,
            policy: ExecutionPolicy::workspace_write_no_network(WORKSPACE_DATA_ROOT),
        }
    }
    fn snapshot(complete: bool) -> EnvironmentTaskSnapshot {
        EnvironmentTaskSnapshot {
            task_id: "task-1".into(),
            operation_id: operation(&fence(), "tool-1", "start"),
            scope: EnvironmentTaskScope::Run {
                run_id: "run-a".into(),
            },
            state: if complete {
                EnvironmentTaskState::Exited
            } else {
                EnvironmentTaskState::Running
            },
            exit_code: complete.then_some(0),
            cleanup_complete: complete,
            output_complete: complete,
        }
    }
    fn started() -> EnvironmentTaskResult {
        EnvironmentTaskResult::Task {
            task: snapshot(false),
            output: Vec::new(),
            earliest_cursor: 0,
            next_cursor: 0,
            gap: false,
            has_more: false,
        }
    }
    fn finished(gap: bool) -> EnvironmentTaskResult {
        EnvironmentTaskResult::Task {
            task: snapshot(true),
            output: vec![EnvironmentTaskOutputChunk {
                cursor: 0,
                stream: EnvironmentTaskStream::Stdout,
                data: b"hello".to_vec(),
            }],
            earliest_cursor: 0,
            next_cursor: 1,
            gap,
            has_more: false,
        }
    }
    #[test]
    fn foreground_command_is_registered_and_output_is_read_without_destroying_instance() {
        let mut replies = VecDeque::from([
            started(),
            EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            },
            finished(false),
        ]);
        let mut requests = Vec::new();
        let out = run_command(&fence(), "tool-1", command(), None, "/usr/bin:/bin", |r| {
            requests.push(r);
            replies.pop_front().ok_or("unexpected request".into())
        })
        .unwrap();
        assert_eq!(out.process.stdout, "hello");
        assert_eq!(out.process.stdout_decode.raw_byte_length, 5);
        assert_eq!(out.process.exit_code, Some(0));
        assert!(
            matches!(&requests[0], EnvironmentTaskRequest::Start { scope:EnvironmentTaskScope::Run {run_id}, program, args, .. } if run_id=="run-a" && program=="/usr/bin/env" && args.iter().any(|a|a=="printf hello"))
        );
        assert!(
            matches!(&requests[1], EnvironmentTaskRequest::Input {close:true,data,..} if data.is_empty())
        );
        assert!(matches!(
            &requests[2],
            EnvironmentTaskRequest::Query {
                max_bytes: 65536,
                ..
            }
        ));
        assert_eq!(requests.len(), 3);
    }
    #[test]
    fn output_loss_is_not_reported_as_successful_complete_capture() {
        let mut replies = VecDeque::from([
            started(),
            EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            },
            finished(true),
        ]);
        let error = run_command(&fence(), "tool-1", command(), None, "/usr/bin:/bin", |_| {
            replies.pop_front().ok_or("unexpected request".into())
        })
        .unwrap_err();
        assert!(error.internal_debug_message().contains("output_gap"));
    }
    #[test]
    fn lost_start_acknowledgement_is_never_retried_with_another_identity() {
        let mut calls = 0;
        let error = run_command(&fence(), "tool-1", command(), None, "/usr/bin:/bin", |_| {
            calls += 1;
            Err("ack lost".into())
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.is_cancellation_indeterminate());
    }

    #[test]
    fn fast_exit_retains_output_returned_with_the_start_receipt() {
        let mut calls = 0;
        let result = run_command(&fence(), "tool-1", command(), None, "/bin", |_| {
            calls += 1;
            Ok(finished(false))
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result.process.stdout, "hello");
    }

    #[test]
    fn cancellation_targets_only_the_registered_run_task_and_waits_for_confirmation() {
        let mut replies = VecDeque::from([
            started(),
            EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            },
            EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            },
            finished(false),
        ]);
        let mut requests = Vec::new();
        let probe = || Ok(Some("cancelled".to_string()));
        let result = run_command(&fence(), "tool-1", command(), Some(&probe), "/bin", |r| {
            requests.push(r);
            replies.pop_front().ok_or("unexpected request".into())
        })
        .unwrap();
        assert_eq!(result.failure_kind, ExecutionHostFailureKind::Cancelled);
        assert_eq!(result.process.exit_code, None);
        assert_eq!(requests.len(), 4);
        assert!(
            matches!(&requests[2], EnvironmentTaskRequest::Terminate {task_id, fence: actual,..} if task_id=="task-1" && actual==&fence())
        );
        assert!(
            matches!(&requests[3], EnvironmentTaskRequest::Query {task_id,..} if task_id=="task-1")
        );
    }

    #[test]
    fn cancellation_probe_failure_is_unknown_and_does_not_authorize_a_signal() {
        let mut requests = Vec::new();
        let mut replies = VecDeque::from([
            started(),
            EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            },
        ]);
        let probe = || Err("cancel query unavailable".to_string());
        let result = run_command(&fence(), "tool-1", command(), Some(&probe), "/bin", |r| {
            requests.push(r);
            replies.pop_front().ok_or("unexpected signal".into())
        });
        assert!(result.unwrap_err().is_cancellation_indeterminate());
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| matches!(
            r,
            EnvironmentTaskRequest::Start { .. } | EnvironmentTaskRequest::Input { .. }
        )));
    }

    #[test]
    fn command_identity_survives_controller_reconnect_but_is_scoped_to_run() {
        let mut identities = Vec::new();
        for (run, epoch) in [("run-a", 1), ("run-a", 3), ("run-b", 4)] {
            let mut current = fence();
            current.controller_id = run.into();
            current.controller_epoch = epoch;
            run_command(&current, "same-tool-call", command(), None, "/bin", |r| {
                let EnvironmentTaskRequest::Start {
                    operation_id,
                    scope,
                    ..
                } = r
                else {
                    panic!("completed replay cannot restart or signal")
                };
                identities.push(operation_id.clone());
                let mut task = snapshot(true);
                task.operation_id = operation_id;
                task.scope = scope;
                Ok(EnvironmentTaskResult::Task {
                    task,
                    output: Vec::new(),
                    earliest_cursor: 0,
                    next_cursor: 0,
                    gap: false,
                    has_more: false,
                })
            })
            .unwrap();
        }
        assert_eq!(identities[0], identities[1]);
        assert_ne!(identities[0], identities[2]);
    }

    #[test]
    fn complete_output_counts_bytes_beyond_retention_without_growing_the_buffer() {
        let pages = 130u64;
        let mut at = 0;
        let result = run_command(&fence(), "tool-1", command(), None, "/bin", |r| {
            if matches!(r, EnvironmentTaskRequest::Input { .. }) {
                return Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                });
            }
            let output = (0..16)
                .map(|offset| EnvironmentTaskOutputChunk {
                    cursor: at * 16 + offset,
                    stream: EnvironmentTaskStream::Stdout,
                    data: vec![b'x'; 4096],
                })
                .collect();
            at += 1;
            Ok(EnvironmentTaskResult::Task {
                task: snapshot(at == pages),
                output,
                earliest_cursor: 0,
                next_cursor: at * 16,
                gap: false,
                has_more: at < pages,
            })
        })
        .unwrap();
        assert_eq!(
            result.process.stdout_decode.raw_byte_length,
            pages as usize * 65536
        );
        assert_eq!(
            result.process.stdout.len(),
            8 * 1024 * 1024 + "\n[output truncated at 8 MiB]".len()
        );
        assert_eq!(
            result.process.runtime_diagnostics[0].code,
            "output_truncated"
        );
    }
}
