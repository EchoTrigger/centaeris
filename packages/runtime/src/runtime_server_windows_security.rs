//! Owner-scoped security attributes for the Windows Local Runtime named pipe.

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_ALL, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    AddAccessAllowedAce, CopySid, CreateWellKnownSid, GetLengthSid, GetTokenInformation,
    InitializeAcl, InitializeSecurityDescriptor, SetSecurityDescriptorDacl, TokenUser,
    WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, PSID, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

pub(crate) fn create_server(
    endpoint: &str,
    first_pipe_instance: bool,
) -> io::Result<NamedPipeServer> {
    let mut security = WindowsPipeSecurity::for_current_user()?;
    let mut attributes = security.attributes();
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first_pipe_instance);
    // SAFETY: `attributes` and everything referenced by its security descriptor
    // remain alive until CreateNamedPipeW returns from this synchronous call.
    unsafe {
        options.create_with_security_attributes_raw(
            endpoint,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
        )
    }
}

struct WindowsPipeSecurity {
    acl: AlignedBuffer,
    descriptor: Box<SECURITY_DESCRIPTOR>,
}

impl WindowsPipeSecurity {
    fn for_current_user() -> io::Result<Self> {
        let user_sid = current_process_user_sid()?;
        let system_sid = local_system_sid()?;
        let acl_size = size_of::<ACL>()
            .checked_add(ace_storage_size(user_sid.byte_len)?)
            .and_then(|size| size.checked_add(ace_storage_size(system_sid.byte_len).ok()?))
            .ok_or_else(|| io::Error::other("Windows pipe ACL size overflow"))?;
        let acl_size = u32::try_from(acl_size)
            .map_err(|_| io::Error::other("Windows pipe ACL is too large"))?;
        let mut acl = AlignedBuffer::new(acl_size as usize)?;
        // SAFETY: `acl` is writable and large enough for both access-allowed
        // ACEs, and both SID buffers contain copies validated by Windows.
        unsafe {
            win32_bool(
                InitializeAcl(acl.as_mut_ptr(), acl_size, ACL_REVISION),
                "InitializeAcl",
            )?;
            win32_bool(
                AddAccessAllowedAce(
                    acl.as_mut_ptr(),
                    ACL_REVISION,
                    GENERIC_ALL,
                    system_sid.as_psid(),
                ),
                "AddAccessAllowedAce(SYSTEM)",
            )?;
            win32_bool(
                AddAccessAllowedAce(
                    acl.as_mut_ptr(),
                    ACL_REVISION,
                    GENERIC_ALL,
                    user_sid.as_psid(),
                ),
                "AddAccessAllowedAce(current user)",
            )?;
        }

        // SAFETY: Windows initializes every field before the descriptor is used.
        let mut descriptor = Box::new(unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() });
        // SAFETY: `descriptor` and `acl` remain alive together in the returned
        // value; SetSecurityDescriptorDacl stores the ACL pointer in descriptor.
        unsafe {
            win32_bool(
                InitializeSecurityDescriptor(
                    (&mut *descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                    SECURITY_DESCRIPTOR_REVISION,
                ),
                "InitializeSecurityDescriptor",
            )?;
            win32_bool(
                SetSecurityDescriptorDacl(
                    (&mut *descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                    1,
                    acl.as_ptr(),
                    0,
                ),
                "SetSecurityDescriptorDacl",
            )?;
        }
        Ok(Self { acl, descriptor })
    }

    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        let _keep_acl_alive = &self.acl;
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&mut *self.descriptor as *mut SECURITY_DESCRIPTOR).cast(),
            bInheritHandle: 0,
        }
    }
}

fn ace_storage_size(sid_len: usize) -> io::Result<usize> {
    size_of::<ACCESS_ALLOWED_ACE>()
        .checked_sub(size_of::<u32>())
        .and_then(|size| size.checked_add(sid_len))
        .ok_or_else(|| io::Error::other("Windows pipe ACE size overflow"))
}

fn current_process_user_sid() -> io::Result<AlignedBuffer> {
    let mut token = ptr::null_mut();
    // SAFETY: token points to writable storage and GetCurrentProcess returns a
    // valid pseudo-handle for the current process.
    unsafe {
        win32_bool(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
            "OpenProcessToken",
        )?;
    }
    let token = OwnedHandle(token);
    let mut required = 0;
    // SAFETY: the first call intentionally supplies no buffer to obtain its size.
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(last_win32_error("GetTokenInformation(size)"));
    }
    let mut token_user = AlignedBuffer::new(required as usize)?;
    // SAFETY: the aligned buffer is writable for exactly `required` bytes.
    unsafe {
        win32_bool(
            GetTokenInformation(
                token.0,
                TokenUser,
                token_user.as_mut_void(),
                required,
                &mut required,
            ),
            "GetTokenInformation(TokenUser)",
        )?;
        let user = &*token_user.as_ptr::<TOKEN_USER>();
        copy_sid(user.User.Sid)
    }
}

fn local_system_sid() -> io::Result<AlignedBuffer> {
    let mut size = SECURITY_MAX_SID_SIZE;
    let mut sid = AlignedBuffer::new(size as usize)?;
    // SAFETY: the buffer is writable for SECURITY_MAX_SID_SIZE bytes and the
    // size pointer is valid for the duration of the call.
    unsafe {
        win32_bool(
            CreateWellKnownSid(WinLocalSystemSid, ptr::null_mut(), sid.as_psid(), &mut size),
            "CreateWellKnownSid(LocalSystem)",
        )?;
    }
    sid.byte_len = size as usize;
    Ok(sid)
}

