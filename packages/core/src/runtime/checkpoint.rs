use serde_json::json;

use crate::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, EventVisibility, RuntimeAgentRunIdentityV1,
    RuntimeAwaitQuestionCheckpointV1, RuntimeEvent, RuntimeWaitChangedV1, RuntimeWaitStatusV1,
};
use crate::runtime::query_loop::{
    decide_loop_step, AgentStateSnapshot, ContinueHop, DoneReason, LoopDecision, ToolsRoute,
};
use crate::session::store::{RuntimeStore, RuntimeStoreTransactionPort, SaveWaitCheckpointRequest};

#[derive(Debug, Clone)]
pub struct SubmitTurnResponse {
    pub paused: bool,
    pub checkpoint: CheckpointRecord,
    pub events: Vec<RuntimeEvent>,
}

#[derive(Debug, Clone)]
pub struct PersistQueryStateRequest {
    pub session_id: String,
    pub turn_id: String,
    pub at_ms: i64,
    pub state: AgentStateSnapshot,
    pub agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
}

#[derive(Debug, Clone)]
pub struct RouteGenerateResultRequest {
    pub session_id: String,
    pub turn_id: String,
    pub state: AgentStateSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryContinuation {
    AwaitQuestion,
    AwaitRuntimeJob,
    ExecuteTools,
    CompleteTerminalTool,
    Finalize,
}

#[derive(Debug, Clone)]
pub struct RouteGenerateResultResponse {
    pub continuation: QueryContinuation,
    pub decision: LoopDecision,
    pub state: AgentStateSnapshot,
}

#[derive(Debug, Clone)]
pub struct TurnCheckpointStore<S: RuntimeStore + RuntimeStoreTransactionPort> {
    store: S,
}

impl<S: RuntimeStore + RuntimeStoreTransactionPort> TurnCheckpointStore<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    fn persist_query_state(
        &self,
        req: PersistQueryStateRequest,
    ) -> Result<SubmitTurnResponse, String> {
        let state = req.state;
        let done_reason = state
            .done_reason
            .as_ref()
            .map(|reason| done_reason_to_str(reason.clone()))
            .ok_or_else(|| "checkpoint_requires_runtime_wait".to_string())?;
        if done_reason != "question" {
            return Err(format!(
                "checkpoint_not_allowed_for_done_reason:{done_reason}"
            ));
        }

        let agent_run_identity = req
            .agent_run_identity
            .as_ref()
            .ok_or_else(|| "runtime_wait_requires_agent_run_identity".to_string())?;
        agent_run_identity.validate()?;
        let wait = RuntimeAwaitQuestionCheckpointV1::new(
            agent_run_identity,
            req.turn_id.as_str(),
            req.turn_id.as_str(),
        )?;
        let continuation_id = wait.continuation_id.clone();
        let checkpoint = CheckpointRecord {
            checkpoint_id: format!("checkpoint:{continuation_id}"),
            kind: CheckpointKindV1::Wait,
            session_id: req.session_id.clone(),
            turn_id: req.turn_id.clone(),
            status: "paused_question".to_string(),
            done_reason: Some(done_reason.to_string()),
            updated_at_ms: req.at_ms,
            payload_json: serde_json::to_string(&wait)
                .map_err(|error| format!("serialize question wait checkpoint failed: {error}"))?,
        };
        let changed = RuntimeWaitChangedV1 {
            schema: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            continuation_id: continuation_id.clone(),
            agent_run_id: agent_run_identity.agent_run_id.clone(),
            status: RuntimeWaitStatusV1::Waiting,
            transition_reason: "await_question".to_string(),
            at_ms: req.at_ms,
        };
        changed.validate()?;
        let event = RuntimeEvent {
            event_id: format!("runtime_wait:{continuation_id}:waiting"),
            session_id: req.session_id,
            task_id: Some(req.turn_id),
            event_type: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            at_ms: req.at_ms,
            visibility: EventVisibility::Internal,
            payload_json: serde_json::to_string(&changed)
                .map_err(|error| format!("serialize runtime wait event failed: {error}"))?,
        };
        self.store.save_wait_checkpoint(SaveWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            event: event.clone(),
        })?;

        Ok(SubmitTurnResponse {
            paused: true,
            checkpoint,
            events: vec![event],
        })
    }

    fn route_after_generate(
        &self,
        req: RouteGenerateResultRequest,
    ) -> Result<RouteGenerateResultResponse, String> {
        let mut state = req.state;
        let decision = decide_loop_step(&state);
        apply_decision_to_state(&mut state, &decision)?;
        Ok(RouteGenerateResultResponse {
            continuation: continuation_from_decision(&decision),
            decision,
            state,
        })
    }
}

impl<S> TurnCheckpointStore<S>
where
    S: RuntimeStore + RuntimeStoreTransactionPort + Clone + Send + 'static,
{
    pub async fn load_checkpoint_by_turn_async(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<CheckpointRecord>, String> {
        let checkpoint_store = self.clone();
        let session_id = session_id.to_string();
        let turn_id = turn_id.to_string();
        run_blocking_runtime_operation("load_checkpoint_by_turn", move || {
            checkpoint_store
                .store
                .load_checkpoint_by_turn(session_id.as_str(), turn_id.as_str())
                .map_err(|error| error.to_string())
        })
        .await
    }

    pub async fn persist_query_state_async(
        &self,
        req: PersistQueryStateRequest,
    ) -> Result<SubmitTurnResponse, String> {
        let checkpoint_store = self.clone();
        run_blocking_runtime_operation("persist_query_state", move || {
            checkpoint_store.persist_query_state(req)
        })
        .await
    }

    pub async fn route_after_generate_async(
        &self,
        req: RouteGenerateResultRequest,
    ) -> Result<RouteGenerateResultResponse, String> {
        let checkpoint_store = self.clone();
        run_blocking_runtime_operation("route_after_generate", move || {
            checkpoint_store.route_after_generate(req)
        })
        .await
    }
}

fn done_reason_to_str(done_reason: DoneReason) -> &'static str {
    match done_reason {
        DoneReason::FinalResponse => "final_response",
        DoneReason::ClarificationNeeded => "clarification_needed",
        DoneReason::Question => "question",
        DoneReason::TerminalTool => "terminal_tool",
        DoneReason::Error => "error",
    }
}

fn continuation_from_decision(decision: &LoopDecision) -> QueryContinuation {
    match decision.next_hop {
        ContinueHop::Finalize => QueryContinuation::Finalize,
        ContinueHop::Tools => QueryContinuation::ExecuteTools,
    }
}

fn apply_decision_to_state(
    state: &mut AgentStateSnapshot,
    decision: &LoopDecision,
) -> Result<(), String> {
    let transition = match decision.next_hop {
        ContinueHop::Finalize => json!({ "reason": "finalize" }),
        ContinueHop::Tools => {
            if decision.tools_route != Some(ToolsRoute::Execute) {
                return Err("tools route must execute declared tools".to_string());
            }
            json!({
                "reason": "tools_route",
                "toolsRoute": "execute",
            })
        }
    };
    state.transition_json = Some(transition.to_string());
    Ok(())
}

async fn run_blocking_runtime_operation<T, F>(
    label: &'static str,
    operation: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("blocking runtime operation join failed: {label}: {error}"))?
}
