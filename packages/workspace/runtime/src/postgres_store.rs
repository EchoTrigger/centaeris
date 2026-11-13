use postgres::{Client, NoTls};
mod session_payload;
pub(crate) mod session_subagents;
pub(crate) use session_payload::decode as decode_session_payload;
use std::cell::Cell;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_POSTGRES_POOL_SIZE: usize = 10;
const DEFAULT_POSTGRES_CONTROL_POOL_SIZE: usize = 2;
const DEFAULT_POSTGRES_LISTENER_LIMIT: usize = 8;
const DEFAULT_POSTGRES_CHECKOUT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_POSTGRES_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

thread_local! {
    static REQUEST_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Scoped to one blocking HTTP handler; pool limits also apply to background work.
pub(crate) struct RequestDeadline(Option<Instant>);

impl RequestDeadline {
    pub(crate) fn enter(deadline: Option<Instant>) -> Self {
        Self(REQUEST_DEADLINE.with(|value| value.replace(deadline)))
    }

    pub(crate) fn limit(timeout: Duration) -> Self {
        let limit = Instant::now() + timeout;
        let deadline =
            REQUEST_DEADLINE.with(|value| value.get().map_or(limit, |current| current.min(limit)));
        Self::enter(Some(deadline))
    }
}

impl Drop for RequestDeadline {
    fn drop(&mut self) {
        REQUEST_DEADLINE.with(|value| value.set(self.0));
    }
}

fn remaining_request_time() -> Result<Option<Duration>, String> {
    REQUEST_DEADLINE.with(|value| match value.get() {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                Err("request deadline exceeded".to_string())
            } else {
                Ok(Some(remaining))
            }
        }
        None => Ok(None),
    })
}

