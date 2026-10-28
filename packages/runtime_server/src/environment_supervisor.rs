//! Bounded private host transport; workload containers receive no control channel.
use centaeris_core::execution::{EnvironmentTaskEnvelope, EnvironmentTaskResult};
use centaeris_core::execution::{EnvironmentTaskSnapshot, EnvironmentTaskState};
use hosted_execution::host_supervisor::HostHelperRequest;
use hosted_execution::host_supervisor::{
    HostLaunchTarget, HostSupervisorEnvelope, HostSupervisorRequest, HostSupervisorResponse,
    HostSupervisorResult, HOST_SUPERVISOR_SCHEMA,
};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};

const REQUEST_LIMIT: usize = 128 * 1024;
const RESPONSE_LIMIT: usize = 1024 * 1024;

fn safe_error(error: &str) -> String {
    if !matches!(error, "host_busy" | "environment_host_busy")
        && !error.is_empty()
        && error.len() <= 96
        && error
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        error.into()
    } else {
        "environment_host_response_rejected".into()
    }
}

fn write_request(
    stream: &mut (impl Write + ?Sized),
    request: HostSupervisorRequest,
) -> Result<(), String> {
    let envelope = HostSupervisorEnvelope {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        request,
    };
    envelope.validate()?;
    let bytes = serde_json::to_vec(&envelope).map_err(|_| "environment_host_request_invalid")?;
    if bytes.len() + 1 > REQUEST_LIMIT {
        return Err("environment_host_request_too_large".into());
    }
    stream
        .write_all(&bytes)
        .and_then(|()| stream.write_all(b"\n"))
        .map_err(|_| "environment_host_transport_unavailable".into())
}
fn read_response(reader: &mut impl BufRead) -> Result<HostSupervisorResult, String> {
    let mut line = Vec::new();
    reader
        .take((RESPONSE_LIMIT + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|_| "environment_host_transport_unavailable")?;
    if line.len() > RESPONSE_LIMIT || line.last() != Some(&b'\n') {
        return Err("environment_host_response_invalid".into());
    }
    let response: HostSupervisorResponse =
        serde_json::from_slice(&line).map_err(|_| "environment_host_response_invalid")?;
    response
        .validate()
        .map_err(|_| "environment_host_response_invalid")?;
    match response {
        HostSupervisorResponse::Success { result, .. } => Ok(result),
        HostSupervisorResponse::Error { error, .. } if error == "host_busy" => {
            Err("environment_host_busy".into())
        }
        HostSupervisorResponse::Error { error, .. } => Err(safe_error(&error)),
    }
}
fn exchange(
    stream: &mut (impl Read + Write + ?Sized),
    request: HostSupervisorRequest,
) -> Result<HostSupervisorResult, String> {
    write_request(stream, request)?;
    stream
        .flush()
        .map_err(|_| "environment_host_transport_unavailable")?;
    read_response(&mut BufReader::new(stream))
}
#[cfg(test)]
fn exchange_helper(
    stream: &mut (impl Read + Write + ?Sized),
    helper: HostHelperRequest,
    input: &mut impl Read,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<EnvironmentTaskSnapshot, String> {
    let mut reader = BufReader::new(stream);
    write_helper_input(&mut reader, &helper, input)?;
    let mut buffer = [0u8; 1];
    let HostSupervisorResult::HelperFinished {
        task,
        stdout_length,
        stderr_length,
    } = read_response(&mut reader)?
    else {
        return Err("environment_host_helper_unconfirmed".into());
    };
    if task.operation_id != helper.operation_id
        || task.scope != helper.scope
        || task.state != EnvironmentTaskState::Exited
        || !task.cleanup_complete
        || !task.output_complete
        || stdout_length
            .checked_add(stderr_length)
            .is_none_or(|n| n > helper.kind.output_limit())
    {
        return Err("environment_host_helper_unconfirmed".into());
    }
    for (length, writer) in [
        (stdout_length, stdout as &mut dyn Write),
        (stderr_length, stderr as &mut dyn Write),
    ] {
        if std::io::copy(&mut reader.by_ref().take(length), writer)
            .map_err(|_| "environment_host_output_unavailable")?
            != length
        {
            return Err("environment_host_output_incomplete".into());
        }
    }
    if reader
        .read(&mut buffer[..1])
        .map_err(|_| "environment_host_output_unavailable")?
        != 0
    {
        return Err("environment_host_output_invalid".into());
    }
    Ok(task)
}

/// A ready receipt authorizes only this upload; it is not task execution success.
fn write_helper_input<S: Read + Write>(
    reader: &mut BufReader<S>,
    helper: &HostHelperRequest,
    input: &mut impl Read,
) -> Result<(), String> {
    helper.validate()?;
    write_request(
        reader.get_mut(),
        HostSupervisorRequest::Helper {
            helper: helper.clone(),
        },
    )?;
    reader
        .get_mut()
        .flush()
        .map_err(|_| "environment_host_transport_unavailable")?;
    match read_response(reader)? {
        HostSupervisorResult::HelperInputReady {
            operation_id,
            input_length,
            input_digest,
        } if operation_id == helper.operation_id
            && input_length == helper.input_length
            && input_digest == helper.input_digest => {}
        _ => return Err("environment_host_helper_admission_unconfirmed".into()),
    }
    let mut remaining = helper.input_length;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let size =
            usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded input chunk");
        let n = input
            .read(&mut buffer[..size])
            .map_err(|_| "environment_host_input_unavailable")?;
        if n == 0 {
            return Err("environment_host_input_incomplete".into());
        }
        hash.update(&buffer[..n]);
        reader
            .get_mut()
            .write_all(&buffer[..n])
            .map_err(|_| "environment_host_transport_unavailable")?;
        remaining -= n as u64;
    }
    if input
        .read(&mut buffer[..1])
        .map_err(|_| "environment_host_input_unavailable")?
        != 0
        || format!("sha256:{:x}", hash.finalize()) != helper.input_digest
    {
        return Err("environment_host_input_rejected".into());
    }
    reader
        .get_mut()
        .flush()
        .map_err(|_| "environment_host_transport_unavailable".into())
}
trait HostStream: Read + Write + Send {}
impl<T: Read + Write + Send> HostStream for T {}

#[cfg(unix)]
fn connection() -> Result<Box<dyn HostStream>, String> {
    use std::os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixStream,
    };
    let path =
        std::env::var("ENVIRONMENT_HOST_SOCKET").map_err(|_| "environment_host_not_configured")?;
    let path = std::path::Path::new(&path);
    if !path.is_absolute()
        || path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return Err("environment_host_configuration_invalid".into());
    }
    // An untrusted writer must not replace the socket between inspection and connect.
    for parent in path.ancestors().skip(1) {
        let metadata =
            std::fs::symlink_metadata(parent).map_err(|_| "environment_host_unavailable")?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
            return Err("environment_host_untrusted".into());
        }
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "environment_host_unavailable")?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err("environment_host_untrusted".into());
    }
    let stream = UnixStream::connect(path).map_err(|_| "environment_host_unavailable")?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .and_then(|()| stream.set_write_timeout(Some(std::time::Duration::from_secs(30))))
        .map_err(|_| "environment_host_transport_unavailable")?;
    Ok(Box::new(stream))
}

