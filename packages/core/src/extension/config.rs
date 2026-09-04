use std::collections::HashSet;

pub trait PluginConfigStore {
    fn disabled_ids(&self) -> Result<HashSet<String>, String>;
    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String>;
}