fn apply_request_timeout(client: &mut Client) -> Result<(), String> {
    if let Some(remaining) = remaining_request_time()? {
        let milliseconds = remaining.as_millis().max(1).to_string();
        client.query_one(
            "SELECT set_config('statement_timeout',$1,false),set_config('lock_timeout',$1,false)",
            &[&milliseconds],
        ).map_err(|error| format!("set request SQL timeout failed: {error}"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostgresConnectionLimits {
    pub ordinary: usize,
    pub execution_control: usize,
    pub listeners: usize,
    pub checkout_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for PostgresConnectionLimits {
    fn default() -> Self {
        Self {
            ordinary: DEFAULT_POSTGRES_POOL_SIZE,
            execution_control: DEFAULT_POSTGRES_CONTROL_POOL_SIZE,
            listeners: DEFAULT_POSTGRES_LISTENER_LIMIT,
            checkout_timeout: DEFAULT_POSTGRES_CHECKOUT_TIMEOUT,
            connect_timeout: DEFAULT_POSTGRES_CONNECT_TIMEOUT,
        }
    }
}

pub(crate) mod agent_run_api_purge;
pub(crate) mod allocation_origin;
mod capacity_wait;
pub(crate) mod cleanup_preservation;
pub(crate) mod cleanup_receipts;
pub(crate) mod controller_authority;
mod creation_attempts;
pub(crate) mod environment_observation;
mod environment_owner_schema;
mod execution_capacity;
pub(crate) mod execution_instances;
mod external_context;
mod host_retirement;
pub(crate) mod lease_renewal;
pub(crate) mod lifecycle_control;
pub(crate) mod maintenance;
pub(crate) mod pre_admission_failure;
pub(crate) mod provision;
mod question;
mod reliability;
pub(crate) mod residency;
mod resource_commit;
pub(crate) mod runsc_rotation;
mod runtime;
pub(crate) use runtime::session_deletion;
mod schema;
mod subagent_persistence;
mod transactions;
mod transcript;
pub(crate) mod turn_supplement;
pub(crate) mod workspace_snapshot_roots;

#[derive(Clone)]
pub struct PostgresRuntimeStore {
    dispatch_host: Option<Arc<execution_capacity::DispatchHost>>,
    subagent_persistence_scope: Option<subagent_persistence::SubagentPersistenceScope>,
    ordinary_connections: Arc<PostgresConnectionPool>,
    control_connections: Arc<PostgresConnectionPool>,
    cleanup_connections: Arc<PostgresConnectionPool>,
    listener_connections: Arc<PostgresConnectionPool>,
}

pub(crate) struct PostgresConnectionPool {
    config: postgres::Config,
    maximum: usize,
    checkout_timeout: Duration,
    state: Mutex<PostgresConnectionPoolState>,
    available: Condvar,
}

#[derive(Default)]
struct PostgresConnectionPoolState {
    idle: Vec<Client>,
    total: usize,
}

struct PooledConnection<'a> {
    pool: &'a PostgresConnectionPool,
    client: Option<Client>,
}

impl fmt::Debug for PostgresRuntimeStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresRuntimeStore")
            .field("database_url", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRunExecutionControlState {
    pub cancellation_requested: bool,
    pub lifecycle_lease_current: bool,
}

impl PostgresRuntimeStore {
    pub(crate) fn sandbox_deployment_id(&self) -> Result<String, String> {
        self.with_client(schema::sandbox_deployment_id)
    }

    #[cfg(test)]
    pub(crate) fn rejecting_http_test_store() -> Self {
        let pool = Arc::new(PostgresConnectionPool::new(
            postgres::Config::new(),
            0,
            Duration::from_millis(1),
        ));
        Self {
            dispatch_host: None,
            subagent_persistence_scope: None,
            ordinary_connections: pool.clone(),
            control_connections: pool.clone(),
            cleanup_connections: pool.clone(),
            listener_connections: pool,
        }
    }

    /// Route every ordinary operation in a control RPC through the reserved pool,
    /// including preflight Session reads. This shares the pool, not its capacity.
    pub(crate) fn execution_control_view(&self) -> Self {
        Self {
            dispatch_host: self.dispatch_host.clone(),
            subagent_persistence_scope: self.subagent_persistence_scope.clone(),
            ordinary_connections: self.control_connections.clone(),
            cleanup_connections: self.cleanup_connections.clone(),
            control_connections: self.control_connections.clone(),
            listener_connections: self.listener_connections.clone(),
        }
    }
    pub(crate) fn cleanup_view(&self) -> Self {
        Self {
            dispatch_host: self.dispatch_host.clone(),
            subagent_persistence_scope: self.subagent_persistence_scope.clone(),
            ordinary_connections: self.cleanup_connections.clone(),
            control_connections: self.control_connections.clone(),
            cleanup_connections: self.cleanup_connections.clone(),
            listener_connections: self.listener_connections.clone(),
        }
    }
    #[cfg(test)]
    pub fn new(database_url: &str) -> Result<Self, String> {
        Self::new_with_limits(database_url, PostgresConnectionLimits::default())
    }

    pub fn new_with_limits(
        database_url: &str,
        limits: PostgresConnectionLimits,
    ) -> Result<Self, String> {
        if database_url.trim().is_empty() {
            return Err("Postgres runtime database URL is required".to_string());
        }
        if limits.ordinary == 0
            || limits.execution_control == 0
            || limits.listeners == 0
            || limits.checkout_timeout.is_zero()
            || limits.connect_timeout.is_zero()
        {
            return Err("Postgres connection limits must be positive".to_string());
        }
        if Instant::now()
            .checked_add(limits.checkout_timeout)
            .is_none()
        {
            return Err("Postgres connection pool checkout timeout is too large".to_string());
        }
        let store = Self {
            dispatch_host: None,
            subagent_persistence_scope: None,
            ordinary_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    limits.connect_timeout,
                    "centaeris-runtime-ordinary",
                )?,
                limits.ordinary,
                limits.checkout_timeout,
            )),
            control_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    limits.connect_timeout,
                    "centaeris-runtime-control",
                )?,
                limits.execution_control,
                limits.checkout_timeout,
            )),
            cleanup_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    limits.connect_timeout,
                    "centaeris-runtime-cleanup",
                )?,
                crate::request_policy::capacity("RUNTIME_POSTGRES_CLEANUP_POOL_SIZE", 2)?,
                limits.checkout_timeout,
            )),
            listener_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    limits.connect_timeout,
                    "centaeris-runtime-listener",
                )?,
                limits.listeners,
                limits.checkout_timeout,
            )),
        };
        store.with_client(schema::ensure_schema)?;
        Ok(store)
    }

    #[cfg(test)]
    fn new_with_pool_limits(
        database_url: &str,
        maximum: usize,
        checkout_timeout: Duration,
    ) -> Result<Self, String> {
        if maximum == 0 || checkout_timeout.is_zero() {
            return Err("Postgres connection pool limits must be positive".to_string());
        }
        if Instant::now().checked_add(checkout_timeout).is_none() {
            return Err("Postgres connection pool checkout timeout is too large".to_string());
        }
        let store = Self {
            dispatch_host: None,
            subagent_persistence_scope: None,
            ordinary_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    DEFAULT_POSTGRES_CONNECT_TIMEOUT,
                    "centaeris-runtime-ordinary",
                )?,
                maximum,
                checkout_timeout,
            )),
            control_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    DEFAULT_POSTGRES_CONNECT_TIMEOUT,
                    "centaeris-runtime-control",
                )?,
                1,
                checkout_timeout,
            )),
            cleanup_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    DEFAULT_POSTGRES_CONNECT_TIMEOUT,
                    "centaeris-runtime-cleanup",
                )?,
                1,
                checkout_timeout,
            )),
            listener_connections: Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    database_url,
                    DEFAULT_POSTGRES_CONNECT_TIMEOUT,
                    "centaeris-runtime-listener",
                )?,
                1,
                checkout_timeout,
            )),
        };
        store.with_client(schema::ensure_schema)?;
        Ok(store)
    }

    pub(crate) fn with_client<T: Send>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        run_postgres_blocking(|| self.ordinary_connections.with_client(operation))
    }

    pub(crate) fn with_client_error<T: Send, E: From<String> + Send>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, E> + Send,
    ) -> Result<T, E> {
        run_postgres_blocking_error(|| self.ordinary_connections.with_client_error(operation))
    }

    pub(crate) fn with_execution_control_client<T: Send>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        run_postgres_blocking(|| self.control_connections.with_client(operation))
    }

    pub(crate) fn with_execution_control_client_error<T: Send, E: From<String> + Send>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, E> + Send,
    ) -> Result<T, E> {
        run_postgres_blocking_error(|| self.control_connections.with_client_error(operation))
    }

    pub(crate) fn with_listener_client<T: Send>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        run_postgres_blocking(|| self.listener_connections.with_client(operation))
    }

    pub fn session_log(
        &self,
        workspace_id: String,
        session_id: String,
        prompt: String,
    ) -> PostgresSessionLog {
        PostgresSessionLog::new_with_projection_store(
            self.clone(),
            self.ordinary_connections.clone(),
            workspace_id,
            session_id,
            prompt,
        )
    }
}

