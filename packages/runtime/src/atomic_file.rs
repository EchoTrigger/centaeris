use centaeris_core::runtime::contracts::current_timestamp_ms;
use std::fs;
use std::io::Write;
use std::path::Path;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

pub(crate) fn write_file_atomically(
    path: &Path,
    content: &[u8],
    label: &str,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{label} path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create {label} parent dir failed: {error}"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{label} path has no file name: {}", path.display()))?;
    let temporary_path = parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        current_timestamp_ms()
    ));
    let write_result = (|| {
        let mut temporary = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temporary_path.as_path())
            .map_err(|error| format!("create temporary {label} failed: {error}"))?;
        temporary
            .write_all(content)
            .map_err(|error| format!("write temporary {label} failed: {error}"))?;
        temporary
            .sync_all()
            .map_err(|error| format!("sync temporary {label} failed: {error}"))?;
        replace_file(temporary_path.as_path(), path)
            .map_err(|error| format!("replace {label} failed: {error}"))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(temporary_path.as_path());
    }
    write_result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
