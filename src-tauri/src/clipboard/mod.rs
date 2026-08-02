use crate::{
    browser_metadata::MetadataBuffer,
    domain::{
        content_hash, normalize_content, CapturedImageSource, ClipboardPayload, ImagePayload,
        NewClip, NewImageClip, OwnCopyGuard,
    },
    persistence::Repository,
    settings::SettingsStore,
};
use arboard::{Clipboard, ImageData};
use chrono::Utc;
use parking_lot::Mutex;
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::{ConnectionExt as XfixesExt, SelectionEventMask},
        xproto::ConnectionExt as XprotoExt,
        Event,
    },
};

#[derive(Default)]
pub struct ClipboardAccess {
    clipboard: Mutex<Option<Clipboard>>,
}

impl ClipboardAccess {
    fn inspect_x11_clipboard(&self, context: &str) -> Option<u32> {
        #[cfg(target_os = "linux")]
        {
            if let Ok((conn, screen_num)) = x11rb::connect(None) {
                if let Ok(clipboard_reply) = conn.intern_atom(false, b"CLIPBOARD") {
                    if let Ok(clipboard_atom) = clipboard_reply.reply() {
                        if let Ok(owner_reply) = conn.get_selection_owner(clipboard_atom.atom) {
                            if let Ok(owner) = owner_reply.reply() {
                                log::info!(
                                    "X11 CLIPBOARD diagnostic [{context}]: selection owner window ID = {}",
                                    owner.owner
                                );
                                return Some(owner.owner);
                            }
                        }
                    }
                }
                let _ = screen_num;
            }
        }
        let _ = context;
        None
    }

    fn with<T>(
        &self,
        operation: impl FnOnce(&mut Clipboard) -> Result<T, arboard::Error>,
    ) -> anyhow::Result<T> {
        let mut clipboard = self.clipboard.lock();
        if clipboard.is_none() {
            *clipboard = Some(Clipboard::new()?);
        }
        let result = operation(clipboard.as_mut().expect("clipboard initialized"));
        if result.is_err() {
            *clipboard = None;
        }
        Ok(result?)
    }

