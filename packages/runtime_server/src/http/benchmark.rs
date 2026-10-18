//! Independently authorized benchmark setup, verification and artifact transfer.
use super::*;
use crate::postgres_store::benchmark::BenchmarkTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use unicode_normalization::UnicodeNormalization;

pub(crate) const ARCHIVE_LIMIT: usize = 32 * 1024 * 1024;
const EXPANDED_LIMIT: u64 = 32 * 1024 * 1024;
const MEMBER_LIMIT: usize = 10000;
const METADATA_LIMIT: usize = 8192;
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ObserveRequest {
    pub schema: String,
    pub session_id: String,
    pub environment_id: String,
    pub agent_run_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecRequest {
    schema: String,
    session_id: String,
    environment_id: String,
    agent_run_id: String,
    instance_id: String,
    profile_digest: String,
    command: Vec<String>,
    cwd: Option<String>,
    user: Option<String>,
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

impl ExecRequest {
    fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.benchmark.exec.v1" {
            return Err("benchmark_request_invalid".into());
        }
        self.binding().validate()?;
        if !(1..=3_600_000).contains(&self.timeout_ms)
            || self.command.is_empty()
            || self.command.len() > 256
            || self
                .command
                .iter()
                .any(|item| item.contains('\0') || item.len() > 16384)
            || self.command[0].is_empty()
            || self.command.iter().map(String::len).sum::<usize>() > 65536
            || self.env.iter().any(|(key, value)| {
                key.is_empty()
                    || !key.bytes().enumerate().all(|(i, c)| {
                        c.is_ascii_alphabetic() || c == b'_' || (i > 0 && c.is_ascii_digit())
                    })
                    || value.contains('\0')
            })
            || self
                .env
                .iter()
                .map(|(key, value)| key.len().saturating_add(value.len()))
                .sum::<usize>()
                > 65536
        {
            return Err("benchmark_exec_request_invalid".into());
        }
        if let Some(cwd) = &self.cwd {
            validate_path(cwd)?;
        }
        if let Some(user) = &self.user {
            validate_exec_user(user)?;
        }
        Ok(())
    }
    fn binding(&self) -> TargetBinding<'_> {
        TargetBinding {
            session: &self.session_id,
            environment: &self.environment_id,
            run: &self.agent_run_id,
            instance: &self.instance_id,
            profile_digest: &self.profile_digest,
        }
    }
}

fn independent_authority(
    configured: Option<&str>,
    internal: Option<&str>,
    supplied: Option<&str>,
) -> bool {
    configured.is_some_and(|key| key.len() >= 32 && Some(key) != internal && supplied == Some(key))
}

fn validate_exec_user(user: &str) -> Result<(), String> {
    let component = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && if value.bytes().all(|c| c.is_ascii_digit()) {
                (value == "0" || !value.starts_with('0'))
                    && value.parse::<u32>().is_ok_and(|id| id < u32::MAX)
            } else {
                value.bytes().enumerate().all(|(index, c)| {
                    c.is_ascii_alphabetic()
                        || c == b'_'
                        || (index > 0 && (c.is_ascii_digit() || b".-".contains(&c)))
                })
            }
    };
    let mut parts = user.split(':');
    if !parts.next().is_some_and(component)
        || parts.next().is_some_and(|group| !component(group))
        || parts.next().is_some()
    {
        return Err("benchmark_exec_user_invalid".into());
    }
    Ok(())
}

