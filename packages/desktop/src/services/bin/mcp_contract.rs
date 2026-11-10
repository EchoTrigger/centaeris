use centaeris_core::extension::{
    mcp_model_contract_digest, McpServersFileV1, McpToolDeclarationV1,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

mod atomic_file {
    include!("../atomic_file.rs");
}

#[derive(Clone, Copy)]
enum Mode {
    Check,
    Write,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<(Mode, Vec<PathBuf>), String> {
    let mut args = args.into_iter();
    let mode = match args.next().as_deref() {
        Some("--check") => Mode::Check,
        Some("--write") => Mode::Write,
        _ => return Err("usage: centaeris-mcp-contract (--check|--write) <file>...".to_string()),
    };
    let paths = args.map(PathBuf::from).collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("at least one MCP declaration file is required".to_string());
    }
    Ok((mode, paths))
}

fn process(path: &Path, mode: Mode) -> Result<(), String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("read MCP declaration failed {}: {error}", path.display()))?;
    let mut value = serde_json::from_str::<Value>(raw.as_str())
        .map_err(|error| format!("parse MCP declaration failed {}: {error}", path.display()))?;
    let servers = value
        .get_mut("servers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "MCP declaration servers must be an array".to_string())?;
    for server in servers {
        let server = server
            .as_object_mut()
            .ok_or_else(|| "MCP server declaration must be an object".to_string())?;
        let server_id = server
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "MCP server id is required".to_string())?;
        let tools = serde_json::from_value::<Vec<McpToolDeclarationV1>>(
            server
                .get("tools")
                .cloned()
                .ok_or_else(|| "MCP server tools are required".to_string())?,
        )
        .map_err(|error| format!("parse MCP tools failed for serverId={server_id}: {error}"))?;
        let digest = mcp_model_contract_digest(server_id, tools.as_slice())?;
        match mode {
            Mode::Check
                if server.get("modelContractDigest").and_then(Value::as_str)
                    != Some(digest.as_str()) =>
            {
                return Err(format!(
                    "MCP modelContractDigest mismatch: serverId={server_id}"
                ));
            }
            Mode::Check => {}
            Mode::Write => {
                server.insert("modelContractDigest".to_string(), Value::String(digest));
            }
        }
    }
    let declaration = serde_json::from_value::<McpServersFileV1>(value.clone())
        .map_err(|error| format!("parse strict MCP declaration failed: {error}"))?;
    declaration.validate()?;
    if matches!(mode, Mode::Write) {
        let mut encoded = serde_json::to_vec_pretty(&value)
            .map_err(|error| format!("encode MCP declaration failed: {error}"))?;
        encoded.push(b'\n');
        atomic_file::write_file_atomically(path, encoded.as_slice(), "MCP declaration")?;
    }
    Ok(())
}