#[cfg(not(unix))]
fn connection() -> Result<Box<dyn HostStream>, String> {
    Err("environment_host_platform_unsupported".into())
}

fn send(request: HostSupervisorRequest) -> Result<HostSupervisorResult, String> {
    exchange(&mut *connection()?, request)
}
/// Fetch a bounded sample over the existing trusted host channel.
pub(crate) fn observe_resources(
    host: &str,
    targets: &[HostLaunchTarget],
) -> Result<hosted_execution::host_supervisor::resources::HostResourceObservation, String> {
    observe_resources_with(host, targets, send)
}
fn observe_resources_with(
    host: &str,
    targets: &[HostLaunchTarget],
    transport: impl FnOnce(HostSupervisorRequest) -> Result<HostSupervisorResult, String>,
) -> Result<hosted_execution::host_supervisor::resources::HostResourceObservation, String> {
    hosted_execution::host_supervisor::resources::validate_resource_request(host, targets)?;
    match transport(HostSupervisorRequest::ObserveResources {
        expected_host_id: host.into(),
        targets: targets.to_vec(),
    })? {
        HostSupervisorResult::ResourceObservation { observation } => Ok(*observation),
        _ => Err("environment_host_resources_unconfirmed".into()),
    }
}

fn register(target: &HostLaunchTarget) -> Result<(), String> {
    match send(HostSupervisorRequest::Register {
        target: target.clone(),
    })? {
        HostSupervisorResult::Registered { target: actual } if actual == *target => Ok(()),
        _ => Err("environment_host_registration_rejected".into()),
    }
}
fn helper_receipt_matches(request: &HostHelperRequest, task: &EnvironmentTaskSnapshot) -> bool {
    request
        .fence
        .matches_start_receipt(task, &request.operation_id, &request.scope)
}

