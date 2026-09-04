use super::types::{
    SkillCapabilityMetadata, SkillCatalogItemV1, SkillCatalogLoadConfig, SkillCatalogSnapshotV1,
    SkillDetailV1, SkillDiagnosticV1, SkillEntryV1, SkillSourceConfigV1, SkillSourceKindV1,
    SkillSourceScopeV1, SkillSourceStatusV1, SKILL_CATALOG_SNAPSHOT_SCHEMA_V1,
    SKILL_SOURCES_CONFIG_SCHEMA_V1,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_SKILL_FILE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone)]
pub struct SkillIndex {
    snapshot: SkillCatalogSnapshotV1,
}

impl SkillIndex {
    pub fn empty() -> Self {
        Self::load(SkillCatalogLoadConfig::default()).expect("empty skill catalog must be valid")
    }

    pub fn load(config: SkillCatalogLoadConfig) -> Result<Self, String> {
        if config.sources_config.schema_version != SKILL_SOURCES_CONFIG_SCHEMA_V1 {
            return Err(format!(
                "unsupported skill source schemaVersion: expected {}, got {}",
                SKILL_SOURCES_CONFIG_SCHEMA_V1, config.sources_config.schema_version
            ));
        }
        if config.max_skills == 0 {
            return Err("skill catalog maxSkills must be greater than zero".to_string());
        }
        super::config::validate_skill_sources_config(&config.sources_config)?;

        let cwd = config
            .cwd
            .as_deref()
            .map(canonical_existing_directory)
            .transpose()?;
        let cwd_text = cwd.as_deref().map(canonical_local_path).transpose()?;

        let mut effective_sources = Vec::new();
        let mut diagnostics = Vec::new();
        for source in &config.sources_config.sources {
            match source_applies(source, cwd.as_deref()) {
                Ok(true) => effective_sources.push(source.clone()),
                Ok(false) => {}
                Err(error) => diagnostics.push(SkillDiagnosticV1 {
                    code: "skill_source_unavailable".to_string(),
                    message: error,
                    source_id: Some(source.source_id.clone()),
                    path: Some(source.path.clone()),
                }),
            }
        }
        effective_sources.sort_by(|left, right| {
            left.scope
                .priority()
                .cmp(&right.scope.priority())
                .then_with(|| left.source_id.cmp(&right.source_id))
        });

        let policies = config
            .sources_config
            .skill_policies
            .iter()
            .map(|policy| {
                (
                    (
                        policy.source_id.clone(),
                        normalize_skill_name(policy.skill_name.as_str()),
                    ),
                    policy.enabled,
                )
            })
            .collect::<HashMap<_, _>>();
        let mut entries = Vec::new();
        let mut seen_policy_targets = HashSet::new();

        'sources: for source in effective_sources.iter().filter(|source| source.enabled) {
            let skill_files = match skill_files_for_source(source) {
                Ok(skill_files) => skill_files,
                Err(error) => {
                    diagnostics.push(SkillDiagnosticV1 {
                        code: "skill_source_unavailable".to_string(),
                        message: error,
                        source_id: Some(source.source_id.clone()),
                        path: Some(source.path.clone()),
                    });
                    continue;
                }
            };
            for skill_file in skill_files {
                if entries.len() >= config.max_skills {
                    diagnostics.push(SkillDiagnosticV1 {
                        code: "skill_catalog_limit_reached".to_string(),
                        message: format!("skill catalog reached maxSkills: {}", config.max_skills),
                        source_id: Some(source.source_id.clone()),
                        path: None,
                    });
                    break 'sources;
                }
                match load_skill_file(source, skill_file.as_path(), &policies) {
                    Ok(entry) => {
                        seen_policy_targets.insert((
                            entry.source_id.clone(),
                            normalize_skill_name(entry.name.as_str()),
                        ));
                        entries.push(entry);
                    }
                    Err(error) => diagnostics.push(SkillDiagnosticV1 {
                        code: "skill_manifest_invalid".to_string(),
                        message: error,
                        source_id: Some(source.source_id.clone()),
                        path: Some(path_to_model_string(skill_file.as_path())),
                    }),
                }
            }
        }

