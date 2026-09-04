use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(windows))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const CONTENT_DIGEST_DOMAIN: &[u8] = b"centaeris.model_observation_content.v1\0";
const MANIFEST_DIGEST_DOMAIN: &[u8] = b"centaeris.model_observation_manifest.v1\0";
const CONTENT_DIRECTORY_EXTENSION: &str = "observations";
const MANIFEST_FILE_PREFIX: &str = "manifest-";
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservationContent {
    digest: String,
    kind: String,
    content_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservationReference {
    kind: String,
    digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservationChange {
    index: usize,
    reference: ObservationReference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservationManifest {
    digest: String,
    parent_digest: Option<String>,
    observation_count: usize,
    changes: Vec<ObservationChange>,
    content_json: String,
}

pub(super) fn validate_session_log_path(path: &Path, session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
        || path.file_stem().and_then(|value| value.to_str()) != Some(session_id)
    {
        return Err(format!(
            "Session log path does not match its validated sessionId: {}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn compact_and_install_wires(
    log_path: &Path,
    session_id: &str,
    wires: &mut [Value],
) -> Result<(), String> {
    validate_session_log_path(log_path, session_id)?;
    if !wires
        .iter()
        .any(|wire| wire.get("type").and_then(Value::as_str) == Some("model_request_started"))
    {
        return Ok(());
    }
    let mut contents = BTreeMap::<String, ObservationContent>::new();
    let content_dir = content_directory(log_path, session_id)?;
    let mut manifest_cache = BTreeMap::<String, Vec<ObservationReference>>::new();
    let mut parent_digest = latest_manifest_digest(log_path, session_id)?;
    let mut parent_references = match parent_digest.as_deref() {
        Some(digest) => {
            resolve_manifest_references(content_dir.as_path(), digest, &mut manifest_cache)?
        }
        None => Vec::new(),
    };
    let mut manifests = Vec::new();
    for wire in wires {
        if wire.get("type").and_then(Value::as_str) != Some("model_request_started") {
            continue;
        }
        require_wire_session(wire, session_id)?;
        let observations = wire
            .pointer("/payload/observations")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "model request storage observations are required".to_string())?;
        let (new_contents, manifest, references) =
            prepare_observation_manifest(&observations, parent_digest.clone(), &parent_references)?;
        for content in new_contents {
            if let Some(existing) = contents.get(content.digest.as_str()) {
                if existing != &content {
                    return Err("model observation content digest conflict".to_string());
                }
            } else {
                contents.insert(content.digest.clone(), content);
            }
        }
        wire.pointer_mut("/payload/observations")
            .ok_or_else(|| "model request storage observations are missing".to_string())?
            .clone_from(&serde_json::json!({"manifestDigest": manifest.digest}));
        parent_digest = Some(manifest.digest.clone());
        parent_references = references.clone();
        manifest_cache.insert(manifest.digest.clone(), references);
        manifests.push(manifest);
    }
    if contents.is_empty() && manifests.is_empty() {
        return Ok(());
    }
    ensure_content_directory(content_dir.as_path())?;
    for content in contents.values() {
        install_content(content_dir.as_path(), content)?;
    }
    for manifest in &manifests {
        install_manifest(content_dir.as_path(), manifest)?;
    }
    Ok(())
}

fn prepare_observation_manifest(
    observations: &[Value],
    parent_digest: Option<String>,
    parent: &[ObservationReference],
) -> Result<
    (
        Vec<ObservationContent>,
        ObservationManifest,
        Vec<ObservationReference>,
    ),
    String,
> {
    let mut known_digests = parent
        .iter()
        .map(|reference| reference.digest.clone())
        .collect::<BTreeSet<_>>();
    let mut contents = Vec::new();
    let mut references = Vec::with_capacity(observations.len());
    for observation in observations {
        let content = observation_content(observation)?;
        references.push(ObservationReference {
            kind: content.kind.clone(),
            digest: content.digest.clone(),
        });
        if known_digests.insert(content.digest.clone()) {
            contents.push(content);
        }
    }
    let manifest = build_manifest(parent_digest, parent, &references)?;
    Ok((contents, manifest, references))
}

pub(super) fn hydrate_wires(
    log_path: &Path,
    session_id: &str,
    wires: &mut [Value],
) -> Result<(), String> {
    validate_session_log_path(log_path, session_id)?;
    let content_dir = content_directory(log_path, session_id)?;
    let mut manifest_cache = BTreeMap::<String, Vec<ObservationReference>>::new();
    let mut references_by_root = BTreeMap::new();
    let mut requested = BTreeMap::<String, String>::new();
    for wire in wires.iter() {
        if wire.get("type").and_then(Value::as_str) != Some("model_request_started") {
            continue;
        }
        require_wire_session(wire, session_id)?;
        let root = observation_manifest_digest(wire)?;
        let references =
            resolve_manifest_references(content_dir.as_path(), root.as_str(), &mut manifest_cache)?;
        for reference in &references {
            if let Some(existing_kind) = requested.get(reference.digest.as_str()) {
                if existing_kind != &reference.kind {
                    return Err("stored model observation content kind conflict".to_string());
                }
            } else {
                requested.insert(reference.digest.clone(), reference.kind.clone());
            }
        }
        references_by_root.insert(root, references);
    }
    if references_by_root.is_empty() {
        return Ok(());
    }
    let mut contents = BTreeMap::new();
    for (digest, expected_kind) in &requested {
        let path = content_file_path(content_dir.as_path(), digest)?;
        let raw = fs::read_to_string(path.as_path()).map_err(|error| {
            format!(
                "read model observation content failed for {}: {error}",
                path.display()
            )
        })?;
        let content = decode_content(digest, raw)?;
        if content.kind != *expected_kind {
            return Err("stored model observation content kind conflict".to_string());
        }
        contents.insert(digest.clone(), content);
    }
    for wire in wires {
        if wire.get("type").and_then(Value::as_str) != Some("model_request_started") {
            continue;
        }
        let root = observation_manifest_digest(wire)?;
        let observations = references_by_root
            .get(root.as_str())
            .ok_or_else(|| "stored model observation manifest is missing".to_string())?
            .iter()
            .map(|reference| {
                let content = contents
                    .get(reference.digest.as_str())
                    .ok_or_else(|| "stored model observation content is missing".to_string())?;
                if content.kind != reference.kind {
                    return Err("stored model observation content binding mismatch".to_string());
                }
                serde_json::from_str::<Value>(content.content_json.as_str()).map_err(|error| {
                    format!("decode stored model observation content failed: {error}")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        wire.pointer_mut("/payload/observations")
            .ok_or_else(|| "stored model request observations are missing".to_string())?
            .clone_from(&Value::Array(observations));
    }
    Ok(())
}

pub(super) fn delete_session_document(path: &Path, session_id: &str) -> Result<(), String> {
    validate_session_log_path(path, session_id)?;
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path, "deleted Session log parent")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "delete session log failed for {}: {error}",
                path.display()
            ))
        }
    }
    let content_dir = content_directory(path, session_id)?;
    match fs::remove_dir_all(content_dir.as_path()) {
        Ok(()) => sync_parent(content_dir.as_path(), "deleted observation CAS parent")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "delete model observation CAS failed for {}: {error}",
                content_dir.display()
            ))
        }
    }
    Ok(())
}

pub(super) fn cleanup_orphan_content_directories(sessions_dir: &Path) -> Result<(), String> {
    if !sessions_dir.exists() {
        return Ok(());
    }
    for year in child_directories(sessions_dir)? {
        for month in child_directories(year.as_path())? {
            for day in child_directories(month.as_path())? {
                for entry in fs::read_dir(day.as_path()).map_err(|error| {
                    format!("read Session observation directory failed: {error}")
                })? {
                    let entry = entry.map_err(|error| {
                        format!("read Session observation entry failed: {error}")
                    })?;
                    let path = entry.path();
                    let file_type = entry.file_type().map_err(|error| {
                        format!("read Session observation entry type failed: {error}")
                    })?;
                    if file_type.is_symlink() {
                        return Err("Session observation GC refuses symbolic links".to_string());
                    }
                    if !file_type.is_dir()
                        || path.extension().and_then(|value| value.to_str())
                            != Some(CONTENT_DIRECTORY_EXTENSION)
                    {
                        continue;
                    }
                    let Some(session_id) = path.file_stem().and_then(|value| value.to_str()) else {
                        continue;
                    };
                    if session_id.is_empty()
                        || !session_id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                        })
                    {
                        continue;
                    }
                    let log_path = day.join(format!("{session_id}.jsonl"));
                    if log_path.exists() {
                        if fs::symlink_metadata(&log_path)
                            .map_err(|error| format!("stat Session log failed: {error}"))?
                            .file_type()
                            .is_symlink()
                        {
                            return Err("Session observation GC refuses symbolic links".to_string());
                        }
                        cleanup_session_content_directory(
                            log_path.as_path(),
                            session_id,
                            path.as_path(),
                        )?;
                        continue;
                    }
                    fs::remove_dir_all(path.as_path()).map_err(|error| {
                        format!(
                            "clean orphan model observation CAS failed for {}: {error}",
                            path.display()
                        )
                    })?;
                    sync_directory(day.as_path(), "orphan observation CAS parent")?;
                }
            }
        }
    }
    Ok(())
}

