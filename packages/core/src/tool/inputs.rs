use std::collections::HashSet;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub const RESOLVED_INPUT_SCHEMA: &str = "runtime.resolved_input.v1";
pub const RESOLVED_INPUT_MANIFEST_SCHEMA: &str = "runtime.resolved_input_manifest.v1";
pub const DECLARED_INPUT_SCHEMA: &str = "runtime.declared_input.v1";

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedInputManifest {
    pub schema: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub inputs: Vec<ResolvedInput>,
}

impl ResolvedInputManifest {
    pub fn validate_against(
        &self,
        agent_run_id: &str,
        expected_digest: &str,
        declared_inputs: &[DeclaredInput],
    ) -> Result<(), String> {
        if self.schema != RESOLVED_INPUT_MANIFEST_SCHEMA {
            return Err("resolved input manifest schema mismatch".to_string());
        }
        require_opaque_ref("agentRunId", agent_run_id)?;
        if self.agent_run_id != agent_run_id {
            return Err("resolved input manifest AgentRun binding mismatch".to_string());
        }
        validate_sha256(self.authorization_digest.as_str())?;
        if self.authorization_digest != expected_digest {
            return Err("resolved input manifest authorization binding mismatch".to_string());
        }
        let mut input_refs = HashSet::new();
        let mut object_versions = HashSet::new();
        let mut virtual_paths = HashSet::new();
        let mut previous_input_ref: Option<&str> = None;
        for input in &self.inputs {
            input.validate()?;
            if let Some(previous) = previous_input_ref {
                if previous >= input.input_ref.as_str() {
                    return Err("resolved inputs must be sorted by unique inputRef".to_string());
                }
            }
            previous_input_ref = Some(input.input_ref.as_str());
            if !input_refs.insert(input.input_ref.as_str()) {
                return Err(format!("duplicate inputRef: {}", input.input_ref));
            }
            if !object_versions.insert((input.object_ref.as_str(), input.source_version.as_str())) {
                return Err(format!(
                    "duplicate objectRef/sourceVersion: {}/{}",
                    input.object_ref, input.source_version
                ));
            }
            if !virtual_paths.insert(input.virtual_path.as_str()) {
                return Err(format!("duplicate virtualPath: {}", input.virtual_path));
            }
            let reference = declared_inputs
                .iter()
                .find(|reference| reference.input_ref == input.input_ref)
                .ok_or_else(|| {
                    format!(
                        "resolved input is not declared for this AgentRun: {}",
                        input.input_ref
                    )
                })?;
            if reference.display_name != input.display_name
                || reference.content_type != input.content_type
                || reference.input_identity.owner_kind != input.owner_kind
                || reference.input_identity.owner_id != input.object_ref
                || reference.input_identity.generation.to_string() != input.source_version
                || reference.input_identity.sha256 != input.sha256
                || reference.size_bytes != input.size_bytes
            {
                return Err(format!(
                    "resolved input metadata changed: {}",
                    input.input_ref
                ));
            }
        }
        Ok(())
    }

    pub fn input_by_ref(&self, input_ref: &str) -> Option<&ResolvedInput> {
        self.inputs
            .iter()
            .find(|input| input.input_ref == input_ref)
    }

