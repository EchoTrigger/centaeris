use std::sync::LazyLock;

use centaeris_core::extension::verify_mcp_declaration_file;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent_run_authorization::canonical_json;
use crate::native_run_execution_authority::{with_run_authority, NativeRunExecutionAuthority};

use super::McpConnectorIdentity;

static AUTHORITY_SQL: LazyLock<String> = LazyLock::new(|| {
    with_run_authority(include_str!(
        "../../../api/app_core/native_mcp_dispatch_authority.sql"
    ))
});

pub(super) fn verify(
    authority: &NativeRunExecutionAuthority,
    connector: &McpConnectorIdentity,
    credential_ref: &str,
    connected_binding: &str,
) -> Result<(), String> {
    // Read the signed declaration again at dispatch. A successful connection
    // does not grant access after its package resource disappears or changes.
    verify_mcp_declaration_file(
        &connector.package_root,
        &connector.resource_path,
        &connector.resource_digest,
    )
    .map_err(|_| "MCP connector resource verification failed")?;

    let rows = authority
        .query(
            AUTHORITY_SQL.as_str(),
            &[
                &connector.plugin_name,
                &connector.resource_path,
                &connector.resource_digest,
                &credential_ref,
            ],
        )
        .map_err(|_| "authorize MCP connector request failed")?;
    let [row] = rows.as_slice() else {
        return Err("MCP connector dispatch authority denied".into());
    };
    let credential_id: &str = row
        .try_get(0)
        .map_err(|_| "MCP connector authorization response invalid")?;
    let credential_version: i64 = row
        .try_get(1)
        .map_err(|_| "MCP connector authorization response invalid")?;
    // The connect response pins Django's existing binding digest. Its
    // canonical ASCII inputs have identical JSON encoding in both consumers.
    let binding = canonical_json(json!({
        "scope": "workspace",
        "workspace": authority.workspace_id(),
        "plugin": &connector.plugin_name,
        "server": &connector.server_id,
        "resourcePath": &connector.resource_path,
        "resourceDigest": &connector.resource_digest,
        "credential": credential_id,
        "credentialVersion": credential_version,
    }))
    .map_err(|_| "MCP connector authorization response invalid")?;
    if format!("sha256:{:x}", Sha256::digest(binding.as_bytes())) != connected_binding {
        return Err("MCP connector binding changed".into());
    }
    Ok(())
}
