//! Physical cleanup orchestration; transport adapters return Core task results.
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskEnvelope, EnvironmentTaskFence,
    EnvironmentTaskRequest, EnvironmentTaskResult, EnvironmentTaskScope,
    ENVIRONMENT_TASK_MAX_RECORDS, ENVIRONMENT_TASK_READ_BYTES, ENVIRONMENT_TASK_SCHEMA,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) trait RetainedCleanupHost {
    fn task_rpc(
        &self,
        instance_id: &str,
        envelope: &EnvironmentTaskEnvelope,
    ) -> Result<EnvironmentTaskResult, String>;
}

pub(crate) struct DockerCleanupHost<'a> {
    pub cleanup: &'a crate::contract::AgentRunCleanup,
    pub lease: &'a centaeris_core::session::RuntimeJobLeaseFence,
    pub allocation: &'a crate::postgres_store::execution_instances::RetainedInstanceAllocation,
    pub resources: centaeris_core::execution::ResidentResourceUsage,
    pub origin_authorization: &'a crate::agent_run_authorization::WorkspaceAgentRunAuthorization,
}

pub(crate) fn confirm_run_cleanup(
    host: &impl RetainedCleanupHost,
    fence: &EnvironmentTaskFence,
) -> Result<(), String> {
    if fence.controller_purpose != EnvironmentTaskControllerPurpose::Cleanup
        || fence.controller_epoch == 0
    {
        return Err("retained_cleanup_authority_invalid".into());
    }
    let rpc = |request| {
        host.task_rpc(
            &fence.environment_instance_id,
            &EnvironmentTaskEnvelope {
                schema: ENVIRONMENT_TASK_SCHEMA.into(),
                request,
            },
        )
    };
    match rpc(EnvironmentTaskRequest::Takeover {
        environment_instance_id: fence.environment_instance_id.clone(),
        controller_id: fence.controller_id.clone(),
        controller_epoch: fence.controller_epoch,
        controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
    })? {
        EnvironmentTaskResult::TakenOver { fence: received } if &received == fence => {}
        _ => return Err("retained_cleanup_takeover_unconfirmed".into()),
    }
    let ids = match rpc(EnvironmentTaskRequest::EndRun {
        fence: fence.clone(),
        run_id: fence.controller_id.clone(),
        operation_id: fence.operation_id(&format!(
            "run_terminal_cleanup:{:x}",
            Sha256::digest(fence.controller_id.as_bytes())
        )),
    })? {
        EnvironmentTaskResult::RunEnded { task_ids, .. }
            if task_ids.len() <= ENVIRONMENT_TASK_MAX_RECORDS =>
        {
            task_ids
        }
        _ => return Err("retained_cleanup_endrun_unconfirmed".into()),
    };
    let expected: BTreeSet<_> = ids.iter().cloned().collect();
    if expected.len() != ids.len() {
        return Err("retained_cleanup_task_identity_invalid".into());
    }
    let tasks = match rpc(EnvironmentTaskRequest::List {
        fence: fence.clone(),
    })? {
        EnvironmentTaskResult::Listed { tasks } if tasks.len() <= ENVIRONMENT_TASK_MAX_RECORDS => {
            tasks
        }
        _ => return Err("retained_cleanup_list_unconfirmed".into()),
    };
    let scope = EnvironmentTaskScope::Run {
        run_id: fence.controller_id.clone(),
    };
    let listed: Vec<_> = tasks.iter().filter(|task| task.scope == scope).collect();
    let actual: BTreeSet<_> = listed.iter().map(|task| task.task_id.clone()).collect();
    if actual != expected || actual.len() != listed.len() {
        return Err("retained_cleanup_task_identity_invalid".into());
    }
    // EndRun is an idempotent action receipt, not a later observation. Query
    // each retained identity using the same cleanup fence to confirm cgroup zero.
    for id in expected {
        match rpc(EnvironmentTaskRequest::Query {
            fence: fence.clone(),
            task_id: id.clone(),
            cursor: 0,
            max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
        })? {
            EnvironmentTaskResult::Task { task, .. }
                if task.task_id == id && task.scope == scope && task.cleanup_complete => {}
            _ => return Err("retained_cleanup_unconfirmed".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::{
        EnvironmentTaskControllerPurpose, EnvironmentTaskScope, EnvironmentTaskSnapshot,
        EnvironmentTaskState,
    };
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct Host {
        replies: RefCell<VecDeque<EnvironmentTaskResult>>,
        requests: RefCell<Vec<EnvironmentTaskRequest>>,
    }
    impl RetainedCleanupHost for Host {
        fn task_rpc(
            &self,
            instance: &str,
            envelope: &EnvironmentTaskEnvelope,
        ) -> Result<EnvironmentTaskResult, String> {
            assert_eq!(instance, "instance-1");
            envelope.validate()?;
            envelope.request.validate_authority()?;
            if let EnvironmentTaskRequest::Query { max_bytes, .. } = &envelope.request {
                // This same Core budget is exercised against the actual output
                // reader in hosted_execution's task service contract test.
                assert_eq!(
                    *max_bytes as usize,
                    centaeris_core::execution::ENVIRONMENT_TASK_READ_BYTES
                );
            }
            self.requests.borrow_mut().push(envelope.request.clone());
            self.replies
                .borrow_mut()
                .pop_front()
                .ok_or("fixture exhausted".into())
        }
    }
    fn fence() -> EnvironmentTaskFence {
        EnvironmentTaskFence {
            environment_instance_id: "instance-1".into(),
            controller_id: "run-1".into(),
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        }
    }
    fn task(id: &str, scope: EnvironmentTaskScope, complete: bool) -> EnvironmentTaskSnapshot {
        EnvironmentTaskSnapshot {
            task_id: id.into(),
            operation_id: format!("start-{id}"),
            scope,
            state: EnvironmentTaskState::Unknown,
            exit_code: None,
            cleanup_complete: complete,
            output_complete: false,
        }
    }
    fn response(task: EnvironmentTaskSnapshot) -> EnvironmentTaskResult {
        EnvironmentTaskResult::Task {
            task,
            output: vec![],
            earliest_cursor: 0,
            next_cursor: 0,
            gap: false,
            has_more: false,
        }
    }
    fn host(
        end_ids: Vec<String>,
        tasks: Vec<EnvironmentTaskSnapshot>,
        queries: Vec<EnvironmentTaskResult>,
    ) -> Host {
        let mut replies = VecDeque::from([
            EnvironmentTaskResult::TakenOver { fence: fence() },
            EnvironmentTaskResult::RunEnded {
                task_ids: end_ids,
                cleanup_complete: false,
            },
            EnvironmentTaskResult::Listed { tasks },
        ]);
        replies.extend(queries);
        Host {
            replies: RefCell::new(replies),
            requests: RefCell::new(vec![]),
        }
    }

    #[test]
    fn asynchronous_endrun_requires_positive_query_and_preserves_environment_tasks() {
        let scope = EnvironmentTaskScope::Run {
            run_id: "run-1".into(),
        };
        let h = host(
            vec!["task-1".into()],
            vec![
                task("task-1", scope.clone(), false),
                task("background", EnvironmentTaskScope::Environment, false),
            ],
            vec![response(task("task-1", scope, true))],
        );
        assert!(confirm_run_cleanup(&h, &fence()).is_ok());
        let requests = h.requests.borrow();
        assert!(
            matches!(&requests[1],EnvironmentTaskRequest::EndRun {operation_id,run_id,..}
            if operation_id.starts_with("2:run_terminal_cleanup:") && run_id=="run-1")
        );
        assert_eq!(requests.len(), 4);
        assert!(
            matches!(&requests[3],EnvironmentTaskRequest::Query {task_id,..} if task_id=="task-1")
        );
    }

    #[test]
    fn unknown_or_accepted_without_cleanup_proof_remains_pending() {
        for state in [
            EnvironmentTaskState::Unknown,
            EnvironmentTaskState::Accepted,
            EnvironmentTaskState::Running,
        ] {
            let mut t = task(
                "task-1",
                EnvironmentTaskScope::Run {
                    run_id: "run-1".into(),
                },
                false,
            );
            t.state = state;
            let h = host(vec!["task-1".into()], vec![t.clone()], vec![response(t)]);
            assert!(confirm_run_cleanup(&h, &fence()).is_err());
        }
    }

    #[test]
    fn missing_or_extra_run_task_and_wrong_query_identity_are_not_cleanup_success() {
        let scope = EnvironmentTaskScope::Run {
            run_id: "run-1".into(),
        };
        for (ids, tasks, queries) in [
            (vec!["missing".into()], vec![], vec![]),
            (vec![], vec![task("extra", scope.clone(), true)], vec![]),
            (
                vec!["task-1".into()],
                vec![task("task-1", scope.clone(), true)],
                vec![response(task("other", scope.clone(), true))],
            ),
        ] {
            assert!(confirm_run_cleanup(&host(ids, tasks, queries), &fence()).is_err());
        }
    }

    #[test]
    fn no_run_tasks_and_live_environment_tasks_are_successful_run_cleanup() {
        let h = host(
            vec![],
            vec![task("background", EnvironmentTaskScope::Environment, false)],
            vec![],
        );
        assert!(confirm_run_cleanup(&h, &fence()).is_ok());
        assert_eq!(h.requests.borrow().len(), 3);
    }

    #[test]
    fn maximum_controller_identity_still_has_a_bounded_cleanup_operation() {
        let f = EnvironmentTaskFence {
            controller_id: "x".repeat(160),
            ..fence()
        };
        let h = Host {
            replies: RefCell::new(VecDeque::from([
                EnvironmentTaskResult::TakenOver { fence: f.clone() },
                EnvironmentTaskResult::RunEnded {
                    task_ids: vec![],
                    cleanup_complete: true,
                },
                EnvironmentTaskResult::Listed { tasks: vec![] },
            ])),
            requests: RefCell::new(vec![]),
        };
        assert!(confirm_run_cleanup(&h, &f).is_ok());
    }
}