fn cleanup_session_content_directory(
    log_path: &Path,
    session_id: &str,
    content_dir: &Path,
) -> Result<(), String> {
    let roots = stored_manifest_digests(log_path, session_id)?;
    if roots.is_empty() {
        fs::remove_dir_all(content_dir).map_err(|error| {
            format!(
                "clean unreferenced model observation CAS failed for {}: {error}",
                content_dir.display()
            )
        })?;
        return sync_parent(content_dir, "unreferenced observation CAS parent");
    }
    let mut cache = BTreeMap::new();
    let mut reachable_contents = BTreeSet::new();
    let mut reachable_manifests = BTreeSet::new();
    for root in &roots {
        for reference in resolve_manifest_references(content_dir, root, &mut cache)? {
            if !reachable_contents.contains(&reference.digest) {
                let path = content_file_path(content_dir, &reference.digest)?;
                let content = decode_content(
                    &reference.digest,
                    fs::read_to_string(path).map_err(|error| {
                        format!("read reachable model observation content failed: {error}")
                    })?,
                )?;
                if content.kind != reference.kind {
                    return Err("stored model observation content kind conflict".to_string());
                }
            }
            reachable_contents.insert(reference.digest);
        }
        let mut next = Some(root.clone());
        while let Some(digest) = next {
            if !reachable_manifests.insert(digest.clone()) {
                break;
            }
            let path = manifest_file_path(content_dir, &digest)?;
            next = decode_manifest(
                &digest,
                fs::read_to_string(path).map_err(|error| {
                    format!("read reachable model observation manifest failed: {error}")
                })?,
            )?
            .parent_digest;
        }
    }
    let mut removed = false;
    for entry in fs::read_dir(content_dir)
        .map_err(|error| format!("read Session observation CAS failed: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("read Session observation CAS entry failed: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("read Session observation CAS entry type failed: {error}"))?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(format!(
                "Session observation CAS contains a non-file entry: {}",
                path.display()
            ));
        }
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| "Session observation CAS filename is not UTF-8".to_string())?
            .to_string();
        let orphan = if name.starts_with('.') && name.ends_with(".tmp") {
            true
        } else if let Some(hex) = name
            .strip_prefix(MANIFEST_FILE_PREFIX)
            .and_then(|value| value.strip_suffix(".json"))
        {
            valid_digest_hex(hex)
                .then(|| !reachable_manifests.contains(format!("sha256:{hex}").as_str()))
                .ok_or_else(|| "Session observation manifest filename is invalid".to_string())?
        } else if let Some(hex) = name.strip_suffix(".json") {
            valid_digest_hex(hex)
                .then(|| !reachable_contents.contains(format!("sha256:{hex}").as_str()))
                .ok_or_else(|| "Session observation content filename is invalid".to_string())?
        } else {
            return Err("Session observation CAS filename is invalid".to_string());
        };
        if orphan {
            fs::remove_file(path.as_path()).map_err(|error| {
                format!(
                    "clean orphan model observation CAS file failed for {}: {error}",
                    path.display()
                )
            })?;
            removed = true;
        }
    }
    if removed {
        sync_directory(content_dir, "cleaned observation CAS directory")?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn content_directory_path(log_path: &Path, session_id: &str) -> Result<PathBuf, String> {
    content_directory(log_path, session_id)
}

fn observation_content(observation: &Value) -> Result<ObservationContent, String> {
    if !observation.is_object() || observation.get("contentDigest").is_some() {
        return Err("model observation storage payload is invalid".to_string());
    }
    let kind = observation
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| valid_kind(kind))
        .ok_or_else(|| "model observation storage kind is invalid".to_string())?
        .to_string();
    let content_json = serde_json::to_string(observation)
        .map_err(|error| format!("encode model observation content failed: {error}"))?;
    let digest = digest_content(content_json.as_str());
    Ok(ObservationContent {
        digest,
        kind,
        content_json,
    })
}

