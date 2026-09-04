use super::normalize_tool_parallelism;
use crate::tool::concurrency::{
    ToolConcurrencyCoordinator, ToolConcurrencyPermit, ToolConcurrencySerialPermit,
};
use crate::tool::layer::{ToolExecutionResult, ToolInvocationRequest, ToolLayer};
use crate::tool::ToolErrorInfo;
use serde_json::json;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
pub(super) type AsyncToolExecutionFuture =
    Pin<Box<dyn Future<Output = ToolExecutionResult> + Send>>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ToolBatchExecutorEvent {
    Queued {
        index: usize,
        tool_call_id: String,
        tool_name: String,
    },
    Started {
        index: usize,
        tool_call_id: String,
        tool_name: String,
        parallel_group: String,
    },
    Finished {
        index: usize,
        tool_call_id: String,
        tool_name: String,
        status: String,
        parallel_group: String,
    },
}
#[derive(Clone)]
pub(super) struct ToolBatchExecutor {
    tools_port: ToolLayer,
    tool_parallelism: usize,
    execute_tool_async:
        Arc<dyn Fn(ToolInvocationRequest) -> AsyncToolExecutionFuture + Send + Sync>,
    tool_concurrency: ToolConcurrencyCoordinator,
}
struct ToolExecutionPermit {
    _parallel_permit: Option<ToolConcurrencyPermit>,
    _serial_permit: Option<ToolConcurrencySerialPermit>,
}
impl ToolExecutionPermit {
    fn parallel(permit: ToolConcurrencyPermit) -> Self {
        Self {
            _parallel_permit: Some(permit),
            _serial_permit: None,
        }
    }
    fn serial(permit: ToolConcurrencySerialPermit) -> Self {
        Self {
            _parallel_permit: None,
            _serial_permit: Some(permit),
        }
    }
}
#[derive(Debug, Clone)]
pub(super) struct ToolBatchExecutorResult {
    pub reports: Vec<(usize, ToolExecutionResult)>,
    pub recovery_policy_trace_json: Vec<String>,
    pub transition_reason: String,
}
impl ToolBatchExecutor {
    #[cfg(test)]
    pub(super) fn new_with_executors(
        tools_port: ToolLayer,
        execute_tool_async: Arc<
            dyn Fn(ToolInvocationRequest) -> AsyncToolExecutionFuture + Send + Sync,
        >,
    ) -> Self {
        Self::new_with_executors_and_parallelism(
            tools_port,
            super::DEFAULT_TOOL_PARALLELISM,
            execute_tool_async,
        )
    }
    #[cfg(test)]
    pub(super) fn new_with_executors_and_parallelism(
        tools_port: ToolLayer,
        tool_parallelism: usize,
        execute_tool_async: Arc<
            dyn Fn(ToolInvocationRequest) -> AsyncToolExecutionFuture + Send + Sync,
        >,
    ) -> Self {
        Self::new_with_executors_and_coordinator(
            tools_port,
            ToolConcurrencyCoordinator::new(tool_parallelism),
            execute_tool_async,
        )
    }
    pub(super) fn new_with_executors_and_coordinator(
        tools_port: ToolLayer,
        tool_concurrency: ToolConcurrencyCoordinator,
        execute_tool_async: Arc<
            dyn Fn(ToolInvocationRequest) -> AsyncToolExecutionFuture + Send + Sync,
        >,
    ) -> Self {
        let tool_parallelism = normalize_tool_parallelism(tool_concurrency.capacity());
        Self {
            tools_port,
            tool_parallelism,
            execute_tool_async,
            tool_concurrency,
        }
    }
    #[cfg(test)]
    pub(super) async fn execute_local_tools_async(
        &self,
        requests: Vec<(usize, ToolInvocationRequest)>,
        sink: Option<&mut (dyn FnMut(ToolBatchExecutorEvent) + Send)>,
    ) -> ToolBatchExecutorResult {
        self.execute_local_tools_with_result_sink_async(requests, sink, None)
            .await
            .expect("tool execution without a result sink cannot fail")
    }

