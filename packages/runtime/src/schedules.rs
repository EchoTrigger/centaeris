//! Profile-owned local schedules. Only the Runtime singleton dispatches Agent work.
use crate::runtime_rpc_transport::EventWriter;
use crate::{
    agent_runtime, message_log, operation_receipts, runtime_config, sessions, user_data_layout,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

static LOCK: Mutex<()> = Mutex::new(());
mod storage;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelSelection {
    pub provider_id: String,
    pub model: String,
    pub thinking_mode: Option<String>,
}
impl ModelSelection {
    pub fn apply(
        &self,
        config: &mut runtime_config::AgentRuntimeConfigResponse,
    ) -> Result<(), String> {
        let item = config
            .selectable_models
            .iter()
            .find(|m| m.provider_id == self.provider_id && m.model == self.model)
            .ok_or("scheduled model is unavailable")?;
        match &self.thinking_mode {
            Some(mode) if item.model_thinking_modes.contains(mode) => {},
            None if item.model_thinking_modes.is_empty() => {},
            _ => return Err("schedule requires an explicit supported thinkingMode (null only for models without reasoning modes)".into()),
        }
        config.model_provider_id = Some(self.provider_id.clone());
        config.model = Some(self.model.clone());
        config.model_thinking_mode = self.thinking_mode.clone();
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Spec {
    pub name: String,
    pub cwd: String,
    pub prompt: String,
    pub cron: Option<String>,
    pub at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    pub timezone: String,
    pub model: ModelSelection,
}
impl Spec {
    fn next(&self, after: i64) -> Result<Option<i64>, String> {
        match (&self.cron, self.at) {
            (Some(cron), None) => next_time(cron, &self.timezone, after).map(Some),
            (None, Some(at)) => Ok((at > after).then_some(at)),
            _ => Err("provide exactly one of cron or at".into()),
        }
    }
    fn validate(&self, now: i64) -> Result<i64, String> {
        if self
            .expires_at
            .is_some_and(|expiry| self.at.is_none_or(|at| expiry < at) || self.cron.is_some())
        {
            return Err("expiresAt requires a one-shot and cannot precede at".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 200
            || self.prompt.trim().is_empty()
            || self.prompt.len() > 65536
        {
            return Err("name and bounded nonempty prompt are required".into());
        }
        if !Path::new(&self.cwd).is_absolute() || !Path::new(&self.cwd).is_dir() {
            return Err("cwd must be an existing absolute directory".into());
        }
        self.timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|e| e.to_string())?;
        self.model.apply(&mut runtime_config::get(
            runtime_config::AgentRuntimeConfigGetRequest {},
        )?)?;
        self.next(now)?
            .ok_or_else(|| "one-shot time must be in the future".into())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    id: String,
    revision: u64,
    enabled: bool,
    deleted: bool,
    spec: Spec,
    next_at: Option<i64>,
    create_digest: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Run {
    id: String,
    schedule_id: String,
    revision: u64,
    scheduled_at: i64,
    spec: Spec,
    status: String,
    session_id: Option<String>,
    agent_run_id: Option<String>,
    error: Option<String>,
}
impl Run {
    fn active(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "running")
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Store {
    schema_version: u32,
    service_enabled: bool,
    plans: Vec<Plan>,
    runs: Vec<Run>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            schema_version: 1,
            service_enabled: false,
            plans: vec![],
            runs: vec![],
        }
    }
}
fn root() -> PathBuf {
    user_data_layout::desktop_data_root_dir().join("runtime/schedules.json")
}
fn next_time(cron: &str, timezone: &str, after: i64) -> Result<i64, String> {
    if cron.split_whitespace().count() != 5 {
        return Err("cron requires five fields: minute hour day month weekday".into());
    }
    let tz = timezone
        .parse::<chrono_tz::Tz>()
        .map_err(|e| e.to_string())?;
    let after = DateTime::<Utc>::from_timestamp_millis(after)
        .ok_or("invalid schedule time")?
        .with_timezone(&tz);
    croner::Cron::from_str(cron)
        .map_err(|e| e.to_string())?
        .find_next_occurrence(&after, false)
        .map(|t| t.timestamp_millis())
        .map_err(|e| e.to_string())
}
#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum Request {
    List {
        limit: Option<usize>,
        cursor: Option<String>,
    },
    Create {
        operation_id: String,
        spec: Spec,
    },
    Update {
        schedule_id: String,
        expected_revision: u64,
        spec: Spec,
    },
    Pause {
        schedule_id: String,
    },
    Resume {
        schedule_id: String,
    },
    Delete {
        schedule_id: String,
    },
    History {
        schedule_id: String,
        limit: Option<usize>,
        cursor: Option<String>,
    },
    Run {
        schedule_id: String,
        operation_id: String,
    },
    Service {
        enabled: bool,
    },
}
fn plan_mut<'a>(store: &'a mut Store, id: &str) -> Result<&'a mut Plan, String> {
    store
        .plans
        .iter_mut()
        .find(|p| p.id == id && !p.deleted)
        .ok_or_else(|| "schedule not found".into())
}
fn operation(id: &str) -> Result<(), String> {
    if id.trim().is_empty() || id.len() > 256 {
        Err("operationId must be nonempty and at most 256 bytes".into())
    } else {
        Ok(())
    }
}
fn occurrence(plan: &Plan, id: String, at: i64, status: &str) -> Run {
    Run {
        agent_run_id: Some(operation_receipts::deterministic_identity(
            "agent-run-",
            "session/prompt",
            &format!("{id}-prompt"),
        )),
        id,
        schedule_id: plan.id.clone(),
        revision: plan.revision,
        scheduled_at: at,
        spec: plan.spec.clone(),
        status: status.into(),
        session_id: None,
        error: None,
    }
}
pub(crate) fn manage(request: Request) -> Result<Value, String> {
    let _guard = LOCK.lock().map_err(|_| "schedule lock poisoned")?;
    let path = root();
    let mut store = storage::load_request(&path, &request)?;
    let now = Utc::now().timestamp_millis();
    let result = match request {
        Request::List { limit, cursor } => return storage::page(&path, None, limit, cursor),
        Request::History {
            schedule_id,
            limit,
            cursor,
        } => return storage::page(&path, Some(&schedule_id), limit, cursor),
        Request::Create { operation_id, spec } => {
            operation(&operation_id)?;
            let id = operation_receipts::deterministic_identity(
                "schedule-",
                "schedule/create",
                &operation_id,
            );
            let digest = operation_receipts::request_digest(&spec)?;
            if let Some(p) = store.plans.iter().find(|p| p.id == id) {
                return if p.create_digest == digest {
                    Ok(json!(p))
                } else {
                    Err("operationId conflict".into())
                };
            }
            let next_at = spec.validate(now)?;
            let plan = Plan {
                id,
                revision: 1,
                enabled: true,
                deleted: false,
                spec,
                next_at: Some(next_at),
                create_digest: digest,
            };
            let result = json!(plan);
            store.plans.push(plan);
            result
        }
        Request::Update {
            schedule_id,
            expected_revision,
            spec,
        } => {
            let next = spec.validate(now)?;
            let p = plan_mut(&mut store, &schedule_id)?;
            if p.revision != expected_revision {
                return Err("schedule revision conflict".into());
            }
            p.revision += 1;
            p.spec = spec;
            p.next_at = Some(next);
            json!(p)
        }
        Request::Pause { schedule_id } => {
            let p = plan_mut(&mut store, &schedule_id)?;
            p.enabled = false;
            json!(p)
        }
        Request::Resume { schedule_id } => {
            let p = plan_mut(&mut store, &schedule_id)?;
            if !p.enabled {
                p.next_at = Some(p.spec.validate(now)?);
                p.enabled = true;
            }
            json!(p)
        }
        Request::Delete { schedule_id } => {
            let p = store
                .plans
                .iter_mut()
                .find(|p| p.id == schedule_id)
                .ok_or("schedule not found")?;
            p.deleted = true;
            p.enabled = false;
            json!({"deleted":true})
        }
        Request::Service { enabled } => {
            store.service_enabled = enabled;
            json!({"serviceEnabled":enabled,"machineAwakeRequired":true})
        }
        Request::Run {
            schedule_id,
            operation_id,
        } => {
            operation(&operation_id)?;
            let id = operation_receipts::deterministic_identity(
                "occurrence-",
                &schedule_id,
                &operation_id,
            );
            if let Some(run) = store.runs.iter().find(|r| r.id == id) {
                return Ok(json!(run));
            }
            if store
                .runs
                .iter()
                .any(|r| r.schedule_id == schedule_id && r.active())
            {
                return Err("previous occurrence is still active".into());
            }
            let p = plan_mut(&mut store, &schedule_id)?;
            let run = occurrence(p, id, now, "pending");
            let result = json!(run);
            store.runs.push(run);
            result
        }
    };
    storage::save(&path, &store)?;
    Ok(result)
}
// Coalesce missed recurring occurrences into the latest due occurrence.
fn plan_due(store: &mut Store, now: i64) -> Result<(), String> {
    if !store.service_enabled {
        return Ok(());
    }
    for p in &mut store.plans {
        if p.deleted || !p.enabled {
            continue;
        }
        let Some(at) = p.next_at.filter(|t| *t <= now) else {
            continue;
        };
        let at = if let Some(cron) = &p.spec.cron {
            let tz = p
                .spec
                .timezone
                .parse::<chrono_tz::Tz>()
                .map_err(|e| e.to_string())?;
            let time = DateTime::<Utc>::from_timestamp_millis(now)
                .ok_or("invalid schedule time")?
                .with_timezone(&tz);
            croner::Cron::from_str(cron)
                .map_err(|e| e.to_string())?
                .find_previous_occurrence(&time, true)
                .map_err(|e| e.to_string())?
                .timestamp_millis()
                .max(at)
        } else {
            at
        };
        let status = if p.spec.expires_at.is_some_and(|expiry| now > expiry) {
            "expired"
        } else if store
            .runs
            .iter()
            .any(|r| r.schedule_id == p.id && r.active())
        {
            "skippedOverlap"
        } else {
            "pending"
        };
        let id = operation_receipts::deterministic_identity(
            "occurrence-",
            &p.id,
            &format!("{}:{at}", p.revision),
        );
        if !store.runs.iter().any(|r| r.id == id) {
            store.runs.push(occurrence(p, id, at, status));
        }
        p.next_at = p.spec.next(now)?;
        if p.next_at.is_none() {
            p.enabled = false;
        }
    }
    Ok(())
}
pub(crate) fn keep_alive() -> bool {
    // Conservatively keep the writer alive on corruption; the worker logs the
    // error instead of silently treating unreadable schedules as an empty store.
    let Ok(_guard) = LOCK.try_lock() else {
        return true;
    };
    storage::keep_alive(&root()).unwrap_or(true)
}
fn dispatch(writer: &EventWriter, run: &mut Run) -> Result<(), String> {
    if let Some(id) = &run.agent_run_id {
        if let Some(existing) = message_log::project_agent_run(id)? {
            run.session_id = Some(existing.session_id.clone());
            run.error = None;
            run.status = if matches!(
                existing.status.as_str(),
                "succeeded" | "failed" | "cancelled"
            ) {
                existing.status
            } else {
                "running".into()
            };
            return Ok(());
        }
    }
    if run.status == "running" {
        run.status = "sessionDeleted".into();
        run.error = Some(
            "Previously admitted Session/AgentRun is no longer available; it was not recreated"
                .into(),
        );
        return Ok(());
    }
    if run
        .spec
        .expires_at
        .is_some_and(|expiry| Utc::now().timestamp_millis() > expiry)
    {
        run.status = "expired".into();
        return Ok(());
    }
    let mut config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})?;
    if let Err(error) = run.spec.model.apply(&mut config).and_then(|()| {
        if Path::new(&run.spec.cwd).is_dir() {
            Ok(())
        } else {
            Err("scheduled working directory is unavailable".into())
        }
    }) {
        run.status = "failedAdmission".into();
        run.error = Some(error);
        return Ok(());
    }
    let session = sessions::create_command(sessions::SessionCreateCommandRequest {
        operation_id: format!("{}-session", run.id),
        title: Some(format!("Scheduled: {}", run.spec.name)),
        cwd: run.spec.cwd.clone(),
    })
    .map_err(|e| e.to_string())?;
    run.session_id = Some(session.id.clone());
    let response = agent_runtime::input_with_model(
        writer.clone(),
        agent_runtime::AgentInputRequest {
            operation_id: format!("{}-prompt", run.id),
            session_id: Some(session.id),
            message: run.spec.prompt.clone(),
            tail_policy: None,
            rewrite_target_message_id: None,
            rewrite_expected_tail_message_id: None,
            auto_continue_after_resume_wait: None,
            attachments: vec![],
        },
        Some(run.spec.model.clone()),
    )
    .map_err(|e| e.to_string())?;
    run.agent_run_id = Some(response.agent_run_id);
    run.status = "running".into();
    run.error = None;
    Ok(())
}
fn tick(writer: &EventWriter) -> Result<(), String> {
    let _guard = LOCK.lock().map_err(|_| "schedule lock poisoned")?;
    let path = root();
    let mut store = storage::load_work(&path, Utc::now().timestamp_millis())?;
    let mut saved = serde_json::to_vec(&store).map_err(|e| e.to_string())?;
    // Reconcile active receipts first, including after a crash between admission
    // and saving its acknowledgement. Retrying uses the same Session/Run keys.
    for index in 0..store.runs.len() {
        if !store.runs[index].active() {
            continue;
        }
        if let Err(error) = dispatch(writer, &mut store.runs[index]) {
            store.runs[index].error = Some(error);
            // No new identity is created on uncertainty. Retain for reconciliation.
        }
        let current = serde_json::to_vec(&store).map_err(|e| e.to_string())?;
        if current != saved {
            storage::save(&path, &store)?;
            saved = current;
        }
    }
    plan_due(&mut store, Utc::now().timestamp_millis())?;
    // Intent and next-fire advance are one atomic write, before external effects.
    if serde_json::to_vec(&store).map_err(|e| e.to_string())? != saved {
        storage::save(&path, &store)?;
    }
    Ok(())
}
pub(crate) async fn run_worker(clients: Arc<crate::runtime_rpc_transport::RuntimeServerClientHub>) {
    while !clients.is_draining() {
        let writer = clients.process_scheduler();
        match tokio::task::spawn_blocking(move || tick(&writer)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("schedule dispatch failed: {e}"),
            Err(e) => eprintln!("schedule worker failed: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
#[cfg(test)]
mod tests;