fn build_manifest(
    parent_digest: Option<String>,
    parent: &[ObservationReference],
    references: &[ObservationReference],
) -> Result<ObservationManifest, String> {
    let changes = references
        .iter()
        .enumerate()
        .filter(|(index, reference)| parent.get(*index) != Some(*reference))
        .map(|(index, reference)| ObservationChange {
            index,
            reference: reference.clone(),
        })
        .collect::<Vec<_>>();
    let content_json = serde_json::to_string(&serde_json::json!({
        "parentDigest": parent_digest,
        "observationCount": references.len(),
        "changes": changes.iter().map(|change| serde_json::json!({
            "index": change.index,
            "kind": change.reference.kind,
            "contentDigest": change.reference.digest,
        })).collect::<Vec<_>>(),
    }))
    .map_err(|error| format!("encode model observation manifest failed: {error}"))?;
    let digest = digest_manifest(content_json.as_str());
    Ok(ObservationManifest {
        digest,
        parent_digest,
        observation_count: references.len(),
        changes,
        content_json,
    })
}

fn observation_manifest_digest(wire: &Value) -> Result<String, String> {
    let object = wire
        .pointer("/payload/observations")
        .and_then(Value::as_object)
        .ok_or_else(|| "stored model request observation manifest is required".to_string())?;
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["manifestDigest"])
    {
        return Err("stored model request observation manifest fields mismatch".to_string());
    }
    let digest = object
        .get("manifestDigest")
        .and_then(Value::as_str)
        .ok_or_else(|| "stored model request observation manifestDigest is required".to_string())?;
    validate_digest(digest)?;
    Ok(digest.to_string())
}

