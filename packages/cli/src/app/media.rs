use super::*;
use std::{borrow::Cow, io::Read, path::Path};

#[derive(Clone)]
pub(super) enum Source {
    Image(ToolImage),
    Video(String),
}

impl Source {
    fn path(&self) -> &str {
        match self {
            Self::Image(image) => &image.path,
            Self::Video(path) => path,
        }
    }
}

pub(super) struct Placement {
    pub(super) key: String,
    pub(super) source: Source,
    pub(super) row: u64,
    pub(super) controls: u64,
    pub(super) size: Rect,
}

#[derive(Clone, Copy)]
enum Action {
    Open,
    Copy,
    Play,
}

#[derive(Clone)]
struct Hit {
    area: Rect,
    key: String,
    source: Source,
    action: Action,
}

struct Poster {
    size: Rect,
    width: u32,
    height: u32,
    path: PathBuf,
    duration: Option<Duration>,
    protocol: StatefulProtocol,
}

struct Video {
    key: String,
    size: Rect,
    playback: video_decode::Playback,
    frame: Option<StatefulProtocol>,
    elapsed: Duration,
}

type PosterResult = (String, bool, Result<Poster, String>);
type Pending<T> = (u64, mpsc::Receiver<T>);

enum OpenResult {
    Native(Option<TemporaryImage>),
    Image(PathBuf, image::DynamicImage),
    Copy(image::RgbaImage),
}

struct TemporaryImage(PathBuf);

impl Drop for TemporaryImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Default)]
pub(super) struct State {
    generation: u64,
    hits: Vec<Hit>,
    active_visible: bool,
    active: Option<Video>,
    posters: HashMap<String, Result<Poster, String>>,
    probe: Option<Pending<PosterResult>>,
    action: Option<Pending<Result<OpenResult, String>>>,
    decoder_available: Option<bool>,
    clear_ids: Vec<u32>,
    clipboard: Option<arboard::Clipboard>,
    external_images: VecDeque<TemporaryImage>,
}

impl State {
    pub(super) fn take_clear_ids(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.clear_ids)
    }

    fn stop(&mut self) -> bool {
        let Some(active) = self.active.take() else {
            return false;
        };
        if let Some(id) = active.playback.kitty_image_id() {
            self.clear_ids.push(id);
        }
        true
    }

    pub(super) fn clear_hits(&mut self) {
        self.hits.clear();
        self.active_visible = false;
    }
}

pub(super) fn stop(app: &mut App) -> bool {
    app.media.stop()
}

pub(super) fn stop_if_hidden(app: &mut App) -> bool {
    !app.media.active_visible && app.media.stop()
}

pub(super) fn video_size(app: &App, key: &str, width: u16) -> Rect {
    if !app.image_policy.scrollback || app.media.decoder_available == Some(false) {
        return Rect::default();
    }
    let dimensions = app
        .media
        .posters
        .get(key)
        .and_then(|result| result.as_ref().ok())
        .map_or((640, 360), |poster| (poster.width, poster.height));
    fitted_video_size(dimensions.0, dimensions.1, &app.image_policy.picker, width)
}

fn fitted_video_size(width: u32, height: u32, picker: &Picker, columns: u16) -> Rect {
    fit_image_preview_size(
        width,
        height,
        picker.font_size(),
        Rect::new(0, 0, columns.saturating_sub(3), (columns / 2).clamp(4, 20)),
    )
}

