use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
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

/// Host-owned lookup for tool budgets shared within an explicit execution scope.
///
/// Each host service owns a registry. Entries hold weak references: executors own
/// the budgets, and expired entries are removed on the next lookup. Independent
/// registries never share budgets, even when their scope identifiers match.
#[derive(Debug, Default)]
pub struct ToolConcurrencyRegistry {
    scopes: Mutex<HashMap<String, Weak<ToolConcurrencyCoordinatorState>>>,
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
}

impl ToolConcurrencyRegistry {
    /// Reuse a live scope's budget, or create one with the normalized capacity.
    ///
    /// # Errors
    /// Returns an error for an empty scope, a poisoned registry, or a capacity
    /// conflict with an existing live budget.
    pub fn for_scope(
        &self,
        scope_id: impl AsRef<str>,
        capacity: usize,
    ) -> Result<ToolConcurrencyCoordinator, String> {
        let scope_id = scope_id.as_ref().trim();
        if scope_id.is_empty() {
            return Err("tool concurrency coordinator scope id is required".to_string());
        }
        let capacity = normalize_tool_parallelism(capacity);
        let mut registry = self
            .scopes
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
            return Ok(ToolConcurrencyCoordinator { state: existing });
        }
        let coordinator = ToolConcurrencyCoordinator::new(capacity);
        registry.insert(scope_id.to_string(), Arc::downgrade(&coordinator.state));
        Ok(coordinator)
    }
}

impl ToolConcurrencyCoordinator {
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

#[cfg(test)]
mod tests {
    use super::{ToolConcurrencyRegistry, DEFAULT_TOOL_PARALLELISM, MAX_TOOL_PARALLELISM};

    #[test]
    fn scope_reuses_matching_capacity_and_rejects_mismatch() {
        let registry = ToolConcurrencyRegistry::default();
        let scope = "session";
        let coordinator = registry
            .for_scope(scope, DEFAULT_TOOL_PARALLELISM)
            .expect("first coordinator");
        let matching = registry
            .for_scope(scope, DEFAULT_TOOL_PARALLELISM)
            .expect("matching coordinator");

        assert_eq!(coordinator.capacity(), DEFAULT_TOOL_PARALLELISM);
        assert_eq!(matching.capacity(), DEFAULT_TOOL_PARALLELISM);
        assert!(registry.for_scope(scope, MAX_TOOL_PARALLELISM).is_err());
    }

    #[tokio::test]
    async fn matching_scopes_share_capacity_but_other_scopes_and_owners_are_isolated() {
        let registry = ToolConcurrencyRegistry::default();
        let independent = ToolConcurrencyRegistry::default();
        let parent = registry.for_scope("session", 1).unwrap();
        let child = registry.for_scope("session", 1).unwrap();
        let other_session = registry.for_scope("other-session", 1).unwrap();
        let other_host = independent.for_scope("session", 1).unwrap();

        let permit = parent.acquire_async().await;
        let waiting = child.acquire_async();
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        assert!(futures::poll!(std::pin::pin!(other_session.acquire_async())).is_ready());
        assert!(futures::poll!(std::pin::pin!(other_host.acquire_async())).is_ready());
        drop(permit);
        let _child_permit = waiting.await;
    }

    #[tokio::test]
    async fn remaining_executor_retains_budget_until_last_coordinator_is_released() {
        let registry = ToolConcurrencyRegistry::default();
        let first = registry.for_scope("session", 1).unwrap();
        let remaining = registry.for_scope("session", 1).unwrap();
        drop(first);
        assert!(registry.for_scope("session", 2).is_err());
        let concurrent = registry.for_scope("session", 1).unwrap();
        {
            let permit = remaining.acquire_async().await;
            let waiting = concurrent.acquire_async();
            tokio::pin!(waiting);
            assert!(futures::poll!(&mut waiting).is_pending());
            drop(permit);
            drop(waiting.await);
        }
        drop(concurrent);
        drop(remaining);

        let replacement = registry.for_scope("session", 2).unwrap();
        assert_eq!(replacement.capacity(), 2);
        let _first = replacement.acquire_async().await;
        let _second = replacement.acquire_async().await;
    }

    #[tokio::test]
    async fn dropping_registry_does_not_end_budgets_owned_by_executors() {
        let registry = ToolConcurrencyRegistry::default();
        let coordinator = registry.for_scope("session", 1).unwrap();
        drop(registry);
        let first = coordinator.acquire_async().await;
        drop(first);
        let _second = coordinator.acquire_async().await;
    }
}