fn validate_archive(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > ARCHIVE_LIMIT {
        return Err("benchmark_archive_limit_exceeded".into());
    }
    if bytes.len() < 512 {
        return Err("benchmark_archive_limit_or_format_invalid".into());
    }
    let mut archive = tar::Archive::new(bytes);
    let mut names = BTreeMap::<String, bool>::new();
    let mut expanded = 0_u64;
    for entry in archive.entries().map_err(|_| "benchmark_archive_invalid")? {
        let mut entry = entry.map_err(|_| "benchmark_archive_invalid")?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("benchmark_archive_entry_type_unsupported".into());
        }
        if let Some(extensions) = entry
            .pax_extensions()
            .map_err(|_| "benchmark_archive_invalid")?
        {
            for extension in extensions {
                let extension = extension.map_err(|_| "benchmark_archive_invalid")?;
                let key = extension.key_bytes();
                if key.starts_with(b"GNU.sparse.") || key == b"SCHILY.realsize" {
                    return Err("benchmark_archive_sparse_unsupported".into());
                }
            }
        }
        if names.len() >= MEMBER_LIMIT || (kind.is_dir() && entry.size() != 0) {
            return Err("benchmark_archive_limit_or_format_invalid".into());
        }
        let raw = entry.path_bytes();
        let path = std::str::from_utf8(&raw)
            .map_err(|_| "benchmark_archive_path_invalid")?
            .trim_end_matches('/');
        if path == "." && kind.is_dir() {
            if names.insert(".".into(), true).is_some() {
                return Err("benchmark_archive_path_invalid".into());
            }
            continue;
        }
        if path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || path.len() > 4096
            || names.len() >= MEMBER_LIMIT
            || names.contains_key(path)
        {
            return Err("benchmark_archive_path_invalid".into());
        }
        let mut prefix = String::new();
        for part in path.split('/').take(path.split('/').count() - 1) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if names.get(&prefix) == Some(&false) {
                return Err("benchmark_archive_parent_collision".into());
            }
        }
        if kind.is_file() {
            let prefix = format!("{path}/");
            if names
                .range(prefix.clone()..)
                .next()
                .is_some_and(|(existing, _)| existing.starts_with(&prefix))
            {
                return Err("benchmark_archive_parent_collision".into());
            }
        }
        names.insert(path.into(), kind.is_dir());
        expanded = expanded
            .checked_add(entry.size())
            .ok_or("benchmark_archive_limit_exceeded")?;
        if expanded > EXPANDED_LIMIT {
            return Err("benchmark_archive_limit_exceeded".into());
        }
        std::io::copy(&mut entry, &mut std::io::sink()).map_err(|_| "benchmark_archive_invalid")?;
    }
    Ok(())
}

struct TargetBinding<'a> {
    session: &'a str,
    environment: &'a str,
    run: &'a str,
    instance: &'a str,
    profile_digest: &'a str,
}
fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
}
impl TargetBinding<'_> {
    fn validate(&self) -> Result<(), String> {
        if ![self.session, self.environment, self.run, self.instance]
            .iter()
            .all(|value| opaque(value))
            || !self
                .profile_digest
                .strip_prefix("sha256:")
                .is_some_and(|h| {
                    h.len() == 64
                        && h.bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                })
        {
            return Err("benchmark_target_request_invalid".into());
        }
        Ok(())
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.len() > 4096
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.nfc().collect::<String>() != path
        || (path != "/"
            && path
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == ".."))
        || ["/proc", "/sys", "/dev", "/run/centaeris", "/opt/centaeris"]
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}/")))
    {
        return Err("benchmark_container_path_invalid".into());
    }
    Ok(())
}

fn authorized<B>(request: &Request<B>) -> bool {
    independent_authority(
        env::var("ENVIRONMENT_MAINTENANCE_TOKEN").ok().as_deref(),
        env::var("INTERNAL_API_TOKEN").ok().as_deref(),
        request
            .headers()
            .get("x-environment-maintenance-token")
            .and_then(|v| v.to_str().ok()),
    )
}
fn key() -> Result<String, String> {
    env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or("benchmark_signing_key_unavailable".into())
}
fn response(value: impl Serialize) -> Result<Response<Body>, String> {
    Ok(http_response(
        200,
        "application/json",
        serde_json::to_vec(&value).map_err(|_| "benchmark_response_invalid")?,
    ))
}

fn physical_identity(facts: &Value, target: &BenchmarkTarget) -> bool {
    let Some(a) = &target.allocation else {
        return false;
    };
    let labels = &facts["Config"]["Labels"];
    facts["Id"].as_str() == a.container_id.as_deref()
        && facts["Image"].as_str() == Some(target.profile.image_digest.as_str())
        && [
            ("centaeris.managed", "true"),
            ("centaeris.agent_run_id", a.agent_run_id.as_str()),
            ("centaeris.execution_id", a.execution_id.as_str()),
            ("centaeris.environment_id", target.environment_id.as_str()),
            ("centaeris.instance_id", a.instance_id.as_str()),
            (
                "centaeris.environment_authorization_digest",
                a.binding_digest.as_str(),
            ),
            (
                "centaeris.retained_profile_digest",
                a.retained_profile_digest.as_str(),
            ),
            (
                "centaeris.benchmark_profile_digest",
                target.profile_digest.as_str(),
            ),
        ]
        .iter()
        .all(|(name, value)| labels[*name].as_str() == Some(*value))
}

fn ready_id(target: &BenchmarkTarget, binding: &TargetBinding<'_>) -> Result<String, String> {
    let a = target
        .allocation
        .as_ref()
        .ok_or("benchmark_allocation_pending")?;
    if target.retirement_state.is_some()
        || !target.resource_holding
        || a.state.as_deref() != Some("resident")
        || a.instance_id != binding.instance
        || target.profile_digest != binding.profile_digest
        || crate::docker_engine::info()?["ID"].as_str() != Some(a.host_id.as_str())
    {
        return Err("benchmark_target_rejected".into());
    }
    let id = a
        .container_id
        .as_deref()
        .ok_or("benchmark_container_pending")?;
    let facts = crate::docker_engine::inspect(id)?.ok_or("benchmark_container_missing")?;
    if !physical_identity(&facts, target) || facts["State"]["Running"] != true {
        return Err("benchmark_physical_identity_rejected".into());
    }
    Ok(id.into())
}

