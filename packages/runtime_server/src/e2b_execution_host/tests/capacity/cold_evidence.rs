//! Observable replay, admission and ownership under bounded cold-cache failures.
use super::*;
use crate::e2b_execution_host::command_evidence::{CacheFault, CacheLimits, CompletedCache};

fn completed(
    host: &E2bExecutionHostRunner,
    count: usize,
) -> Vec<(String, ExecutionCommandRequest, ExecutionHostCommandOutput)> {
    (0..count)
        .map(|index| {
            let id = format!("cold-command-{index}");
            let request = numbered_command(host, index);
            let output = host
                .run_host_command(Some(&id), request.clone(), None)
                .unwrap();
            (id, request, output)
        })
        .collect()
}
fn cold_entry(
    host: &E2bExecutionHostRunner,
    history: &[(String, ExecutionCommandRequest, ExecutionHostCommandOutput)],
) -> (
    String,
    ExecutionCommandRequest,
    ExecutionHostCommandOutput,
    std::path::PathBuf,
) {
    let cache = host.completed_cache.lock().unwrap();
    history
        .iter()
        .find_map(|(id, request, output)| {
            cache
                .entry_path(id)
                .map(|path| (id.clone(), request.clone(), output.clone(), path))
        })
        .expect("confirmed completion must have moved to cold storage")
}
fn reject_new_without_dispatch(
    host: &E2bExecutionHostRunner,
    transport: &SequentialTransport,
    id: &str,
    index: usize,
) {
    let starts = transport.starts.load(Ordering::SeqCst);
    let waits = transport.waits.load(Ordering::SeqCst);
    assert!(matches!(
        host.run_host_command(Some(id), numbered_command(host, index), None),
        Err(ExecutionError::CapacityRejected {
            stage: ExecutionOperationStage::Start,
            ..
        })
    ));
    assert_eq!(transport.starts.load(Ordering::SeqCst), starts);
    assert_eq!(transport.waits.load(Ordering::SeqCst), waits);
}

#[test]
fn completed_history_replays_full_results_after_multiple_hot_windows() {
    let transport = Arc::new(SequentialTransport::new(false));
    let host = sequential_host(transport.clone());
    let history = completed(&host, 512);
    assert_eq!(transport.starts.load(Ordering::SeqCst), 512);
    for (id, request, output) in history.iter().rev() {
        assert_eq!(
            host.run_host_command(Some(id), request.clone(), None)
                .unwrap(),
            *output
        );
        match host
            .recover_host_command(id, request.clone(), None)
            .unwrap()
        {
            ExecutionHostCommandRecovery::Completed(found) => assert_eq!(*found, *output),
            other => panic!("closed history was lost: {other:?}"),
        }
    }
    let (id, mut request, _, _) = cold_entry(&host, &history);
    request.args.push("different command".into());
    assert!(matches!(
        host.run_host_command(Some(&id), request, None),
        Err(ExecutionError::Denied { .. })
    ));
    assert_eq!(transport.starts.load(Ordering::SeqCst), 512);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 512);
}

#[test]
fn failed_cold_index_write_or_publish_keeps_hot_replay_and_never_dispatches_next() {
    for fault in [
        CacheFault::IndexReserve,
        CacheFault::Write,
        CacheFault::Publish,
    ] {
        let transport = Arc::new(SequentialTransport::new(false));
        let host = sequential_host(transport.clone());
        let history = completed(&host, 128);
        host.completed_cache.lock().unwrap().fail_next(fault);
        reject_new_without_dispatch(&host, &transport, "after-failed-spill", 128);
        if let Some(root) = host.completed_cache.lock().unwrap().root_path() {
            assert_eq!(
                std::fs::read_dir(root).unwrap().count(),
                0,
                "failed spills leave no published or temporary receipt"
            );
        }
        for (id, request, output) in &history {
            assert_eq!(
                host.run_host_command(Some(id), request.clone(), None)
                    .unwrap(),
                *output
            );
        }
        assert_eq!(transport.starts.load(Ordering::SeqCst), 128);
        assert!(host.run_host_command(Some("after-failed-spill"),numbered_command(&host,128),None).is_ok(),"a retry after a definite spill failure can make progress without losing old identities");
        assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
    }
}