        for policy in &config.sources_config.skill_policies {
            let key = (
                policy.source_id.clone(),
                normalize_skill_name(policy.skill_name.as_str()),
            );
            if effective_sources
                .iter()
                .any(|source| source.enabled && source.source_id == policy.source_id)
                && !seen_policy_targets.contains(&key)
            {
                diagnostics.push(SkillDiagnosticV1 {
                    code: "skill_policy_target_missing".to_string(),
                    message: format!(
                        "skill policy target is not present: sourceId={} skillName={}",
                        policy.source_id, policy.skill_name
                    ),
                    source_id: Some(policy.source_id.clone()),
                    path: None,
                });
            }
        }

        apply_name_precedence(entries.as_mut_slice(), &mut diagnostics);
        entries.sort_by(|left, right| {
            left.scope
                .priority()
                .cmp(&right.scope.priority())
                .then_with(|| left.source_id.cmp(&right.source_id))
                .then_with(|| left.name.cmp(&right.name))
        });
        diagnostics.sort_by(|left, right| {
            left.code
                .cmp(&right.code)
                .then_with(|| left.source_id.cmp(&right.source_id))
                .then_with(|| left.path.cmp(&right.path))
        });
        let sources = effective_sources
            .iter()
            .map(SkillSourceStatusV1::from)
            .collect::<Vec<_>>();
        let catalog_hash = build_catalog_hash(
            cwd_text.as_deref(),
            sources.as_slice(),
            entries.as_slice(),
            diagnostics.as_slice(),
        )?;
        Ok(Self {
            snapshot: SkillCatalogSnapshotV1 {
                schema: SKILL_CATALOG_SNAPSHOT_SCHEMA_V1.to_string(),
                cwd: cwd_text,
                catalog_hash,
                sources,
                skills: entries,
                diagnostics,
            },
        })
    }

    pub fn snapshot(&self) -> SkillCatalogSnapshotV1 {
        self.snapshot.clone()
    }

    pub fn entries(&self) -> &[SkillEntryV1] {
        self.snapshot.skills.as_slice()
    }

    pub fn catalog_items(&self) -> Vec<SkillCatalogItemV1> {
        self.snapshot
            .skills
            .iter()
            .filter(|entry| {
                entry.enabled
                    && entry.allow_implicit_invocation
                    && entry.shadowed_by.is_none()
                    && entry.errors.is_empty()
            })
            .map(|entry| SkillCatalogItemV1 {
                name: entry.name.clone(),
                description: entry.description.clone(),
                location: entry.skill_md_path.clone(),
            })
            .collect()
    }

    pub fn find_by_id(&self, skill_id: &str) -> Option<&SkillEntryV1> {
        let normalized = skill_id.trim();
        self.snapshot
            .skills
            .iter()
            .find(|entry| entry.skill_id == normalized)
    }

    pub fn detail(&self, skill_id: &str) -> Result<SkillDetailV1, String> {
        let entry = self
            .find_by_id(skill_id)
            .ok_or_else(|| format!("skill not found: {skill_id}"))?;
        let content = fs::read_to_string(entry.skill_md_path.as_str()).map_err(|error| {
            format!("read skill content failed {}: {error}", entry.skill_md_path)
        })?;
        Ok(SkillDetailV1 {
            skill: entry.clone(),
            content,
        })
    }
}

pub(crate) fn validate_source_path(
    kind: SkillSourceKindV1,
    path_raw: &str,
) -> Result<String, String> {
    let normalized = path_raw.trim();
    if normalized.is_empty() {
        return Err("skill source path is required".to_string());
    }
    let path = PathBuf::from(normalized);
    if !path.is_absolute() {
        return Err(format!("skill source path must be absolute: {normalized}"));
    }
    match kind {
        SkillSourceKindV1::CatalogDirectory if !path.is_dir() => {
            return Err(format!(
                "skill catalogDirectory must be an existing directory: {normalized}"
            ));
        }
        SkillSourceKindV1::SkillFile => {
            if !path.is_file() {
                return Err(format!(
                    "skill skillFile must be an existing file: {normalized}"
                ));
            }
            if path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                return Err(format!("skillFile must be named SKILL.md: {normalized}"));
            }
        }
        SkillSourceKindV1::CatalogDirectory => {}
    }
    canonical_local_path(path.as_path())
}