fn sync(path: &Path, snapshots: &[(String, PathBuf)]) -> Result<(), String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("read MCP declaration failed {}: {error}", path.display()))?;
    let mut value = serde_json::from_str::<Value>(raw.as_str())
        .map_err(|error| format!("parse MCP declaration failed {}: {error}", path.display()))?;
    let mut snapshots = snapshots.iter().cloned().collect::<HashMap<_, _>>();
    let servers = value
        .get_mut("servers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "MCP declaration servers must be an array".to_string())?;
    for server in servers {
        let server = server
            .as_object_mut()
            .ok_or_else(|| "MCP server declaration must be an object".to_string())?;
        let server_id = server
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "MCP server id is required".to_string())?
            .to_string();
        let snapshot_path = snapshots
            .remove(&server_id)
            .ok_or_else(|| format!("missing tools/list snapshot: serverId={server_id}"))?;
        let snapshot =
            serde_json::from_slice::<Value>(&fs::read(&snapshot_path).map_err(|error| {
                format!(
                    "read tools/list snapshot failed {}: {error}",
                    snapshot_path.display()
                )
            })?)
            .map_err(|error| {
                format!(
                    "parse tools/list snapshot failed {}: {error}",
                    snapshot_path.display()
                )
            })?;
        let result = snapshot
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                format!("tools/list snapshot result is missing: serverId={server_id}")
            })?;
        if result
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null())
        {
            return Err(format!(
                "tools/list snapshot is paginated: serverId={server_id}"
            ));
        }
        let live_tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("tools/list result.tools is missing: serverId={server_id}"))?;
        let declared_tools = server
            .get_mut("tools")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| format!("declared tools are missing: serverId={server_id}"))?;
        let declared_source_names = declared_tools
            .iter()
            .map(|tool| {
                tool.get("sourceName")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("declared sourceName is missing: serverId={server_id}"))
            })
            .collect::<Result<HashSet<_>, _>>()?;
        let mut live_by_name = HashMap::with_capacity(live_tools.len());
        for live in live_tools {
            let source_name = live
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("tools/list sourceName is missing: serverId={server_id}"))?;
            if live_by_name.insert(source_name, live).is_some() {
                return Err(format!(
                    "tools/list sourceName must occur exactly once: serverId={server_id} sourceName={source_name}"
                ));
            }
            if !declared_source_names.contains(source_name) {
                return Err(format!(
                    "unknown tools/list sourceName: serverId={server_id} sourceName={source_name}"
                ));
            }
        }
        for declared in declared_tools.iter_mut() {
            let declared = declared.as_object_mut().ok_or_else(|| {
                format!("declared MCP tool must be an object: serverId={server_id}")
            })?;
            let source_name = declared
                .get("sourceName")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("declared sourceName is missing: serverId={server_id}"))?;
            let live = live_by_name.get(source_name).ok_or_else(|| {
                format!(
                    "declared MCP tool is missing from tools/list: serverId={server_id} sourceName={source_name}"
                )
            })?;
            let description = live
                .get("description")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "tools/list description is missing: serverId={server_id} sourceName={source_name}"
                    )
                })?
                .to_string();
            let input_schema = live
                .get("inputSchema")
                .filter(|schema| schema.is_object())
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "tools/list inputSchema is missing: serverId={server_id} sourceName={source_name}"
                    )
                })?;
            declared.insert("description".to_string(), Value::String(description));
            declared.insert("inputSchema".to_string(), input_schema);
        }
        declared_tools.sort_by(|left, right| {
            left.get("sourceName")
                .and_then(Value::as_str)
                .cmp(&right.get("sourceName").and_then(Value::as_str))
        });
        let tools = serde_json::from_value::<Vec<McpToolDeclarationV1>>(Value::Array(
            declared_tools.clone(),
        ))
        .map_err(|error| {
            format!("parse synchronized tools failed: serverId={server_id}: {error}")
        })?;
        server.insert(
            "modelContractDigest".to_string(),
            Value::String(mcp_model_contract_digest(&server_id, tools.as_slice())?),
        );
    }
    if !snapshots.is_empty() {
        let mut ids = snapshots.keys().map(|id| id.as_str()).collect::<Vec<_>>();
        ids.sort_unstable();
        return Err(format!("unknown tools/list snapshots: {}", ids.join(",")));
    }
    let declaration = serde_json::from_value::<McpServersFileV1>(value.clone())
        .map_err(|error| format!("parse strict MCP declaration failed: {error}"))?;
    declaration.validate()?;
    let mut encoded = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("encode MCP declaration failed: {error}"))?;
    encoded.push(b'\n');
    atomic_file::write_file_atomically(path, encoded.as_slice(), "MCP declaration")
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--sync") {
        let path = args
            .get(1)
            .map(PathBuf::from)
            .ok_or_else(|| "--sync requires an MCP declaration file".to_string())?;
        let mut ids = HashSet::new();
        let snapshots = args
            .iter()
            .skip(2)
            .map(|binding| {
                let (server_id, snapshot) = binding
                    .split_once('=')
                    .ok_or_else(|| "snapshot binding must be serverId=path".to_string())?;
                if server_id.is_empty() || snapshot.is_empty() || !ids.insert(server_id.to_string())
                {
                    return Err(
                        "snapshot bindings must use unique non-empty serverId=path".to_string()
                    );
                }
                Ok((server_id.to_string(), PathBuf::from(snapshot)))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if snapshots.is_empty() {
            return Err("--sync requires at least one serverId=path snapshot".to_string());
        }
        sync(path.as_path(), snapshots.as_slice())?;
        println!("{}: ok", path.display());
        return Ok(());
    }
    let (mode, paths) = parse_args(args)?;
    for path in paths {
        process(path.as_path(), mode)?;
        println!("{}: ok", path.display());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("centaeris-mcp-contract: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_then_checks_all_server_digests_offline() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-mcp-contract-{}-{}.json",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        fs::write(
            &path,
            r#"{"schema":"mcp_servers_v1","servers":[{"id":"banana-source","transport":{"type":"stdio","program":"bin/banana","args":[]},"lifecycle":"auto","startupTimeoutMs":1000,"toolTimeoutMs":1000,"tools":[{"sourceName":"search","name":"banana_search","description":"Search bananas.","inputSchema":{"type":"object"},"concurrencySafe":true,"scopes":[]}]}]}"#,
        )
        .expect("fixture");

        process(path.as_path(), Mode::Write).expect("write digest");
        process(path.as_path(), Mode::Check).expect("check digest");
        let value: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert!(value["servers"][0]["modelContractDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")));
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn sync_requires_and_copies_the_exact_live_tools_list() {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        );
        let declaration_path =
            std::env::temp_dir().join(format!("centaeris-mcp-sync-{suffix}.json"));
        let snapshot_path =
            std::env::temp_dir().join(format!("centaeris-mcp-tools-list-{suffix}.json"));
        fs::write(
            &declaration_path,
            r#"{"schema":"mcp_servers_v1","servers":[{"id":"banana-source","transport":{"type":"stdio","program":"bin/banana","args":[]},"lifecycle":"auto","startupTimeoutMs":1000,"toolTimeoutMs":1000,"tools":[{"sourceName":"search","name":"banana_search","concurrencySafe":true,"scopes":[]}]}]}"#,
        )
        .expect("declaration fixture");
        fs::write(
            &snapshot_path,
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"search","description":"Search exact bananas.\nPreserve this line.","inputSchema":{"required":["query","filters"],"properties":{"filters":{"type":"array","items":{"type":"string"}},"query":{"type":"string"}},"type":"object"}},{"name":"extra","description":"Ignore me.","inputSchema":{"type":"object"}}]}}"#,
        )
        .expect("tools/list fixture");

        let original = fs::read(&declaration_path).expect("read original declaration");
        assert!(sync(
            declaration_path.as_path(),
            &[("banana-source".to_string(), snapshot_path.clone())],
        )
        .expect_err("reject extra live tool")
        .contains("unknown tools/list sourceName"));
        assert_eq!(
            fs::read(&declaration_path).expect("read rejected declaration"),
            original
        );
        fs::write(
            &snapshot_path,
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"search","description":"Search exact bananas.\nPreserve this line.","inputSchema":{"required":["query","filters"],"properties":{"filters":{"type":"array","items":{"type":"string"}},"query":{"type":"string"}},"type":"object"}}]}}"#,
        )
        .expect("exact tools/list fixture");
        sync(
            declaration_path.as_path(),
            &[("banana-source".to_string(), snapshot_path.clone())],
        )
        .expect("sync exact set");
        process(declaration_path.as_path(), Mode::Check).expect("check synchronized contract");
        let value: Value = serde_json::from_slice(
            &fs::read(&declaration_path).expect("read synchronized declaration"),
        )
        .expect("synchronized JSON");
        assert_eq!(
            value["servers"][0]["tools"][0]["description"],
            "Search exact bananas.\nPreserve this line."
        );
        assert_eq!(
            value["servers"][0]["tools"][0]["inputSchema"]["required"],
            serde_json::json!(["query", "filters"])
        );
        assert_eq!(value["servers"][0]["tools"].as_array().unwrap().len(), 1);
        fs::remove_file(declaration_path).expect("cleanup declaration");
        fs::remove_file(snapshot_path).expect("cleanup snapshot");
    }
}
