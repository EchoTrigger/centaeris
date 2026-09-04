use super::manifest::validate_relative_resource_path;
use crate::runtime::canonical_json;
use crate::tool::limits::{
    json_size_with_limit, read_tool_contract_file, validate_tool_description, ToolContractBudget,
    MAX_TOOL_CONTRACT_BYTES, MAX_TOOL_INPUT_SCHEMA_BYTES,
};
use crate::tool::{DynamicToolContract, ToolTurnBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;
use url::Url;

pub const MCP_SERVERS_SCHEMA_V1: &str = "mcp_servers_v1";
const MAX_SERVERS: usize = 32;
const MAX_TOOLS_PER_SERVER: usize = 128;
const MAX_ARGS: usize = 32;
const MCP_MODEL_CONTRACT_DIGEST_DOMAIN_V1: &str = "centaeris.mcp_model_contract.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServersFileV1 {
    pub schema: String,
    pub servers: Vec<McpServerDeclarationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerDeclarationV1 {
    pub id: String,
    #[serde(rename = "modelContractDigest")]
    pub model_contract_digest: String,
    pub transport: McpTransportV1,
    pub lifecycle: McpLifecycleV1,
    #[serde(rename = "startupTimeoutMs")]
    pub startup_timeout_ms: u64,
    #[serde(rename = "toolTimeoutMs")]
    pub tool_timeout_ms: u64,
    pub tools: Vec<McpToolDeclarationV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpLifecycleV1 {
    Auto,
    Initialize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum McpTransportV1 {
    Stdio {
        program: String,
        args: Vec<String>,
    },
    StreamableHttp {
        url: String,
        #[serde(
            rename = "bearerCredentialRef",
            default,
            deserialize_with = "deserialize_optional_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        bearer_credential_ref: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolDeclarationV1 {
    #[serde(rename = "sourceName")]
    pub source_name: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "concurrencySafe")]
    pub concurrency_safe: bool,
    pub scopes: Vec<String>,
}

pub fn load_mcp_servers_file(path: &Path) -> Result<McpServersFileV1, String> {
    let raw = read_tool_contract_file(path).map_err(|error| {
        format!(
            "read MCP server declaration failed {}: {error}",
            path.display()
        )
    })?;
    let declaration = serde_json::from_str::<McpServersFileV1>(raw.as_str()).map_err(|error| {
        format!(
            "parse MCP server declaration failed {}: {error}",
            path.display()
        )
    })?;
    declaration.validate()?;
    Ok(declaration)
}

impl McpServersFileV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != MCP_SERVERS_SCHEMA_V1 {
            return Err("MCP server declaration schema mismatch".to_string());
        }
        if self.servers.is_empty() || self.servers.len() > MAX_SERVERS {
            return Err("MCP server declaration must contain 1-32 servers".to_string());
        }
        json_size_with_limit(self, MAX_TOOL_CONTRACT_BYTES)?;
        let mut previous_id: Option<&str> = None;
        for server in &self.servers {
            server.validate()?;
            if previous_id.is_some_and(|previous| previous >= server.id.as_str()) {
                return Err("MCP servers must be sorted by unique id".to_string());
            }
            previous_id = Some(server.id.as_str());
        }
        Ok(())
    }
}

