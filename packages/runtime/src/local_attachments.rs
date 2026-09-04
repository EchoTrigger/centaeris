use centaeris_core::model::prepared_prompt::ModelInputImageResolverPort;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_IMAGES_PER_MESSAGE: usize = 8;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const INPUT_REF_PREFIX: &str = "local-image:";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LocalImageInputRequest {
    pub(crate) placeholder: String,
    pub(crate) local_path: String,
}

pub(crate) fn import_local_images(
    requests: &[LocalImageInputRequest],
    message: &str,
) -> Result<Vec<Value>, String> {
    if requests.len() > MAX_IMAGES_PER_MESSAGE {
        return Err(format!(
            "too many input images: maximum {MAX_IMAGES_PER_MESSAGE}"
        ));
    }
    let mut placeholders = HashSet::new();
    for request in requests {
        if request.placeholder.trim().is_empty()
            || message.match_indices(request.placeholder.as_str()).count() != 1
            || !placeholders.insert(request.placeholder.as_str())
        {
            return Err("input image placeholder is invalid".to_string());
        }
    }
    requests
        .iter()
        .enumerate()
        .map(|(index, request)| import_local_image(request, index + 1))
        .collect()
}

fn import_local_image(request: &LocalImageInputRequest, number: usize) -> Result<Value, String> {
    let placeholder = request.placeholder.as_str();
    let source = PathBuf::from(request.local_path.as_str());
    let metadata = fs::metadata(source.as_path())
        .map_err(|error| format!("read input image metadata failed: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "input image size is invalid: bytes={} maximum={MAX_IMAGE_BYTES}",
            metadata.len()
        ));
    }
    let bytes =
        fs::read(source.as_path()).map_err(|error| format!("read input image failed: {error}"))?;
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err("input image must be PNG".to_string());
    }
    let digest = hex_sha256(bytes.as_slice());
    let destination = managed_image_path(digest.as_str())?;
    if destination.exists() {
        verify_managed_image(destination.as_path(), digest.as_str())?;
    } else {
        crate::atomic_file::write_file_atomically(
            destination.as_path(),
            bytes.as_slice(),
            "managed input image",
        )?;
    }
    Ok(json!({
        "inputRef": format!("{INPUT_REF_PREFIX}{digest}"),
        "displayName": format!("Image {number}"),
        "contentType": "image/png",
        "placeholder": placeholder,
    }))
}

#[derive(Default)]
pub(crate) struct LocalModelInputImageResolver;

impl ModelInputImageResolverPort for LocalModelInputImageResolver {
    fn resolve(&self, input_ref: &str, content_type: &str) -> Result<Vec<u8>, String> {
        if content_type != "image/png" {
            return Err(format!(
                "unsupported managed image contentType: {content_type}"
            ));
        }
        let digest = input_ref
            .strip_prefix(INPUT_REF_PREFIX)
            .filter(|value| is_sha256(value))
            .ok_or_else(|| format!("invalid managed image inputRef: {input_ref}"))?;
        let path = managed_image_path(digest)?;
        verify_managed_image(path.as_path(), digest)?;
        fs::read(path).map_err(|error| format!("read managed input image failed: {error}"))
    }
}

fn managed_image_path(digest: &str) -> Result<PathBuf, String> {
    if !is_sha256(digest) {
        return Err("managed input image digest is invalid".to_string());
    }
    Ok(crate::user_data_layout::runtime_inputs_dir_path().join(format!("{digest}.png")))
}

fn verify_managed_image(path: &Path, expected_digest: &str) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("read managed input image metadata failed: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "managed input image size is invalid: bytes={} maximum={MAX_IMAGE_BYTES}",
            metadata.len()
        ));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("read managed input image failed: {error}"))?;
    if !bytes.starts_with(PNG_SIGNATURE) || hex_sha256(bytes.as_slice()) != expected_digest {
        return Err(format!(
            "managed input image is corrupt: {}",
            path.display()
        ));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_image_verification_detects_corruption() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-managed-image-test-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend_from_slice(b"image");
        std::fs::write(path.as_path(), bytes.as_slice()).expect("write image fixture");
        let digest = hex_sha256(bytes.as_slice());
        verify_managed_image(path.as_path(), digest.as_str()).expect("verify image");
        assert!(verify_managed_image(path.as_path(), &"b".repeat(64)).is_err());
        let _ = std::fs::remove_file(path);
    }
}
