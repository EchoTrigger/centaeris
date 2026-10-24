//! Sequential completion must not become a lifetime command quota for a Run.
//! Fake transport observes physical dispatch calls; this is not VM acceptance.
use super::*;

struct SequentialTransport {
    starts: AtomicUsize,
    waits: AtomicUsize,
    lose_first_start_reply: AtomicBool,
}

impl SequentialTransport {
    fn new(lose_first_start_reply: bool) -> Self {
        Self {
            starts: AtomicUsize::new(0),
            waits: AtomicUsize::new(0),
            lose_first_start_reply: AtomicBool::new(lose_first_start_reply),
        }
    }
}

impl E2bHostTransport for SequentialTransport {
    fn inspect(&self, binding: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        Ok(E2bSandboxObservation {
            binding: binding.clone(),
            running: true,
            on_timeout_kill: true,
            auto_resume: false,
        })
    }

    fn helper(
        &self,
        _: &E2bSandboxBinding,
        _: TrustedHelperKind,
        _: &[u8],
        _: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        Err(E2bHostError::NotDispatched(
            "unexpected_capacity_test_helper",
        ))
    }

    fn start(
        &self,
        _: &E2bSandboxBinding,
        _: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        let index = self.starts.fetch_add(1, Ordering::SeqCst);
        if self.lose_first_start_reply.swap(false, Ordering::SeqCst) {
            return Err(E2bHostError::Unknown("capacity_test_start_reply_lost"));
        }
        Ok(i32::try_from(index + 1000).unwrap())
    }

    fn wait(
        &self,
        binding: &E2bSandboxBinding,
        pid: i32,
        _: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        Ok(E2bCommandOutput {
            sandbox_id: binding.sandbox_id.clone(),
            pid,
            exit_code: 0,
            stdout: format!("completed:{pid}").into_bytes(),
            stderr: Vec::new(),
        })
    }

    fn quiesce(&self, _: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        Ok(())
    }
}

fn sequential_host(transport: Arc<SequentialTransport>) -> E2bExecutionHostRunner {
    E2bExecutionHostRunner::new(
        profile(),
        binding(),
        transport,
        Arc::new(FixtureAuthority::new()),
    )
    .unwrap()
}

fn numbered_command(host: &E2bExecutionHostRunner, index: usize) -> ExecutionCommandRequest {
    let mut request = command(host);
    request.args = vec!["-c".into(), format!("printf command-{index}")];
    request
}

#[test]
fn sequential_completed_commands_keep_replay_and_admit_the_next_command() {
    let transport = Arc::new(SequentialTransport::new(false));
    let host = sequential_host(transport.clone());
    let mut first = None;
    for index in 0..128 {
        let operation = format!("sequential-command-{index}");
        let request = numbered_command(&host, index);
        let output = host
            .run_host_command(Some(&operation), request.clone(), None)
            .unwrap();
        assert_eq!(output.process.exit_code, Some(0));
        assert_eq!(transport.starts.load(Ordering::SeqCst), index + 1);
        assert_eq!(transport.waits.load(Ordering::SeqCst), index + 1);
        assert_eq!(
            host.run_host_command(Some(&operation), request.clone(), None)
                .unwrap(),
            output,
            "replaying a completed operation must not dispatch or wait again"
        );
        match host
            .recover_host_command(&operation, request.clone(), None)
            .unwrap()
        {
            ExecutionHostCommandRecovery::Completed(recovered) => assert_eq!(*recovered, output),
            other => panic!("completed operation lost replay evidence: {other:?}"),
        }
        assert_eq!(transport.starts.load(Ordering::SeqCst), index + 1);
        assert_eq!(transport.waits.load(Ordering::SeqCst), index + 1);
        first.get_or_insert((operation, request, output));
    }
    let (first_operation, first_request, first_output) = first.unwrap();
    assert_eq!(
        host.run_host_command(Some(&first_operation), first_request.clone(), None)
            .unwrap(),
        first_output
    );
    let mut changed = first_request.clone();
    changed.args.push("changed".into());
    assert!(matches!(
        host.run_host_command(Some(&first_operation), changed, None),
        Err(ExecutionError::Denied { .. })
    ));
    assert_eq!(transport.starts.load(Ordering::SeqCst), 128);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 128);

    // Every prior wait returned a final exit. No command is concurrently active.
    let next_request = numbered_command(&host, 128);
    let next = host.run_host_command(Some("sequential-command-128"), next_request.clone(), None);
    assert!(
        next.is_ok(),
        "129th sequential command was rejected after 128 confirmed completions; starts={}, waits={}, result={next:?}",
        transport.starts.load(Ordering::SeqCst),
        transport.waits.load(Ordering::SeqCst),
    );
    let next = next.unwrap();
    assert_eq!(next.process.exit_code, Some(0));
    assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 129);
    assert_eq!(
        host.run_host_command(Some("sequential-command-128"), next_request, None)
            .unwrap(),
        next
    );
    assert_eq!(
        host.run_host_command(Some(&first_operation), first_request, None)
            .unwrap(),
        first_output,
        "old completed identity must retain its original result after later admission"
    );
    assert_eq!(transport.starts.load(Ordering::SeqCst), 129);
    assert_eq!(transport.waits.load(Ordering::SeqCst), 129);
}

#[test]
fn unresolved_command_never_reexecutes_after_completed_history_pressure() {
    let transport = Arc::new(SequentialTransport::new(true));
    let host = sequential_host(transport.clone());
    let unresolved = numbered_command(&host, 0);
    assert!(matches!(
        host.run_host_command(Some("unresolved-first"), unresolved.clone(), None),
        Err(ExecutionError::OperationIndeterminate { .. })
    ));
    for index in 1..=129 {
        match host.run_host_command(
            Some(&format!("completed-later-{index}")),
            numbered_command(&host, index),
            None,
        ) {
            Ok(output) => assert_eq!(output.process.exit_code, Some(0)),
            Err(ExecutionError::CapacityRejected {
                stage: ExecutionOperationStage::Start,
                ..
            }) => (),
            other => panic!("unexpected history-pressure result: {other:?}"),
        }
    }
    let starts = transport.starts.load(Ordering::SeqCst);
    let waits = transport.waits.load(Ordering::SeqCst);
    assert!(matches!(
        host.recover_host_command("unresolved-first", unresolved.clone(), None)
            .unwrap(),
        ExecutionHostCommandRecovery::Unknown { .. }
    ));
    assert!(matches!(
        host.run_host_command(Some("unresolved-first"), unresolved, None),
        Err(ExecutionError::OperationIndeterminate { .. })
    ));
    assert_eq!(transport.starts.load(Ordering::SeqCst), starts);
    assert_eq!(transport.waits.load(Ordering::SeqCst), waits);
}

mod cold_evidence;
