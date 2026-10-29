use centaeris_core::execution::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

fn unknown(reason: impl Into<String>) -> ExecutionError {
    ExecutionError::OperationIndeterminate {
        stage: ExecutionOperationStage::Query,
        reason: reason.into(),
    }
}
fn attempt_error(stage: ExecutionOperationStage, reason: String) -> ExecutionError {
    // These exact errors are emitted only before accepting this RPC attempt.
    // An Input rejection does not undo an earlier accepted Start.
    if environment_task_capacity_rejection(&reason)
        || (stage == ExecutionOperationStage::Start && reason == "environment_host_busy")
    {
        ExecutionError::CapacityRejected { stage, reason }
    } else {
        ExecutionError::OperationIndeterminate { stage, reason }
    }
}
fn cancellation_unknown(reason: impl Into<String>) -> ExecutionError {
    ExecutionError::CancellationIndeterminate {
        reason: reason.into(),
    }
}

fn operation(fence: &EnvironmentTaskFence, id: &str, action: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"centaeris:retained-command:v3\0");
    hash.update(
        serde_json::to_vec(&(&fence.controller_owner, id, action)).expect("typed command identity"),
    );
    fence.operation_id(&format!("command_{:x}", hash.finalize()))
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
        CommandMode::Execute(&[]),
        rpc,
    )
}

pub(super) fn run_command_with_stdin(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    request: ExecutionCommandRequest,
    input: &[u8],
    command_path: &str,
    rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
) -> Result<ExecutionHostCommandOutput, ExecutionError> {
    run_command_with_input(
        fence,
        operation_id,
        request,
        None,
        command_path,
        CommandMode::Execute(input),
        rpc,
    )
}

pub(super) fn recover_command(
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
        CommandMode::Recover,
        rpc,
    )
}

enum CommandMode<'a> {
    Execute(&'a [u8]),
    Recover,
}

struct InputTarget<'a> {
    task_id: &'a str,
    start_id: &'a str,
    scope: &'a EnvironmentTaskScope,
}

struct CommandIo<Rpc, Now, Delay> {
    rpc: Rpc,
    now: Now,
    delay: Delay,
}

struct CommandControl<'a> {
    deadline: Instant,
    cancellation_probe: Option<&'a ExecutionCancellationProbe>,
}

