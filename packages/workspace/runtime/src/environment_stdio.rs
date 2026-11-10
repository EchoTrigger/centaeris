//! Streaming I/O over the fenced environment task protocol.
use centaeris_core::execution::*;
use std::{collections::BTreeMap, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

pub(crate) type TaskRpc =
    Arc<dyn Fn(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct EnvironmentStdioRequest {
    pub fence: EnvironmentTaskFence,
    pub operation_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub rpc: TaskRpc,
}
impl std::fmt::Debug for EnvironmentStdioRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentStdioRequest")
            .field("operation_id", &self.operation_id)
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}
impl EnvironmentStdioRequest {
    pub(crate) async fn spawn_async(&self) -> Result<crate::docker_engine::ExecChild, String> {
        let scope = match &self.fence.controller_owner {
            EnvironmentTaskOwner::AgentRun { run_id } => EnvironmentTaskScope::Run {
                run_id: run_id.clone(),
            },
            EnvironmentTaskOwner::MaintenanceOperation { operation_id } => {
                EnvironmentTaskScope::Operation {
                    operation_id: operation_id.clone(),
                }
            }
        };
        let start = EnvironmentTaskRequest::Start {
            fence: self.fence.clone(),
            operation_id: self.operation_id.clone(),
            scope: scope.clone(),
            program: self.program.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
            timeout_ms: 0,
        };
        start.validate_authority()?;
        let result = call(self.rpc.clone(), start).await?;
        let EnvironmentTaskResult::Task { task, .. } = result else {
            return Err("environment stdio start unconfirmed".into());
        };
        if !self
            .fence
            .matches_start_receipt(&task, &self.operation_id, &scope)
            || task.state == EnvironmentTaskState::Unknown
        {
            return Err("environment stdio start unconfirmed".into());
        }
        let request = self.clone();
        let id = task.task_id;
        Ok(crate::docker_engine::ExecChild::from_streams(
            &tokio::runtime::Handle::current(),
            id.clone(),
            move |stdin, stdout, stderr| async move {
                let input = request.send_input(&id, &scope, stdin);
                let output = request.receive_output(&id, &scope, stdout, stderr);
                tokio::pin!(input, output);
                tokio::select! {result=&mut output=>result,result=&mut input=>{result?;output.await}}
            },
        ))
    }
    fn matches(&self, task: &EnvironmentTaskSnapshot, scope: &EnvironmentTaskScope) -> bool {
        self.fence
            .matches_start_receipt(task, &self.operation_id, scope)
    }
    async fn send_input(
        &self,
        id: &str,
        scope: &EnvironmentTaskScope,
        mut stdin: DuplexStream,
    ) -> Result<(), String> {
        let mut buffer = [0u8; 4096];
        let mut sequence = 0u64;
        loop {
            let count = stdin
                .read(&mut buffer)
                .await
                .map_err(|_| "environment stdio input unavailable")?;
            let data = buffer[..count].to_vec();
            let close = count == 0;
            let operation_id = self
                .fence
                .operation_id(&format!("stdio-input:{id}:{sequence}"));
            let request_digest = environment_task_input_digest(&data, close)?;
            let mutation = EnvironmentTaskRequest::Input {
                fence: self.fence.clone(),
                task_id: id.into(),
                operation_id: operation_id.clone(),
                data,
                close,
            };
            let result = match call(self.rpc.clone(), mutation).await {
                Ok(result) => result,
                Err(error) => call(
                    self.rpc.clone(),
                    EnvironmentTaskRequest::QueryInputReceipt {
                        fence: self.fence.clone(),
                        task_id: id.into(),
                        operation_id,
                        request_digest,
                    },
                )
                .await
                .map_err(|_| error)?,
            };
            if !matches!(result,EnvironmentTaskResult::Mutated{ref task} if self.matches(task,scope))
            {
                return Err("environment stdio input receipt unconfirmed".into());
            }
            if close {
                return Ok(());
            }
            sequence = sequence
                .checked_add(1)
                .ok_or("environment stdio input identity exhausted")?;
        }
    }
    async fn receive_output(
        &self,
        id: &str,
        scope: &EnvironmentTaskScope,
        mut stdout: DuplexStream,
        mut stderr: DuplexStream,
    ) -> Result<crate::docker_engine::ExecStatus, String> {
        let mut cursor = 0u64;
        loop {
            let result = call(
                self.rpc.clone(),
                EnvironmentTaskRequest::Query {
                    fence: self.fence.clone(),
                    task_id: id.into(),
                    cursor,
                    max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
                },
            )
            .await?;
            let EnvironmentTaskResult::Task {
                task,
                output,
                earliest_cursor,
                next_cursor,
                gap,
                has_more,
            } = result
            else {
                return Err("environment stdio query unconfirmed".into());
            };
            if !self.matches(&task, scope) {
                return Err("environment stdio task identity mismatch".into());
            }
            if gap || earliest_cursor > cursor {
                return Err("environment stdio output unavailable".into());
            }
            let mut next = cursor;
            let mut total = 0usize;
            for chunk in output {
                total = total
                    .checked_add(chunk.data.len())
                    .ok_or("environment stdio output invalid")?;
                if chunk.cursor != next || total > ENVIRONMENT_TASK_READ_BYTES {
                    return Err("environment stdio output invalid".into());
                }
                let sink = if chunk.stream == EnvironmentTaskStream::Stdout {
                    &mut stdout
                } else {
                    &mut stderr
                };
                sink.write_all(&chunk.data).await.map_err(|_| {
                    "environment stdio transport disconnected; remote outcome unknown"
                })?;
                next = next
                    .checked_add(1)
                    .ok_or("environment stdio output invalid")?;
            }
            if next != next_cursor || (has_more && next == cursor) {
                return Err("environment stdio output invalid".into());
            }
            cursor = next;
            if task.state == EnvironmentTaskState::Unknown {
                return Err("environment stdio process outcome unknown".into());
            }
            if task.state == EnvironmentTaskState::Exited && task.output_complete && !has_more {
                return task
                    .exit_code
                    .map(crate::docker_engine::ExecStatus::from)
                    .ok_or_else(|| "environment stdio exit status unavailable".into());
            }
            if !has_more {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

async fn call(
    rpc: TaskRpc,
    request: EnvironmentTaskRequest,
) -> Result<EnvironmentTaskResult, String> {
    tokio::task::spawn_blocking(move || rpc(request))
        .await
        .map_err(|_| "environment stdio RPC worker failed".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct Facts {
        requests: Vec<EnvironmentTaskRequest>,
        input: Vec<u8>,
        closed: bool,
    }
    fn fixture(gap: bool, revoked: bool) -> (EnvironmentStdioRequest, Arc<Mutex<Facts>>) {
        let facts = Arc::new(Mutex::new(Facts::default()));
        let fence = EnvironmentTaskFence {
            environment_instance_id: format!("instance_{}", "a".repeat(64)),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run".into(),
            },
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        let task_id = fence.task_id("1:mcp").unwrap();
        let state = facts.clone();
        let rpc = Arc::new(move |request: EnvironmentTaskRequest| {
            let mut facts = state.lock().unwrap();
            facts.requests.push(request.clone());
            let mut task = EnvironmentTaskSnapshot {
                task_id: task_id.clone(),
                operation_id: "1:mcp".into(),
                scope: EnvironmentTaskScope::Run {
                    run_id: "run".into(),
                },
                state: EnvironmentTaskState::Running,
                exit_code: None,
                output_complete: false,
                cleanup_complete: false,
            };
            match request {
                EnvironmentTaskRequest::Start { .. } => Ok(EnvironmentTaskResult::Task {
                    task,
                    output: vec![],
                    earliest_cursor: 0,
                    next_cursor: 0,
                    gap: false,
                    has_more: false,
                }),
                EnvironmentTaskRequest::Input { data, close, .. } => {
                    facts.input.extend(data);
                    facts.closed |= close;
                    Ok(EnvironmentTaskResult::Mutated { task })
                }
                EnvironmentTaskRequest::QueryInputReceipt { .. } => {
                    Ok(EnvironmentTaskResult::Mutated { task })
                }
                EnvironmentTaskRequest::Query { cursor, .. } => {
                    if revoked {
                        return Err("controller authority revoked".into());
                    }
                    let output = if facts.closed && cursor == 0 {
                        vec![
                            EnvironmentTaskOutputChunk {
                                cursor: 0,
                                stream: EnvironmentTaskStream::Stdout,
                                data: facts.input.clone(),
                            },
                            EnvironmentTaskOutputChunk {
                                cursor: 1,
                                stream: EnvironmentTaskStream::Stderr,
                                data: b"diagnostic".to_vec(),
                            },
                        ]
                    } else {
                        vec![]
                    };
                    if facts.closed {
                        task.state = EnvironmentTaskState::Exited;
                        task.exit_code = Some(7);
                        task.output_complete = true;
                    }
                    let next_cursor = if output.is_empty() { cursor } else { 2 };
                    Ok(EnvironmentTaskResult::Task {
                        task,
                        output,
                        earliest_cursor: 0,
                        next_cursor,
                        gap,
                        has_more: false,
                    })
                }
                _ => Err("unexpected lifecycle operation".into()),
            }
        });
        (
            EnvironmentStdioRequest {
                fence,
                operation_id: "1:mcp".into(),
                program: "/plugins/server".into(),
                args: vec!["literal ; argument".into()],
                cwd: "/project".into(),
                env: [("MCP_VALUE".into(), "native environment".into())].into(),
                rpc,
            },
            facts,
        )
    }

    #[tokio::test]
    async fn stdio_uses_one_owned_start_and_preserves_binary_streams_native_env_and_exit() {
        let (request, facts) = fixture(false, false);
        let mut child = request.spawn_async().await.unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let payload = vec![255u8; 12001];
        stdin.write_all(&payload).await.unwrap();
        stdin.shutdown().await.unwrap();
        drop(stdin);
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut out = vec![];
        let mut err = vec![];
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err))
        })
        .await
        .unwrap();
        result.0.unwrap();
        result.1.unwrap();
        assert_eq!(out, payload);
        assert_eq!(err, b"diagnostic");
        assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(7));
        let facts = facts.lock().unwrap();
        assert_eq!(
            facts
                .requests
                .iter()
                .filter(|r| matches!(r, EnvironmentTaskRequest::Start { .. }))
                .count(),
            1
        );
        let EnvironmentTaskRequest::Start {
            program,
            args,
            cwd,
            env,
            scope,
            ..
        } = &facts.requests[0]
        else {
            panic!("start expected")
        };
        assert_eq!(program, "/plugins/server");
        assert_eq!(args, &request.args);
        assert_eq!(cwd, "/project");
        assert_eq!(env, &request.env);
        assert_eq!(
            scope,
            &EnvironmentTaskScope::Run {
                run_id: "run".into()
            }
        );
        assert!(facts.requests.iter().all(|r| !matches!(
            r,
            EnvironmentTaskRequest::Takeover { .. }
                | EnvironmentTaskRequest::EndRun { .. }
                | EnvironmentTaskRequest::EndOperation { .. }
                | EnvironmentTaskRequest::Terminate { .. }
        )));
    }

    #[tokio::test]
    async fn stdio_output_gap_or_revoked_authority_is_an_error_not_successful_eof() {
        for (gap, revoked) in [(true, false), (false, true)] {
            let (request, _) = fixture(gap, revoked);
            let mut child = request.spawn_async().await.unwrap();
            let mut stdout = child.stdout.take().unwrap();
            assert!(tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stdout.read_to_end(&mut vec![])
            )
            .await
            .unwrap()
            .is_err());
            assert!(child.try_wait().is_err());
        }
    }

    #[tokio::test]
    async fn local_stdio_disconnect_does_not_send_a_remote_stop_or_replay_start() {
        let (request, facts) = fixture(false, false);
        let mut child = request.spawn_async().await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        child.disconnect();
        assert!(stdout.read_to_end(&mut vec![]).await.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let facts = facts.lock().unwrap();
        assert_eq!(
            facts
                .requests
                .iter()
                .filter(|r| matches!(r, EnvironmentTaskRequest::Start { .. }))
                .count(),
            1
        );
        assert!(facts
            .requests
            .iter()
            .all(|r| !matches!(r, EnvironmentTaskRequest::Terminate { .. })));
    }
    #[tokio::test]
    async fn stdio_lost_input_acknowledgement_queries_receipt_without_writing_again() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (mut request, facts) = fixture(false, false);
        let original = request.rpc.clone();
        let lost = Arc::new(AtomicBool::new(false));
        request.rpc = Arc::new(move |request| {
            let lose = matches!(&request,EnvironmentTaskRequest::Input{data,..} if !data.is_empty())
                && !lost.swap(true, Ordering::SeqCst);
            let result = original(request);
            if lose {
                Err("lost input acknowledgement".into())
            } else {
                result
            }
        });
        let mut child = request.spawn_async().await.unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"delivered once").await.unwrap();
        stdin.shutdown().await.unwrap();
        drop(stdin);
        let mut stdout = child.stdout.take().unwrap();
        let mut out = vec![];
        stdout.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"delivered once");
        let facts = facts.lock().unwrap();
        assert_eq!(facts.input, b"delivered once");
        assert!(facts
            .requests
            .iter()
            .any(|r| matches!(r, EnvironmentTaskRequest::QueryInputReceipt { .. })));
    }
}