    fn read_text(&self) -> Option<String> {
        for attempt in 0..3 {
            if let Ok(text) = self.with(Clipboard::get_text) {
                return Some(text);
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        None
    }

    fn read_payload(&self, max_image_bytes: u64) -> Option<ClipboardPayload> {
        let _owner_before = self.inspect_x11_clipboard("before read_payload");
        for attempt in 0..3 {
            if let Ok(image) = self.with(Clipboard::get_image) {
                let payload = ImagePayload {
                    width: u32::try_from(image.width).ok()?,
                    height: u32::try_from(image.height).ok()?,
                    rgba: image.bytes.into_owned(),
                    source_mime: None,
                    source_bytes: None,
                    image_source: CapturedImageSource::ClipboardImage,
                };
                if payload.validate().is_ok() {
                    let _owner_after = self.inspect_x11_clipboard("after Clipboard::get_image");
                    log::info!(
                        "Captured ClipboardImage: {}x{}, fingerprint: {}",
                        payload.width,
                        payload.height,
                        ClipboardPayload::Image(payload.clone()).fingerprint()
                    );
                    return Some(ClipboardPayload::Image(payload));
                }
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        if let Some(image) = self.read_file_image(max_image_bytes) {
            log::info!(
                "Captured CopiedImageFile: {}x{}, mime: {:?}, fingerprint: {}",
                image.width,
                image.height,
                image.source_mime,
                ClipboardPayload::Image(image.clone()).fingerprint()
            );
            return Some(ClipboardPayload::Image(image));
        }
        self.read_text().map(ClipboardPayload::Text)
    }

    fn read_file_image(&self, max_image_bytes: u64) -> Option<ImagePayload> {
        let paths = self.with(|clipboard| clipboard.get().file_list()).ok()?;
        if paths.len() != 1 {
            return None;
        }
        let path = paths.first()?;
        decode_image_file(path, max_image_bytes)
    }
}

fn decode_image_file(path: &std::path::Path, max_image_bytes: u64) -> Option<ImagePayload> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let expected_mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => return None,
    };
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_image_bytes {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let expected_format = match expected_mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => return None,
    };
    if reader.format() != Some(expected_format) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32_768);
    limits.max_image_height = Some(32_768);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    let payload = ImagePayload {
        width,
        height,
        rgba: decoded.into_raw(),
        source_mime: Some(expected_mime.to_owned()),
        source_bytes: Some(bytes),
        image_source: CapturedImageSource::CopiedImageFile,
    };
    payload.validate().ok()?;
    Some(payload)
}

fn payload_fingerprint(payload: &ClipboardPayload) -> String {
    payload.fingerprint()
}

pub fn set_clipboard_payload(
    payload: &ClipboardPayload,
    fingerprint: &str,
    guard: &OwnCopyGuard,
    access: &ClipboardAccess,
) -> anyhow::Result<()> {
    let token = guard.mark_pending(fingerprint);
    let result = match payload {
        ClipboardPayload::Text(content) => {
            access.with(|clipboard| clipboard.set_text(content.to_owned()))
        }
        ClipboardPayload::Image(image) => access.with(|clipboard| {
            clipboard.set_image(ImageData {
                width: image.width as usize,
                height: image.height as usize,
                bytes: Cow::Borrowed(&image.rgba),
            })
        }),
    };
    match result {
        Ok(()) => {
            guard.commit(token);
            Ok(())
        }
        Err(error) => {
            guard.cancel(token);
            Err(error)
        }
    }
}

#[cfg(test)]
pub fn set_clipboard(
    content: &str,
    guard: &OwnCopyGuard,
    access: &ClipboardAccess,
) -> anyhow::Result<()> {
    let payload = ClipboardPayload::Text(content.to_owned());
    let fingerprint = payload_fingerprint(&payload);
    set_clipboard_payload(&payload, &fingerprint, guard, access)
}

fn x11_notifications() -> anyhow::Result<Receiver<()>> {
    let (sender, receiver) = mpsc::channel();
    let (connection, screen_number) = x11rb::connect(None)?;
    connection.xfixes_query_version(5, 0)?.reply()?;
    let clipboard = connection.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
    let root = connection.setup().roots[screen_number].root;
    connection.xfixes_select_selection_input(
        root,
        clipboard,
        SelectionEventMask::SET_SELECTION_OWNER,
    )?;
    connection.flush()?;
    std::thread::spawn(move || loop {
        match connection.wait_for_event() {
            Ok(Event::XfixesSelectionNotify(event)) if event.selection == clipboard => {
                if sender.send(()).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!("XFixes Clipboard notifications stopped: {error}");
                break;
            }
        }
    });
    Ok(receiver)
}

pub fn start(
    app: AppHandle,
    repo: Arc<Repository>,
    metadata: Arc<MetadataBuffer>,
    guard: Arc<OwnCopyGuard>,
    access: Arc<ClipboardAccess>,
    paused: Arc<AtomicBool>,
    settings: Arc<SettingsStore>,
) {
    std::thread::spawn(move || {
        let notifications = match x11_notifications() {
            Ok(receiver) => Some(receiver),
            Err(error) => {
                log::warn!("XFixes unavailable, using Clipboard polling fallback: {error}");
                None
            }
        };
        let initial_limits = settings.get();
        let mut last_fingerprint = access
            .read_payload(initial_limits.max_image_size_mb as u64 * 1024 * 1024)
            .as_ref()
            .map(payload_fingerprint)
            .unwrap_or_default();

        loop {
            let from_event = if let Some(receiver) = &notifications {
                match receiver.recv_timeout(Duration::from_millis(500)) {
                    Ok(()) => true,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        std::thread::sleep(Duration::from_millis(350));
                        false
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(350));
                false
            };
            let max_image_bytes = settings.get().max_image_size_mb as u64 * 1024 * 1024;
            let Some(payload) = access.read_payload(max_image_bytes) else {
                continue;
            };
            let fingerprint = payload_fingerprint(&payload);
            let is_same = fingerprint == last_fingerprint;
            last_fingerprint.clone_from(&fingerprint);

            if paused.load(Ordering::Relaxed) {
                continue;
            }
            if !from_event && is_same {
                continue;
            }
            if guard.should_suppress(&fingerprint) {
                continue;
            }
            let now = Utc::now();
            let save_result = match &payload {
                ClipboardPayload::Text(text) => {
                    let normalized = normalize_content(text);
                    if normalized.is_empty() {
                        continue;
                    }
                    let hash = content_hash(&normalized);
                    let browser = metadata.take_match(&hash, normalized.len(), now);
                    repo.upsert_clip(NewClip {
                        content: text,
                        domain: browser.as_ref().map(|event| event.domain.as_str()),
                        page_title: browser.as_ref().map(|event| event.page_title.as_str()),
                        now: now.timestamp_millis(),
                    })
                    .map(|(_, receipt)| {
                        if let Some(receipt) = receipt {
                            metadata.push_receipt(&hash, normalized.len(), receipt);
                            metadata.reconcile_pending(
                                &repo,
                                Some(&|| {
                                    let _ = app.emit("clips-changed", ());
                                }),
                            );
                        }
                    })
                }
                ClipboardPayload::Image(image) => {
                    let limits = settings.get();
                    let (image_hash, image_length) = payload
                        .match_key()
                        .expect("validated image payload has a match key");
                    let mut browser = metadata.take_match(&image_hash, image_length, Utc::now());
                    // Chrome must read and hash the just-written image after the DOM copy event.
                    // Give that high-confidence metadata a short bounded window to arrive.
                    for _ in 0..4 {
                        if browser.is_some() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                        browser = metadata.take_match(&image_hash, image_length, Utc::now());
                    }
                    repo.upsert_image(NewImageClip {
                        image,
                        domain: browser.as_ref().map(|event| event.domain.as_str()),
                        page_title: browser.as_ref().map(|event| event.page_title.as_str()),
                        now: now.timestamp_millis(),
                        max_image_bytes: limits.max_image_size_mb as u64 * 1024 * 1024,
                        max_storage_bytes: limits.max_storage_size_mb as u64 * 1024 * 1024,
                    })
                    .map(|(_summary, receipt)| {
                        if let Some(receipt) = receipt {
                            metadata.push_receipt(&image_hash, image_length, receipt);
                            metadata.reconcile_pending(
                                &repo,
                                Some(&|| {
                                    let _ = app.emit("clips-changed", ());
                                }),
                            );
                        }
                    })
                }
            };
            match save_result {
                Ok(()) => {
                    let _ = app.emit("clips-changed", ());

                    if let ClipboardPayload::Image(ref image_payload) = payload {
                        if image_payload.image_source == CapturedImageSource::ClipboardImage {
                            log::info!(
                                "ClipboardImage saved to DB successfully. Re-publishing PNG image to X11 CLIPBOARD to maintain selection ownership for immediate Ctrl+V..."
                            );
                            if let Err(error) =
                                set_clipboard_payload(&payload, &fingerprint, &guard, &access)
                            {
                                log::error!(
                                    "Failed to re-publish ClipboardImage to X11 CLIPBOARD: {error}"
                                );
                            } else {
                                log::info!(
                                    "Successfully re-published ClipboardImage to X11 CLIPBOARD."
                                );
                            }
                        } else {
                            log::info!(
                                "CopiedImageFile detected. Preserving file-list semantics (no set_image re-publication)."
                            );
                        }
                    }
                }
                Err(error) => {
                    log::error!("Не удалось сохранить Clipboard: {error}");
                    let _ = app.emit("clipboard-warning", error.to_string());
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_png_jpeg_and_webp_files_but_not_svg() {
        let temp = tempfile::tempdir().unwrap();
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            1,
            image::Rgb([200, 10, 20]),
        ));
        for (extension, format, mime) in [
            ("png", image::ImageFormat::Png, "image/png"),
            ("jpg", image::ImageFormat::Jpeg, "image/jpeg"),
            ("webp", image::ImageFormat::WebP, "image/webp"),
        ] {
            let path = temp.path().join(format!("sample.{extension}"));
            source.save_with_format(&path, format).unwrap();
            let decoded = decode_image_file(&path, 1024 * 1024).unwrap();
            assert_eq!(decoded.width, 2);
            assert_eq!(decoded.height, 1);
            assert_eq!(decoded.source_mime.as_deref(), Some(mime));
            assert_eq!(decoded.image_source, CapturedImageSource::CopiedImageFile);
            assert!(decoded
                .source_bytes
                .as_ref()
                .is_some_and(|bytes| !bytes.is_empty()));
        }
        let svg = temp.path().join("active.svg");
        std::fs::write(&svg, "<svg xmlns='http://www.w3.org/2000/svg'></svg>").unwrap();
        assert!(decode_image_file(&svg, 1024 * 1024).is_none());
    }

    #[test]
    #[ignore = "requires a live X11 session"]
    fn owned_clipboard_remains_available_after_set_returns() {
        let access = ClipboardAccess::default();
        let guard = OwnCopyGuard::default();
        let value = format!("KitsuPin persistent X11 owner {}", std::process::id());
        set_clipboard(&value, &guard, &access).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut independent_reader = Clipboard::new().unwrap();
        assert_eq!(independent_reader.get_text().unwrap(), value);
    }

    #[test]
    #[ignore = "requires a live X11 session"]
    fn owned_image_remains_available_after_set_returns() {
        let access = ClipboardAccess::default();
        let guard = OwnCopyGuard::default();
        let payload = ClipboardPayload::Image(ImagePayload {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            source_mime: None,
            source_bytes: None,
            image_source: CapturedImageSource::ClipboardImage,
        });
        let fingerprint = payload.fingerprint();
        set_clipboard_payload(&payload, &fingerprint, &guard, &access).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut independent_reader = Clipboard::new().unwrap();
        let image = independent_reader.get_image().unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.bytes.as_ref(), &[255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
