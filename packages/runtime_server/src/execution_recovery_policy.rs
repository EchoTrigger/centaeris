//! Conservative policy for advancing a recovery checkpoint after a sandbox loss.
//!
//! This module deliberately consumes typed host evidence. Display text and tool summaries are
//! never recovery evidence. Snapshot reuse requires a stable host activity epoch across the
//! verified process quiescence and exclusive collection, and through the final recovery decision.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Instant;

#[derive(Debug, Default)]
struct DispatchState {
    active: usize,
    waiting: usize,
    capturing: bool,
    persistent_dispatch: bool,
}

#[derive(Debug, Default)]
struct ActivityGate {
    state: Mutex<DispatchState>,
    changed: Condvar,
    epoch: AtomicU64,
}

pub(crate) struct DispatchActivityGuard(Arc<ActivityGate>);
pub(crate) struct SnapshotActivityGuard {
    gate: Arc<ActivityGate>,
    acquired: Instant,
    observation_phase: &'static str,
}

impl SnapshotActivityGuard {
    pub(crate) fn observe_as(mut self, phase: &'static str) -> Self {
        self.observation_phase = phase;
        self
    }
}

impl DispatchActivityGuard {
    pub(crate) fn register_persistent_dispatch(&self) -> Result<(), &'static str> {
        self.0
            .state
            .lock()
            .map_err(|_| "execution activity lock poisoned")?
            .persistent_dispatch = true;
        Ok(())
    }
}

impl Drop for DispatchActivityGuard {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active -= 1;
        self.0.changed.notify_all();
    }
}

impl Drop for SnapshotActivityGuard {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.capturing = false;
        self.gate.changed.notify_all();
        drop(state);
        crate::observations::timing(self.observation_phase, self.acquired, "released");
    }
}

static EXECUTION_GATES: OnceLock<Mutex<HashMap<String, Weak<ActivityGate>>>> = OnceLock::new();

/// Tracks only newly committed tool records after an in-process checkpoint.
/// Reconstructed state deliberately starts without a snapshot witness.
#[derive(Default)]
pub(crate) struct RecoverySuffix {
    pub(crate) calls: usize,
    pub(crate) receipts: usize,
    pub(crate) non_bash_calls: usize,
    pub(crate) successful_receipts: usize,
    pub(crate) facts: usize,
    pub(crate) parallel_receipts: usize,
    pub(crate) receipt_executed: Option<bool>,
}

impl RecoverySuffix {
    pub(crate) fn record_call(&mut self, name: &str) {
        self.calls = self.calls.saturating_add(1);
        self.non_bash_calls = self
            .non_bash_calls
            .saturating_add(usize::from(name != "bash"));
    }

    pub(crate) fn record_receipt(
        &mut self,
        result: &centaeris_core::tool::layer::ToolExecutionResult,
    ) {
        self.receipts = self.receipts.saturating_add(1);
        self.successful_receipts = self
            .successful_receipts
            .saturating_add(usize::from(result.result_state().is_success()));
        self.facts = self.facts.saturating_add(result.facts.len());
        self.parallel_receipts = self
            .parallel_receipts
            .saturating_add(usize::from(result.parallel_group.is_some()));
        self.receipt_executed = result
            .details
            .get("executed")
            .and_then(serde_json::Value::as_bool);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostFailureEvidence {
    /// A host dispatch gate proved failure before dispatching a process into the sandbox.
    PreDispatchProcessNotStarted,
    // Negative evidence fixtures exercise rejection; production reports Unknown
    // unless its dispatch gate establishes PreDispatchProcessNotStarted.
    #[cfg(test)]
    DispatchStarted,
    #[cfg(test)]
    CancellationIndeterminate,
    #[cfg(test)]
    PostDispatchIo,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkspaceReuseEvidence {
    pub(crate) prior_checkpoint_snapshot_valid: bool,
    pub(crate) generation_known: bool,
    /// True only when exclusive admission and a non-destructive process observation proved
    /// the host quiescent before collection (or a new Execution had no dispatch opportunity).
    /// A generation-only reuse does not establish this fact.
    pub(crate) snapshot_was_quiesced: bool,
    /// Monotonic host activity epoch captured after quiescence and snapshot completion.
    pub(crate) snapshot_activity_epoch: u64,
    /// Current epoch. User commands, lifecycle hooks, plugin/MCP process dispatch, input
    /// materialization, and filesystem mutation increment it before dispatch.
    pub(crate) current_activity_epoch: u64,
    /// Both identities name the same in-memory host runner for the old Execution. A replacement
    /// runner cannot inherit this witness merely because its counter has the same value.
    pub(crate) snapshot_host_instance: u64,
    pub(crate) current_host_instance: u64,
}

#[derive(Debug)]
pub(crate) struct ExecutionActivityWitness {
    host_instance: u64,
    gate: Arc<ActivityGate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotCaptureToken {
    host_instance: u64,
    activity_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotCaptureError {
    ActivityChanged,
    HostChanged,
}

impl ExecutionActivityWitness {
    pub(crate) fn for_execution(host_instance: u64, execution: &str) -> Result<Self, &'static str> {
        let mut registry = EXECUTION_GATES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "execution activity registry lock poisoned")?;
        registry.retain(|_, gate| gate.strong_count() > 0);
        let gate = registry
            .get(execution)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let gate = Arc::new(ActivityGate::default());
                registry.insert(execution.into(), Arc::downgrade(&gate));
                gate
            });
        Ok(Self {
            host_instance,
            gate,
        })
    }

    pub(crate) fn begin_dispatch_activity(&self) -> Result<DispatchActivityGuard, &'static str> {
        let started = Instant::now();
        let mut state = self
            .gate
            .state
            .lock()
            .map_err(|_| "execution activity lock poisoned")?;
        state.waiting += 1;
        let queued = state.capturing;
        while state.capturing {
            state = self
                .gate
                .changed
                .wait(state)
                .map_err(|_| "execution activity lock poisoned")?;
        }
        state.waiting -= 1;
        state.active += 1;
        drop(state);
        let activity = DispatchActivityGuard(self.gate.clone());
        crate::observations::timing(
            "workspaceDispatchWait",
            started,
            if queued { "queued" } else { "ready" },
        );
        Ok(activity)
    }