impl PostgresConnectionPool {
    fn new(config: postgres::Config, maximum: usize, checkout_timeout: Duration) -> Self {
        Self {
            config,
            maximum,
            checkout_timeout,
            state: Mutex::new(PostgresConnectionPoolState::default()),
            available: Condvar::new(),
        }
    }

    fn with_client<T>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_client_error(operation)
    }

    fn with_client_error<T, E: From<String>>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut connection = PooledConnection {
            pool: self,
            client: Some(self.checkout().map_err(E::from)?),
        };
        remaining_request_time().map_err(E::from)?;
        let result = operation(connection.client.as_mut().expect("checked out client"));
        if result.is_ok()
            && remaining_request_time().is_ok()
            && reset_connection(connection.client.as_mut().expect("checked out client")).is_ok()
        {
            connection.return_healthy().map_err(E::from)?;
        }
        // Never invoke operation again: an error after SQL was sent can represent an
        // unknown write or commit outcome. Protocol-level reconciliation owns replay.
        result
    }

    fn checkout(&self) -> Result<Client, String> {
        #[cfg(test)]
        assert_ne!(self.maximum, 0, "HTTP rejection must not access a database");
        let deadline = Instant::now()
            + remaining_request_time()?.map_or(self.checkout_timeout, |remaining| {
                remaining.min(self.checkout_timeout)
            });
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Postgres connection pool lock poisoned".to_string())?;
        loop {
            if Instant::now() >= deadline {
                return Err("Postgres connection pool checkout timed out".to_string());
            }
            if let Some(client) = state.idle.pop() {
                drop(state);
                let mut client = client;
                // Setting request timeouts also proves this idle connection is
                // alive. Background work still needs its explicit health probe.
                let healthy = if REQUEST_DEADLINE.with(Cell::get).is_some() {
                    apply_request_timeout(&mut client).is_ok()
                } else {
                    client.simple_query("SELECT 1").is_ok()
                };
                if healthy {
                    return Ok(client);
                }
                drop(client);
                self.discard();
                state = self
                    .state
                    .lock()
                    .map_err(|_| "Postgres connection pool lock poisoned".to_string())?;
                continue;
            }
            if state.total < self.maximum {
                state.total += 1;
                drop(state);
                let mut config = self.config.clone();
                let remaining = deadline.saturating_duration_since(Instant::now());
                config.connect_timeout(
                    config
                        .get_connect_timeout()
                        .copied()
                        .unwrap_or(remaining)
                        .min(remaining),
                );
                return match connect(&config).and_then(|mut client| {
                    apply_request_timeout(&mut client)?;
                    Ok(client)
                }) {
                    Ok(client) => Ok(client),
                    Err(error) => {
                        self.discard();
                        Err(error)
                    }
                };
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Postgres connection pool checkout timed out".to_string());
            }
            let (next, timeout) = self
                .available
                .wait_timeout(state, remaining)
                .map_err(|_| "Postgres connection pool lock poisoned".to_string())?;
            state = next;
            if timeout.timed_out() && state.idle.is_empty() {
                return Err("Postgres connection pool checkout timed out".to_string());
            }
        }
    }

    fn discard(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.total = state.total.saturating_sub(1);
            self.available.notify_one();
        }
    }
}