pub(crate) fn helper_start(
    target: &HostLaunchTarget,
    request: HostHelperRequest,
    input: &mut impl Read,
) -> Result<EnvironmentTaskSnapshot, String> {
    target.validate()?;
    if target.environment_instance_id != request.fence.environment_instance_id {
        return Err("environment_host_target_rejected".into());
    }
    register(target)?;
    let mut reader = BufReader::new(connection()?);
    write_helper_input(&mut reader, &request, input)?;
    let HostSupervisorResult::Task { response } = read_response(&mut reader)? else {
        return Err("environment_helper_start_unconfirmed".into());
    };
    let result = match response {
        centaeris_core::execution::EnvironmentTaskResponse::Success { result, .. } => result,
        centaeris_core::execution::EnvironmentTaskResponse::Error { error, .. } => {
            return Err(safe_error(&error))
        }
    };
    let task = match result {
        EnvironmentTaskResult::Mutated { task } | EnvironmentTaskResult::Task { task, .. } => task,
        _ => return Err("environment_helper_start_unconfirmed".into()),
    };
    if !helper_receipt_matches(&request, &task) {
        return Err("environment_helper_start_unconfirmed".into());
    }
    Ok(task)
}

pub(crate) struct HelperOutput {
    pub task: EnvironmentTaskSnapshot,
    stdout_length: u64,
    stderr_length: u64,
    reader: BufReader<Box<dyn HostStream>>,
}
impl HelperOutput {
    pub(crate) fn copy_to(
        mut self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<(), String> {
        for (length, writer) in [
            (self.stdout_length, stdout as &mut dyn Write),
            (self.stderr_length, stderr as &mut dyn Write),
        ] {
            if std::io::copy(&mut self.reader.by_ref().take(length), writer)
                .map_err(|_| "environment_host_output_unavailable")?
                != length
            {
                return Err("environment_host_output_incomplete".into());
            }
        }
        let mut extra = [0u8; 1];
        if self
            .reader
            .read(&mut extra)
            .map_err(|_| "environment_host_output_unavailable")?
            != 0
        {
            return Err("environment_host_output_invalid".into());
        }
        Ok(())
    }
}
pub(crate) fn prepare_helper_output(
    target: &HostLaunchTarget,
    helper: HostHelperRequest,
    task_id: &str,
) -> Result<HelperOutput, String> {
    target.validate()?;
    helper.validate()?;
    if target.environment_instance_id != helper.fence.environment_instance_id {
        return Err("environment_host_target_rejected".into());
    }
    let mut stream = connection()?;
    write_request(
        &mut *stream,
        HostSupervisorRequest::HelperOutput {
            helper: helper.clone(),
            task_id: task_id.into(),
        },
    )?;
    stream
        .flush()
        .map_err(|_| "environment_host_transport_unavailable")?;
    let mut reader = BufReader::new(stream);
    let HostSupervisorResult::HelperFinished {
        task,
        stdout_length,
        stderr_length,
    } = read_response(&mut reader)?
    else {
        return Err("environment_host_helper_unconfirmed".into());
    };
    if task.task_id != task_id
        || !helper_receipt_matches(&helper, &task)
        || task.state != EnvironmentTaskState::Exited
        || !task.cleanup_complete
        || !task.output_complete
        || stdout_length
            .checked_add(stderr_length)
            .is_none_or(|n| n > helper.kind.output_limit())
    {
        return Err("environment_host_helper_unconfirmed".into());
    }
    Ok(HelperOutput {
        task,
        stdout_length,
        stderr_length,
        reader,
    })
}

pub(crate) fn helper(
    target: &HostLaunchTarget,
    request: HostHelperRequest,
    input: &mut impl Read,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<EnvironmentTaskSnapshot, String> {
    let mut task = helper_start(target, request.clone(), input)?;
    let start = std::time::Instant::now();
    while task.state != EnvironmentTaskState::Exited
        || !task.cleanup_complete
        || !task.output_complete
    {
        if task.state == EnvironmentTaskState::Unknown
            || start.elapsed()
                > std::time::Duration::from_millis(request.kind.timeout_ms().saturating_add(5000))
        {
            return Err("environment_helper_outcome_unknown".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        let result = crate::environment_supervisor::task(
            target.clone(),
            EnvironmentTaskEnvelope {
                schema: centaeris_core::execution::ENVIRONMENT_TASK_SCHEMA.into(),
                request: centaeris_core::execution::EnvironmentTaskRequest::Query {
                    fence: request.fence.clone(),
                    task_id: task.task_id.clone(),
                    cursor: 0,
                    max_bytes: centaeris_core::execution::ENVIRONMENT_TASK_READ_BYTES as u32,
                },
            },
        )?;
        let EnvironmentTaskResult::Task { task: current, .. } = result else {
            return Err("environment_helper_outcome_unknown".into());
        };
        if current.task_id != task.task_id || !helper_receipt_matches(&request, &current) {
            return Err("environment_helper_outcome_unknown".into());
        }
        task = current;
    }
    let output = prepare_helper_output(target, request, &task.task_id)?;
    let task = output.task.clone();
    output.copy_to(stdout, stderr)?;
    Ok(task)
}

pub(crate) fn inspect(instance: &str) -> Result<serde_json::Value, String> {
    match send(HostSupervisorRequest::Inspect {
        environment_instance_id: instance.into(),
    })? {
        HostSupervisorResult::Inspection { inspection } => Ok(inspection),
        _ => Err("environment_host_inspection_unconfirmed".into()),
    }
}

pub(crate) fn pause_if_idle(
    target: HostLaunchTarget,
) -> Result<hosted_execution::host_supervisor::IdlePauseOutcome, String> {
    pause_if_idle_with(target, send)
}

fn pause_if_idle_with(
    target: HostLaunchTarget,
    transport: impl FnOnce(HostSupervisorRequest) -> Result<HostSupervisorResult, String>,
) -> Result<hosted_execution::host_supervisor::IdlePauseOutcome, String> {
    target.validate()?;
    match transport(HostSupervisorRequest::PauseIfIdle {
        target: target.clone(),
    })? {
        HostSupervisorResult::IdlePause {
            target: actual,
            outcome,
        } if actual == target => Ok(outcome),
        _ => Err("environment_host_idle_pause_unconfirmed".into()),
    }
}

pub(crate) fn retire(target: HostLaunchTarget) -> Result<(), String> {
    retire_with(target, send)
}

fn retire_with(
    target: HostLaunchTarget,
    transport: impl FnOnce(HostSupervisorRequest) -> Result<HostSupervisorResult, String>,
) -> Result<(), String> {
    target.validate()?;
    // Docker is already absent: registering here would attempt to resurrect its
    // physical binding. Host independently verifies absence and its own records.
    match transport(HostSupervisorRequest::Retire {
        target: target.clone(),
    })? {
        HostSupervisorResult::Retired { target: actual } if actual == target => Ok(()),
        _ => Err("environment_host_retirement_unconfirmed".into()),
    }
}

pub(crate) fn task(
    target: HostLaunchTarget,
    envelope: EnvironmentTaskEnvelope,
) -> Result<EnvironmentTaskResult, String> {
    task_with(target, envelope, send)
}

fn task_with(
    target: HostLaunchTarget,
    envelope: EnvironmentTaskEnvelope,
    mut transport: impl FnMut(HostSupervisorRequest) -> Result<HostSupervisorResult, String>,
) -> Result<EnvironmentTaskResult, String> {
    target.validate()?;
    envelope.validate()?;
    envelope.request.validate_authority()?;
    match transport(HostSupervisorRequest::Register {
        target: target.clone(),
    })? {
        HostSupervisorResult::Registered { target: actual } if actual == target => {}
        _ => return Err("environment_host_registration_rejected".into()),
    }
    match transport(HostSupervisorRequest::Task { envelope })? {
        HostSupervisorResult::Task { response } => {
            response.validate()?;
            match response {
                centaeris_core::execution::EnvironmentTaskResponse::Success { result, .. } => {
                    Ok(result)
                }
                centaeris_core::execution::EnvironmentTaskResponse::Error { error, .. } => {
                    Err(safe_error(&error))
                }
            }
        }
        _ => Err("environment_host_task_unconfirmed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::*;
    struct Wire {
        read: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }
    impl Read for Wire {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.read.read(bytes)
        }
    }
    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn target() -> HostLaunchTarget {
        HostLaunchTarget {
            environment_instance_id: format!("instance_{}", "a".repeat(64)),
            container_id: "c".repeat(64),
            environment_id: "environment".into(),
            profile_digest: format!("sha256:{}", "b".repeat(64)),
            image_digest: format!("sha256:{}", "d".repeat(64)),
        }
    }
    #[test]
    fn idle_pause_requires_exact_target_and_preserves_typed_non_success() {
        use hosted_execution::host_supervisor::IdlePauseOutcome;
        let expected = target();
        for outcome in [
            IdlePauseOutcome::Paused,
            IdlePauseOutcome::Busy,
            IdlePauseOutcome::Unknown {
                reason: "task_state_unknown".into(),
            },
        ] {
            let actual = pause_if_idle_with(expected.clone(), |request| {
                let HostSupervisorRequest::PauseIfIdle { target } = request else {
                    panic!("idle pause must use the admission-fenced host operation")
                };
                assert_eq!(target, expected);
                Ok(HostSupervisorResult::IdlePause {
                    target,
                    outcome: outcome.clone(),
                })
            })
            .unwrap();
            assert_eq!(actual, outcome);
        }
        let mut foreign = expected.clone();
        foreign.container_id = "e".repeat(64);
        assert_eq!(
            pause_if_idle_with(expected.clone(), |_| Ok(HostSupervisorResult::IdlePause {
                target: foreign,
                outcome: IdlePauseOutcome::Paused
            })),
            Err("environment_host_idle_pause_unconfirmed".into())
        );
        assert_eq!(
            pause_if_idle_with(expected.clone(), |_| Ok(HostSupervisorResult::Registered {
                target: expected.clone()
            })),
            Err("environment_host_idle_pause_unconfirmed".into())
        );
        assert_eq!(
            pause_if_idle_with(expected, |_| Err(
                "environment_host_transport_unavailable".into()
            )),
            Err("environment_host_transport_unavailable".into())
        );
    }

    #[test]
    fn host_retirement_requires_exact_ack_without_registering_absent_container() {
        let expected = target();
        retire_with(expected.clone(), |request| {
            let HostSupervisorRequest::Retire { target } = request else {
                panic!("retirement cannot register or dispatch a task")
            };
            assert_eq!(target, expected);
            Ok(HostSupervisorResult::Retired {
                target: expected.clone(),
            })
        })
        .unwrap();
        let mut wrong = expected.clone();
        wrong.container_id = "e".repeat(64);
        assert_eq!(
            retire_with(expected.clone(), |_| Ok(HostSupervisorResult::Retired {
                target: wrong
            }))
            .unwrap_err(),
            "environment_host_retirement_unconfirmed"
        );
        assert_eq!(
            retire_with(expected.clone(), |_| Ok(HostSupervisorResult::Registered {
                target: expected.clone()
            }))
            .unwrap_err(),
            "environment_host_retirement_unconfirmed"
        );
        assert_eq!(
            retire_with(expected, |_| Err("host_retirement_busy".into())).unwrap_err(),
            "host_retirement_busy"
        );
    }
    fn envelope() -> EnvironmentTaskEnvelope {
        EnvironmentTaskEnvelope {
            schema: ENVIRONMENT_TASK_SCHEMA.into(),
            request: EnvironmentTaskRequest::List {
                fence: EnvironmentTaskFence {
                    environment_instance_id: target().environment_instance_id,
                    controller_owner: EnvironmentTaskOwner::AgentRun {
                        run_id: "run".into(),
                    },
                    controller_epoch: 1,
                    controller_purpose: EnvironmentTaskControllerPurpose::Execute,
                },
            },
        }
    }
    #[test]
    fn external_supervisor_requires_matching_registration_before_task_dispatch() {
        let expected = target();
        let mut calls = 0;
        let result = task_with(expected.clone(), envelope(), |request| {
            calls += 1;
            match request {
                HostSupervisorRequest::Register { target } => {
                    assert_eq!(target, expected);
                    Ok(HostSupervisorResult::Registered { target })
                }
                HostSupervisorRequest::Task { envelope: actual } => {
                    assert_eq!(actual, envelope());
                    Ok(HostSupervisorResult::Task {
                        response: EnvironmentTaskResponse::from_result(Ok(
                            EnvironmentTaskResult::Listed { tasks: vec![] },
                        )),
                    })
                }
                _ => panic!("unexpected request"),
            }
        })
        .unwrap();
        assert!(matches!(result, EnvironmentTaskResult::Listed { .. }));
        assert_eq!(calls, 2);
        let mut calls = 0;
        assert_eq!(
            task_with(expected, envelope(), |_| {
                calls += 1;
                let mut changed = target();
                changed.container_id = "e".repeat(64);
                Ok(HostSupervisorResult::Registered { target: changed })
            }),
            Err("environment_host_registration_rejected".into())
        );
        assert_eq!(calls, 1);
    }
    #[test]
    fn external_supervisor_jsonl_is_bounded_strict_and_redacts_remote_diagnostics() {
        for response in [b"{}\n".to_vec(), vec![b'x'; RESPONSE_LIMIT + 1], b"{\"status\":\"error\",\"schema\":\"environment.host.v1\",\"error\":\"private token in diagnostics\"}\n".to_vec()] {
            let mut wire = Wire { read: std::io::Cursor::new(response), written: vec![] };
            let error = exchange(&mut wire, HostSupervisorRequest::Register { target: target() }).unwrap_err();
            assert!(!error.contains("private")); assert!(!error.contains("token"));
            let request: HostSupervisorEnvelope = serde_json::from_slice(&wire.written).unwrap(); request.validate().unwrap();
        }
        let mut request = envelope();
        request.request = EnvironmentTaskRequest::Start {
            fence: if let EnvironmentTaskRequest::List { fence } = envelope().request {
                fence
            } else {
                unreachable!()
            },
            operation_id: "1:start".into(),
            scope: EnvironmentTaskScope::Run {
                run_id: "run".into(),
            },
            program: "/bin/sh".into(),
            env: Default::default(),
            args: vec!["x".repeat(REQUEST_LIMIT)],
            cwd: "/".into(),
            timeout_ms: 1000,
        };
        let mut wire = Wire {
            read: std::io::Cursor::new(vec![]),
            written: vec![],
        };
        assert!(exchange(&mut wire, HostSupervisorRequest::Task { envelope: request }).is_err());
        assert!(wire.written.is_empty());
    }
    #[test]
    fn helper_receipts_survive_same_owner_controller_renewal() {
        use centaeris_core::execution::{EnvironmentTaskOwner, EnvironmentTaskScope};
        use hosted_execution::host_supervisor::TrustedHelperKind;
        let original = if let EnvironmentTaskRequest::List { fence } = envelope().request {
            fence
        } else {
            unreachable!()
        };
        let mut request = HostHelperRequest {
            operation_id: original.operation_id("prepare-workspace"),
            scope: EnvironmentTaskScope::Run {
                run_id: "run".into(),
            },
            kind: TrustedHelperKind::PrepareWorkspace,
            input_digest: format!("sha256:{:x}", Sha256::digest([])),
            input_length: 0,
            fence: original.clone(),
        };
        let task = EnvironmentTaskSnapshot {
            task_id: original.task_id(&request.operation_id).unwrap(),
            operation_id: request.operation_id.clone(),
            scope: request.scope.clone(),
            state: EnvironmentTaskState::Exited,
            exit_code: Some(0),
            cleanup_complete: true,
            output_complete: true,
        };
        assert!(helper_receipt_matches(&request, &task));
        request.fence.controller_epoch += 1;
        request.operation_id = request.fence.operation_id("prepare-workspace");
        assert!(helper_receipt_matches(&request, &task));
        let mut changed = request.clone();
        changed.fence.controller_owner = EnvironmentTaskOwner::AgentRun {
            run_id: "foreign".into(),
        };
        assert!(!helper_receipt_matches(&changed, &task));
        changed = request.clone();
        changed.fence.environment_instance_id = "foreign-instance".into();
        assert!(!helper_receipt_matches(&changed, &task));
        changed = request.clone();
        changed.operation_id = changed.fence.operation_id("another-helper");
        assert!(!helper_receipt_matches(&changed, &task));
        let mut changed_task = task.clone();
        changed_task.scope = EnvironmentTaskScope::Environment;
        assert!(!helper_receipt_matches(&request, &changed_task));
        changed_task = task.clone();
        changed_task.operation_id = "99:prepare-workspace".into();
        assert!(!helper_receipt_matches(&request, &changed_task));
    }

    #[test]
    fn helper_transport_keeps_binary_bytes_prefetched_with_the_json_header() {
        use hosted_execution::host_supervisor::{HostHelperRequest, TrustedHelperKind};
        let fence = if let EnvironmentTaskRequest::List { fence } = envelope().request {
            fence
        } else {
            unreachable!()
        };
        let helper = HostHelperRequest {
            operation_id: fence.operation_id("filesystem"),
            scope: EnvironmentTaskScope::Run {
                run_id: "run".into(),
            },
            kind: TrustedHelperKind::FileSystem,
            input_digest: format!("sha256:{:x}", sha2::Sha256::digest(b"input")),
            input_length: 5,
            fence,
        };
        let task = EnvironmentTaskSnapshot {
            task_id: "task".into(),
            operation_id: helper.operation_id.clone(),
            scope: helper.scope.clone(),
            state: EnvironmentTaskState::Exited,
            exit_code: Some(0),
            cleanup_complete: true,
            output_complete: true,
        };
        let response = HostSupervisorResponse::Success {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            result: HostSupervisorResult::HelperFinished {
                task,
                stdout_length: 2 * 1024 * 1024,
                stderr_length: 3,
            },
        };
        let ready = HostSupervisorResponse::Success {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            result: HostSupervisorResult::HelperInputReady {
                operation_id: helper.operation_id.clone(),
                input_length: helper.input_length,
                input_digest: helper.input_digest.clone(),
            },
        };
        let mut bytes = serde_json::to_vec(&ready).unwrap();
        bytes.push(b'\n');
        bytes.extend(serde_json::to_vec(&response).unwrap());
        bytes.push(b'\n');
        bytes.extend(vec![0xff; 2 * 1024 * 1024]);
        bytes.extend(b"err");
        let mut wire = Wire {
            read: std::io::Cursor::new(bytes),
            written: vec![],
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = exchange_helper(
            &mut wire,
            helper.clone(),
            &mut &b"input"[..],
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(stdout, vec![0xff; 2 * 1024 * 1024]);
        assert_eq!(stderr, b"err");
        assert!(wire.written.ends_with(b"\ninput"));
        let mut changed = helper;
        changed.input_length = 6;
        let mut wire = Wire {
            read: std::io::Cursor::new(vec![]),
            written: vec![],
        };
        assert!(exchange_helper(
            &mut wire,
            changed,
            &mut &b"input"[..],
            &mut Vec::new(),
            &mut Vec::new()
        )
        .is_err());
    }
    #[test]
    fn resource_observation_uses_exact_host_and_targets_without_registering_work() {
        let targets = vec![target()];
        let error = observe_resources_with("host", &targets, |request| {
            match request {
                HostSupervisorRequest::ObserveResources {
                    expected_host_id,
                    targets: actual,
                } => {
                    assert_eq!(expected_host_id, "host");
                    assert_eq!(actual, targets);
                }
                _ => panic!("observation must not register, start, or mutate an environment"),
            }
            Err("observed_transport_failure".into())
        })
        .unwrap_err();
        assert_eq!(error, "observed_transport_failure");
    }
    #[test]
    fn resource_observation_rejects_invalid_inventory_before_transport() {
        let mut calls = 0;
        assert!(observe_resources_with("", &[], |_| {
            calls += 1;
            Err("must_not_send".into())
        })
        .is_err());
        assert_eq!(calls, 0);
    }
    #[test]
    fn resource_observation_rejects_unrelated_success_response() {
        let t = target();
        assert_eq!(
            observe_resources_with("host", std::slice::from_ref(&t), |_| {
                Ok(HostSupervisorResult::Registered { target: t.clone() })
            })
            .unwrap_err(),
            "environment_host_resources_unconfirmed"
        );
    }
    #[test]
    fn host_busy_is_only_trusted_from_the_top_level_admission_response() {
        let response =
            b"{\"status\":\"error\",\"schema\":\"environment.host.v1\",\"error\":\"host_busy\"}\n";
        assert_eq!(
            read_response(&mut std::io::Cursor::new(response)).unwrap_err(),
            "environment_host_busy"
        );
        assert_eq!(
            safe_error("host_busy"),
            "environment_host_response_rejected"
        );
        assert_eq!(
            safe_error("environment_host_busy"),
            "environment_host_response_rejected"
        );
        assert_eq!(
            read_response(&mut std::io::Cursor::new(b"" as &[u8])).unwrap_err(),
            "environment_host_response_invalid"
        );
    }

    #[test]
    fn helper_admission_busy_never_reads_or_sends_binary_input() {
        let EnvironmentTaskRequest::List { fence } = envelope().request else {
            unreachable!()
        };
        let helper = HostHelperRequest {
            operation_id: fence.operation_id("filesystem"),
            scope: EnvironmentTaskScope::Run {
                run_id: "run".into(),
            },
            kind: hosted_execution::host_supervisor::TrustedHelperKind::FileSystem,
            input_digest: format!("sha256:{:x}", Sha256::digest(b"input")),
            input_length: 5,
            fence,
        };
        let mut input = std::io::Cursor::new(b"input");
        let mut wire = Wire { read: std::io::Cursor::new(b"{\"status\":\"error\",\"schema\":\"environment.host.v1\",\"error\":\"host_busy\"}\n".to_vec()), written: vec![] };
        let error = exchange_helper(
            &mut wire,
            helper,
            &mut input,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            input.position(),
            0,
            "rejected admission cannot consume input"
        );
        assert_eq!(error, "environment_host_busy");
        assert_eq!(
            wire.written.iter().filter(|&&byte| byte == b'\n').count(),
            1
        );
        assert!(wire.written.ends_with(b"\n"), "header only; no binary body");
    }

    #[test]
    fn helper_upload_requires_matching_ready_identity_and_complete_response() {
        let EnvironmentTaskRequest::List { fence } = envelope().request else {
            unreachable!()
        };
        let helper = HostHelperRequest {
            operation_id: fence.operation_id("filesystem"),
            scope: EnvironmentTaskScope::Run {
                run_id: "run".into(),
            },
            kind: hosted_execution::host_supervisor::TrustedHelperKind::FileSystem,
            input_digest: format!("sha256:{:x}", Sha256::digest(b"input")),
            input_length: 5,
            fence,
        };
        for field in ["operationId", "inputLength", "inputDigest", "transport"] {
            let mut ready = serde_json::json!({"status":"success","schema":HOST_SUPERVISOR_SCHEMA,
                "result":{"kind":"helperInputReady","operationId":helper.operation_id,
                    "inputLength":helper.input_length,"inputDigest":helper.input_digest}});
            match field {
                "operationId" => ready["result"][field] = serde_json::json!("1:foreign"),
                "inputLength" => ready["result"][field] = serde_json::json!(6),
                "inputDigest" => {
                    ready["result"][field] = serde_json::json!(format!("sha256:{}", "a".repeat(64)))
                }
                _ => {}
            }
            let mut bytes = serde_json::to_vec(&ready).unwrap();
            if field != "transport" {
                bytes.push(b'\n');
            }
            let wire = Wire {
                read: std::io::Cursor::new(bytes),
                written: vec![],
            };
            let mut reader = BufReader::new(wire);
            let mut input = std::io::Cursor::new(b"input");
            assert!(
                write_helper_input(&mut reader, &helper, &mut input).is_err(),
                "{field}"
            );
            assert_eq!(input.position(), 0, "{field}");
            assert!(reader.get_ref().written.ends_with(b"\n"), "{field}");
        }
    }
}
