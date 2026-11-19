use super::*;

const MAX_ROWS: u16 = 20;

pub(super) fn size(app: &App, image: &ToolImage, width: u16) -> Rect {
    let width = width.saturating_sub(3); // One inner column on each side and the scrollbar.
    if !app.image_policy.scrollback || width == 0 || image.width_px == 0 || image.height_px == 0 {
        return Rect::default();
    }
    let (cell_width, cell_height) = app.image_policy.picker.font_size();
    let columns_per_row = f64::from(image.width_px) * f64::from(cell_height)
        / (f64::from(image.height_px) * f64::from(cell_width));
    let rows = (f64::from(width) / columns_per_row).round().max(1.0) as u16;
    if rows <= MAX_ROWS {
        Rect::new(0, 0, width, rows)
    } else {
        let columns = (f64::from(MAX_ROWS) * columns_per_row).round() as u16;
        Rect::new(0, 0, columns.clamp(1, width), MAX_ROWS)
    }
}

struct ImageContext {
    session_id: Option<String>,
    workspace_root: String,
    image: ToolImage,
}

impl ImageContext {
    fn matches_session(&self, session: Option<&TuiSession>, root: &str) -> bool {
        self.session_id.as_deref() == session.map(|session| session.id.as_str())
            && self.workspace_root == root
    }

    fn is_current(&self, app: &App) -> bool {
        self.matches_session(app.session.active_session.as_ref(), &app.session.workspace_root)
            && app.session.transcript.iter().any(|line| {
                matches!(line.content(), TranscriptLine::Tool(tool) if tool.images.contains(&self.image))
            })
    }
}

pub(super) struct Cached<T> {
    context: ImageContext,
    pub(super) value: T,
}

impl<T> Cached<T> {
    pub(super) fn matches(
        &self,
        session: Option<&TuiSession>,
        root: &str,
        image: &ToolImage,
    ) -> bool {
        self.context.matches_session(session, root) && &self.context.image == image
    }
}

pub(super) struct Pending {
    context: ImageContext,
    size: Rect,
    work: Work,
}

pub(super) struct Prepared {
    pub(super) size: Rect,
    pub(super) protocol: StatefulProtocol,
}

enum Work {
    ReadRetained(RetainedImageResponse),
    ReadWorkspace(RuntimeResponse),
    Encode(mpsc::Receiver<Result<StatefulProtocol, String>>),
}

enum ImageInput {
    Retained(Vec<u8>),
    Workspace { response: Value, root: String },
    Resize(StatefulProtocol),
}

fn decoded_bytes(image: &ToolImage) -> u64 {
    u64::from(image.width_px)
        .saturating_mul(u64::from(image.height_px))
        .saturating_mul(4)
}

pub(super) fn visible_images(
    view: &TranscriptView,
    scroll: u64,
    height: u16,
) -> impl Iterator<Item = (&TranscriptImagePlacement, u16, bool)> {
    let mut remaining = TOOL_IMAGE_CACHE_MAX_DECODED_BYTES;
    view.images.iter().rev().filter_map(move |placement| {
        let row = placement.row.checked_sub(scroll)?;
        if row.saturating_add(u64::from(placement.size.height)) > u64::from(height) {
            return None;
        }
        let bytes = decoded_bytes(&placement.image);
        // Select a stable set of visible images so completing one load does not
        // repeatedly evict and reload another image in the same viewport.
        let fits_cache = if bytes > TOOL_IMAGE_MAX_DECODED_BYTES {
            true // Let the per-image check report the usual size error.
        } else if bytes <= remaining {
            remaining -= bytes;
            true
        } else {
            false
        };
        Some((placement, row as u16, fits_cache))
    })
}

pub(super) fn prepare_visible(app: &mut App, view: &TranscriptView, height: u16) -> bool {
    if app.pending_inline_image.is_some() {
        return false;
    }
    let Some((placement, _, _)) = visible_images(view, app.session.transcript_scroll, height)
        .filter(|(placement, _, fits_cache)| {
            let image = &placement.image;
            let session = app.session.active_session.as_ref();
            let root = &app.session.workspace_root;
            *fits_cache
                && !app.inline_images.get(&image.key).is_some_and(|entry| {
                    entry.matches(session, root, image) && entry.value.size == placement.size
                })
                && !app
                    .inline_image_errors
                    .get(&image.key)
                    .is_some_and(|entry| entry.matches(session, root, image))
        })
        .last()
    else {
        return false;
    };
    let context = ImageContext {
        session_id: app
            .session
            .active_session
            .as_ref()
            .map(|session| session.id.clone()),
        workspace_root: app.session.workspace_root.clone(),
        image: placement.image.clone(),
    };
    let size = placement.size;
    let work = if let Some(entry) = take_cached(app, &context.image.key).filter(|entry| {
        entry.matches(
            app.session.active_session.as_ref(),
            &context.workspace_root,
            &context.image,
        )
    }) {
        start_encode(
            app,
            &context,
            size,
            ImageInput::Resize(entry.value.protocol),
        )
    } else {
        start(app, &context)
    };
    match work {
        Ok(work) => {
            app.pending_inline_image = Some(Pending {
                context,
                size,
                work,
            });
            false
        }
        Err(error) => {
            cache_result(app, context, size, Err(error));
            true
        }
    }
}