fn latest_manifest_digest(log_path: &Path, session_id: &str) -> Result<Option<String>, String> {
    Ok(stored_manifest_digests(log_path, session_id)?.pop())
}

fn stored_manifest_digests(log_path: &Path, session_id: &str) -> Result<Vec<String>, String> {
    let contents = fs::read_to_string(log_path).map_err(|error| {
        format!(
            "read Session log for observation manifest failed for {}: {error}",
            log_path.display()
        )
    })?;
    let mut roots = Vec::new();
    for (index, line) in contents.lines().enumerate().skip(1) {
        let wire = serde_json::from_str::<Value>(line).map_err(|error| {
            format!(
                "decode Session observation manifest line {} failed: {error}",
                index + 1
            )
        })?;
        if wire.get("type").and_then(Value::as_str) == Some("model_request_started") {
            require_wire_session(&wire, session_id)?;
            roots.push(observation_manifest_digest(&wire)?);
        }
    }
    Ok(roots)
}

fn resolve_manifest_references(
    content_dir: &Path,
    digest: &str,
    cache: &mut BTreeMap<String, Vec<ObservationReference>>,
) -> Result<Vec<ObservationReference>, String> {
    validate_digest(digest)?;
    if let Some(references) = cache.get(digest) {
        return Ok(references.clone());
    }
    let mut chain = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut next = Some(digest.to_string());
    let mut references = Vec::new();
    while let Some(current) = next {
        if let Some(cached) = cache.get(current.as_str()) {
            references = cached.clone();
            break;
        }
        if !visiting.insert(current.clone()) {
            return Err("stored model observation manifest parent cycle".to_string());
        }
        let path = manifest_file_path(content_dir, current.as_str())?;
        let raw = fs::read_to_string(path.as_path()).map_err(|error| {
            format!(
                "read model observation manifest failed for {}: {error}",
                path.display()
            )
        })?;
        let manifest = decode_manifest(current.as_str(), raw)?;
        next = manifest.parent_digest.clone();
        chain.push(manifest);
    }
    for manifest in chain.into_iter().rev() {
        references = apply_manifest(&manifest, references)?;
    }
    cache.insert(digest.to_string(), references);
    cache
        .get(digest)
        .cloned()
        .ok_or_else(|| "stored model observation manifest is missing".to_string())
}