impl McpServerDeclarationV1 {
    fn validate(&self) -> Result<(), String> {
        json_size_with_limit(self, MAX_TOOL_CONTRACT_BYTES)?;
        validate_lower_kebab("MCP server id", self.id.as_str())?;
        match &self.transport {
            McpTransportV1::Stdio { program, args } => {
                validate_relative_resource_path("MCP stdio program", program.as_str())?;
                if args.len() > MAX_ARGS
                    || args.iter().any(|arg| {
                        arg.len() > 4_096
                            || arg.chars().any(char::is_control)
                            || arg.nfc().collect::<String>() != *arg
                    })
                {
                    return Err("MCP stdio args are invalid".to_string());
                }
            }
            McpTransportV1::StreamableHttp {
                url,
                bearer_credential_ref,
            } => {
                validate_https_endpoint(url.as_str())?;
                if let Some(credential_ref) = bearer_credential_ref {
                    validate_lower_kebab("MCP bearer credential ref", credential_ref.as_str())?;
                }
            }
        }
        if !(1..=60_000).contains(&self.startup_timeout_ms) {
            return Err("MCP startupTimeoutMs must be between 1 and 60000".to_string());
        }
        if !(1..=300_000).contains(&self.tool_timeout_ms) {
            return Err("MCP toolTimeoutMs must be between 1 and 300000".to_string());
        }
        if self.tools.is_empty() || self.tools.len() > MAX_TOOLS_PER_SERVER {
            return Err("MCP server must declare 1-128 tools".to_string());
        }
        let mut previous_source_name: Option<&str> = None;
        let mut model_names = HashSet::new();
        for tool in &self.tools {
            validate_exact_text("MCP sourceName", tool.source_name.as_str(), 128)?;
            validate_lower_snake("MCP model tool name", tool.name.as_str())?;
            validate_model_description(tool.description.as_str())?;
            if !tool.input_schema.is_object() {
                return Err(
                    "MCP tool inputSchema must be an object of at most 65536 bytes".to_string(),
                );
            }
            json_size_with_limit(&tool.input_schema, MAX_TOOL_INPUT_SCHEMA_BYTES)
                .map_err(|error| format!("MCP tool inputSchema: {error}"))?;
            if previous_source_name.is_some_and(|previous| previous >= tool.source_name.as_str()) {
                return Err("MCP tools must be sorted by unique sourceName".to_string());
            }
            previous_source_name = Some(tool.source_name.as_str());
            if !model_names.insert(tool.name.as_str()) {
                return Err("MCP model tool names must be unique per server".to_string());
            }
            let mut previous_scope: Option<&str> = None;
            for scope in &tool.scopes {
                validate_exact_text("MCP tool scope", scope.as_str(), 128)?;
                if previous_scope.is_some_and(|previous| previous >= scope.as_str()) {
                    return Err("MCP tool scopes must be sorted and unique".to_string());
                }
                previous_scope = Some(scope.as_str());
            }
        }
        require_sha256(
            "MCP modelContractDigest",
            self.model_contract_digest.as_str(),
        )?;
        if mcp_model_contract_digest(self.id.as_str(), self.tools.as_slice())?
            != self.model_contract_digest
        {
            return Err("MCP modelContractDigest mismatch".to_string());
        }
        Ok(())
    }