    pub(crate) fn try_snapshot_activity(
        &self,
    ) -> Result<Option<SnapshotActivityGuard>, &'static str> {
        self.try_snapshot_admission(false)
    }

    pub(crate) fn try_final_snapshot_activity(
        &self,
    ) -> Result<Option<SnapshotActivityGuard>, &'static str> {
        self.try_snapshot_admission(true)
    }

    fn try_snapshot_admission(
        &self,
        finalizing: bool,
    ) -> Result<Option<SnapshotActivityGuard>, &'static str> {
        let mut state = self
            .gate
            .state
            .lock()
            .map_err(|_| "execution activity lock poisoned")?;
        if state.active != 0
            || state.waiting != 0
            || state.capturing
            || (!finalizing && state.persistent_dispatch)
        {
            return Ok(None);
        }
        state.capturing = true;
        Ok(Some(SnapshotActivityGuard {
            gate: self.gate.clone(),
            acquired: Instant::now(),
            observation_phase: "workspaceSnapshotHold",
        }))
    }

    #[cfg(test)]
    pub(crate) fn wait_snapshot_activity(&self) -> Result<SnapshotActivityGuard, &'static str> {
        let mut state = self
            .gate
            .state
            .lock()
            .map_err(|_| "execution activity lock poisoned")?;
        while state.active != 0 || state.capturing || state.waiting != 0 {
            state = self
                .gate
                .changed
                .wait(state)
                .map_err(|_| "execution activity lock poisoned")?;
        }
        state.capturing = true;
        Ok(SnapshotActivityGuard {
            gate: self.gate.clone(),
            acquired: Instant::now(),
            observation_phase: "workspaceSnapshotHold",
        })
    }

    #[cfg(test)]
    pub(crate) fn new(host_instance: u64) -> Self {
        Self {
            host_instance,
            gate: Arc::new(ActivityGate::default()),
        }
    }

    pub(crate) fn invalidate_before_dispatch(&self) -> Result<u64, &'static str> {
        self.gate
            .epoch
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |epoch| {
                epoch.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| "execution activity epoch overflow")
    }

    pub(crate) fn begin_snapshot_capture(&self) -> SnapshotCaptureToken {
        SnapshotCaptureToken {
            host_instance: self.host_instance,
            activity_epoch: self.gate.epoch.load(Ordering::SeqCst),
        }
    }

    /// Complete only after verified host quiescence and snapshot collection both succeed.
    pub(crate) fn complete_quiesced_snapshot(
        &self,
        token: SnapshotCaptureToken,
        snapshot_valid: bool,
        generation_known: bool,
    ) -> Result<WorkspaceReuseEvidence, SnapshotCaptureError> {
        if token.host_instance != self.host_instance {
            return Err(SnapshotCaptureError::HostChanged);
        }
        if token.activity_epoch != self.gate.epoch.load(Ordering::SeqCst) {
            return Err(SnapshotCaptureError::ActivityChanged);
        }
        // Bind the evidence to the epoch actually checked above. Reading a new
        // epoch here could associate a concurrent dispatch with an older snapshot.
        Ok(WorkspaceReuseEvidence {
            prior_checkpoint_snapshot_valid: snapshot_valid,
            generation_known,
            snapshot_was_quiesced: true,
            snapshot_activity_epoch: token.activity_epoch,
            current_activity_epoch: token.activity_epoch,
            snapshot_host_instance: token.host_instance,
            current_host_instance: token.host_instance,
        })
    }

    #[cfg(test)]
    pub(crate) fn capture_without_quiescence(
        &self,
        snapshot_valid: bool,
        generation_known: bool,
    ) -> WorkspaceReuseEvidence {
        self.capture(snapshot_valid, generation_known, false)
    }

    #[cfg(test)]
    fn capture(
        &self,
        snapshot_valid: bool,
        generation_known: bool,
        snapshot_was_quiesced: bool,
    ) -> WorkspaceReuseEvidence {
        let epoch = self.gate.epoch.load(Ordering::SeqCst);
        WorkspaceReuseEvidence {
            prior_checkpoint_snapshot_valid: snapshot_valid,
            generation_known,
            snapshot_was_quiesced,
            snapshot_activity_epoch: epoch,
            current_activity_epoch: epoch,
            snapshot_host_instance: self.host_instance,
            current_host_instance: self.host_instance,
        }
    }

    pub(crate) fn workspace_evidence(
        &self,
        snapshot: &WorkspaceReuseEvidence,
    ) -> WorkspaceReuseEvidence {
        WorkspaceReuseEvidence {
            current_activity_epoch: self.gate.epoch.load(Ordering::SeqCst),
            current_host_instance: self.host_instance,
            ..*snapshot
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryEvidence {
    pub(crate) failure: HostFailureEvidence,
    /// Audit data only. Existing adapters report `false` for some indeterminate post-dispatch
    /// failures, so this field can never establish eligibility by itself.
    pub(crate) receipt_reported_executed: Option<bool>,
    pub(crate) durable_receipt_sequence: u64,
    pub(crate) previous_checkpoint_sequence: u64,
    /// The advanced checkpoint is attached to a real next ModelRequestStarted safe point.
    pub(crate) next_model_request_started: bool,
    pub(crate) open_tool_calls: usize,
    pub(crate) parallel_tool_calls: usize,
    pub(crate) tool_calls_since_checkpoint: usize,
    pub(crate) receipts_since_checkpoint: usize,
    pub(crate) all_suffix_tools_are_bash: bool,
    pub(crate) external_or_mcp_calls_since_checkpoint: usize,
    pub(crate) successful_receipts_since_checkpoint: usize,
    pub(crate) facts_since_checkpoint: usize,
    pub(crate) workspace: WorkspaceReuseEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckpointAdvance {
    pub(crate) source_checkpoint_sequence: u64,
    pub(crate) covered_through_sequence: u64,
}

pub(crate) fn recovery_eligibility(
    evidence: &RecoveryEvidence,
) -> Result<CheckpointAdvance, &'static str> {
    if evidence.failure != HostFailureEvidence::PreDispatchProcessNotStarted {
        return Err("sandbox loss lacks typed pre-dispatch process-not-started evidence");
    }
    if evidence.receipt_reported_executed != Some(false) {
        return Err("tool receipt does not report a non-executed operation");
    }
    if !evidence.next_model_request_started {
        return Err("checkpoint advance requires a real next model request safe point");
    }
    if evidence.durable_receipt_sequence <= evidence.previous_checkpoint_sequence {
        return Err("durable tool ledger is not ahead of the previous checkpoint");
    }
    if evidence.open_tool_calls != 0 || evidence.parallel_tool_calls != 0 {
        return Err("tool execution is open, parallel, or indeterminate");
    }
    if evidence.tool_calls_since_checkpoint != 1
        || evidence.receipts_since_checkpoint != 1
        || !evidence.all_suffix_tools_are_bash
        || evidence.external_or_mcp_calls_since_checkpoint != 0
    {
        return Err("checkpoint suffix is not one closed bash call and receipt");
    }
    if evidence.successful_receipts_since_checkpoint != 0 || evidence.facts_since_checkpoint != 0 {
        return Err("checkpoint suffix contains a successful tool or semantic fact");
    }
    let workspace = evidence.workspace;
    if !workspace.prior_checkpoint_snapshot_valid || !workspace.generation_known {
        return Err("previous workspace snapshot is absent or has unknown generation");
    }
    if !workspace.snapshot_was_quiesced
        || workspace.current_activity_epoch != workspace.snapshot_activity_epoch
        || workspace.current_host_instance != workspace.snapshot_host_instance
    {
        return Err("execution host cannot prove the previous workspace snapshot remains complete");
    }
    Ok(CheckpointAdvance {
        source_checkpoint_sequence: evidence.previous_checkpoint_sequence,
        covered_through_sequence: evidence.durable_receipt_sequence,
    })
}

#[cfg(test)]
mod tests;