pub(super) fn controls_line(source: &Source, size: Rect, width: u16) -> Line<'static> {
    let label = match (source, size.height > 0) {
        (Source::Image(_), false) => "[Open Image]",
        (Source::Video(_), false) => "[Open Video]",
        _ => return Line::default(),
    };
    let path = fit_middle_columns(
        source.path(),
        usize::from(width).saturating_sub(label.len() + 3),
    );
    Line::styled(
        format!("{path}  {label}"),
        Style::default().fg(theme().muted),
    )
}

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App, view: &TranscriptView) {
    if app.session.image_preview.is_some() {
        return;
    }
    let scroll = app.session.transcript_scroll;
    let mut active_visible = false;
    for placement in &view.media {
        let rich = placement.size.height > 0;
        let rect = placement.row.checked_sub(scroll).and_then(|offset| {
            (offset + u64::from(placement.size.height) <= u64::from(area.height)).then(|| {
                Rect::new(
                    area.x + area.width.saturating_sub(placement.size.width) / 2,
                    area.y + offset as u16,
                    placement.size.width,
                    placement.size.height,
                )
            })
        });
        let video = matches!(placement.source, Source::Video(_));
        if let Some(rect) = rect.filter(|_| video && rich) {
            if let Some(active) = app
                .media
                .active
                .as_mut()
                .filter(|v| v.key == placement.key && v.size == placement.size)
            {
                active_visible = true;
                if let Some(protocol) = active.frame.as_mut() {
                    protocol.render(rect, frame.buffer_mut());
                } else {
                    render_poster(frame, &mut app.media.posters, placement, rect);
                }
            } else {
                render_poster(frame, &mut app.media.posters, placement, rect);
                if app
                    .media
                    .posters
                    .get(&placement.key)
                    .is_some_and(|poster| poster.is_ok())
                {
                    app.media.hits.push(Hit {
                        area: rect,
                        key: placement.key.clone(),
                        source: placement.source.clone(),
                        action: Action::Play,
                    });
                }
            }
        }
        if !video {
            if let Some(rect) = rect.filter(|_| rich) {
                app.media.hits.push(Hit {
                    area: rect,
                    key: placement.key.clone(),
                    source: placement.source.clone(),
                    action: Action::Open,
                });
            }
        }
        let Some(row) = placement
            .controls
            .checked_sub(scroll)
            .filter(|y| *y < u64::from(area.height))
        else {
            continue;
        };
        let row = area.y + row as u16;
        if !rich {
            let label = if video {
                "[Open Video]"
            } else {
                "[Open Image]"
            };
            let width = usize::from(area.width).saturating_sub(label.len() + 2);
            let path = fit_middle_columns(placement.source.path(), width);
            let x = area.x + Line::raw(path).width() as u16 + 2;
            hit_button(
                frame,
                &mut app.media.hits,
                placement,
                Action::Open,
                label,
                Position::new(x, row),
                area.right(),
            );
            continue;
        }
        let progress = app
            .media
            .active
            .as_ref()
            .filter(|v| v.key == placement.key)
            .map(|v| {
                let elapsed = v.elapsed.as_secs();
                let total = app
                    .media
                    .posters
                    .get(&placement.key)
                    .and_then(|p| p.as_ref().ok())
                    .and_then(|p| p.duration)
                    .map(|d| d.as_secs());
                total.map_or_else(
                    || format!("{}:{:02}", elapsed / 60, elapsed % 60),
                    |total| {
                        format!(
                            "{}:{:02} / {}:{:02}",
                            elapsed / 60,
                            elapsed % 60,
                            total / 60,
                            total % 60
                        )
                    },
                )
            });
        let first = if video {
            progress.as_deref().unwrap_or("[Play]")
        } else {
            "[Open]"
        };
        let second = if video { "[Open]" } else { "[Copy]" };
        let width = (first.len() + second.len() + 3) as u16;
        let x = area.x + area.width.saturating_sub(width) / 2;
        if progress.is_some() {
            frame.render_widget(
                Paragraph::new(first).style(Style::default().fg(theme().muted)),
                Rect::new(x, row, first.len() as u16, 1),
            );
        } else {
            hit_button(
                frame,
                &mut app.media.hits,
                placement,
                if video { Action::Play } else { Action::Open },
                first,
                Position::new(x, row),
                area.right(),
            );
        }
        hit_button(
            frame,
            &mut app.media.hits,
            placement,
            if video { Action::Open } else { Action::Copy },
            second,
            Position::new(x + first.len() as u16 + 3, row),
            area.right(),
        );
    }
    app.media.active_visible = active_visible;
}

