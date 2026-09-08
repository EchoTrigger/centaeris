use centaeris_core::model::prepared_prompt::{
    inspect_model_input_image, ModelInputImageResolverPort, MODEL_INPUT_IMAGE_MAX_BYTES,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_IMAGE_BYTES: u64 = MODEL_INPUT_IMAGE_MAX_BYTES as u64;
const MAX_IMAGES_PER_MESSAGE: usize = 8;
#[cfg(test)]
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
    import_local_image_at(
        request,
        number,
        &crate::user_data_layout::runtime_inputs_dir_path(),
    )
}

fn import_local_image_at(
    request: &LocalImageInputRequest,
    number: usize,
    inputs: &Path,
) -> Result<Value, String> {
    let placeholder = request.placeholder.as_str();
    let source = PathBuf::from(request.local_path.as_str());
    let bytes = read_png(source.as_path())?;
    let digest = hex_sha256(bytes.as_slice());
    let destination = inputs.join(format!("{digest}.png"));
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

fn read_png(source: &Path) -> Result<Vec<u8>, String> {
    let file =
        fs::File::open(source).map_err(|error| format!("open input image failed: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("read input image metadata failed: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "input image size is invalid: bytes={} maximum={MAX_IMAGE_BYTES}",
            metadata.len()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read input image failed: {error}"))?;
    let (content_type, _, _) = inspect_model_input_image(&bytes)?;
    if content_type != "image/png" {
        return Err("input image must be PNG".to_string());
    }
    Ok(bytes)
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
        verify_managed_image(path.as_path(), digest)
    }
}

fn managed_image_path(digest: &str) -> Result<PathBuf, String> {
    if !is_sha256(digest) {
        return Err("managed input image digest is invalid".to_string());
    }
    Ok(crate::user_data_layout::runtime_inputs_dir_path().join(format!("{digest}.png")))
}

fn verify_managed_image(path: &Path, expected_digest: &str) -> Result<Vec<u8>, String> {
    let bytes = read_png(path)?;
    if hex_sha256(bytes.as_slice()) != expected_digest {
        return Err(format!(
            "managed input image is corrupt: {}",
            path.display()
        ));
    }
    Ok(bytes)
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
    use base64::Engine as _;

    fn png() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC").expect("PNG fixture")
    }

    #[test]
    fn invalid_png_is_rejected_before_creating_managed_storage() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-image-invalid-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let source = root.join("bad.png");
        std::fs::write(&source, PNG_SIGNATURE).expect("fixture");
        let request = LocalImageInputRequest {
            placeholder: "[image]".into(),
            local_path: source.to_string_lossy().into_owned(),
        };
        let result = import_local_image_at(&request, 1, &root.join("inputs"));
        let created_storage = root.join("inputs").exists();
        std::fs::remove_dir_all(root).expect("cleanup fixture");
        assert!(
            result.is_err(),
            "signature-only content must not be imported"
        );
        assert!(!created_storage);
    }

    #[test]
    fn valid_png_import_reuses_identity_and_preserves_bytes() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-image-import-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let source = root.join("source.png");
        std::fs::write(&source, png()).expect("fixture");
        let request = LocalImageInputRequest {
            placeholder: "[image]".into(),
            local_path: source.to_string_lossy().into_owned(),
        };
        let first = import_local_image_at(&request, 1, &root.join("inputs")).expect("import");
        assert_eq!(
            first,
            import_local_image_at(&request, 1, &root.join("inputs")).expect("repeat")
        );
        assert_eq!(
            std::fs::read(
                root.join("inputs")
                    .join(format!("{}.png", hex_sha256(&png())))
            )
            .expect("saved"),
            png()
        );
        std::fs::remove_dir_all(root).expect("cleanup fixture");
    }

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
        let bytes = png();
        std::fs::write(path.as_path(), bytes.as_slice()).expect("write image fixture");
        let digest = hex_sha256(bytes.as_slice());
        verify_managed_image(path.as_path(), digest.as_str()).expect("verify image");
        assert!(verify_managed_image(path.as_path(), &"b".repeat(64)).is_err());
        let _ = std::fs::remove_file(path);
    }
}