    pub fn input_by_virtual_path(&self, raw_path: &str) -> Result<Option<&ResolvedInput>, String> {
        let Ok(normalized) = canonical_virtual_path(raw_path) else {
            return Ok(None);
        };
        Ok(self
            .inputs
            .iter()
            .find(|input| input.virtual_path == normalized))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredInputResolutionFailureKind {
    AssetRemoved,
    AccessRevoked,
    SourceDeleted,
    StaleGeneration,
    AssetUnavailable,
    HostUnavailable,
}

impl DeferredInputResolutionFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AssetRemoved => "asset_removed",
            Self::AccessRevoked => "access_revoked",
            Self::SourceDeleted => "source_deleted",
            Self::StaleGeneration => "stale_generation",
            Self::AssetUnavailable => "asset_unavailable",
            Self::HostUnavailable => "host_unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeferredInputResolutionError {
    pub kind: DeferredInputResolutionFailureKind,
    pub message: String,
}

impl DeferredInputResolutionError {
    pub fn new(kind: DeferredInputResolutionFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub trait DeferredInputResolverPort: Send + Sync {
    fn resolve_deferred_input(
        &self,
        reference: &DeclaredInput,
    ) -> Result<ResolvedInput, DeferredInputResolutionError>;
}

pub struct ResolvedInputState {
    agent_run_id: String,
    authorization_digest: String,
    declared_inputs: Vec<DeclaredInput>,
    manifest: Mutex<ResolvedInputManifest>,
    resolver: Option<Arc<dyn DeferredInputResolverPort>>,
}

impl ResolvedInputState {
    pub fn new(
        agent_run_id: String,
        authorization_digest: String,
        declared_inputs: Vec<DeclaredInput>,
        manifest: ResolvedInputManifest,
        resolver: Option<Arc<dyn DeferredInputResolverPort>>,
    ) -> Result<Self, String> {
        require_opaque_ref("agentRunId", agent_run_id.as_str())?;
        validate_sha256(authorization_digest.as_str())?;
        validate_declared_inputs(declared_inputs.as_slice())?;
        manifest.validate_against(
            agent_run_id.as_str(),
            authorization_digest.as_str(),
            declared_inputs.as_slice(),
        )?;
        Ok(Self {
            agent_run_id,
            authorization_digest,
            declared_inputs,
            manifest: Mutex::new(manifest),
            resolver,
        })
    }

    pub fn agent_run_id(&self) -> &str {
        self.agent_run_id.as_str()
    }

    pub fn authorization_digest(&self) -> &str {
        self.authorization_digest.as_str()
    }

    pub fn input_by_ref(&self, input_ref: &str) -> Result<Option<ResolvedInput>, String> {
        Ok(self
            .manifest
            .lock()
            .map_err(|_| "resolved input manifest lock poisoned".to_string())?
            .input_by_ref(input_ref)
            .cloned())
    }

    pub fn input_by_virtual_path(&self, path: &str) -> Result<Option<ResolvedInput>, String> {
        Ok(self
            .manifest
            .lock()
            .map_err(|_| "resolved input manifest lock poisoned".to_string())?
            .input_by_virtual_path(path)?
            .cloned())
    }

    pub fn inputs_by_virtual_path_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<ResolvedInput>, String> {
        let prefix = canonical_virtual_path(prefix)?;
        let prefix = format!("{}/", prefix.trim_end_matches('/'));
        let mut inputs = self
            .manifest
            .lock()
            .map_err(|_| "resolved input manifest lock poisoned".to_string())?
            .inputs
            .iter()
            .filter(|input| input.virtual_path.starts_with(prefix.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        inputs.sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
        Ok(inputs)
    }

    pub fn display_name_by_ref(&self, input_ref: &str) -> Result<Option<String>, String> {
        if let Some(reference) = self
            .declared_inputs
            .iter()
            .find(|reference| reference.input_ref == input_ref)
        {
            return Ok(Some(reference.display_name.clone()));
        }
        Ok(None)
    }

    pub fn declared_input_by_ref(&self, input_ref: &str) -> Option<DeclaredInput> {
        self.declared_inputs
            .iter()
            .find(|reference| reference.input_ref == input_ref)
            .cloned()
    }

    pub fn declared_inputs(&self) -> Vec<DeclaredInput> {
        self.declared_inputs.clone()
    }

    pub fn resolve_input(
        &self,
        input_ref: &str,
    ) -> Result<ResolvedInput, DeferredInputResolutionError> {
        let reference = self
            .declared_inputs
            .iter()
            .find(|reference| reference.input_ref == input_ref)
            .cloned()
            .ok_or_else(|| {
                DeferredInputResolutionError::new(
                    DeferredInputResolutionFailureKind::AssetUnavailable,
                    "requested input is not declared for this AgentRun",
                )
            })?;
        let existing = self.input_by_ref(input_ref).map_err(|error| {
            DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::HostUnavailable,
                error,
            )
        })?;
        let Some(resolver) = self.resolver.as_ref() else {
            return existing.ok_or_else(|| {
                DeferredInputResolutionError::new(
                    DeferredInputResolutionFailureKind::HostUnavailable,
                    "deferred input resolver is not configured",
                )
            });
        };
        let resolved = resolver.resolve_deferred_input(&reference)?;
        if resolved.input_ref != reference.input_ref
            || resolved.display_name != reference.display_name
            || resolved.content_type != reference.content_type
            || resolved.owner_kind != reference.input_identity.owner_kind
            || resolved.object_ref != reference.input_identity.owner_id
            || resolved.source_version != reference.input_identity.generation.to_string()
            || resolved.sha256 != reference.input_identity.sha256
            || resolved.size_bytes != reference.size_bytes
        {
            return Err(DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::HostUnavailable,
                "deferred input resolver returned mismatched metadata",
            ));
        }
        resolved.validate().map_err(|error| {
            DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::HostUnavailable,
                error,
            )
        })?;
        let mut manifest = self.manifest.lock().map_err(|_| {
            DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::HostUnavailable,
                "resolved input manifest lock poisoned",
            )
        })?;
        if let Some(existing) = manifest.input_by_ref(input_ref) {
            if existing != &resolved {
                return Err(DeferredInputResolutionError::new(
                    DeferredInputResolutionFailureKind::HostUnavailable,
                    "deferred input resolver changed an existing resolution",
                ));
            }
            return Ok(existing.clone());
        }
        if manifest.inputs.iter().any(|item| {
            item.object_ref == resolved.object_ref && item.source_version == resolved.source_version
        }) {
            return Err(DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::HostUnavailable,
                "deferred input resolver duplicated an object version",
            ));
        }
        if manifest
            .inputs
            .iter()
            .any(|item| item.virtual_path == resolved.virtual_path)
        {
            return Err(DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::HostUnavailable,
                "deferred input resolver duplicated a virtual path",
            ));
        }
        manifest.inputs.push(resolved.clone());
        manifest
            .inputs
            .sort_by(|left, right| left.input_ref.cmp(&right.input_ref));
        Ok(resolved)
    }
}

