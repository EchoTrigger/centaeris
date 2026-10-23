//! Runtime-owned live SDK contexts. Durable authority and settlement stay in PostgreSQL.
use crate::e2b_runtime::E2bPreparedHost;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_OWNED_RUNS: usize = 64;
const MAX_OWNED_ENVIRONMENTS: usize = 64;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct E2bRunBinding {
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub execution_id: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct E2bEnvironmentBinding {
    pub environment_id: String,
    pub profile_digest: String,
    pub provider_config_digest: String,
}

pub(crate) struct BoundSlot<B, T> {
    binding: B,
    // Held for the whole protected step, including waits. Independent keys
    // have separate guards; a slow create or tool never owns the map lock.
    state: Mutex<Option<T>>,
}

pub(crate) type E2bRunSlot = BoundSlot<E2bRunBinding, E2bLiveContext>;
pub(crate) type E2bEnvironmentSlot = BoundSlot<E2bEnvironmentBinding, Arc<E2bPreparedHost>>;

pub(crate) struct E2bLiveContext {
    pub host: Arc<E2bPreparedHost>,
    pub workspace_prepared: bool,
}

impl E2bLiveContext {
    pub(crate) fn new(host: Arc<E2bPreparedHost>) -> Self {
        Self {
            host,
            workspace_prepared: false,
        }
    }
}

impl<B, T> BoundSlot<B, T> {
    pub(crate) fn try_enter(&self) -> Result<Option<MutexGuard<'_, Option<T>>>, String> {
        match self.state.try_lock() {
            Ok(state) => Ok(Some(state)),
            Err(std::sync::TryLockError::WouldBlock) => Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => Err("e2b_live_context_poisoned".into()),
        }
    }
}

struct BoundedSlots<B, T> {
    slots: Mutex<HashMap<String, Arc<BoundSlot<B, T>>>>,
}