fn render_poster(
    frame: &mut Frame,
    posters: &mut HashMap<String, Result<Poster, String>>,
    placement: &Placement,
    area: Rect,
) {
    match posters.get_mut(&placement.key) {
        Some(Ok(poster)) if poster.size == placement.size => {
            poster.protocol.render(area, frame.buffer_mut())
        }
        Some(Err(_)) => frame.render_widget(
            Paragraph::new("Video preview unavailable").style(Style::default().fg(theme().muted)),
            area,
        ),
        _ => frame.render_widget(
            Paragraph::new("Loading video…").style(Style::default().fg(theme().muted)),
            area,
        ),
    }
}

fn hit_button(
    frame: &mut Frame,
    hits: &mut Vec<Hit>,
    placement: &Placement,
    action: Action,
    label: &str,
    position: Position,
    right: u16,
) {
    let width = label.len() as u16;
    if position.x.saturating_add(width) <= right {
        let area = Rect::new(position.x, position.y, width, 1);
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(theme().muted)),
            area,
        );
        hits.push(Hit {
            area,
            key: placement.key.clone(),
            source: placement.source.clone(),
            action,
        });
    }
}

pub(super) fn prepare_visible(app: &mut App, view: &TranscriptView, height: u16) {
    if app.media.probe.is_some() {
        return;
    }
    let scroll = app.session.transcript_scroll;
    let Some(placement) = view.media.iter().find(|placement| {
        matches!(placement.source, Source::Video(_))
            && placement.size.height > 0
            && placement.row < scroll + u64::from(height)
            && placement.controls >= scroll
            && !app
                .media
                .posters
                .get(&placement.key)
                .is_some_and(|p| p.as_ref().map_or(true, |p| p.size == placement.size))
    }) else {
        return;
    };
    let key = placement.key.clone();
    let source = placement.source.path().to_owned();
    let root = app.session.workspace_root.clone();
    let width = app.render_width;
    let picker = app.image_policy.picker.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let available = video_decode::available();
        let result = if available {
            prepare_poster(&root, &source, picker, width)
        } else {
            Err("FFmpeg is unavailable".into())
        };
        let _ = tx.send((key, available, result));
    });
    app.media.probe = Some((app.foreground_generation, rx));
}

fn prepare_poster(
    root: &str,
    source: &str,
    picker: Picker,
    columns: u16,
) -> Result<Poster, String> {
    let path = workspace_file(root, source, VIDEO_EXTENSIONS)?;
    let probe = video_decode::probe(path.clone())?;
    let size = fitted_video_size(probe.width, probe.height, &picker, columns);
    let mut protocol = picker.new_resize_protocol(probe.poster);
    protocol.resize_encode(&Resize::Scale(Some(FilterType::Triangle)), size);
    match protocol.last_encoding_result() {
        Some(Ok(())) => Ok(Poster {
            size,
            width: probe.width,
            height: probe.height,
            path,
            duration: probe.duration,
            protocol,
        }),
        Some(Err(error)) => Err(format!("encode video poster failed: {error}")),
        None => Err("video poster cannot fit".into()),
    }
}