pub(crate) fn canonical_virtual_path(raw_path: &str) -> Result<String, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() || trimmed.contains('\\') {
        return Err("resolved input virtual path is invalid".to_string());
    }
    if trimmed.starts_with('/') {
        return Err("resolved input absolute path alias is unsupported".to_string());
    }
    let relative = trimmed;
    if relative.is_empty()
        || relative.ends_with('/')
        || relative
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err("resolved input virtual path is invalid".to_string());
    }
    Ok(relative.to_string())
}

impl ResolvedInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RESOLVED_INPUT_SCHEMA {
            return Err("resolved_input_schema_mismatch".to_string());
        }
        require_opaque_ref("inputRef", self.input_ref.as_str())?;
        require_opaque_ref("objectRef", self.object_ref.as_str())?;
        require_non_empty("ownerKind", self.owner_kind.as_str())?;
        require_non_empty("displayName", self.display_name.as_str())?;
        require_non_empty("contentType", self.content_type.as_str())?;
        require_non_empty("sourceVersion", self.source_version.as_str())?;
        require_non_empty("evidenceKind", self.evidence_kind.as_str())?;
        validate_virtual_path(self.virtual_path.as_str())?;
        validate_sha256(self.sha256.as_str())?;
        Ok(())
    }
}

fn validate_declared_inputs(inputs: &[DeclaredInput]) -> Result<(), String> {
    let mut input_refs = HashSet::new();
    let mut previous_input_ref: Option<&str> = None;
    for input in inputs {
        input.validate()?;
        if let Some(previous) = previous_input_ref {
            if previous >= input.input_ref.as_str() {
                return Err("declared inputs must be sorted by unique inputRef".to_string());
            }
        }
        previous_input_ref = Some(input.input_ref.as_str());
        if !input_refs.insert(input.input_ref.as_str()) {
            return Err(format!("duplicate inputRef: {}", input.input_ref));
        }
    }
    Ok(())
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

    fn declared_input() -> DeclaredInput {
        DeclaredInput {
            schema: DECLARED_INPUT_SCHEMA.to_string(),
            input_ref: "input_1".to_string(),
            display_name: "notice.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            input_identity: InputIdentityV1 {
                owner_kind: "sourceObject".to_string(),
                owner_id: "srcobj_1".to_string(),
                generation: 1,
                sha256: format!("sha256:{}", "a".repeat(64)),
            },
            size_bytes: 10,
        }
    }

    fn resolved_input() -> ResolvedInput {
        ResolvedInput {
            schema: RESOLVED_INPUT_SCHEMA.to_string(),
            input_ref: "input_1".to_string(),
            object_ref: "srcobj_1".to_string(),
            owner_kind: "sourceObject".to_string(),
            virtual_path: "sources/srcobj_1/notice.pdf".to_string(),
            display_name: "notice.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            size_bytes: 10,
            sha256: format!("sha256:{}", "a".repeat(64)),
            source_version: "1".to_string(),
            evidence_kind: "workspaceSource".to_string(),
            citation_allowed: true,
        }
    }

    #[test]
    fn resolved_input_rejects_paths_and_invalid_hashes() {
        let mut input = resolved_input();
        input.virtual_path = "../secret.pdf".to_string();
        assert!(input
            .validate()
            .expect_err("parent path rejected")
            .contains("virtualPath"));

        let mut input = resolved_input();
        input.sha256 = "sha256:banana".to_string();
        assert!(input.validate().expect_err("hash rejected").contains("64"));

        let mut input = resolved_input();
        input.sha256 = format!("sha256:{}", "A".repeat(64));
        assert!(input.validate().is_err());
    }

    #[test]
    fn resolved_input_manifest_allows_an_empty_then_declared_input_resolution() {
        let declared = declared_input();
        let mut manifest = ResolvedInputManifest {
            schema: RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
            agent_run_id: "agent_run_1".to_string(),
            authorization_digest: format!("sha256:{}", "b".repeat(64)),
            inputs: Vec::new(),
        };
        manifest
            .validate_against(
                "agent_run_1",
                manifest.authorization_digest.as_str(),
                std::slice::from_ref(&declared),
            )
            .expect("valid manifest");
        assert_eq!(
            manifest
                .input_by_virtual_path("sources/srcobj_1/notice.pdf")
                .expect("canonical virtual path")
                .map(|input| input.input_ref.as_str()),
            None
        );

        manifest.inputs.push(resolved_input());
        manifest
            .validate_against(
                "agent_run_1",
                manifest.authorization_digest.as_str(),
                &[declared],
            )
            .expect("one resolved input is valid");
        assert_eq!(
            manifest
                .input_by_virtual_path("sources/srcobj_1/notice.pdf")
                .expect("canonical virtual path")
                .map(|input| input.input_ref.as_str()),
            Some("input_1")
        );
        assert_eq!(
            manifest
                .input_by_virtual_path("/mnt/data/sources/srcobj_1/notice.pdf")
                .expect("absolute execution path is not a virtual path alias"),
            None
        );

        manifest.inputs[0].display_name = "changed.pdf".to_string();
        assert!(manifest
            .validate_against(
                "agent_run_1",
                manifest.authorization_digest.as_str(),
                &[declared_input()]
            )
            .is_err());
    }
}