#[test]
fn missing_or_corrupted_cold_receipt_never_becomes_an_unseen_command() {
    for missing in [true, false] {
        let transport = Arc::new(SequentialTransport::new(false));
        let host = sequential_host(transport.clone());
        let history = completed(&host, 129);
        let (id, request, _, path) = cold_entry(&host, &history);
        if missing {
            std::fs::remove_file(&path).unwrap();
        } else {
            let mut corrupted = std::fs::read(&path).unwrap();
            corrupted[0] ^= 1;
            std::fs::write(&path, corrupted).unwrap();
        }
        assert!(matches!(
            host.run_host_command(Some(&id), request.clone(), None),
            Err(ExecutionError::OperationIndeterminate { .. })
        ));
        assert!(matches!(
            host.recover_host_command(&id, request.clone(), None),
            Err(ExecutionError::OperationIndeterminate { .. })
        ));
        let mut changed = request;
        changed.args.push("changed".into());
        assert!(matches!(
            host.run_host_command(Some(&id), changed, None),
            Err(ExecutionError::Denied { .. })
        ));
        assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
        assert_eq!(transport.waits.load(Ordering::SeqCst), 129);
    }
}

#[test]
fn cold_entry_and_byte_budgets_reject_only_before_new_dispatch() {
    for entries in [true, false] {
        let transport = Arc::new(SequentialTransport::new(false));
        let mut host = sequential_host(transport.clone());
        host.completed_cache = Mutex::new(CompletedCache::with_limits(CacheLimits {
            entries: if entries { 1 } else { 8 },
            ..CacheLimits::default()
        }));
        let history = completed(&host, 129);
        if !entries {
            let mut cache = host.completed_cache.lock().unwrap();
            let used = cache.used_bytes();
            assert!(used > 0);
            cache.limit_bytes(used);
        }
        reject_new_without_dispatch(&host, &transport, "over-cold-budget", 129);
        for (id, request, output) in &history {
            assert_eq!(
                host.run_host_command(Some(id), request.clone(), None)
                    .unwrap(),
                *output
            );
        }
        assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
    }
}

#[test]
fn cold_record_encoding_budget_preserves_original_hot_completion() {
    let transport = Arc::new(SequentialTransport::new(false));
    let mut host = sequential_host(transport.clone());
    host.completed_cache = Mutex::new(CompletedCache::with_limits(CacheLimits {
        record_bytes: 1,
        ..CacheLimits::default()
    }));
    let history = completed(&host, 128);
    reject_new_without_dispatch(&host, &transport, "oversized-receipt", 128);
    for (id, request, output) in history {
        assert_eq!(
            host.run_host_command(Some(&id), request, None).unwrap(),
            output
        );
    }
    assert_eq!(transport.starts.load(Ordering::SeqCst), 128);
}