fn canonical_existing_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(format!(
            "skill catalog cwd must be an existing absolute directory: {}",
            path.display()
        ));
    }
    path.canonicalize().map_err(|error| {
        format!(
            "canonicalize skill catalog cwd failed {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn canonical_local_path(path: &Path) -> Result<String, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("canonicalize path failed {}: {error}", path.display()))?;
    Ok(path_to_model_string(canonical.as_path()))
}

fn source_applies(source: &SkillSourceConfigV1, cwd: Option<&Path>) -> Result<bool, String> {
    if source.scope != SkillSourceScopeV1::Workspace {
        return Ok(true);
    }
    let Some(source_root) = source.workspace_root.as_deref() else {
        return Ok(false);
    };
    let Some(cwd) = cwd else {
        return Ok(false);
    };
    let source_root = canonical_existing_directory(Path::new(source_root))?;
    Ok(canonical_paths_equal(source_root.as_path(), cwd))
}

fn skill_files_for_source(source: &SkillSourceConfigV1) -> Result<Vec<PathBuf>, String> {
    let path = PathBuf::from(source.path.as_str());
    match source.kind {
        SkillSourceKindV1::SkillFile => {
            validate_source_path(source.kind, source.path.as_str())?;
            Ok(vec![path])
        }
        SkillSourceKindV1::CatalogDirectory => {
            validate_source_path(source.kind, source.path.as_str())?;
            let mut files = Vec::new();
            for entry in fs::read_dir(path.as_path()).map_err(|error| {
                format!(
                    "read skill catalogDirectory failed sourceId={} path={}: {error}",
                    source.source_id, source.path
                )
            })? {
                let entry = entry.map_err(|error| {
                    format!(
                        "read skill catalog entry failed sourceId={}: {error}",
                        source.source_id
                    )
                })?;
                let file_type = entry.file_type().map_err(|error| {
                    format!(
                        "read skill catalog entry type failed sourceId={}: {error}",
                        source.source_id
                    )
                })?;
                if !file_type.is_dir() {
                    continue;
                }
                let candidate = entry.path().join("SKILL.md");
                if candidate.is_file() {
                    files.push(candidate);
                }
            }
            files.sort();
            Ok(files)
        }
    }
}

fn load_skill_file(
    source: &SkillSourceConfigV1,
    skill_file: &Path,
    policies: &HashMap<(String, String), bool>,
) -> Result<SkillEntryV1, String> {
    let metadata = fs::metadata(skill_file).map_err(|error| {
        format!(
            "read SKILL.md metadata failed {}: {error}",
            skill_file.display()
        )
    })?;
    if metadata.len() > MAX_SKILL_FILE_BYTES {
        return Err(format!(
            "SKILL.md exceeds {} bytes: {}",
            MAX_SKILL_FILE_BYTES,
            skill_file.display()
        ));
    }
    let raw = fs::read_to_string(skill_file)
        .map_err(|error| format!("read SKILL.md failed {}: {error}", skill_file.display()))?;
    let parsed = parse_skill_frontmatter(raw.as_str())?;
    validate_skill_name(parsed.name.as_str())?;
    if parsed.description.trim().is_empty() {
        return Err("SKILL.md frontmatter description is required".to_string());
    }
    if parsed.description.chars().count() > 1024 {
        return Err("SKILL.md description exceeds 1024 characters".to_string());
    }
    let parent_name = skill_file
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "SKILL.md parent directory is invalid: {}",
                skill_file.display()
            )
        })?;
    if parent_name != parsed.name {
        return Err(format!(
            "SKILL.md name must match parent directory: name={} directory={parent_name}",
            parsed.name
        ));
    }
    let normalized_name = normalize_skill_name(parsed.name.as_str());
    let enabled = policies
        .get(&(source.source_id.clone(), normalized_name.clone()))
        .copied()
        .unwrap_or(true);
    let skill_id = format!("{}:{}", source.source_id, normalized_name);
    Ok(SkillEntryV1 {
        skill_id,
        source_id: source.source_id.clone(),
        scope: source.scope,
        name: parsed.name,
        description: parsed.description,
        enabled,
        allow_implicit_invocation: !parsed.disable_model_invocation,
        capability_metadata: SkillCapabilityMetadata {
            allowed_tools: parsed.allowed_tools,
        },
        skill_md_path: path_to_model_string(skill_file),
        root_path: path_to_model_string(skill_file.parent().unwrap_or_else(|| Path::new(""))),
        content_hash: sha256_hex(raw.as_bytes()),
        shadowed_by: None,
        errors: Vec::new(),
    })
}

