//! Workspace-owned input identities and their strict signed/wire representations.
use std::path::{Component, Path};

use centaeris_core::source::SourceIdentityV1;
use centaeris_core::tool::inputs;

pub(crate) const DECLARED_INPUT_SCHEMA: &str = "runtime.declared_input.v1";
pub(crate) const RESOLVED_INPUT_SCHEMA: &str = "runtime.resolved_input.v1";
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedInput {
    pub schema: String,
    pub input_ref: String,
    pub object_ref: String,
    pub owner_kind: String,
    pub virtual_path: String,
    pub display_name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub source_version: String,
    pub evidence_kind: String,
    pub citation_allowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredInput {
    pub schema: String,
    pub input_ref: String,
    pub display_name: String,
    pub content_type: String,
    pub input_identity: InputIdentityV1,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputIdentityV1 {
    pub owner_kind: String,
    pub owner_id: String,
    pub generation: u64,
    pub sha256: String,
}

impl DeclaredInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != DECLARED_INPUT_SCHEMA {
            return Err("declared_input_schema_mismatch".to_string());
        }
        require_opaque_ref("inputRef", self.input_ref.as_str())?;
        require_non_empty("displayName", self.display_name.as_str())?;
        require_non_empty("contentType", self.content_type.as_str())?;
        self.input_identity.validate()
    }
}

impl InputIdentityV1 {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.owner_kind.as_str(),
            "sourceObject" | "userLibraryObject" | "artifact"
        ) {
            return Err(format!("unsupported ownerKind: {}", self.owner_kind));
        }
        require_opaque_ref("ownerId", self.owner_id.as_str())?;
        if self.generation == 0 {
            return Err("input identity generation must be positive".to_string());
        }
        validate_sha256(self.sha256.as_str())
    }
}

impl ResolvedInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RESOLVED_INPUT_SCHEMA {
            return Err("resolved_input_schema_mismatch".to_string());
        }
        require_opaque_ref("inputRef", self.input_ref.as_str())?;
        require_opaque_ref("objectRef", self.object_ref.as_str())?;
        if !matches!(
            self.owner_kind.as_str(),
            "sourceObject" | "userLibraryObject" | "artifact"
        ) {
            return Err(format!("unsupported ownerKind: {}", self.owner_kind));
        }
        require_non_empty("displayName", self.display_name.as_str())?;
        require_non_empty("contentType", self.content_type.as_str())?;
        require_non_empty("sourceVersion", self.source_version.as_str())?;
        if !matches!(
            self.evidence_kind.as_str(),
            "workspaceSource" | "userProvided" | "generatedArtifact"
        ) {
            return Err(format!("unsupported evidenceKind: {}", self.evidence_kind));
        }
        validate_virtual_path(self.virtual_path.as_str())?;
        validate_sha256(self.sha256.as_str())?;
        Ok(())
    }
}

impl DeclaredInput {
    pub(crate) fn to_core(&self) -> Result<inputs::DeclaredInput, String> {
        self.validate()?;
        Ok(inputs::DeclaredInput {
            schema: inputs::DECLARED_INPUT_SCHEMA.into(),
            input_ref: self.input_ref.clone(),
            display_name: self.display_name.clone(),
            content_type: self.content_type.clone(),
            input_identity: SourceIdentityV1 {
                source_ref: format!(
                    "{}:{}",
                    self.input_identity.owner_kind, self.input_identity.owner_id
                ),
                revision: self.input_identity.generation.to_string(),
                sha256: self.input_identity.sha256.clone(),
            },
            size_bytes: self.size_bytes,
        })
    }
}

impl ResolvedInput {
    pub(crate) fn to_core(&self) -> Result<inputs::ResolvedInput, String> {
        self.validate()?;
        Ok(inputs::ResolvedInput {
            schema: inputs::RESOLVED_INPUT_SCHEMA.into(),
            input_ref: self.input_ref.clone(),
            source_ref: format!("{}:{}", self.owner_kind, self.object_ref),
            virtual_path: self.virtual_path.clone(),
            display_name: self.display_name.clone(),
            content_type: self.content_type.clone(),
            size_bytes: self.size_bytes,
            sha256: self.sha256.clone(),
            source_version: self.source_version.clone(),
        })
    }
}

fn require_non_empty(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(format!("{name} is required without outer whitespace"));
    }
    if value.nfc().collect::<String>() != value {
        return Err(format!("{name} must use NFC Unicode normalization"));
    }
    Ok(())
}

fn require_opaque_ref(name: &str, value: &str) -> Result<(), String> {
    require_non_empty(name, value)?;
    if value.contains('/') || value.contains('\\') {
        return Err(format!("{name} must be an opaque ref, not a path"));
    }
    Ok(())
}

fn validate_virtual_path(value: &str) -> Result<(), String> {
    require_non_empty("virtualPath", value)?;
    if value.contains('\\') || value.contains(':') || Path::new(value).is_absolute() {
        return Err("virtualPath must be a relative POSIX path".to_string());
    }
    if value.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err("virtualPath must not contain empty, dot, or parent components".to_string());
    }
    for component in Path::new(value).components() {
        if !matches!(component, Component::Normal(_)) {
            return Err("virtualPath must not contain root, dot, or parent components".to_string());
        }
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err("sha256 must use sha256:<hex> format".to_string());
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("sha256 must contain 64 lowercase hexadecimal characters".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_wire_identity_is_preserved_while_core_uses_an_opaque_source() {
        let wire = json!({
            "schema": DECLARED_INPUT_SCHEMA,
            "inputRef": "input-one", "displayName": "one.txt", "contentType": "text/plain",
            "sizeBytes": 3,
            "inputIdentity": {"ownerKind": "userLibraryObject", "ownerId": "one", "generation": 2,
                "sha256": format!("sha256:{}", "a".repeat(64))}
        });
        let input: DeclaredInput = serde_json::from_value(wire.clone()).unwrap();
        let core = input.to_core().unwrap();
        core.validate().unwrap();
        assert_eq!(serde_json::to_value(&input).unwrap(), wire);
        assert_eq!(core.input_identity.source_ref, "userLibraryObject:one");
        assert_eq!(core.input_identity.revision, "2");
        let mut other_owner = input.clone();
        other_owner.input_identity.owner_kind = "sourceObject".into();
        assert_ne!(
            other_owner.to_core().unwrap().input_identity,
            core.input_identity
        );
        other_owner.input_identity.owner_kind = "unknown".into();
        assert!(other_owner.to_core().is_err());
    }

    #[test]
    fn resolved_workspace_policy_is_validated_before_core_conversion() {
        let wire = json!({
            "schema": RESOLVED_INPUT_SCHEMA, "inputRef": "input-one", "objectRef": "one",
            "ownerKind": "userLibraryObject", "virtualPath": "inputs/one.txt", "displayName": "one.txt",
            "contentType": "text/plain", "sizeBytes": 3, "sourceVersion": "2",
            "sha256": format!("sha256:{}", "a".repeat(64)), "evidenceKind": "userProvided",
            "citationAllowed": false
        });
        let input: ResolvedInput = serde_json::from_value(wire.clone()).unwrap();
        let core = input.to_core().unwrap();
        core.validate().unwrap();
        assert_eq!(serde_json::to_value(&input).unwrap(), wire);
        assert_eq!(core.source_ref, "userLibraryObject:one");
        assert_eq!(core.source_version, "2");
        let mut invalid = input;
        invalid.evidence_kind = "unknown".into();
        assert!(invalid.to_core().is_err());
    }
}
