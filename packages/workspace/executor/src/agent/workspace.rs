//! Workload identity and workspace selected by the trusted host launcher.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(super) struct Workspace {
    pub root: PathBuf,
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "Workload identity is enforced by Linux host helpers."
        )
    )]
    pub uid: u32,
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "Workload identity is enforced by Linux host helpers."
        )
    )]
    pub gid: u32,
}

impl Workspace {
    fn new(root: &str, user: &str) -> Result<Self, String> {
        if !root.starts_with('/')
            || root.contains(['\\', '\0'])
            || root.chars().any(char::is_control)
            || root == "/run/centaeris"
            || root.starts_with("/run/centaeris/")
            || (root != "/"
                && root
                    .split('/')
                    .skip(1)
                    .any(|p| p.is_empty() || p == "." || p == ".."))
        {
            return Err("environment workspace is invalid".into());
        }
        let (uid, gid) = user.split_once(':').ok_or("environment user is invalid")?;
        let id = |value: &str| -> Result<u32, String> {
            if value.is_empty()
                || (value != "0" && value.starts_with('0'))
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err("environment user is invalid".into());
            }
            value
                .parse::<u32>()
                .ok()
                .filter(|id| *id != u32::MAX)
                .ok_or_else(|| "environment user is invalid".into())
        };
        Ok(Self {
            root: root.into(),
            uid: id(uid)?,
            gid: id(gid)?,
        })
    }

    pub fn relative_path(&self, path: &str) -> Result<String, String> {
        let root = self
            .root
            .to_str()
            .ok_or("environment workspace is invalid")?;
        if path == root || path == "." {
            return Ok(".".into());
        }
        let prefix = if root == "/" {
            "/".to_owned()
        } else {
            format!("{root}/")
        };
        let relative = if let Some(relative) = path.strip_prefix(&prefix) {
            relative
        } else if Path::new(path).is_absolute() {
            return Err("sandbox path escaped its workspace".into());
        } else {
            path
        };
        if relative.is_empty()
            || relative.contains('\\')
            || relative.chars().any(char::is_control)
            || relative
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err("sandbox path contains an invalid component".into());
        }
        Ok(relative.into())
    }
}

pub(super) fn context() -> Result<&'static Workspace, String> {
    static WORKSPACE: OnceLock<Result<Workspace, String>> = OnceLock::new();
    WORKSPACE
        .get_or_init(|| {
            let root = std::env::var("CENTAERIS_WORKSPACE_ROOT")
                .unwrap_or_else(|_| centaeris_core::execution::WORKSPACE_DATA_ROOT.into());
            let user =
                std::env::var("CENTAERIS_WORKSPACE_USER").unwrap_or_else(|_| "10001:10001".into());
            Workspace::new(&root, &user)
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(unix)]
fn prepare_directory(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
    preserve: bool,
) -> Result<(), String> {
    use std::ffi::CString;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    use std::path::Component;
    if !path.is_absolute() {
        return Err("workspace preparation requires an absolute path".into());
    }
    let parts = path
        .components()
        .filter(|c| *c != Component::RootDir)
        .collect::<Vec<_>>();
    let mut directory = File::open("/").map_err(|_| "workspace root unavailable")?;
    for (index, component) in parts.iter().enumerate() {
        let Component::Normal(part) = component else {
            return Err("workspace directory is invalid".into());
        };
        let name = CString::new(part.as_bytes()).map_err(|_| "workspace directory is invalid")?;
        let final_part = index + 1 == parts.len();
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        // SAFETY: directory is held open and name is a single, NUL-terminated component.
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        let mut created = false;
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            // SAFETY: same anchored parent descriptor and validated name.
            if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
                created = true;
            } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                return Err("workspace directory creation failed".into());
            }
            // SAFETY: the new directory is reopened without following a link.
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err("workspace directory contains an unsupported node".into());
        }
        // SAFETY: openat returned a new owned descriptor.
        let next = unsafe { File::from_raw_fd(fd) };
        if created {
            let (owner, group, permissions) = if final_part {
                (uid, gid, mode)
            } else {
                (
                    unsafe { libc::geteuid() },
                    unsafe { libc::getegid() },
                    0o755,
                )
            };
            // SAFETY: both calls operate on this newly created, opened directory only.
            if unsafe { libc::fchown(next.as_raw_fd(), owner, group) } != 0
                || unsafe { libc::fchmod(next.as_raw_fd(), permissions) } != 0
            {
                return Err("workspace directory ownership failed".into());
            }
        } else if final_part && !preserve {
            let metadata = next
                .metadata()
                .map_err(|_| "workspace state inspection failed")?;
            if metadata.uid() != uid || metadata.gid() != gid || metadata.mode() & 0o7777 != mode {
                return Err("workspace state permissions are invalid".into());
            }
        }
        directory = next;
    }
    Ok(())
}

pub(super) fn prepare() -> Result<(), String> {
    #[cfg(unix)]
    {
        let workspace = context()?;
        prepare_directory(&workspace.root, workspace.uid, workspace.gid, 0o755, true)?;
        prepare_directory(Path::new(super::STATE_ROOT), 0, 0, 0o700, false)
    }
    #[cfg(not(unix))]
    {
        Err("workspace preparation requires Linux".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_and_ownership_follow_the_selected_profile() {
        for (root, user, ids) in [
            ("/mnt/data", "10001:10001", (10001, 10001)),
            ("/project", "0:0", (0, 0)),
        ] {
            let workspace = Workspace::new(root, user).unwrap();
            assert_eq!((workspace.uid, workspace.gid), ids);
            assert_eq!(
                workspace
                    .relative_path(&format!("{root}/reports/a.txt"))
                    .unwrap(),
                "reports/a.txt"
            );
            assert_eq!(
                workspace.relative_path("reports/a.txt").unwrap(),
                "reports/a.txt"
            );
            assert!(workspace.relative_path("/other/a.txt").is_err());
            assert!(workspace.relative_path("../a.txt").is_err());
        }
    }

    #[test]
    fn workspace_configuration_requires_an_absolute_root_and_numeric_identity() {
        for root in [
            "relative",
            "/a/../b",
            "/a//b",
            "/run/centaeris/private",
            "/a\\b",
        ] {
            assert!(Workspace::new(root, "10001:10001").is_err());
        }
        for user in ["root", "01:1", "1:-1", "4294967295:1", "1:2:3"] {
            assert!(Workspace::new("/project", user).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_preparation_creates_missing_directories_and_preserves_image_content() {
        use std::fs;
        use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
        let root = std::env::temp_dir().join(format!(
            "centaeris-prepare-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        let target = root.join("new/workspace");
        prepare_directory(&target, uid, gid, 0o755, true).unwrap();
        let metadata = fs::metadata(&target).unwrap();
        assert_eq!(
            (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
            (uid, gid, 0o755)
        );
        fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(target.join("from-image"), b"preserved").unwrap();
        prepare_directory(&target, uid, gid, 0o755, true).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o750);
        assert_eq!(fs::read(target.join("from-image")).unwrap(), b"preserved");
        let state = root.join("state");
        prepare_directory(&state, uid, gid, 0o700, false).unwrap();
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_directory(&state, uid, gid, 0o700, false).is_err());
        symlink(&target, root.join("link")).unwrap();
        assert!(prepare_directory(&root.join("link/escape"), uid, gid, 0o755, true).is_err());
        assert!(!target.join("escape").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
