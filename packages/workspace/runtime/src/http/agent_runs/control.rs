//! Terminal-only processing: no model client, host construction or execution grant.
use crate::*;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlRequest {
    schema: String,
    job_id: String,
    lease_owner: String,
    agent_run_control: crate::contract::AgentRunControl,
}

pub(crate) fn advance(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let request: ControlRequest = match serde_json::from_slice(request.body()) {
        Ok(value) => value,
        Err(_) => return json_error_response(400, "agent_run_control_invalid"),
    };
    if request.schema != "runtime.agent_run.control.v1"
        || !(16..=160).contains(&request.lease_owner.len())
        || request.lease_owner.chars().any(char::is_control)
    {
        return json_error_response(400, "agent_run_control_invalid");
    }
    let key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
        .map_err(|_| "control signing key unavailable")?;
    let checked = crate::contract::AgentRunCancelRequest {
        schema: crate::contract::AGENT_RUN_CANCEL_SCHEMA.into(),
        agent_run_control: request.agent_run_control.clone(),
    };
    if checked.validate(key.as_bytes()).is_err() {
        return json_error_response(400, "agent_run_control_invalid");
    }
    let control = &request.agent_run_control;
    if state
        .job_store
        .require_control_lease(control, &request.job_id, &request.lease_owner)
        .is_err()
    {
        return json_error_response(409, "agent_run_control_lease_rejected");
    }
    let result = advance_terminal(
        state.job_store.as_ref(),
        control,
        &request.job_id,
        &request.lease_owner,
    );
    match result {
        Ok(value) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&value).map_err(|e| e.to_string())?,
        )),
        Err(error) => {
            failure_diagnostics::report(&control.agent_run_id, "control_advance", &error);
            json_error_response(503, "agent_run_control_pending")
        }
    }
}

fn advance_terminal(
    store: &PostgresRuntimeStore,
    control: &crate::contract::AgentRunControl,
    job: &str,
    owner: &str,
) -> Result<serde_json::Value, String> {
    let original = &control.authorization;
    let run = &control.agent_run_id;
    let mut terminal =
        load_terminal_state_for_binding(store, run, original, &control.authorization_digest)?;
    if terminal.is_none() {
        let state = store.agent_run_execution_control_state(run, job, owner, now_ms()?)?;
        if !state.cancellation_requested || !state.lifecycle_lease_current {
            return Err("control cancellation evidence missing".into());
        }
        let mut sequence=store.with_client(|client| {
            let upper=client.query_one("SELECT COALESCE(MAX(agent_run_sequence),0) FROM app_core_sessionevent WHERE agent_run_id=$1 AND NOT session_level", &[&run]).map_err(|e|e.to_string())?.get::<_,i32>(0);
            let mut sequence=AgentRunSessionState::new(original.session_id.clone(),run.clone())?;
            let mut hydrator=postgres_store::SessionWireHydrator::default();
            recovery_read::visit(client,recovery_read::Scope::BoundRun {run,session:&original.session_id,workspace:&original.workspace_id},upper,|client,position,mut wire| {
                hydrator.hydrate_record(client,&mut wire)?;
                let event=parse_wire_record(&wire).map_err(|e|e.to_string())?.event;
                sequence.restore(SequencedSessionRecord {sequence:position as u64,event})
            })?;
            let position=client.query_one("SELECT COALESCE(MAX(sequence),0) FROM app_core_sessionevent WHERE session_id=$1", &[&original.session_id]).map_err(|e|e.to_string())?.get::<_,i32>(0);
            sequence.set_committed_session_sequence(u64::try_from(position).map_err(|_|"invalid Session position")?);
            Ok(sequence)
        })?;
        let log = store.session_log(
            original.workspace_id.clone(),
            original.session_id.clone(),
            String::new(),
        );
        let fence = RuntimeJobLeaseFence {
            job_id: job.into(),
            job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner: owner.into(),
        };
        if sequence.is_empty() {
            log.commit_pre_admission_cancellation(run, &control.authorization_digest, &fence)?;
        } else {
            let now = now_ms()?;
            let mut records = vec![];
            if let Some(end) =
                end_active_execution(&mut sequence, run, "cancelled", "cancelled", false, now)?
            {
                records.push(end);
            }
            records.push(sequence.interrupt(
                run,
                "cancelled",
                "AgentRun cancelled by user.",
                false,
                now,
            )?);
            log.append_session_records_with_runtime_job_lease_blocking(run, &records, &fence)?;
        }
        terminal =
            load_terminal_state_for_binding(store, run, original, &control.authorization_digest)?;
    }
    let terminal = terminal.ok_or("control terminal commit missing")?;
    let mut result = json!({"schema":"runtime.agent_run.control.result.v1","agentRunId":run,"disposition":"terminal","terminalState":terminal});
    if let Some(receipts) = store.confirm_no_effects(control, job, owner)? {
        result["disposition"] = json!("released");
        result["cleanupReceiptId"] = receipts["cleanupReceiptId"].clone();
        result["releaseReceiptId"] = receipts["releaseReceiptId"].clone();
    }
    Ok(result)
}