impl<Rpc, Now, Delay> CommandIo<Rpc, Now, Delay>
where
    Rpc: FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
    Now: FnMut() -> Instant,
    Delay: FnMut(Duration),
{
    fn start_without_probe(
        &mut self,
        request: &EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        // Hooks have no cancellation probe. Only the host's definitive
        // pre-dispatch rejection permits a short wait on the identical Start.
        let deadline = (self.now)() + Duration::from_secs(1);
        loop {
            let error = match (self.rpc)(request.clone()) {
                Ok(response) => return Ok(response),
                Err(reason) if reason == "environment_host_busy" => reason,
                Err(reason) => return Err(reason),
            };
            let remaining = deadline.saturating_duration_since((self.now)());
            if remaining.is_zero() {
                return Err(error);
            }
            (self.delay)(Duration::from_millis(25).min(remaining));
            if (self.now)() >= deadline {
                return Err(error);
            }
        }
    }

    fn pause_busy(&mut self, control: &CommandControl<'_>) -> Result<bool, ExecutionError> {
        if (self.now)() >= control.deadline {
            return Ok(false);
        }
        if let Some(probe) = control.cancellation_probe {
            if probe().map_err(unknown)?.is_some() {
                return Ok(false);
            }
        }
        (self.delay)(
            Duration::from_millis(25).min(control.deadline.saturating_duration_since((self.now)())),
        );
        if (self.now)() >= control.deadline {
            return Ok(false);
        }
        if let Some(probe) = control.cancellation_probe {
            if probe().map_err(unknown)?.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn confirm_input(
    fence: &EnvironmentTaskFence,
    target: InputTarget<'_>,
    operation_id: String,
    data: Vec<u8>,
    close: bool,
    io: &mut CommandIo<
        impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
        impl FnMut() -> Instant,
        impl FnMut(Duration),
    >,
    control: &CommandControl<'_>,
) -> Result<bool, ExecutionError> {
    let InputTarget {
        task_id,
        start_id,
        scope,
    } = target;
    let confirmed = |result: &EnvironmentTaskResult| {
        matches!(result,
        EnvironmentTaskResult::Mutated { task }
        if task.task_id == task_id && task.scope == *scope
            && fence.matches_original_operation_id(&task.operation_id, start_id))
    };
    let request_digest = environment_task_input_digest(&data, close)
        .map_err(|reason| attempt_error(ExecutionOperationStage::Input, reason))?;
    let phase = if close {
        "retainedInputEof"
    } else {
        "retainedInputData"
    };
    let input = EnvironmentTaskRequest::Input {
        fence: fence.clone(),
        task_id: task_id.into(),
        operation_id: operation_id.clone(),
        data,
        close,
    };
    let reason = loop {
        match crate::observations::timed(phase, || (io.rpc)(input.clone())) {
            Ok(result) if confirmed(&result) => return Ok(true),
            Ok(_) => break "command_input_unconfirmed".into(),
            Err(reason) if reason == "environment_host_busy" => {
                if !io.pause_busy(control)? {
                    return Ok(false);
                }
            }
            Err(reason) if environment_task_capacity_rejection(&reason) => {
                return Err(attempt_error(ExecutionOperationStage::Input, reason));
            }
            Err(reason) => break reason,
        }
    };
    // An uncertain acknowledgement permits only a read of this original
    // operation's durable receipt. Never repeat the stdin write or EOF.
    let receipt = EnvironmentTaskRequest::QueryInputReceipt {
        fence: fence.clone(),
        task_id: task_id.into(),
        operation_id,
        request_digest,
    };
    loop {
        match crate::observations::timed("retainedInputReceipt", || (io.rpc)(receipt.clone())) {
            Ok(result) if confirmed(&result) => return Ok(true),
            Err(reason) if reason == "environment_host_busy" => {
                if !io.pause_busy(control)? {
                    return Ok(false);
                }
            }
            _ => return Err(attempt_error(ExecutionOperationStage::Input, reason)),
        }
    }
}

fn run_command_with_input(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    request: ExecutionCommandRequest,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    command_path: &str,
    mode: CommandMode<'_>,
    rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
) -> Result<ExecutionHostCommandOutput, ExecutionError> {
    run_command_with_io(
        fence,
        operation_id,
        request,
        cancellation_probe,
        command_path,
        mode,
        CommandIo {
            rpc,
            now: Instant::now,
            delay: std::thread::sleep,
        },
    )
}

struct CommandTiming {
    started: Instant,
    operation_id: String,
    task_id: String,
}

fn run_command_with_io(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    request: ExecutionCommandRequest,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    command_path: &str,
    mode: CommandMode<'_>,
    mut io: CommandIo<
        impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
        impl FnMut() -> Instant,
        impl FnMut(Duration),
    >,
) -> Result<ExecutionHostCommandOutput, ExecutionError> {
    let mut observation = (crate::observations::operation_timing_enabled()
        && matches!(
            fence.controller_owner,
            EnvironmentTaskOwner::AgentRun { .. }
        ))
    .then(|| CommandTiming {
        started: (io.now)(),
        operation_id: String::new(),
        task_id: String::new(),
    });
    let result = run_command_inner(
        fence,
        operation_id,
        request,
        cancellation_probe,
        command_path,
        mode,
        &mut io,
        &mut observation,
    );
    if let (Some(mut observation), EnvironmentTaskOwner::AgentRun { run_id }) =
        (observation, &fence.controller_owner)
    {
        if observation.task_id.is_empty() && !observation.operation_id.is_empty() {
            observation.task_id = fence.task_id(&observation.operation_id).unwrap_or_default();
        }
        crate::observations::operation_timing(
            "hostCommand",
            (io.now)().saturating_duration_since(observation.started),
            if result.is_ok() { "ok" } else { "error" },
            run_id,
            &observation.operation_id,
            &observation.task_id,
        );
    }
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "The observation sidecar preserves the existing command execution boundary"
)]
fn run_command_inner(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
    request: ExecutionCommandRequest,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    command_path: &str,
    mode: CommandMode<'_>,
    io: &mut CommandIo<
        impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
        impl FnMut() -> Instant,
        impl FnMut(Duration),
    >,
    observation: &mut Option<CommandTiming>,
) -> Result<ExecutionHostCommandOutput, ExecutionError> {
    let (input, recovering) = match mode {
        CommandMode::Execute(input) => (input, false),
        CommandMode::Recover => (&[][..], true),
    };
    if operation_id.is_empty()
        || fence.controller_purpose != EnvironmentTaskControllerPurpose::Execute
    {
        return Err(ExecutionError::Denied {
            reason: "retained command identity required".into(),
        });
    }
    let scope = match &fence.controller_owner {
        EnvironmentTaskOwner::AgentRun { run_id } => EnvironmentTaskScope::Run {
            run_id: run_id.clone(),
        },
        EnvironmentTaskOwner::MaintenanceOperation { operation_id } => {
            EnvironmentTaskScope::Operation {
                operation_id: operation_id.clone(),
            }
        }
    };
    fence
        .authorize_start(&scope)
        .map_err(|reason| ExecutionError::Denied { reason })?;
    let mut env = std::collections::BTreeMap::from([
        ("HOME".into(), WORKSPACE_HOME.into()),
        ("PATH".into(), command_path.into()),
        ("TMPDIR".into(), "/tmp".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("TERM".into(), "dumb".into()),
    ]);
    env.extend(request.env.clone());
    let start_id = operation(fence, operation_id, "start");
    if let Some(observation) = observation {
        observation.operation_id.clone_from(&start_id);
    }
    let (id, mut first, running) = if recovering {
        // Recovery already owns this task identity. Its first Query belongs to
        // the same cancellation/deadline loop as every later Query.
        (fence.task_id(&start_id).map_err(unknown)?, None, false)
    } else {
        let first_request = EnvironmentTaskRequest::Start {
            fence: fence.clone(),
            operation_id: start_id.clone(),
            scope: scope.clone(),
            program: request.program,
            args: request.args,
            env,
            cwd: request.cwd.to_string_lossy().into_owned(),
            timeout_ms: request.timeout_ms,
        };
        let first = if let Some(probe) = cancellation_probe {
            super::admission_wait::start(|| (io.rpc)(first_request.clone()), probe, &mut io.delay)
        } else {
            io.start_without_probe(&first_request)
        }
        .map_err(|reason| attempt_error(ExecutionOperationStage::Start, reason))?;
        let (id, running) = match &first {
            EnvironmentTaskResult::Task { task, .. }
                if task.scope == scope
                    && fence.matches_original_operation_id(&task.operation_id, &start_id) =>
            {
                (
                    task.task_id.clone(),
                    task.state == EnvironmentTaskState::Running,
                )
            }
            _ => {
                return Err(attempt_error(
                    ExecutionOperationStage::Start,
                    "command_start_unconfirmed".into(),
                ))
            }
        };
        (id, Some(first), running)
    };
    if let Some(observation) = observation {
        observation.task_id.clone_from(&id);
    }
    // Start capacity waits precede execution; after acceptance, Input/Query
    // pressure must never restart this command's original timeout budget.
    let deadline = (io.now)() + Duration::from_millis(request.timeout_ms.saturating_add(5000));
    let control = CommandControl {
        deadline,
        cancellation_probe,
    };
    let mut input_interrupted = false;
    if running && !recovering {
        for (index, chunk) in input.chunks(4096).enumerate() {
            if !confirm_input(
                fence,
                InputTarget {
                    task_id: &id,
                    start_id: &start_id,
                    scope: &scope,
                },
                operation(fence, operation_id, &format!("input-{index}")),
                chunk.to_vec(),
                false,
                io,
                &control,
            )? {
                input_interrupted = true;
                break;
            }
        }
        // A fast process can exit before stdin is closed. Its confirmed terminal
        // query below resolves that race without replaying the command.
        if input.is_empty() {
            let close = EnvironmentTaskRequest::Input {
                fence: fence.clone(),
                task_id: id.clone(),
                operation_id: operation(fence, operation_id, "close"),
                data: Vec::new(),
                close: true,
            };
            loop {
                match crate::observations::timed("retainedInputEof", || (io.rpc)(close.clone())) {
                    Err(reason) if reason == "environment_host_busy" => {
                        if !io.pause_busy(&control)? {
                            input_interrupted = true;
                            break;
                        }
                    }
                    _ => break,
                }
            }
        } else if !input_interrupted {
            input_interrupted = !confirm_input(
                fence,
                InputTarget {
                    task_id: &id,
                    start_id: &start_id,
                    scope: &scope,
                },
                operation(fence, operation_id, "close"),
                Vec::new(),
                true,
                io,
                &control,
            )?;
        }
    } else if !input.is_empty() && !recovering {
        return Err(attempt_error(
            ExecutionOperationStage::Input,
            "command_exited_before_input".into(),
        ));
    }
    let mut cancelled = false;
    let mut cancellation_pending = input_interrupted;
    let mut cancel_deadline = None;
    let mut cursor = 0;
    let (mut stdout, mut stderr) = (Capture::default(), Capture::default());
    let exit_code = loop {
        if (io.now)() >= deadline || cancel_deadline.is_some_and(|at| (io.now)() >= at) {
            // A rejected Query is still subject to the original task deadline.
            // EndRun cleanup retains responsibility for unconfirmed termination.
            let _ = (io.rpc)(EnvironmentTaskRequest::Terminate {
                fence: fence.clone(),
                task_id: id.clone(),
                operation_id: operation(fence, operation_id, "deadline"),
            });
            return Err(cancellation_unknown("command_cleanup_unconfirmed"));
        }
        let cancellation_requested = if cancelled {
            false
        } else if cancellation_pending {
            true
        } else if let Some(probe) = cancellation_probe {
            probe().map_err(unknown)?.is_some()
        } else {
            false
        };
        if cancellation_requested {
            cancellation_pending = true;
            let result = (io.rpc)(EnvironmentTaskRequest::Terminate {
                fence: fence.clone(),
                task_id: id.clone(),
                operation_id: operation(fence, operation_id, "cancel"),
            });
            match result {
                Ok(_) => {}
                Err(reason) if reason == "environment_host_busy" => {
                    (io.delay)(Duration::from_millis(25));
                    continue;
                }
                Err(reason) => return Err(cancellation_unknown(reason)),
            }
            cancelled = true;
            cancel_deadline = Some((io.now)() + Duration::from_secs(5));
        }
        let response = match first.take() {
            Some(result) => result,
            None => match (io.rpc)(EnvironmentTaskRequest::Query {
                fence: fence.clone(),
                task_id: id.clone(),
                cursor,
                max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
            }) {
                Ok(response) => response,
                Err(reason) if reason == "environment_host_busy" => {
                    (io.delay)(Duration::from_millis(25));
                    continue;
                }
                Err(reason) => return Err(unknown(reason)),
            },
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
        if task.task_id != id
            || task.scope != scope
            || !fence.matches_original_operation_id(&task.operation_id, &start_id)
        {
            return Err(unknown("command_task_identity_changed"));
        }
        if gap || earliest_cursor > cursor {
            return Err(ExecutionError::OutputUnavailable {
                reason: "command_output_gap".into(),
                exit_code: if task.state == EnvironmentTaskState::Exited {
                    task.exit_code
                } else {
                    None
                },
            });
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
        if !has_more {
            (io.delay)(Duration::from_millis(25));
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

    mod busy_tests;

    #[test]
    fn host_command_observation_includes_output_and_cleanup_tail() {
        let observation_started = Instant::now();
        let clock = std::cell::Cell::new(observation_started);
        let mut queries = 0;
        let (result, records) = crate::observations::capture_operation_records(|| {
            run_command_with_io(
                &fence(),
                "tool-1",
                command(),
                None,
                "/bin",
                CommandMode::Execute(&[]),
                CommandIo {
                    now: || clock.get(),
                    delay: |duration| clock.set(clock.get() + duration),
                    rpc: |request| match request {
                        EnvironmentTaskRequest::Start { .. } => Ok(started()),
                        EnvironmentTaskRequest::Input { .. } => {
                            Ok(EnvironmentTaskResult::Mutated {
                                task: snapshot(false),
                            })
                        }
                        EnvironmentTaskRequest::Query { .. } => {
                            queries += 1;
                            clock.set(clock.get() + Duration::from_millis(40));
                            let EnvironmentTaskResult::Task {
                                mut task,
                                mut output,
                                earliest_cursor,
                                next_cursor,
                                gap,
                                has_more,
                            } = finished(false)
                            else {
                                unreachable!()
                            };
                            task.cleanup_complete = queries != 1;
                            task.output_complete = queries != 2;
                            if queries > 1 {
                                output.clear();
                            }
                            Ok(EnvironmentTaskResult::Task {
                                task,
                                output,
                                earliest_cursor,
                                next_cursor,
                                gap,
                                has_more,
                            })
                        }
                        _ => panic!("unexpected command operation"),
                    },
                },
            )
        });
        assert_eq!(result.unwrap().process.stdout, "hello");
        assert_eq!(
            queries, 3,
            "leader exit must not replace complete output and cleanup"
        );
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record["phase"], "hostCommand");
        assert_eq!(
            record["elapsedMs"],
            clock
                .get()
                .duration_since(observation_started)
                .as_secs_f64()
                * 1000.0
        );
        assert_eq!(record["runId"], "run-a");
        assert_eq!(record["jobId"], "agent_run.lifecycle:run-a");
        assert_eq!(
            record["operationId"],
            operation(&fence(), "tool-1", "start")
        );
        assert_eq!(record["taskId"], "task-1");
        assert_eq!(record["outcome"], "ok");
    }

    #[test]
    fn host_command_failed_start_records_error_with_same_derived_identity() {
        let clock = std::cell::Cell::new(Instant::now());
        let (result, records) = crate::observations::capture_operation_records(|| {
            run_command_with_io(
                &fence(),
                "tool-1",
                command(),
                None,
                "/bin",
                CommandMode::Execute(&[]),
                CommandIo {
                    now: || clock.get(),
                    delay: |_| panic!("unknown start cannot retry"),
                    rpc: |_| {
                        clock.set(clock.get() + Duration::from_millis(7));
                        Err("secret command output must not appear".into())
                    },
                },
            )
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["elapsedMs"], 7.0);
        assert_eq!(records[0]["outcome"], "error");
        let start_id = operation(&fence(), "tool-1", "start");
        assert_eq!(records[0]["operationId"], start_id);
        assert_eq!(records[0]["taskId"], fence().task_id(&start_id).unwrap());
        assert!(!records[0].to_string().contains("secret"));
    }

    fn run_stdin_fixture(
        fence: &EnvironmentTaskFence,
        operation_id: &str,
        input: &[u8],
        output_limit: usize,
        path: &str,
        rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
    ) -> Result<Vec<u8>, String> {
        let request = ExecutionCommandRequest {
            program: crate::docker_execution_host::AGENT_BINARY.into(),
            args: vec!["filesystem-once".into()],
            cwd: WORKSPACE_DATA_ROOT.into(),
            env: Default::default(),
            timeout_ms: 30_000,
            policy: ExecutionPolicy::workspace_write_no_network(WORKSPACE_DATA_ROOT),
        };
        let output = run_command_with_input(
            fence,
            operation_id,
            request,
            None,
            path,
            CommandMode::Execute(input),
            rpc,
        )
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

    #[test]
    fn recovery_queries_original_task_without_start_or_input() {
        let mut renewed = fence();
        renewed.controller_epoch += 1;
        let task_id = renewed
            .task_id(&operation(&renewed, "tool-1", "start"))
            .unwrap();
        let mut response = finished(false);
        if let EnvironmentTaskResult::Task { task, .. } = &mut response {
            task.task_id = task_id.clone();
        }
        let mut requests = Vec::new();
        let out = recover_command(&renewed, "tool-1", command(), None, "/usr/bin:/bin", |request| {
            requests.push(request.clone());
            assert!(matches!(request, EnvironmentTaskRequest::Query { ref fence, task_id: ref observed_id, cursor: 0, .. } if fence == &renewed && observed_id == &task_id));
            Ok(response.clone())
        }).unwrap();
        assert_eq!(out.process.stdout, "hello");
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn recovery_observes_running_task_and_preserves_original_output() {
        let owner = fence();
        let id = owner
            .task_id(&operation(&owner, "tool-1", "start"))
            .unwrap();
        let mut replies = VecDeque::from([started(), finished(false)]);
        let mut queries = 0;
        let result = recover_command(
            &owner,
            "tool-1",
            command(),
            None,
            "/usr/bin:/bin",
            |request| {
                assert!(matches!(request, EnvironmentTaskRequest::Query { .. }));
                queries += 1;
                let mut reply = replies.pop_front().unwrap();
                if let EnvironmentTaskResult::Task { task, .. } = &mut reply {
                    task.task_id = id.clone();
                }
                Ok(reply)
            },
        )
        .unwrap();
        assert_eq!(queries, 2);
        assert_eq!(result.process.stdout, "hello");
        assert_eq!(result.process.exit_code, Some(0));
    }

    #[test]
    fn recovery_rejects_unknown_foreign_and_missing_output_without_dispatch() {
        let owner = fence();
        let id = owner
            .task_id(&operation(&owner, "tool-1", "start"))
            .unwrap();
        for variant in 0..5 {
            let mut calls = 0;
            let result = recover_command(
                &owner,
                "tool-1",
                command(),
                None,
                "/usr/bin:/bin",
                |request| {
                    calls += 1;
                    assert!(matches!(request, EnvironmentTaskRequest::Query { .. }));
                    if variant == 0 {
                        return Err("task_not_found".into());
                    }
                    let mut reply = finished(variant == 4);
                    if let EnvironmentTaskResult::Task { task, .. } = &mut reply {
                        task.task_id = id.clone();
                        match variant {
                            1 => task.task_id = "foreign-task".into(),
                            2 => task.operation_id = owner.operation_id("foreign-operation"),
                            3 => {
                                task.scope = EnvironmentTaskScope::Run {
                                    run_id: "foreign-run".into(),
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(reply)
                },
            );
            assert_eq!(calls, 1);
            if variant == 4 {
                assert!(matches!(
                    result,
                    Err(ExecutionError::OutputUnavailable {
                        exit_code: Some(0),
                        ..
                    })
                ));
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn run_failure_classification_preserves_canonical_history_capacity() {
        for reason in [
            "read AGENTS.md failed: Execution cancellation could not be verified",
            "read AGENTS.md failed: task_history_capacity: transport context",
            "unrelated task_history_capacity",
        ] {
            assert_eq!(
                crate::public_agent_failure_reason(reason, "core_run_failed"),
                "core_run_failed"
            );
        }
        assert_eq!(
            crate::public_agent_failure_reason(
                "read AGENTS.md failed: task_history_capacity",
                "core_run_failed"
            ),
            "task_history_capacity"
        );
    }

    #[test]
    fn start_rpc_history_capacity_is_a_definitive_rejection() {
        let mut calls = 0;
        let error = run_command(
            &fence(),
            "tool-1",
            command(),
            None,
            "/usr/bin:/bin",
            |request| {
                calls += 1;
                assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                Err("task_history_capacity".into())
            },
        )
        .unwrap_err();
        assert_eq!(
            calls, 1,
            "a rejected Start must not query, input, terminate, or retry"
        );
        assert_eq!(
            error,
            ExecutionError::CapacityRejected {
                stage: ExecutionOperationStage::Start,
                reason: "task_history_capacity".into()
            }
        );
    }

    #[test]
    fn hosted_foreground_capacity_wait_retries_only_the_identical_unaccepted_start() {
        let mut requests = Vec::new();
        let probe = || Ok(None);
        let output = run_command(
            &fence(),
            "tool-1",
            command(),
            Some(&probe),
            "/usr/bin:/bin",
            |request| {
                requests.push(serde_json::to_value(&request).unwrap());
                assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                if requests.len() < 4 {
                    Err("task_capacity".into())
                } else {
                    Ok(finished(false))
                }
            },
        )
        .unwrap();
        assert_eq!(output.process.stdout, "hello");
        assert_eq!(requests.len(), 4);
        assert!(
            requests.iter().all(|request| request == &requests[0]),
            "all parameters, fence and operation identity must remain original"
        );
    }

    #[test]
    fn hosted_foreground_capacity_wait_observes_cancel_without_input_or_second_start() {
        let mut starts = 0;
        let probe = || Ok(Some("agent_run_cancel_requested".into()));
        let error = run_command(
            &fence(),
            "tool-1",
            command(),
            Some(&probe),
            "/bin",
            |request| {
                starts += 1;
                assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                Err("task_capacity".into())
            },
        )
        .unwrap_err();
        assert_eq!(starts, 1);
        assert_eq!(
            error,
            ExecutionError::OperationIndeterminate {
                stage: ExecutionOperationStage::Start,
                reason: "agent_run_cancel_requested".into()
            }
        );
    }

    #[test]
    fn start_rpc_file_helper_preserves_history_capacity_reason() {
        let mut calls = 0;
        let error = run_stdin_fixture(
            &fence(),
            "helper-1",
            b"filesystem request",
            1024,
            "/usr/bin",
            |request| {
                calls += 1;
                assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                Err("task_history_capacity".into())
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error, "task_history_capacity");
    }

    #[test]
    fn start_rpc_unknown_errors_remain_indeterminate_without_retry() {
        for reason in [
            "ack lost",
            "task_history_capacity: transport context",
            " task_history_capacity",
        ] {
            let mut calls = 0;
            let error = run_command(
                &fence(),
                "tool-1",
                command(),
                None,
                "/usr/bin:/bin",
                |request| {
                    calls += 1;
                    assert!(matches!(request, EnvironmentTaskRequest::Start { .. }));
                    Err(reason.into())
                },
            )
            .unwrap_err();
            assert_eq!(calls, 1);
            assert_eq!(
                error,
                ExecutionError::OperationIndeterminate {
                    stage: ExecutionOperationStage::Start,
                    reason: reason.into()
                }
            );
        }
    }

    #[test]
    fn agents_helper_data_digest_matches_the_read_request_receipt() {
        let frame = serde_json::to_vec(&super::super::SandboxFileSystemRequest {
            path: "AGENTS.md".into(),
            operation: ExecutionFileSystemOperation::ReadFile {
                max_bytes: 32_768 * 4,
            },
        })
        .unwrap();
        assert_eq!(frame.len(), 70);
        assert_eq!(
            environment_task_input_digest(&frame, false).unwrap(),
            "68cb8d8fc673e3426d24d9a3f268ba39f2d561987ac970ca699a41a33b2e4445"
        );
        assert_ne!(
            environment_task_input_digest(&frame, true).unwrap(),
            environment_task_input_digest(&frame, false).unwrap()
        );
    }

    #[test]
    fn helper_lost_data_ack_reads_original_receipt_then_sends_only_first_eof() {
        let frame = serde_json::to_vec(&super::super::SandboxFileSystemRequest {
            path: "AGENTS.md".into(),
            operation: ExecutionFileSystemOperation::ReadFile {
                max_bytes: 32_768 * 4,
            },
        })
        .unwrap();
        let (mut starts, mut writes, mut receipt_queries, mut closes) = (0, 0, 0, 0);
        let mut original_input = String::new();
        let result = run_stdin_fixture(&fence(), "tool-1", &frame, 10, "/usr/bin", |r| match r {
            EnvironmentTaskRequest::Start { .. } => {
                starts += 1;
                Ok(started())
            }
            EnvironmentTaskRequest::Input {
                data,
                close: false,
                operation_id,
                ..
            } => {
                writes += 1;
                assert_eq!(writes, 1, "confirmed bytes must never be written twice");
                assert_eq!(data, frame);
                original_input = operation_id;
                Err("task_relay_output_unknown".into())
            }
            EnvironmentTaskRequest::QueryInputReceipt {
                task_id,
                operation_id,
                request_digest,
                ..
            } => {
                receipt_queries += 1;
                assert_eq!(task_id, "task-1");
                assert_eq!(operation_id, original_input);
                assert_eq!(
                    request_digest,
                    environment_task_input_digest(&frame, false).unwrap()
                );
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Input {
                close: true, data, ..
            } => {
                closes += 1;
                assert_eq!((writes, receipt_queries, closes), (1, 1, 1));
                assert!(
                    data.is_empty(),
                    "EOF is still its original distinct operation"
                );
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Query { .. } => {
                assert_eq!(closes, 1);
                Ok(finished(false))
            }
            _ => panic!("receipt reconciliation must not mutate another task"),
        })
        .unwrap();
        assert_eq!(result, b"hello");
        assert_eq!((starts, writes, receipt_queries, closes), (1, 1, 1, 1));
    }

    #[test]
    fn helper_lost_eof_ack_reads_its_receipt_without_closing_twice() {
        let (mut writes, mut closes, mut queries) = (0, 0, 0);
        let result = run_stdin_fixture(&fence(), "tool-1", b"frame", 10, "/usr/bin", |r| match r {
            EnvironmentTaskRequest::Start { .. } => Ok(started()),
            EnvironmentTaskRequest::Input { close: false, .. } => {
                writes += 1;
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Input { close: true, .. } => {
                closes += 1;
                assert_eq!(closes, 1);
                Err("task_relay_output_unknown".into())
            }
            EnvironmentTaskRequest::QueryInputReceipt {
                operation_id,
                request_digest,
                ..
            } => {
                queries += 1;
                assert_eq!(operation_id, operation(&fence(), "tool-1", "close"));
                assert_eq!(
                    request_digest,
                    environment_task_input_digest(&[], true).unwrap()
                );
                Ok(EnvironmentTaskResult::Mutated {
                    task: snapshot(false),
                })
            }
            EnvironmentTaskRequest::Query { .. } => Ok(finished(false)),
            _ => panic!("must not repeat Start/Input or terminate on ACK loss"),
        })
        .unwrap();
        assert_eq!(result, b"hello");
        assert_eq!((writes, closes, queries), (1, 1, 1));
    }

    #[test]
    fn helper_rejects_unconfirmed_eof_receipt_before_querying_output() {
        for invalid in [
            EnvironmentTaskResult::Listed { tasks: Vec::new() },
            EnvironmentTaskResult::Mutated {
                task: EnvironmentTaskSnapshot {
                    task_id: "different-task".into(),
                    ..snapshot(false)
                },
            },
            EnvironmentTaskResult::Mutated {
                task: EnvironmentTaskSnapshot {
                    operation_id: operation(&fence(), "different-tool", "start"),
                    ..snapshot(false)
                },
            },
        ] {
            let mut queries = 0;
            let mut starts = 0;
            let mut data_inputs = 0;
            let result =
                run_stdin_fixture(&fence(), "tool-1", b"frame", 10, "/usr/bin", |r| match r {
                    EnvironmentTaskRequest::Start { .. } => {
                        starts += 1;
                        Ok(started())
                    }
                    EnvironmentTaskRequest::Input { close: false, .. } => {
                        data_inputs += 1;
                        Ok(EnvironmentTaskResult::Mutated {
                            task: snapshot(false),
                        })
                    }
                    EnvironmentTaskRequest::Input { close: true, .. } => Ok(invalid.clone()),
                    EnvironmentTaskRequest::QueryInputReceipt { .. } => {
                        Err("task_operation_unknown".into())
                    }
                    EnvironmentTaskRequest::Query { .. } => {
                        queries += 1;
                        Ok(finished(false))
                    }
                    _ => panic!("unconfirmed input must not repeat a mutation"),
                });
            assert!(result.is_err(), "EOF needs its exact original task receipt");
            assert_eq!((starts, data_inputs, queries), (1, 1, 0));
        }
    }

    #[test]
    fn helper_rejects_foreign_original_input_receipts_without_eof_or_rewrite() {
        for change in 0..4 {
            let mut task = snapshot(false);
            match change {
                0 => task.task_id = "different-task".into(),
                1 => task.scope = EnvironmentTaskScope::Environment,
                2 => task.operation_id = operation(&fence(), "different-tool", "start"),
                _ => (),
            }
            let mut data_inputs = 0;
            let mut receipt_queries = 0;
            let result =
                run_stdin_fixture(&fence(), "tool-1", b"frame", 10, "/usr/bin", |r| match r {
                    EnvironmentTaskRequest::Start { .. } => Ok(started()),
                    EnvironmentTaskRequest::Input { close: false, .. } => {
                        data_inputs += 1;
                        Err("task_relay_output_unknown".into())
                    }
                    EnvironmentTaskRequest::QueryInputReceipt { .. } => {
                        receipt_queries += 1;
                        if change == 3 {
                            Ok(EnvironmentTaskResult::Listed { tasks: Vec::new() })
                        } else {
                            Ok(EnvironmentTaskResult::Mutated { task: task.clone() })
                        }
                    }
                    _ => panic!(
                        "invalid receipt must not authorize EOF, output query, or mutation replay"
                    ),
                });
            assert!(result.is_err());
            assert_eq!((data_inputs, receipt_queries), (1, 1));
        }
    }

    #[test]
    fn helper_unknown_input_never_rewrites_bytes_or_sends_eof() {
        let mut starts = 0;
        let mut inputs = 0;
        let result = run_stdin_fixture(&fence(), "tool-1", b"frame", 10, "/usr/bin", |r| match r {
            EnvironmentTaskRequest::Start { .. } => {
                starts += 1;
                Ok(started())
            }
            EnvironmentTaskRequest::Input { close: false, .. } => {
                inputs += 1;
                Err("task_relay_output_unknown".into())
            }
            EnvironmentTaskRequest::QueryInputReceipt { .. } => {
                Err("task_operation_unknown".into())
            }
            _ => panic!("unknown input must preserve its task and never resend input/EOF"),
        });
        assert!(result.unwrap_err().contains("input outcome is unknown"));
        assert_eq!((starts, inputs), (1, 1));
    }

    #[test]
    fn file_helper_delivers_exact_stdin_and_requires_bounded_complete_output() {
        let input = vec![b'x'; 5000];
        let mut received = Vec::new();
        let mut closed = false;
        let result = run_stdin_fixture(&fence(), "tool-1", &input, 10, "/usr/bin", |r| match r {
            EnvironmentTaskRequest::Start {
                program,
                args,
                scope,
                ..
            } => {
                assert_eq!(program, super::super::AGENT_BINARY);
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
        let result = run_stdin_fixture(&fence(), "tool-1", b"", 4, "/usr/bin", |_| {
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
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run-a".into(),
            },
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
            matches!(&requests[0], EnvironmentTaskRequest::Start { scope:EnvironmentTaskScope::Run {run_id}, program, args, .. } if run_id=="run-a" && program=="/bin/bash" && args.iter().any(|a|a=="printf hello"))
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
        assert!(error.internal_debug_message().contains("exitCode=Some(0)"));
        assert!(!error.is_cancellation_indeterminate());
        assert!(error.model_visible_message().contains("output"));
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
        assert!(!error.is_cancellation_indeterminate());
        assert!(error.model_visible_message().contains("start"));
        assert!(error.model_visible_message().contains("unknown"));
    }

    #[test]
    fn input_capacity_does_not_claim_the_command_never_started() {
        let mut calls = 0;
        let error = run_stdin_fixture(&fence(), "tool-1", b"request", 100, "/bin", |request| {
            calls += 1;
            match request {
                EnvironmentTaskRequest::Start { .. } => Ok(started()),
                EnvironmentTaskRequest::Input { .. } => Err("task_input_receipt_capacity".into()),
                _ => panic!("must not repeat or terminate after failed input"),
            }
        })
        .unwrap_err();
        assert_eq!(calls, 2);
        assert!(error.contains("input rejected"));
        assert!(error.contains("already started"));
        assert!(!error.contains("cancellation"));
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
    fn same_run_controller_reconstruction_reads_original_task_without_repeating_input() {
        let current = EnvironmentTaskFence {
            controller_epoch: 2,
            ..fence()
        };
        let mut replies = VecDeque::from([
            started(),
            EnvironmentTaskResult::Mutated {
                task: snapshot(false),
            },
            finished(false),
        ]);
        let out = run_command(&current, "tool-1", command(), None, "/bin", |_| {
            replies.pop_front().ok_or("unexpected request".into())
        })
        .unwrap();
        assert_eq!(out.process.stdout, "hello");
        assert!(replies.is_empty());
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
        assert!(!result.unwrap_err().is_cancellation_indeterminate());
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
            current.controller_owner = EnvironmentTaskOwner::AgentRun { run_id: run.into() };
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
        assert!(EnvironmentTaskFence {
            controller_epoch: 3,
            ..fence()
        }
        .matches_original_operation_id(&identities[0], &identities[1]));
        assert!(!EnvironmentTaskFence {
            controller_epoch: 4,
            ..fence()
        }
        .matches_original_operation_id(&identities[0], &identities[2]));
    }

    #[test]
    fn typed_owner_kind_separates_equal_command_identities() {
        let run = fence();
        let maintenance = EnvironmentTaskFence {
            controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
                operation_id: "run-a".into(),
            },
            ..run.clone()
        };
        assert_ne!(
            operation(&run, "same", "start"),
            operation(&maintenance, "same", "start")
        );
    }

    #[test]
    fn authorized_maintenance_commands_use_operation_scope_without_a_fake_run() {
        let maintenance = EnvironmentTaskFence {
            controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
                operation_id: "maintenance-1".into(),
            },
            ..fence()
        };
        let result = run_command(&maintenance, "tool-1", command(), None, "/bin", |request| {
            request.validate_authority()?;
            let EnvironmentTaskRequest::Start {
                operation_id,
                scope,
                fence,
                ..
            } = request
            else {
                panic!("completed command must not signal or restart");
            };
            assert_eq!(fence.controller_owner, maintenance.controller_owner);
            assert_eq!(
                scope,
                EnvironmentTaskScope::Operation {
                    operation_id: "maintenance-1".into()
                }
            );
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
        });
        assert!(result.is_ok(), "{result:?}");
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