    pub fn dynamic_tool_contracts(
        &self,
        plugin_name: &str,
    ) -> Result<Vec<DynamicToolContract>, String> {
        self.validate()?;
        let provider_id = mcp_provider_id(plugin_name, self.id.as_str())?;
        Ok(self
            .tools
            .iter()
            .map(|tool| DynamicToolContract {
                name: tool.name.clone(),
                category: "external.mcp".to_string(),
                summary: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                provider_id: provider_id.clone(),
                scopes: tool.scopes.clone(),
                concurrency_safe: tool.concurrency_safe,
                turn_behavior: ToolTurnBehavior::ContinueTurn,
            })
            .collect())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpModelToolContractV1<'a> {
    source_name: &'a str,
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpModelContractV1<'a> {
    schema: &'static str,
    server_id: &'a str,
    tools: Vec<McpModelToolContractV1<'a>>,
}

pub fn mcp_model_contract_digest(
    server_id: &str,
    tools: &[McpToolDeclarationV1],
) -> Result<String, String> {
    validate_lower_kebab("MCP server id", server_id)?;
    let mut budget = ToolContractBudget::default();
    for tool in tools {
        budget.add(tool)?;
        validate_model_description(&tool.description)?;
        json_size_with_limit(&tool.input_schema, MAX_TOOL_INPUT_SCHEMA_BYTES)
            .map_err(|error| format!("MCP tool inputSchema: {error}"))?;
    }
    let mut tools = tools.iter().collect::<Vec<_>>();
    tools.sort_by(|left, right| left.source_name.cmp(&right.source_name));
    canonical_json::sha256(
        MCP_MODEL_CONTRACT_DIGEST_DOMAIN_V1,
        &McpModelContractV1 {
            schema: "mcp_model_contract_v1",
            server_id,
            tools: tools
                .into_iter()
                .map(|tool| McpModelToolContractV1 {
                    source_name: tool.source_name.as_str(),
                    name: tool.name.as_str(),
                    description: tool.description.as_str(),
                    input_schema: &tool.input_schema,
                })
                .collect(),
        },
    )
}

pub fn mcp_provider_id(plugin_name: &str, server_id: &str) -> Result<String, String> {
    validate_lower_kebab("MCP plugin name", plugin_name)?;
    validate_lower_kebab("MCP server id", server_id)?;
    Ok(format!("mcp:{plugin_name}:{server_id}"))
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn validate_https_endpoint(value: &str) -> Result<(), String> {
    validate_exact_text("MCP Streamable HTTP URL", value, 2_048)?;
    let url = Url::parse(value).map_err(|_| "MCP Streamable HTTP URL is invalid".to_string())?;
    let has_userinfo = value.split_once("://").is_some_and(|(_, remainder)| {
        remainder
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    });
    if url.scheme() != "https"
        || url.host_str().is_none()
        || has_userinfo
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "MCP Streamable HTTP URL must be credential-free HTTPS without query or fragment"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_exact_text(label: &str, value: &str, max_chars: usize) -> Result<(), String> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
        || value.nfc().collect::<String>() != value
    {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

fn validate_model_description(value: &str) -> Result<(), String> {
    validate_tool_description(value)?;
    if value.nfc().collect::<String>() != value {
        return Err("MCP tool description is invalid or oversized".to_string());
    }
    Ok(())
}

fn require_sha256(label: &str, value: &str) -> Result<(), String> {
    if !value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(format!("{label} must be sha256:<64 lowercase hex>"));
    }
    Ok(())
}

fn validate_lower_kebab(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || value.starts_with('-')
        || value.ends_with('-')
        || value.contains("--")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("{label} must use lower-kebab-case"));
    }
    Ok(())
}

pub(super) fn validate_lower_snake(label: &str, value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_lowercase()
        || bytes.last() == Some(&b'_')
        || bytes.windows(2).any(|pair| pair == b"__")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(format!("{label} must use canonical lower_snake_case"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> McpServersFileV1 {
        let tools = vec![McpToolDeclarationV1 {
            source_name: "banana-search".to_string(),
            name: "banana_search".to_string(),
            description: "Search bananas.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
            concurrency_safe: true,
            scopes: vec!["banana:read".to_string()],
        }];
        McpServersFileV1 {
            schema: MCP_SERVERS_SCHEMA_V1.to_string(),
            servers: vec![McpServerDeclarationV1 {
                id: "banana-source".to_string(),
                model_contract_digest: mcp_model_contract_digest("banana-source", tools.as_slice())
                    .expect("model contract digest"),
                transport: McpTransportV1::Stdio {
                    program: "bin/banana-mcp".to_string(),
                    args: vec![],
                },
                lifecycle: McpLifecycleV1::Auto,
                startup_timeout_ms: 10_000,
                tool_timeout_ms: 60_000,
                tools,
            }],
        }
    }

    #[test]
    fn validates_exact_stdio_declaration() {
        valid().validate().expect("valid MCP declaration");

        let mut declaration = valid();
        declaration.servers[0].transport = McpTransportV1::Stdio {
            program: "../banana".to_string(),
            args: vec![],
        };
        assert!(declaration.validate().is_err());

        let mut declaration = valid();
        declaration.servers[0].tools[0].name = "bananaTool".to_string();
        assert!(declaration.validate().is_err());

        assert!(serde_json::from_str::<McpServersFileV1>(
            r#"{"schema":"mcp_servers_v1","servers":[],"banana":true}"#
        )
        .is_err());
        let mut declaration = valid();
        declaration.servers[0].tools[0].description = "Changed.".to_string();
        assert_eq!(
            declaration.validate().expect_err("stale digest"),
            "MCP modelContractDigest mismatch"
        );
    }

    #[test]
    fn derives_static_contract_and_digest_with_array_order_preserved() {
        let declaration = valid();
        let server = &declaration.servers[0];
        let contracts = server
            .dynamic_tool_contracts("banana-plugin")
            .expect("static contracts");
        assert_eq!(contracts[0].provider_id, "mcp:banana-plugin:banana-source");
        assert_eq!(contracts[0].summary, "Search bananas.");
        assert_eq!(contracts[0].input_schema, server.tools[0].input_schema);

        let mut reordered = server.tools.clone();
        reordered[0].input_schema["required"] = json!(["other", "query"]);
        let first = mcp_model_contract_digest("banana-source", server.tools.as_slice())
            .expect("first digest");
        let second = mcp_model_contract_digest("banana-source", reordered.as_slice())
            .expect("second digest");
        assert_ne!(first, second);
        assert_ne!(
            first,
            mcp_model_contract_digest("other-source", server.tools.as_slice())
                .expect("other server digest")
        );

        let first_tool = server.tools[0].clone();
        let mut second_tool = first_tool.clone();
        second_tool.source_name = "pear-search".to_string();
        second_tool.name = "pear_search".to_string();
        second_tool.description = "Search pears.".to_string();
        let sorted = vec![first_tool.clone(), second_tool.clone()];
        let unsorted = vec![second_tool, first_tool];
        assert_eq!(
            mcp_model_contract_digest("banana-source", sorted.as_slice()).expect("sorted digest"),
            mcp_model_contract_digest("banana-source", unsorted.as_slice())
                .expect("unsorted digest")
        );
        let mut unsorted_declaration = declaration.clone();
        unsorted_declaration.servers[0].tools = unsorted;
        unsorted_declaration.servers[0].model_contract_digest = mcp_model_contract_digest(
            "banana-source",
            unsorted_declaration.servers[0].tools.as_slice(),
        )
        .expect("manifest digest");
        assert!(unsorted_declaration.validate().is_err());
    }

    #[test]
    fn preserves_description_whitespace_in_frozen_and_dynamic_contracts() {
        let mut declaration = valid();
        let server = &mut declaration.servers[0];
        let description = "\n  Search bananas.\n";
        server.tools[0].description = description.to_string();
        server.model_contract_digest =
            mcp_model_contract_digest(&server.id, &server.tools).expect("exact digest");
        server
            .validate()
            .expect("non-empty description with whitespace");
        let registry = crate::tool::DynamicToolRegistry::from_contracts(
            server.dynamic_tool_contracts("banana-plugin").unwrap(),
        )
        .expect("exact dynamic contract");
        assert_eq!(
            registry.find_contract("banana_search").unwrap().summary,
            description
        );

        server.tools[0].description = description.trim().to_string();
        assert_eq!(
            server.validate().unwrap_err(),
            "MCP modelContractDigest mismatch"
        );
        for invalid in [String::new(), " \n\t".to_string(), "a".repeat(4_097)] {
            server.tools[0].description = invalid;
            assert!(server.validate().is_err());
        }
    }

    #[test]
    fn validates_streamable_https_transport_and_optional_bearer_ref() {
        let mut declaration = valid();
        declaration.servers[0].transport = McpTransportV1::StreamableHttp {
            url: "https://banana.invalid:8443/mcp".to_string(),
            bearer_credential_ref: None,
        };
        declaration.validate().expect("HTTPS transport");

        declaration.servers[0].transport = McpTransportV1::StreamableHttp {
            url: "https://banana.invalid:8443/mcp".to_string(),
            bearer_credential_ref: Some("banana-token".to_string()),
        };
        declaration
            .validate()
            .expect("HTTPS transport with bearer credential ref");

        for url in [
            "http://banana.invalid/mcp",
            "https://user@banana.invalid/mcp",
            "https://@banana.invalid/mcp",
            "https://:banana@banana.invalid/mcp",
            "https://banana.invalid/mcp?",
            "https://banana.invalid/mcp?token=banana",
            "https://banana.invalid/mcp#banana",
        ] {
            declaration.servers[0].transport = McpTransportV1::StreamableHttp {
                url: url.to_string(),
                bearer_credential_ref: None,
            };
            assert!(declaration.validate().is_err(), "accepted {url}");
        }
        declaration.servers[0].transport = McpTransportV1::StreamableHttp {
            url: "https://banana.invalid/mcp".to_string(),
            bearer_credential_ref: Some("banana_ref".to_string()),
        };
        assert!(declaration.validate().is_err());
        assert!(serde_json::from_str::<McpServersFileV1>(
            r#"{"schema":"mcp_servers_v1","servers":[{"id":"banana-source","transport":{"type":"streamableHttp","url":"https://banana.invalid/mcp","bearerCredentialRef":null},"lifecycle":"auto","startupTimeoutMs":10000,"toolTimeoutMs":60000,"tools":[]}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<McpServersFileV1>(
            r#"{"schema":"mcp_servers_v1","servers":[{"id":"banana-source","transport":{"type":"streamableHttp","url":"https://banana.invalid/mcp","credentialRef":"banana"},"lifecycle":"auto","startupTimeoutMs":10000,"toolTimeoutMs":60000,"tools":[]}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<McpServersFileV1>(
            r#"{"schema":"mcp_servers_v1","servers":[{"id":"banana-source","transport":{"type":"banana"},"lifecycle":"auto","startupTimeoutMs":10000,"toolTimeoutMs":60000,"tools":[]}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<McpServersFileV1>(
            r#"{"schema":"mcp_servers_v1","servers":[{"id":"banana-source","transport":{"type":"streamableHttp","url":"https://banana.invalid/mcp"},"lifecycle":"banana","startupTimeoutMs":10000,"toolTimeoutMs":60000,"tools":[]}]}"#
        )
        .is_err());
    }
}
