use super::{ModelSessionConfig, ModelSessionConfigStore};

#[derive(Debug, Default)]
pub struct EmptyModelSessionConfigStore;

impl EmptyModelSessionConfigStore {
    pub fn new() -> Self {
        Self
    }
}

impl ModelSessionConfigStore for EmptyModelSessionConfigStore {
    fn get_session_config(&self, _session_id: &str) -> Result<Option<ModelSessionConfig>, String> {
        Ok(None)
    }
}
