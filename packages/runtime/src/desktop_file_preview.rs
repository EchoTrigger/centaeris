use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

const DESKTOP_FILE_PREVIEW_MAX_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopFilePreviewReadRequest {
    pub(crate) path: String,
    pub(crate) workspace_root: Option<String>,
    pub(crate) base_path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopFilePreviewReadResponse {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) content: String,
    pub(crate) byte_len: u64,
    pub(crate) encoding: String,
    pub(crate) content_kind: String,
    pub(crate) mime_type: Option<String>,
    pub(crate) data_url: Option<String>,
}

struct ResolvedDesktopPreviewFile {
    root: PathBuf,
    path: PathBuf,
}

pub(crate) fn read(
    request: DesktopFilePreviewReadRequest,
) -> Result<DesktopFilePreviewReadResponse, String> {
    let resolved = resolve_desktop_preview_file(request)?;
    read_desktop_preview_file(resolved)
}

fn resolve_desktop_preview_file(
    request: DesktopFilePreviewReadRequest,
) -> Result<ResolvedDesktopPreviewFile, String> {
    let raw_path = request.path.trim();
    if raw_path.is_empty() {
        return Err(String::from("desktop file preview path is empty"));
    }

    let requested_path = user_supplied_path(raw_path);
    if requested_path.is_absolute() {
        let path = canonical_preview_file_path(requested_path)?;
        return Ok(ResolvedDesktopPreviewFile {
            root: preview_root_for_file(path.as_path())?,
            path,
        });
    }

    let relative_path = relative_preview_path(raw_path)?;
    let base_path = request
        .base_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            request
                .workspace_root
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| {
            "desktop file preview requires an absolute path or basePath/workspaceRoot".to_string()
        })?;
    let base_root = canonical_preview_base_path(user_supplied_path(base_path))?;
    let path = canonical_preview_file_path(base_root.join(relative_path))?;
    Ok(ResolvedDesktopPreviewFile {
        root: base_root,
        path,
    })
}

fn read_desktop_preview_file(
    resolved: ResolvedDesktopPreviewFile,
) -> Result<DesktopFilePreviewReadResponse, String> {
    let metadata = fs::metadata(resolved.path.as_path())
        .map_err(|error| format!("read desktop preview metadata failed: {error}"))?;
    if metadata.len() > DESKTOP_FILE_PREVIEW_MAX_BYTES {
        return Err(format!(
            "desktop file preview is too large ({} bytes, max {})",
            metadata.len(),
            DESKTOP_FILE_PREVIEW_MAX_BYTES
        ));
    }

    let bytes = fs::read(resolved.path.as_path())
        .map_err(|error| format!("read desktop preview file failed: {error}"))?;
    let byte_len = bytes.len() as u64;
    let name = resolved
        .path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| display_path(resolved.path.as_path()));

    if let Some(mime_type) = preview_image_mime_type(resolved.path.as_path()) {
        return Ok(DesktopFilePreviewReadResponse {
            root: display_path(resolved.root.as_path()),
            path: display_path(resolved.path.as_path()),
            name,
            content: String::new(),
            byte_len,
            encoding: String::from("base64"),
            content_kind: String::from("image"),
            mime_type: Some(mime_type.to_string()),
            data_url: Some(data_url(mime_type, bytes.as_slice())),
        });
    }

    if is_pdf_path(resolved.path.as_path()) {
        return Ok(DesktopFilePreviewReadResponse {
            root: display_path(resolved.root.as_path()),
            path: display_path(resolved.path.as_path()),
            name,
            content: String::new(),
            byte_len,
            encoding: String::from("base64"),
            content_kind: String::from("pdf"),
            mime_type: Some(String::from("application/pdf")),
            data_url: Some(data_url("application/pdf", bytes.as_slice())),
        });
    }

    let content = String::from_utf8(bytes)
        .map_err(|_| "desktop file preview supports UTF-8 text, images, and PDF only".to_string())?
        .trim_start_matches('\u{feff}')
        .to_string();
    Ok(DesktopFilePreviewReadResponse {
        root: display_path(resolved.root.as_path()),
        path: display_path(resolved.path.as_path()),
        name,
        content,
        byte_len,
        encoding: String::from("utf-8"),
        content_kind: String::from("text"),
        mime_type: Some(String::from("text/plain; charset=utf-8")),
        data_url: None,
    })
}

fn user_supplied_path(raw_path: &str) -> PathBuf {
    let normalized = raw_path.trim().trim_start_matches(r"\\?\");
    PathBuf::from(normalized)
}

fn relative_preview_path(raw_path: &str) -> Result<PathBuf, String> {
    let path = user_supplied_path(raw_path);
    if path
        .components()
        .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(String::from(
            "desktop file preview relative path cannot include a drive or root",
        ));
    }
    Ok(path)
}

fn canonical_preview_base_path(path: PathBuf) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path.as_path())
        .map_err(|error| format!("resolve desktop preview base path failed: {error}"))?;
    if canonical.is_file() {
        return canonical
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "desktop preview base file has no parent".to_string());
    }
    if !canonical.is_dir() {
        return Err(String::from("desktop preview base path is not a directory"));
    }
    Ok(canonical)
}

fn canonical_preview_file_path(path: PathBuf) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path.as_path())
        .map_err(|error| format!("resolve desktop preview file failed: {error}"))?;
    if !canonical.is_file() {
        return Err(String::from("desktop preview path is not a file"));
    }
    Ok(canonical)
}

fn preview_root_for_file(path: &Path) -> Result<PathBuf, String> {
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "desktop preview file has no parent".to_string())
}

fn preview_image_mime_type(file_path: &Path) -> Option<&'static str> {
    let extension = file_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().to_ascii_lowercase())?;
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

fn is_pdf_path(file_path: &Path) -> bool {
    file_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

fn data_url(mime_type: &str, bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        mime_type,
        general_purpose::STANDARD.encode(bytes)
    )
}

fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "centaeris-electron-desktop-preview-{name}-{millis}"
        ));
        fs::create_dir_all(path.as_path()).expect("create temp dir");
        path
    }

    #[test]
    fn reads_relative_text_with_cwd() {
        let root = unique_temp_dir("relative-text");
        fs::write(root.join("sample.txt"), "desktop preview smoke\n").expect("write sample");

        let response = read(DesktopFilePreviewReadRequest {
            path: String::from("sample.txt"),
            workspace_root: Some(root.to_string_lossy().to_string()),
            base_path: None,
        })
        .expect("read preview");

        assert_eq!(response.content_kind, "text");
        assert!(response.content.contains("desktop preview smoke"));
        assert!(response.path.ends_with("sample.txt"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_pdf_as_data_url() {
        let root = unique_temp_dir("pdf");
        fs::write(
            root.join("sample.pdf"),
            b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF\n",
        )
        .expect("write pdf");

        let response = read(DesktopFilePreviewReadRequest {
            path: root.join("sample.pdf").to_string_lossy().to_string(),
            workspace_root: None,
            base_path: None,
        })
        .expect("read pdf");

        assert_eq!(response.content_kind, "pdf");
        assert_eq!(response.mime_type.as_deref(), Some("application/pdf"));
        assert!(response
            .data_url
            .as_deref()
            .unwrap_or("")
            .starts_with("data:application/pdf;base64,"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relative_path_without_base_fails_loudly() {
        let error = read(DesktopFilePreviewReadRequest {
            path: String::from("sample.txt"),
            workspace_root: None,
            base_path: None,
        })
        .expect_err("relative path should fail");

        assert!(error.contains("absolute path or basePath/workspaceRoot"));
    }
}
