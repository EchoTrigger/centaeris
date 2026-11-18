//! Profile-owned Agent lifecycle shared by embedded and RPC hosts.

use crate::agent_runtime;
use crate::errors::RuntimeHostError;
use crate::runtime_rpc_transport::RuntimeServerClientHub;
use crate::subagent_scheduler;
use centaeris_core::session::reliability::{ListRuntimeJobsRequest, RuntimeJobStatus};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

mod session_writers;
pub(crate) use session_writers::SessionWriters;

/// The existing local profile's Agent host, embedded in the caller's Tokio runtime.
/// Owns profile configuration, storage, recovery and background work. RPC
/// listeners have their own entry point over the same host lifecycle.
pub struct LocalAgentHost {
    clients: Arc<RuntimeServerClientHub>,
    task: tokio::task::JoinHandle<Result<(), RuntimeHostError>>,
}

impl LocalAgentHost {
    pub(crate) fn clients(&self) -> &Arc<RuntimeServerClientHub> {
        &self.clients
    }

    /// Starts the profile-scoped host. Its single-writer lock remains held for
    /// the process lifetime, including after shutdown. Requires Tokio.
    pub async fn start() -> Result<Self, RuntimeHostError> {
        Self::start_with_session_writers(false).await
    }

    pub(crate) async fn start_for_cli() -> Result<Self, RuntimeHostError> {
        Self::start_with_session_writers(true).await
    }

    async fn start_with_session_writers(session_writers: bool) -> Result<Self, RuntimeHostError> {
        let (ready, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run_inner(session_writers, Some(ready), |clients| async move {
            clients.service_shutdown_requested().await;
            Ok(())
        }));
        match receiver.await {
            Ok(clients) => Ok(Self { clients, task }),
            Err(_) => match task.await {
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(RuntimeHostError::new(
                    "agent_start_failed",
                    "Agent host closed before initialization",
                )),
                Err(error) => Err(RuntimeHostError::new(
                    "agent_start_failed",
                    error.to_string(),
                )),
            },
        }
    }

    /// Registers a native SDK client with a bounded canonical event stream.
    pub fn connect(
        &self,
        viewer_id: &str,
    ) -> Result<centaeris_agent::AgentClient, RuntimeHostError> {
        crate::session_api::connect_native(&self.clients, viewer_id)
    }

    /// Stops admission and awaits accepted Runs, workers, processes and storage.
    pub async fn shutdown(self) -> Result<(), RuntimeHostError> {
        self.clients.request_service_shutdown()?;
        self.task
            .await
            .map_err(|error| RuntimeHostError::new("agent_shutdown_failed", error.to_string()))?
    }
}

const RUNTIME_SERVER_SHUTDOWN_DRAIN_MS: u64 = 5_000;
const RUNTIME_SERVER_SHUTDOWN_CLEANUP_MS: u64 = 5_000;

#[derive(Debug)]
pub(crate) struct RuntimeServerSingleton {
    _file: File,
}

// Blocking Host operations may outlive a cooperative shutdown. The profile
// writer lock remains held until the embedding process exits.
static PROCESS_SINGLETON: OnceLock<RuntimeServerSingleton> = OnceLock::new();
static SHARED_PROFILE_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn lock_profile_file(name: &str, read_only: bool) -> Result<Option<File>, String> {
    let Some(profile) = SHARED_PROFILE_ROOT.get() else {
        return Ok(None);
    };
    let path = profile.join("runtime").join(format!("{name}.lock"));
    let file = session_writers::open_lock(&path)?;
    let result = if read_only {
        FileExt::lock_shared(&file)
    } else {
        FileExt::lock_exclusive(&file)
    };
    result.map_err(|error| format!("Lock {} failed: {error}", path.display()))?;
    Ok(Some(file))
}

