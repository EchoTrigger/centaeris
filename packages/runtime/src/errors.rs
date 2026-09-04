use serde_json::json;
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeHostError {
    code: String,
    message: String,
}

impl RuntimeHostError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub(crate) fn unknown_command(command: &str) -> Self {
        Self::new("unknown_command", format!("unknown command: {}", command))
    }

    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Self::new("transport_error", message)
    }

    pub(crate) fn to_runtime_rpc_error(&self) -> crate::runtime_rpc::RuntimeRpcError {
        let error = match self.code.as_str() {
            "invalid_request" => {
                crate::runtime_rpc::RuntimeRpcError::invalid_request(self.message.as_str())
            }
            "unknown_command" => {
                crate::runtime_rpc::RuntimeRpcError::method_not_found(self.message.as_str())
            }
            _ => crate::runtime_rpc::RuntimeRpcError::internal_error(self.message.as_str()),
        };
        error.with_data(json!({
            "code": self.code.as_str(),
            "message": self.message.as_str(),
        }))
    }
}

impl Display for RuntimeHostError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for RuntimeHostError {}

impl From<serde_json::Error> for RuntimeHostError {
    fn from(error: serde_json::Error) -> Self {
        Self::invalid_request(format!("json parse failed: {}", error))
    }
}