#[test]
fn concurrent_spill_and_same_identity_replays_dispatch_each_command_once() {
    let transport = Arc::new(SequentialTransport::new(false));
    let host = Arc::new(sequential_host(transport.clone()));
    let history = completed(&host, 128);
    std::thread::scope(|scope| {
        let tasks = (0..32)
            .map(|index| {
                let host = host.clone();
                let old = history[index % history.len()].clone();
                scope.spawn(move || {
                    assert_eq!(
                        host.run_host_command(Some(&old.0), old.1, None).unwrap(),
                        old.2
                    );
                    let id = format!("concurrent-new-{}", index % 8);
                    host.run_host_command(Some(&id), numbered_command(&host, 128 + index % 8), None)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            assert_eq!(task.join().unwrap().process.exit_code, Some(0));
        }
    });
    assert_eq!(transport.starts.load(Ordering::SeqCst), 136);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 136);
}

#[test]
fn successor_has_no_old_run_replay_authority_and_old_cache_is_owned_until_drop() {
    let transport = Arc::new(SequentialTransport::new(false));
    let host = sequential_host(transport.clone());
    let history = completed(&host, 129);
    let (id, request, output, path) = cold_entry(&host, &history);
    let root = host.completed_cache.lock().unwrap().root_path().unwrap();
    let next = host.successor(Arc::new(FixtureAuthority::new())).unwrap();
    assert!(matches!(
        next.recover_host_command(&id, request.clone(), None)
            .unwrap(),
        ExecutionHostCommandRecovery::Unknown { .. }
    ));
    assert_eq!(
        host.run_host_command(Some(&id), request, None).unwrap(),
        output
    );
    assert!(path.is_file());
    drop(host);
    assert!(
        !root.exists(),
        "only the finished runner's privately owned cache is retired"
    );
    assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
}

#[test]
fn a_full_window_of_unknown_receipts_never_spills_or_repeats_start() {
    let transport = Arc::new(SequentialTransport::new(false));
    let host = sequential_host(transport.clone());
    for index in 0..128 {
        assert!(matches!(
            host.recover_host_command(
                &format!("unknown-{index}"),
                numbered_command(&host, index),
                None
            )
            .unwrap(),
            ExecutionHostCommandRecovery::Unknown { .. }
        ));
    }
    reject_new_without_dispatch(&host, &transport, "new-while-all-unknown", 128);
    for index in 0..128 {
        assert!(matches!(
            host.run_host_command(
                Some(&format!("unknown-{index}")),
                numbered_command(&host, index),
                None
            ),
            Err(ExecutionError::OperationIndeterminate { .. })
        ));
    }
    assert!(host.completed_cache.lock().unwrap().root_path().is_none());
    assert_eq!(transport.starts.load(Ordering::SeqCst), 0);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeldPhase {
    Start,
    Wait,
}
struct HeldTransport {
    inner: Arc<SequentialTransport>,
    phase: HeldPhase,
    hold_once: AtomicBool,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl HeldTransport {
    fn hold(&self, phase: HeldPhase) {
        if self.phase == phase && self.hold_once.swap(false, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    }
}
impl E2bHostTransport for HeldTransport {
    fn inspect(&self, b: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        self.inner.inspect(b)
    }
    fn helper(
        &self,
        b: &E2bSandboxBinding,
        k: TrustedHelperKind,
        input: &[u8],
        limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.inner.helper(b, k, input, limit)
    }
    fn start(
        &self,
        b: &E2bSandboxBinding,
        r: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        self.hold(HeldPhase::Start);
        self.inner.start(b, r)
    }
    fn wait(
        &self,
        b: &E2bSandboxBinding,
        pid: i32,
        timeout: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        self.hold(HeldPhase::Wait);
        self.inner.wait(b, pid, timeout)
    }
    fn quiesce(&self, b: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        self.inner.quiesce(b)
    }
}

#[test]
fn prepared_and_running_receipts_are_not_retired_under_history_pressure() {
    for phase in [HeldPhase::Start, HeldPhase::Wait] {
        let base = Arc::new(SequentialTransport::new(false));
        let (entered, observe) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let transport = Arc::new(HeldTransport {
            inner: base.clone(),
            phase,
            hold_once: AtomicBool::new(true),
            entered,
            release: Mutex::new(resume),
        });
        let host = Arc::new(
            E2bExecutionHostRunner::new(
                profile(),
                binding(),
                transport,
                Arc::new(FixtureAuthority::new()),
            )
            .unwrap(),
        );
        std::thread::scope(|scope| {
            let running = host.clone();
            let task = scope.spawn(move || {
                running.run_host_command(Some("held-original"), numbered_command(&running, 0), None)
            });
            observe.recv_timeout(Duration::from_secs(10)).unwrap();
            for index in 1..128 {
                assert!(matches!(
                    host.recover_host_command(
                        &format!("held-unknown-{index}"),
                        numbered_command(&host, index),
                        None
                    )
                    .unwrap(),
                    ExecutionHostCommandRecovery::Unknown { .. }
                ));
            }
            reject_new_without_dispatch(&host, &base, "not-before-held-exit", 128);
            assert!(host.successor(Arc::new(FixtureAuthority::new())).is_err());
            release.send(()).unwrap();
            assert_eq!(task.join().unwrap().unwrap().process.exit_code, Some(0));
        });
        assert!(host
            .run_host_command(Some("after-held-exit"), numbered_command(&host, 128), None)
            .is_ok());
        assert_eq!(base.starts.load(Ordering::SeqCst), 2);
        for index in 1..128 {
            assert!(matches!(
                host.run_host_command(
                    Some(&format!("held-unknown-{index}")),
                    numbered_command(&host, index),
                    None
                ),
                Err(ExecutionError::OperationIndeterminate { .. })
            ));
        }
        assert_eq!(base.starts.load(Ordering::SeqCst), 2);
    }
}

struct NotDispatchedTransport(SequentialTransport);
impl E2bHostTransport for NotDispatchedTransport {
    fn inspect(&self, b: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        self.0.inspect(b)
    }
    fn helper(
        &self,
        b: &E2bSandboxBinding,
        k: TrustedHelperKind,
        input: &[u8],
        limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.0.helper(b, k, input, limit)
    }
    fn start(
        &self,
        _: &E2bSandboxBinding,
        _: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        Err(E2bHostError::NotDispatched(
            "controlled_start_not_dispatched",
        ))
    }
    fn wait(
        &self,
        b: &E2bSandboxBinding,
        pid: i32,
        timeout: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        self.0.wait(b, pid, timeout)
    }
    fn quiesce(&self, b: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        self.0.quiesce(b)
    }
}
#[test]
fn not_dispatched_receipts_keep_request_binding_in_the_hot_window() {
    let transport = Arc::new(NotDispatchedTransport(SequentialTransport::new(false)));
    let host = E2bExecutionHostRunner::new(
        profile(),
        binding(),
        transport.clone(),
        Arc::new(FixtureAuthority::new()),
    )
    .unwrap();
    for index in 0..128 {
        assert!(matches!(
            host.run_host_command(
                Some(&format!("not-sent-{index}")),
                numbered_command(&host, index),
                None
            ),
            Err(ExecutionError::PolicyUnavailable { .. })
        ));
    }
    reject_new_without_dispatch(&host, &transport.0, "beyond-not-dispatched-window", 128);
    let mut changed = numbered_command(&host, 0);
    changed.args.push("changed".into());
    assert!(matches!(
        host.run_host_command(Some("not-sent-0"), changed, None),
        Err(ExecutionError::Denied { .. })
    ));
    assert!(matches!(
        host.recover_host_command("not-sent-0", numbered_command(&host, 0), None)
            .unwrap(),
        ExecutionHostCommandRecovery::NotAccepted { .. }
    ));
    assert_eq!(transport.0.starts.load(Ordering::SeqCst), 128);
    assert_eq!(transport.0.waits.load(Ordering::SeqCst), 0);
    assert!(host.completed_cache.lock().unwrap().root_path().is_none());
}

#[test]
fn cold_io_timings_record_closed_phases_and_failures_without_reexecuting_commands() {
    let transport = Arc::new(SequentialTransport::new(false));
    let host = sequential_host(transport.clone());
    let (result, events) =
        crate::observations::capture_timings(true, || -> Result<(), ExecutionError> {
            let mut history = Vec::new();
            for index in 0..129 {
                let id = format!("cold-command-{index}");
                let request = numbered_command(&host, index);
                let output = host.run_host_command(Some(&id), request.clone(), None)?;
                history.push((id, request, output));
            }
            let (id, request, output, path) = cold_entry(&host, &history);
            assert_eq!(
                host.run_host_command(Some(&id), request.clone(), None)?,
                output
            );
            assert!(matches!(
                host.recover_host_command(&id, request.clone(), None)?,
                ExecutionHostCommandRecovery::Completed(_)
            ));
            std::fs::remove_file(path).unwrap();
            assert!(matches!(
                host.run_host_command(Some(&id), request, None),
                Err(ExecutionError::OperationIndeterminate { .. })
            ));
            assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
            assert_eq!(transport.waits.load(Ordering::SeqCst), 129);
            Ok(())
        });
    assert!(
        events
            .iter()
            .any(|event| event["phase"] == "e2bCommandEvidenceSpill" && event["outcome"] == "ok"),
        "a real completed spill needs a closed IO timing record"
    );
    assert!(events
        .iter()
        .any(|event| event["phase"] == "e2bCommandEvidenceLoad" && event["outcome"] == "ok"));
    assert!(events
        .iter()
        .any(|event| event["phase"] == "e2bCommandEvidenceLoad" && event["outcome"] == "error"));
    result.unwrap();
    // Activity witnesses emit their own existing timing records into the same
    // collector. The closed field/name contract here belongs to cache IO only.
    for event in events.iter().filter(|event| {
        event["phase"]
            .as_str()
            .is_some_and(|phase| phase.starts_with("e2bCommandEvidence"))
    }) {
        assert!(matches!(
            event["phase"].as_str(),
            Some("e2bCommandEvidenceSpill" | "e2bCommandEvidenceLoad")
        ));
        assert!(event["elapsedMs"].as_f64().unwrap() >= 0.0);
        assert!(
            event.as_object().unwrap().keys().all(|name| matches!(
                name.as_str(),
                "schema" | "source" | "atUnixMs" | "jobId" | "phase" | "elapsedMs" | "outcome"
            )),
            "cache observations cannot contain command identity, paths or contents"
        );
    }
    let transport = Arc::new(SequentialTransport::new(false));
    let host = sequential_host(transport.clone());
    completed(&host, 128);
    host.completed_cache
        .lock()
        .unwrap()
        .fail_next(CacheFault::Publish);
    let (_, errors) = crate::observations::capture_timings(true, || {
        reject_new_without_dispatch(&host, &transport, "timed-failed-spill", 128)
    });
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["phase"], "e2bCommandEvidenceSpill");
    assert_eq!(errors[0]["outcome"], "error");
}

struct SizedReceiptTransport {
    inner: Arc<SequentialTransport>,
    large_args: Mutex<Option<Vec<String>>>,
    large_pid: Mutex<Option<i32>>,
}
impl E2bHostTransport for SizedReceiptTransport {
    fn inspect(&self, b: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        self.inner.inspect(b)
    }
    fn helper(
        &self,
        b: &E2bSandboxBinding,
        k: TrustedHelperKind,
        input: &[u8],
        limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.inner.helper(b, k, input, limit)
    }
    fn start(
        &self,
        b: &E2bSandboxBinding,
        r: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        let pid = self.inner.start(b, r)?;
        if self
            .large_args
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|args| args == &r.args)
        {
            *self.large_pid.lock().unwrap() = Some(pid);
        }
        Ok(pid)
    }
    fn wait(
        &self,
        b: &E2bSandboxBinding,
        pid: i32,
        timeout: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        let mut output = self.inner.wait(b, pid, timeout)?;
        if *self.large_pid.lock().unwrap() == Some(pid) {
            output.stdout = vec![b'L'; 32 * 1024];
        }
        Ok(output)
    }
    fn quiesce(&self, b: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        self.inner.quiesce(b)
    }
}

#[test]
fn a_large_first_completed_candidate_does_not_block_a_smaller_receipt_that_fits() {
    // An all-small control establishes that this actual encoded-output contract
    // fits the same cold budget, without hard-coding a receipt's JSON length.
    let control_transport = Arc::new(SequentialTransport::new(false));
    let mut control = sequential_host(control_transport.clone());
    control.completed_cache = Mutex::new(CompletedCache::with_limits(CacheLimits {
        bytes: 4096,
        ..CacheLimits::default()
    }));
    completed(&control, 128);
    assert!(control
        .run_host_command(
            Some("small-control-next"),
            numbered_command(&control, 128),
            None
        )
        .is_ok());
    assert_eq!(control_transport.starts.load(Ordering::SeqCst), 129);
    drop(control);

    let transport = Arc::new(SequentialTransport::new(false));
    let sized = Arc::new(SizedReceiptTransport {
        inner: transport.clone(),
        large_args: Mutex::new(None),
        large_pid: Mutex::new(None),
    });
    let mut host = E2bExecutionHostRunner::new(
        profile(),
        binding(),
        sized.clone(),
        Arc::new(FixtureAuthority::new()),
    )
    .unwrap();
    host.completed_cache = Mutex::new(CompletedCache::with_limits(CacheLimits {
        bytes: 4096,
        ..CacheLimits::default()
    }));
    let requests = (0..128)
        .map(|index| {
            (
                format!("fit-command-{index:03}"),
                numbered_command(&host, index),
            )
        })
        .collect::<Vec<_>>();
    // Prepare identities before dispatch so the controllable provider can make
    // the actual first admission candidate large, independent of HashMap seed.
    // The assertion below concerns admission and replay, not candidate ordering.
    for (id, request) in &requests {
        host.record(id, &host.request_digest(request).unwrap(), true)
            .unwrap();
    }
    let first = host.commands.lock().unwrap().keys().next().unwrap().clone();
    *sized.large_args.lock().unwrap() = Some(
        requests
            .iter()
            .find(|(id, _)| id == &first)
            .unwrap()
            .1
            .args
            .clone(),
    );
    assert_eq!(transport.starts.load(Ordering::SeqCst), 0);
    let history = requests
        .into_iter()
        .map(|(id, request)| {
            let output = host
                .run_host_command(Some(&id), request.clone(), None)
                .unwrap();
            (id, request, output)
        })
        .collect::<Vec<_>>();
    assert_eq!(transport.starts.load(Ordering::SeqCst), 128);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 128);
    let (result, events) = crate::observations::capture_timings(true, || {
        host.run_host_command(
            Some("after-large-first"),
            numbered_command(&host, 128),
            None,
        )
    });
    assert!(result.is_ok(),"a fitting completed receipt must allow progress after an oversized first candidate: {result:?}");
    let spills = events
        .iter()
        .filter(|event| event["phase"] == "e2bCommandEvidenceSpill")
        .collect::<Vec<_>>();
    assert_eq!(
        spills.len(),
        1,
        "non-fitting candidates must be checked before any cold IO"
    );
    assert_eq!(spills[0]["outcome"], "ok");
    for (id, request, output) in history {
        assert_eq!(
            host.run_host_command(Some(&id), request, None).unwrap(),
            output
        );
    }
    assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 129);
}