impl Drop for PostgresConnectionPool {
    fn drop(&mut self) {
        let clients = match self.state.get_mut() {
            Ok(state) => std::mem::take(&mut state.idle),
            Err(poisoned) => std::mem::take(&mut poisoned.into_inner().idle),
        };
        if clients.is_empty() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            let _ = std::thread::spawn(move || drop(clients)).join();
        } else {
            drop(clients);
        }
    }
}

impl PooledConnection<'_> {
    fn return_healthy(&mut self) -> Result<(), String> {
        let mut state = self
            .pool
            .state
            .lock()
            .map_err(|_| "Postgres connection pool lock poisoned".to_string())?;
        state
            .idle
            .push(self.client.take().expect("checked out client"));
        drop(state);
        self.pool.available.notify_one();
        Ok(())
    }
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            drop(client);
            self.pool.discard();
        }
    }
}

fn postgres_config(
    database_url: &str,
    connect_timeout: Duration,
    application_name: &str,
) -> Result<postgres::Config, String> {
    let mut config = database_url
        .parse::<postgres::Config>()
        .map_err(|error| format!("parse Postgres runtime database URL failed: {error}"))?;
    config.connect_timeout(connect_timeout);
    config.application_name(application_name);
    Ok(config)
}

fn connect(config: &postgres::Config) -> Result<Client, String> {
    let mut client = config
        .connect(NoTls)
        .map_err(|error| format!("connect Postgres runtime store failed: {error}"))?;
    client
        .batch_execute("SET search_path TO runtime, public")
        .map_err(|error| format!("set Postgres runtime search_path failed: {error}"))?;
    Ok(client)
}

fn reset_connection(client: &mut Client) -> Result<(), String> {
    client
        .simple_query("DISCARD ALL")
        .map_err(|error| format!("discard Postgres runtime connection state failed: {error}"))?;
    client
        .batch_execute("SET search_path TO runtime, public")
        .map_err(|error| format!("restore Postgres runtime search_path failed: {error}"))
}

pub(crate) fn run_postgres_blocking<T: Send>(
    operation: impl FnOnce() -> Result<T, String> + Send,
) -> Result<T, String> {
    run_postgres_blocking_error(operation)
}

fn run_postgres_blocking_error<T: Send, E: From<String> + Send>(
    operation: impl FnOnce() -> Result<T, E> + Send,
) -> Result<T, E> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::task::block_in_place(operation)
            }))
            .map_err(|_| E::from("Postgres blocking operation thread panicked".to_string()))?;
        }
        let deadline = REQUEST_DEADLINE.with(Cell::get);
        return std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let _deadline = RequestDeadline::enter(deadline);
                    operation()
                })
                .join()
                .map_err(|_| E::from("Postgres blocking operation thread panicked".to_string()))?
        });
    }
    operation()
}

