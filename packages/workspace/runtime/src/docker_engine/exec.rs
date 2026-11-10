//! Fenced environment task I/O. Disconnecting never means remote cancellation.
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::watch;

const BUFFER_BYTES: usize = 64 * 1024;

struct ExecControl {
    task: tokio::task::AbortHandle,
    id: String,
    completion: watch::Sender<Option<Result<ExecStatus, String>>>,
    eof_wakers: Arc<[futures::task::AtomicWaker; 2]>,
}
impl ExecControl {
    fn disconnect(&self) {
        self.completion.send_if_modified(|state| {
            if state.is_none() {
                *state = Some(Err(format!(
                    "environment task {} transport disconnected; remote outcome unknown",
                    self.id
                )));
                true
            } else {
                false
            }
        });
        for waker in self.eof_wakers.iter() {
            waker.wake();
        }
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
    #[cfg(test)]
    pub(crate) fn code(self) -> Option<i32> {
        Some(self.0)
    }
}
impl From<i32> for ExecStatus {
    fn from(code: i32) -> Self {
        Self(code)
    }
}
impl std::fmt::Display for ExecStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit code {}", self.0)
    }
}

pub(crate) struct ExecChild {
    pub stdin: Option<ExecWriter>,
    pub stdout: Option<ExecReader>,
    pub stderr: Option<ExecReader>,
    control: Arc<ExecControl>,
}
impl ExecChild {
    /// Share the I/O/completion shell without granting a Docker execution path.
    pub(crate) fn from_streams<F>(
        runtime: &tokio::runtime::Handle,
        id: String,
        driver: impl FnOnce(DuplexStream, DuplexStream, DuplexStream) -> F + Send + 'static,
    ) -> Self
    where
        F: std::future::Future<Output = Result<ExecStatus, String>> + Send + 'static,
    {
        let (stdin, source) = tokio::io::duplex(BUFFER_BYTES);
        let (stdout, stdout_sink) = tokio::io::duplex(BUFFER_BYTES);
        let (stderr, stderr_sink) = tokio::io::duplex(BUFFER_BYTES);
        let (completion, _) = watch::channel(None);
        let eof_wakers = Arc::new([
            futures::task::AtomicWaker::new(),
            futures::task::AtomicWaker::new(),
        ]);
        let finished_wakers = eof_wakers.clone();
        let completed = completion.clone();
        let task = runtime.spawn(async move {
            let result = driver(source, stdout_sink, stderr_sink).await;
            completed.send_if_modified(|state| {
                if state.is_none() {
                    *state = Some(result);
                    true
                } else {
                    false
                }
            });
            for waker in finished_wakers.iter() {
                waker.wake();
            }
        });
        let control = Arc::new(ExecControl {
            task: task.abort_handle(),
            id,
            completion,
            eof_wakers,
        });
        Self {
            stdin: Some(ExecWriter {
                inner: stdin,
                control: control.clone(),
            }),
            stdout: Some(ExecReader {
                inner: stdout,
                control: control.clone(),
                stream: 0,
            }),
            stderr: Some(ExecReader {
                inner: stderr,
                control: control.clone(),
                stream: 1,
            }),
            control,
        }
    }
    #[cfg(test)]
    pub(crate) fn try_wait(&mut self) -> Result<Option<ExecStatus>, String> {
        self.control.completion.borrow().clone().transpose()
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
    stream: usize,
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
            // A stream can close before the driver publishes its outcome.
            // Register before checking the state, then completion/disconnect
            // wakes both readers without translating unknown into normal EOF.
            self.control.eof_wakers[self.stream].register(cx.waker());
            match &*self.control.completion.borrow() {
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error.clone()))),
                Some(Ok(_)) => {}
                None => return Poll::Pending,
            }
        }
        result
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_eof_waits_for_the_driver_outcome_instead_of_hiding_a_late_error() {
        let (finish, pending) = tokio::sync::oneshot::channel();
        let mut child = ExecChild::from_streams(
            &tokio::runtime::Handle::current(),
            "owned-task".into(),
            move |stdin, stdout, stderr| async move {
                drop((stdin, stdout, stderr));
                pending.await.unwrap();
                Err("owned process outcome unknown".into())
            },
        );
        let mut stdout = child.stdout.take().unwrap();
        let mut bytes = Vec::new();
        let read = stdout.read_to_end(&mut bytes);
        tokio::pin!(read);
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut read)
            .await
            .is_err());
        finish.send(()).unwrap();
        assert!(read.await.is_err());
    }

    fn fixture(
        bytes: Result<(Vec<u8>, Vec<u8>), String>,
        exit: Result<ExecStatus, String>,
    ) -> ExecChild {
        ExecChild::from_streams(
            &tokio::runtime::Handle::current(),
            "owned-task".into(),
            move |_stdin, mut stdout, mut stderr| async move {
                let (out, err) = bytes?;
                stdout.write_all(&out).await.map_err(|e| e.to_string())?;
                stderr.write_all(&err).await.map_err(|e| e.to_string())?;
                exit
            },
        )
    }

    #[tokio::test]
    async fn binary_streams_preserve_separation_and_nonzero_exit_beyond_pipe_capacity() {
        let bytes = vec![255u8; BUFFER_BYTES * 3];
        let mut child = fixture(
            Ok((bytes.clone(), b"diagnostic".to_vec())),
            Ok(ExecStatus::from(7)),
        );
        child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a.unwrap();
        b.unwrap();
        assert_eq!(out, bytes);
        assert_eq!(err, b"diagnostic");
        assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(7));
    }

    #[tokio::test]
    async fn stream_failure_and_unconfirmed_exit_never_become_successful_eof() {
        for mut child in [
            fixture(Err("transport lost".into()), Ok(ExecStatus::from(0))),
            fixture(Ok((vec![], vec![])), Err("exit unknown".into())),
        ] {
            let mut stdout = child.stdout.take().unwrap();
            assert!(stdout.read_to_end(&mut Vec::new()).await.is_err());
            assert!(child.try_wait().is_err());
        }
    }

    #[tokio::test]
    async fn disconnect_unblocks_readers_without_claiming_remote_cancellation() {
        let mut child = ExecChild::from_streams(
            &tokio::runtime::Handle::current(),
            "owned-task".into(),
            move |stdin, stdout, stderr| async move {
                let _streams = (stdin, stdout, stderr);
                std::future::pending::<Result<ExecStatus, String>>().await
            },
        );
        let mut stdout = child.stdout.take().unwrap();
        child.disconnect();
        assert!(child
            .try_wait()
            .unwrap_err()
            .contains("remote outcome unknown"));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), stdout.read_to_end(&mut Vec::new()))
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn stdin_eof_and_output_progress_concurrently_beyond_pipe_capacity() {
        let mut child = ExecChild::from_streams(
            &tokio::runtime::Handle::current(),
            "owned-task".into(),
            move |mut stdin, mut stdout, _stderr| async move {
                tokio::io::copy(&mut stdin, &mut stdout)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(ExecStatus::from(0))
            },
        );
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let payload = vec![42; BUFFER_BYTES * 3];
        let expected = payload.clone();
        let send = async move {
            stdin.write_all(&payload).await.unwrap();
            drop(stdin);
        };
        let receive = async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.unwrap();
            bytes
        };
        let (_, bytes) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(send, receive)
        })
        .await
        .unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(0));
    }
}
