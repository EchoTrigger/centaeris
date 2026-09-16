use super::super::nono_policy::{capabilities, unavailable};
use centaeris_core::execution::{ExecutionError, ExecutionPolicy};
use nono::sandbox::{detect_abi, PreparedLandlockSandbox, SeccompOpts};
use nono::Sandbox;
use std::path::Path;
use std::process::Command;

pub(super) fn prepare(
    policy: &ExecutionPolicy,
    program: &Path,
    runtime: &Path,
    scratch: &Path,
    control: Option<&Path>,
) -> Result<PreparedLandlockSandbox, ExecutionError> {
    let caps = capabilities(policy, program, runtime, Some(scratch), control)?;
    let abi = detect_abi().map_err(unavailable)?;
    if !abi.has_truncate() {
        return Err(unavailable(
            "local nono execution requires Landlock ABI 3 or newer",
        ));
    }
    Sandbox::prepare_seccomp_with_abi(&caps, &abi, SeccompOpts::network_baseline())
        .map_err(unavailable)
}

pub(super) fn attach(command: &mut Command, sandbox: PreparedLandlockSandbox) {
    use std::os::unix::process::CommandExt;
    let fixed_boundary = fixed_socket_boundary();
    // SAFETY: nono's prepared application uses raw syscalls without allocation.
    // The closure owns all opened capability descriptors until exec closes them.
    unsafe {
        command.pre_exec(move || {
            sandbox
                .apply_raw()
                .map_err(|e| std::io::Error::from_raw_os_error(e.errno()))?;
            let filter = libc::sock_fprog {
                len: fixed_boundary.len() as u16,
                filter: fixed_boundary.as_ptr().cast_mut(),
            };
            if libc::prctl(libc::PR_SET_SECCOMP, 2, &filter) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

// nono's static network baseline intentionally permits AF_UNIX. Local tool
// processes have no Host socket grants. Keep anonymous socketpair IPC working,
// while refusing creation of sockets that could connect to a Host service.
// io_uring and descriptor/memory extraction cannot bypass this fixed boundary.
// This filter contains no path policy, profiles, prompts or dynamic grants.
fn fixed_socket_boundary() -> Vec<libc::sock_filter> {
    fn instruction(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        }
    }
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc00000b7;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!("local nono execution currently supports Linux x86_64 and aarch64");
    let load = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
    let equal = libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K;
    let ret = libc::BPF_RET | libc::BPF_K;
    let denied = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
    // Linux UAPI seccomp_data offsets: nr=0, arch=4, args[0]=16.
    let mut filter = vec![
        instruction(load, 0, 0, 4),
        instruction(equal, 1, 0, AUDIT_ARCH),
        instruction(ret, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        instruction(load, 0, 0, 0),
        instruction(
            libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K,
            0,
            1,
            0x40000000,
        ),
        instruction(ret, 0, 0, denied),
    ];
    for syscall in [
        libc::SYS_io_uring_setup,
        libc::SYS_pidfd_getfd,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
    ] {
        filter.push(instruction(equal, 0, 1, syscall as u32));
        filter.push(instruction(ret, 0, 0, denied));
    }
    filter.extend([
        instruction(equal, 0, 3, libc::SYS_socket as u32),
        instruction(load, 0, 0, 16),
        instruction(equal, 0, 1, libc::AF_UNIX as u32),
        instruction(ret, 0, 0, denied),
        instruction(ret, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn runtime_executable_cannot_be_inside_a_writable_grant() {
        let root =
            std::env::temp_dir().join(format!("centaeris-nono-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let runtime = root.join("runtime");
        std::fs::write(&runtime, "fixture executable").unwrap();
        let policy = ExecutionPolicy::workspace_write_no_network(&root);
        let rejected = prepare(&policy, Path::new("/usr/bin/true"), &runtime, &root, None).is_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(
            rejected,
            "the tool must not be able to replace its trusted Runtime helper"
        );
    }

    #[test]
    fn private_unix_socket_is_inaccessible_even_with_public_internet() {
        let root =
            std::env::temp_dir().join(format!("centaeris-nono-socket-{}", std::process::id()));
        let workspace = root.join("workspace");
        let scratch = root.join("scratch");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let socket = root.join("private.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let policy = ExecutionPolicy::workspace_write_public_internet(&workspace);
        let sandbox = prepare(
            &policy,
            Path::new("/usr/bin/python3"),
            Path::new("/usr/bin/python3"),
            &scratch,
            None,
        )
        .unwrap();
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", "import socket,sys\ntry:\n s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])\nexcept PermissionError: sys.exit(0)\nsys.exit(77)"]).arg(&socket);
        attach(&mut command, sandbox);
        let status = command.status().unwrap();
        let connected = listener.accept().is_ok();
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(status.code(), Some(0));
        assert!(!connected);
    }
}
