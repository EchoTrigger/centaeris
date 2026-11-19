//! Decode local video on a worker and keep only a short queue of terminal frames.

use centaeris_agent::local_execution_host::capture_application_command;
use image::{DynamicImage, RgbImage, Rgba};
use ratatui::layout::Rect;
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::{kitty::StatefulKitty, ImageSource, StatefulProtocol, StatefulProtocolType},
    FilterType, Resize, ResizeEncodeRender,
};
use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock, PoisonError},
    thread,
    time::{Duration, Instant},
};

const FPS: u64 = 10;
const FRAME_INTERVAL: Duration = Duration::from_millis(1_000 / FPS);
const DECODE_EDGE: u32 = 640;
const FRAME_QUEUE: usize = 2;
const MAX_RENDER_PIXELS: u32 = 4_000_000;
const WAIT_FRAME: Duration = Duration::from_secs(8);
const WAIT_EXIT: Duration = Duration::from_secs(1);
const INPUT_FORMATS: &str = "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,avi,mpeg,mpegts,ogg,flv";

struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

fn tools() -> Result<&'static Tools, String> {
    static TOOLS: OnceLock<Option<Tools>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            let path = env::var_os("PATH")?;
            let find = |name: &str| {
                let executable = format!("{name}{}", env::consts::EXE_SUFFIX);
                env::split_paths(&path)
                    .filter(|directory| directory.is_absolute())
                    .map(|directory| directory.join(&executable))
                    .find(|candidate| candidate.is_file())
            };
            Some(Tools {
                ffmpeg: find("ffmpeg")?,
                ffprobe: find("ffprobe")?,
            })
        })
        .as_ref()
        .ok_or_else(|| "FFmpeg and ffprobe are needed for inline video".to_string())
}

pub(super) fn available() -> bool {
    tools().is_ok()
}

pub(super) struct Probe {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) duration: Option<Duration>,
    pub(super) poster: DynamicImage,
}

/// Call from the media loading worker. Both metadata and the first frame have a deadline.
pub(super) fn probe(path: PathBuf) -> Result<Probe, String> {
    validate_file(&path)?;
    let mut inspect = Command::new(&tools()?.ffprobe);
    input_limits(&mut inspect);
    inspect
        .args([
            "-select_streams",
            "V:0",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            "-i",
        ])
        .arg(&path);
    let (capture, truncated) = capture_application_command(&mut inspect, &[], 5_000, 1024)?;
    if capture.timed_out || capture.exit_code != Some(0) || truncated {
        return Err("could not read video metadata".into());
    }
    let duration = capture
        .stdout
        .trim()
        .parse::<f64>()
        .ok()
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok());
    let stream = decode_stream(path, true, Ok)?;
    let poster = stream
        .frames
        .recv_timeout(WAIT_FRAME)
        .map_err(|error| format!("could not load video poster: {error}"))??;
    Ok(Probe {
        width: poster.width(),
        height: poster.height(),
        duration,
        poster,
    })
}

pub(super) enum PlaybackEvent {
    Frame {
        protocol: StatefulProtocol,
        elapsed: Duration,
    },
    Finished,
    Failed(String),
}

pub(super) struct Playback {
    stream: DecodeStream<StatefulProtocol>,
    kitty_image_id: Option<u32>,
    next_frame_at: Instant,
    elapsed: Duration,
    finished: bool,
}

