pub use centaeris_agent::RuntimeHostError;
use serde_json::json;

pub(crate) fn to_runtime_rpc_error(
    error: &RuntimeHostError,
) -> crate::runtime_rpc::RuntimeRpcError {
    let rpc_error = match error.code() {
        "invalid_request" => crate::runtime_rpc::RuntimeRpcError::invalid_request(error.message()),
        "unknown_command" => crate::runtime_rpc::RuntimeRpcError::method_not_found(error.message()),
        _ => crate::runtime_rpc::RuntimeRpcError::internal_error(error.message()),
    };
    rpc_error.with_data(json!({
        "code": error.code(),
        "message": error.message(),
    }))
}
