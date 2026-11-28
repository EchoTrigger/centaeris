use std::fs;
use std::process::Command;

#[test]
fn writes_contract_digest_to_bare_relative_declaration_path() {
    let directory = tempfile::tempdir().unwrap();
    let filename = "mcp-servers.json";
    fs::write(
        directory.path().join(filename),
        r#"{
            "schema": "mcp_servers_v1",
            "servers": [{
                "id": "banana-source",
                "transport": {"type": "stdio", "program": "bin/banana-mcp", "args": []},
                "lifecycle": "auto",
                "startupTimeoutMs": 10000,
                "toolTimeoutMs": 60000,
                "tools": [{
                    "sourceName": "banana-search",
                    "name": "banana_search",
                    "description": "Search bananas.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"]
                    },
                    "concurrencySafe": true,
                    "scopes": ["banana:read"]
                }]
            }]
        }"#,
    )
    .unwrap();

    for mode in ["--write", "--check"] {
        let output = Command::new(env!("CARGO_BIN_EXE_centaeris-mcp-contract"))
            .current_dir(directory.path())
            .args([mode, filename])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