pub(super) fn start(path: PathBuf, picker: Picker, area: Rect) -> Result<Playback, String> {
    let (cell_width, cell_height) = picker.font_size();
    let pixel_width = u32::from(area.width) * u32::from(cell_width);
    let pixel_height = u32::from(area.height) * u32::from(cell_height);
    if pixel_width == 0
        || pixel_height == 0
        || pixel_width > u32::from(u16::MAX)
        || pixel_height > u32::from(u16::MAX)
        || pixel_width.saturating_mul(pixel_height) > MAX_RENDER_PIXELS
    {
        return Err("video display area is out of range".into());
    }
    let kitty_image_id = if picker.protocol_type() == ProtocolType::Kitty {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes).map_err(|error| format!("allocate video image ID: {error}"))?;
        Some(u32::from_ne_bytes(bytes).max(1))
    } else {
        None
    };
    let stream = decode_stream(path, false, move |image| {
        // Scrollback graphics are disabled under tmux. Reuse the same Kitty ID
        // for every frame so advancing does not accumulate terminal images.
        let mut protocol = if let Some(id) = kitty_image_id {
            let font_size = picker.font_size();
            let source = ImageSource::new(image, font_size, Rgba([0, 0, 0, 0]));
            StatefulProtocol::new(
                source,
                font_size,
                StatefulProtocolType::Kitty(StatefulKitty::new(id, false)),
            )
        } else {
            picker.new_resize_protocol(image)
        };
        protocol.resize_encode(&Resize::Scale(Some(FilterType::Triangle)), area);
        match protocol.last_encoding_result() {
            Some(Ok(())) => Ok(protocol),
            Some(Err(error)) => Err(format!("prepare video frame: {error}")),
            None => Err("video frame encoder produced no result".into()),
        }
    })?;
    Ok(Playback {
        stream,
        kitty_image_id,
        next_frame_at: Instant::now(),
        elapsed: Duration::ZERO,
        finished: false,
    })
}

impl Playback {
    pub(super) fn kitty_image_id(&self) -> Option<u32> {
        self.kitty_image_id
    }

    pub(super) fn poll(&mut self) -> Option<PlaybackEvent> {
        let now = Instant::now();
        if self.finished || now < self.next_frame_at {
            return None;
        }
        let result = match self.stream.frames.try_recv() {
            Ok(Ok(protocol)) => {
                let elapsed = self.elapsed;
                self.elapsed += FRAME_INTERVAL;
                self.next_frame_at = now + FRAME_INTERVAL;
                return Some(PlaybackEvent::Frame { protocol, elapsed });
            }
            Ok(Err(error)) => PlaybackEvent::Failed(error),
            Err(mpsc::TryRecvError::Disconnected) => PlaybackEvent::Finished,
            Err(mpsc::TryRecvError::Empty) if now - self.next_frame_at >= WAIT_FRAME => {
                self.stream.cancel();
                PlaybackEvent::Failed("video decoder stopped responding".into())
            }
            Err(mpsc::TryRecvError::Empty) => return None,
        };
        self.finished = true;
        Some(result)
    }
}

#[derive(Default)]
struct Process {
    cancelled: bool,
    child: Option<Child>,
}

fn process_lock(process: &Mutex<Process>) -> MutexGuard<'_, Process> {
    process.lock().unwrap_or_else(PoisonError::into_inner)
}

struct DecodeStream<T> {
    frames: mpsc::Receiver<Result<T, String>>,
    process: Arc<Mutex<Process>>,
}

impl<T> DecodeStream<T> {
    fn cancel(&self) {
        let mut process = process_lock(&self.process);
        process.cancelled = true;
        if let Some(child) = &mut process.child {
            let _ = child.kill();
        }
    }
}

impl<T> Drop for DecodeStream<T> {
    fn drop(&mut self) {
        // Killing the child unblocks pending pipe reads. Dropping the receiver
        // next unblocks a worker waiting on the full frame queue.
        self.cancel();
    }
}

fn decode_stream<T: Send + 'static>(
    path: PathBuf,
    poster_only: bool,
    mut prepare: impl FnMut(DynamicImage) -> Result<T, String> + Send + 'static,
) -> Result<DecodeStream<T>, String> {
    let (send, frames) = mpsc::sync_channel(FRAME_QUEUE);
    let process = Arc::new(Mutex::new(Process::default()));
    let worker_process = Arc::clone(&process);
    thread::Builder::new()
        .name("centaeris-tui-video".into())
        .spawn(move || {
            let result = decode(&path, poster_only, &worker_process, |image| {
                let frame = prepare(image)?;
                Ok(send.send(Ok(frame)).is_ok())
            });
            if let Err(error) = result {
                let _ = send.send(Err(error));
            }
        })
        .map_err(|error| format!("start video worker: {error}"))?;
    Ok(DecodeStream { frames, process })
}

