//! Hosted HTTP transport. Domain validation stays in the protocol modules.
use super::*;
use axum::extract::State;

use axum::routing::{any, MethodFilter, MethodRouter};
use axum::Router;

mod agent_runs;
mod catalogs;

type Operation = fn(HttpServerState, Request<Bytes>) -> Result<Response<Body>, String>;

pub(super) fn router(state: HttpServerState) -> Router {
    Router::new()
        .route(
            "/agent-runs/step",
            on_blocking(MethodFilter::POST, agent_runs::step),
        )
        .route(
            "/agent-runs/cancel",
            on_blocking(MethodFilter::POST, agent_runs::cancel),
        )
        .route(
            "/internal/agent-runs/supplement",
            on_blocking(MethodFilter::POST, agent_runs::supplement),
        )
        .route(
            "/agent-runs/teardown",
            on_blocking(MethodFilter::POST, agent_runs::teardown),
        )
        .route(
            "/internal/model-catalog",
            on_blocking(MethodFilter::GET, catalogs::model_catalog),
        )
        .route(
            "/internal/execution-profile",
            on_blocking(MethodFilter::GET, catalogs::execution_profile),
        )
        .route(
            "/internal/host-event-input",
            on_blocking(MethodFilter::POST, catalogs::host_event_input),
        )
        .route(
            "/internal/plugins/inspect",
            on_blocking(MethodFilter::POST, catalogs::plugin_inspect),
        )
        .route(
            "/skills/catalog",
            on_blocking(MethodFilter::POST, catalogs::skills_catalog),
        )
        .route(
            "/skills/detail",
            on_blocking(MethodFilter::POST, catalogs::skill_detail),
        )
        .route(
            "/mcp/catalog",
            on_blocking(MethodFilter::POST, catalogs::mcp_catalog),
        )
        .route(
            "/hooks/catalog",
            on_blocking(MethodFilter::POST, catalogs::hooks_catalog),
        )
        .route("/internal/jobs/schedule", post_job(job_protocol::schedule))
        .route("/internal/jobs/claim", post_job(job_protocol::claim))
        .route("/internal/jobs/wait", post_job(job_protocol::wait))
        .route("/internal/jobs/start", post_job(job_protocol::start))
        .route(
            "/internal/jobs/heartbeat",
            post_job(job_protocol::heartbeat),
        )
        .route("/internal/jobs/yield", post_job(job_protocol::yield_job))
        .route("/internal/jobs/complete", post_job(job_protocol::complete))
        .route("/internal/jobs/fail", post_job(job_protocol::fail))
        .route(
            "/internal/jobs/reconcile",
            post_job(job_protocol::reconcile),
        )
        .route(
            "/internal/job-outbox/pending",
            post_job(job_protocol::pending_outbox),
        )
        .route(
            "/internal/job-outbox/published",
            post_job(job_protocol::publish_outbox),
        )
        .route(
            "/internal/job-outbox/wake-waiter",
            post_job(job_protocol::wake_waiter),
        )
        .route(
            "/internal/job-outbox/reconcile-waiters",
            post_job(job_protocol::reconcile_waiters),
        )
        .route(
            "/internal/jobs/{*job_id}",
            on_blocking(MethodFilter::GET, |state, request| {
                job_response(job_protocol::get_job(
                    target(&request),
                    state.job_store.as_ref(),
                ))
            }),
        )
        .route(
            "/internal/transcript/page",
            post_transcript(transcript_protocol::page),
        )
        .route(
            "/internal/transcript/patches",
            post_transcript(transcript_protocol::patches),
        )
        .route(
            "/internal/transcript/content",
            post_transcript(transcript_protocol::content),
        )
        .fallback(fallback)
        .with_state(state)
}

// One adapter for synchronous domain operations. axum owns path/method routing;
// any(fallback) retains the existing 404 response without synthesizing Allow.
fn on_blocking<F>(method: MethodFilter, operation: F) -> MethodRouter<HttpServerState>
where
    F: FnOnce(HttpServerState, Request<Bytes>) -> Result<Response<Body>, String>
        + Clone
        + Send
        + Sync
        + 'static,
{
    any(fallback).on(method, move |state, request| {
        dispatch(state, request, Some(operation.clone()))
    })
}

fn post_job(
    operation: fn(&[u8], &PostgresRuntimeStore) -> job_protocol::ProtocolResult,
) -> MethodRouter<HttpServerState> {
    on_blocking(MethodFilter::POST, move |state, request| {
        job_response(operation(request.body(), state.job_store.as_ref()))
    })
}

fn post_transcript(
    operation: impl Fn(&[u8], &PostgresRuntimeStore) -> Result<(u16, Vec<u8>), String>
        + Clone
        + Send
        + Sync
        + 'static,
) -> MethodRouter<HttpServerState> {
    on_blocking(MethodFilter::POST, move |state, request| {
        operation(request.body(), state.job_store.as_ref())
            .map(|(status, body)| http_response(status, "application/json", body))
    })
}

async fn fallback(state: State<HttpServerState>, request: Request<Body>) -> Response<Body> {
    dispatch(state, request, None::<Operation>).await
}

