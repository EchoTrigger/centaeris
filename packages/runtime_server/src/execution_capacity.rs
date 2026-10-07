#[derive(Clone, Copy)]
pub(crate) struct ExecutionCapacity {
    pub(crate) global: usize,
    pub(crate) tenant: usize,
}

impl ExecutionCapacity {
    pub(crate) fn from_env() -> Result<Self, String> {
        Ok(Self {
            global: crate::request_capacity::capacity("EXECUTION_GLOBAL_LIMIT", 8)?,
            tenant: crate::request_capacity::capacity("EXECUTION_TENANT_LIMIT", 4)?,
        })
    }
}