fn owned_operation<T: Send>(
    state: &HttpServerState,
    binding: &TargetBinding<'_>,
    operation: impl FnOnce(&BenchmarkTarget, &str) -> Result<T, String> + Send,
) -> Result<T, String> {
    binding.validate()?;
    state.job_store.with_benchmark_target(
        binding.session,
        binding.environment,
        binding.run,
        key()?.as_bytes(),
        |target| {
            let id = ready_id(target, binding)?;
            operation(target, &id)
        },
    )
}

pub(super) fn observe(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if !authorized(&request) {
        return json_error_response(403, "environment_maintenance_authority_required");
    }
    let value: ObserveRequest = match serde_json::from_slice(request.body()) {
        Ok(value) => value,
        Err(_) => return json_error_response(400, "benchmark_request_invalid"),
    };
    if value.schema != "runtime.benchmark.observe.v1"
        || ![
            value.session_id.as_str(),
            value.environment_id.as_str(),
            value.agent_run_id.as_str(),
        ]
        .iter()
        .all(|value| opaque(value))
    {
        return json_error_response(400, "benchmark_request_invalid");
    }
    let result=state.job_store.with_benchmark_target(&value.session_id,&value.environment_id,&value.agent_run_id,key()?.as_bytes(),|target|{
        let a=target.allocation.as_ref();
        let correct_host=a.is_some_and(|a|crate::docker_engine::info().ok().is_some_and(|info|info["ID"].as_str()==Some(a.host_id.as_str())));
        let (physical,running)=match a.and_then(|a|a.container_id.as_deref()).filter(|_|correct_host){
            Some(id)=>match crate::docker_engine::inspect(id){Ok(None)=>("absent",false),Ok(Some(facts)) if physical_identity(&facts,target)=>("present",facts["State"]["Running"]==true),_=>("unknown",false)},None=>("unknown",false)};
        let state=if target.retirement_state.as_deref()==Some("destroyed") && !target.resource_holding && physical=="absent"{"destroyed"}
            else if target.retirement_state.is_some(){"unknown"}
            else if a.is_none(){"waitingSetup"}
            else if a.is_some_and(|a|a.container_id.is_none() || a.state.as_deref()==Some("reserved")){"allocationPending"}
            else if physical=="present" && running && target.resource_holding {if target.setup_ready{"ready"}else{"waitingSetup"}}
            else{"unknown"};
        Ok(json!({"schema":"runtime.benchmark.observe.result.v1","state":state,"physicalState":physical,"sessionId":target.session_id,"environmentId":target.environment_id,"agentRunId":target.agent_run_id,"instanceId":a.map(|a|&a.instance_id),"containerId":a.and_then(|a|a.container_id.as_ref()),"hostId":a.map(|a|&a.host_id),"controllerEpoch":target.controller_epoch,"imageDigest":target.profile.image_digest,"profileDigest":target.profile_digest,"resourceHolding":u8::from(target.resource_holding)}))
    });
    observation_response(result)
}

fn observation_response(result: Result<Value, String>) -> Result<Response<Body>, String> {
    match result {
        Ok(value) => response(value),
        Err(error) if error == "benchmark_store_unavailable" => {
            json_error_response(503, "benchmark_observation_unavailable")
        }
        Err(_) => json_error_response(409, "benchmark_observation_rejected"),
    }
}

