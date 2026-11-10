use super::*;
use crate::handlers::{handle_request_async, RuntimeHostState};
use crate::message_log;
use crate::protocol::HostCommandRequest;
use crate::runtime_server::RuntimeClientKind;
use crate::subagent_scheduler::queue_tests::Profile;
use centaeris_core::runtime::contracts::{
    AgentRunInterventionV1, AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1,
};
use centaeris_core::runtime::TurnControl;
use serde_json::{json, Value};
use std::error::Error;
use std::sync::Mutex;

async fn rpc(
    writer: &EventWriter,
    method: &str,
    request: Value,
) -> Result<Value, RuntimeHostError> {
    handle_request_async(
        Arc::new(Mutex::new(RuntimeHostState::default())),
        HostCommandRequest {
            command: method.to_string(),
            payload: json!({"request": request}),
        },
        writer.clone(),
    )
    .await
}

#[test]
fn sdk_controls_share_run_input_and_resume_observation() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let profile = Profile::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime.block_on(async {
        let outcome = async {
            let clients = Arc::new(RuntimeServerClientHub::default());
            let native = connect_native(&clients, "sdk-control-owner")?;
            let (writer, _outbound) = clients.connect()?.ok_or("RPC client was rejected")?;
            writer.register_client(RuntimeClientKind::Tui, "rpc-control-observer")?;
            let session = native
                .create_session(SessionCreateCommandRequest {
                    operation_id: "sdk-control-session".try_into()?,
                    title: Some("SDK controls".to_string()),
                    cwd: profile
                        .root
                        .join("workspace")
                        .to_string_lossy()
                        .into_owned(),
                })
                .await?;
            let other = native
                .create_session(SessionCreateCommandRequest {
                    operation_id: "sdk-other-control-session".try_into()?,
                    title: None,
                    cwd: profile
                        .root
                        .join("workspace")
                        .to_string_lossy()
                        .into_owned(),
                })
                .await?;
            let run_id = "sdk-control-run";
            let turn_id = "sdk-control-turn";
            message_log::append_agent_run_started(session.id(), turn_id, run_id, "start", 1)?;
            let control = TurnControl::new();
            let lease = writer.start_agent_run(session.id(), run_id, turn_id, control.clone())?;

            let blank_cancellation = session
                .cancel("  ", None)
                .await
                .expect_err("bound cancellation requires a Run identity");
            assert_eq!(blank_cancellation.code(), "invalid_request");
            let blank_subscription = session
                .subscribe(Some("  "))
                .await
                .expect_err("an explicit Run identity must be nonempty");
            assert_eq!(blank_subscription.code(), "invalid_request");

            let unknown_attachment = rpc(
                &writer,
                "_centaeris/session/agent-runs/attach",
                json!({
                    "sessionId": session.id(),
                    "agentRunId": "sdk-unknown-control-run",
                    "viewerId": "rpc-control-observer",
                }),
            )
            .await?;
            assert!(unknown_attachment["agentRun"].is_null());
            let unknown_cancellation = rpc(
                &writer,
                "_centaeris/session/agent-runs/cancel",
                json!({"sessionId": session.id(), "agentRunId": "sdk-unknown-control-run"}),
            )
            .await?;
            assert!(unknown_cancellation["agentRun"].is_null());
            assert_eq!(unknown_cancellation["cancelAccepted"], false);
            let missing_native_run = session.cancel("sdk-unknown-control-run", None).await?;
            assert!(missing_native_run.agent_run.is_none());
            assert!(!missing_native_run.cancel_accepted);

            let whitespace_attachment = rpc(
                &writer,
                "_centaeris/session/agent-runs/attach",
                json!({
                    "sessionId": format!(" \t{}\n ", session.id()),
                    "agentRunId": run_id,
                    "viewerId": "rpc-control-observer",
                }),
            )
            .await?;
            assert_eq!(whitespace_attachment["agentRun"]["agentRunId"], run_id);
            assert_eq!(whitespace_attachment["agentRun"]["sessionId"], session.id());

            let wrong_subscription = rpc(
                &writer,
                "_centaeris/session/agent-runs/attach",
                json!({
                    "sessionId": other.id(),
                    "agentRunId": run_id,
                    "viewerId": "rpc-control-observer",
                }),
            )
            .await
            .expect_err("RPC attachment must reject mismatched Run ownership");
            assert_eq!(wrong_subscription.code(), "invalid_request");
            let wrong_cancellation = rpc(
                &writer,
                "_centaeris/session/agent-runs/cancel",
                json!({"sessionId": other.id(), "agentRunId": run_id}),
            )
            .await
            .expect_err("RPC cancellation must reject mismatched Run ownership");
            assert_eq!(wrong_cancellation.code(), "invalid_request");

            let wrong_supplement = other
                .supplement(run_id, "belongs to another Session".to_string())
                .await
                .expect_err("bound Session must reject another Session's Run");
            let rpc_wrong_supplement = rpc(
                &writer,
                "_centaeris/session/supplement",
                json!({
                    "sessionId": other.id(),
                    "agentRunId": run_id,
                    "message": "belongs to another Session",
                }),
            )
            .await
            .expect_err("RPC must reject the mismatched Session");
            assert_eq!(wrong_supplement.code(), rpc_wrong_supplement.code());
            assert_eq!(wrong_supplement.message(), rpc_wrong_supplement.message());
            let wrong_intervention = other
                .answer_now(AgentRunInterventionV1::answer_now(
                    "sdk-wrong-session-intervention",
                    run_id,
                ))
                .await
                .expect_err("bound Session must reject a foreign Run intervention");
            assert_eq!(wrong_intervention.code(), "invalid_request");
            assert!(!control.is_answer_now_requested()?);

            let supplement = session
                .supplement(run_id, "from native SDK".to_string())
                .await?;
            assert!(supplement.accepted);
            assert_eq!(supplement.session_id, session.id());
            assert_eq!(supplement.agent_run_id, run_id);
            assert_eq!(supplement.queued_count, 1);
            let supplemented = rpc(
                &writer,
                "_centaeris/session/supplement",
                json!({"sessionId": session.id(), "agentRunId": run_id, "message": "from RPC"}),
            )
            .await?;
            assert_eq!(supplemented["accepted"], true);
            assert_eq!(supplemented["queuedCount"], 2);
            assert_eq!(supplemented["sessionId"], session.id());
            assert_eq!(supplemented["agentRunId"], run_id);
            assert_eq!(
                control.take_resume_message(turn_id)?.as_deref(),
                Some("from native SDK")
            );
            assert_eq!(
                control.take_resume_message(turn_id)?.as_deref(),
                Some("from RPC")
            );
            assert!(control.take_resume_message(turn_id)?.is_none());

            let (resumed, subscription) = native.resume_session(session.id()).await?;
            assert_eq!(resumed.id(), session.id());
            assert_eq!(
                subscription
                    .agent_run
                    .ok_or("resumed Run missing")?
                    .agent_run_id,
                run_id
            );
            let observed = resumed.list_runs(true).await?;
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].agent_run_id, run_id);
            assert_eq!(observed[0].status, "running");
            assert_eq!(
                message_log::project_agent_runs_for_session(session.id())?.len(),
                1
            );

            let native_compaction = session
                .compact_context()
                .await
                .expect_err("active Run blocks manual compaction");
            let rpc_compaction = rpc(
                &writer,
                "_centaeris/session/compact_context",
                json!({"sessionId": session.id()}),
            )
            .await
            .expect_err("RPC must reject the same active Run");
            assert_eq!(native_compaction.code(), rpc_compaction.code());
            assert_eq!(native_compaction.message(), rpc_compaction.message());

            let intervention = AgentRunInterventionV1::answer_now("sdk-control-answer-now", run_id);
            let accepted = session.answer_now(intervention.clone()).await?;
            assert!(accepted.accepted);
            assert_eq!(accepted.disposition, "accepted");
            assert_eq!(accepted.session_id, session.id());
            assert_eq!(accepted.agent_run_id, run_id);
            assert!(control.is_answer_now_requested()?);
            let repeated = rpc(
                &writer,
                "_centaeris/session/answer_now",
                serde_json::to_value(intervention)?,
            )
            .await?;
            assert_eq!(repeated["accepted"], false);
            assert_eq!(repeated["disposition"], "alreadyConverging");
            assert_eq!(repeated["interventionId"], accepted.intervention_id);
            let changes: Vec<_> = agent_runtime::agent_runtime_store_actor()?
                .list_events(session.id(), 10, 0)
                .await?
                .into_iter()
                .filter(|event| event.event_type == AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1)
                .collect();
            assert_eq!(changes.len(), 1);
            let requested: Value = serde_json::from_str(&changes[0].payload_json)?;
            assert_eq!(requested["actorId"], "sdk-control-owner");
            assert_eq!(requested["interventionId"], accepted.intervention_id);

            message_log::append_agent_run_terminal(
                session.id(),
                turn_id,
                run_id,
                "cancelled",
                Some("test owner finished"),
                2,
            )?;
            writer.finish_agent_run(&lease.lease_id)?;
            assert!(session.list_runs(false).await?.is_empty());
            let terminal = session.list_runs(true).await?;
            assert_eq!(terminal.len(), 1);
            assert_eq!(terminal[0].agent_run_id, run_id);
            assert_eq!(terminal[0].status, "cancelled");
            clients.disconnect(&writer)?;
            Ok::<(), Box<dyn Error>>(())
        }
        .await;
        let closed = agent_runtime::close_storage_owners().await;
        outcome?;
        closed?;
        Ok::<(), Box<dyn Error>>(())
    });
    drop(runtime);
    drop(profile);
    result.expect("SDK and RPC control behavior");
}

