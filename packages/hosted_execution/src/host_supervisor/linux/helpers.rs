//! Bounded binary frames for fixed host helpers. Input is verified before launch.
use super::super::{
    HostHelperRequest, HostSupervisorResponse, HostSupervisorResult, TrustedHelperKind,
    HOST_SUPERVISOR_SCHEMA,
};
use centaeris_core::execution::EnvironmentTaskSnapshot;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

const INPUT_BUDGET: u64 = 2 * super::super::HELPER_STREAM_LIMIT;
static RESERVED_INPUT: AtomicU64 = AtomicU64::new(0);

pub(super) struct VerifiedInput {
    file: File,
    permit: std::sync::Arc<InputReservation>,
}
struct InputReservation(u64);
impl Drop for InputReservation {
    fn drop(&mut self) {
        RESERVED_INPUT.fetch_sub(self.0, Ordering::AcqRel);
    }
}

fn reserve_input(root: &Path, bytes: u64) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(root.as_os_str().as_bytes())
        .map_err(|_| "host_helper_buffer_unavailable")?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL-terminated and the output has the platform statvfs layout.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err("host_helper_buffer_unavailable".into());
    }
    // SAFETY: successful statvfs initialized the output.
    let stats = unsafe { stats.assume_init() };
    let available = stats.f_bavail.saturating_mul(stats.f_frsize);
    if bytes > 0 && bytes > available.saturating_sub(64 * 1024 * 1024) {
        return Err("host_helper_input_capacity".into());
    }
    RESERVED_INPUT
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(bytes).filter(|n| *n <= INPUT_BUDGET)
        })
        .map_err(|_| "host_helper_input_capacity".to_owned())?;
    Ok(bytes)
}

pub(super) fn receive_input(
    root: &Path,
    request: &HostHelperRequest,
    reader: &mut impl Read,
) -> Result<VerifiedInput, String> {
    request.validate()?;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = root.join(format!(
        ".helper-input-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| "host_helper_buffer_unavailable")?;
    // Keep only a private descriptor. Interrupted uploads consume no persistent spool.
    fs::remove_file(&path).map_err(|_| "host_helper_buffer_unavailable")?;
    let mut prepared = VerifiedInput {
        file,
        permit: std::sync::Arc::new(InputReservation(reserve_input(root, request.input_length)?)),
    };
    let file = &mut prepared.file;
    let mut remaining = request.input_length;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        reader
            .read_exact(&mut buffer[..wanted])
            .map_err(|_| "host_helper_input_incomplete")?;
        digest.update(&buffer[..wanted]);
        file.write_all(&buffer[..wanted])
            .map_err(|_| "host_helper_buffer_unavailable")?;
        remaining -= wanted as u64;
    }
    if format!("sha256:{:x}", digest.finalize()) != request.input_digest {
        return Err("host_helper_input_digest_mismatch".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "host_helper_buffer_unavailable")?;
    Ok(prepared)
}

pub(super) fn dispatch_helper(
    manager: &crate::task_service::linux::NativeManager,
    request: &HostHelperRequest,
    input: VerifiedInput,
    writer: &mut impl Write,
) -> Result<(), String> {
    use centaeris_core::execution::EnvironmentTaskResult;
    let result = manager.start_helper(
        request,
        input
            .file
            .try_clone()
            .map_err(|_| "host_helper_buffer_unavailable")?,
        Some(input.permit.clone()),
    )?;
    let task = match &result {
        EnvironmentTaskResult::Task { task, .. } => task.clone(),
        _ => return Err("host_helper_start_unconfirmed".into()),
    };
    if !request
        .fence
        .matches_start_receipt(&task, &request.operation_id, &request.scope)
    {
        return Err("host_helper_start_unconfirmed".into());
    }
    let response = HostSupervisorResponse::Success {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        result: HostSupervisorResult::Task {
            response: centaeris_core::execution::EnvironmentTaskResponse::from_result(Ok(result)),
        },
    };
    serde_json::to_writer(&mut *writer, &response)
        .map_err(|_| "host_helper_response_unavailable")?;
    writer
        .write_all(b"\n")
        .map_err(|_| "host_helper_response_unavailable".into())
}

pub(super) fn send_completed(
    writer: &mut impl Write,
    task: EnvironmentTaskSnapshot,
    stdout: &mut File,
    stderr: &mut File,
    kind: TrustedHelperKind,
) -> Result<(), String> {
    if !task.output_complete {
        return Err("host_helper_output_pending".into());
    }
    let stdout_length = stdout
        .metadata()
        .map_err(|_| "host_helper_output_unavailable")?
        .len();
    let stderr_length = stderr
        .metadata()
        .map_err(|_| "host_helper_output_unavailable")?
        .len();
    if stdout_length
        .checked_add(stderr_length)
        .is_none_or(|n| n > kind.output_limit())
    {
        return Err("host_helper_output_too_large".into());
    }
    let response = HostSupervisorResponse::Success {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        result: HostSupervisorResult::HelperFinished {
            task,
            stdout_length,
            stderr_length,
        },
    };
    serde_json::to_writer(&mut *writer, &response)
        .map_err(|_| "host_helper_response_unavailable")?;
    writer
        .write_all(b"\n")
        .map_err(|_| "host_helper_response_unavailable")?;
    for (file, length) in [(stdout, stdout_length), (stderr, stderr_length)] {
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "host_helper_output_unavailable")?;
        let sent = std::io::copy(&mut file.take(length), writer)
            .map_err(|_| "host_helper_response_unavailable")?;
        if sent != length {
            return Err("host_helper_output_incomplete".into());
        }
    }
    writer
        .flush()
        .map_err(|_| "host_helper_response_unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::*;

    fn request(bytes: &[u8]) -> HostHelperRequest {
        HostHelperRequest {
            fence: EnvironmentTaskFence {
                environment_instance_id: format!("instance_{}", "a".repeat(64)),
                controller_owner: EnvironmentTaskOwner::AgentRun {
                    run_id: "run".into(),
                },
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
            operation_id: "1:input".into(),
            scope: EnvironmentTaskScope::Run {
                run_id: "run".into(),
            },
            kind: TrustedHelperKind::MaterializeInput,
            input_digest: format!("sha256:{:x}", Sha256::digest(bytes)),
            input_length: bytes.len() as u64,
        }
    }

    #[test]
    fn helper_buffer_verifies_full_input_before_returning_an_executable_descriptor() {
        let root = std::env::temp_dir();
        let body = b"metadata\n\0binary\xff";
        let request = request(body);
        let mut prepared = receive_input(&root, &request, &mut body.as_slice()).unwrap();
        let mut actual = Vec::new();
        prepared.file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, body);
        let mut corrupt = body.to_vec();
        corrupt[0] ^= 1;
        assert!(
            matches!(receive_input(&root,&request,&mut corrupt.as_slice()),Err(e) if e=="host_helper_input_digest_mismatch")
        );
        assert!(receive_input(&root, &request, &mut &body[..4]).is_err());
    }

    #[test]
    fn helper_header_bounds_input_before_consuming_the_binary_stream() {
        struct NoRead;
        impl Read for NoRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("invalid header must not read input")
            }
        }
        let mut request = request(b"");
        request.kind = TrustedHelperKind::InputInventory;
        request.input_length = 1;
        assert!(receive_input(&std::env::temp_dir(), &request, &mut NoRead).is_err());
        request.input_length = 0;
        request.fence.controller_purpose = EnvironmentTaskControllerPurpose::Cleanup;
        assert!(receive_input(&std::env::temp_dir(), &request, &mut NoRead).is_err());
    }
}