pub(crate) fn exec_timeout(body: &[u8]) -> Result<Duration, String> {
    let value: ExecRequest =
        serde_json::from_slice(body).map_err(|_| "benchmark_request_invalid")?;
    value.validate()?;
    Ok(Duration::from_millis(
        value.timeout_ms.saturating_add(35_000),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecResult {
    schema: &'static str,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    outcome: &'static str,
}

fn collect_output(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut exceeded = false;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "benchmark_exec_output_unknown")?;
        if count == 0 {
            break;
        }
        let keep = count.min(OUTPUT_LIMIT.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
        exceeded |= keep < count;
    }
    if exceeded {
        return Err("benchmark_exec_output_limit_exceeded".into());
    }
    Ok(output)
}

fn command_request(
    target: &BenchmarkTarget,
    id: &str,
    value: &ExecRequest,
) -> Result<crate::docker_engine::ExecRequest, String> {
    let user = value.user.as_deref().unwrap_or(&target.profile.user);
    let cwd = value
        .cwd
        .as_deref()
        .unwrap_or(&target.profile.workspace_root);
    validate_exec_user(user)?;
    validate_path(cwd)?;
    let a = target
        .allocation
        .as_ref()
        .ok_or("benchmark_allocation_pending")?;
    let mut command = crate::docker_engine::ExecRequest::owned(
        id,
        &a.agent_run_id,
        &a.execution_id,
        value.command.clone(),
    );
    command.cwd = Some(cwd.into());
    command.user = Some(user.into());
    command.env = value
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    Ok(command)
}

pub(super) fn exec(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if !authorized(&request) {
        return json_error_response(403, "environment_maintenance_authority_required");
    }
    let value: ExecRequest = match serde_json::from_slice(request.body()) {
        Ok(value) => value,
        Err(_) => return json_error_response(400, "benchmark_request_invalid"),
    };
    if value.validate().is_err() {
        return json_error_response(400, "benchmark_request_invalid");
    }
    let started = owned_operation(&state, &value.binding(), |target, id| {
        command_request(target, id, &value)?.spawn()
    });
    let mut child = match started {
        Ok(child) => child,
        Err(_) => {
            return response(ExecResult {
                schema: "runtime.benchmark.exec.result.v1",
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                outcome: "unknown",
            })
        }
    };
    child.stdin.take();
    let stdout = child.stdout.take().ok_or("benchmark_exec_output_unknown")?;
    let stderr = child.stderr.take().ok_or("benchmark_exec_output_unknown")?;
    let out = std::thread::spawn(move || collect_output(stdout));
    let err = std::thread::spawn(move || collect_output(stderr));
    let deadline = Instant::now() + Duration::from_millis(value.timeout_ms);
    let mut timed_out = false;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                timed_out = true;
                child.disconnect();
                break None;
            }
            Err(_) => {
                child.disconnect();
                break None;
            }
        }
    };
    let stdout = out.join().map_err(|_| "benchmark_exec_output_unknown")?;
    let stderr = err.join().map_err(|_| "benchmark_exec_output_unknown")?;
    if stdout
        .as_ref()
        .err()
        .is_some_and(|e| e == "benchmark_exec_output_limit_exceeded")
        || stderr
            .as_ref()
            .err()
            .is_some_and(|e| e == "benchmark_exec_output_limit_exceeded")
    {
        return json_error_response(413, "benchmark_exec_output_limit_exceeded");
    }
    let outcome = if exit.is_some() && stdout.is_ok() && stderr.is_ok() {
        "completed"
    } else {
        "unknown"
    };
    response(ExecResult {
        schema: "runtime.benchmark.exec.result.v1",
        exit_code: exit,
        stdout: String::from_utf8_lossy(&stdout.unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned(),
        timed_out,
        outcome,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransferRequest {
    schema: String,
    session_id: String,
    environment_id: String,
    agent_run_id: String,
    instance_id: String,
    profile_digest: String,
    path: String,
}
impl TransferRequest {
    fn binding(&self) -> TargetBinding<'_> {
        TargetBinding {
            session: &self.session_id,
            environment: &self.environment_id,
            run: &self.agent_run_id,
            instance: &self.instance_id,
            profile_digest: &self.profile_digest,
        }
    }
}

fn upload_metadata<B>(request: &Request<B>) -> Result<TransferRequest, &'static str> {
    if request.headers().get_all("content-type").iter().count() != 1
        || request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some("application/x-tar")
    {
        return Err("benchmark_transfer_content_type_invalid");
    }
    if request
        .headers()
        .get_all("x-benchmark-transfer")
        .iter()
        .count()
        != 1
    {
        return Err("benchmark_transfer_metadata_invalid");
    }
    let raw = request
        .headers()
        .get("x-benchmark-transfer")
        .ok_or("benchmark_transfer_metadata_invalid")?;
    if raw.len() > METADATA_LIMIT {
        return Err("benchmark_transfer_metadata_invalid");
    }
    let value: TransferRequest = serde_json::from_slice(raw.as_bytes())
        .map_err(|_| "benchmark_transfer_metadata_invalid")?;
    if value.schema != "runtime.benchmark.upload.v1"
        || value.binding().validate().is_err()
        || validate_path(&value.path).is_err()
    {
        return Err("benchmark_request_invalid");
    }
    Ok(value)
}

pub(crate) fn upload_body_authority<B>(request: &Request<B>) -> Result<(), (u16, &'static str)> {
    if ["x-internal-token", "x-environment-maintenance-token"]
        .iter()
        .any(|key| request.headers().get_all(*key).iter().count() > 1)
    {
        return Err((400, "request_header_duplicate"));
    }
    let internal = env::var("INTERNAL_API_TOKEN").ok();
    if internal.as_ref().is_none_or(|token| {
        token.is_empty()
            || request
                .headers()
                .get("x-internal-token")
                .and_then(|v| v.to_str().ok())
                != Some(token.as_str())
    }) {
        return Err((401, "unauthorized"));
    }
    if !authorized(request) {
        return Err((403, "environment_maintenance_authority_required"));
    }
    upload_metadata(request)
        .map(|_| ())
        .map_err(|code| (400, code))
}

pub(super) fn upload(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if !authorized(&request) {
        return json_error_response(403, "environment_maintenance_authority_required");
    }
    let value = match upload_metadata(&request) {
        Ok(value) => value,
        Err(code) => return json_error_response(400, code),
    };
    if value.schema != "runtime.benchmark.upload.v1"
        || value.binding().validate().is_err()
        || validate_path(&value.path).is_err()
    {
        return json_error_response(400, "benchmark_request_invalid");
    }
    let bytes = request.into_body();
    if let Err(error) = validate_archive(&bytes) {
        if error == "benchmark_archive_limit_exceeded" {
            return json_error_response(413, "benchmark_archive_limit_exceeded");
        }
        return json_error_response(400, "benchmark_archive_invalid");
    }
    let count = bytes.len();
    match owned_operation(&state, &value.binding(), |_, id| {
        crate::docker_engine::benchmark_transfer::upload(id, &value.path, bytes)
    }) {
        Ok(()) => {
            response(json!({"schema":"runtime.benchmark.upload.result.v1","uploadedBytes":count}))
        }
        Err(error) => transfer_error_response(&error),
    }
}

pub(super) fn download(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if !authorized(&request) {
        return json_error_response(403, "environment_maintenance_authority_required");
    }
    let value: TransferRequest = match serde_json::from_slice(request.body()) {
        Ok(value) => value,
        Err(_) => return json_error_response(400, "benchmark_request_invalid"),
    };
    if value.schema != "runtime.benchmark.download.v1"
        || value.binding().validate().is_err()
        || validate_path(&value.path).is_err()
    {
        return json_error_response(400, "benchmark_request_invalid");
    }
    download_response(owned_operation(&state, &value.binding(), |_, id| {
        crate::docker_engine::benchmark_transfer::download(id, &value.path, ARCHIVE_LIMIT)
    }))
}

fn download_response(result: Result<Vec<u8>, String>) -> Result<Response<Body>, String> {
    match result {
        Ok(bytes) => {
            if let Err(error) = validate_archive(&bytes) {
                if error == "benchmark_archive_limit_exceeded" {
                    return json_error_response(413, "benchmark_archive_limit_exceeded");
                }
                return json_error_response(409, "benchmark_download_archive_unsupported");
            }
            Ok(http_response(200, "application/x-tar", bytes))
        }
        Err(error) => transfer_error_response(&error),
    }
}

fn transfer_error_response(error: &str) -> Result<Response<Body>, String> {
    let (status, code) = match error {
        "benchmark_archive_limit_exceeded" => (413, "benchmark_archive_limit_exceeded"),
        "benchmark_download_path_missing" => (404, "benchmark_download_path_missing"),
        "benchmark_upload_outcome_unknown" | "benchmark_dispatch_commit_unknown" => {
            (409, "benchmark_transfer_outcome_unknown")
        }
        "benchmark_allocation_pending"
        | "benchmark_container_pending"
        | "benchmark_container_missing"
        | "benchmark_transfer_target_unavailable" => (409, "benchmark_transfer_target_unavailable"),
        "benchmark_target_rejected"
        | "benchmark_physical_identity_rejected"
        | "benchmark_session_missing"
        | "benchmark_environment_missing"
        | "benchmark_run_missing"
        | "benchmark_session_environment_rejected"
        | "benchmark_run_binding_rejected"
        | "benchmark_agent_binding_rejected"
        | "benchmark_grant_invalid"
        | "benchmark_grant_binding_rejected"
        | "benchmark_profile_binding_rejected"
        | "benchmark_profile_missing"
        | "benchmark_profile_invalid"
        | "benchmark_allocation_binding_rejected"
        | "benchmark_allocation_missing"
        | "benchmark_incarnation_ambiguous"
        | "benchmark_resource_binding_rejected"
        | "benchmark_resources_invalid"
        | "benchmark_retirement_binding_rejected"
        | "benchmark_setup_binding_rejected"
        | "benchmark_setup_instance_rejected"
        | "benchmark_unbound_resource_holding" => (409, "benchmark_transfer_identity_rejected"),
        _ => (503, "benchmark_transfer_dependency_unavailable"),
    };
    json_error_response(status, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_limits_match_the_controller_contract() {
        let value: Value = serde_json::from_str(include_str!(
            "../../../../scripts/harbor/benchmark-transport.json"
        ))
        .unwrap();
        assert_eq!(
            value,
            json!({"schema":"centaeris.benchmarkTransport.v1", "archiveBytes":ARCHIVE_LIMIT,
            "expandedBytes":EXPANDED_LIMIT, "members":MEMBER_LIMIT, "metadataBytes":METADATA_LIMIT})
        );
    }

    #[test]
    fn raw_upload_metadata_is_strict_bounded_and_does_not_take_identity_from_tar_content() {
        let metadata = json!({"schema":"runtime.benchmark.upload.v1", "sessionId":"session", "environmentId":"environment",
            "agentRunId":"run", "instanceId":"instance", "profileDigest":format!("sha256:{}", "b".repeat(64)), "path":"/tests"});
        let request = |value: &Value, content_type: &str| {
            Request::builder()
                .header("content-type", content_type)
                .header(
                    "x-benchmark-transfer",
                    serde_json::to_string(value).unwrap(),
                )
                .body(Bytes::from_static(b"arbitrary tar data"))
                .unwrap()
        };
        assert_eq!(
            upload_metadata(&request(&metadata, "application/x-tar"))
                .unwrap()
                .instance_id,
            "instance"
        );
        assert!(upload_metadata(&request(&metadata, "application/json")).is_err());
        for extra in ["archiveBase64", "containerId", "unknown"] {
            let mut value = metadata.clone();
            value[extra] = "untrusted".into();
            assert!(upload_metadata(&request(&value, "application/x-tar")).is_err());
        }
        let mut missing = request(&metadata, "application/x-tar");
        missing.headers_mut().remove("x-benchmark-transfer");
        assert!(upload_metadata(&missing).is_err());
        let mut duplicate = request(&metadata, "application/x-tar");
        duplicate
            .headers_mut()
            .append("x-benchmark-transfer", "{}".parse().unwrap());
        assert!(upload_metadata(&duplicate).is_err());
        for path in [
            "../foreign".into(),
            "/tests/../foreign".into(),
            "/tests\nforeign".into(),
            format!("/{}", "x".repeat(METADATA_LIMIT)),
        ] {
            let mut value = metadata.clone();
            value["path"] = path.into();
            assert!(upload_metadata(&request(&value, "application/x-tar")).is_err());
        }
    }

    #[test]
    fn download_errors_distinguish_missing_limit_identity_and_uncertain_outcomes_without_details() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for (error, status, code) in [
            (
                "benchmark_download_path_missing",
                404,
                "benchmark_download_path_missing",
            ),
            (
                "benchmark_archive_limit_exceeded",
                413,
                "benchmark_archive_limit_exceeded",
            ),
            (
                "benchmark_physical_identity_rejected",
                409,
                "benchmark_transfer_identity_rejected",
            ),
            (
                "benchmark_run_binding_rejected",
                409,
                "benchmark_transfer_identity_rejected",
            ),
            (
                "benchmark_dispatch_commit_unknown",
                409,
                "benchmark_transfer_outcome_unknown",
            ),
            (
                "benchmark_store_unavailable",
                503,
                "benchmark_transfer_dependency_unavailable",
            ),
            (
                "Docker error secret=/private/path",
                503,
                "benchmark_transfer_dependency_unavailable",
            ),
        ] {
            let response = download_response(Err(error.into())).unwrap();
            assert_eq!(response.status(), status, "{error}");
            let body = runtime
                .block_on(axum::body::to_bytes(response.into_body(), 4096))
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"error":code})
            );
        }
    }

    #[test]
    fn observation_store_unavailable_is_temporary_but_binding_errors_are_conflicts() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for (error, status, code) in [
            (
                "benchmark_store_unavailable",
                503,
                "benchmark_observation_unavailable",
            ),
            (
                "benchmark_run_binding_rejected",
                409,
                "benchmark_observation_rejected",
            ),
            (
                "benchmark_allocation_binding_rejected",
                409,
                "benchmark_observation_rejected",
            ),
            (
                "benchmark_profile_missing",
                409,
                "benchmark_observation_rejected",
            ),
            (
                "benchmark_dispatch_commit_unknown",
                409,
                "benchmark_observation_rejected",
            ),
        ] {
            let response = observation_response(Err(error.into())).unwrap();
            assert_eq!(response.status().as_u16(), status, "{error}");
            let bytes = runtime
                .block_on(axum::body::to_bytes(response.into_body(), 4096))
                .unwrap();
            let payload: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(payload, json!({"error": code}), "{error}");
        }
    }

    fn exec_payload() -> Value {
        serde_json::json!({
            "schema":"runtime.benchmark.exec.v1", "sessionId":"session", "environmentId":"environment",
            "agentRunId":"run", "instanceId":"instance", "profileDigest":format!("sha256:{}", "a".repeat(64)),
            "command":["/bin/bash","-lc","echo exact"], "cwd":"/app", "user":null,"env":{},"timeoutMs":1000
        })
    }

    fn exec_request() -> ExecRequest {
        serde_json::from_value(exec_payload()).unwrap()
    }

    fn archive(path: &str, kind: tar::EntryType) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(0);
        header.set_entry_type(kind);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("../outside").unwrap();
        }
        header.set_cksum();
        archive
            .append_data(&mut header, path, std::io::empty())
            .unwrap();
        archive.into_inner().unwrap()
    }

    #[test]
    fn maintenance_authority_is_required_independently_of_internal_token() {
        let maintenance = "m".repeat(32);
        let internal = "i".repeat(32);
        assert!(independent_authority(
            Some(&maintenance),
            Some(&internal),
            Some(&maintenance)
        ));
        for (configured, supplied) in [
            (None, None),
            (Some(""), Some("")),
            (Some(internal.as_str()), Some(internal.as_str())),
            (Some(maintenance.as_str()), Some(internal.as_str())),
        ] {
            assert!(!independent_authority(
                configured,
                Some(&internal),
                supplied
            ));
        }
    }

    #[test]
    fn actual_http_operations_reject_missing_maintenance_header_before_parsing_or_storage() {
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let _runtime_guard = runtime.enter();
        let store = PostgresRuntimeStore::rejecting_http_test_store();
        let state = HttpServerState {
            runtime: runtime.clone(),
            store: Arc::new(RuntimeStoreActor::start(store.clone()).unwrap()),
            job_store: Arc::new(store),
            tool_layers: Default::default(),
            execution_profile: Arc::new(RuntimeExecutionProfile {
                schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
                image_capability: "workspace-general",
                image_digest: "sha256:fixture".into(),
            }),
            request_capacity: request_capacity::RequestCapacity::for_http_tests(
                Duration::from_secs(1),
            ),
        };
        let operations: [super::Operation; 4] = [observe, exec, upload, download];
        for operation in operations {
            let request = Request::builder()
                .method("POST")
                .uri("/internal/benchmark/exec")
                .body(Bytes::from_static(b"not JSON"))
                .unwrap();
            assert_eq!(operation(state.clone(), request).unwrap().status(), 403);
        }
    }

    #[test]
    fn exact_exec_request_accepts_explicit_command_and_rejects_unknown_fields() {
        exec_request().validate().unwrap();
        let mut value = exec_payload();
        value["containerId"] = "arbitrary".into();
        assert!(serde_json::from_value::<ExecRequest>(value.clone()).is_err());
        value["command"] = serde_json::json!(["/bin/bash"]);
        assert!(serde_json::from_value::<ExecRequest>(value).is_err());
    }

    #[test]
    fn exec_request_rejects_invalid_identity_deadline_and_environment() {
        for mutate in [
            |r: &mut ExecRequest| r.instance_id = "../foreign".into(),
            |r: &mut ExecRequest| r.timeout_ms = 0,
            |r: &mut ExecRequest| r.timeout_ms = 3_600_001,
            |r: &mut ExecRequest| r.command = vec![],
            |r: &mut ExecRequest| {
                r.env.insert("BAD=KEY".into(), "value".into());
            },
        ] {
            let mut request = exec_request();
            mutate(&mut request);
            assert!(request.validate().is_err());
        }
    }

    #[test]
    fn verifier_archive_above_eight_mib_is_accepted_within_the_bounded_transfer_cap() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(11_506_423);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "private_test.tar.gz",
                std::io::repeat(b'x').take(11_506_423),
            )
            .unwrap();
        validate_archive(&builder.into_inner().unwrap()).unwrap();
    }

    #[test]
    fn bounded_regular_tar_is_accepted_but_links_devices_and_invalid_archive_are_rejected() {
        validate_archive(&archive("tests/test.sh", tar::EntryType::Regular)).unwrap();
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Fifo,
            tar::EntryType::Char,
        ] {
            assert!(validate_archive(&archive("tests/unsafe", kind)).is_err());
        }
        assert!(validate_archive(b"not tar").is_err());
        assert!(validate_archive(&vec![0; ARCHIVE_LIMIT + 1]).is_err());
    }

    #[test]
    fn tar_rejects_traversal_duplicates_and_file_parent_collisions() {
        for paths in [
            vec!["../outside"],
            vec!["/outside"],
            vec!["dir\\outside"],
            vec!["file", "file"],
            vec!["dir", "dir/child"],
            vec!["dir/child", "dir"],
        ] {
            let mut builder = tar::Builder::new(Vec::new());
            for path in paths {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_size(0);
                header.set_entry_type(tar::EntryType::Regular);
                header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
                header.set_cksum();
                builder.append(&header, std::io::empty()).unwrap();
            }
            assert!(validate_archive(&builder.into_inner().unwrap()).is_err());
        }
        let path = format!("tests/{}/test.sh", "a".repeat(180));
        validate_archive(&archive(&path, tar::EntryType::Regular)).unwrap();
    }

    #[test]
    fn pax_mtime_is_supported_but_sparse_extensions_are_rejected() {
        for (key, accepted) in [
            ("mtime", true),
            ("GNU.sparse.realsize", false),
            ("GNU.sparse.map", false),
            ("SCHILY.realsize", false),
        ] {
            let mut builder = tar::Builder::new(Vec::new());
            builder.append_pax_extensions([(key, &b"123"[..])]).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, "test.sh", std::io::empty())
                .unwrap();
            assert_eq!(
                validate_archive(&builder.into_inner().unwrap()).is_ok(),
                accepted,
                "{key}"
            );
        }
    }

    #[test]
    fn exec_deadline_is_bounded_and_output_overflow_is_an_explicit_error() {
        let body = serde_json::to_vec(&exec_payload()).unwrap();
        assert_eq!(exec_timeout(&body).unwrap(), Duration::from_secs(36));
        assert_eq!(collect_output(&b"exact"[..]).unwrap(), b"exact");
        assert_eq!(
            collect_output(&vec![b'x'; OUTPUT_LIMIT + 1][..]).unwrap_err(),
            "benchmark_exec_output_limit_exceeded"
        );
    }

    #[test]
    fn maintenance_exec_accepts_docker_user_selectors_without_changing_the_signed_profile() {
        let target = test_target();
        for user in [
            "root",
            "0",
            "0:0",
            "1000:1000",
            "root:root",
            "1000:users",
            "agent:1000",
        ] {
            let mut request = exec_request();
            request.user = Some(user.into());
            let actual = command_request(&target, "exact-container", &request).unwrap();
            assert_eq!(actual.user.as_deref(), Some(user));
            assert_eq!(target.profile.user, "0:0");
        }
        for user in [
            "",
            ":",
            ":root",
            "root:",
            "root:root:root",
            "root\n",
            "../root",
            "root group",
            "4294967295",
        ] {
            assert!(validate_exec_user(user).is_err(), "{user:?}");
        }
    }

    #[test]
    fn physical_identity_requires_original_allocation_and_both_profile_bindings() {
        let target = test_target();
        let facts = json!({"Id":"exact-container","Image":target.profile.image_digest,"Config":{"Labels":{"centaeris.managed":"true","centaeris.agent_run_id":"agent_run_1","centaeris.execution_id":"origin-execution","centaeris.environment_id":"environment","centaeris.instance_id":"instance","centaeris.environment_authorization_digest":"binding","centaeris.retained_profile_digest":"retained-profile","centaeris.benchmark_profile_digest":target.profile_digest}}});
        assert!(physical_identity(&facts, &target));
        for pointer in [
            "/Id",
            "/Image",
            "/Config/Labels/centaeris.instance_id",
            "/Config/Labels/centaeris.environment_id",
            "/Config/Labels/centaeris.agent_run_id",
            "/Config/Labels/centaeris.execution_id",
            "/Config/Labels/centaeris.retained_profile_digest",
            "/Config/Labels/centaeris.benchmark_profile_digest",
        ] {
            let mut wrong = facts.clone();
            *wrong.pointer_mut(pointer).unwrap() = "foreign".into();
            assert!(!physical_identity(&wrong, &target), "{pointer}");
        }
    }

    fn test_target() -> BenchmarkTarget {
        use crate::postgres_store::benchmark::BenchmarkAllocation;
        let grant: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
            serde_json::from_str(include_str!(
                "../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
            ))
            .unwrap();
        BenchmarkTarget {
            session_id: "sess_1".into(),
            environment_id: "environment".into(),
            agent_run_id: "agent_run_1".into(),
            controller_epoch: 3,
            profile: grant.benchmark_profile.unwrap(),
            profile_digest: grant.benchmark_profile_digest.unwrap(),
            allocation: Some(BenchmarkAllocation {
                execution_id: "origin-execution".into(),
                agent_run_id: "agent_run_1".into(),
                instance_id: "instance".into(),
                host_id: "host".into(),
                container_id: Some("exact-container".into()),
                binding_digest: "binding".into(),
                retained_profile_digest: "retained-profile".into(),
                state: Some("resident".into()),
            }),
            resource_holding: true,
            retirement_state: None,
            setup_ready: true,
        }
    }
}