fn start(app: &mut App, context: &ImageContext) -> Result<Work, String> {
    ensure_runtime(app)?;
    let session_id = context
        .session_id
        .as_deref()
        .ok_or("inline image requires an active session")?;
    let runtime = app
        .runtime
        .as_ref()
        .ok_or("runtime client is not connected")?;
    runtime
        .retained_execution_image_async(session_id.to_owned(), context.image.sha256.clone())
        .map(Work::ReadRetained)
}

fn read_workspace(app: &mut App, context: &ImageContext) -> Result<Work, String> {
    let runtime = app
        .runtime
        .as_mut()
        .ok_or("runtime client is not connected")?;
    let response = runtime.request_async(
        "workspace_read_file",
        json!({"request": {
            "sessionId": context.session_id,
            "workspaceRoot": context.workspace_root,
            "path": context.image.path,
        }}),
    )?;
    Ok(Work::ReadWorkspace(response))
}

fn start_encode(
    app: &App,
    context: &ImageContext,
    size: Rect,
    input: ImageInput,
) -> Result<Work, String> {
    let picker = app.image_policy.picker.clone();
    let image = context.image.clone();
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("centaeris-tui-inline-image".into())
        .spawn(move || {
            let protocol = match input {
                ImageInput::Retained(bytes) => decode_tool_image(&bytes, &image)
                    .map(|decoded| picker.new_resize_protocol(decoded)),
                ImageInput::Workspace { response, root } => {
                    decode_workspace_tool_image(response, &root, &image)
                        .map(|decoded| picker.new_resize_protocol(decoded))
                }
                ImageInput::Resize(protocol) => Ok(protocol),
            };
            let _ = tx.send(protocol.and_then(|mut protocol| {
                protocol.resize_encode(&Resize::Scale(Some(FilterType::Triangle)), size);
                match protocol.last_encoding_result() {
                    Some(Ok(())) => Ok(protocol),
                    Some(Err(error)) => Err(format!("prepare inline image failed: {error}")),
                    None => Err("inline image encoder returned no result".into()),
                }
            }));
        })
        .map_err(|error| format!("start inline image worker failed: {error}"))?;
    Ok(Work::Encode(rx))
}

pub(super) fn poll(app: &mut App) -> bool {
    let Some(mut pending) = app.pending_inline_image.take() else {
        return false;
    };
    let next = match &pending.work {
        Work::ReadRetained(response) => match response.try_recv() {
            Ok(_) if !pending.context.is_current(app) => return true,
            Ok(Ok(Some(bytes))) => start_encode(
                app,
                &pending.context,
                pending.size,
                ImageInput::Retained(bytes),
            ),
            Ok(Ok(None)) => read_workspace(app, &pending.context),
            Ok(Err(error)) => Err(error),
            Err(mpsc::TryRecvError::Empty) => {
                app.pending_inline_image = Some(pending);
                return false;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err("retained image worker closed".into()),
        },
        Work::ReadWorkspace(_) if !pending.context.is_current(app) => return true,
        Work::ReadWorkspace(response) => match response.try_recv() {
            Ok(Some(response)) => {
                let input = ImageInput::Workspace {
                    response,
                    root: pending.context.workspace_root.clone(),
                };
                start_encode(app, &pending.context, pending.size, input)
            }
            Ok(None) => {
                app.pending_inline_image = Some(pending);
                return false;
            }
            Err(error) => Err(error.to_string()),
        },
        Work::Encode(response) => match response.try_recv() {
            Ok(result) => {
                if pending.context.is_current(app) {
                    cache_result(app, pending.context, pending.size, result);
                }
                return true;
            }
            Err(mpsc::TryRecvError::Empty) => {
                app.pending_inline_image = Some(pending);
                return false;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err("inline image worker closed".into()),
        },
    };
    match next {
        Ok(work) => {
            pending.work = work;
            app.pending_inline_image = Some(pending);
            false
        }
        Err(error) => {
            if pending.context.is_current(app) {
                cache_result(app, pending.context, pending.size, Err(error));
            }
            true
        }
    }
}

fn take_cached(app: &mut App, key: &str) -> Option<Cached<Prepared>> {
    let entry = app.inline_images.remove(key);
    if entry.is_some() {
        app.inline_image_cache_order.retain(|(old_key, bytes)| {
            if old_key == key {
                app.inline_image_cache_bytes -= bytes;
            }
            old_key != key
        });
    }
    entry
}

fn cache_result(
    app: &mut App,
    context: ImageContext,
    size: Rect,
    result: Result<StatefulProtocol, String>,
) {
    let key = context.image.key.clone();
    match result {
        Ok(protocol) => {
            let bytes = decoded_bytes(&context.image);
            while app.inline_image_cache_bytes + bytes > TOOL_IMAGE_CACHE_MAX_DECODED_BYTES {
                let Some((oldest, size)) = app.inline_image_cache_order.pop_front() else {
                    break;
                };
                app.inline_images.remove(&oldest);
                app.inline_image_cache_bytes -= size;
            }
            app.inline_image_errors.remove(&key);
            app.inline_images.insert(
                key.clone(),
                Cached {
                    context,
                    value: Prepared { size, protocol },
                },
            );
            app.inline_image_cache_order.push_back((key, bytes));
            app.inline_image_cache_bytes += bytes;
        }
        Err(value) => {
            app.inline_image_errors
                .insert(key, Cached { context, value });
        }
    }
}