fn apply_name_precedence(entries: &mut [SkillEntryV1], diagnostics: &mut Vec<SkillDiagnosticV1>) {
    let mut winners = BTreeMap::<String, usize>::new();
    for index in 0..entries.len() {
        let normalized = normalize_skill_name(entries[index].name.as_str());
        if let Some(winner_index) = winners.get(&normalized).copied() {
            if entries[winner_index].scope.priority() == entries[index].scope.priority() {
                diagnostics.push(SkillDiagnosticV1 {
                    code: "skill_name_ambiguous".to_string(),
                    message: format!(
                        "same-priority Skill name conflict: name={} sourceIds={},{}; using sourceId={}",
                        entries[index].name,
                        entries[winner_index].source_id,
                        entries[index].source_id,
                        entries[winner_index].source_id
                    ),
                    source_id: Some(entries[index].source_id.clone()),
                    path: Some(entries[index].skill_md_path.clone()),
                });
            }
            entries[index].shadowed_by = Some(entries[winner_index].skill_id.clone());
        } else {
            winners.insert(normalized, index);
        }
    }
}

#[derive(Debug)]
struct ParsedSkillFrontmatter {
    name: String,
    description: String,
    allowed_tools: Vec<String>,
    disable_model_invocation: bool,
}

fn parse_skill_frontmatter(markdown: &str) -> Result<ParsedSkillFrontmatter, String> {
    let normalized = markdown.trim_start_matches('\u{feff}');
    let mut lines = normalized.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("SKILL.md must start with YAML frontmatter".to_string());
    }
    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line.to_string());
    }
    if !closed {
        return Err("SKILL.md frontmatter is not terminated".to_string());
    }
    let fields = parse_frontmatter_fields(frontmatter.as_slice())?;
    let name = fields
        .get("name")
        .cloned()
        .ok_or_else(|| "SKILL.md frontmatter name is required".to_string())?;
    let description = fields
        .get("description")
        .cloned()
        .ok_or_else(|| "SKILL.md frontmatter description is required".to_string())?;
    let allowed_tools = fields
        .get("allowed-tools")
        .map(|value| parse_allowed_tools(value.as_str()))
        .unwrap_or_default();
    let disable_model_invocation = match fields.get("disable-model-invocation") {
        None => false,
        Some(value) if value.eq_ignore_ascii_case("true") => true,
        Some(value) if value.eq_ignore_ascii_case("false") => false,
        Some(value) => {
            return Err(format!(
                "disable-model-invocation must be true or false, got {value}"
            ));
        }
    };
    Ok(ParsedSkillFrontmatter {
        name,
        description,
        allowed_tools,
        disable_model_invocation,
    })
}

