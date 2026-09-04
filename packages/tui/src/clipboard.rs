use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static CLIPBOARD_IMAGE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) enum ClipboardContent {
    Image(PathBuf),
    Text(String),
}

pub(crate) fn copy_text(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    clipboard
        .set_text(text)
        .map_err(|error| format!("copy text failed: {error}"))
}

pub(crate) fn paste() -> Result<ClipboardContent, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(_) => {
            return clipboard
                .get_text()
                .map(ClipboardContent::Text)
                .map_err(|error| format!("clipboard has no text or image: {error}"));
        }
    };
    let width =
        u32::try_from(image.width).map_err(|_| "clipboard image width is too large".to_string())?;
    let height = u32::try_from(image.height)
        .map_err(|_| "clipboard image height is too large".to_string())?;
    let rgba = image::RgbaImage::from_raw(width, height, image.bytes.into_owned())
        .ok_or_else(|| "clipboard image has an invalid RGBA buffer".to_string())?;
    write_temp_png(image::DynamicImage::ImageRgba8(rgba)).map(ClipboardContent::Image)
}

pub(crate) fn image_from_pasted_text(text: &str) -> Result<Option<PathBuf>, String> {
    let candidate = text.trim();
    let candidate = candidate
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(candidate);
    let candidate = candidate.replace("` ", " ");
    let path = PathBuf::from(candidate);
    let supported_extension = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp"
            )
        });
    if !path.is_absolute() || !supported_extension || !path.is_file() {
        return Ok(None);
    }
    let image = image::ImageReader::open(path.as_path())
        .map_err(|error| format!("open pasted image failed: {error}"))?
        .decode()
        .map_err(|error| format!("decode pasted image failed: {error}"))?;
    write_temp_png(image).map(Some)
}

fn write_temp_png(image: image::DynamicImage) -> Result<PathBuf, String> {
    let mut encoded = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
        .map_err(|error| format!("encode clipboard image failed: {error}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = std::env::temp_dir().join(format!(
        "centaeris-clipboard-{}-{timestamp}-{}.png",
        std::process::id(),
        CLIPBOARD_IMAGE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(path.as_path(), encoded)
        .map_err(|error| format!("write clipboard image failed: {error}"))?;
    Ok(path)
}
