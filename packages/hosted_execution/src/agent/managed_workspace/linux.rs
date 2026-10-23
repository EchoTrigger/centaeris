//! Linux mount operations stay in the guest namespace shared with future tools.
use super::*;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PRIVATE_ROOT: &str = "/var/lib/centaeris-host";
const BACKING_NAME: &str = "managed-workspace";
const MARKER_NAME: &str = "managed-workspace.json";
const LOCK_NAME: &str = "managed-workspace.lock";
const WORKSPACE_ROOT: &str = "/mnt/data";
const AGENT_UID: u32 = 10_001;
const AGENT_GID: u32 = 10_001;

pub(super) fn prepare() -> Result<(), String> {
    let scope = Scope {
        environment_id: std::env::var("CENTAERIS_MANAGED_ENVIRONMENT_ID")
            .map_err(|_| "managed_workspace_scope_unavailable")?,
        sandbox_id: std::env::var("CENTAERIS_MANAGED_SANDBOX_ID")
            .map_err(|_| "managed_workspace_scope_unavailable")?,
    };
    scope.validate()?;
    let context = super::super::workspace::context()?;
    if context.root != Path::new(WORKSPACE_ROOT)
        || context.uid != AGENT_UID
        || context.gid != AGENT_GID
    {
        return Err("managed_workspace_context_invalid".into());
    }
    let mut disk = DiskWorkspace::open(Path::new(PRIVATE_ROOT), &context.root)?;
    let _lock = disk.lock()?;
    if disk.marker()?.is_none()
        && (!super::super::load_inventory()?.inputs.is_empty()
            || !super::super::agent_process_ids()?.is_empty())
    {
        return Err("managed_workspace_initial_state_not_empty".into());
    }
    initialize(&scope, &mut disk)
}

struct DiskWorkspace {
    private: File,
    target: PathBuf,
}

impl DiskWorkspace {
    fn open(private: &Path, target: &Path) -> Result<Self, String> {
        let private = open_directory(private, true)?;
        let mut information: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: private is an open directory; information is a valid writable statfs.
        if unsafe { libc::fstatfs(private.as_raw_fd(), &mut information) } != 0 {
            return Err("managed_workspace_backing_filesystem_unavailable".into());
        }
        // A workspace must not silently become a guest-memory allocation.
        if [0x0102_1994_u32, 0x8584_58f6_u32].contains(&(information.f_type as u32)) {
            return Err("managed_workspace_backing_requires_disk".into());
        }
        let _target = open_directory(target, false)?;
        Ok(Self {
            private,
            target: target.to_path_buf(),
        })
    }