fn parse_frontmatter_fields(lines: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut fields = BTreeMap::new();
    let mut index = 0usize;
    while index < lines.len() {
        let raw = lines[index].as_str();
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            index += 1;
            continue;
        }
        if raw.starts_with(' ') || raw.starts_with('\t') {
            return Err(format!("unexpected indented frontmatter line: {trimmed}"));
        }
        let (key, raw_value) = trimmed
            .split_once(':')
            .ok_or_else(|| format!("invalid frontmatter line: {trimmed}"))?;
        let key = key.trim().to_ascii_lowercase();
        if fields.contains_key(key.as_str()) {
            return Err(format!("duplicate SKILL.md frontmatter field: {key}"));
        }
        let value = raw_value.trim();
        if matches!(value, ">" | "|" | ">-" | "|-") {
            let fold = value.starts_with('>');
            let mut block = Vec::new();
            index += 1;
            while index < lines.len() {
                let block_line = lines[index].as_str();
                if !block_line.starts_with(' ') && !block_line.starts_with('\t') {
                    break;
                }
                block.push(block_line.trim().to_string());
                index += 1;
            }
            fields.insert(
                key,
                if fold {
                    block.join(" ")
                } else {
                    block.join("\n")
                },
            );
            continue;
        }
        if value.is_empty() {
            let mut nested_items = Vec::new();
            index += 1;
            while index < lines.len() {
                let nested_line = lines[index].as_str();
                if !nested_line.starts_with(' ') && !nested_line.starts_with('\t') {
                    break;
                }
                let nested = nested_line.trim();
                if let Some(item) = nested.strip_prefix('-').map(str::trim) {
                    if !item.is_empty() {
                        nested_items.push(item.to_string());
                    }
                }
                index += 1;
            }
            fields.insert(key, nested_items.join(","));
            continue;
        }
        fields.insert(key, unquote_yaml_scalar(value));
        index += 1;
    }
    Ok(fields)
}

fn unquote_yaml_scalar(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_allowed_tools(value: &str) -> Vec<String> {
    let normalized = value.trim().trim_start_matches('[').trim_end_matches(']');
    let mut tools = normalized
        .split(|character: char| character == ',' || character.is_whitespace())
        .map(|tool| tool.trim().trim_matches('"').trim_matches('\''))
        .filter(|tool| !tool.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();
    tools
}

pub(crate) fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("SKILL.md name must contain 1 to 64 characters".to_string());
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!("invalid SKILL.md name: {name}"));
    }
    if !name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    }) {
        return Err(format!("invalid SKILL.md name: {name}"));
    }
    Ok(())
}

fn build_catalog_hash(
    cwd: Option<&str>,
    sources: &[SkillSourceStatusV1],
    entries: &[SkillEntryV1],
    diagnostics: &[SkillDiagnosticV1],
) -> Result<String, String> {
    let value = serde_json::json!({
        "schema": SKILL_CATALOG_SNAPSHOT_SCHEMA_V1,
        "cwd": cwd,
        "sources": sources,
        "skills": entries,
        "diagnostics": diagnostics,
    });
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| format!("serialize skill catalog hash input failed: {error}"))?;
    Ok(sha256_hex(encoded.as_slice()))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn normalize_skill_name(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(windows)]
fn canonical_paths_equal(left: &Path, right: &Path) -> bool {
    path_to_model_string(left).eq_ignore_ascii_case(path_to_model_string(right).as_str())
}

#[cfg(not(windows))]
fn canonical_paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

