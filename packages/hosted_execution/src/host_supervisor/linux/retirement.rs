//! Exact-instance host spool retirement. Runtime remains the durable cleanup authority.
use super::*;

pub(super) struct RetirementStore {
    control: PathBuf,
    output: PathBuf,
    owner: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    registration: Registration,
    evidence: Option<Evidence>,
}

/// Temporary evidence is durable before unlinking any spool. PostgreSQL owns
/// the lasting cleanup receipt; successful host cleanup removes this intent.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Evidence {
    container_absent: bool,
    workers_quiesced: bool,
    control_files: u64,
    control_bytes: u64,
    control_sha256: String,
    output_files: u64,
    output_bytes: u64,
}

impl RetirementStore {
    pub(super) fn new(config: &HostSupervisorConfig) -> Self {
        Self {
            control: config.control_root.clone().into(),
            output: config.output_root.clone().into(),
            owner: 0,
        }
    }
    fn intent_path(&self, id: &str) -> PathBuf {
        self.control.join(format!("{id}.retire.json"))
    }
    fn registration_path(&self, id: &str) -> PathBuf {
        self.control.join(format!("{id}.target.json"))
    }
    fn roots(&self, id: &str) -> [PathBuf; 3] {
        [
            self.output.join(id),
            self.output.join(format!("{id}.helpers")),
            self.control.join(id),
        ]
    }
    fn metadata(&self, path: &Path) -> Result<Option<fs::Metadata>, String> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("host_retirement_state_unknown".into()),
        };
        if metadata.file_type().is_symlink()
            || metadata.uid() != self.owner
            || metadata.mode() & 0o022 != 0
            || (!metadata.is_file() && !metadata.is_dir())
        {
            return Err("host_retirement_path_untrusted".into());
        }
        Ok(Some(metadata))
    }
    fn read<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<Option<T>, String> {
        let Some(metadata) = self.metadata(path)? else {
            return Ok(None);
        };
        if !metadata.is_file() || metadata.len() > MAX_RESPONSE as u64 {
            return Err("host_retirement_state_invalid".into());
        }
        let mut bytes = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .and_then(|file| file.take(MAX_RESPONSE as u64 + 1).read_to_end(&mut bytes))
            .map_err(|_| "host_retirement_state_unknown")?;
        if bytes.len() > MAX_RESPONSE {
            return Err("host_retirement_state_invalid".into());
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "host_retirement_state_invalid".into())
    }
    fn persist(&self, intent: &Intent) -> Result<(), String> {
        let id = &intent.registration.target.environment_instance_id;
        let path = self.intent_path(id);
        let temporary = self.control.join(format!("{id}.retire.pending"));
        self.metadata(&path)?;
        self.metadata(&temporary)?;
        let bytes = serde_json::to_vec(intent).map_err(|_| "host_retirement_evidence_failed")?;
        if bytes.len() > MAX_RESPONSE {
            return Err("host_retirement_evidence_failed".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| "host_retirement_evidence_failed")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "host_retirement_evidence_failed")?;
        fs::rename(&temporary, path)
            .and_then(|_| File::open(&self.control)?.sync_all())
            .map_err(|_| "host_retirement_evidence_failed".into())
    }
    pub(super) fn blocks(&self, id: &str) -> Result<bool, String> {
        self.metadata(&self.intent_path(id))
            .map(|metadata| metadata.is_some())
    }
    pub(super) fn retire(
        &self,
        target: &HostLaunchTarget,
        absent: impl FnOnce() -> Result<bool, String>,
        quiesce: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        target.validate()?;
        let id = &target.environment_instance_id;
        let saved: Option<Intent> = self.read(&self.intent_path(id))?;
        let registered: Option<Registration> = self.read(&self.registration_path(id))?;
        if saved
            .as_ref()
            .is_some_and(|intent| &intent.registration.target != target)
            || registered
                .as_ref()
                .is_some_and(|registration| &registration.target != target)
            || saved
                .as_ref()
                .zip(registered.as_ref())
                .is_some_and(|(intent, registration)| &intent.registration != registration)
        {
            return Err("host_retirement_target_mismatch".into());
        }
        let registration = saved
            .as_ref()
            .map(|intent| intent.registration.clone())
            .or(registered);
        let Some(registration) = registration else {
            // An unknown leftover is never authority to unlink it. A completely
            // absent instance needs no perpetual host-side tombstone.
            for path in self.roots(id) {
                if self.metadata(&path)?.is_some() {
                    return Err("host_retirement_unregistered_state".into());
                }
            }
            if !absent()? {
                return Err("host_retirement_container_present".into());
            }
            quiesce()?;
            return Ok(());
        };
        if !absent()? {
            return Err("host_retirement_container_present".into());
        }
        let mut intent = saved.unwrap_or(Intent {
            registration,
            evidence: None,
        });
        if !self.blocks(id)? {
            self.persist(&intent)?;
        }
        quiesce()?;
        // Validate all three trees before deleting even the first one. A bad
        // link/type leaves the whole pending cleanup available for inspection.
        let mut control = TreeSummary::default();
        let mut output = TreeSummary::default();
        for (index, path) in self.roots(id).iter().enumerate() {
            if self
                .metadata(path)?
                .is_some_and(|metadata| !metadata.is_dir())
            {
                return Err("host_retirement_path_untrusted".into());
            }
            self.summarize(
                path,
                path,
                if index == 2 {
                    &mut control
                } else {
                    &mut output
                },
                index == 2,
                0,
            )?;
        }
        if intent.evidence.is_none() {
            intent.evidence = Some(Evidence {
                container_absent: true,
                workers_quiesced: true,
                control_files: control.files,
                control_bytes: control.bytes,
                control_sha256: format!("{:x}", control.digest.finalize()),
                output_files: output.files,
                output_bytes: output.bytes,
            });
            self.persist(&intent)?;
        }
        for path in self.roots(id) {
            if self.metadata(&path)?.is_some() {
                fs::remove_dir_all(&path).map_err(|_| "host_retirement_pending")?;
            }
        }
        self.remove_file(&self.registration_path(id))?;
        File::open(&self.output)
            .and_then(|file| file.sync_all())
            .map_err(|_| "host_retirement_pending")?;
        File::open(&self.control)
            .and_then(|file| file.sync_all())
            .map_err(|_| "host_retirement_pending")?;
        // This is deliberately last: a crash anywhere above is resumable from
        // the exact immutable registration and already-persisted evidence.
        self.remove_file(&self.intent_path(id))?;
        File::open(&self.control)
            .and_then(|file| file.sync_all())
            .map_err(|_| "host_retirement_pending".into())
    }
    fn pending(&self, after: &mut String) -> Result<Vec<HostLaunchTarget>, String> {
        // Only four durable intents are considered per tick, rotating past a
        // blocked or corrupt record so another environment can still finish.
        let mut names = std::collections::BTreeSet::new();
        for entry in fs::read_dir(&self.control).map_err(|_| "host_retirement_state_unknown")? {
            let entry = entry.map_err(|_| "host_retirement_state_unknown")?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(id) = name.strip_suffix(".retire.json") else {
                continue;
            };
            if !id
                .strip_prefix("instance_")
                .is_some_and(super::super::hex_id)
                || name <= *after
            {
                continue;
            }
            names.insert(name);
            if names.len() > 4 {
                names.pop_last();
            }
        }
        if names.is_empty() {
            after.clear();
        }
        let mut targets = Vec::new();
        for name in names {
            *after = name.clone();
            let Ok(Some(intent)) = self.read::<Intent>(&self.control.join(&name)) else {
                continue;
            };
            let target = intent.registration.target;
            if target.validate().is_ok()
                && format!("{}.retire.json", target.environment_instance_id) == name
            {
                targets.push(target);
            }
        }
        Ok(targets)
    }
    fn remove_file(&self, path: &Path) -> Result<(), String> {
        if let Some(metadata) = self.metadata(path)? {
            if !metadata.is_file() {
                return Err("host_retirement_state_invalid".into());
            }
            fs::remove_file(path).map_err(|_| "host_retirement_pending")?;
        }
        Ok(())
    }
    fn summarize(
        &self,
        root: &Path,
        path: &Path,
        summary: &mut TreeSummary,
        hash: bool,
        depth: usize,
    ) -> Result<(), String> {
        let Some(metadata) = self.metadata(path)? else {
            return Ok(());
        };
        if depth > 32 || summary.entries >= 65_536 {
            return Err("host_retirement_scan_limit".into());
        }
        summary.entries += 1;
        if metadata.is_dir() {
            let remaining = 65_536 - summary.entries;
            let mut entries = fs::read_dir(path)
                .map_err(|_| "host_retirement_state_unknown")?
                .take(remaining + 1)
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "host_retirement_state_unknown")?;
            if entries.len() > remaining {
                return Err("host_retirement_scan_limit".into());
            }
            entries.sort();
            for entry in entries {
                self.summarize(root, &entry, summary, hash, depth + 1)?;
            }
        } else {
            summary.files += 1;
            summary.bytes = summary
                .bytes
                .checked_add(metadata.len())
                .ok_or("host_retirement_scan_limit")?;
            if hash {
                if summary.bytes > 64 * 1024 * 1024 {
                    return Err("host_retirement_scan_limit".into());
                }
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| "host_retirement_path_untrusted")?;
                summary
                    .digest
                    .update(relative.as_os_str().as_encoded_bytes());
                summary.digest.update(metadata.len().to_le_bytes());
                let mut file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                    .map_err(|_| "host_retirement_state_unknown")?;
                let mut bytes = [0; 64 * 1024];
                loop {
                    let count = file
                        .read(&mut bytes)
                        .map_err(|_| "host_retirement_state_unknown")?;
                    if count == 0 {
                        break;
                    }
                    summary.digest.update(&bytes[..count]);
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct TreeSummary {
    files: u64,
    bytes: u64,
    entries: usize,
    digest: Sha256,
}

/// A successful, exact full-ID list query is independent evidence. CLI/daemon
/// failures remain unknown; an inspect failure alone never proves absence.
pub(super) fn docker_absent(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
) -> Result<bool, String> {
    let _lock = COMMAND_LOCK.lock().map_err(|_| "host_command_failed")?;
    let endpoint = format!("unix://{}", config.docker_socket);
    let filter = format!("id={}", target.container_id);
    let bytes = command_bytes_unlocked(
        &config.docker_path,
        &[
            "--host",
            &endpoint,
            "container",
            "ls",
            "--all",
            "--no-trunc",
            "--filter",
            &filter,
            "--format",
            "{{json .ID}}",
        ],
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "host_retirement_container_unknown")?;
    let mut present = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let id: String =
            serde_json::from_str(line).map_err(|_| "host_retirement_container_unknown")?;
        if id != target.container_id {
            return Err("host_retirement_container_unknown".into());
        }
        present = true;
    }
    Ok(!present)
}

pub(super) fn start_retries(host: &Arc<Host>) -> Result<(), String> {
    let weak = Arc::downgrade(host);
    thread::Builder::new()
        .name("host-retirement".into())
        .spawn(move || {
            let mut after = String::new();
            while let Some(host) = weak.upgrade() {
                if let Ok(targets) = host.retirement.pending(&mut after) {
                    for target in targets {
                        let _ = host.retire(&target);
                    }
                }
                drop(host);
                thread::sleep(Duration::from_secs(1));
            }
        })
        .map_err(|_| "host_retirement_worker_failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Fixture {
        root: PathBuf,
        store: RetirementStore,
        registration: Registration,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "native-retirement-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let store = RetirementStore {
                control: root.join("control"),
                output: root.join("output"),
                // Tests own only their private fixture; production fixes this to root.
                owner: unsafe { libc::geteuid() },
            };
            fs::create_dir(&store.control).unwrap();
            fs::create_dir(&store.output).unwrap();
            let target = HostLaunchTarget {
                environment_instance_id: format!("instance_{}", "a".repeat(64)),
                container_id: "b".repeat(64),
                environment_id: "environment".into(),
                profile_digest: format!("sha256:{}", "c".repeat(64)),
                image_digest: format!("sha256:{}", "d".repeat(64)),
            };
            let registration = Registration {
                target,
                init_pid: 123,
                init_start_time: 456,
                bundle: "/private/bundle".into(),
                cgroup: "/private/cgroup".into(),
                spec_digest: "e".repeat(64),
                process: serde_json::json!({}),
            };
            let fixture = Self {
                root,
                store,
                registration,
            };
            fixture.seed();
            fixture
        }
        fn id(&self) -> &str {
            &self.registration.target.environment_instance_id
        }
        fn seed(&self) {
            fs::write(
                self.store
                    .control
                    .join(format!("{}.target.json", self.id())),
                serde_json::to_vec(&self.registration).unwrap(),
            )
            .unwrap();
            fs::create_dir(self.store.control.join(self.id())).unwrap();
            fs::write(
                self.store.control.join(self.id()).join("history.sqlite"),
                b"original uncertain task facts",
            )
            .unwrap();
            fs::create_dir(self.store.output.join(self.id())).unwrap();
            fs::write(
                self.store.output.join(self.id()).join("task.output"),
                b"retained output",
            )
            .unwrap();
            fs::create_dir(self.store.output.join(format!("{}.helpers", self.id()))).unwrap();
            fs::write(
                self.store
                    .output
                    .join(format!("{}.helpers", self.id()))
                    .join("task.stdout"),
                b"helper output",
            )
            .unwrap();
        }
        fn remaining(&self) -> bool {
            self.store.control.join(self.id()).exists()
        }
        fn retire(&self) -> Result<(), String> {
            self.store
                .retire(&self.registration.target, || Ok(true), || Ok(()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn retirement_reclaims_only_the_exact_instance_and_leaves_no_permanent_receipt() {
        let fixture = Fixture::new();
        let sibling = fixture
            .store
            .output
            .join(format!("instance_{}", "f".repeat(64)));
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("output"), b"other live environment").unwrap();
        fixture.retire().unwrap();
        assert!(!fixture.remaining());
        assert!(!fixture.store.output.join(fixture.id()).exists());
        assert!(!fixture
            .store
            .output
            .join(format!("{}.helpers", fixture.id()))
            .exists());
        assert_eq!(
            fs::read(sibling.join("output")).unwrap(),
            b"other live environment"
        );
        assert_eq!(fs::read_dir(&fixture.store.control).unwrap().count(), 0);
        fixture.retire().unwrap();
    }

    #[test]
    fn retirement_keeps_live_or_unobservable_containers_and_mismatched_targets() {
        let fixture = Fixture::new();
        for observed in [Ok(false), Err("docker unavailable".into())] {
            assert!(fixture
                .store
                .retire(
                    &fixture.registration.target,
                    || observed,
                    || panic!("must not quiesce a live or unknown target")
                )
                .is_err());
            assert!(fixture.remaining());
        }
        let mut mismatch = fixture.registration.target.clone();
        mismatch.container_id = "f".repeat(64);
        assert!(fixture
            .store
            .retire(
                &mismatch,
                || panic!("identity must be checked first"),
                || Ok(())
            )
            .is_err());
        assert!(fixture.remaining());
    }

    #[test]
    fn durable_retirement_intent_survives_busy_readers_and_restart() {
        let fixture = Fixture::new();
        let result = fixture.store.retire(
            &fixture.registration.target,
            || Ok(true),
            || Err("host_retirement_busy".into()),
        );
        assert_eq!(result.unwrap_err(), "host_retirement_busy");
        assert!(fixture.remaining());
        assert!(fixture
            .store
            .control
            .join(format!("{}.retire.json", fixture.id()))
            .is_file());
        fixture.retire().unwrap();
        assert!(!fixture.remaining());
    }

    #[test]
    fn retirement_never_deletes_unregistered_or_symlinked_orphans() {
        let fixture = Fixture::new();
        let registration = fixture
            .store
            .control
            .join(format!("{}.target.json", fixture.id()));
        fs::remove_file(&registration).unwrap();
        assert!(fixture.retire().is_err());
        assert!(fixture.remaining());
        fs::write(
            registration,
            serde_json::to_vec(&fixture.registration).unwrap(),
        )
        .unwrap();
        let outside = fixture.root.join("outside");
        fs::write(&outside, b"protected unrelated content").unwrap();
        std::os::unix::fs::symlink(
            &outside,
            fixture.store.output.join(fixture.id()).join("escape"),
        )
        .unwrap();
        assert!(fixture.retire().is_err());
        assert_eq!(fs::read(outside).unwrap(), b"protected unrelated content");
        assert!(fixture.remaining());
    }

    #[test]
    fn cleanup_cannot_unlink_outputs_before_durable_evidence_and_restarts_after_partial_unlink() {
        let fixture = Fixture::new();
        let temporary = fixture
            .store
            .control
            .join(format!("{}.retire.pending", fixture.id()));
        let error = fixture
            .store
            .retire(
                &fixture.registration.target,
                || Ok(true),
                || {
                    fs::create_dir(&temporary).unwrap();
                    Ok(())
                },
            )
            .unwrap_err();
        assert_eq!(error, "host_retirement_evidence_failed");
        assert!(fixture
            .store
            .output
            .join(fixture.id())
            .join("task.output")
            .is_file());
        fs::remove_dir(temporary).unwrap();
        let mut intent: Intent = fixture
            .store
            .read(&fixture.store.intent_path(fixture.id()))
            .unwrap()
            .unwrap();
        intent.evidence = Some(Evidence {
            container_absent: true,
            workers_quiesced: true,
            control_files: 1,
            control_bytes: 29,
            control_sha256: "a".repeat(64),
            output_files: 2,
            output_bytes: 28,
        });
        fixture.store.persist(&intent).unwrap();
        fs::remove_dir_all(fixture.store.output.join(fixture.id())).unwrap();
        fs::remove_file(fixture.store.registration_path(fixture.id())).unwrap();
        let restarted = RetirementStore {
            control: fixture.store.control.clone(),
            output: fixture.store.output.clone(),
            owner: fixture.store.owner,
        };
        restarted
            .retire(&fixture.registration.target, || Ok(true), || Ok(()))
            .unwrap();
        assert!(!fixture.remaining());
        assert_eq!(fs::read_dir(&fixture.store.control).unwrap().count(), 0);
    }

    #[test]
    fn retry_scan_is_bounded_rotates_past_busy_instances_and_never_adopts_unknown_files() {
        let fixture = Fixture::new();
        for byte in b'a'..=b'f' {
            let mut registration = fixture.registration.clone();
            registration.target.environment_instance_id =
                format!("instance_{}", char::from(byte).to_string().repeat(64));
            fixture
                .store
                .persist(&Intent {
                    registration,
                    evidence: None,
                })
                .unwrap();
        }
        fs::write(
            fixture.store.control.join("unknown.retire.json"),
            b"untrusted orphan",
        )
        .unwrap();
        let mut after = String::new();
        let first = fixture.store.pending(&mut after).unwrap();
        assert_eq!(first.len(), 4);
        let second = fixture.store.pending(&mut after).unwrap();
        assert_eq!(second.len(), 2);
        assert!(first.iter().all(|a| second
            .iter()
            .all(|b| a.environment_instance_id != b.environment_instance_id)));
        assert!(fixture.store.pending(&mut after).unwrap().is_empty());
        assert_eq!(fixture.store.pending(&mut after).unwrap().len(), 4);
        assert_eq!(
            fs::read(fixture.store.control.join("unknown.retire.json")).unwrap(),
            b"untrusted orphan"
        );
    }

    fn configuration(fixture: &Fixture, docker_path: String) -> HostSupervisorConfig {
        HostSupervisorConfig {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            socket_path: fixture.root.join("socket").to_string_lossy().into(),
            control_root: fixture.store.control.to_string_lossy().into(),
            output_root: fixture.store.output.to_string_lossy().into(),
            runc_path: "/unused/runc".into(),
            runc_root: "/unused/runc-root".into(),
            docker_path,
            docker_socket: "/unused/docker.sock".into(),
            controller_uid: 0,
            helper_path: None,
        }
    }

    #[test]
    fn active_output_request_lease_blocks_retirement_and_new_admission_until_reader_finishes() {
        let fixture = Fixture::new();
        fs::remove_file(
            fixture
                .store
                .control
                .join(fixture.id())
                .join("history.sqlite"),
        )
        .unwrap();
        let manager = Arc::new(
            NativeManager::retirement_fixture(
                fixture.store.control.join(fixture.id()),
                fixture.store.output.join(fixture.id()),
                fixture.id(),
            )
            .unwrap(),
        );
        let reader_lease = manager.clone();
        let mut output =
            File::open(fixture.store.output.join(fixture.id()).join("task.output")).unwrap();
        let host = Host {
            config: configuration(&fixture, "/bin/false".into()),
            managers: Mutex::new(
                [(fixture.id().into(), (fixture.registration.clone(), manager))].into(),
            ),
            _lease: File::open(&fixture.root).unwrap(),
            helper_binary: None,
            retirement: RetirementStore {
                control: fixture.store.control.clone(),
                output: fixture.store.output.clone(),
                owner: fixture.store.owner,
            },
        };
        assert_eq!(
            host.retire_with_probe(&fixture.registration.target, || Ok(true))
                .unwrap_err(),
            "host_retirement_busy"
        );
        assert_eq!(
            host.manager(&fixture.registration.target).err().unwrap(),
            "host_retirement_pending"
        );
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"retained output");
        drop(output);
        drop(reader_lease);
        host.retire_with_probe(&fixture.registration.target, || Ok(true))
            .unwrap();
        assert!(host.managers.lock().unwrap().is_empty());
        assert!(!fixture.remaining());
    }

    #[test]
    fn independent_docker_absence_requires_successful_exact_full_id_query() {
        let fixture = Fixture::new();
        let command = fixture.root.join("docker-probe");
        let response = fixture.root.join("docker-response");
        let arguments = fixture.root.join("docker-arguments");
        fs::write(
            &command,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat '{}'\n",
                arguments.display(),
                response.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
        let config = configuration(&fixture, command.to_string_lossy().into());
        fs::write(&response, b"").unwrap();
        assert!(docker_absent(&config, &fixture.registration.target).unwrap());
        let args = fs::read_to_string(arguments).unwrap();
        assert!(args
            .lines()
            .any(|value| value == format!("id={}", fixture.registration.target.container_id)));
        assert!(args.lines().any(|value| value == "--all"));
        assert!(args.lines().any(|value| value == "--no-trunc"));
        fs::write(
            &response,
            serde_json::to_vec(&fixture.registration.target.container_id).unwrap(),
        )
        .unwrap();
        assert!(!docker_absent(&config, &fixture.registration.target).unwrap());
        for invalid in [
            b"not JSON".to_vec(),
            serde_json::to_vec(&"f".repeat(64)).unwrap(),
        ] {
            fs::write(&response, invalid).unwrap();
            assert!(docker_absent(&config, &fixture.registration.target).is_err());
        }
        fs::write(&command, b"#!/bin/sh\nexit 1\n").unwrap();
        assert!(docker_absent(&config, &fixture.registration.target).is_err());
    }
}
