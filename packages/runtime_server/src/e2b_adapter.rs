//! Trusted host-side transport to the official Python SDK adapter.
//! Provider attempts and settlement remain owned by the calling store.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod bulk;
mod power_observations;
#[cfg(test)]
mod tests;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const REQUEST_SCHEMA: &str = "runtime.e2b.adapter.request.v1";
const RESPONSE_SCHEMA: &str = "runtime.e2b.adapter.response.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterFailure {
    /// This invocation did not enter the SDK process. Earlier attempts may exist.
    NotDispatched(&'static str),
    /// Retain the persisted attempt and investigation responsibility.
    Unknown(&'static str),
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AdapterOutcome {
    Succeeded,
    NotDispatched,
    Unknown,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterResponse {
    schema: String,
    pub request_id: String,
    pub outcome: AdapterOutcome,
    pub result: Option<Value>,
    pub error: Option<AdapterError>,
}

struct Streams {
    requests: mpsc::SyncSender<Vec<u8>>,
    responses: mpsc::Receiver<Result<Vec<u8>, ()>>,
}

/// One bounded SDK lane. Use separate processes for ordinary, short control and
/// slow cleanup because the SDK connection pool is process-wide.
///
/// Launch the trusted Python interpreter directly, without a shell or launcher.
/// Calls never retry or replay automatically. Process termination is transport
/// cleanup; it is not evidence that a provider side effect has stopped.
pub struct AdapterProcess {
    child: Mutex<Child>,
    streams: Mutex<Streams>,
    closed: AtomicBool,
}

impl AdapterProcess {
    pub(crate) fn spawn_observed(
        command: Command,
        observation: crate::observations::power::PowerObservation,
    ) -> Result<Self, String> {
        Self::spawn_inner(command, Some(observation))
    }

    pub fn spawn(command: Command) -> Result<Self, String> {
        Self::spawn_inner(command, None)
    }

    fn spawn_inner(
        mut command: Command,
        observation: Option<crate::observations::power::PowerObservation>,
    ) -> Result<Self, String> {
        let diagnostics = observation
            .filter(|value| value.is_enabled())
            .and_then(power_observations::start_reader);
        if diagnostics.is_some() {
            command.env("CENTAERIS_E2B_POWER_OBSERVATIONS", "1");
        } else {
            command.env_remove("CENTAERIS_E2B_POWER_OBSERVATIONS");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Diagnostics may contain credentials or workload data.
            .stderr(if diagnostics.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .spawn()
            .map_err(|_| "e2b_adapter_start_failed".to_string())?;
        if let (Some(sender), Some(stderr)) = (diagnostics, child.stderr.take()) {
            let _ = sender.send(stderr);
        }
        let mut input = child.stdin.take().expect("piped adapter stdin");
        let output = child.stdout.take().expect("piped adapter stdout");
        let (send_response, responses) = mpsc::sync_channel(1);
        let (requests, receive_request) = mpsc::sync_channel::<Vec<u8>>(1);
        let send_write_error = send_response.clone();
        let writer = std::thread::Builder::new()
            .name("e2b-adapter-requests".into())
            .spawn(move || {
                while let Ok(frame) = receive_request.recv() {
                    if input
                        .write_all(&frame)
                        .and_then(|()| input.flush())
                        .is_err()
                    {
                        let _ = send_write_error.send(Err(()));
                        break;
                    }
                }
            });
        let reader = if writer.is_ok() {
            std::thread::Builder::new()
                .name("e2b-adapter-responses".into())
                .spawn(move || {
                    let mut reader = BufReader::new(output);
                    loop {
                        let mut frame = Vec::new();
                        let result = (&mut reader)
                            .take((MAX_FRAME_BYTES + 1) as u64)
                            .read_until(b'\n', &mut frame);
                        if !matches!(result, Ok(size) if size > 0 && size <= MAX_FRAME_BYTES)
                            || frame.last() != Some(&b'\n')
                        {
                            let _ = send_response.send(Err(()));
                            break;
                        }
                        if send_response.send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                })
        } else {
            Err(std::io::Error::other("adapter writer unavailable"))
        };
        if reader.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("e2b_adapter_io_start_failed".into());
        }
        Ok(Self {
            child: Mutex::new(child),
            streams: Mutex::new(Streams {
                requests,
                responses,
            }),
            closed: AtomicBool::new(false),
        })
    }

    pub fn call(
        &self,
        request_id: &str,
        operation: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        let started = Instant::now();
        if !valid_text(request_id, 128)
            || !valid_text(operation, 64)
            || !payload.is_object()
            || timeout.is_zero()
        {
            return Err(AdapterFailure::NotDispatched("adapter_request_invalid"));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(AdapterFailure::NotDispatched("adapter_closed"));
        }
        // Refuse excess work rather than wait behind an unrelated long request.
        let streams = match self.streams.try_lock() {
            Ok(streams) => streams,
            Err(TryLockError::WouldBlock) => {
                return Err(AdapterFailure::NotDispatched("adapter_busy"));
            }
            Err(TryLockError::Poisoned(_)) => {
                self.close();
                return Err(AdapterFailure::NotDispatched("adapter_closed"));
            }
        };
        if self.closed.load(Ordering::Acquire) {
            return Err(AdapterFailure::NotDispatched("adapter_closed"));
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            schema: &'static str,
            request_id: &'a str,
            operation: &'a str,
            payload: Value,
        }
        let mut frame = BoundedFrame(Vec::new());
        serde_json::to_writer(
            &mut frame,
            &Request {
                schema: REQUEST_SCHEMA,
                request_id,
                operation,
                payload,
            },
        )
        .and_then(|()| frame.write_all(b"\n").map_err(serde_json::Error::io))
        .map_err(|_| AdapterFailure::NotDispatched("adapter_request_too_large"))?;
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(AdapterFailure::NotDispatched(
                "adapter_deadline_before_dispatch",
            ));
        }
        // The dedicated writer prevents a full stdin pipe bypassing the deadline.
        streams
            .requests
            .try_send(frame.0)
            .map_err(|_| AdapterFailure::NotDispatched("adapter_request_channel_unavailable"))?;
        let response = match streams.responses.recv_timeout(remaining) {
            Ok(Ok(frame)) => serde_json::from_slice::<AdapterResponse>(&frame)
                .map_err(|_| AdapterFailure::Unknown("adapter_response_invalid")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(AdapterFailure::Unknown("adapter_deadline_exceeded"))
            }
            Ok(Err(())) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(AdapterFailure::Unknown("adapter_response_unavailable"))
            }
        };
        let response = response.and_then(|value| {
            if value.schema != RESPONSE_SCHEMA
                || value.request_id != request_id
                || match value.outcome {
                    AdapterOutcome::Succeeded => value.result.is_none() || value.error.is_some(),
                    AdapterOutcome::NotDispatched | AdapterOutcome::Unknown => {
                        value.error.is_none()
                    }
                }
            {
                Err(AdapterFailure::Unknown("adapter_response_invalid"))
            } else {
                Ok(value)
            }
        });
        if response.is_err() {
            self.close();
        }
        response
    }

    pub fn close(&self) {
        let _ = self.stop_and_wait();
    }

    pub(super) fn stop_and_wait(&self) -> Result<(), &'static str> {
        self.closed.store(true, Ordering::Release);
        let mut child = self
            .child
            .lock()
            .map_err(|_| "adapter_child_lock_unavailable")?;
        if child
            .try_wait()
            .map_err(|_| "adapter_child_status_unavailable")?
            .is_some()
        {
            return Ok(());
        }
        if child.kill().is_err() {
            return if child
                .try_wait()
                .map_err(|_| "adapter_child_status_unavailable")?
                .is_some()
            {
                Ok(())
            } else {
                Err("adapter_child_stop_unconfirmed")
            };
        }
        child
            .wait()
            .map(|_| ())
            .map_err(|_| "adapter_child_wait_unconfirmed")
    }
}

impl Drop for AdapterProcess {
    fn drop(&mut self) {
        self.close();
    }
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

struct BoundedFrame(Vec<u8>);

impl Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("adapter frame limit exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
