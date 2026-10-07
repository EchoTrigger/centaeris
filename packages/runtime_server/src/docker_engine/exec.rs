//! One attached Engine exec. Disconnecting never means remote cancellation.
use super::{shared, MUTATION_TIMEOUT, QUERY_TIMEOUT};
use bollard::container::LogOutput;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::ExecConfig;
use futures::TryStreamExt;
use std::io::{self, Read, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::watch;

const BUFFER_BYTES: usize = 64 * 1024;
// Streaming reads use the host's operation deadline; this bounds an abandoned
// helper which has no caller deadline. It is not the MCP connection lifetime.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecRequest {
    pub container: String,
    pub command: Vec<String>,
    pub user: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<String>,
    pub expected_labels: std::collections::BTreeMap<String, String>,
}

impl ExecRequest {
    pub(crate) fn new(container: &str, command: Vec<String>) -> Self {
        Self {
            container: container.into(),
            command,
            user: None,
            cwd: None,
            env: Vec::new(),
            expected_labels: Default::default(),
        }
    }

    pub(crate) fn owned(container: &str, run: &str, execution: &str, command: Vec<String>) -> Self {
        let mut request = Self::new(container, command);
        request.expected_labels = [
            ("centaeris.managed".into(), "true".into()),
            ("centaeris.agent_run_id".into(), run.into()),
            ("centaeris.execution_id".into(), execution.into()),
        ]
        .into();
        request
    }

    fn target_id(
        &self,
        facts: &bollard::models::ContainerInspectResponse,
    ) -> Result<String, String> {
        let labels = facts
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        if !self
            .expected_labels
            .iter()
            .all(|(key, value)| labels.and_then(|labels| labels.get(key)) == Some(value))
        {
            return Err("Docker exec container identity mismatch; refusing dispatch".into());
        }
        if facts.state.as_ref().and_then(|state| state.running) != Some(true) {
            return Err("Docker exec container is not running".into());
        }
        facts
            .id
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "Docker exec container ID missing".into())
    }

    fn config(&self) -> ExecConfig {
        ExecConfig {
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(false),
            privileged: Some(false),
            cmd: Some(self.command.clone()),
            user: self.user.clone(),
            working_dir: self.cwd.clone(),
            env: Some(self.env.clone()),
            ..Default::default()
        }
    }

    pub(crate) fn spawn(&self) -> Result<ExecChild, String> {
        let engine = shared()?;
        engine.run(MUTATION_TIMEOUT, self.spawn_async())
    }

    pub(crate) async fn spawn_async(&self) -> Result<ExecChild, String> {
        let engine = shared()?;
        // Bind the name once. Exec create and start subsequently use immutable
        // daemon identities and are each sent exactly once, without replay.
        let facts = tokio::time::timeout(
            QUERY_TIMEOUT,
            engine.docker.inspect_container(
                &self.container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            ),
        )
        .await
        .map_err(|_| "Docker exec identity lookup timed out")?
        .map_err(|e| e.to_string())?;
        let container = self.target_id(&facts)?;
        let created = tokio::time::timeout(
            MUTATION_TIMEOUT,
            engine.docker.create_exec(&container, self.config()),
        )
        .await
        .map_err(|_| "Docker exec creation outcome unknown; not replayed")?
        .map_err(|e| format!("Docker exec creation failed; not replayed: {e}"))?;
        let started = tokio::time::timeout(
            MUTATION_TIMEOUT,
            engine.docker.start_exec(
                &created.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: false,
                    output_capacity: Some(BUFFER_BYTES),
                }),
            ),
        )
        .await
        .map_err(|_| "Docker exec start outcome unknown; not replayed")?
        .map_err(|e| format!("Docker exec start outcome unknown; not replayed: {e}"))?;
        let StartExecResults::Attached { output, input } = started else {
            return Err("Docker exec unexpectedly detached".into());
        };
        let docker = engine.docker.clone();
        let id = created.id;
        let lookup = id.clone();
        let exit = async move {
            tokio::time::timeout(QUERY_TIMEOUT, async {
                loop {
                    let facts = docker
                        .inspect_exec(&lookup)
                        .await
                        .map_err(|e| e.to_string())?;
                    if facts.running == Some(false) {
                        let code = facts.exit_code.ok_or("Docker exec exit code missing")?;
                        return i32::try_from(code)
                            .map(ExecStatus)
                            .map_err(|_| "Docker exec exit code out of range".to_string());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| "Docker exec exit outcome unknown; not replayed".to_string())?
        };
        Ok(attach_streams(
            engine.runtime.handle(),
            id,
            output,
            input,
            exit,
        ))
    }
}

fn attach_streams(
    runtime: &tokio::runtime::Handle,
    id: String,
    mut output: Pin<
        Box<dyn futures::Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>,
    >,
    mut input: Pin<Box<dyn AsyncWrite + Send>>,
    exit: impl std::future::Future<Output = Result<ExecStatus, String>> + Send + 'static,
) -> ExecChild {
    let (stdin, mut source) = tokio::io::duplex(BUFFER_BYTES);
    let (stdout, mut stdout_sink) = tokio::io::duplex(BUFFER_BYTES);
    let (stderr, mut stderr_sink) = tokio::io::duplex(BUFFER_BYTES);
    let (completion, state) = watch::channel(None);
    let completed = completion.clone();
    let task = runtime.spawn(async move {
        let result = async {
            let send = async {
                tokio::io::copy(&mut source, &mut input)
                    .await
                    .map_err(|e| e.to_string())?;
                input.shutdown().await.map_err(|e| e.to_string())
            };
            let receive = async {
                while let Some(frame) = output.try_next().await.map_err(|e| e.to_string())? {
                    match frame {
                        LogOutput::StdOut { message } => stdout_sink.write_all(&message).await,
                        LogOutput::StdErr { message } => stderr_sink.write_all(&message).await,
                        _ => return Err("unexpected Docker exec stream type".to_string()),
                    }
                    .map_err(|e| e.to_string())?;
                }
                exit.await
            };
            tokio::pin!(send, receive);
            tokio::select! {
                result = &mut receive => result,
                result = &mut send => { result?; receive.await }
            }
        }
        .await;
        completed.send_if_modified(|state| {
            if state.is_none() {
                *state = Some(result);
                true
            } else {
                false
            }
        });
    });
    let control = Arc::new(ExecControl {
        task: task.abort_handle(),
        completion,
    });
    ExecChild {
        id,
        stdin: Some(ExecWriter {
            inner: stdin,
            control: control.clone(),
        }),
        stdout: Some(ExecReader {
            inner: stdout,
            control: control.clone(),
        }),
        stderr: Some(ExecReader {
            inner: stderr,
            control: control.clone(),
        }),
        state,
        control,
    }
}

struct ExecControl {
    task: tokio::task::AbortHandle,
    completion: watch::Sender<Option<Result<ExecStatus, String>>>,
}
impl ExecControl {
    fn disconnect(&self) {
        self.completion.send_if_modified(|state| {
            if state.is_none() {
                *state = Some(Err(
                    "Docker exec transport disconnected; remote outcome unknown".into(),
                ));
                true
            } else {
                false
            }
        });
        self.task.abort();
    }
}
impl Drop for ExecControl {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecStatus(i32);
impl ExecStatus {
    pub(crate) fn code(self) -> Option<i32> {
        Some(self.0)
    }
    pub(crate) fn success(self) -> bool {
        self.0 == 0
    }
}
impl std::fmt::Display for ExecStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit code {}", self.0)
    }
}

