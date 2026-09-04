use crate::errors::RuntimeHostError;
use crate::runtime_rpc::RuntimeRpcRequest;

#[derive(Debug, Clone)]
pub(crate) struct HostCommandRequest {
    pub(crate) command: String,
    pub(crate) payload: serde_json::Value,
}

impl TryFrom<RuntimeRpcRequest> for HostCommandRequest {
    type Error = RuntimeHostError;

    fn try_from(request: RuntimeRpcRequest) -> Result<Self, Self::Error> {
        request.validate().map_err(|error| {
            RuntimeHostError::invalid_request(format!("invalid rpc request: {error}"))
        })?;
        if request.method.trim().is_empty() {
            return Err(RuntimeHostError::invalid_request("rpc method is required"));
        }
        Ok(Self {
            command: request.method,
            payload: normalize_params(request.params),
        })
    }
}

fn normalize_params(params: serde_json::Value) -> serde_json::Value {
    if params.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        params
    }
}