fn apply_manifest(
    manifest: &ObservationManifest,
    mut references: Vec<ObservationReference>,
) -> Result<Vec<ObservationReference>, String> {
    if manifest.parent_digest.is_none() && !references.is_empty() {
        return Err("root model observation manifest has a parent state".to_string());
    }
    references.truncate(manifest.observation_count);
    for change in &manifest.changes {
        if change.index < references.len() {
            if references[change.index] == change.reference {
                return Err("model observation manifest contains a redundant change".to_string());
            }
            references[change.index] = change.reference.clone();
        } else if change.index == references.len() {
            references.push(change.reference.clone());
        } else {
            return Err("model observation manifest append range has a hole".to_string());
        }
    }
    if references.len() != manifest.observation_count {
        return Err("model observation manifest count mismatch".to_string());
    }
    Ok(references)
}

fn decode_manifest(digest: &str, content_json: String) -> Result<ObservationManifest, String> {
    validate_digest(digest)?;
    if digest_manifest(content_json.as_str()) != digest {
        return Err("stored model observation manifest digest mismatch".to_string());
    }
    let value = serde_json::from_str::<Value>(content_json.as_str())
        .map_err(|error| format!("decode stored model observation manifest failed: {error}"))?;
    if serde_json::to_string(&value)
        .map_err(|error| format!("encode stored model observation manifest failed: {error}"))?
        != content_json
    {
        return Err("stored model observation manifest is not canonical JSON".to_string());
    }
    let object = value
        .as_object()
        .ok_or_else(|| "stored model observation manifest is invalid".to_string())?;
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["changes", "observationCount", "parentDigest"])
    {
        return Err("stored model observation manifest fields mismatch".to_string());
    }
    let parent_digest = match object.get("parentDigest") {
        Some(Value::Null) => None,
        Some(Value::String(value)) => {
            validate_digest(value)?;
            if value == digest {
                return Err("stored model observation manifest self-parent".to_string());
            }
            Some(value.clone())
        }
        _ => return Err("stored model observation manifest parentDigest is invalid".to_string()),
    };
    let observation_count = object
        .get("observationCount")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "stored model observation manifest count is invalid".to_string())?;
    let change_values = object
        .get("changes")
        .and_then(Value::as_array)
        .ok_or_else(|| "stored model observation manifest changes are invalid".to_string())?;
    let mut changes = Vec::with_capacity(change_values.len());
    let mut previous_index = None;
    for value in change_values {
        let object = value
            .as_object()
            .ok_or_else(|| "stored model observation manifest change is invalid".to_string())?;
        if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != BTreeSet::from(["contentDigest", "index", "kind"])
        {
            return Err("stored model observation manifest change fields mismatch".to_string());
        }
        let index = object
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|index| *index < observation_count)
            .ok_or_else(|| {
                "stored model observation manifest change index is invalid".to_string()
            })?;
        if previous_index.is_some_and(|previous| index <= previous) {
            return Err("stored model observation manifest changes are not ordered".to_string());
        }
        previous_index = Some(index);
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| valid_kind(kind))
            .ok_or_else(|| {
                "stored model observation manifest change kind is invalid".to_string()
            })?;
        let reference_digest = object
            .get("contentDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "stored model observation manifest change contentDigest is invalid".to_string()
            })?;
        validate_digest(reference_digest)?;
        changes.push(ObservationChange {
            index,
            reference: ObservationReference {
                kind: kind.to_string(),
                digest: reference_digest.to_string(),
            },
        });
    }
    if parent_digest.is_none()
        && changes
            .iter()
            .enumerate()
            .any(|(index, change)| change.index != index)
    {
        return Err("root model observation manifest append range has a hole".to_string());
    }
    Ok(ObservationManifest {
        digest: digest.to_string(),
        parent_digest,
        observation_count,
        changes,
        content_json,
    })
}