async fn dispatch<F>(
    state: State<HttpServerState>,
    request: Request<Body>,
    operation: Option<F>,
) -> Response<Body>
where
    F: FnOnce(HttpServerState, Request<Bytes>) -> Result<Response<Body>, String> + Send + 'static,
{
    dispatch_http_operation(state, request, move |state, request| {
        let path = request.uri().path();
        let job_scope =
            path.starts_with("/internal/jobs") || path.starts_with("/internal/job-outbox");
        // axum ignores query strings and implicitly accepts HEAD on GET. This API
        // has always matched the complete target and rejected HEAD operations.
        let supported = operation.is_some()
            && request.method() != "HEAD"
            && (request.uri().query().is_none()
                || (request.method() == "GET" && path.starts_with("/internal/jobs/")));
        if !job_scope && !supported {
            return Ok(http_response(404, "text/plain", b"not_found".to_vec()));
        }
        let token = match env::var("INTERNAL_API_TOKEN") {
            Ok(token) if !job_scope || !token.is_empty() => token,
            _ if job_scope => return json_error_response(500, "internal_token_unavailable"),
            _ => return Err("INTERNAL_API_TOKEN is required".to_string()),
        };
        if request
            .headers()
            .get("x-internal-token")
            .and_then(|value| value.to_str().ok())
            != Some(token.as_str())
        {
            return json_error_response(401, "unauthorized");
        }
        if supported {
            return operation.expect("supported route")(state, request);
        }
        // GET also reads identities that coincide with POST paths, and the empty
        // suffix must reach the existing ID validator rather than a new 404.
        if request.method() == "GET" && path.starts_with("/internal/jobs/") {
            return job_response(job_protocol::get_job(
                target(&request),
                state.job_store.as_ref(),
            ));
        }
        json_error_response(404, "job_protocol_not_found")
    })
    .await
}

fn target(request: &Request<Bytes>) -> &str {
    request
        .uri()
        .path_and_query()
        .map_or(request.uri().path(), |value| value.as_str())
}

fn job_response(result: job_protocol::ProtocolResult) -> Result<Response<Body>, String> {
    match result {
        Ok(value) => Ok(http_response(
            200,
            "application/json",
            value.to_string().into_bytes(),
        )),
        Err((status, error)) => json_error_response(status, error),
    }
}

pub(super) async fn dispatch_http_operation<F>(
    State(state): State<HttpServerState>,
    request: Request<Body>,
    operation: F,
) -> Response<Body>
where
    F: FnOnce(HttpServerState, Request<Bytes>) -> Result<Response<Body>, String> + Send + 'static,
{
    let observed_route = observations::route(request.uri().path());
    let observed_lane =
        if request.method().as_str() == "POST" && request.uri().path() == "/internal/jobs/wait" {
            2
        } else {
            0
        };
    let admission_started = Instant::now();
    let lane = state
        .request_capacity
        .lane(request.method().as_str(), request.uri().path());
    let observed_lane = if lane.control { 1 } else { observed_lane };
    let deadline = lane.deadline.map(|duration| Instant::now() + duration);
    let slot = if lane.control {
        // Absorb brief heartbeat bursts within the same control deadline.
        // Connection admission bounds these waiters; they execute no DB work.
        tokio::time::timeout_at(
            deadline.expect("control deadline").into(),
            lane.slots.acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        lane.slots.try_acquire_owned().ok()
    };
    let request_slot = match slot {
        Some(slot) => slot,
        None => {
            observations::Request::rejected(observed_route, observed_lane, admission_started);
            return bounded_json_error_response(503, "runtime_busy");
        }
    };
    let observed_request =
        observations::Request::admitted(observed_route, observed_lane, admission_started);
    let read = read_axum_request(request);
    let read_result = if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline.into(), read).await {
            Ok(result) => result,
            Err(_) => {
                return bounded_json_error_response(408, "request_body_timeout");
            }
        }
    } else {
        read.await
    };
    let request = match read_result {
        Ok(request) => request,
        Err(response) => return response,
    };
    let task = tokio::task::spawn_blocking(move || {
        let _context = observations::Context::enter(request.body());
        let _observed_request = observed_request;
        let _request_slot = request_slot;
        let _deadline = postgres_store::RequestDeadline::enter(deadline);
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(bounded_json_error_response(
                504,
                "request_deadline_exceeded",
            ));
        }
        let mut state = state;
        if lane.control {
            state.job_store = Arc::new(state.job_store.execution_control_view());
        }
        operation(state, request)
    });
    let result = if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline.into(), task).await {
            Ok(result) => result,
            // Dropping a JoinHandle does not stop blocking work. The closure owns
            // the permit until it actually exits; timeout must not mint capacity
            // or imply that an uncertain write was rolled back.
            Err(_) => {
                return bounded_json_error_response(504, "request_deadline_exceeded");
            }
        }
    } else {
        task.await
    };
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return bounded_json_error_response(504, "request_deadline_exceeded");
    }
    match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            eprintln!("runtime request failed: {error}");
            bounded_json_error_response(500, "internal_error")
        }
        Err(error) => {
            eprintln!("runtime request task failed: {error}");
            bounded_json_error_response(500, "internal_error")
        }
    }
}