pub use runtime::PostgresSessionLog;
pub(crate) use runtime::SessionWireHydrator;

#[cfg(test)]
pub(crate) mod integration_tests;

#[cfg(test)]
mod tests {
    use super::{
        postgres_config, run_postgres_blocking, PostgresConnectionPool, PostgresRuntimeStore,
        PostgresSessionLog, DEFAULT_POSTGRES_CHECKOUT_TIMEOUT, DEFAULT_POSTGRES_CONNECT_TIMEOUT,
        DEFAULT_POSTGRES_POOL_SIZE,
    };
    use centaeris_core::session::{
        canonical_session_record, RuntimeJobLeaseFence, SequencedSessionRecord, SessionRecordType,
    };
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn connect_inside_tokio_runtime_returns_error_instead_of_panicking() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                PostgresRuntimeStore::new("postgresql://127.0.0.1:1/centaeris?connect_timeout=1")
            })
        }));

        assert!(
            result.is_ok(),
            "Postgres connect must not panic inside Tokio"
        );
        assert!(
            result.expect("connection attempt must not panic").is_err(),
            "closed test port must return an error"
        );
    }

    #[test]
    fn postgres_operation_stays_on_a_tokio_spawn_blocking_thread() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let stayed_on_caller = runtime.block_on(async {
            tokio::task::spawn_blocking(|| {
                let caller = std::thread::current().id();
                run_postgres_blocking(|| Ok(std::thread::current().id() == caller))
            })
            .await
            .expect("join Tokio blocking task")
            .expect("run Postgres blocking boundary")
        });
        assert!(
            stayed_on_caller,
            "spawn_blocking must not create a second OS thread"
        );
    }

    #[test]
    fn postgres_operation_panic_inside_tokio_remains_an_error() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let result = runtime.block_on(async {
            tokio::task::spawn_blocking(|| {
                run_postgres_blocking::<()>(|| panic!("test operation panic"))
            })
            .await
            .expect("join Tokio blocking task")
        });
        assert_eq!(
            result.unwrap_err(),
            "Postgres blocking operation thread panicked"
        );
    }

    #[test]
    fn postgres_operation_uses_plain_thread_bridge_for_current_thread_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread Tokio runtime");
        let moved = runtime.block_on(async {
            let caller = std::thread::current().id();
            run_postgres_blocking(|| Ok(std::thread::current().id() != caller))
                .expect("run current-thread Postgres boundary")
        });
        assert!(
            moved,
            "current-thread Runtime needs the plain-thread bridge"
        );
    }

    #[test]
    fn postgres_connection_defaults_reserve_ten_ordinary_two_control_and_eight_listeners() {
        let limits = super::PostgresConnectionLimits::default();
        assert_eq!(
            (limits.ordinary, limits.execution_control, limits.listeners),
            (10, 2, 8)
        );
        assert_eq!(limits.checkout_timeout, Duration::from_secs(5));
        assert_eq!(limits.connect_timeout, Duration::from_secs(3));
    }

    #[test]
    fn postgres_store_rejects_unrepresentable_checkout_timeout() {
        let error = PostgresRuntimeStore::new_with_limits(
            "postgresql://127.0.0.1/centaeris",
            super::PostgresConnectionLimits {
                checkout_timeout: Duration::MAX,
                ..super::PostgresConnectionLimits::default()
            },
        )
        .expect_err("unrepresentable checkout deadline must fail at configuration time");
        assert_eq!(
            error,
            "Postgres connection pool checkout timeout is too large"
        );
    }

    #[test]
    #[ignore = "requires dedicated Postgres test database"]
    fn postgres_deadline_checkout_configures_and_replaces_connections_without_replay() {
        fn settings(client: &mut postgres::Client) -> Result<(i32, i64, i64), String> {
            client
                .query_one(
                    "SELECT pg_backend_pid(),\
                     (SELECT setting::bigint FROM pg_settings WHERE name='statement_timeout'),\
                     (SELECT setting::bigint FROM pg_settings WHERE name='lock_timeout')",
                    &[],
                )
                .map(|row| (row.get(0), row.get(1), row.get(2)))
                .map_err(|error| error.to_string())
        }

        let _guard = super::integration_tests::TEST_LOCK
            .lock()
            .expect("Postgres test lock");
        let url = std::env::var("CENTAERIS_TEST_POSTGRES_URL")
            .expect("CENTAERIS_TEST_POSTGRES_URL is required");
        let mut administrator =
            postgres::Client::connect(&url, postgres::NoTls).expect("connect test administrator");
        super::integration_tests::owned_database::require_owned_database(&mut administrator);
        let defaults = settings(&mut administrator).expect("read default SQL timeouts");
        let pool = PostgresConnectionPool::new(
            postgres_config(
                &url,
                DEFAULT_POSTGRES_CONNECT_TIMEOUT,
                "centaeris-runtime-pool-test",
            )
            .expect("test Postgres config"),
            1,
            DEFAULT_POSTGRES_CHECKOUT_TIMEOUT,
        );
        let first = {
            let _deadline = super::RequestDeadline::limit(Duration::from_secs(10));
            pool.with_client(settings)
                .expect("configure new connection before operation")
        };
        assert!(first.1 > 0 && first.1 <= 10_000);
        assert_eq!(first.1, first.2);
        let reused = {
            let _deadline = super::RequestDeadline::limit(Duration::from_secs(5));
            pool.with_client(settings)
                .expect("configure reused connection before operation")
        };
        assert_eq!(first.0, reused.0, "healthy connection must be reused");
        assert!(reused.1 > 0 && reused.1 <= 5_000);
        assert_eq!(reused.1, reused.2);

        assert!(administrator
            .query_one("SELECT pg_terminate_backend($1)", &[&first.0])
            .expect("terminate idle pooled backend")
            .get::<_, bool>(0));
        let stopped_by = std::time::Instant::now() + Duration::from_secs(5);
        while administrator
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                &[&first.0],
            )
            .expect("observe terminated backend exit")
            .get::<_, bool>(0)
        {
            assert!(
                std::time::Instant::now() < stopped_by,
                "terminated backend must exit before replacement checkout"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut calls = 0;
        let replacement = {
            let _deadline = super::RequestDeadline::limit(Duration::from_secs(5));
            pool.with_client(|client| {
                calls += 1;
                settings(client)
            })
            .expect("replace closed idle connection before operation")
        };
        assert_eq!(calls, 1, "business operation must not be replayed");
        assert_ne!(first.0, replacement.0);
        assert!(replacement.1 > 0 && replacement.1 <= 5_000);
        assert_eq!(replacement.1, replacement.2);
        let clean = pool
            .with_client(settings)
            .expect("background work gets restored SQL defaults");
        assert_eq!(replacement.0, clean.0);
        assert_eq!((clean.1, clean.2), (defaults.1, defaults.2));
    }

    #[test]
    fn synchronous_session_append_inside_tokio_returns_error_instead_of_panicking() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio runtime");
        let session_log = PostgresSessionLog::new(
            Arc::new(PostgresConnectionPool::new(
                postgres_config(
                    "postgresql://127.0.0.1:1/centaeris?connect_timeout=1",
                    DEFAULT_POSTGRES_CONNECT_TIMEOUT,
                    "centaeris-runtime-ordinary",
                )
                .expect("test Postgres config"),
                DEFAULT_POSTGRES_POOL_SIZE,
                DEFAULT_POSTGRES_CHECKOUT_TIMEOUT,
            )),
            "workspace_1".to_string(),
            "session_1".to_string(),
            "hello".to_string(),
        );
        let events = vec![SequencedSessionRecord {
            sequence: 1,
            event: canonical_session_record(
                "event_1",
                SessionRecordType::AgentRunStarted,
                "session_1",
                Some("run_1".to_string()),
                Some("run_1".to_string()),
                1,
                serde_json::json!({"userObjective":"hello"}),
            )
            .expect("canonical turn start"),
        }];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                session_log.append_session_records_with_runtime_job_lease_blocking(
                    "run_1",
                    events.as_slice(),
                    &RuntimeJobLeaseFence {
                        job_id: "run.lifecycle:run_1".to_string(),
                        job_kind: "run.lifecycle".to_string(),
                        lease_owner: "worker_1".to_string(),
                    },
                )
            })
        }));

        assert!(
            result.is_ok(),
            "sync SessionLog API must not panic in Tokio"
        );
        assert!(result.expect("SessionLog call must not panic").is_err());
    }
}
