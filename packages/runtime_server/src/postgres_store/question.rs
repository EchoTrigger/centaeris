use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::runtime::question::{QuestionCheckpoint, QuestionResponse, RESPONSE_SCHEMA};
use centaeris_core::session::reliability::WakeRuntimeJobRequest;

use super::{runtime::append_runtime_event_idempotent, PostgresRuntimeStore};

impl PostgresRuntimeStore {
    pub(crate) fn respond_to_question(
        &self,
        session_id: &str,
        job_id: &str,
        response: &QuestionResponse,
        now: i64,
    ) -> Result<&'static str, String> {
        response.validate()?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            // Serialize the operation before looking up its durable receipt, including replays
            // after the original wait was consumed or the original Job became terminal.
            tx.query_one("SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                &[&format!("question:{}:{}", response.agent_run_id, response.operation_id)])
                .map_err(|error| error.to_string())?;
            let rows = tx.query(
                "SELECT event_id,session_id,payload_json FROM runtime_events WHERE event_type=$1 AND (event_id=$2 OR session_id=$3)",
                &[&RESPONSE_SCHEMA,&response.event_id(),&session_id],
            ).map_err(|error| error.to_string())?;
            for row in rows {
                // Runtime text events may contain JSON escapes such as U+0000, which
                // PostgreSQL JSONB rejects. Decode response text with the Core codec.
                let existing: QuestionResponse = serde_json::from_str(&row.get::<_,String>(2)).map_err(|error| error.to_string())?;
                if row.get::<_,String>(0) != response.event_id()
                    && (existing.agent_run_id != response.agent_run_id || existing.operation_id != response.operation_id) {
                    continue;
                }
                if row.get::<_,String>(1) != session_id || existing != *response {
                    return Err("question_response_idempotency_conflict".into());
                }
                tx.commit().map_err(|error| error.to_string())?;
                return Ok("existing");
            }
            super::turn_supplement::validate_active_job(&mut tx,&response.agent_run_id,job_id,
                session_id,&response.authorization_digest,None,true).map_err(|error| error.to_string())?;
            let row = tx.query_opt(
                "SELECT turn_id,payload_json FROM checkpoints WHERE session_id=$1 AND kind='wait' AND done_reason='question' FOR UPDATE",
                &[&session_id],
            ).map_err(|error| error.to_string())?.ok_or("question_wait_not_found")?;
            let wait: QuestionCheckpoint = serde_json::from_str(&row.get::<_,String>(1))
                .map_err(|error| format!("invalid question v2 checkpoint: {error}"))?;
            wait.validate()?;
            response.validate_binding(&wait)?;
            if row.get::<_,String>(0) != wait.turn_id { return Err("question_checkpoint_turn_mismatch".into()); }
            append_runtime_event_idempotent(&mut tx,&RuntimeEvent {
                event_id:response.event_id(),session_id:session_id.into(),task_id:Some(wait.turn_id),
                event_type:RESPONSE_SCHEMA.into(),at_ms:now,visibility:EventVisibility::Internal,
                payload_json:serde_json::to_string(response).map_err(|error| error.to_string())?,
            })?;
            // Reuse the durable wake receipt so an answer racing a running step's yield
            // remains runnable; a bare run_at_ms update would lose that wake.
            super::reliability::wake_runtime_job_in_transaction(&mut tx,&WakeRuntimeJobRequest {
                job_id:job_id.into(),source_job_id:response.event_id(),woken_at_ms:now,
                transition_reason:"question_response".into(),
            })?;
            tx.query_one("SELECT public.workspace_presentation_bump_v1($1)", &[&format!("session:{session_id}:state")])
                .map_err(|error| format!("notify question response presentation failed: {error}"))?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok("accepted")
        })
    }
}
