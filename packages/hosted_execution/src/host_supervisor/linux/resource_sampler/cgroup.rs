//! Descriptor-relative reads never follow a workload-controlled cgroup symlink.
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path},
};
const MAX_READ: u64 = 256 * 1024;
pub(super) struct Directory(File);
impl Directory {
    pub(super) fn open(root: &Path, relative: &Path) -> Result<Self, String> {
        let name =
            CString::new(root.as_os_str().as_bytes()).map_err(|_| "host_resource_path_invalid")?;
        // SAFETY: name is NUL terminated; successful ownership transfers into File.
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err("host_resource_directory_unavailable".into());
        }
        let mut dir = Self(unsafe { File::from_raw_fd(fd) });
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err("host_resource_path_invalid".into());
            };
            let name = CString::new(name.as_bytes()).map_err(|_| "host_resource_path_invalid")?;
            // SAFETY: dir owns a live directory FD; NOFOLLOW rejects symlink traversal.
            let fd = unsafe {
                libc::openat(
                    dir.0.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err("host_resource_directory_unavailable".into());
            }
            dir = Self(unsafe { File::from_raw_fd(fd) });
        }
        Ok(dir)
    }
    pub(super) fn identity(&self) -> Result<(u64, u64), String> {
        use std::os::unix::fs::MetadataExt;
        let m = self
            .0
            .metadata()
            .map_err(|_| "host_resource_directory_unavailable")?;
        Ok((m.dev(), m.ino()))
    }
    pub(super) fn read(&self, name: &str) -> Result<String, String> {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err("host_resource_path_invalid".into());
        }
        let name = CString::new(name).map_err(|_| "host_resource_path_invalid")?;
        // SAFETY: the directory FD and NUL-terminated leaf are valid for openat.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err("host_resource_counter_unavailable".into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file
            .metadata()
            .map_err(|_| "host_resource_counter_unavailable")?
            .is_file()
        {
            return Err("host_resource_counter_invalid".into());
        }
        let mut text = String::new();
        file.take(MAX_READ + 1)
            .read_to_string(&mut text)
            .map_err(|_| "host_resource_counter_unavailable")?;
        if text.len() as u64 > MAX_READ {
            return Err("host_resource_counter_oversized".into());
        }
        Ok(text)
    }
}
