use crate::{atomic_file, user_data_layout};
pub use centaeris_agent::OperationId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const OPERATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
const PROMPT_METHOD: &str = "session/prompt";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OperationReceipt {
    schema_version: u32,
    method: String,
    operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_user_prompt: Option<bool>,
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
    OperationId::deserialize(deserializer).map(|operation| operation.as_str().to_owned())
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
    write_receipt(method, operation_id, request_digest, result, None)
}

pub(crate) fn write_prompt(
    operation_id: &str,
    request_digest: String,
    result: Value,
    external_user_prompt: bool,
) -> Result<(), String> {
    write_receipt(
        PROMPT_METHOD,
        operation_id,
        request_digest,
        result,
        Some(external_user_prompt),
    )
}

fn write_receipt(
    method: &str,
    operation_id: &str,
    request_digest: String,
    result: Value,
    external_user_prompt: Option<bool>,
) -> Result<(), String> {
    let receipt = OperationReceipt {
        schema_version: OPERATION_RECEIPT_SCHEMA_VERSION,
        method: method.to_string(),
        operation_id: operation_id.to_string(),
        external_user_prompt,
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

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForkOpenings {
    session_id: String,
    openings: Vec<(String, String)>,
}

pub(crate) fn persist_fork_external_prompt_openings(
    session_id: &str,
    pairs: &HashSet<(String, String)>,
) -> Result<(), String> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut openings: Vec<_> = pairs.iter().cloned().collect();
    openings.sort_unstable();
    let encoded = serde_json::to_vec(&ForkOpenings {
        session_id: session_id.to_string(),
        openings,
    })
    .map_err(|error| error.to_string())?;
    atomic_file::write_file_atomically(
        &receipt_path("fork-prompt-openings", session_id),
        &encoded,
        "inherited prompt origins",
    )
}

pub(crate) fn external_prompt_openings(
    session_id: &str,
) -> Result<HashSet<(String, String)>, String> {
    let inherited_path = receipt_path("fork-prompt-openings", session_id);
    let mut openings = match fs::read(inherited_path) {
        Ok(bytes) => {
            let inherited: ForkOpenings = serde_json::from_slice(&bytes)
                .map_err(|error| format!("Decode inherited prompt origins failed: {error}"))?;
            if inherited.session_id != session_id {
                return Err("Inherited prompt origins Session mismatch".into());
            }
            inherited.openings.into_iter().collect()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
        Err(error) => return Err(format!("Read inherited prompt origins failed: {error}")),
    };
    let directory = user_data_layout::runtime_operation_receipts_dir_path().join("session-prompt");
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(openings),
        Err(error) => return Err(format!("Read prompt receipts failed: {error}")),
    };
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes =
            fs::read(&path).map_err(|error| format!("Read prompt receipt failed: {error}"))?;
        let receipt: OperationReceipt = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Decode prompt receipt failed: {error}"))?;
        if receipt.result["sessionId"].as_str() != Some(session_id) {
            continue;
        }
        if receipt.method != PROMPT_METHOD
            || receipt.schema_version != OPERATION_RECEIPT_SCHEMA_VERSION
            || path != receipt_path(PROMPT_METHOD, &receipt.operation_id)
        {
            return Err("Prompt receipt identifier mismatch".into());
        }
        // Legacy cet generated a bare 128-bit lowercase hexadecimal ID.
        // Background starts used operation identities namespaced for their source.
        let external = receipt.external_user_prompt.unwrap_or_else(|| {
            receipt.operation_id.len() == 32
                && receipt
                    .operation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if external {
            let run = receipt.result["agentRunId"]
                .as_str()
                .ok_or("Prompt receipt missing agentRunId")?;
            let turn = receipt.result["turnId"]
                .as_str()
                .ok_or("Prompt receipt missing turnId")?;
            if run != deterministic_identity("agent-run-", PROMPT_METHOD, &receipt.operation_id)
                || turn != deterministic_identity("turn-", PROMPT_METHOD, &receipt.operation_id)
            {
                return Err("Prompt receipt run or turn identifier mismatch".into());
            }
            openings.insert((run.to_string(), turn.to_string()));
        }
    }
    Ok(openings)
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