pub(crate) struct ExecChild {
    #[cfg_attr(not(test), allow(dead_code))]
    pub id: String,
    pub stdin: Option<ExecWriter>,
    pub stdout: Option<ExecReader>,
    pub stderr: Option<ExecReader>,
    state: watch::Receiver<Option<Result<ExecStatus, String>>>,
    control: Arc<ExecControl>,
}
impl ExecChild {
    pub(crate) fn try_wait(&mut self) -> Result<Option<ExecStatus>, String> {
        self.state.borrow().clone().transpose()
    }
    pub(crate) fn wait(&mut self) -> Result<ExecStatus, String> {
        shared()?.run(STREAM_IDLE_TIMEOUT, async {
            loop {
                if let Some(result) = self.state.borrow().clone() {
                    return result;
                }
                self.state
                    .changed()
                    .await
                    .map_err(|_| "Docker exec completion lost".to_string())?;
            }
        })
    }
    /// Closes local I/O only. The host must separately confirm cancellation.
    pub(crate) fn disconnect(&mut self) {
        self.stdin.take();
        self.control.disconnect();
    }
}
impl Drop for ExecChild {
    fn drop(&mut self) {
        self.disconnect();
    }
}

pub(crate) struct ExecReader {
    inner: DuplexStream,
    control: Arc<ExecControl>,
}
impl AsyncRead for ExecReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let previous = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(())))
            && buf.filled().len() == previous
            && buf.remaining() > 0
        {
            if let Some(Err(error)) = &*self.control.completion.borrow() {
                return Poll::Ready(Err(io::Error::other(error.clone())));
            }
        }
        result
    }
}
impl Read for ExecReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        shared()
            .map_err(io::Error::other)?
            .run(STREAM_IDLE_TIMEOUT, async {
                AsyncReadExt::read(self, buf)
                    .await
                    .map_err(|e| e.to_string())
            })
            .map_err(io::Error::other)
    }
}
pub(crate) struct ExecWriter {
    inner: DuplexStream,
    control: Arc<ExecControl>,
}
impl AsyncWrite for ExecWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Some(Err(error)) = &*self.control.completion.borrow() {
            return Poll::Ready(Err(io::Error::other(error.clone())));
        }
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
impl Write for ExecWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        shared()
            .map_err(io::Error::other)?
            .run(STREAM_IDLE_TIMEOUT, async {
                AsyncWriteExt::write(self, bytes)
                    .await
                    .map_err(|e| e.to_string())
            })
            .map_err(io::Error::other)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(
        frames: Vec<Result<LogOutput, bollard::errors::Error>>,
        exit: Result<ExecStatus, String>,
    ) -> ExecChild {
        attach_streams(
            &tokio::runtime::Handle::current(),
            "fixture-exec".into(),
            Box::pin(futures::stream::iter(frames)),
            Box::pin(tokio::io::sink()),
            async { exit },
        )
    }
    #[tokio::test]
    async fn attached_binary_streams_preserve_separation_and_nonzero_exit() {
        let bytes = vec![255u8; BUFFER_BYTES * 3];
        let mut child = fixture(
            vec![
                Ok(LogOutput::StdOut {
                    message: bytes.clone().into(),
                }),
                Ok(LogOutput::StdErr {
                    message: b"diagnostic".to_vec().into(),
                }),
            ],
            Ok(ExecStatus(7)),
        );
        child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (a, b) = tokio::join!(
            AsyncReadExt::read_to_end(&mut stdout, &mut out),
            AsyncReadExt::read_to_end(&mut stderr, &mut err)
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(out, bytes);
        assert_eq!(err, b"diagnostic");
        assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(7));
    }
    #[tokio::test]
    async fn attachment_failure_and_unconfirmed_exit_never_become_successful_eof() {
        for child in [
            fixture(
                vec![Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 503,
                    message: "lost".into(),
                })],
                Ok(ExecStatus(0)),
            ),
            fixture(Vec::new(), Err("exit unknown".into())),
        ] {
            let mut child = child;
            let mut stdout = child.stdout.take().unwrap();
            assert!(AsyncReadExt::read_to_end(&mut stdout, &mut Vec::new())
                .await
                .is_err());
            assert!(child.try_wait().is_err());
        }
    }
    #[tokio::test]
    async fn disconnect_unblocks_readers_without_claiming_remote_cancellation() {
        let mut child = attach_streams(
            &tokio::runtime::Handle::current(),
            "fixture-exec".into(),
            Box::pin(futures::stream::pending()),
            Box::pin(tokio::io::sink()),
            async { Ok(ExecStatus(0)) },
        );
        let mut stdout = child.stdout.take().unwrap();
        child.disconnect();
        let error = child.try_wait().unwrap_err();
        assert!(error.contains("remote outcome unknown"));
        assert!(tokio::time::timeout(
            Duration::from_secs(1),
            AsyncReadExt::read_to_end(&mut stdout, &mut Vec::new())
        )
        .await
        .unwrap()
        .is_err());
    }
    #[test]
    fn exec_request_preserves_arguments_environment_and_identity_without_shell_parsing() {
        let mut request =
            ExecRequest::new("owned", vec!["/bin/tool".into(), "a b;$(literal)".into()]);
        request.user = Some("10001:10001".into());
        request.cwd = Some("/mnt/data".into());
        request.env = vec!["X=a=b".into()];
        let body = request.config();
        assert_eq!(body.cmd, Some(request.command));
        assert_eq!(body.env, Some(request.env));
        assert_eq!(body.user, request.user);
        assert_eq!(body.working_dir, request.cwd);
        assert_eq!(body.tty, Some(false));
        assert_eq!(body.privileged, Some(false));
    }

    #[test]
    fn exec_resolves_only_the_expected_live_container() {
        let request = ExecRequest::owned("name", "run", "execution", vec!["tool".into()]);
        let mut facts: bollard::models::ContainerInspectResponse = serde_json::from_value(serde_json::json!({
            "Id":"immutable", "State":{"Running":true}, "Config":{"Labels":request.expected_labels}
        })).unwrap();
        assert_eq!(request.target_id(&facts).unwrap(), "immutable");
        facts
            .config
            .as_mut()
            .unwrap()
            .labels
            .as_mut()
            .unwrap()
            .insert("centaeris.execution_id".into(), "foreign".into());
        assert!(request.target_id(&facts).is_err());
        facts.config.as_mut().unwrap().labels =
            Some(request.expected_labels.clone().into_iter().collect());
        facts.state.as_mut().unwrap().running = Some(false);
        assert!(request.target_id(&facts).is_err());
    }

    #[tokio::test]
    async fn stdin_eof_and_output_progress_concurrently_beyond_pipe_capacity() {
        let (input, mut remote) = tokio::io::duplex(BUFFER_BYTES);
        let frames = futures::stream::once(async move {
            let mut bytes = Vec::new();
            remote.read_to_end(&mut bytes).await.unwrap();
            Ok(LogOutput::StdOut {
                message: bytes.into(),
            })
        });
        let mut child = attach_streams(
            &tokio::runtime::Handle::current(),
            "fixture".into(),
            Box::pin(frames),
            Box::pin(input),
            async { Ok(ExecStatus(0)) },
        );
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let payload = vec![42; BUFFER_BYTES * 3];
        let expected = payload.clone();
        let send = async move {
            AsyncWriteExt::write_all(&mut stdin, &payload)
                .await
                .unwrap();
            drop(stdin);
        };
        let receive = async move {
            let mut bytes = Vec::new();
            AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
                .await
                .unwrap();
            bytes
        };
        let (_, bytes) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(send, receive)
        })
        .await
        .unwrap();
        assert_eq!(bytes, expected);
        assert!(child.try_wait().unwrap().unwrap().success());
    }
}