    pub(super) async fn execute_local_tools_with_result_sink_async(
        &self,
        requests: Vec<(usize, ToolInvocationRequest)>,
        mut sink: Option<&mut (dyn FnMut(ToolBatchExecutorEvent) + Send)>,
        mut result_sink: Option<
            &mut (dyn FnMut(usize, ToolExecutionResult) -> Result<ToolExecutionResult, String>
                      + Send),
        >,
    ) -> Result<ToolBatchExecutorResult, String> {
        if requests.is_empty() {
            return Ok(ToolBatchExecutorResult {
                reports: vec![],
                recovery_policy_trace_json: vec![],
                transition_reason: "tool_batch_execution_async".to_string(),
            });
        }
        let mut pending = VecDeque::with_capacity(requests.len());
        let mut has_safe_parallel = false;
        let mut has_serial = false;
        for (index, request) in requests {
            emit_tool_executor_event(
                &mut sink,
                ToolBatchExecutorEvent::Queued {
                    index,
                    tool_call_id: request.tool_call_id.clone(),
                    tool_name: request.tool_name.clone(),
                },
            );
            let concurrency_safe = self.tools_port.can_handle(request.tool_name.as_str())
                && self
                    .tools_port
                    .is_concurrency_safe(request.tool_name.as_str());
            if concurrency_safe {
                has_safe_parallel = true;
            } else {
                has_serial = true;
            }
            pending.push_back((index, request, concurrency_safe));
        }
        let mut reports = vec![];
        let mut recovery_policy_trace_json = vec![];
        let transition_reason = if !has_safe_parallel {
            "tool_batch_execution_serial_async".to_string()
        } else if !has_serial {
            "tool_batch_execution_parallel_async".to_string()
        } else {
            "tool_batch_execution_mixed_async".to_string()
        };
        while let Some((index, request, concurrency_safe)) = pending.pop_front() {
            if concurrency_safe {
                let mut safe_parallel = VecDeque::new();
                safe_parallel.push_back((index, request));
                while pending.front().is_some_and(|(_, _, safe)| *safe) {
                    let (index, request, _) = pending.pop_front().expect("safe request");
                    safe_parallel.push_back((index, request));
                }
                while !safe_parallel.is_empty() {
                    let mut handles =
                        VecDeque::with_capacity(self.tool_parallelism.min(safe_parallel.len()));
                    for _ in 0..self.tool_parallelism {
                        let Some((index, request)) = safe_parallel.pop_front() else {
                            break;
                        };
                        let tool_call_id = request.tool_call_id.clone();
                        let tool_name = request.tool_name.clone();
                        let permit = ToolExecutionPermit::parallel(
                            self.tool_concurrency.acquire_async().await,
                        );
                        emit_tool_executor_event(
                            &mut sink,
                            ToolBatchExecutorEvent::Started {
                                index,
                                tool_call_id: tool_call_id.clone(),
                                tool_name: tool_name.clone(),
                                parallel_group: "safe_parallel".to_string(),
                            },
                        );
                        let execute_tool_async = Arc::clone(&self.execute_tool_async);
                        let handle = tokio::spawn(async move {
                            let _permit = permit;
                            let mut report = execute_tool_async(request).await;
                            if report.parallel_group.is_none() {
                                report.parallel_group = Some("safe_parallel".to_string());
                            }
                            if report.transition_reason.is_none() {
                                report.transition_reason = Some("parallel_exec_async".to_string());
                            }
                            report
                        });
                        handles.push_back((index, tool_call_id, tool_name, handle));
                    }
                    while let Some((index, tool_call_id, tool_name, handle)) = handles.pop_front() {
                        let report = match handle.await {
                            Ok(report) => report,
                            Err(error) => {
                                let now = current_time_ms();
                                recovery_policy_trace_json.push(                                json!({                                    "policy": "streaming_parallel_async_task_failed",                                    "priority": 60,                                    "stage": "tools",                                    "action": "record_error_result",                                    "meta": {                                        "error": error.to_string(),                                        "taskTree": "tokio",                                    },                                })                                .to_string(),                            );
                                ToolExecutionResult {
                                    tool_call_id: tool_call_id.clone(),
                                    tool_name: tool_name.clone(),
                                    status: "error".to_string(),
                                    content: "Tool execution failed because its runtime task terminated unexpectedly."
                                        .to_string(),
                                    details: json!({ "joinError": error.to_string() }),
                                    facts: Vec::new(),
                                    error: Some(ToolErrorInfo::new(
                                        crate::tool::ToolFailureKind::HostUnavailable,
                                        "Tool execution failed because its runtime task terminated unexpectedly",
                                        "Tool execution failed",
                                    )),
                                    started_at_ms: now,
                                    completed_at_ms: now,
                                    latency_ms: 0,
                                    parallel_group: Some("safe_parallel".to_string()),
                                    transition_reason: Some(
                                        "streaming_parallel_async_task_failed".to_string(),
                                    ),
                                }
                            }
                        };
                        let report = match apply_result_sink(&mut result_sink, index, report) {
                            Ok(report) => report,
                            Err(error) => {
                                for (_, _, _, handle) in &handles {
                                    handle.abort();
                                }
                                while let Some((_, _, _, handle)) = handles.pop_front() {
                                    let _ = handle.await;
                                }
                                return Err(error);
                            }
                        };
                        emit_tool_executor_event(
                            &mut sink,
                            ToolBatchExecutorEvent::Finished {
                                index,
                                tool_call_id: report.tool_call_id.clone(),
                                tool_name: report.tool_name.clone(),
                                status: report.status.clone(),
                                parallel_group: "safe_parallel".to_string(),
                            },
                        );
                        reports.push((index, report));
                    }
                }
                continue;
            }

            let _permit =
                ToolExecutionPermit::serial(self.tool_concurrency.acquire_serial_async().await);
            emit_tool_executor_event(
                &mut sink,
                ToolBatchExecutorEvent::Started {
                    index,
                    tool_call_id: request.tool_call_id.clone(),
                    tool_name: request.tool_name.clone(),
                    parallel_group: "serial".to_string(),
                },
            );
            let mut report = (self.execute_tool_async)(request).await;
            if report.parallel_group.is_none() {
                report.parallel_group = Some("serial".to_string());
            }
            if report.transition_reason.is_none() {
                report.transition_reason = Some("serial_local_exec_async".to_string());
            }
            let report = apply_result_sink(&mut result_sink, index, report)?;
            emit_tool_executor_event(
                &mut sink,
                ToolBatchExecutorEvent::Finished {
                    index,
                    tool_call_id: report.tool_call_id.clone(),
                    tool_name: report.tool_name.clone(),
                    status: report.status.clone(),
                    parallel_group: "serial".to_string(),
                },
            );
            reports.push((index, report));
        }
        reports.sort_by_key(|(index, _)| *index);
        Ok(ToolBatchExecutorResult {
            reports,
            recovery_policy_trace_json,
            transition_reason,
        })
    }
}

