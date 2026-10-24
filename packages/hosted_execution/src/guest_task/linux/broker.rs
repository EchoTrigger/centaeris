//! Root-private resident broker. Core owns task authority and durable receipts.
use super::*;
use std::{
    io::{BufRead, BufReader},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    thread,
    time::Duration,
};
const SOCKET_ROOT: &str = "/run/centaeris-guest";
const SOCKET: &str = "/run/centaeris-guest/task.sock";
const MAX_REQUEST: usize = 128 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const IO_DEADLINE: Duration = Duration::from_secs(10);

trait Endpoint: Send + Sync {
    fn handle(&self, envelope: EnvironmentTaskEnvelope) -> EnvironmentTaskResponse;
}
impl Endpoint for GuestTaskService {
    fn handle(&self, envelope: EnvironmentTaskEnvelope) -> EnvironmentTaskResponse {
        self.handle(envelope)
    }
}
type Factory = dyn Fn(&EnvironmentTaskEnvelope) -> Result<Arc<dyn Endpoint>, String> + Send + Sync;
struct Broker {
    service: Mutex<Option<Arc<dyn Endpoint>>>,
    factory: Box<Factory>,
}
impl Broker {
    fn new(
        factory: impl Fn(&EnvironmentTaskEnvelope) -> Result<Arc<dyn Endpoint>, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            service: Mutex::new(None),
            factory: Box::new(factory),
        }
    }
    fn handle(&self, envelope: EnvironmentTaskEnvelope) -> EnvironmentTaskResponse {
        let service = (|| {
            envelope.validate()?;
            envelope.request.validate_authority()?;
            let mut service = self
                .service
                .lock()
                .map_err(|_| "guest_broker_unavailable")?;
            if service.is_none() {
                if !matches!(envelope.request, EnvironmentTaskRequest::Takeover { .. }) {
                    return Err("guest_initial_takeover_required".into());
                }
                *service = Some((self.factory)(&envelope)?);
            }
            Ok(service.as_ref().expect("initialized service").clone())
        })();
        match service {
            Ok(service) => service.handle(envelope),
            Err(error) => EnvironmentTaskResponse::from_result(Err(error)),
        }
    }
}

fn root_peer(stream: &UnixStream) -> bool {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    // SAFETY: the socket and credential output storage are valid.
    (unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    }) == 0
        && cred.uid == 0
}
fn frame(reader: &mut impl BufRead, limit: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut data)
        .map_err(|_| "guest_frame_invalid")?;
    if data.len() > limit || data.last() != Some(&b'\n') {
        return Err("guest_frame_invalid".into());
    }
    Ok(data)
}
fn response(stream: &mut UnixStream, value: &EnvironmentTaskResponse) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "guest_response_invalid")?;
    if bytes.len() >= MAX_RESPONSE {
        return Err("guest_response_oversized".into());
    }
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|_| "guest_response_unknown".into())
}
fn exchange(broker: &Broker, mut stream: UnixStream) -> Result<(), String> {
    if !root_peer(&stream) {
        return Err("guest_controller_unauthorized".into());
    }
    stream
        .set_read_timeout(Some(IO_DEADLINE))
        .and_then(|()| stream.set_write_timeout(Some(IO_DEADLINE)))
        .map_err(|_| "guest_socket_unavailable")?;
    let request = (|| {
        let mut reader =
            BufReader::new(stream.try_clone().map_err(|_| "guest_socket_unavailable")?);
        let bytes = frame(&mut reader, MAX_REQUEST)?;
        let envelope: EnvironmentTaskEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| "guest_request_invalid")?;
        Ok(envelope)
    })();
    let value = match request {
        Ok(envelope) => broker.handle(envelope),
        Err(error) => EnvironmentTaskResponse::from_result(Err(error)),
    };
    response(&mut stream, &value)
}

/// Stops only in tests. On return all accepted request threads have joined;
/// stopping a listener never fabricates physical task cleanup or exit receipts.
fn serve(listener: UnixListener, broker: Arc<Broker>, stop: &AtomicBool) -> Result<(), String> {
    let active = AtomicUsize::new(0);
    thread::scope(|scope| {
        for stream in listener.incoming() {
            let mut stream = stream.map_err(|_| "guest_socket_accept_failed")?;
            if stop.load(Ordering::Acquire) {
                break;
            }
            if active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 8).then_some(n + 1)
                })
                .is_err()
            {
                let _ = stream.set_write_timeout(Some(IO_DEADLINE));
                let _ = response(
                    &mut stream,
                    &EnvironmentTaskResponse::from_result(Err("guest_broker_busy".into())),
                );
                continue;
            }
            let active = &active;
            let broker = broker.clone();
            scope.spawn(move || {
                struct Permit<'a>(&'a AtomicUsize);
                impl Drop for Permit<'_> {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _permit = Permit(active);
                let _ = exchange(&broker, stream);
            });
        }
        Ok(())
    })
}