fn path_to_model_string(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let without_verbatim_prefix = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        raw.strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| raw.into_owned())
    };
    without_verbatim_prefix.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::skills::{
        SkillPolicyV1, SkillSourceConfigV1, SkillSourceKindV1, SkillSourceScopeV1,
        SkillSourcesConfigV1,
    };

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris-skill-catalog-{label}-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(root.as_path()).expect("create temp root");
        root
    }

    fn write_skill(catalog: &Path, name: &str, description: &str) -> PathBuf {
        let dir = catalog.join(name);
        fs::create_dir_all(dir.as_path()).expect("create skill dir");
        let path = dir.join("SKILL.md");
        fs::write(
            path.as_path(),
            format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\n"),
        )
        .expect("write skill");
        path
    }

    fn source(source_id: &str, scope: SkillSourceScopeV1, path: &Path) -> SkillSourceConfigV1 {
        SkillSourceConfigV1 {
            source_id: source_id.to_string(),
            scope,
            kind: SkillSourceKindV1::CatalogDirectory,
            path: canonical_local_path(path).expect("canonical path"),
            workspace_root: None,
            enabled: true,
        }
    }

    fn workspace_source(
        source_id: &str,
        path: &Path,
        workspace_root: &Path,
    ) -> SkillSourceConfigV1 {
        SkillSourceConfigV1 {
            source_id: source_id.to_string(),
            scope: SkillSourceScopeV1::Workspace,
            kind: SkillSourceKindV1::CatalogDirectory,
            path: canonical_local_path(path).expect("canonical path"),
            workspace_root: Some(
                canonical_local_path(workspace_root).expect("canonical workspace root"),
            ),
            enabled: true,
        }
    }

    #[test]
    fn empty_config_never_guesses_skill_directories() {
        let root = temp_root("no-guess");
        write_skill(root.as_path(), "hidden-skill", "Must not be discovered.");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: Some(root.clone()),
            ..SkillCatalogLoadConfig::default()
        })
        .expect("load empty catalog");
        assert!(index.entries().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_catalog_loads_only_direct_skill_children() {
        let root = temp_root("explicit");
        write_skill(root.as_path(), "direct-skill", "Direct skill.");
        let nested_catalog = root.join("scope");
        fs::create_dir_all(nested_catalog.as_path()).expect("nested catalog");
        write_skill(
            nested_catalog.as_path(),
            "nested-skill",
            "Must not be discovered.",
        );
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![source(
                "source-user",
                SkillSourceScopeV1::User,
                root.as_path(),
            )],
            skill_policies: Vec::new(),
        };
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: config,
            max_skills: 16,
        })
        .expect("load explicit catalog");
        assert_eq!(index.entries().len(), 1);
        assert_eq!(index.entries()[0].name, "direct-skill");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_skill_file_loads_only_the_selected_manifest() {
        let root = temp_root("exact-skill-file");
        let selected = write_skill(root.as_path(), "selected-skill", "Selected skill.");
        write_skill(root.as_path(), "neighbor-skill", "Must not be discovered.");
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![SkillSourceConfigV1 {
                source_id: "source-file".to_string(),
                scope: SkillSourceScopeV1::User,
                kind: SkillSourceKindV1::SkillFile,
                path: canonical_local_path(selected.as_path()).expect("canonical manifest"),
                workspace_root: None,
                enabled: true,
            }],
            skill_policies: Vec::new(),
        };

        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: config,
            max_skills: 16,
        })
        .expect("load exact skill file");

        assert_eq!(index.entries().len(), 1);
        assert_eq!(index.entries()[0].name, "selected-skill");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_source_applies_only_to_its_exact_workspace() {
        let root = temp_root("workspace-scope");
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        let catalog_a = root.join("catalog-a");
        let catalog_b = root.join("catalog-b");
        let user_catalog = root.join("user-catalog");
        for directory in [
            &workspace_a,
            &workspace_b,
            &catalog_a,
            &catalog_b,
            &user_catalog,
        ] {
            fs::create_dir_all(directory).expect("test directory");
        }
        write_skill(
            catalog_a.as_path(),
            "workspace-a-skill",
            "Workspace A skill.",
        );
        write_skill(
            catalog_b.as_path(),
            "workspace-b-skill",
            "Workspace B skill.",
        );
        write_skill(user_catalog.as_path(), "user-skill", "User skill.");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: Some(workspace_a.clone()),
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![
                    workspace_source("workspace-a", catalog_a.as_path(), workspace_a.as_path()),
                    workspace_source("workspace-b", catalog_b.as_path(), workspace_b.as_path()),
                    source("user", SkillSourceScopeV1::User, user_catalog.as_path()),
                ],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("workspace-scoped catalog");
        let names = index
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["workspace-a-skill", "user-skill"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn catalog_hash_is_stable_when_files_do_not_change() {
        let root = temp_root("stable-hash");
        write_skill(root.as_path(), "stable-skill", "Stable skill.");
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![source(
                "source-user",
                SkillSourceScopeV1::User,
                root.as_path(),
            )],
            skill_policies: Vec::new(),
        };
        let load = || {
            SkillIndex::load(SkillCatalogLoadConfig {
                cwd: None,
                sources_config: config.clone(),
                max_skills: 16,
            })
            .expect("load catalog")
        };
        assert_eq!(
            load().snapshot().catalog_hash,
            load().snapshot().catalog_hash
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn catalog_hash_changes_when_skill_content_changes() {
        let root = temp_root("content-hash");
        let manifest = write_skill(root.as_path(), "content-skill", "Content skill.");
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![source(
                "source-user",
                SkillSourceScopeV1::User,
                root.as_path(),
            )],
            skill_policies: Vec::new(),
        };
        let load = || {
            SkillIndex::load(SkillCatalogLoadConfig {
                cwd: None,
                sources_config: config.clone(),
                max_skills: 16,
            })
            .expect("load catalog")
        };
        let before = load().snapshot().catalog_hash;
        fs::write(
            manifest,
            "---\nname: content-skill\ndescription: Content skill.\n---\nChanged body.\n",
        )
        .expect("change skill body");
        let after = load().snapshot().catalog_hash;
        assert_ne!(before, after);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn disabled_policy_keeps_skill_in_ui_snapshot_but_not_model_catalog() {
        let root = temp_root("disabled");
        write_skill(root.as_path(), "disabled-skill", "Disabled skill.");
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![source(
                "source-user",
                SkillSourceScopeV1::User,
                root.as_path(),
            )],
            skill_policies: vec![SkillPolicyV1 {
                source_id: "source-user".to_string(),
                skill_name: "disabled-skill".to_string(),
                enabled: false,
            }],
        };
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: config,
            max_skills: 16,
        })
        .expect("load catalog");
        assert_eq!(index.entries().len(), 1);
        assert!(!index.entries()[0].enabled);
        assert!(index.catalog_items().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn disable_model_invocation_keeps_detail_but_hides_prompt_metadata() {
        let root = temp_root("explicit-only");
        let skill_dir = root.join("explicit-only-skill");
        fs::create_dir_all(skill_dir.as_path()).expect("skill directory");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: explicit-only-skill\ndescription: Explicit-only skill.\ndisable-model-invocation: true\n---\nInstructions.\n",
        )
        .expect("skill file");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![source(
                    "source-user",
                    SkillSourceScopeV1::User,
                    root.as_path(),
                )],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("load explicit-only skill");
        assert_eq!(index.entries().len(), 1);
        assert!(!index.entries()[0].allow_implicit_invocation);
        assert!(index.catalog_items().is_empty());
        assert!(index
            .detail(index.entries()[0].skill_id.as_str())
            .expect("skill detail")
            .content
            .contains("Instructions."));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn higher_scope_shadows_lower_scope_without_hiding_ui_entry() {
        let root = temp_root("scope-precedence");
        let workspace = root.join("workspace");
        let workspace_catalog = root.join("workspace-catalog");
        let user_catalog = root.join("user-catalog");
        for directory in [&workspace, &workspace_catalog, &user_catalog] {
            fs::create_dir_all(directory).expect("test directory");
        }
        write_skill(
            workspace_catalog.as_path(),
            "shared-skill",
            "Workspace version.",
        );
        write_skill(user_catalog.as_path(), "shared-skill", "User version.");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: Some(workspace.clone()),
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![
                    source(
                        "source-user",
                        SkillSourceScopeV1::User,
                        user_catalog.as_path(),
                    ),
                    workspace_source(
                        "source-workspace",
                        workspace_catalog.as_path(),
                        workspace.as_path(),
                    ),
                ],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("load scope precedence");
        assert_eq!(index.entries().len(), 2);
        let workspace_entry = index
            .entries()
            .iter()
            .find(|entry| entry.scope == SkillSourceScopeV1::Workspace)
            .expect("workspace entry");
        let user_entry = index
            .entries()
            .iter()
            .find(|entry| entry.scope == SkillSourceScopeV1::User)
            .expect("user entry");
        assert!(workspace_entry.shadowed_by.is_none());
        assert_eq!(
            user_entry.shadowed_by.as_deref(),
            Some(workspace_entry.skill_id.as_str())
        );
        assert_eq!(index.catalog_items().len(), 1);
        assert_eq!(index.catalog_items()[0].description, "Workspace version.");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn same_scope_duplicate_name_is_diagnostic_and_keeps_one_model_skill() {
        let root = temp_root("same-scope-duplicate");
        let first_catalog = root.join("first");
        let second_catalog = root.join("second");
        fs::create_dir_all(first_catalog.as_path()).expect("first catalog");
        fs::create_dir_all(second_catalog.as_path()).expect("second catalog");
        write_skill(first_catalog.as_path(), "duplicate-skill", "First version.");
        write_skill(
            second_catalog.as_path(),
            "duplicate-skill",
            "Second version.",
        );
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![
                    source(
                        "source-first",
                        SkillSourceScopeV1::User,
                        first_catalog.as_path(),
                    ),
                    source(
                        "source-second",
                        SkillSourceScopeV1::User,
                        second_catalog.as_path(),
                    ),
                ],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("same-scope duplicate is isolated");
        assert_eq!(index.entries().len(), 2);
        assert_eq!(index.catalog_items().len(), 1);
        assert!(index
            .snapshot()
            .diagnostics
            .iter()
            .any(|item| item.code == "skill_name_ambiguous"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unavailable_source_is_diagnostic_and_keeps_healthy_skill() {
        let root = temp_root("unavailable-source");
        let healthy_catalog = root.join("healthy");
        fs::create_dir_all(healthy_catalog.as_path()).expect("healthy catalog");
        write_skill(healthy_catalog.as_path(), "healthy-skill", "Healthy skill.");
        let missing = root.join("missing");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![
                    SkillSourceConfigV1 {
                        source_id: "source-missing".to_string(),
                        scope: SkillSourceScopeV1::User,
                        kind: SkillSourceKindV1::CatalogDirectory,
                        path: missing.to_string_lossy().to_string(),
                        workspace_root: None,
                        enabled: true,
                    },
                    source(
                        "source-healthy",
                        SkillSourceScopeV1::User,
                        healthy_catalog.as_path(),
                    ),
                ],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("unavailable source is isolated");
        assert_eq!(index.catalog_items().len(), 1);
        assert_eq!(index.catalog_items()[0].name, "healthy-skill");
        let diagnostic = index
            .snapshot()
            .diagnostics
            .into_iter()
            .find(|item| item.code == "skill_source_unavailable")
            .expect("source diagnostic");
        assert_eq!(diagnostic.source_id.as_deref(), Some("source-missing"));
        assert_eq!(
            diagnostic.path.as_deref(),
            Some(missing.to_string_lossy().as_ref())
        );
        assert!(!missing.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_frontmatter_is_a_visible_diagnostic_without_fake_metadata() {
        let root = temp_root("invalid");
        let dir = root.join("broken-skill");
        fs::create_dir_all(dir.as_path()).expect("skill dir");
        fs::write(dir.join("SKILL.md"), "# missing frontmatter\n").expect("skill file");
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![source(
                "source-user",
                SkillSourceScopeV1::User,
                root.as_path(),
            )],
            skill_policies: Vec::new(),
        };
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: config,
            max_skills: 16,
        })
        .expect("load catalog with diagnostic");
        assert!(index.entries().is_empty());
        assert_eq!(
            index.snapshot().diagnostics[0].code,
            "skill_manifest_invalid"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_standard_metadata_does_not_hide_a_valid_skill() {
        let root = temp_root("nested-metadata");
        let skill_dir = root.join("metadata-skill");
        fs::create_dir_all(skill_dir.as_path()).expect("skill directory");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: metadata-skill\ndescription: Use this skill when metadata is present.\nmetadata:\n  short-description: Metadata example\nallowed-tools:\n  - read\n  - bash\n---\n# Metadata skill\n",
        )
        .expect("skill file");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![source(
                    "source-user",
                    SkillSourceScopeV1::User,
                    root.as_path(),
                )],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("skill index");
        assert_eq!(index.entries().len(), 1);
        assert_eq!(
            index.entries()[0].capability_metadata.allowed_tools,
            vec!["bash".to_string(), "read".to_string()]
        );
        let _ = fs::remove_dir_all(root);
    }
}