pub(crate) fn writer_lock_path(data_root: &Path) -> Result<PathBuf, RuntimeHostError> {
    let profile_id = crate::user_data_layout::profile_identity_for(data_root)
        .map_err(|error| RuntimeHostError::new("runtime_server_profile_identity_failed", error))?;
    Ok(data_root
        .join("runtime")
        .join(format!("centaeris-runtime-{profile_id}.writer.lock")))
}

pub(crate) fn log_runtime_server(message: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr(), "{message}");
}

pub(crate) async fn run<F, Fut>(
    ready: Option<tokio::sync::oneshot::Sender<Arc<RuntimeServerClientHub>>>,
    serve: F,
) -> Result<(), RuntimeHostError>
where
    F: FnOnce(Arc<RuntimeServerClientHub>) -> Fut,
    Fut: std::future::Future<Output = Result<(), RuntimeHostError>>,
{
    run_inner(false, ready, serve).await
}

async fn run_inner<F, Fut>(
    session_writers: bool,
    ready: Option<tokio::sync::oneshot::Sender<Arc<RuntimeServerClientHub>>>,
    serve: F,
) -> Result<(), RuntimeHostError>
where
    F: FnOnce(Arc<RuntimeServerClientHub>) -> Fut,
    Fut: std::future::Future<Output = Result<(), RuntimeHostError>>,
{
    crate::user_data_layout::ensure_runtime_endpoint_layout()
        .map_err(|error| RuntimeHostError::new("runtime_endpoint_layout_init_failed", error))?;
    let data_root = crate::user_data_layout::desktop_data_root_dir();
    let profile_lock = writer_lock_path(&data_root)?;
    let singleton = RuntimeServerSingleton::acquire_profile(&profile_lock, session_writers)?;
    PROCESS_SINGLETON.set(singleton).map_err(|_| {
        RuntimeHostError::new(
            "runtime_server_already_running",
            "Runtime Server already initialized in this process",
        )
    })?;
    let writers = if session_writers {
        SHARED_PROFILE_ROOT.set(data_root.clone()).map_err(|_| {
            RuntimeHostError::new("runtime_server_already_running", "CLI profile is already initialized")
        })?;
        Some(SessionWriters::new(&data_root))
    } else {
        None
    };
    let result = run_with_business_owners(writers, ready, serve).await;
    #[cfg(feature = "native-job-store")]
    let result = {
        let closed = tokio::time::timeout(
            Duration::from_millis(RUNTIME_SERVER_SHUTDOWN_CLEANUP_MS),
            agent_runtime::close_storage_owners(),
        )
        .await;
        let closed = match closed {
            Ok(result) => result.map_err(|error| RuntimeHostError::new("runtime_server_store_shutdown_failed", error)),
            Err(_) => Err(RuntimeHostError::new("runtime_server_store_shutdown_unconfirmed",
                "Database shutdown is still pending; accepted work was not declared cancelled or closed")),
        };
        result.and(closed)
    };
    result
}