unsafe fn copy_sid(source: PSID) -> io::Result<AlignedBuffer> {
    // SAFETY: caller provides a SID returned by GetTokenInformation.
    let size = unsafe { GetLengthSid(source) };
    if size == 0 {
        return Err(last_win32_error("GetLengthSid"));
    }
    let destination = AlignedBuffer::new(size as usize)?;
    // SAFETY: destination is writable for `size` bytes and source is valid.
    unsafe {
        win32_bool(CopySid(size, destination.as_psid(), source), "CopySid")?;
    }
    Ok(destination)
}

struct AlignedBuffer {
    words: Vec<usize>,
    byte_len: usize,
}

impl AlignedBuffer {
    fn new(byte_len: usize) -> io::Result<Self> {
        if byte_len == 0 {
            return Err(io::Error::other("Windows security buffer is empty"));
        }
        let word_size = size_of::<usize>();
        let word_len = byte_len
            .checked_add(word_size - 1)
            .map(|size| size / word_size)
            .ok_or_else(|| io::Error::other("Windows security buffer size overflow"))?;
        Ok(Self {
            words: vec![0; word_len],
            byte_len,
        })
    }

    fn as_mut_void(&mut self) -> *mut c_void {
        self.words.as_mut_ptr().cast()
    }

    fn as_psid(&self) -> PSID {
        self.words.as_ptr().cast_mut().cast()
    }

    fn as_ptr<T>(&self) -> *const T {
        self.words.as_ptr().cast()
    }

    fn as_mut_ptr<T>(&mut self) -> *mut T {
        self.words.as_mut_ptr().cast()
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: this wrapper uniquely owns the process token handle.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn win32_bool(result: i32, operation: &str) -> io::Result<()> {
    if result == 0 {
        Err(last_win32_error(operation))
    } else {
        Ok(())
    }
}

fn last_win32_error(operation: &str) -> io::Error {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() } as i32;
    io::Error::other(format!(
        "{operation} failed: {}",
        io::Error::from_raw_os_error(code)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Security::{
        EqualSid, GetAce, GetKernelObjectSecurity, GetSecurityDescriptorDacl,
        DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    #[tokio::test]
    async fn named_pipe_dacl_allows_only_current_user_and_system() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let endpoint = format!(
            r"\\.\pipe\centaeris-runtime-security-test-{}-{nonce}",
            std::process::id()
        );
        let server = create_server(endpoint.as_str(), true).expect("create secured pipe");
        let descriptor = object_security_descriptor(server.as_raw_handle().cast())
            .expect("read pipe security descriptor");
        let mut dacl_present = 0;
        let mut dacl = ptr::null_mut();
        let mut dacl_defaulted = 0;
        // SAFETY: descriptor contains the self-relative security descriptor
        // returned by GetKernelObjectSecurity and all output pointers are valid.
        unsafe {
            win32_bool(
                GetSecurityDescriptorDacl(
                    descriptor.as_ptr::<c_void>().cast_mut(),
                    &mut dacl_present,
                    &mut dacl,
                    &mut dacl_defaulted,
                ),
                "GetSecurityDescriptorDacl",
            )
            .expect("read DACL");
        }
        assert_ne!(dacl_present, 0, "pipe must have an explicit DACL");
        assert!(!dacl.is_null(), "pipe DACL must not grant everyone access");
        // SAFETY: dacl is non-null and owned by the descriptor buffer.
        assert_eq!(unsafe { (*dacl).AceCount }, 2);

        let current_user = current_process_user_sid().expect("current user SID");
        let system = local_system_sid().expect("SYSTEM SID");
        let mut saw_user = false;
        let mut saw_system = false;
        for index in 0..2 {
            let mut raw_ace = ptr::null_mut();
            // SAFETY: both ACE indices are below the checked AceCount.
            unsafe {
                win32_bool(GetAce(dacl, index, &mut raw_ace), "GetAce").expect("read ACE");
                let ace = &*(raw_ace as *const ACCESS_ALLOWED_ACE);
                assert_eq!(ace.Mask, FILE_ALL_ACCESS);
                let sid = (&ace.SidStart as *const u32).cast_mut().cast();
                if EqualSid(sid, current_user.as_psid()) != 0 {
                    saw_user = true;
                } else if EqualSid(sid, system.as_psid()) != 0 {
                    saw_system = true;
                } else {
                    panic!("pipe DACL contains an unexpected trustee");
                }
            }
        }
        assert!(saw_user, "pipe DACL must grant the current user");
        assert!(saw_system, "pipe DACL must grant LocalSystem");
    }

    fn object_security_descriptor(handle: HANDLE) -> io::Result<AlignedBuffer> {
        let mut required = 0;
        // SAFETY: the first call intentionally supplies no output buffer.
        unsafe {
            GetKernelObjectSecurity(
                handle,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                0,
                &mut required,
            );
        }
        if required == 0 {
            return Err(last_win32_error("GetKernelObjectSecurity(size)"));
        }
        let mut descriptor = AlignedBuffer::new(required as usize)?;
        // SAFETY: descriptor is writable for `required` bytes.
        unsafe {
            win32_bool(
                GetKernelObjectSecurity(
                    handle,
                    DACL_SECURITY_INFORMATION,
                    descriptor.as_mut_void(),
                    required,
                    &mut required,
                ),
                "GetKernelObjectSecurity(DACL)",
            )?;
        }
        Ok(descriptor)
    }
}
