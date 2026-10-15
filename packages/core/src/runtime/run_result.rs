//! Collect only committed boundaries; persistence remains owned by existing safe points.
use super::*;
use sha2::{Digest, Sha256};

impl From<&CheckpointRecord> for TurnCheckpointRef {
    fn from(value: &CheckpointRecord) -> Self {
        Self {
            checkpoint_id: value.checkpoint_id.clone(),
            kind: value.kind.clone(),
            session_id: value.session_id.clone(),
            turn_id: value.turn_id.clone(),
            status: value.status.clone(),
            done_reason: value.done_reason.clone(),
            updated_at_ms: value.updated_at_ms,
            payload_sha256: format!("sha256:{:x}", Sha256::digest(value.payload_json.as_bytes())),
            payload_byte_length: value.payload_json.len() as u64,
        }
    }
}

#[derive(Default)]
pub(super) struct RunResultCollector {
    receipts: Vec<TurnCommitReceipt>,
    last_turn: Option<TurnStepResult>,
}
impl RunResultCollector {
    pub(super) fn push(&mut self, turn: TurnStepResult) {
        self.receipts.push(TurnCommitReceipt {
            turn_id: turn.turn_id.clone(),
            continuation: turn.continuation,
            provider_tool_call_ids: turn
                .provider_tool_calls
                .iter()
                .map(|call| call.id.clone())
                .collect(),
            tool_result_ids: turn
                .tool_results
                .iter()
                .map(|result| result.tool_call_id.clone())
                .collect(),
            checkpoint_ref: turn.checkpoint.as_ref().map(TurnCheckpointRef::from),
        });
        // All existing safe-point persistence and stream callbacks have returned.
        // Keep the current boundary for waits/AnswerNow; drop the previous owner.
        self.last_turn = Some(turn);
    }
    pub(super) fn into_result(self, stop: AgentRunStop) -> AgentRunResult {
        let final_reply = self
            .last_turn
            .as_ref()
            .filter(|turn| {
                matches!(
                    turn.continuation,
                    QueryContinuation::Finalize | QueryContinuation::CompleteTerminalTool
                )
            })
            .and_then(|turn| turn.current_assistant_reply.clone());
        AgentRunResult {
            final_reply,
            turn_receipts: self.receipts,
            last_turn: self.last_turn,
            stop,
        }
    }
    pub(super) fn len(&self) -> usize {
        self.receipts.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }
    pub(super) fn last(&self) -> Option<&TurnStepResult> {
        self.last_turn.as_ref()
    }
    #[cfg(test)]
    fn retained_payloads(&self) -> (usize, usize, usize) {
        let receipt_bytes = self
            .receipts
            .iter()
            .map(|turn| {
                turn.turn_id.len()
                    + turn
                        .provider_tool_call_ids
                        .iter()
                        .map(String::len)
                        .sum::<usize>()
                    + turn.tool_result_ids.iter().map(String::len).sum::<usize>()
                    + turn
                        .checkpoint_ref
                        .as_ref()
                        .map(|checkpoint| {
                            checkpoint.checkpoint_id.len()
                                + checkpoint.session_id.len()
                                + checkpoint.turn_id.len()
                                + checkpoint.status.len()
                                + checkpoint
                                    .done_reason
                                    .as_ref()
                                    .map(String::len)
                                    .unwrap_or(0)
                                + checkpoint.payload_sha256.len()
                        })
                        .unwrap_or(0)
            })
            .sum();
        (
            usize::from(self.last_turn.is_some()),
            self.last_turn
                .as_ref()
                .map(|turn| serde_json::to_vec(&turn.session_snapshot).unwrap().len())
                .unwrap_or(0),
            receipt_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn step(round: usize) -> TurnStepResult {
        let message = ChatMessage {
            message_id: format!("message-{round}"),
            role: MessageRole::User,
            content: "x".repeat(round * 32),
            created_at_ms: 1,
            metadata: Default::default(),
        };
        TurnStepResult {
            current_assistant_reply: None,
            turn_id: format!("turn-{round}"),
            continuation: QueryContinuation::ExecuteTools,
            checkpoint: Some(CheckpointRecord {
                checkpoint_id: format!("checkpoint-{round}"),
                kind: contracts::CheckpointKindV1::Wait,
                session_id: "session-retention".into(),
                turn_id: format!("turn-{round}"),
                status: "waiting".into(),
                done_reason: None,
                updated_at_ms: 1,
                payload_json: "正文".repeat(512),
            }),
            provider_tool_calls: vec![ToolCallEnvelope {
                id: format!("call-{round}"),
                name: "test_tool".into(),
                args_json: "x".repeat(1024),
            }],
            tool_results: vec![ToolExecutionResult {
                tool_call_id: format!("call-{round}"),
                tool_name: "test_tool".into(),
                status: "ok".into(),
                content: "y".repeat(1024),
                details: serde_json::json!({"body":"z".repeat(1024)}),
                facts: vec![],
                error: None,
                started_at_ms: 1,
                completed_at_ms: 2,
                latency_ms: 1,
                parallel_group: None,
                transition_reason: None,
            }],
            tool_use_summary: None,
            tool_operations_json: None,
            agent_run_resource_usage: Default::default(),
            runtime_events: vec![],
            session_snapshot: SessionStateSnapshot {
                session_id: "session-retention".into(),
                messages: vec![message.clone()],
                context_window: vec![message],
                model_semantics: Default::default(),
                completed_turn: None,
                updated_at_ms: 1,
                metadata: Default::default(),
            },
        }
    }
    #[test]
    fn query_loop_result_retention_keeps_one_current_snapshot_through_ten_thousand_commits() {
        let mut previous = None;
        for rounds in [100, 1000, 10000] {
            let mut collector = RunResultCollector::default();
            for round in 1..=rounds {
                collector.push(step(round));
            }
            let (snapshots, body_bytes, receipt_bytes) = collector.retained_payloads();
            println!(
                "{}",
                serde_json::json!({"rounds":rounds,"retainedFullSnapshots":snapshots,"retainedSnapshotSerializedBytes":body_bytes,"receiptIdentityBytes":receipt_bytes})
            );
            assert_eq!(
                snapshots, 1,
                "committed historical snapshots must not remain retained"
            );
            let last_bytes = serde_json::to_vec(&step(rounds).session_snapshot)
                .unwrap()
                .len();
            let retained = collector.last().unwrap();
            let current_output_bytes = retained
                .provider_tool_calls
                .iter()
                .map(|call| call.args_json.len())
                .sum::<usize>()
                + retained
                    .tool_results
                    .iter()
                    .map(|result| {
                        result.content.len() + serde_json::to_vec(&result.details).unwrap().len()
                    })
                    .sum::<usize>()
                + retained.checkpoint.as_ref().unwrap().payload_json.len();
            assert_eq!(
                current_output_bytes,
                1024 + 1024
                    + serde_json::to_vec(&serde_json::json!({"body":"z".repeat(1024)}))
                        .unwrap()
                        .len()
                    + 3072
            );
            println!(
                "{}",
                serde_json::json!({"rounds":rounds,"retainedCurrentToolAndCheckpointContentBytes":current_output_bytes,"retainedToolCallCount":retained.provider_tool_calls.len(),"retainedToolResultCount":retained.tool_results.len()})
            );

            assert_eq!(retained.provider_tool_calls.len(), 1);
            assert_eq!(retained.tool_results.len(), 1);
            assert_eq!(collector.receipts.len(), rounds);
            let checkpoint = collector
                .receipts
                .last()
                .unwrap()
                .checkpoint_ref
                .as_ref()
                .unwrap();
            assert_eq!(checkpoint.payload_byte_length, 3072);
            assert_eq!(
                checkpoint.payload_sha256,
                format!("sha256:{:x}", Sha256::digest("正文".repeat(512).as_bytes()))
            );
            assert_eq!(
                collector.receipts.last().unwrap().tool_result_ids,
                vec![format!("call-{rounds}")]
            );
            assert_eq!(body_bytes, last_bytes);
            if let Some(previous_bytes) = previous {
                assert!(body_bytes + receipt_bytes <= previous_bytes * 12);
            }
            previous = Some(body_bytes + receipt_bytes);
        }
    }
    #[test]
    fn query_loop_final_reply_never_reuses_a_historical_assistant() {
        for continuation in [
            QueryContinuation::Finalize,
            QueryContinuation::CompleteTerminalTool,
            QueryContinuation::AwaitQuestion,
        ] {
            let mut turn = step(1);
            turn.continuation = continuation;
            turn.session_snapshot.messages[0].role = MessageRole::Assistant;
            turn.session_snapshot.messages[0].content = "old run answer".into();
            let mut collector = RunResultCollector::default();
            collector.push(turn);
            let result = collector.into_result(AgentRunStop::Finalized);
            assert_eq!(result.final_reply, None);
            assert!(matches!(
                subagent_worker_outcome_from_query_loop_response(result),
                SubagentWorkerRunOutcome::Failed { .. }
            ));
        }
        for continuation in [
            QueryContinuation::Finalize,
            QueryContinuation::CompleteTerminalTool,
        ] {
            let mut collector = RunResultCollector::default();
            collector.push(step(1));
            let mut turn = step(2);
            turn.continuation = continuation;
            turn.current_assistant_reply = nonempty_current_assistant_reply(" current result ");
            collector.push(turn);
            let result = collector.into_result(AgentRunStop::Finalized);
            assert_eq!(result.final_reply.as_deref(), Some("current result"));
            assert!(
                matches!(subagent_worker_outcome_from_query_loop_response(result),SubagentWorkerRunOutcome::Succeeded{summary,output_refs} if summary=="current result" && output_refs==vec!["turn:turn-1","turn:turn-2"])
            );
        }
        assert_eq!(nonempty_current_assistant_reply("  "), None);
    }
}
