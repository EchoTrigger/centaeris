use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

pub const DEFAULT_TOOL_PARALLELISM: usize = 16;
pub const MAX_TOOL_PARALLELISM: usize = 64;

pub fn normalize_tool_parallelism(value: usize) -> usize {
    value.clamp(1, MAX_TOOL_PARALLELISM)
}

#[derive(Debug, Clone)]
pub struct ToolConcurrencyCoordinator {
    state: Arc<ToolConcurrencyCoordinatorState>,
}

#[derive(Debug)]
struct ToolConcurrencyCoordinatorState {
    capacity: usize,
    semaphore: Arc<Semaphore>,
    serial_semaphore: Arc<Semaphore>,
}

#[derive(Debug)]
pub struct ToolConcurrencyPermit {
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug)]
pub struct ToolConcurrencySerialPermit {
    _serial_permit: OwnedSemaphorePermit,
    _tool_permit: ToolConcurrencyPermit,
}

impl ToolConcurrencyCoordinator {
    pub fn new(capacity: usize) -> Self {
        let capacity = normalize_tool_parallelism(capacity);
        Self {
            state: Arc::new(ToolConcurrencyCoordinatorState {
                capacity,
                semaphore: Arc::new(Semaphore::new(capacity)),
                serial_semaphore: Arc::new(Semaphore::new(1)),
            }),
        }
    }

    pub fn global_for_scope(scope_id: impl AsRef<str>, capacity: usize) -> Result<Self, String> {
        let scope_id = scope_id.as_ref().trim();
        if scope_id.is_empty() {
            return Err("tool concurrency coordinator scope id is required".to_string());
        }
        let capacity = normalize_tool_parallelism(capacity);
        let mut registry = tool_concurrency_registry()
            .lock()
            .map_err(|_| "tool concurrency coordinator registry poisoned".to_string())?;
        registry.retain(|_, state| state.strong_count() > 0);
        if let Some(existing) = registry.get(scope_id).and_then(Weak::upgrade) {
            if existing.capacity != capacity {
                return Err(format!(
                    "tool concurrency coordinator scope {scope_id} already uses capacity {}, requested {capacity}",
                    existing.capacity
                ));
            }
            return Ok(Self { state: existing });
        }
        let coordinator = Self::new(capacity);
        registry.insert(scope_id.to_string(), Arc::downgrade(&coordinator.state));
        Ok(coordinator)
    }

    pub fn capacity(&self) -> usize {
        self.state.capacity
    }

    pub fn acquire(&self) -> ToolConcurrencyPermit {
        ToolConcurrencyPermit {
            _permit: acquire_owned_blocking(self.state.semaphore.clone()),
        }
    }

    pub fn acquire_serial(&self) -> ToolConcurrencySerialPermit {
        let serial_permit = acquire_owned_blocking(self.state.serial_semaphore.clone());
        let tool_permit = self.acquire();
        ToolConcurrencySerialPermit {
            _serial_permit: serial_permit,
            _tool_permit: tool_permit,
        }
    }

    pub async fn acquire_async(&self) -> ToolConcurrencyPermit {
        let permit = self
            .state
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("tool concurrency coordinator closed");
        ToolConcurrencyPermit { _permit: permit }
    }

    pub async fn acquire_serial_async(&self) -> ToolConcurrencySerialPermit {
        let serial_permit = self
            .state
            .serial_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("tool serial concurrency coordinator closed");
        let tool_permit = self.acquire_async().await;
        ToolConcurrencySerialPermit {
            _serial_permit: serial_permit,
            _tool_permit: tool_permit,
        }
    }
}

fn acquire_owned_blocking(semaphore: Arc<Semaphore>) -> OwnedSemaphorePermit {
    loop {
        match semaphore.clone().try_acquire_owned() {
            Ok(permit) => return permit,
            Err(TryAcquireError::NoPermits) => {
                std::thread::park_timeout(Duration::from_millis(1));
            }
            Err(TryAcquireError::Closed) => {
                panic!("tool concurrency coordinator closed");
            }
        }
    }
}

fn tool_concurrency_registry(
) -> &'static Mutex<HashMap<String, Weak<ToolConcurrencyCoordinatorState>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<ToolConcurrencyCoordinatorState>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::{ToolConcurrencyCoordinator, DEFAULT_TOOL_PARALLELISM, MAX_TOOL_PARALLELISM};

    #[test]
    fn global_scope_reuses_matching_capacity_and_rejects_mismatch() {
        let scope = format!(
            "test-scope-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        );
        let coordinator =
            ToolConcurrencyCoordinator::global_for_scope(scope.as_str(), DEFAULT_TOOL_PARALLELISM)
                .expect("first coordinator");
        let matching =
            ToolConcurrencyCoordinator::global_for_scope(scope.as_str(), DEFAULT_TOOL_PARALLELISM)
                .expect("matching coordinator");

        assert_eq!(coordinator.capacity(), DEFAULT_TOOL_PARALLELISM);
        assert_eq!(matching.capacity(), DEFAULT_TOOL_PARALLELISM);
        assert!(
            ToolConcurrencyCoordinator::global_for_scope(scope.as_str(), MAX_TOOL_PARALLELISM)
                .is_err()
        );
    }
}
