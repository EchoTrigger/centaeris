use serde::{Deserialize, Serialize};

const MAX_OPERATION_ID_BYTES: usize = 128;

/// An operation identity validated once before admission, for native and RPC callers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OperationId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > MAX_OPERATION_ID_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err("operationId must be 1-128 ASCII letters, digits, hyphens, underscores, dots, or colons".to_string());
        }
        Ok(Self(value))
    }
}

impl TryFrom<&str> for OperationId {
    type Error = String;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for OperationId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
