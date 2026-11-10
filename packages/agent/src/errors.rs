use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone)]
pub struct RuntimeHostError {
    code: String,
    message: String,
}

impl RuntimeHostError {
    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn unknown_command(command: &str) -> Self {
        Self::new("unknown_command", format!("unknown command: {}", command))
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::new("transport_error", message)
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