#[test]
fn sdk_question_and_compaction_share_rpc_rejections() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let profile = Profile::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime.block_on(async {
        let outcome = async {
            let clients = Arc::new(RuntimeServerClientHub::default());
            let native = connect_native(&clients, "sdk-rejected-control-owner")?;
            let (writer, _outbound) = clients.connect()?.ok_or("RPC client was rejected")?;
            writer.register_client(RuntimeClientKind::Tui, "rpc-rejected-control-observer")?;
            let session = native
                .create_session(SessionCreateCommandRequest {
                    operation_id: "sdk-rejected-control-session".try_into()?,
                    title: None,
                    cwd: profile
                        .root
                        .join("workspace")
                        .to_string_lossy()
                        .into_owned(),
                })
                .await?;
            let native_question = session
                .answer_question(QuestionAnswer {
                    question_id: "question-without-checkpoint".to_string(),
                    answers: Some(vec!["continue".to_string()]),
                    answer_text: None,
                    auto_continue_after_resume_wait: None,
                })
                .await
                .expect_err("question answer requires a paused checkpoint");
            let rpc_question = rpc(
                &writer,
                "_centaeris/session/answer_question",
                json!({
                    "sessionId": session.id(),
                    "questionId": "question-without-checkpoint",
                    "answers": ["continue"],
                }),
            )
            .await
            .expect_err("RPC question requires the same checkpoint");
            assert_eq!(native_question.code(), rpc_question.code());
            assert_eq!(native_question.message(), rpc_question.message());

            let native_compaction = session
                .compact_context()
                .await
                .expect_err("compaction requires an existing Run");
            let rpc_compaction = rpc(
                &writer,
                "_centaeris/session/compact_context",
                json!({"sessionId": session.id()}),
            )
            .await
            .expect_err("RPC compaction requires the same Run");
            assert_eq!(native_compaction.code(), rpc_compaction.code());
            assert_eq!(native_compaction.message(), rpc_compaction.message());
            assert!(session.list_runs(true).await?.is_empty());
            let listed = rpc(
                &writer,
                "_centaeris/session/agent-runs",
                json!({"sessionId": session.id(), "includeTerminal": true}),
            )
            .await?;
            assert_eq!(listed["agentRuns"], json!([]));
            clients.disconnect(&writer)?;
            Ok::<(), Box<dyn Error>>(())
        }
        .await;
        let closed = agent_runtime::close_storage_owners().await;
        outcome?;
        closed?;
        Ok::<(), Box<dyn Error>>(())
    });
    drop(runtime);
    drop(profile);
    result.expect("SDK and RPC control rejection parity");
}