fn install_content(content_dir: &Path, expected: &ObservationContent) -> Result<(), String> {
    let final_path = content_file_path(content_dir, expected.digest.as_str())?;
    if final_path.exists() {
        return verify_existing_content(final_path.as_path(), expected);
    }
    install_json_atomically(
        content_dir,
        final_path.as_path(),
        expected.digest.as_str(),
        expected.content_json.as_str(),
    )?;
    verify_existing_content(final_path.as_path(), expected)
}

fn install_manifest(content_dir: &Path, expected: &ObservationManifest) -> Result<(), String> {
    let final_path = manifest_file_path(content_dir, expected.digest.as_str())?;
    if final_path.exists() {
        return verify_existing_manifest(final_path.as_path(), expected);
    }
    install_json_atomically(
        content_dir,
        final_path.as_path(),
        expected.digest.as_str(),
        expected.content_json.as_str(),
    )?;
    verify_existing_manifest(final_path.as_path(), expected)
}

fn install_json_atomically(
    content_dir: &Path,
    final_path: &Path,
    digest: &str,
    content_json: &str,
) -> Result<(), String> {
    let digest_hex = digest.strip_prefix("sha256:").expect("validated digest");
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = content_dir.join(format!(
        ".{digest_hex}.{}.{}.tmp",
        std::process::id(),
        counter
    ));
    let mut temp = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(temp_path.as_path())
        .map_err(|error| {
            format!(
                "create model observation content temp failed for {}: {error}",
                temp_path.display()
            )
        })?;
    let write_result = temp
        .write_all(content_json.as_bytes())
        .and_then(|_| temp.sync_all());
    if let Err(error) = write_result {
        drop(temp);
        let _ = fs::remove_file(temp_path.as_path());
        return Err(format!(
            "write model observation content temp failed for {}: {error}",
            temp_path.display()
        ));
    }
    drop(temp);
    match durable_rename(temp_path.as_path(), final_path) {
        Ok(()) => sync_directory(content_dir, "model observation CAS directory"),
        Err(_error) if final_path.exists() => {
            let _ = fs::remove_file(temp_path.as_path());
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(temp_path.as_path());
            Err(format!(
                "install model observation content failed for {}: {error}",
                final_path.display()
            ))
        }
    }
}

fn verify_existing_manifest(path: &Path, expected: &ObservationManifest) -> Result<(), String> {
    let raw = fs::read_to_string(path).map_err(|error| {
        format!(
            "read existing model observation manifest failed for {}: {error}",
            path.display()
        )
    })?;
    if decode_manifest(expected.digest.as_str(), raw)? != *expected {
        return Err("model observation manifest digest conflict".to_string());
    }
    Ok(())
}

fn verify_existing_content(path: &Path, expected: &ObservationContent) -> Result<(), String> {
    let raw = fs::read_to_string(path).map_err(|error| {
        format!(
            "read existing model observation content failed for {}: {error}",
            path.display()
        )
    })?;
    let stored = decode_content(expected.digest.as_str(), raw)?;
    if stored != *expected {
        return Err("model observation content digest conflict".to_string());
    }
    Ok(())
}

fn decode_content(digest: &str, content_json: String) -> Result<ObservationContent, String> {
    validate_digest(digest)?;
    if digest_content(content_json.as_str()) != digest {
        return Err("stored model observation content digest mismatch".to_string());
    }
    let value = serde_json::from_str::<Value>(content_json.as_str())
        .map_err(|error| format!("decode stored model observation content failed: {error}"))?;
    if serde_json::to_string(&value)
        .map_err(|error| format!("encode stored model observation content failed: {error}"))?
        != content_json
    {
        return Err("stored model observation content is not canonical JSON".to_string());
    }
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| valid_kind(kind))
        .ok_or_else(|| "stored model observation content kind is invalid".to_string())?
        .to_string();
    if !value.is_object() || value.get("contentDigest").is_some() {
        return Err("stored model observation content fields mismatch".to_string());
    }
    Ok(ObservationContent {
        digest: digest.to_string(),
        kind,
        content_json,
    })
}