fn validate_file(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if !matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(prefix)) if matches!(
            prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
        )
    ) {
        return Err("video input must be on a local drive".into());
    }
    if !path.is_absolute() || !fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err("video input must be an existing local file".into());
    }
    Ok(())
}

fn input_limits(command: &mut Command) {
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-max_alloc",
        "134217728",
        "-max_pixels",
        "33554432",
        "-protocol_whitelist",
        "file,pipe",
        "-format_whitelist",
        INPUT_FORMATS,
    ]);
}

fn decode(
    path: &Path,
    poster_only: bool,
    process: &Mutex<Process>,
    mut emit: impl FnMut(DynamicImage) -> Result<bool, String>,
) -> Result<(), String> {
    validate_file(path)?;
    let mut command = Command::new(&tools()?.ffmpeg);
    input_limits(&mut command);
    command
        .args(["-nostdin", "-threads", "1", "-filter_threads", "1", "-i"])
        .arg(path)
        .args(["-map", "0:V:0", "-an", "-sn", "-dn", "-vf"]);
    let scale = format!(
        "scale=w='min(iw,{DECODE_EDGE})':h='min(ih,{DECODE_EDGE})':force_original_aspect_ratio=decrease"
    );
    if poster_only {
        command.arg(scale).args(["-frames:v", "1"]);
    } else {
        command.arg(format!("fps={FPS},{scale}"));
    }
    command
        .args([
            "-c:v",
            "ppm",
            "-pix_fmt",
            "rgb24",
            "-f",
            "image2pipe",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    if process_lock(process).cancelled {
        return Ok(());
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("start FFmpeg: {error}"))?;
    let stdout = child.stdout.take();
    {
        let mut state = process_lock(process);
        if state.cancelled {
            drop(state);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        state.child = Some(child);
    }
    let _reap = Reap(process);
    let stdout = stdout.ok_or("FFmpeg stdout is unavailable")?;
    let mut frames = BufReader::new(stdout);
    let mut decoded_frame = false;
    while let Some(frame) = read_frame(&mut frames)? {
        decoded_frame = true;
        if !emit(frame)? {
            return Ok(());
        }
    }
    wait_for_ffmpeg(process)?;
    if !decoded_frame && !process_lock(process).cancelled {
        return Err("video contains no decodable frames".into());
    }
    Ok(())
}

// Reap on the worker, including error and cancellation paths. The UI only
// sends the termination signal and never waits for the process or the thread.
struct Reap<'a>(&'a Mutex<Process>);

impl Drop for Reap<'_> {
    fn drop(&mut self) {
        let child = process_lock(self.0).child.take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for_ffmpeg(process: &Mutex<Process>) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_EXIT;
    loop {
        {
            let mut process = process_lock(process);
            if process.cancelled {
                return Ok(());
            }
            let child = process
                .child
                .as_mut()
                .ok_or("FFmpeg child is unavailable")?;
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("wait for FFmpeg: {error}"))?
            {
                process.child = None;
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("video decoding failed ({status})"))
                };
            }
        }
        if Instant::now() >= deadline {
            return Err("FFmpeg did not exit after its output closed".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_frame(reader: &mut impl BufRead) -> Result<Option<DynamicImage>, String> {
    let mut line = String::new();
    let read_line = |reader: &mut dyn BufRead, line: &mut String| {
        line.clear();
        reader
            .take(128)
            .read_line(line)
            .map_err(|error| format!("read video frame header: {error}"))
    };
    if read_line(reader, &mut line)? == 0 {
        return Ok(None);
    }
    if line.trim_end() != "P6" {
        return Err("invalid video frame format".into());
    }
    read_line(reader, &mut line)?;
    let (width, height) = line
        .trim_end()
        .split_once(' ')
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)))
        .filter(|&(width, height)| {
            (1..=DECODE_EDGE).contains(&width) && (1..=DECODE_EDGE).contains(&height)
        })
        .ok_or("invalid video frame dimensions")?;
    read_line(reader, &mut line)?;
    if line.trim_end() != "255" || !line.ends_with('\n') {
        return Err("invalid video frame color depth".into());
    }
    let mut rgb = vec![0; (width * height * 3) as usize];
    reader
        .read_exact(&mut rgb)
        .map_err(|error| format!("read decoded video frame: {error}"))?;
    let image = RgbImage::from_raw(width, height, rgb).ok_or("invalid decoded video frame")?;
    Ok(Some(DynamicImage::ImageRgb8(image)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_reader_preserves_boundaries_and_rejects_oversized_or_truncated_frames() {
        let mut stream = &b"P6\n1 1\n255\n\x00\x0a\xffP6\n1 1\n255\n\x01\x02\x03"[..];
        let first = read_frame(&mut stream).unwrap().unwrap();
        assert_eq!(first.as_bytes(), &[0, 10, 255]);
        let second = read_frame(&mut stream).unwrap().unwrap();
        assert_eq!(second.as_bytes(), &[1, 2, 3]);
        assert!(read_frame(&mut stream).unwrap().is_none());
        assert!(read_frame(&mut &b"P6\n641 1\n255\n"[..]).is_err());
        assert!(read_frame(&mut &b"P6\n1 1\n255\n\x01\x02"[..]).is_err());
    }

    #[test]
    #[ignore = "requires system FFmpeg and ffprobe"]
    fn real_video_reaches_end_replays_and_reaps_after_cancellation() {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).unwrap();
        let id = u64::from_ne_bytes(bytes);
        let path = env::temp_dir().join(format!("centaeris-inline-video-{id}.mp4"));
        struct Fixture<'a>(&'a Path);
        impl Drop for Fixture<'_> {
            fn drop(&mut self) {
                let _ = fs::remove_file(self.0);
            }
        }
        let _fixture = Fixture(&path);
        let mut command = Command::new(&tools().unwrap().ffmpeg);
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=12",
            "-t",
            "0.5",
            "-an",
            "-c:v",
            "mpeg4",
        ]);
        command.arg(&path);
        let (output, _) = capture_application_command(&mut command, &[], 5_000, 1024).unwrap();
        assert_eq!(output.exit_code, Some(0), "{}", output.stderr);

        let poster = probe(path.clone()).unwrap();
        assert_eq!((poster.width, poster.height), (640, 360));
        assert_eq!(poster.duration, Some(Duration::from_millis(500)));

        let mut picker = Picker::from_fontsize((8, 16));
        picker.set_protocol_type(ProtocolType::Kitty);
        let area = Rect::new(0, 0, 20, 5);
        let mut playback = start(path.clone(), picker.clone(), area).unwrap();
        let deadline = Instant::now() + WAIT_FRAME;
        let mut frames = 0;
        loop {
            assert!(Instant::now() < deadline, "playback did not finish");
            match playback.poll() {
                Some(PlaybackEvent::Frame { protocol, elapsed }) => {
                    assert_eq!(elapsed, FRAME_INTERVAL * frames);
                    match protocol.protocol_type() {
                        StatefulProtocolType::Kitty(kitty) => {
                            assert_eq!(Some(kitty.unique_id), playback.kitty_image_id());
                        }
                        _ => panic!("expected Kitty"),
                    }
                    frames += 1;
                }
                Some(PlaybackEvent::Finished) => break,
                Some(PlaybackEvent::Failed(error)) => panic!("{error}"),
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(frames, 5);
        assert!(playback.poll().is_none());

        let mut replay = start(path.clone(), picker, area).unwrap();
        let deadline = Instant::now() + WAIT_FRAME;
        loop {
            assert!(Instant::now() < deadline, "replay returned no frame");
            match replay.poll() {
                Some(PlaybackEvent::Frame { elapsed, .. }) => {
                    assert_eq!(elapsed, Duration::ZERO);
                    break;
                }
                Some(PlaybackEvent::Failed(error)) => panic!("{error}"),
                Some(PlaybackEvent::Finished) => panic!("replay ended before its first frame"),
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        let process = Arc::clone(&replay.stream.process);
        drop(replay);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Arc::strong_count(&process) > 1 {
            assert!(
                Instant::now() < deadline,
                "decoder worker did not shut down"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(process_lock(&process).child.is_none());
    }
}