    fn lock(&self) -> Result<File, String> {
        let file = open_at(
            &self.private,
            LOCK_NAME,
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )?
        .ok_or("managed_workspace_lock_unavailable")?;
        validate_private_file(&file)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: the descriptor remains open for the lifetime of the returned File.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(file);
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) || Instant::now() >= deadline {
                return Err("managed_workspace_lock_unavailable".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn backing_file(&self) -> Result<Option<File>, String> {
        let file = open_at(
            &self.private,
            BACKING_NAME,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            0,
        )?;
        if let Some(file) = &file {
            let metadata = file
                .metadata()
                .map_err(|_| "managed_workspace_backing_unavailable")?;
            if !metadata.is_dir()
                || metadata.uid() != AGENT_UID
                || metadata.gid() != AGENT_GID
                || metadata.mode() & 0o7777 != 0o755
            {
                return Err("managed_workspace_backing_unsafe".into());
            }
        }
        Ok(file)
    }
}

impl WorkspaceBackend for DiskWorkspace {
    fn marker(&mut self) -> Result<Option<Marker>, String> {
        let Some(mut file) = open_at(
            &self.private,
            MARKER_NAME,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?
        else {
            return Ok(None);
        };
        validate_private_file(&file)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((MARKER_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "managed_workspace_marker_unavailable")?;
        parse_marker(&bytes).map(Some)
    }

    fn backing(&mut self) -> Result<Option<DirectoryIdentity>, String> {
        self.backing_file()?
            .map(|file| directory_identity(&file))
            .transpose()
    }

    fn mount_state(&mut self) -> Result<MountState, String> {
        let mut information = Vec::new();
        // Observe the calling thread: /proc/self may describe the group leader
        // after this thread enters a different mount namespace.
        File::open("/proc/thread-self/mountinfo")
            .and_then(|file| file.take(2 * 1024 * 1024 + 1).read_to_end(&mut information))
            .map_err(|_| "managed_workspace_mount_observation_unavailable")?;
        if information.len() > 2 * 1024 * 1024 {
            return Err("managed_workspace_mount_observation_unavailable".into());
        }
        let expected = self
            .target
            .to_str()
            .ok_or("managed_workspace_context_invalid")?
            .replace('\\', "\\134")
            .replace(' ', "\\040")
            .replace('\t', "\\011")
            .replace('\n', "\\012");
        let mounted = std::str::from_utf8(&information)
            .map_err(|_| "managed_workspace_mount_observation_unavailable")?
            .lines()
            .any(|line| line.split_whitespace().nth(4) == Some(expected.as_str()));
        if !mounted {
            return Ok(MountState::Unbound);
        }
        let target = open_directory(&self.target, false)?;
        let actual = directory_identity(&target)?;
        if self.backing()? == Some(actual) {
            Ok(MountState::Backing)
        } else {
            Ok(MountState::Foreign)
        }
    }

    fn write_marker(&mut self, marker: &Marker) -> Result<(), String> {
        let bytes = serde_json::to_vec(marker).map_err(|_| "managed_workspace_marker_invalid")?;
        if bytes.len() > MARKER_MAX_BYTES {
            return Err("managed_workspace_marker_invalid".into());
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "managed_workspace_marker_unavailable")?
            .as_nanos();
        let temporary = format!(".managed-workspace-{}-{nonce}.tmp", std::process::id());
        let mut file = open_at(
            &self.private,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        )?
        .ok_or("managed_workspace_marker_unavailable")?;
        let result: Result<(), String> = (|| {
            validate_private_file(&file)?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "managed_workspace_marker_unavailable")?;
            let source =
                CString::new(temporary.as_str()).map_err(|_| "managed_workspace_marker_invalid")?;
            let target =
                CString::new(MARKER_NAME).map_err(|_| "managed_workspace_marker_invalid")?;
            // SAFETY: both names are single components in the same held root-owned directory.
            if unsafe {
                libc::renameat(
                    self.private.as_raw_fd(),
                    source.as_ptr(),
                    self.private.as_raw_fd(),
                    target.as_ptr(),
                )
            } != 0
            {
                return Err("managed_workspace_marker_unavailable".into());
            }
            self.private
                .sync_all()
                .map_err(|_| "managed_workspace_marker_unavailable".into())
        })();
        if result.is_err() {
            // Only this invocation's newly-created temporary file is eligible for removal.
            let name = CString::new(temporary).map_err(|_| "managed_workspace_marker_invalid")?;
            unsafe {
                libc::unlinkat(self.private.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        result
    }

    fn create_backing(&mut self) -> Result<DirectoryIdentity, String> {
        let name = CString::new(BACKING_NAME).map_err(|_| "managed_workspace_backing_unsafe")?;
        // SAFETY: the root-owned private parent is held open and name is one fixed component.
        if unsafe { libc::mkdirat(self.private.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err("managed_workspace_backing_creation_unknown".into());
        }
        let file = open_at(
            &self.private,
            BACKING_NAME,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            0,
        )?
        .ok_or("managed_workspace_backing_unavailable")?;
        // SAFETY: this is the newly created and opened directory inside the private parent.
        if unsafe { libc::fchown(file.as_raw_fd(), AGENT_UID, AGENT_GID) } != 0
            || unsafe { libc::fchmod(file.as_raw_fd(), 0o755) } != 0
        {
            return Err("managed_workspace_backing_creation_unknown".into());
        }
        file.sync_all()
            .and_then(|_| self.private.sync_all())
            .map_err(|_| "managed_workspace_backing_creation_unknown")?;
        directory_identity(&file)
    }

    fn backing_is_empty(&mut self) -> Result<bool, String> {
        let file = self
            .backing_file()?
            .ok_or("managed_workspace_backing_missing")?;
        fs::read_dir(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| "managed_workspace_backing_unavailable")?
            .next()
            .transpose()
            .map(|entry| entry.is_none())
            .map_err(|_| "managed_workspace_backing_unavailable".into())
    }

    fn bind_backing(&mut self) -> Result<(), String> {
        let source = self
            .backing_file()?
            .ok_or("managed_workspace_backing_missing")?;
        let target = open_directory(&self.target, false)?;
        let source_path = CString::new(format!("/proc/self/fd/{}", source.as_raw_fd()))
            .map_err(|_| "managed_workspace_backing_unsafe")?;
        let target_path = CString::new(format!("/proc/self/fd/{}", target.as_raw_fd()))
            .map_err(|_| "managed_workspace_context_invalid")?;
        // SAFETY: both fixed directories are anchored by held descriptors; no shell or user path is used.
        if unsafe {
            libc::mount(
                source_path.as_ptr(),
                target_path.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } != 0
        {
            return Err("managed_workspace_mount_unknown".into());
        }
        Ok(())
    }
}

fn open_at(
    parent: &File,
    name: &str,
    flags: i32,
    mode: libc::mode_t,
) -> Result<Option<File>, String> {
    if name.is_empty() || name.contains('/') {
        return Err("managed_workspace_private_path_invalid".into());
    }
    let name = CString::new(name).map_err(|_| "managed_workspace_private_path_invalid")?;
    // SAFETY: parent is held open, name is one validated component, mode is supplied for O_CREAT.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
        )
    };
    if descriptor < 0 {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err("managed_workspace_private_node_unavailable".into());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(Some(unsafe { File::from_raw_fd(descriptor) }))
}

fn open_directory(path: &Path, create_private: bool) -> Result<File, String> {
    if !path.is_absolute() {
        return Err("managed_workspace_private_path_invalid".into());
    }
    let mut parent = File::open("/").map_err(|_| "managed_workspace_private_path_invalid")?;
    let components = path
        .components()
        .filter(|part| *part != Component::RootDir)
        .collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err("managed_workspace_private_path_invalid".into());
        };
        let name = name
            .to_str()
            .ok_or("managed_workspace_private_path_invalid")?;
        let final_part = index + 1 == components.len();
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let next = match open_at(&parent, name, flags, 0)? {
            Some(file) => file,
            None if create_private && final_part => {
                let component =
                    CString::new(name).map_err(|_| "managed_workspace_private_path_invalid")?;
                // SAFETY: the parent is held open and this fixed private directory is one component.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    return Err("managed_workspace_private_path_invalid".into());
                }
                parent
                    .sync_all()
                    .map_err(|_| "managed_workspace_private_path_invalid")?;
                open_at(&parent, name, flags, 0)?.ok_or("managed_workspace_private_path_invalid")?
            }
            None => return Err("managed_workspace_private_path_invalid".into()),
        };
        let metadata = next
            .metadata()
            .map_err(|_| "managed_workspace_private_path_invalid")?;
        if !final_part || create_private {
            // Root-owned sticky /tmp is accepted for the isolated namespace test fixture.
            let safely_sticky = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
            if metadata.uid() != 0
                || metadata.gid() != 0
                || (metadata.mode() & 0o022 != 0 && !safely_sticky)
                || (create_private && final_part && metadata.mode() & 0o7777 != 0o700)
            {
                return Err("managed_workspace_private_path_unsafe".into());
            }
        }
        parent = next;
    }
    Ok(parent)
}

fn validate_private_file(file: &File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "managed_workspace_private_file_unavailable")?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err("managed_workspace_private_file_unsafe".into());
    }
    Ok(())
}

fn directory_identity(file: &File) -> Result<DirectoryIdentity, String> {
    let metadata = file
        .metadata()
        .map_err(|_| "managed_workspace_directory_unavailable")?;
    if !metadata.is_dir() {
        return Err("managed_workspace_directory_unsafe".into());
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[test]
    #[ignore = "requires explicit root and CAP_SYS_ADMIN; uses a fresh subprocess mount namespace"]
    fn managed_workspace_mount_namespace_preserves_template_and_files() {
        const CHILD: &str = "CENTAERIS_MANAGED_MOUNT_TEST_CHILD";
        if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "agent::managed_workspace::linux::tests::managed_workspace_mount_namespace_preserves_template_and_files", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "this isolated test requires root"
        );
        assert_eq!(
            unsafe { libc::unshare(libc::CLONE_NEWNS) },
            0,
            "must obtain a private mount namespace before creating any mount"
        );
        let slash = CString::new("/").unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    std::ptr::null(),
                    slash.as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                )
            },
            0
        );
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Path::new("/var/tmp").join(format!(
            "centaeris-managed-workspace-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.join("workspace");
        let private = root.join("private");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("AGENTS.md"), b"template instructions").unwrap();
        let scope = Scope {
            environment_id: "test-environment".into(),
            sandbox_id: "test-sandbox".into(),
        };
        let mut disk = DiskWorkspace::open(&private, &target).unwrap();
        {
            let _lock = disk.lock().unwrap();
            initialize(&scope, &mut disk).unwrap();
        }
        assert!(!target.join("AGENTS.md").exists());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        fs::write(target.join("restored.txt"), b"keep restored bytes").unwrap();
        let identity = disk.backing().unwrap();
        let handles = (0..8)
            .map(|_| {
                let private = private.clone();
                let target = target.clone();
                let scope = scope.clone();
                std::thread::spawn(move || {
                    let mut disk = DiskWorkspace::open(&private, &target).unwrap();
                    let _lock = disk.lock().unwrap();
                    initialize(&scope, &mut disk).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            fs::read(target.join("restored.txt")).unwrap(),
            b"keep restored bytes"
        );
        assert_eq!(disk.backing().unwrap(), identity);
        let target_c = CString::new(target.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::umount2(target_c.as_ptr(), 0) }, 0);
        assert_eq!(
            fs::read(target.join("AGENTS.md")).unwrap(),
            b"template instructions"
        );
        {
            let _lock = disk.lock().unwrap();
            initialize(&scope, &mut disk).unwrap();
        }
        assert_eq!(
            fs::read(target.join("restored.txt")).unwrap(),
            b"keep restored bytes"
        );
        assert_eq!(unsafe { libc::umount2(target_c.as_ptr(), 0) }, 0);
        let partial_target = root.join("partial-target");
        fs::create_dir(&partial_target).unwrap();
        let partial_private = root.join("partial-private");
        let mut partial = DiskWorkspace::open(&partial_private, &partial_target).unwrap();
        let _lock = partial.lock().unwrap();
        partial
            .write_marker(&Marker {
                schema: MARKER_SCHEMA.into(),
                scope: scope.clone(),
                state: InitializationState::Preparing,
                backing: None,
            })
            .unwrap();
        fs::create_dir(partial_private.join(BACKING_NAME)).unwrap();
        fs::set_permissions(
            partial_private.join(BACKING_NAME),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(initialize(&scope, &mut partial).is_err());
        assert_eq!(
            fs::metadata(partial_private.join(BACKING_NAME))
                .unwrap()
                .uid(),
            0
        );
        assert_eq!(
            partial.marker().unwrap().unwrap().state,
            InitializationState::Preparing
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