fn require_wire_session(wire: &Value, expected_session_id: &str) -> Result<(), String> {
    if wire.get("sessionId").and_then(Value::as_str) != Some(expected_session_id) {
        return Err("stored model request sessionId mismatch".to_string());
    }
    Ok(())
}

fn valid_kind(kind: &str) -> bool {
    matches!(
        kind,
        "system_prompt" | "message" | "input_image" | "tool_catalog" | "compaction_prompt"
    )
}

fn digest_content(content_json: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTENT_DIGEST_DOMAIN);
    digest.update(content_json.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

fn digest_manifest(content_json: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(MANIFEST_DIGEST_DOMAIN);
    digest.update(content_json.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

fn validate_digest(digest: &str) -> Result<(), String> {
    if digest
        .strip_prefix("sha256:")
        .is_none_or(|hex| !valid_digest_hex(hex))
    {
        return Err("model observation content digest is invalid".to_string());
    }
    Ok(())
}

fn valid_digest_hex(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn content_directory(log_path: &Path, session_id: &str) -> Result<PathBuf, String> {
    validate_session_log_path(log_path, session_id)?;
    let path = log_path.with_extension(CONTENT_DIRECTORY_EXTENSION);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("model observation CAS must be a real directory".to_string())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("stat model observation CAS failed: {error}")),
    }
    Ok(path)
}

fn content_file_path(content_dir: &Path, digest: &str) -> Result<PathBuf, String> {
    validate_digest(digest)?;
    Ok(content_dir.join(format!(
        "{}.json",
        digest.strip_prefix("sha256:").expect("validated digest")
    )))
}

fn manifest_file_path(content_dir: &Path, digest: &str) -> Result<PathBuf, String> {
    validate_digest(digest)?;
    Ok(content_dir.join(format!(
        "{MANIFEST_FILE_PREFIX}{}.json",
        digest.strip_prefix("sha256:").expect("validated digest")
    )))
}

fn ensure_content_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => sync_parent(path, "model observation CAS parent")?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| format!("stat model observation CAS failed: {error}"))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("model observation CAS must be a real directory".to_string());
            }
        }
        Err(error) => {
            return Err(format!(
                "create model observation CAS failed for {}: {error}",
                path.display()
            ))
        }
    }
    Ok(())
}

fn child_directories(path: &Path) -> Result<Vec<PathBuf>, String> {
    if fs::symlink_metadata(path)
        .map_err(|error| format!("stat Session directory failed: {error}"))?
        .file_type()
        .is_symlink()
    {
        return Err("Session observation GC refuses symbolic links".to_string());
    }
    fs::read_dir(path)
        .map_err(|error| {
            format!(
                "read Session directory failed for {}: {error}",
                path.display()
            )
        })?
        .filter_map(|entry| match entry {
            Ok(entry) => match entry.file_type() {
                Ok(file_type) if file_type.is_symlink() => Some(Err(
                    "Session observation GC refuses symbolic links".to_string(),
                )),
                Ok(file_type) if file_type.is_dir() => Some(Ok(entry.path())),
                Ok(_) => None,
                Err(error) => Some(Err(format!("read Session directory type failed: {error}"))),
            },
            Err(error) => Some(Err(format!("read Session directory entry failed: {error}"))),
        })
        .collect()
}

fn sync_parent(path: &Path, label: &str) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("{label} is missing"))?;
    sync_directory(parent, label)
}

#[cfg(windows)]
fn durable_rename(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn durable_rename(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn sync_directory(path: &Path, label: &str) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|error| format!("open {label} failed for {}: {error}", path.display()))?;
    match directory.sync_all() {
        Ok(()) => Ok(()),
        // Windows FlushFileBuffers rejects directory handles. CAS installs use
        // MoveFileExW(MOVEFILE_WRITE_THROUGH), the platform write-through
        // equivalent for the directory entry created by the atomic rename.
        Err(error) if error.raw_os_error() == Some(5) => Ok(()),
        Err(error) => Err(format!(
            "sync {label} failed for {}: {error}",
            path.display()
        )),
    }
}

#[cfg(not(windows))]
fn sync_directory(path: &Path, label: &str) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {label} failed for {}: {error}", path.display()))
}

#[cfg(test)]
mod tests;