async fn run_with_business_owners<F, Fut>(
    writers: Option<SessionWriters>,
    ready: Option<tokio::sync::oneshot::Sender<Arc<RuntimeServerClientHub>>>,
    serve: F,
) -> Result<(), RuntimeHostError>
where
    F: FnOnce(Arc<RuntimeServerClientHub>) -> Fut,
    Fut: std::future::Future<Output = Result<(), RuntimeHostError>>,
{
    {
        let _initialization = lock_profile_file("initialize", false)
            .map_err(RuntimeHostError::transport)?;
        crate::user_data_layout::ensure_user_data_layout()
            .map_err(|error| RuntimeHostError::new("user_data_layout_init_failed", error))?;
        crate::system_skills_deployment::deploy();
        agent_runtime::agent_runtime_store_actor()
            .map_err(|error| RuntimeHostError::new("runtime_server_store_init_failed", error))?;
    }
    if writers.is_none() {
        agent_runtime::recover_unsealed_live_text_journals().map_err(|error| {
            RuntimeHostError::new("runtime_server_live_text_recovery_failed", error)
        })?;
        subagent_scheduler::reconcile_stopped_jobs()
            .await
            .map_err(|error| RuntimeHostError::new("runtime_server_job_recovery_failed", error))?;
        #[cfg(feature = "native-job-store")]
        {
            let startup = Arc::new(RuntimeServerClientHub::default());
            if let Err(error) =
                crate::subagent_delivery::reconcile_terminal_jobs(&startup.broadcaster()).await
            {
                eprintln!("centaeris startup terminal recovery has pending failures: {error}");
            }
        }
    }
    let clients = Arc::new(RuntimeServerClientHub::with_session_writers(writers));
    let shutdown_jobs = Arc::new(subagent_scheduler::ShutdownJobs::default());
    let schedule_worker = tokio::spawn(crate::schedules::run_worker(Arc::clone(&clients)));
    let process_worker = tokio::spawn(crate::process_agent::run_worker(Arc::clone(&clients)));
    let background_worker = tokio::spawn(subagent_scheduler::run_background_worker(
        clients.broadcaster(),
        Arc::clone(&clients),
        Arc::clone(&shutdown_jobs),
    ));
    if let Some(ready) = ready {
        if ready.send(Arc::clone(&clients)).is_err() {
            clients.request_service_shutdown()?;
        }
    }
    let result = serve(Arc::clone(&clients)).await;
    // A listener error also closes business admission before storage admission.
    // Idle and explicit shutdown already share this same admission boundary.
    clients.request_service_shutdown()?;
    crate::terminal_sessions::manager().begin_shutdown();
    crate::process_sessions::manager().begin_shutdown();
    // The listener may finish because a new connection observes draining at
    // the same instant as the shutdown notification. Both exits must drain.
    let result = if clients.is_draining() {
        let drained = drain_service_shutdown(
            &clients,
            &background_worker,
            &shutdown_jobs,
            Duration::from_millis(RUNTIME_SERVER_SHUTDOWN_DRAIN_MS),
            Duration::from_millis(RUNTIME_SERVER_SHUTDOWN_CLEANUP_MS),
        )
        .await;
        result.and(drained)
    } else {
        result
    };
    background_worker.abort();
    process_worker.abort();
    schedule_worker.abort();
    let _ = background_worker.await;
    let _ = process_worker.await;
    let _ = schedule_worker.await;
    #[cfg(feature = "native-job-store")]
    let result = result.and(
        match tokio::time::timeout(
            Duration::from_millis(RUNTIME_SERVER_SHUTDOWN_CLEANUP_MS),
            clients.drain_background_work(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(RuntimeHostError::new(
                "runtime_server_business_shutdown_unconfirmed",
                "Accepted background sequences remain unfinished; recovery facts are retained",
            )),
        },
    );
    let shutdown_writer = clients.broadcaster();
    let processes = tokio::task::spawn_blocking(move || {
        crate::terminal_sessions::manager().shutdown()?;
        crate::process_sessions::manager().shutdown()?;
        crate::process_agent::flush_owned_completions(&shutdown_writer)
    })
    .await
    .map_err(|error| RuntimeHostError::transport(error.to_string()))
    .and_then(|result| {
        result.map_err(|error| RuntimeHostError::new("process_shutdown_failed", error))
    });
    let disconnected = clients.disconnect_all();
    result.and(processes).and(disconnected)
}

pub(crate) async fn drain_service_shutdown(
    clients: &Arc<RuntimeServerClientHub>,
    background_worker: &tokio::task::JoinHandle<()>,
    shutdown_jobs: &subagent_scheduler::ShutdownJobs,
    drain: Duration,
    cleanup: Duration,
) -> Result<(), RuntimeHostError> {
    // The shutdown reply shares the normal asynchronous writer. Yield before
    // checking an empty server, without making delivery a completion condition.
    tokio::task::yield_now().await;
    let drained =
        tokio::time::timeout(drain, wait_for_runtime_work(clients, background_worker)).await;
    if let Ok(result) = drained {
        return result;
    }
    // Cancellation closes admission; the owner remains responsible for tool
    // receipts and its terminal fact. A timeout never fabricates that fact.
    let result = tokio::time::timeout(cleanup, async {
        let active_runs = clients.active_agent_runs()?;
        tokio::task::spawn_blocking(move || {
            for active in active_runs {
                active.close_with_shutdown(|| Ok(()))?;
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|error| {
            RuntimeHostError::new("runtime_server_shutdown_failed", error.to_string())
        })?
        .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        subagent_scheduler::stop_pending_jobs_for_owner(shutdown_jobs, &clients.broadcaster())
            .await
            .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        wait_for_runtime_work(clients, background_worker).await
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            log_runtime_server(format_args!("runtime server shutdown cleanup timed out; unfinished outcomes will be reconciled on restart"));
            #[cfg(feature = "native-job-store")]
            {
                Err(RuntimeHostError::new(
                    "runtime_server_business_shutdown_unconfirmed",
                    "Business shutdown did not finish before its deadline",
                ))
            }
            #[cfg(not(feature = "native-job-store"))]
            {
                Ok(())
            }
        }
    }
}

async fn wait_for_runtime_work(
    clients: &Arc<RuntimeServerClientHub>,
    background_worker: &tokio::task::JoinHandle<()>,
) -> Result<(), RuntimeHostError> {
    loop {
        let store = agent_runtime::agent_runtime_job_store()
            .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        let roots = clients.broadcaster().owned_sessions().map_err(RuntimeHostError::transport)?;
        let job_scopes = roots.map_or_else(|| vec![None], |ids| ids.into_iter().map(Some).collect());
        let mut has_jobs = false;
        for session_id in job_scopes {
            let jobs = store.list_runtime_jobs(ListRuntimeJobsRequest {
                statuses: vec![
                    RuntimeJobStatus::Queued,
                    RuntimeJobStatus::Leased,
                    RuntimeJobStatus::Running,
                ],
                session_id,
                limit: 1,
                ..Default::default()
            })
            .await
            .map_err(|error| {
                RuntimeHostError::new("runtime_server_shutdown_failed", error.diagnostic_message())
            })?;
            if !jobs.is_empty() {
                has_jobs = true;
                break;
            }
        }
        if clients.active_agent_runs()?.is_empty()
            && clients.active_action_count() == 0
            && !has_jobs
            && background_worker.is_finished()
            && !crate::terminal_sessions::manager().has_active()
            && !crate::process_sessions::manager().has_active()
            && !crate::process_sessions::manager().has_pending_notifications()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

impl RuntimeServerSingleton {
    #[cfg(test)]
    pub(crate) fn acquire(lock_path: &Path) -> Result<Self, RuntimeHostError> {
        Self::acquire_profile(lock_path, false)
    }

    fn acquire_profile(lock_path: &Path, shared: bool) -> Result<Self, RuntimeHostError> {
        let parent = lock_path.parent().ok_or_else(|| {
            RuntimeHostError::new(
                "runtime_server_lock_invalid",
                format!("runtime server lock has no parent: {}", lock_path.display()),
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_lock_directory_failed",
                format!(
                    "create runtime server lock directory {} failed: {error}",
                    parent.display()
                ),
            )
        })?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|error| {
                RuntimeHostError::new(
                    "runtime_server_lock_open_failed",
                    format!(
                        "open runtime server lock {} failed: {error}",
                        lock_path.display()
                    ),
                )
            })?;
        let result = if shared {
            FileExt::try_lock_shared(&file)
        } else {
            FileExt::try_lock_exclusive(&file)
        };
        result.map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_already_running",
                format!(
                    "runtime server singleton lock {} is held: {error}",
                    lock_path.display()
                ),
            )
        })?;
        Ok(Self { _file: file })
    }
}