fn apply_result_sink<TSink>(
    sink: &mut Option<&mut TSink>,
    index: usize,
    result: ToolExecutionResult,
) -> Result<ToolExecutionResult, String>
where
    TSink: FnMut(usize, ToolExecutionResult) -> Result<ToolExecutionResult, String> + ?Sized,
{
    match sink.as_deref_mut() {
        Some(callback) => callback(index, result),
        None => Ok(result),
    }
}

fn emit_tool_executor_event<TSink>(sink: &mut Option<&mut TSink>, event: ToolBatchExecutorEvent)
where
    TSink: FnMut(ToolBatchExecutorEvent) + ?Sized,
{
    if let Some(callback) = sink.as_deref_mut() {
        callback(event);
    }
}
fn current_time_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}
#[cfg(test)]
fn panic_async_tool_result() -> ToolExecutionResult {
    panic!("async safe parallel task panic for test")
}
#[cfg(test)]
mod tests {
    use super::{ToolBatchExecutor, ToolBatchExecutorEvent};
    use crate::runtime::{
        normalize_tool_parallelism, DEFAULT_TOOL_PARALLELISM, MAX_TOOL_PARALLELISM,
    };
    use crate::tool::concurrency::ToolConcurrencyCoordinator;
    use crate::tool::layer::ToolExecutionResult;
    use crate::tool::layer::{ToolInvocationRequest, ToolLayer};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    fn read_request(index: usize) -> (usize, ToolInvocationRequest) {
        (
            index,
            ToolInvocationRequest {
                tool_call_id: format!("call-{index}"),
                tool_name: "read".to_string(),
                args_json: "{}".to_string(),
            },
        )
    }
    fn bash_request(index: usize) -> (usize, ToolInvocationRequest) {
        (
            index,
            ToolInvocationRequest {
                tool_call_id: format!("bash-call-{index}"),
                tool_name: "bash".to_string(),
                args_json: "{\"command\":\"echo serial\"}".to_string(),
            },
        )
    }
    fn publish_request(index: usize) -> (usize, ToolInvocationRequest) {
        (
            index,
            ToolInvocationRequest {
                tool_call_id: format!("publish-call-{index}"),
                tool_name: "external_publish".to_string(),
                args_json: "{\"path\":\"/mnt/data/report.pdf\"}".to_string(),
            },
        )
    }
    fn ok_report(request: ToolInvocationRequest) -> ToolExecutionResult {
        ToolExecutionResult {
            tool_call_id: request.tool_call_id,
            tool_name: request.tool_name,
            status: "ok".to_string(),
            content: "ok".to_string(),
            details: serde_json::json!({}),
            facts: Vec::new(),
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: None,
        }
    }
    fn record_active_start(active: &AtomicUsize, max_active: &AtomicUsize) {
        let current = active.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        let mut observed = max_active.load(Ordering::SeqCst);
        while current > observed {
            match max_active.compare_exchange(observed, current, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(next) => observed = next,
            }
        }
    }
    #[test]
    fn tool_parallelism_normalization_uses_default_and_hard_cap() {
        assert_eq!(DEFAULT_TOOL_PARALLELISM, 16);
        assert_eq!(MAX_TOOL_PARALLELISM, 64);
        assert_eq!(normalize_tool_parallelism(0), 1);
        assert_eq!(normalize_tool_parallelism(32), 32);
        assert_eq!(normalize_tool_parallelism(128), MAX_TOOL_PARALLELISM);
    }
    #[tokio::test]
    async fn async_safe_parallel_execution_respects_configured_parallelism_limit() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let execute_tool_async = {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            Arc::new(move |request: ToolInvocationRequest| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                Box::pin(async move {
                    record_active_start(&active, &max_active);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let executor = ToolBatchExecutor::new_with_executors_and_parallelism(
            ToolLayer::new(),
            2,
            execute_tool_async,
        );
        let result = executor
            .execute_local_tools_async((0..5).map(read_request).collect(), None)
            .await;
        assert_eq!(result.reports.len(), 5);
        assert_eq!(
            result.transition_reason,
            "tool_batch_execution_parallel_async"
        );
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn shared_coordinator_limits_async_safe_parallel_execution_across_executors() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let execute_tool_async = {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            Arc::new(move |request: ToolInvocationRequest| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                Box::pin(async move {
                    record_active_start(&active, &max_active);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let coordinator = ToolConcurrencyCoordinator::new(3);
        let executor_a = ToolBatchExecutor::new_with_executors_and_coordinator(
            ToolLayer::new(),
            coordinator.clone(),
            execute_tool_async.clone(),
        );
        let executor_b = ToolBatchExecutor::new_with_executors_and_coordinator(
            ToolLayer::new(),
            coordinator,
            execute_tool_async,
        );
        let (result_a, result_b) = tokio::join!(
            executor_a.execute_local_tools_async((0..5).map(read_request).collect(), None),
            executor_b.execute_local_tools_async((5..10).map(read_request).collect(), None),
        );
        assert_eq!(result_a.reports.len(), 5);
        assert_eq!(result_b.reports.len(), 5);
        assert_eq!(max_active.load(Ordering::SeqCst), 3);
    }
    #[tokio::test]
    async fn shared_coordinator_serializes_async_serial_execution_across_executors() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let execute_tool_async = {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            Arc::new(move |request: ToolInvocationRequest| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                Box::pin(async move {
                    record_active_start(&active, &max_active);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let coordinator = ToolConcurrencyCoordinator::new(2);
        let executor_a = ToolBatchExecutor::new_with_executors_and_coordinator(
            ToolLayer::new(),
            coordinator.clone(),
            execute_tool_async.clone(),
        );
        let executor_b = ToolBatchExecutor::new_with_executors_and_coordinator(
            ToolLayer::new(),
            coordinator,
            execute_tool_async,
        );
        let (result_a, result_b) = tokio::join!(
            executor_a.execute_local_tools_async(vec![bash_request(0)], None),
            executor_b.execute_local_tools_async(vec![bash_request(1)], None),
        );
        assert_eq!(result_a.reports.len(), 1);
        assert_eq!(result_b.reports.len(), 1);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn durable_result_safe_point_precedes_the_next_serial_tool() {
        let executions = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let execute_tool_async = {
            let executions = Arc::clone(&executions);
            Arc::new(move |request: ToolInvocationRequest| {
                let executions = Arc::clone(&executions);
                Box::pin(async move {
                    executions
                        .lock()
                        .expect("record executions")
                        .push(request.tool_name.clone());
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let executor = ToolBatchExecutor::new_with_executors(ToolLayer::new(), execute_tool_async);
        let mut result_sink = |_index, report: ToolExecutionResult| {
            assert_eq!(report.tool_name, "external_publish");
            Err("session fact commit failed".to_string())
        };

        let error = executor
            .execute_local_tools_with_result_sink_async(
                vec![publish_request(0), bash_request(1)],
                None,
                Some(&mut result_sink),
            )
            .await
            .expect_err("safe-point failure must stop the serial batch");

        assert_eq!(error, "session fact commit failed");
        assert_eq!(
            *executions.lock().expect("read executions"),
            vec!["external_publish"]
        );
    }

    #[tokio::test]
    async fn mixed_batch_does_not_move_safe_tools_across_serial_tools() {
        let executions = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let execute_tool_async = {
            let executions = Arc::clone(&executions);
            Arc::new(move |request: ToolInvocationRequest| {
                let executions = Arc::clone(&executions);
                Box::pin(async move {
                    executions
                        .lock()
                        .expect("record executions")
                        .push(request.tool_call_id.clone());
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let executor = ToolBatchExecutor::new_with_executors_and_parallelism(
            ToolLayer::new(),
            3,
            execute_tool_async,
        );

        let result = executor
            .execute_local_tools_async(
                vec![read_request(0), bash_request(1), read_request(2)],
                None,
            )
            .await;

        assert_eq!(
            *executions.lock().expect("read executions"),
            vec!["call-0", "bash-call-1", "call-2"]
        );
        assert_eq!(result.transition_reason, "tool_batch_execution_mixed_async");
    }

    #[tokio::test]
    async fn result_sink_failure_aborts_and_reaps_remaining_parallel_tasks() {
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let execute_tool_async = {
            let started = Arc::clone(&started);
            let completed = Arc::clone(&completed);
            Arc::new(move |request: ToolInvocationRequest| {
                let started = Arc::clone(&started);
                let completed = Arc::clone(&completed);
                Box::pin(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    if request.tool_call_id != "call-0" {
                        return std::future::pending::<ToolExecutionResult>().await;
                    }
                    while started.load(Ordering::SeqCst) < 3 {
                        tokio::task::yield_now().await;
                    }
                    completed.fetch_add(1, Ordering::SeqCst);
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let coordinator = ToolConcurrencyCoordinator::new(3);
        let executor = ToolBatchExecutor::new_with_executors_and_coordinator(
            ToolLayer::new(),
            coordinator.clone(),
            execute_tool_async,
        );
        let mut result_sink = |_index, _report| Err("session fact commit failed".to_string());

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            executor.execute_local_tools_with_result_sink_async(
                (0..3).map(read_request).collect(),
                None,
                Some(&mut result_sink),
            ),
        )
        .await
        .expect("failed batch must reap tasks promptly")
        .expect_err("result sink failure must fail the batch");

        assert_eq!(error, "session fact commit failed");
        assert_eq!(started.load(Ordering::SeqCst), 3);
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_millis(100), async {
            let _first = coordinator.acquire_async().await;
            let _second = coordinator.acquire_async().await;
            let _third = coordinator.acquire_async().await;
        })
        .await
        .expect("aborted tasks must release all concurrency permits");
    }

    #[tokio::test]
    async fn complete_four_call_batch_executes_each_call_once_and_pairs_results_in_order() {
        let executions = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let execute_tool_async = {
            let executions = Arc::clone(&executions);
            Arc::new(move |request: ToolInvocationRequest| {
                let executions = Arc::clone(&executions);
                Box::pin(async move {
                    executions
                        .lock()
                        .expect("record executions")
                        .push(request.tool_call_id.clone());
                    ok_report(request)
                }) as super::AsyncToolExecutionFuture
            })
        };
        let executor = ToolBatchExecutor::new_with_executors_and_parallelism(
            ToolLayer::new(),
            4,
            execute_tool_async,
        );

        let result = executor
            .execute_local_tools_async((0..4).map(read_request).collect(), None)
            .await;

        let mut executed = executions.lock().expect("read executions").clone();
        executed.sort();
        assert_eq!(executed, vec!["call-0", "call-1", "call-2", "call-3"]);
        assert_eq!(
            result
                .reports
                .iter()
                .map(|(_, report)| report.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["call-0", "call-1", "call-2", "call-3"]
        );
    }

    #[tokio::test]
    async fn async_safe_parallel_join_failure_records_error_report() {
        let execute_tool_async = Arc::new(move |_request: ToolInvocationRequest| {
            Box::pin(async move { super::panic_async_tool_result() })
                as super::AsyncToolExecutionFuture
        });
        let executor = ToolBatchExecutor::new_with_executors(ToolLayer::new(), execute_tool_async);
        let mut events = vec![];
        let result = executor
            .execute_local_tools_async(
                vec![(
                    0,
                    ToolInvocationRequest {
                        tool_call_id: "call-panic".to_string(),
                        tool_name: "read".to_string(),
                        args_json: "{\"path\":\"Cargo.toml\",\"offset\":0,\"limit\":1}".to_string(),
                    },
                )],
                Some(&mut |event| events.push(event)),
            )
            .await;
        assert_eq!(result.reports.len(), 1);
        assert_eq!(result.reports[0].0, 0);
        let report = &result.reports[0].1;
        assert_eq!(report.tool_call_id, "call-panic");
        assert_eq!(report.status, "error");
        assert_eq!(
            report.transition_reason.as_deref(),
            Some("streaming_parallel_async_task_failed")
        );
        assert!(result
            .recovery_policy_trace_json
            .iter()
            .any(|item| item.contains("streaming_parallel_async_task_failed")));
        assert!(events.iter().any(|event| matches!(            event,            ToolBatchExecutorEvent::Finished {                tool_call_id,                status,                parallel_group,                ..            } if tool_call_id == "call-panic"                && status == "error"                && parallel_group == "safe_parallel"        )));
    }
}