fn trusted_socket(path: &Path) -> Result<(), String> {
    private_existing(path.parent().ok_or("guest_private_path_invalid")?, true)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "guest_broker_unavailable")?;
    if !metadata.file_type().is_socket() || metadata.uid() != 0 || metadata.mode() & 0o777 != 0o600
    {
        return Err("guest_socket_untrusted".into());
    }
    Ok(())
}
fn connect(path: &Path) -> Result<UnixStream, String> {
    trusted_socket(path)?;
    let stream = UnixStream::connect(path).map_err(|_| "guest_broker_unavailable")?;
    if !root_peer(&stream) {
        return Err("guest_broker_unauthorized".into());
    }
    stream
        .set_read_timeout(Some(IO_DEADLINE))
        .and_then(|()| stream.set_write_timeout(Some(IO_DEADLINE)))
        .map_err(|_| "guest_socket_unavailable")?;
    Ok(stream)
}

/// Once bytes may have reached the broker, never reconnect or replay the request.
/// The caller can reconcile a Core operation receipt; local timeout is unknown.
fn request_once(
    mut stream: UnixStream,
    envelope: &EnvironmentTaskEnvelope,
) -> Result<EnvironmentTaskResponse, String> {
    envelope.validate()?;
    envelope.request.validate_authority()?;
    let mut data = serde_json::to_vec(envelope).map_err(|_| "guest_request_invalid")?;
    if data.len() >= MAX_REQUEST {
        return Err("guest_request_oversized".into());
    }
    data.push(b'\n');
    stream
        .write_all(&data)
        .and_then(|()| stream.shutdown(Shutdown::Write))
        .map_err(|_| "guest_broker_reply_unknown")?;
    let mut reader = BufReader::new(stream);
    let bytes = frame(&mut reader, MAX_RESPONSE).map_err(|_| "guest_broker_reply_unknown")?;
    let response: EnvironmentTaskResponse =
        serde_json::from_slice(&bytes).map_err(|_| "guest_broker_reply_unknown")?;
    response
        .validate()
        .map_err(|_| "guest_broker_reply_unknown")?;
    Ok(response)
}

fn lease(root: &Path) -> Result<File, String> {
    private_directory(root)?;
    let path = root.join("broker.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| "guest_broker_lock_unavailable")?;
    private_existing(&path, false)?;
    // SAFETY: file owns a valid descriptor; this lease is nonblocking.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("guest_broker_already_running".into());
    }
    Ok(file)
}
fn bind(path: &Path) -> Result<UnixListener, String> {
    if fs::symlink_metadata(path).is_ok() {
        trusted_socket(path)?;
        match UnixStream::connect(path) {
            Ok(_) => return Err("guest_broker_already_running".into()),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                fs::remove_file(path).map_err(|_| "guest_stale_socket_unavailable")?
            }
            Err(_) => return Err("guest_socket_untrusted".into()),
        }
    }
    let listener = UnixListener::bind(path).map_err(|_| "guest_socket_bind_failed")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "guest_socket_unavailable")?;
    trusted_socket(path)?;
    Ok(listener)
}

pub(super) fn run_broker() -> Result<(), String> {
    if std::env::args().len() != 2 {
        return Err("guest_fixed_entrypoint_required".into());
    }
    // SAFETY: geteuid has no arguments or side effects.
    if unsafe { libc::geteuid() } != 0 {
        return Err("guest_broker_requires_root".into());
    }
    let _lease = lease(Path::new(SOCKET_ROOT))?;
    let listener = bind(Path::new(SOCKET))?;
    let broker = Arc::new(Broker::new(|initial| {
        Ok(Arc::new(GuestTaskService::open(initial)?))
    }));
    serve(listener, broker, &AtomicBool::new(false))
}

/// The template init must already own the broker in a separate service domain.
/// A one-shot client never detaches a descendant or takes process ownership.
fn client_request(
    path: &Path,
    envelope: &EnvironmentTaskEnvelope,
) -> Result<EnvironmentTaskResponse, String> {
    envelope.validate()?;
    envelope.request.validate_authority()?;
    request_once(connect(path)?, envelope)
}

pub(super) fn run_client() -> Result<(), String> {
    if std::env::args().len() != 2 {
        return Err("guest_fixed_entrypoint_required".into());
    }
    // SAFETY: geteuid has no arguments or side effects.
    if unsafe { libc::geteuid() } != 0 {
        return Err("guest_broker_requires_root".into());
    }
    let result = (|| {
        let bytes = frame(&mut std::io::stdin().lock(), MAX_REQUEST)?;
        let envelope: EnvironmentTaskEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| "guest_request_invalid")?;
        client_request(Path::new(SOCKET), &envelope)
    })();
    let value = result.unwrap_or_else(|error| EnvironmentTaskResponse::from_result(Err(error)));
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value).map_err(|_| "guest_response_unknown")?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|_| "guest_response_unknown".into())
}

#[cfg(test)]
mod tests;
