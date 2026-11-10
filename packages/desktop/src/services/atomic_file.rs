use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH,
};

/// Syncs complete content and atomically replaces the target. Failures clean only
/// owned temporary files; parent-directory durability is not guaranteed.
pub(crate) fn write_file_atomically(
    path: &Path,
    content: &[u8],
    label: &str,
) -> Result<(), String> {
    write_file_with_temporary(path, content, label, |parent| {
        let builder = tempfile::Builder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::PermissionsExt;
            let mut builder = builder;
            // Match the previous OpenOptions creation mode, still subject to umask.
            builder.permissions(fs::Permissions::from_mode(0o666));
            builder
        };
        builder.tempfile_in(parent)
    })
}

fn write_file_with_temporary(
    path: &Path,
    content: &[u8],
    label: &str,
    create_temporary: impl FnOnce(&Path) -> std::io::Result<NamedTempFile>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{label} path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create {label} parent dir failed: {error}"))?;
    path.file_name()
        .ok_or_else(|| format!("{label} path has no file name: {}", path.display()))?;
    let mut temporary = create_temporary(parent)
        .map_err(|error| format!("create temporary {label} failed: {error}"))?;
    temporary
        .write_all(content)
        .map_err(|error| format!("write temporary {label} failed: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("sync temporary {label} failed: {error}"))?;
    // Close the file handle before replacement, retaining owned-path cleanup on failure.
    // File content is synced; this does not add parent-directory durability guarantees.
    let mut temporary_path = temporary.into_temp_path();
    replace_file(&temporary_path, path)
        .map_err(|error| format!("replace {label} failed: {error}"))?;
    temporary_path.disable_cleanup(true);
    Ok(())
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
    // NamedTempFile marks Windows files temporary; publication must clear that hint.
    if unsafe { SetFileAttributesW(source.as_ptr(), FILE_ATTRIBUTE_NORMAL) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_only_target(root: &Path, target: &Path) {
        let entries: Vec<_> = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(entries, vec![target.to_path_buf()]);
    }

    #[test]
    fn failed_exclusive_creation_preserves_other_writers_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let other_temporary = root.path().join("other-writer.tmp");
        fs::write(&target, b"old").unwrap();
        fs::write(&other_temporary, b"other writer").unwrap();
        let result = write_file_with_temporary(&target, b"new", "test", |parent| {
            tempfile::Builder::new()
                .prefix("other-writer.tmp")
                .rand_bytes(0)
                .tempfile_in(parent)
        });
        assert!(result
            .unwrap_err()
            .starts_with("create temporary test failed:"));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(fs::read(&other_temporary).unwrap(), b"other writer");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn failed_write_preserves_target_and_cleans_owned_temporary() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::write(&target, b"old").unwrap();
        let result = write_file_with_temporary(&target, b"new", "test", |parent| {
            let temporary = NamedTempFile::new_in(parent)?.into_temp_path();
            let read_only = fs::File::open(&temporary)?;
            Ok(NamedTempFile::from_parts(read_only, temporary))
        });
        assert!(result
            .unwrap_err()
            .starts_with("write temporary test failed:"));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_only_target(root.path(), &target);
    }

    #[test]
    fn failed_replacement_preserves_target_directory_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("old"), b"old").unwrap();
        let error = write_file_atomically(&target, b"new", "test").unwrap_err();
        assert!(error.starts_with("replace test failed:"));
        assert_eq!(fs::read(target.join("old")).unwrap(), b"old");
        assert_only_target(root.path(), &target);
    }

    #[test]
    fn concurrent_writes_publish_a_complete_payload_without_temporary_leftovers() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::write(&target, b"old").unwrap();
        let payloads: Vec<_> = (0..16).map(|writer| vec![writer; 32 * 1024]).collect();
        let barrier = std::sync::Barrier::new(payloads.len());
        let results = std::thread::scope(|scope| {
            let writers: Vec<_> = payloads
                .iter()
                .map(|payload| {
                    let target = &target;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        write_file_atomically(target, payload, "test")
                    })
                })
                .collect();
            writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(results.iter().any(Result::is_ok));
        for result in results {
            // Windows may reject simultaneous target replacements. Ownership cleanup
            // must remain correct whether publication succeeds or fails.
            if cfg!(windows) {
                if let Err(error) = result {
                    assert!(error.starts_with("replace test failed:"));
                }
            } else {
                result.unwrap();
            }
        }
        let actual = fs::read(&target).unwrap();
        assert!(payloads.contains(&actual));
        assert_only_target(root.path(), &target);
    }

    #[test]
    fn successful_write_creates_parents_and_replaces_existing_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("nested/target");
        write_file_atomically(&target, b"old", "test").unwrap();
        write_file_atomically(&target, b"new", "test").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        #[cfg(windows)]
        {
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileAttributesW, FILE_ATTRIBUTE_TEMPORARY, INVALID_FILE_ATTRIBUTES,
            };
            let path: Vec<_> = target
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
            assert_ne!(attributes, INVALID_FILE_ATTRIBUTES);
            assert_eq!(attributes & FILE_ATTRIBUTE_TEMPORARY, 0);
        }
        assert_only_target(target.parent().unwrap(), &target);
    }

    #[cfg(unix)]
    #[test]
    fn published_mode_matches_regular_file_creation_under_current_umask() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let control = root.path().join("control");
        let target = root.path().join("target");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&control)
            .unwrap();
        write_file_atomically(&target, b"new", "test").unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&target), mode(&control));
    }

    #[cfg(windows)]
    #[test]
    fn target_without_delete_sharing_preserves_old_content_on_replacement_failure() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::write(&target, b"old").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&target)
            .unwrap();
        let error = write_file_atomically(&target, b"new", "test").unwrap_err();
        assert!(error.starts_with("replace test failed:"));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_only_target(root.path(), &target);
        drop(held);
    }
}
