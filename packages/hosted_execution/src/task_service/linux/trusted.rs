use super::*;
use crate::host_supervisor::{HostHelperRequest, TrustedHelperKind, HELPER_STREAM_LIMIT};
use std::os::fd::FromRawFd;

fn static_elf(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 64 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err("host_helper_requires_static_elf".into());
    }
    let offset = u64::from_le_bytes(bytes[32..40].try_into().expect("ELF field"));
    let size = u16::from_le_bytes(bytes[54..56].try_into().expect("ELF field")) as u64;
    let count = u16::from_le_bytes(bytes[56..58].try_into().expect("ELF field")) as u64;
    if size < 56 || count > 1024 {
        return Err("host_helper_requires_static_elf".into());
    }
    for index in 0..count {
        let start = offset
            .checked_add(
                index
                    .checked_mul(size)
                    .ok_or("host_helper_requires_static_elf")?,
            )
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("host_helper_requires_static_elf")?;
        let field = bytes
            .get(
                start
                    ..start
                        .checked_add(4)
                        .ok_or("host_helper_requires_static_elf")?,
            )
            .ok_or("host_helper_requires_static_elf")?;
        if u32::from_le_bytes(field.try_into().expect("ELF field")) == 3 {
            return Err("host_helper_requires_static_elf".into());
        }
    }
    Ok(())
}
fn sealed_file(bytes: &[u8]) -> Result<File, String> {
    // SAFETY: the constant name is NUL terminated; ownership transfers to File.
    let fd = unsafe {
        libc::memfd_create(
            c"centaeris-helper".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err("host_helper_sealing_failed".into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .map_err(|_| "host_helper_sealing_failed")?;
    // A workload can reopen /proc/self/fd/3 with O_RDWR. Seals, unlike open
    // flags or a read-only pathname bind, prevent modification of this object.
    if unsafe {
        libc::fcntl(
            fd,
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } < 0
    {
        return Err("host_helper_sealing_failed".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "host_helper_sealing_failed")?;
    Ok(file)
}

pub(crate) struct HelperBinary {
    pub file: File,
    pub digest: String,
}
impl HelperBinary {
    pub(crate) fn load(path: &str, expected: &str) -> Result<Self, String> {
        crate::host_supervisor::linux::protected(Path::new(path), false)?;
        let mut data = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "host_helper_unavailable")?
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "host_helper_unavailable")?;
        if data.len() > 128 * 1024 * 1024
            || format!("sha256:{:x}", Sha256::digest(&data)) != expected
        {
            return Err("host_helper_digest_mismatch".into());
        }
        static_elf(&data)?;
        Ok(Self {
            file: sealed_file(&data)?,
            digest: expected.into(),
        })
    }
}

pub(super) struct HelperCapture {
    stdout: File,
    stderr: File,
    written: [u64; 2],
    finished: [bool; 2],
    limit: u64,
    paths: [PathBuf; 2],
    pub(super) retained: bool,
}
impl Drop for HelperCapture {
    fn drop(&mut self) {
        if !self.retained {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}
impl HelperCapture {
    fn open(root: &Path, id: &str, limit: u64, existing: bool) -> Result<Self, String> {
        let mut charged = 0u64;
        for entry in fs::read_dir(root).map_err(|_| "host_helper_output_unavailable")? {
            let metadata = entry
                .map_err(|_| "host_helper_output_unavailable")?
                .metadata()
                .map_err(|_| "host_helper_output_unavailable")?;
            if !metadata.is_file() {
                return Err("host_helper_output_unavailable".into());
            }
            charged = charged
                .checked_add(charged_bytes(&metadata))
                .ok_or("host_helper_output_capacity")?;
        }
        if !existing
            && charged
                .checked_add(limit)
                .is_none_or(|n| n > 2 * HELPER_STREAM_LIMIT)
        {
            return Err("host_helper_output_capacity".into());
        }
        let open = |suffix| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(root.join(format!("{id}.{suffix}")))
                .map_err(|_| "host_helper_output_unavailable")
        };
        let stdout = open("stdout")?;
        let stderr = open("stderr")?;
        if !existing {
            if stdout
                .metadata()
                .map_err(|_| "host_helper_output_unavailable")?
                .len()
                != 0
                || stderr
                    .metadata()
                    .map_err(|_| "host_helper_output_unavailable")?
                    .len()
                    != 0
            {
                return Err("host_helper_output_history_conflict".into());
            }
            // Reserve the full possible result before exec; completion shrinks
            // this private sparse reservation to the exact retained byte count.
            stdout
                .set_len(limit)
                .map_err(|_| "host_helper_output_capacity")?;
        }
        Ok(Self {
            stdout,
            stderr,
            written: [0, 0],
            finished: [false, false],
            limit,
            paths: [
                root.join(format!("{id}.stdout")),
                root.join(format!("{id}.stderr")),
            ],
            retained: existing,
        })
    }
    pub(super) fn append(
        &mut self,
        stream: &EnvironmentTaskStream,
        data: &[u8],
    ) -> Result<(), String> {
        let slot = usize::from(*stream == EnvironmentTaskStream::Stderr);
        if self
            .written
            .iter()
            .sum::<u64>()
            .checked_add(data.len() as u64)
            .is_none_or(|n| n > self.limit)
        {
            return Err("host_helper_output_too_large".into());
        }
        let file = if slot == 0 {
            &mut self.stdout
        } else {
            &mut self.stderr
        };
        file.seek(SeekFrom::Start(self.written[slot]))
            .and_then(|_| file.write_all(data))
            .map_err(|_| "host_helper_output_unavailable")?;
        self.written[slot] += data.len() as u64;
        Ok(())
    }
    pub(super) fn finish(&mut self, stream: &EnvironmentTaskStream) -> Result<(), String> {
        self.finished[usize::from(*stream == EnvironmentTaskStream::Stderr)] = true;
        if self.finished == [true, true] {
            self.stdout
                .set_len(self.written[0])
                .and_then(|_| self.stdout.sync_all())
                .map_err(|_| "host_helper_output_unavailable")?;
            self.stderr
                .set_len(self.written[1])
                .and_then(|_| self.stderr.sync_all())
                .map_err(|_| "host_helper_output_unavailable")?;
        }
        Ok(())
    }
}

pub(super) struct TrustedLaunch {
    pub request: HostHelperRequest,
    pub binary: Arc<HelperBinary>,
    pub input: File,
    pub capture: Arc<Mutex<HelperCapture>>,
    pub input_guard: Option<Arc<dyn Send + Sync>>,
}
pub(super) struct ReservedInput {
    send: Option<mpsc::Sender<ChildStdin>>,
    worker: Option<thread::JoinHandle<bool>>,
}
impl ReservedInput {
    pub(super) fn new(
        mut input: File,
        guard: Option<Arc<dyn Send + Sync>>,
    ) -> Result<Self, String> {
        let (send, receive) = mpsc::channel::<ChildStdin>();
        let worker = thread::Builder::new()
            .name("helper-stdin".into())
            .spawn(move || {
                let result = if let Ok(mut stdin) = receive.recv() {
                    std::io::copy(&mut input, &mut stdin)
                        .and_then(|_| stdin.flush())
                        .is_ok()
                } else {
                    false
                };
                drop(input);
                drop(guard);
                result
            })
            .map_err(|_| "task_thread_capacity")?;
        Ok(Self {
            send: Some(send),
            worker: Some(worker),
        })
    }
    pub(super) fn deliver(mut self, input: ChildStdin) -> Result<thread::JoinHandle<bool>, String> {
        self.send
            .take()
            .expect("reserved input")
            .send(input)
            .map_err(|_| "task_worker_delivery_failed")?;
        Ok(self.worker.take().expect("reserved input worker"))
    }
}
impl Drop for ReservedInput {
    fn drop(&mut self) {
        self.send.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl TrustedLaunch {
    pub(super) fn identity(&self) -> Result<String, String> {
        helper_identity(&self.request, &self.binary.digest)
    }
    pub(super) fn process(
        &self,
        baseline: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut process = crate::host_supervisor::task_process(
            baseline,
            "/proc/self/fd/3",
            &[self.request.kind.entrypoint().into()],
            baseline["cwd"].as_str().ok_or("host_process_invalid")?,
            &BTreeMap::new(),
        )?;
        let (root, user) = crate::host_supervisor::workspace_context(baseline)?;
        process["env"] = serde_json::json!([
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            "LANG=C.UTF-8",
            format!("CENTAERIS_WORKSPACE_ROOT={root}"),
            format!("CENTAERIS_WORKSPACE_USER={user}")
        ]);
        if !matches!(
            self.request.kind,
            TrustedHelperKind::QuiesceAgentProcesses | TrustedHelperKind::FileSystem
        ) {
            process["user"] = serde_json::json!({"uid":0,"gid":0,"additionalGids":[]});
            let caps =
                serde_json::json!(["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_KILL"]);
            process["capabilities"] = serde_json::json!({"bounding":caps,"permitted":caps,"effective":caps,"inheritable":[],"ambient":[]});
        }
        Ok(process)
    }
}
fn helper_identity(request: &HostHelperRequest, binary_digest: &str) -> Result<String, String> {
    digest(&(
        &request.kind,
        &request.input_digest,
        request.input_length,
        binary_digest,
    ))
}

impl NativeManager {
    fn helper_start_request(&self, request: &HostHelperRequest) -> EnvironmentTaskRequest {
        EnvironmentTaskRequest::Start {
            fence: request.fence.clone(),
            operation_id: request.operation_id.clone(),
            scope: request.scope.clone(),
            program: "/proc/self/fd/3".into(),
            args: vec![request.kind.entrypoint().into()],
            env: BTreeMap::new(),
            cwd: self.workspace_root.clone(),
            timeout_ms: request.kind.timeout_ms(),
        }
    }
    pub(crate) fn start_helper(
        &self,
        request: &HostHelperRequest,
        mut input: File,
        input_guard: Option<Arc<dyn Send + Sync>>,
    ) -> Result<EnvironmentTaskResult, String> {
        request.validate()?;
        // Serialize each instance's capacity check and reservation through the
        // persistent start boundary. Tasks and completion polling stay independent.
        let _admission = self
            .helper_admission
            .lock()
            .map_err(|_| "task_service_failed")?;
        let binary = self
            .helper_binary
            .as_ref()
            .ok_or("host_helper_not_configured")?
            .clone();
        if input
            .metadata()
            .map_err(|_| "host_helper_input_incomplete")?
            .len()
            != request.input_length
        {
            return Err("host_helper_input_incomplete".into());
        }
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| "host_helper_input_incomplete")?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|_| "host_helper_input_incomplete")?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if format!("sha256:{:x}", hash.finalize()) != request.input_digest {
            return Err("host_helper_input_digest_mismatch".into());
        }
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| "host_helper_input_incomplete")?;
        let task_id = task_identity(&request.fence, &request.operation_id)?;
        let existing = {
            let manager = self.manager.lock().map_err(|_| "task_service_failed")?;
            manager.journal.check(&request.fence)?;
            manager.journal.check_execution(&request.fence)?;
            manager.journal.tasks.contains_key(&task_id)
                || store::task(&self.control, &task_id)?.is_some()
        };
        let capture = Arc::new(Mutex::new(HelperCapture::open(
            &self.helper_root,
            &task_id,
            request.kind.output_limit(),
            existing,
        )?));
        let launch = TrustedLaunch {
            request: request.clone(),
            binary,
            input,
            capture,
            input_guard,
        };
        Manager::handle_with_helper(
            &self.manager,
            self.helper_start_request(request),
            Some(&launch),
        )
    }
    pub(crate) fn helper_output(
        &self,
        request: &HostHelperRequest,
        task_id: &str,
    ) -> Result<(EnvironmentTaskSnapshot, File, File), String> {
        request.validate()?;
        if task_id.is_empty()
            || !task_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("task_invalid_id".into());
        }
        let task = {
            let manager = self.manager.lock().map_err(|_| "task_service_failed")?;
            manager.journal.check(&request.fence)?;
            let record = manager
                .journal
                .tasks
                .get(task_id)
                .cloned()
                .or(store::task(&self.control, task_id)?)
                .ok_or("task_not_found")?;
            let binary = self
                .helper_binary
                .as_ref()
                .ok_or("host_helper_not_configured")?;
            let expected = task_start_digest(
                &self.helper_start_request(request),
                Some(helper_identity(request, &binary.digest)?),
            )?;
            if record.digest != expected
                || !request.fence.matches_start_receipt(
                    &record.snapshot,
                    &request.operation_id,
                    &request.scope,
                )
            {
                return Err("host_helper_output_identity_mismatch".into());
            }
            if record.snapshot.state != EnvironmentTaskState::Exited
                || !record.snapshot.output_complete
                || !record.snapshot.cleanup_complete
            {
                return Err("host_helper_output_pending".into());
            }
            record.snapshot
        };
        let open = |suffix| {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(self.helper_root.join(format!("{task_id}.{suffix}")))
                .map_err(|_| "host_helper_output_unavailable".to_owned())
        };
        Ok((task, open("stdout")?, open("stderr")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_helper_releases_its_output_reservation_for_retry() {
        let root = std::env::temp_dir().join(format!(
            "helper-reservation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let capture = HelperCapture::open(&root, "task-rejected", 128, false).unwrap();
        drop(capture);
        let remaining = fs::read_dir(&root).unwrap().count();
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(remaining, 0, "a task rejected before persistent acceptance must not consume its operation's output reservation");
    }
    #[test]
    fn accepted_helper_keeps_exact_output_for_lost_acknowledgement_replay() {
        let root = std::env::temp_dir().join(format!(
            "helper-replay-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let mut capture = HelperCapture::open(&root, "task-accepted", 128, false).unwrap();
        capture.retained = true;
        capture
            .append(&EnvironmentTaskStream::Stdout, b"result")
            .unwrap();
        capture
            .append(&EnvironmentTaskStream::Stderr, b"diagnostic")
            .unwrap();
        capture.finish(&EnvironmentTaskStream::Stdout).unwrap();
        capture.finish(&EnvironmentTaskStream::Stderr).unwrap();
        drop(capture);
        drop(HelperCapture::open(&root, "task-accepted", 128, true).unwrap());
        assert_eq!(
            fs::read(root.join("task-accepted.stdout")).unwrap(),
            b"result"
        );
        assert_eq!(
            fs::read(root.join("task-accepted.stderr")).unwrap(),
            b"diagnostic"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn trusted_helper_rejects_a_workload_controlled_dynamic_interpreter() {
        let mut elf = vec![0u8; 120];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        static_elf(&elf).unwrap();
        elf[64..68].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(
            static_elf(&elf).unwrap_err(),
            "host_helper_requires_static_elf"
        );
    }
    #[test]
    fn trusted_binary_descriptor_cannot_reopen_into_a_host_file_write() {
        let mut file = sealed_file(b"immutable helper").unwrap();
        let mut reopened = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .unwrap();
        assert!(reopened.write_all(b"attacker").is_err());
        assert!(reopened.set_len(0).is_err());
        let mut data = Vec::new();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_to_end(&mut data).unwrap();
        assert_eq!(data, b"immutable helper");
    }
}
