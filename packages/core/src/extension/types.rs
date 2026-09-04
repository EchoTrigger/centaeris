use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginDescriptorV1 {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub enabled: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub tools: Vec<String>,
    pub scopes: Vec<String>,
    pub activation_status: String,
    pub policy_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginCapabilitiesV1 {
    pub skills: Vec<String>,
    pub cli: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub apps: Vec<String>,
    pub hooks: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetailV1 {
    pub descriptor: PluginDescriptorV1,
    pub capabilities: PluginCapabilitiesV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalogStateV1 {
    pub schema: String,
    pub enabled_plugins: Vec<String>,
    pub disabled_plugins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginResourceDigestV1 {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivatedPluginPackageV1 {
    pub name: String,
    pub version: String,
    pub package_digest: String,
    pub skills: Vec<PluginResourceDigestV1>,
    pub cli: Vec<PluginResourceDigestV1>,
    pub mcp_servers: Vec<PluginResourceDigestV1>,
    pub hooks: Vec<PluginResourceDigestV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginActivationSnapshotV1 {
    pub schema: String,
    pub digest: String,
    pub packages: Vec<ActivatedPluginPackageV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSourceRefV1 {
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginListRequestV1 {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginDetailRequestV1 {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSetEnabledRequestV1 {
    pub id: String,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_list_request_is_single_purpose_and_rejects_discriminators() {
        serde_json::from_value::<PluginListRequestV1>(serde_json::json!({}))
            .expect("empty plugin list request");

        let error = serde_json::from_value::<PluginListRequestV1>(serde_json::json!({
            "kind": "banana"
        }))
        .expect_err("plugin list request must reject unknown fields");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn plugin_descriptor_has_no_generic_kind_field() {
        let descriptor = PluginDescriptorV1 {
            id: "demo".to_string(),
            name: "Demo".to_string(),
            description: String::new(),
            source: "local".to_string(),
            enabled: true,
            path: "/plugins/demo".to_string(),
            manifest_path: Some("/plugins/demo/.centaeris-plugin/plugin.json".to_string()),
            errors: Vec::new(),
            version: None,
            tools: Vec::new(),
            scopes: Vec::new(),
            activation_status: "enabled".to_string(),
            policy_source: "user".to_string(),
        };

        let encoded = serde_json::to_value(descriptor).expect("serialize plugin descriptor");
        assert!(encoded.get("kind").is_none());
    }
}
