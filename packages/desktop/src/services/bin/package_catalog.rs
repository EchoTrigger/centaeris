use centaeris_core::extension::{
    build_plugin_activation_snapshot, load_mcp_servers_file, load_plugin_manifest_file,
};
use std::fs;
use std::path::{Path, PathBuf};

fn parse_args(
    args: impl IntoIterator<Item = String>,
) -> Result<(Vec<PathBuf>, Option<PathBuf>), String> {
    let mut roots = Vec::new();
    let mut output = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--output" {
            if output.is_some() {
                return Err("--output may be specified only once".to_string());
            }
            output = Some(PathBuf::from(
                args.next()
                    .ok_or_else(|| "--output requires a path".to_string())?,
            ));
        } else if arg.starts_with('-') {
            return Err(format!("unknown argument: {arg}"));
        } else {
            roots.push(PathBuf::from(arg));
        }
    }
    if roots.is_empty() {
        return Err("at least one package root is required".to_string());
    }
    Ok((roots, output))
}

fn build_catalog(roots: &[PathBuf]) -> Result<String, String> {
    for root in roots {
        let metadata = fs::symlink_metadata(root)
            .map_err(|error| format!("inspect package root failed {}: {error}", root.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "package root must not be a symlink: {}",
                root.display()
            ));
        }
    }
    let snapshot = build_plugin_activation_snapshot(roots)?;
    for root in roots {
        let manifest =
            load_plugin_manifest_file(root.join(".centaeris-plugin/plugin.json").as_path())?;
        for path in manifest.paths.mcp_servers {
            load_mcp_servers_file(root.join(path).as_path())?;
        }
    }
    serde_json::to_string_pretty(&snapshot)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("encode package catalog failed: {error}"))
}

fn write_catalog(output: Option<&Path>, json: &str) -> Result<(), String> {
    match output {
        Some(path) => fs::write(path, json)
            .map_err(|error| format!("write package catalog failed {}: {error}", path.display())),
        None => {
            print!("{json}");
            Ok(())
        }
    }
}

fn run() -> Result<(), String> {
    let (roots, output) = parse_args(std::env::args().skip(1))?;
    let json = build_catalog(&roots)?;
    write_catalog(output.as_deref(), json.as_str())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("centaeris-package-catalog: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_rejects_incomplete_or_stale_mcp_contracts_offline() {
        use centaeris_core::extension::{mcp_model_contract_digest, McpToolDeclarationV1};
        use serde_json::json;

        let root =
            std::env::temp_dir().join(format!("centaeris-catalog-mcp-{}", std::process::id()));
        fs::create_dir_all(root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(root.join("mcp")).unwrap();
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"banana","version":"1.0.0","paths":{"mcpServers":["mcp/tools.json"]}}"#,
        )
        .unwrap();
        let tools = vec![McpToolDeclarationV1 {
            source_name: "search".into(),
            name: "banana_search".into(),
            description: "Search bananas.".into(),
            input_schema: json!({"type":"object"}),
            concurrency_safe: true,
            scopes: vec![],
        }];
        let valid = json!({"schema":"mcp_servers_v1","servers":[{
            "id":"banana-source", "modelContractDigest":mcp_model_contract_digest("banana-source", &tools).unwrap(),
            "transport":{"type":"streamableHttp","url":"https://banana.invalid/mcp"},
            "lifecycle":"initialize","startupTimeoutMs":1000,"toolTimeoutMs":1000,"tools":tools
        }]});
        let path = root.join("mcp/tools.json");
        fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
        assert!(build_catalog(std::slice::from_ref(&root)).is_ok());
        for field in ["description", "inputSchema", "modelContractDigest"] {
            let mut invalid = valid.clone();
            let server = &mut invalid["servers"][0];
            let object = if field == "modelContractDigest" {
                server
            } else {
                &mut server["tools"][0]
            };
            object.as_object_mut().unwrap().remove(field);
            fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(build_plugin_activation_snapshot(std::slice::from_ref(&root)).is_ok());
            assert!(build_catalog(std::slice::from_ref(&root))
                .unwrap_err()
                .contains(&format!("missing field `{field}`")));
        }
        let mut tampered = valid;
        tampered["servers"][0]["tools"][0]["description"] = "Changed without repinning.".into();
        fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert_eq!(
            build_catalog(std::slice::from_ref(&root)).unwrap_err(),
            "MCP modelContractDigest mismatch"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builds_exact_v1_catalog_from_banana_package() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-package-catalog-banana-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"banana","version":"1.0.0","paths":{}}"#,
        )
        .expect("manifest");

        let json = build_catalog(std::slice::from_ref(&root)).expect("catalog");
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["schema"], "plugin_activation_snapshot_v1");
        assert_eq!(value["packages"][0]["name"], "banana");

        fs::remove_dir_all(root).expect("cleanup");
    }
}
