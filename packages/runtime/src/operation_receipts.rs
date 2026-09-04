use crate::{atomic_file, user_data_layout};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

const OPERATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_OPERATION_ID_BYTES: usize = 128;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OperationReceipt {
    schema_version: u32,
    method: String,
    operation_id: String,
    pub(crate) request_digest: String,
    pub(crate) result: Value,
}

pub(crate) fn request_digest<T: Serialize>(request: &T) -> Result<String, String> {
    let encoded = serde_json::to_vec(request)
        .map_err(|error| format!("serialize operation request digest input failed: {error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}

pub(crate) fn deserialize_operation_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty()
        || value.len() > MAX_OPERATION_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(serde::de::Error::custom(
            "operationId must be 1-128 ASCII letters, digits, hyphens, underscores, dots, or colons",
        ));
    }
    Ok(value)
}

pub(crate) fn deterministic_identity(prefix: &str, method: &str, operation_id: &str) -> String {
    let digest = Sha256::digest(format!("centaeris:{method}\0{operation_id}").as_bytes());
    format!("{prefix}{}", hex_prefix(digest.as_slice(), 16))
}

pub(crate) fn read(method: &str, operation_id: &str) -> Result<Option<OperationReceipt>, String> {
    let path = receipt_path(method, operation_id);
    if !path.exists() {
        return Ok(None);
    }
    if !path.is_file() {
        return Err(format!(
            "operation receipt path is not a file: {}",
            path.display()
        ));
    }
    let raw = fs::read(path.as_path())
        .map_err(|error| format!("read operation receipt failed {}: {error}", path.display()))?;
    let receipt = serde_json::from_slice::<OperationReceipt>(raw.as_slice()).map_err(|error| {
        format!(
            "decode operation receipt failed {}: {error}",
            path.display()
        )
    })?;
    if receipt.schema_version != OPERATION_RECEIPT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported operation receipt schemaVersion: {}",
            receipt.schema_version
        ));
    }
    if receipt.method != method || receipt.operation_id != operation_id {
        return Err("operation receipt identity mismatch".to_string());
    }
    Ok(Some(receipt))
}

pub(crate) fn write(
    method: &str,
    operation_id: &str,
    request_digest: String,
    result: Value,
) -> Result<(), String> {
    let receipt = OperationReceipt {
        schema_version: OPERATION_RECEIPT_SCHEMA_VERSION,
        method: method.to_string(),
        operation_id: operation_id.to_string(),
        request_digest,
        result,
    };
    let mut encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("serialize operation receipt failed: {error}"))?;
    encoded.push(b'\n');
    atomic_file::write_file_atomically(
        receipt_path(method, operation_id).as_path(),
        encoded.as_slice(),
        "operation receipt",
    )
}

fn receipt_path(method: &str, operation_id: &str) -> PathBuf {
    let identity = Sha256::digest(format!("{method}\0{operation_id}").as_bytes());
    user_data_layout::runtime_operation_receipts_dir_path()
        .join(method.replace('/', "-"))
        .join(format!("{}.json", hex_prefix(identity.as_slice(), 32)))
}

fn hex_prefix(bytes: &[u8], length: usize) -> String {
    let mut result = String::with_capacity(length * 2);
    for byte in bytes.iter().take(length) {
        use std::fmt::Write as _;
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }
    result
}