pub(super) fn click(app: &mut App, position: Position) -> bool {
    let Some(hit) = app
        .media
        .hits
        .iter()
        .find(|hit| hit.area.contains(position))
        .cloned()
    else {
        return false;
    };
    if matches!(hit.action, Action::Play) {
        let result = match app.media.posters.get(&hit.key) {
            Some(Ok(poster)) => video_decode::start(
                poster.path.clone(),
                app.image_policy.picker.clone(),
                poster.size,
            )
            .map(|playback| Video {
                key: hit.key,
                size: poster.size,
                playback,
                frame: None,
                elapsed: Duration::ZERO,
            }),
            Some(Err(error)) => Err(error.clone()),
            None => Err("Loading video…".into()),
        };
        match result {
            Ok(active) => {
                app.media.stop();
                app.media.active = Some(active);
            }
            Err(error) => show_message(app, error),
        }
        return true;
    }
    if app.media.action.is_some() {
        return true;
    }
    let root = app.session.workspace_root.clone();
    let pixel_preview = matches!(
        app.image_policy.picker.protocol_type(),
        ProtocolType::Kitty | ProtocolType::Iterm2
    );
    let retained = match (&hit.source, &app.runtime, &app.session.active_session) {
        (Source::Image(image), Some(runtime), Some(session)) => runtime
            .retained_execution_image_async(session.id.clone(), image.sha256.clone())
            .ok(),
        _ => None,
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(open_or_copy(hit, &root, pixel_preview, retained));
    });
    app.media.action = Some((app.foreground_generation, rx));
    true
}

fn open_or_copy(
    hit: Hit,
    root: &str,
    pixel_preview: bool,
    retained: Option<RetainedImageResponse>,
) -> Result<OpenResult, String> {
    match hit.source {
        Source::Image(image) if pixel_preview || matches!(hit.action, Action::Copy) => {
            let bytes = read_retained(retained);
            let bytes = match bytes {
                Some(bytes) => bytes,
                None => {
                    let path = workspace_file(root, &image.path, IMAGE_EXTENSIONS)?;
                    read_workspace_image(&path, &image)?
                }
            };
            let decoded = decode_tool_image(&bytes, &image)?;
            if matches!(hit.action, Action::Copy) {
                Ok(OpenResult::Copy(decoded.into_rgba8()))
            } else {
                Ok(OpenResult::Image(PathBuf::from(image.path), decoded))
            }
        }
        Source::Image(image) => {
            if let Ok(path) = workspace_file(root, &image.path, IMAGE_EXTENSIONS) {
                if let Ok(bytes) = read_workspace_image(&path, &image) {
                    if bytes.len() as u64 == image.byte_length
                        && format!("sha256:{:x}", Sha256::digest(&bytes)) == image.sha256
                    {
                        open_native(&path)?;
                        return Ok(OpenResult::Native(None));
                    }
                }
            }
            let bytes = read_retained(retained).ok_or("Image file is no longer available")?;
            let decoded = decode_tool_image(&bytes, &image)?;
            let file = TemporaryImage(crate::clipboard::write_temp_png(decoded)?);
            open_native(&file.0)?;
            Ok(OpenResult::Native(Some(file)))
        }
        Source::Video(path) => {
            open_native(&workspace_file(root, &path, VIDEO_EXTENSIONS)?)?;
            Ok(OpenResult::Native(None))
        }
    }
}

fn read_retained(retained: Option<RetainedImageResponse>) -> Option<Vec<u8>> {
    retained?.recv_timeout(Duration::from_secs(5)).ok()?.ok()?
}

fn read_workspace_image(path: &Path, image: &ToolImage) -> Result<Vec<u8>, String> {
    let limit = image.byte_length.min(TOOL_IMAGE_MAX_DECODED_BYTES);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("read media failed: {error}"))?;
    Ok(bytes)
}

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "tif", "ico",
];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "webm", "mov", "mkv"];

fn workspace_file(root: &str, candidate: &str, extensions: &[&str]) -> Result<PathBuf, String> {
    if candidate.starts_with("\\\\") || candidate.starts_with("//") || candidate.contains("://") {
        return Err("Media path must be local".into());
    }
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("resolve media workspace failed: {error}"))?;
    let path = std::fs::canonicalize(root.join(candidate))
        .map_err(|error| format!("resolve media file failed: {error}"))?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err("Media file is outside the session workspace".into());
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(std::path::Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err("Media path must be on a local drive".into());
    }
    if !path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            extensions
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
    {
        return Err("Media file has an unsupported format".into());
    }
    Ok(path)
}

