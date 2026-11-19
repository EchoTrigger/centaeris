use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommandCapture {
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
}

pub(crate) fn configure_background_command(_command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
}

pub(crate) fn run_command_capture(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandCapture, String> {
    let mut command = Command::new(program);
    configure_background_command(&mut command);
    let mut child = command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn {program} failed: {error}"))?;
    let stdout_reader = child.stdout.take().map(|mut stdout| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .map_err(|error| format!("read stdout failed: {error}"))
        })
    });
    let stderr_reader = child.stderr.take().map(|mut stderr| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .map_err(|error| format!("read stderr failed: {error}"))
        })
    });
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stderr) =
                    join_command_capture_readers(stdout_reader, stderr_reader, program)?;
                return Ok(CommandCapture {
                    exit_code: status.code(),
                    stdout: decode_command_output(stdout.as_slice()),
                    stderr: decode_command_output(stderr.as_slice()),
                    timed_out: false,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let (stdout, stderr) =
                        join_command_capture_readers(stdout_reader, stderr_reader, program)?;
                    return Ok(CommandCapture {
                        exit_code: None,
                        stdout: decode_command_output(stdout.as_slice()),
                        stderr: decode_command_output(stderr.as_slice()),
                        timed_out: true,
                    });
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("poll {program} failed: {error}")),
        }
    }
}

fn join_command_capture_readers(
    stdout_reader: Option<thread::JoinHandle<Result<Vec<u8>, String>>>,
    stderr_reader: Option<thread::JoinHandle<Result<Vec<u8>, String>>>,
    program: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let stdout = match stdout_reader {
        Some(handle) => handle
            .join()
            .map_err(|_| format!("collect {program} stdout panicked"))??,
        None => Vec::new(),
    };
    let stderr = match stderr_reader {
        Some(handle) => handle
            .join()
            .map_err(|_| format!("collect {program} stderr panicked"))??,
        None => Vec::new(),
    };
    Ok((stdout, stderr))
}

pub(crate) fn decode_command_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.len() >= 2 {
        let zero_odd = bytes
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|byte| **byte == 0)
            .count();
        let odd_count = bytes.len() / 2;
        if odd_count > 0 && zero_odd.saturating_mul(2) >= odd_count {
            let units = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>();
            return String::from_utf16_lossy(units.as_slice())
                .replace('\0', "")
                .trim()
                .to_string();
        }
    }
    String::from_utf8_lossy(bytes)
        .replace('\0', "")
        .trim()
        .to_string()
}

pub(crate) fn command_capture_tail(text: &str, max_chars: usize) -> String {
    let normalized = text.trim();
    if normalized.chars().count() <= max_chars {
        return normalized.to_string();
    }
    let tail = normalized
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{}", tail.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_capture_tail_keeps_short_text() {
        assert_eq!(command_capture_tail("  short output  ", 20), "short output");
    }

    #[test]
    fn command_capture_tail_truncates_by_characters() {
        assert_eq!(
            command_capture_tail("one two three four", 10),
            "...three four"
        );
        assert_eq!(command_capture_tail("alpha beta gamma", 5), "...gamma");
    }

    #[test]
    fn decode_command_output_reads_utf8() {
        assert_eq!(decode_command_output(b" hello \n"), "hello");
    }

    #[test]
    fn decode_command_output_reads_utf16_le() {
        let bytes = "windows output\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();

        assert_eq!(decode_command_output(bytes.as_slice()), "windows output");
    }
}
