//! Model-facing access to retained Environment tasks; authority stays in the port.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskRequest,
    EnvironmentTaskResult, EnvironmentTaskScope, EnvironmentTaskSnapshot,
    ENVIRONMENT_TASK_READ_BYTES,
};
use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
};
use centaeris_core::tool::{DynamicToolContract, ToolTurnBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) const RETAINED_TASK_PROVIDER_ID: &str = "centaeris.background_task";

/// Every RPC must verify the current controller authority, including a stop after its query.
pub(crate) trait EnvironmentTaskPort: Send + Sync {
    fn environment_task_fence(&self) -> Result<EnvironmentTaskFence, String>;
    fn environment_task_rpc(
        &self,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String>;
}

impl EnvironmentTaskPort for crate::docker_execution_host::DockerExecutionHostRunner {
    fn environment_task_fence(&self) -> Result<EnvironmentTaskFence, String> {
        Self::environment_task_fence(self)
    }

    fn environment_task_rpc(
        &self,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        Self::environment_task_rpc(self, request)
    }
}

pub(crate) fn retained_task_contracts() -> Vec<DynamicToolContract> {
    [
        ("background_task_start", "Start a managed task that can continue after this Run ends. Run the long-lived program directly using this tool instead of Bash '&'; do not daemonize and exit its parent, because parent exit cleans up descendants. A lost reply means unknown; check existing tasks before starting another.", json!({
            "program":{"type":"string","minLength":1,"maxLength":4096},
            "args":{"type":"array","items":{"type":"string"},"maxItems":1024},
            "cwd":{"type":"string","description":"Absolute working directory under /mnt/data or /home/agent; defaults to /mnt/data."},
            "timeout_ms":{"type":"integer","minimum":0,"maximum":86400000,"description":"Zero disables the task timeout."}
        }), vec!["program"]),
        ("background_task_list", "List background tasks belonging to this environment.", json!({}), vec![]),
        ("background_task_query", "Read task status and a bounded page of output. Preserve the returned cursor; a gap means older output is unavailable.", json!({
            "task_id":{"type":"string","minLength":1,"maxLength":128},
            "cursor":{"type":"integer","minimum":0}
        }), vec!["task_id"]),
        ("background_task_stop", "Request termination of one environment task. Inspect cleanupComplete before assuming its processes have stopped.", json!({
            "task_id":{"type":"string","minLength":1,"maxLength":128}
        }), vec!["task_id"]),
    ].into_iter().map(|(name, summary, properties, required)| DynamicToolContract {
        name: name.into(), category: "runtime.execution".into(), summary: summary.into(),
        input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        provider_id: RETAINED_TASK_PROVIDER_ID.into(), scopes: vec![], concurrency_safe: false,
        turn_behavior: ToolTurnBehavior::ContinueTurn,
    }).collect()
}

pub(crate) struct RetainedTaskToolProvider {
    port: Arc<dyn EnvironmentTaskPort>,
}

impl RetainedTaskToolProvider {
    pub(crate) fn new(port: Arc<dyn EnvironmentTaskPort>) -> Self {
        Self { port }
    }
}

impl DynamicToolProvider for RetainedTaskToolProvider {
    fn provider_id(&self) -> &str {
        RETAINED_TASK_PROVIDER_ID
    }
    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        let port = self.port.clone();
        Box::pin(async move {
            // The port can perform Docker/HTTP I/O. A lost worker reply is never permission to retry.
            Ok(
                tokio::task::spawn_blocking(move || execute_task(request, port.as_ref()))
                    .await
                    .unwrap_or_else(|_| failure("background_task_unknown")),
            )
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: String,
    #[serde(default)]
    timeout_ms: u64,
}
fn default_cwd() -> String {
    "/mnt/data".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    task_id: String,
    #[serde(default)]
    cursor: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopArgs {
    task_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {}
enum Args {
    Start(StartArgs),
    List,
    Query(QueryArgs),
    Stop(StopArgs),
}

fn parse(request: &DynamicToolProviderRequest) -> Result<Args, ()> {
    if request.contract.name != request.tool_name
        || request.contract.provider_id.as_deref() != Some(RETAINED_TASK_PROVIDER_ID)
        || !request.contract.dynamic
        || request
            .contract
            .schema_hash
            .as_deref()
            .is_none_or(str::is_empty)
        || request.tool_call_id.is_empty()
        || request.tool_call_id.len() > 1024
        || request.tool_call_id.chars().any(char::is_control)
        || request.args_json.len() > 128 * 1024
    {
        return Err(());
    }
    match request.tool_name.as_str() {
        "background_task_start" => {
            let a: StartArgs = serde_json::from_str(&request.args_json).map_err(|_| ())?;
            let path = &a.cwd;
            if a.program.is_empty()
                || a.program.len() > 4096
                || a.program.contains('\0')
                || a.args.len() > 1024
                || a.args.iter().any(|s| s.contains('\0'))
                || a.args.iter().map(String::len).sum::<usize>() > 65536
                || a.timeout_ms > 86400000
                || path.len() > 4096
                || path.contains('\\')
                || path.chars().any(char::is_control)
                || !(path == "/mnt/data"
                    || path.starts_with("/mnt/data/")
                    || path == "/home/agent"
                    || path.starts_with("/home/agent/"))
                || path
                    .split('/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(());
            }
            Ok(Args::Start(a))
        }
        "background_task_list" => {
            serde_json::from_str::<ListArgs>(&request.args_json).map_err(|_| ())?;
            Ok(Args::List)
        }
        "background_task_query" => {
            let a: QueryArgs = serde_json::from_str(&request.args_json).map_err(|_| ())?;
            valid_task_id(&a.task_id)?;
            Ok(Args::Query(a))
        }
        "background_task_stop" => {
            let a: StopArgs = serde_json::from_str(&request.args_json).map_err(|_| ())?;
            valid_task_id(&a.task_id)?;
            Ok(Args::Stop(a))
        }
        _ => Err(()),
    }
}
fn valid_task_id(id: &str) -> Result<(), ()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
    {
        Err(())
    } else {
        Ok(())
    }
}
fn operation_id(request: &DynamicToolProviderRequest, fence: &EnvironmentTaskFence) -> String {
    // JSON framing plus a domain prevents ambiguous concatenation and cross-Run reuse.
    let mut hash = Sha256::new();
    hash.update(b"centaeris:background-task-operation:v1\0");
    hash.update(
        serde_json::to_vec(&(
            &fence.controller_id,
            &request.tool_call_id,
            &request.tool_name,
        ))
        .expect("string tuple"),
    );
    format!("tool-{:x}", hash.finalize())
}
fn execute_task(
    request: DynamicToolProviderRequest,
    port: &dyn EnvironmentTaskPort,
) -> DynamicToolProviderResponse {
    let args = match parse(&request) {
        Ok(a) => a,
        Err(()) => return failure("background_task_invalid"),
    };
    let fence = match port.environment_task_fence() {
        Ok(f) => f,
        Err(error) => return host_failure(&error, false),
    };
    if fence.controller_purpose != EnvironmentTaskControllerPurpose::Execute {
        return failure("background_task_forbidden");
    }
    let mutation = matches!(args, Args::Start(_) | Args::Stop(_));
    let expected_id = match &args {
        Args::Query(a) => Some(a.task_id.clone()),
        Args::Stop(a) => Some(a.task_id.clone()),
        _ => None,
    };
    let rpc = match args {
        Args::Start(a) => EnvironmentTaskRequest::Start {
            operation_id: operation_id(&request, &fence),
            fence,
            scope: EnvironmentTaskScope::Environment,
            program: a.program,
            args: a.args,
            cwd: a.cwd,
            timeout_ms: a.timeout_ms,
        },
        Args::List => EnvironmentTaskRequest::List { fence },
        Args::Query(a) => EnvironmentTaskRequest::Query {
            fence,
            task_id: a.task_id,
            cursor: a.cursor,
            max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
        },
        Args::Stop(a) => {
            let observed = port.environment_task_rpc(EnvironmentTaskRequest::Query {
                fence: fence.clone(),
                task_id: a.task_id.clone(),
                cursor: 0,
                max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
            });
            match observed {
                Ok(EnvironmentTaskResult::Task { task, .. })
                    if task.task_id == a.task_id
                        && task.scope == EnvironmentTaskScope::Environment => {}
                Ok(_) => return failure("background_task_forbidden"),
                Err(error) => return host_failure(&error, false),
            }
            // This second RPC independently rechecks authority in the port; the query grants nothing.
            EnvironmentTaskRequest::Terminate {
                operation_id: operation_id(&request, &fence),
                fence,
                task_id: a.task_id,
            }
        }
    };
    match port.environment_task_rpc(rpc) {
        Ok(result) => project(result, &request.tool_name, expected_id.as_deref()),
        Err(error) => host_failure(&error, mutation),
    }
}
fn host_failure(error: &str, mutation: bool) -> DynamicToolProviderResponse {
    // Exact allow-list only: arbitrary adapter diagnostics may contain private host details.
    failure(match error {
        "task_controller_fenced"
        | "task_controller_unauthorized"
        | "task_cleanup_scope_denied"
        | "task_cleanup_execute_denied" => "background_task_forbidden",
        "task_not_found" => "background_task_not_found",
        "task_capacity" | "task_thread_capacity" | "task_receipt_capacity" => {
            "background_task_capacity"
        }
        "task_operation_conflict" => "background_task_conflict",
        "task_invalid_start" | "task_invalid_cwd" => "background_task_invalid",
        _ if mutation => "background_task_unknown",
        _ => "background_task_unavailable",
    })
}
fn failure(code: &str) -> DynamicToolProviderResponse {
    let message=match code {
        "background_task_invalid"=>"Invalid background task request.",
        "background_task_forbidden"=>"Current authority does not permit this background task operation.",
        "background_task_not_found"=>"Background task was not found.",
        "background_task_capacity"=>"Background task capacity is unavailable.",
        "background_task_conflict"=>"This operation identity already describes a different request.",
        "background_task_unknown"=>"The task operation outcome is unknown. Check existing task status before starting another task.",
        _=>"Background task service is unavailable.",
    };
    let mut details = json!({"schema":"background_task.result.v1","errorCode":code});
    if code == "background_task_unknown" {
        details["state"] = json!("unknown");
    }
    response(details, message, true)
}
fn summary(task: &EnvironmentTaskSnapshot) -> Value {
    json!({"taskId":task.task_id,"state":task.state,"exitCode":task.exit_code,"cleanupComplete":task.cleanup_complete,"outputComplete":task.output_complete})
}
fn project(
    result: EnvironmentTaskResult,
    name: &str,
    expected_id: Option<&str>,
) -> DynamicToolProviderResponse {
    let valid = |task: &EnvironmentTaskSnapshot| {
        task.scope == EnvironmentTaskScope::Environment
            && expected_id.is_none_or(|id| task.task_id == id)
    };
    let details = match result {
        EnvironmentTaskResult::Listed { tasks } if name == "background_task_list" => {
            json!({"tasks":tasks.iter().filter(|t|t.scope==EnvironmentTaskScope::Environment).map(summary).collect::<Vec<_>>()})
        }
        EnvironmentTaskResult::Task {
            task,
            output,
            earliest_cursor,
            next_cursor,
            gap,
            has_more,
        } if matches!(name, "background_task_start" | "background_task_query") && valid(&task) => {
            let mut details = summary(&task);
            details["output"]=json!(output.into_iter().map(|chunk|json!({"cursor":chunk.cursor,"stream":chunk.stream,
                    "text":String::from_utf8_lossy(&chunk.data),"byteLength":chunk.data.len(),"lossy":std::str::from_utf8(&chunk.data).is_err()})).collect::<Vec<_>>());
            details["earliestCursor"] = json!(earliest_cursor);
            details["nextCursor"] = json!(next_cursor);
            details["gap"] = json!(gap);
            details["hasMore"] = json!(has_more);
            details
        }
        EnvironmentTaskResult::Mutated { task }
            if name == "background_task_stop" && valid(&task) =>
        {
            summary(&task)
        }
        _ => {
            return failure(
                if matches!(name, "background_task_start" | "background_task_stop") {
                    "background_task_unknown"
                } else {
                    "background_task_forbidden"
                },
            )
        }
    };
    let mut response = response(details, "", false);
    // The model must receive the bounded public projection to operate on the
    // returned task identity and advance output cursors on subsequent calls.
    response.content = response.details.to_string();
    response
}
fn response(mut details: Value, content: &str, is_error: bool) -> DynamicToolProviderResponse {
    details["schema"] = json!("background_task.result.v1");
    DynamicToolProviderResponse {
        content: content.into(),
        details,
        is_error,
        facts: vec![],
        transition_reason: None,
    }
}

#[cfg(test)]
mod tests;