#[cfg(windows)]
fn open_native(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let original = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let prefix = ['\\' as u16, '\\' as u16, '?' as u16, '\\' as u16];
    let path = original
        .strip_prefix(&prefix)
        .unwrap_or(&original)
        .iter()
        .copied()
        .chain([0])
        .collect::<Vec<_>>();
    // The wide filename remains valid for this synchronous ShellExecuteW call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err(format!("Open media failed ({})", result as isize));
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_native(path: &Path) -> Result<(), String> {
    use std::process::Command;
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(command)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("open media failed: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open media failed: {status}"))
    }
}

fn take_ready<T>(pending: &mut Option<Pending<T>>, generation: u64) -> Option<T> {
    let (owner, rx) = pending.as_ref()?;
    if *owner == generation {
        match rx.try_recv() {
            Ok(value) => {
                *pending = None;
                return Some(value);
            }
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }
    *pending = None;
    None
}

pub(super) fn poll(app: &mut App) -> bool {
    let generation = app.foreground_generation;
    let mut changed = false;
    if app.media.generation != generation {
        app.media.generation = generation;
        app.media.posters.clear();
        changed |= app.media.stop();
    }
    if let Some((key, available, result)) = take_ready(&mut app.media.probe, generation) {
        if app.media.decoder_available != Some(available) {
            app.media.decoder_available = Some(available);
        }
        if app.media.posters.len() >= 8 && !app.media.posters.contains_key(&key) {
            app.media.posters.clear();
        }
        app.media.posters.insert(key, result);
        invalidate_transcript_layout(&mut app.session);
        changed = true;
    }
    if let Some(result) = take_ready(&mut app.media.action, generation) {
        if let Err(error) = finish_action(app, result) {
            show_message(app, error);
        }
        changed = true;
    }
    if let Some(event) = app.media.active.as_mut().and_then(|v| v.playback.poll()) {
        match event {
            video_decode::PlaybackEvent::Frame { protocol, elapsed } => {
                if let Some(active) = app.media.active.as_mut() {
                    active.frame = Some(protocol);
                    active.elapsed = elapsed;
                }
            }
            video_decode::PlaybackEvent::Finished => {
                app.media.stop();
            }
            video_decode::PlaybackEvent::Failed(error) => {
                app.media.stop();
                show_message(app, error);
            }
        }
        changed = true;
    }
    changed
}

fn finish_action(app: &mut App, result: Result<OpenResult, String>) -> Result<(), String> {
    match result? {
        OpenResult::Native(None) => {}
        OpenResult::Native(Some(file)) => {
            if app.media.external_images.len() >= 4 {
                app.media.external_images.pop_front();
            }
            app.media.external_images.push_back(file);
        }
        OpenResult::Image(path, decoded) => {
            if app.transcript_area.is_none()
                || dashboard::visible(app)
                || settings::visible(app)
                || app.session_picker.is_some()
                || app.model_panel.is_some()
                || app.session.subagents.detail_open()
                || app.session.image_preview.is_some()
            {
                return Ok(());
            }
            let viewer =
                start_pixel_image_preview(path, app.image_policy.picker.clone(), Some(decoded))?;
            app.session.image_preview = Some(ImagePreview::Pixels(Box::new(viewer)));
            app.image_preview_area = None;
            app.session.transcript_selection = None;
            app.mouse_drag = None;
        }
        OpenResult::Copy(image) => {
            if app.media.clipboard.is_none() {
                app.media.clipboard =
                    Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
            }
            let width = image.width() as usize;
            let height = image.height() as usize;
            app.media
                .clipboard
                .as_mut()
                .expect("image clipboard initialized")
                .set_image(arboard::ImageData {
                    width,
                    height,
                    bytes: Cow::Owned(image.into_raw()),
                })
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
