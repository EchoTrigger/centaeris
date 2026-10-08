use centaeris_core::runtime::subagent::SubagentSchedulerEvent;
use centaeris_core::session::store::RuntimeStoreActor;

async fn active_job_snapshot<T, Fetch, FetchFuture>(mut fetch: Fetch) -> Result<Vec<T>, String>
where
    Fetch: FnMut(usize, usize) -> FetchFuture,
    FetchFuture: std::future::Future<Output = Result<Vec<T>, String>>,
{
    let mut limit = 128;
    loop {
        // Only the last complete query is a snapshot. Completed jobs may shrink
        // earlier prefixes, so offsets and merged pages cannot establish completeness.
        let jobs = fetch(limit, 0).await?;
        if jobs.len() < limit {
            return Ok(jobs);
        }
        limit = limit
            .checked_mul(2)
            .ok_or_else(|| "terminal_subagent_snapshot_limit_overflow".to_string())?;
    }
}

/// The host calls this only after an authoritative parent terminal fact.
/// Core owns child cancellation; the host supplies the exact parent identity.
pub(crate) async fn settle_terminal_parent_subagent_jobs(
    store: &RuntimeStoreActor,
    session_id: &str,
    parent_agent_run_id: &str,
    at_ms: i64,
) -> Result<Vec<SubagentSchedulerEvent>, String> {
    use centaeris_core::runtime::subagent::{
        cancel_subagent_run_job_async, load_subagent_work_packet_async,
        subagent_work_packet_runtime_binding, CancelSubagentRunJobRequest, SUBAGENT_RUN_JOB_KIND,
    };
    use centaeris_core::session::reliability::{ListRuntimeJobsRequest, RuntimeJobStatus};

    if session_id.trim().is_empty() || parent_agent_run_id.trim().is_empty() {
        return Err("terminal_parent_identity_missing".to_string());
    }
    let mut events = Vec::new();
    loop {
        let jobs = active_job_snapshot(|limit, offset| {
            store.list_runtime_jobs(ListRuntimeJobsRequest {
                statuses: vec![
                    RuntimeJobStatus::Queued,
                    RuntimeJobStatus::Leased,
                    RuntimeJobStatus::Running,
                ],
                job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
                session_id: Some(session_id.to_string()),
                branch_id: None,
                limit,
                offset,
            })
        })
        .await?;
        let mut owned_seen = false;
        for job in jobs {
            let packet = load_subagent_work_packet_async(store, &job).await?;
            let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
            if binding.parent_agent_run_id != parent_agent_run_id {
                continue;
            }
            owned_seen = true;
            match cancel_subagent_run_job_async(
                store,
                CancelSubagentRunJobRequest {
                    job_id: job.job_id.clone(),
                    reason: "parent_agent_run_terminal".to_string(),
                    cancelled_at_ms: at_ms,
                },
            )
            .await
            {
                Ok(event) => events.push(event),
                Err(error) => {
                    // A worker may win the terminal race. Preserve its authoritative result.
                    if !store
                        .get_runtime_job(&job.job_id)
                        .await?
                        .is_some_and(|current| current.status.is_terminal())
                    {
                        return Err(error);
                    }
                }
            }
        }
        if !owned_seen {
            return Ok(events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
    use centaeris_core::session::reliability::{
        ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest, RuntimeJobStatus,
        ScheduleRuntimeJobRequest, StartRuntimeJobRequest,
    };

    async fn child(
        store: &RuntimeStoreActor,
        session: &str,
        parent: &str,
        turn: &str,
        name: &str,
        status: RuntimeJobStatus,
    ) -> String {
        let contract = centaeris_core::tool::list_tool_contracts()
            .into_iter()
            .find(|c| c.name == "read")
            .unwrap();
        let packet = serde_json::json!({
            "run_context": {"schema":"agent_run_context_v1", "sessionId":session, "branchId":format!("child-{name}"), "turnId":format!("child-turn-{name}"), "agentRunId":format!("child-run-{name}"), "agentRef":{"agentId":name,"agentRunId":format!("child-run-{name}")}, "parentAgentRef":{"agentId":"main","agentRunId":parent}, "parentTurnId":turn,"depth":1,"cwd":std::env::current_dir().unwrap().display().to_string(),"cancellationScopeId":format!("scope-{name}"),"parentCancellationScopeId":"parent-scope","cancelOnParentCancel":true,"createdAtMs":100},
            "task_brief":{"task_id":null,"objective":"Synthetic child","success_criteria":[],"constraints":[],"output_hint":null},
            "hot_view":{"summary":"","recent_message_ids":[],"state_kv":{}}, "object_refs":[],
            "allowedTools":["read"],"delegatedToolContracts":[{"name":"read","providerId":contract.provider_id,"contractDigest":contract.contract_digest().unwrap(),"concurrencySafe":contract.concurrency_safe}],"writablePathPrefixes":[],
            "output_contract":{"response_mode":"summary","expected_sections":[],"require_artifact_refs":false,"max_summary_chars":null},"parent_checkpoint_id":null,"context_mode":"Borrow"
        });
        let job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: session.to_string(),
            parent_turn_id: turn.to_string(),
            tool_call_id: name.to_string(),
            subagent_id: name.to_string(),
            work_packet_ref: serde_json::json!({"workPacket": packet}).to_string(),
            checkpoint_id: None,
            run_at_ms: 100,
            created_at_ms: 100,
            max_retries: 0,
        });
        let id = job.job_id.clone();
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
            .await
            .unwrap();
        if status != RuntimeJobStatus::Queued {
            let claimed = store
                .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                    now_ms: 200,
                    worker_id: "worker:terminal-test".to_string(),
                    job_id: Some(id.clone()),
                    job_kind: Some("subagent.run".to_string()),
                    session_id: None,
                    limit: 1,
                    lease_ms: 1000,
                })
                .await
                .unwrap();
            let owner = claimed[0].lease_owner.clone().unwrap();
            if status == RuntimeJobStatus::Running {
                store
                    .start_runtime_job(StartRuntimeJobRequest {
                        job_id: id.clone(),
                        lease_owner: owner,
                        started_at_ms: 200,
                    })
                    .await
                    .unwrap();
            } else if status == RuntimeJobStatus::Succeeded {
                store
                    .complete_runtime_job(CompleteRuntimeJobRequest {
                        job_id: id.clone(),
                        lease_owner: owner,
                        completed_at_ms: 200,
                        output_refs: vec![],
                    })
                    .await
                    .unwrap();
            }
        }
        id
    }

    #[test]
    fn active_snapshot_keeps_owned_child_when_other_jobs_finish_between_reads() {
        let mut reads = 0;
        let snapshot = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(active_job_snapshot(|limit, offset| {
                // Initially a full prefix contains only another run's jobs. They then finish,
                // shifting the target child from beyond that prefix to the beginning.
                let current = if reads == 0 {
                    (0..128).chain(std::iter::once(999)).collect::<Vec<_>>()
                } else {
                    vec![999]
                };
                reads += 1;
                let page = current.into_iter().skip(offset).take(limit).collect();
                std::future::ready(Ok(page))
            }))
            .unwrap();
        assert_eq!(snapshot, vec![999]);
    }

    #[test]
    fn terminal_parent_settles_all_its_turns_and_preserves_other_runs_and_terminal_children() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-results")
            .join(format!(
                "terminal-subagent-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let backend =
            centaeris_runtime_sqlite::SqliteRuntimeStore::new(root.join("runtime.sqlite")).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let store = RuntimeStoreActor::start(backend).unwrap();
            let queued = child(
                &store,
                "session",
                "old-run",
                "first-turn",
                "queued",
                RuntimeJobStatus::Queued,
            )
            .await;
            let leased = child(
                &store,
                "session",
                "old-run",
                "later-turn",
                "leased",
                RuntimeJobStatus::Leased,
            )
            .await;
            let running = child(
                &store,
                "session",
                "old-run",
                "later-turn",
                "running",
                RuntimeJobStatus::Running,
            )
            .await;
            let completed = child(
                &store,
                "session",
                "old-run",
                "first-turn",
                "completed",
                RuntimeJobStatus::Succeeded,
            )
            .await;
            let newer = child(
                &store,
                "session",
                "new-run",
                "new-turn",
                "newer",
                RuntimeJobStatus::Queued,
            )
            .await;
            let other = child(
                &store,
                "other-session",
                "old-run",
                "first-turn",
                "other",
                RuntimeJobStatus::Queued,
            )
            .await;
            let events = settle_terminal_parent_subagent_jobs(&store, "session", "old-run", 300)
                .await
                .unwrap();
            assert_eq!(events.len(), 3);
            for id in [queued, leased, running] {
                let job = store.get_runtime_job(&id).await.unwrap().unwrap();
                assert_eq!(job.status, RuntimeJobStatus::Cancelled);
                assert_eq!(job.lease_owner, None);
                assert_eq!(job.lease_expires_at_ms, None);
            }
            assert_eq!(
                store
                    .get_runtime_job(&completed)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                RuntimeJobStatus::Succeeded
            );
            for id in [newer, other] {
                assert_eq!(
                    store.get_runtime_job(&id).await.unwrap().unwrap().status,
                    RuntimeJobStatus::Queued
                );
            }
            assert!(
                settle_terminal_parent_subagent_jobs(&store, "session", "old-run", 400)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    }
}