impl<B, T> Default for BoundedSlots<B, T> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<B: PartialEq, T> BoundedSlots<B, T> {
    fn get_or_insert(
        &self,
        key: &str,
        binding: B,
        capacity: usize,
    ) -> Result<Option<Arc<BoundSlot<B, T>>>, String> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| "e2b_live_registry_poisoned")?;
        if let Some(slot) = slots.get(key) {
            if slot.binding != binding {
                return Err("e2b_live_context_binding_rejected".into());
            }
            return Ok(Some(slot.clone()));
        }
        if slots.len() >= capacity {
            return Ok(None);
        }
        let slot = Arc::new(BoundSlot {
            binding,
            state: Mutex::new(None),
        });
        slots.insert(key.to_owned(), slot.clone());
        Ok(Some(slot))
    }

    fn get(
        &self,
        key: &str,
        matches: impl FnOnce(&B) -> bool,
    ) -> Result<Option<Arc<BoundSlot<B, T>>>, String> {
        let slots = self
            .slots
            .lock()
            .map_err(|_| "e2b_live_registry_poisoned")?;
        match slots.get(key) {
            Some(slot) if matches(&slot.binding) => Ok(Some(slot.clone())),
            Some(_) => Err("e2b_live_context_binding_rejected".into()),
            None => Ok(None),
        }
    }

    fn remove(&self, key: &str, matches: impl FnOnce(&B) -> bool) -> Result<(), String> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| "e2b_live_registry_poisoned")?;
        if slots.get(key).is_some_and(|slot| !matches(&slot.binding)) {
            return Err("e2b_live_context_binding_rejected".into());
        }
        slots.remove(key);
        Ok(())
    }

    fn remove_slot(&self, key: &str, expected: &Arc<BoundSlot<B, T>>) -> Result<(), String> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| "e2b_live_registry_poisoned")?;
        match slots.get(key) {
            Some(current) if Arc::ptr_eq(current, expected) => {
                slots.remove(key);
                Ok(())
            }
            Some(_) => Err("e2b_live_context_binding_rejected".into()),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
pub(crate) struct E2bRunRegistry {
    runs: BoundedSlots<E2bRunBinding, E2bLiveContext>,
    environments: BoundedSlots<E2bEnvironmentBinding, Arc<E2bPreparedHost>>,
}

impl E2bRunRegistry {
    pub(crate) fn get_or_insert(
        &self,
        binding: E2bRunBinding,
    ) -> Result<Option<Arc<E2bRunSlot>>, String> {
        self.runs
            .get_or_insert(&binding.agent_run_id.clone(), binding, MAX_OWNED_RUNS)
    }

    /// Reserve the bounded environment slot before any provider side effect.
    /// The caller holds its nonblocking guard through preparation or cleanup.
    pub(crate) fn environment(
        &self,
        binding: E2bEnvironmentBinding,
    ) -> Result<Option<Arc<E2bEnvironmentSlot>>, String> {
        self.environments.get_or_insert(
            &binding.environment_id.clone(),
            binding,
            MAX_OWNED_ENVIRONMENTS,
        )
    }

    /// Control work must not reserve ordinary live-context capacity merely to
    /// prove that a never-admitted Run had no provider effects.
    pub(crate) fn existing_environment(
        &self,
        binding: &E2bEnvironmentBinding,
    ) -> Result<Option<Arc<E2bEnvironmentSlot>>, String> {
        self.environments
            .get(&binding.environment_id, |current| current == binding)
    }

    /// Observe only the original Run context; never substitute the latest
    /// environment anchor for that Run's command and helper evidence.
    pub(crate) fn current_run(
        &self,
        run: &str,
        digest: &str,
    ) -> Result<Option<Arc<E2bRunSlot>>, String> {
        self.runs
            .get(run, |binding| binding.authorization_digest == digest)
    }

    /// The caller must still hold this slot's guard and possess store-confirmed
    /// exact retirement, or confirmed NoEffects/NotDispatched with an empty slot.
    /// A stale slot cannot erase a replacement context for the same environment.
    pub(crate) fn remove_environment(
        &self,
        binding: &E2bEnvironmentBinding,
        expected: &Arc<E2bEnvironmentSlot>,
    ) -> Result<(), String> {
        if &expected.binding != binding {
            return Err("e2b_live_context_binding_rejected".into());
        }
        self.environments
            .remove_slot(&binding.environment_id, expected)
    }

    /// Call only after the store has confirmed this Run's provider cleanup.
    pub(crate) fn remove_settled(&self, run: &str, digest: &str) -> Result<(), String> {
        self.runs
            .remove(run, |binding| binding.authorization_digest == digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(run: &str) -> E2bRunBinding {
        E2bRunBinding {
            agent_run_id: run.into(),
            authorization_digest: format!("digest:{run}"),
            execution_id: format!("execution:{run}"),
        }
    }

    #[test]
    fn duplicate_step_cannot_enter_the_owned_context_but_other_runs_can_progress() {
        let registry = E2bRunRegistry::default();
        let first = registry.get_or_insert(binding("a")).unwrap().unwrap();
        let held = first.try_enter().unwrap().unwrap();
        let same = registry.get_or_insert(binding("a")).unwrap().unwrap();
        let duplicate_admitted = same.try_enter().unwrap().is_some();
        let other = registry.get_or_insert(binding("b")).unwrap().unwrap();
        assert!(other.try_enter().unwrap().is_some());
        drop(held);
        assert!(
            !duplicate_admitted,
            "one live context must own the whole step"
        );
        assert!(same.try_enter().unwrap().is_some());
    }

    #[test]
    fn changed_authorization_or_execution_cannot_replace_existing_live_evidence() {
        let registry = E2bRunRegistry::default();
        registry.get_or_insert(binding("a")).unwrap().unwrap();
        for field in ["authorization", "execution"] {
            let mut changed = binding("a");
            if field == "authorization" {
                changed.authorization_digest = "another-digest".into();
            } else {
                changed.execution_id = "another-execution".into();
            }
            assert!(registry.get_or_insert(changed).is_err(), "{field}");
        }
    }

    #[test]
    fn full_registry_applies_admission_pressure_until_exact_settled_run_is_removed() {
        let registry = E2bRunRegistry::default();
        for index in 0..MAX_OWNED_RUNS {
            registry
                .get_or_insert(binding(&index.to_string()))
                .unwrap()
                .unwrap();
        }
        assert!(registry.get_or_insert(binding("excess")).unwrap().is_none());
        assert!(registry.remove_settled("0", "wrong-digest").is_err());
        assert!(registry.get_or_insert(binding("excess")).unwrap().is_none());
        registry.remove_settled("0", "digest:0").unwrap();
        assert!(registry.get_or_insert(binding("excess")).unwrap().is_some());
        registry.remove_settled("0", "digest:0").unwrap();
    }

    fn environment_binding(environment: &str) -> E2bEnvironmentBinding {
        E2bEnvironmentBinding {
            environment_id: environment.into(),
            profile_digest: "profile-frozen".into(),
            provider_config_digest: "provider-frozen".into(),
        }
    }

    #[test]
    fn shared_environment_slot_preserves_payload_and_excludes_other_consumers() {
        let contexts: BoundedSlots<E2bEnvironmentBinding, String> = BoundedSlots::default();
        let first = contexts
            .get_or_insert("environment", environment_binding("environment"), 2)
            .unwrap()
            .unwrap();
        *first.try_enter().unwrap().unwrap() = Some("original live SDK context".into());
        let held = first.try_enter().unwrap().unwrap();
        let successor = contexts
            .get_or_insert("environment", environment_binding("environment"), 2)
            .unwrap()
            .unwrap();
        assert!(successor.try_enter().unwrap().is_none());
        let independent = contexts
            .get_or_insert("other", environment_binding("other"), 2)
            .unwrap()
            .unwrap();
        assert!(independent.try_enter().unwrap().is_some());
        drop(held);
        assert_eq!(
            successor.try_enter().unwrap().unwrap().as_deref(),
            Some("original live SDK context")
        );
    }

    #[test]
    fn frozen_environment_profile_and_provider_cannot_replace_owned_context() {
        let registry = E2bRunRegistry::default();
        registry
            .environment(environment_binding("environment"))
            .unwrap()
            .unwrap();
        for changed in ["profile", "provider"] {
            let mut binding = environment_binding("environment");
            if changed == "profile" {
                binding.profile_digest = "different-profile".into();
            } else {
                binding.provider_config_digest = "different-provider".into();
            }
            assert!(registry.environment(binding).is_err(), "{changed}");
        }
    }

    #[test]
    fn environment_capacity_is_reserved_while_payload_is_still_absent() {
        let registry = E2bRunRegistry::default();
        for index in 0..MAX_OWNED_ENVIRONMENTS {
            registry
                .environment(environment_binding(&index.to_string()))
                .unwrap()
                .unwrap();
        }
        assert!(registry
            .environment(environment_binding("excess"))
            .unwrap()
            .is_none());
        assert!(registry
            .environment(environment_binding("0"))
            .unwrap()
            .is_some());
        assert!(registry
            .existing_environment(&environment_binding("excess"))
            .unwrap()
            .is_none());
        assert!(registry
            .existing_environment(&environment_binding("0"))
            .unwrap()
            .is_some());
    }

    #[test]
    fn settled_run_removal_preserves_the_environment_slot_for_a_successor() {
        let registry = E2bRunRegistry::default();
        let original = registry
            .environment(environment_binding("environment"))
            .unwrap()
            .unwrap();
        registry.get_or_insert(binding("a")).unwrap().unwrap();
        registry.remove_settled("a", "digest:a").unwrap();
        assert!(registry.current_run("a", "digest:a").unwrap().is_none());
        let successor = registry
            .environment(environment_binding("environment"))
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&original, &successor));
        registry.get_or_insert(binding("b")).unwrap().unwrap();
        assert!(registry.current_run("b", "digest:a").is_err());
        assert!(registry.current_run("b", "digest:b").unwrap().is_some());
    }

    #[test]
    fn stale_environment_slot_cannot_remove_a_replacement_or_a_different_binding() {
        let registry = E2bRunRegistry::default();
        let binding = environment_binding("environment");
        let old = registry.environment(binding.clone()).unwrap().unwrap();
        let held = old.try_enter().unwrap().unwrap();
        assert!(held.is_none());
        let mut changed = binding.clone();
        changed.profile_digest = "different-profile".into();
        assert!(registry.remove_environment(&changed, &old).is_err());
        registry.remove_environment(&binding, &old).unwrap();
        let replacement = registry.environment(binding.clone()).unwrap().unwrap();
        assert!(registry.remove_environment(&binding, &old).is_err());
        assert!(Arc::ptr_eq(
            &replacement,
            &registry.environment(binding).unwrap().unwrap()
        ));
        drop(held);
    }
}
