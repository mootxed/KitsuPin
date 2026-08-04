pub mod backends;
pub mod session;

pub use backends::ClipboardNotification;

use crate::{
    browser_metadata::MetadataBuffer,
    domain::{
        content_hash, normalize_content, CapturedImageSource, ClipboardEventOrigin,
        ClipboardPayload, ImagePayload, NewClip, NewImageClip, OwnCopyGuard,
    },
    persistence::Repository,
    settings::SettingsStore,
};
use arboard::{Clipboard, ImageData};
use chrono::Utc;
use parking_lot::Mutex;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
        Arc,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use x11rb::protocol::xproto::ConnectionExt as XprotoExt;

pub trait ClipboardReader {
    fn file_list(&self) -> Result<Vec<PathBuf>, arboard::Error>;
    fn get_image(&self) -> Result<ImageData<'static>, arboard::Error>;
    fn get_text(&self) -> Result<String, arboard::Error>;
    #[allow(dead_code)]
    fn get_selection_owner(&self) -> Option<u32>;
}

#[derive(Default)]
pub struct ClipboardAccess {
    clipboard: Mutex<Option<Clipboard>>,
}

impl ClipboardAccess {
    pub fn inspect_x11_clipboard(&self, context: &str) -> Option<u32> {
        #[cfg(target_os = "linux")]
        {
            if let Ok((conn, _screen_num)) = x11rb::connect(None) {
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
            }
        }
        let _ = context;
        None
    }

    fn with_arboard<T>(
        &self,
        operation: impl FnOnce(&mut Clipboard) -> Result<T, arboard::Error>,
    ) -> Result<T, arboard::Error> {
        let mut clipboard = self.clipboard.lock();
        if clipboard.is_none() {
            *clipboard = Some(Clipboard::new()?);
        }
        let result = operation(clipboard.as_mut().expect("clipboard initialized"));
        if matches!(result, Err(arboard::Error::ClipboardNotSupported)) {
            *clipboard = None;
        }
        result
    }

    fn with<T>(
        &self,
        operation: impl FnOnce(&mut Clipboard) -> Result<T, arboard::Error>,
    ) -> anyhow::Result<T> {
        Ok(self.with_arboard(operation)?)
    }

    fn read_payload(&self, max_image_bytes: u64) -> Option<ClipboardPayload> {
        let _owner_before = self.inspect_x11_clipboard("before read_payload");
        for attempt in 0..3 {
            if let Some(payload) = read_payload_with_reader(self, max_image_bytes) {
                let _owner_after = self.inspect_x11_clipboard("after read_payload");
                return Some(payload);
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        None
    }
}

impl ClipboardReader for ClipboardAccess {
    fn file_list(&self) -> Result<Vec<PathBuf>, arboard::Error> {
        self.with_arboard(|clipboard| clipboard.get().file_list())
    }

    fn get_image(&self) -> Result<ImageData<'static>, arboard::Error> {
        for attempt in 0..3 {
            if let Ok(image) = self.with_arboard(Clipboard::get_image) {
                return Ok(image);
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        self.with_arboard(Clipboard::get_image)
    }

    fn get_text(&self) -> Result<String, arboard::Error> {
        for attempt in 0..3 {
            if let Ok(text) = self.with_arboard(Clipboard::get_text) {
                return Ok(text);
            }
            *self.clipboard.lock() = None;
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        self.with_arboard(Clipboard::get_text)
    }

    fn get_selection_owner(&self) -> Option<u32> {
        self.inspect_x11_clipboard("ClipboardReader::get_selection_owner")
    }
}

pub fn read_payload_with_reader<R: ClipboardReader>(
    reader: &R,
    max_image_bytes: u64,
) -> Option<ClipboardPayload> {
    if let Ok(paths) = reader.file_list() {
        if paths.len() == 1 {
            let path = &paths[0];
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg" | "webp")) {
                if let Some(image) = decode_image_file(path, max_image_bytes) {
                    log::info!(
                        "Captured CopiedImageFile: {}x{}, mime: {:?}, fingerprint: {}",
                        image.width,
                        image.height,
                        image.source_mime,
                        ClipboardPayload::Image(image.clone()).fingerprint()
                    );
                    return Some(ClipboardPayload::Image(image));
                } else {
                    // Corrupted or oversized image file!
                    // Prohibit falling back to get_image to preserve file clipboard semantics.
                    return reader.get_text().ok().map(ClipboardPayload::Text);
                }
            } else {
                // Non-graphic single file (SVG, PDF, directory, unsupported format).
                // Prohibit falling back to raw get_image; fallback to text if any.
                return reader.get_text().ok().map(ClipboardPayload::Text);
            }
        } else {
            // Empty or multiple files in file_list -> fallback to text directly.
            return reader.get_text().ok().map(ClipboardPayload::Text);
        }
    }

    // file_list missing or unavailable -> inspect raw Clipboard image
    if let Ok(image) = reader.get_image() {
        if let (Ok(width), Ok(height)) = (u32::try_from(image.width), u32::try_from(image.height)) {
            let payload = ImagePayload {
                width,
                height,
                rgba: image.bytes.into_owned(),
                source_mime: None,
                source_bytes: None,
                image_source: CapturedImageSource::ClipboardImage,
            };
            if payload.validate().is_ok() {
                log::info!(
                    "Captured ClipboardImage: {}x{}, fingerprint: {}",
                    payload.width,
                    payload.height,
                    ClipboardPayload::Image(payload.clone()).fingerprint()
                );
                return Some(ClipboardPayload::Image(payload));
            }
        }
    }

    // Fallback to text
    reader.get_text().ok().map(ClipboardPayload::Text)
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
    let token = guard.mark_pending_with_details(fingerprint, ClipboardEventOrigin::KitsuPin, None);
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
#[allow(dead_code)]
pub fn set_clipboard(
    content: &str,
    guard: &OwnCopyGuard,
    access: &ClipboardAccess,
) -> anyhow::Result<()> {
    let payload = ClipboardPayload::Text(content.to_owned());
    let fingerprint = payload_fingerprint(&payload);
    set_clipboard_payload(&payload, &fingerprint, guard, access)
}

#[derive(Debug, Default)]
pub struct ClipboardGeneration {
    current: AtomicU64,
}

impl ClipboardGeneration {
    pub fn current(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
    }

    pub fn next(&self) -> u64 {
        self.current.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[allow(dead_code)]
    pub fn set(&self, val: u64) {
        self.current.store(val, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RepublishDecision {
    Republish,
    SkipNewerClipboard,
    SkipFilePayload,
    SkipOwnEvent,
}

pub fn evaluate_republish(
    captured_generation: u64,
    current_generation: u64,
    image_source: Option<CapturedImageSource>,
    is_own_event: bool,
    owner_changed: bool,
) -> RepublishDecision {
    if is_own_event {
        RepublishDecision::SkipOwnEvent
    } else if image_source == Some(CapturedImageSource::CopiedImageFile) {
        RepublishDecision::SkipFilePayload
    } else if owner_changed || current_generation != captured_generation {
        RepublishDecision::SkipNewerClipboard
    } else if image_source == Some(CapturedImageSource::ClipboardImage) {
        RepublishDecision::Republish
    } else {
        RepublishDecision::SkipFilePayload
    }
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
    let generation = Arc::new(ClipboardGeneration::default());
    let generation_clone = Arc::clone(&generation);
    let selected_monitor = backends::select_monitor();
    log::info!(
        "Clipboard watcher starting with monitor: {} (session: {})",
        selected_monitor.name(),
        selected_monitor.session_type()
    );

    std::thread::spawn(move || {
        let mode = match selected_monitor.start(generation_clone) {
            Ok(mode) => mode,
            Err(error) => {
                log::warn!("Clipboard monitor startup failed: {error}. Falling back to polling.");
                crate::capabilities::update_clipboard_monitoring_status(
                    crate::capabilities::CapabilityStatus::Degraded(
                        "XFixes недоступен, используется polling каждые 350 мс".to_string(),
                    ),
                );
                backends::MonitorMode::Polling(Duration::from_millis(350))
            }
        };

        let mut polling_interval: Option<Duration> = None;
        let mut notifications: Option<Receiver<ClipboardNotification>> = None;

        match mode {
            backends::MonitorMode::Disabled => {
                log::info!(
                    "Passive global clipboard monitoring disabled for session type '{}'. KitsuPin running in limited support mode.",
                    selected_monitor.session_type()
                );
                return;
            }
            backends::MonitorMode::EventDriven(rx) => {
                notifications = Some(rx);
            }
            backends::MonitorMode::Polling(interval) => {
                log::info!("Clipboard watcher starting in polling fallback mode ({interval:?})");
                polling_interval = Some(interval);
            }
        }

        let initial_limits = settings.get();
        let mut last_fingerprint = access
            .read_payload(initial_limits.max_image_size_mb as u64 * 1024 * 1024)
            .as_ref()
            .map(payload_fingerprint)
            .unwrap_or_default();

        loop {
            let (from_event, notif) = if let Some(ref rx) = notifications {
                match rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(mut first_notif) => {
                        while let Ok(newer_notif) = rx.try_recv() {
                            first_notif = newer_notif;
                        }
                        (true, Some(first_notif))
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        log::warn!(
                            "Clipboard notifications channel disconnected. Switching to emergency polling fallback."
                        );
                        crate::capabilities::update_clipboard_monitoring_status(
                            crate::capabilities::CapabilityStatus::Degraded(
                                "XFixes недоступен, используется polling каждые 350 мс".to_string(),
                            ),
                        );
                        notifications = None;
                        polling_interval = Some(Duration::from_millis(350));
                        continue;
                    }
                }
            } else if let Some(interval) = polling_interval {
                std::thread::sleep(interval);
                (false, None)
            } else {
                break;
            };

            let captured_owner_before = access.inspect_x11_clipboard("watcher before read");
            let captured_gen = generation.current();

            if let (
                Some(ClipboardNotification::X11Changed {
                    owner, sequence, ..
                }),
                Some(curr_owner),
            ) = (notif, captured_owner_before)
            {
                if owner != curr_owner && owner != 0 {
                    log::info!(
                        "X11 event sequence {} owner window ID {} differs from current owner window ID {}; reading current selection",
                        sequence,
                        owner,
                        curr_owner
                    );
                }
            }

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

            let captured_owner_after = access.inspect_x11_clipboard("watcher after read");
            let _post_read_gen = generation.current();
            let owner_changed = captured_owner_before.is_some()
                && captured_owner_after.is_some()
                && captured_owner_before != captured_owner_after;

            let is_own_event =
                guard.should_suppress_with_owner(&fingerprint, captured_owner_before);
            if is_own_event {
                log::info!("Suppressed own clipboard copy event (fingerprint: {fingerprint})");
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

                    let image_source = match &payload {
                        ClipboardPayload::Image(img) => Some(img.image_source),
                        _ => None,
                    };

                    let current_gen_before_republish = generation.current();
                    let current_owner_before_republish =
                        access.inspect_x11_clipboard("immediately before republish");

                    let owner_changed_before_republish = captured_owner_after.is_some()
                        && current_owner_before_republish.is_some()
                        && captured_owner_after != current_owner_before_republish;

                    let decision = evaluate_republish(
                        captured_gen,
                        current_gen_before_republish,
                        image_source,
                        false,
                        owner_changed || owner_changed_before_republish,
                    );

                    match decision {
                        RepublishDecision::Republish => {
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
                        }
                        RepublishDecision::SkipNewerClipboard => {
                            log::info!(
                                "Skipping re-publication of image: newer clipboard activity detected (captured gen: {}, current gen: {}, owner_changed: {}).",
                                captured_gen,
                                current_gen_before_republish,
                                owner_changed || owner_changed_before_republish
                            );
                        }
                        RepublishDecision::SkipFilePayload => {
                            log::info!(
                                "CopiedImageFile detected or non-raw image payload. Preserving file-list semantics (no set_image re-publication)."
                            );
                        }
                        RepublishDecision::SkipOwnEvent => {
                            log::info!("Skipping re-publication for internal own copy event.");
                        }
                    }
                }
                Err(error) => {
                    log::error!("Failed to save Clipboard: {error}");
                    let _ = app.emit("clipboard-warning", error.to_string());
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockClipboardReader {
        file_list: Option<Vec<PathBuf>>,
        image: Option<ImageData<'static>>,
        text: Option<String>,
        #[allow(dead_code)]
        owner: Option<u32>,
    }

    impl ClipboardReader for MockClipboardReader {
        fn file_list(&self) -> Result<Vec<PathBuf>, arboard::Error> {
            self.file_list
                .clone()
                .ok_or(arboard::Error::ContentNotAvailable)
        }

        fn get_image(&self) -> Result<ImageData<'static>, arboard::Error> {
            self.image
                .as_ref()
                .map(|img| ImageData {
                    width: img.width,
                    height: img.height,
                    bytes: Cow::Owned(img.bytes.to_vec()),
                })
                .ok_or(arboard::Error::ContentNotAvailable)
        }

        fn get_text(&self) -> Result<String, arboard::Error> {
            self.text.clone().ok_or(arboard::Error::ContentNotAvailable)
        }

        fn get_selection_owner(&self) -> Option<u32> {
            self.owner
        }
    }

    // --- Generation & Republish Decision Tests ---

    #[test]
    fn republish_decision_image_a_unchanged_generation() {
        assert_eq!(
            evaluate_republish(
                10,
                10,
                Some(CapturedImageSource::ClipboardImage),
                false,
                false
            ),
            RepublishDecision::Republish
        );
    }

    #[test]
    fn republish_decision_image_a_then_text_b_skips() {
        assert_eq!(
            evaluate_republish(
                10,
                11,
                Some(CapturedImageSource::ClipboardImage),
                false,
                false
            ),
            RepublishDecision::SkipNewerClipboard
        );
    }

    #[test]
    fn republish_decision_image_a_then_image_c_skips() {
        assert_eq!(
            evaluate_republish(
                10,
                12,
                Some(CapturedImageSource::ClipboardImage),
                false,
                false
            ),
            RepublishDecision::SkipNewerClipboard
        );
    }

    #[test]
    fn republish_decision_own_event_suppressed() {
        assert_eq!(
            evaluate_republish(
                10,
                10,
                Some(CapturedImageSource::ClipboardImage),
                true,
                false
            ),
            RepublishDecision::SkipOwnEvent
        );
    }

    #[test]
    fn republish_decision_copied_image_file_skips() {
        assert_eq!(
            evaluate_republish(
                10,
                10,
                Some(CapturedImageSource::CopiedImageFile),
                false,
                false
            ),
            RepublishDecision::SkipFilePayload
        );
    }

    #[test]
    fn republish_decision_owner_changed_during_read_skips() {
        assert_eq!(
            evaluate_republish(
                10,
                10,
                Some(CapturedImageSource::ClipboardImage),
                false,
                true
            ),
            RepublishDecision::SkipNewerClipboard
        );
    }

    #[test]
    fn republish_decision_owner_read_failure_safe() {
        assert_eq!(
            evaluate_republish(
                10,
                11,
                Some(CapturedImageSource::ClipboardImage),
                false,
                false
            ),
            RepublishDecision::SkipNewerClipboard
        );
    }

    #[test]
    fn republish_decision_generation_wrapping_overflow() {
        let gen1 = u64::MAX;
        let gen2 = u64::MAX.wrapping_add(1);
        assert_eq!(
            evaluate_republish(
                gen1,
                gen2,
                Some(CapturedImageSource::ClipboardImage),
                false,
                false
            ),
            RepublishDecision::SkipNewerClipboard
        );
    }

    // --- Reader Priority & File Tests ---

    #[test]
    fn file_list_plus_png_selects_copied_image_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.png");
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            1,
            image::Rgb([200, 10, 20]),
        ));
        source
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();

        let mock = MockClipboardReader {
            file_list: Some(vec![path]),
            image: Some(ImageData {
                width: 2,
                height: 1,
                bytes: Cow::Owned(vec![200, 10, 20, 255, 200, 10, 20, 255]),
            }),
            text: Some("file:///path/to/sample.png".to_owned()),
            owner: Some(101),
        };

        let payload = read_payload_with_reader(&mock, 1024 * 1024).unwrap();
        match payload {
            ClipboardPayload::Image(img) => {
                assert_eq!(img.image_source, CapturedImageSource::CopiedImageFile);
            }
            _ => panic!("Expected CopiedImageFile payload"),
        }
    }

    #[test]
    fn only_image_png_selects_clipboard_image() {
        let mock = MockClipboardReader {
            file_list: None,
            image: Some(ImageData {
                width: 2,
                height: 1,
                bytes: Cow::Owned(vec![255, 0, 0, 255, 0, 255, 0, 255]),
            }),
            text: None,
            owner: Some(102),
        };

        let payload = read_payload_with_reader(&mock, 1024 * 1024).unwrap();
        match payload {
            ClipboardPayload::Image(img) => {
                assert_eq!(img.image_source, CapturedImageSource::ClipboardImage);
            }
            _ => panic!("Expected ClipboardImage payload"),
        }
    }

    #[test]
    fn one_jpeg_and_webp_files_selected_as_copied_image_file() {
        let temp = tempfile::tempdir().unwrap();
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            1,
            image::Rgb([10, 200, 50]),
        ));

        for (ext, fmt) in [
            ("jpg", image::ImageFormat::Jpeg),
            ("webp", image::ImageFormat::WebP),
        ] {
            let path = temp.path().join(format!("test.{ext}"));
            source.save_with_format(&path, fmt).unwrap();

            let mock = MockClipboardReader {
                file_list: Some(vec![path]),
                image: None,
                text: None,
                owner: Some(103),
            };

            let payload = read_payload_with_reader(&mock, 1024 * 1024).unwrap();
            match payload {
                ClipboardPayload::Image(img) => {
                    assert_eq!(img.image_source, CapturedImageSource::CopiedImageFile);
                }
                _ => panic!("Expected CopiedImageFile for {ext}"),
            }
        }
    }

    #[test]
    fn svg_file_not_decoded_as_image() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.svg");
        std::fs::write(&path, "<svg xmlns='http://www.w3.org/2000/svg'></svg>").unwrap();

        let mock = MockClipboardReader {
            file_list: Some(vec![path.clone()]),
            image: Some(ImageData {
                width: 10,
                height: 10,
                bytes: Cow::Owned(vec![0; 400]),
            }),
            text: Some(format!("file://{}", path.display())),
            owner: Some(104),
        };

        let payload = read_payload_with_reader(&mock, 1024 * 1024).unwrap();
        match payload {
            ClipboardPayload::Text(text) => {
                assert!(text.contains("sample.svg"));
            }
            _ => panic!("SVG file should not be decoded as image payload"),
        }
    }

    #[test]
    fn multiple_files_not_converted_to_image() {
        let temp = tempfile::tempdir().unwrap();
        let path1 = temp.path().join("1.png");
        let path2 = temp.path().join("2.png");
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            1,
            1,
            image::Rgb([0, 0, 0]),
        ));
        img.save_with_format(&path1, image::ImageFormat::Png)
            .unwrap();
        img.save_with_format(&path2, image::ImageFormat::Png)
            .unwrap();

        let mock = MockClipboardReader {
            file_list: Some(vec![path1, path2]),
            image: Some(ImageData {
                width: 1,
                height: 1,
                bytes: Cow::Owned(vec![0, 0, 0, 255]),
            }),
            text: Some("file:///1.png\nfile:///2.png".to_owned()),
            owner: Some(105),
        };

        let payload = read_payload_with_reader(&mock, 1024 * 1024).unwrap();
        match payload {
            ClipboardPayload::Text(text) => {
                assert!(text.contains("1.png"));
            }
            _ => panic!("Multiple files should not be converted to single image"),
        }
    }

    #[test]
    fn directory_not_decoded_as_image() {
        let temp = tempfile::tempdir().unwrap();
        let dir_path = temp.path().join("dir.png");
        std::fs::create_dir(&dir_path).unwrap();

        let mock = MockClipboardReader {
            file_list: Some(vec![dir_path.clone()]),
            image: None,
            text: Some(format!("file://{}", dir_path.display())),
            owner: Some(106),
        };

        let payload = read_payload_with_reader(&mock, 1024 * 1024).unwrap();
        match payload {
            ClipboardPayload::Text(_) => {}
            _ => panic!("Directory with .png extension must not be decoded as image"),
        }
    }

    #[test]
    fn corrupted_image_file_does_not_replace_active_clipboard() {
        let temp = tempfile::tempdir().unwrap();
        let corrupt_path = temp.path().join("corrupt.png");
        std::fs::write(&corrupt_path, b"not a png image data").unwrap();

        let mock = MockClipboardReader {
            file_list: Some(vec![corrupt_path]),
            image: Some(ImageData {
                width: 5,
                height: 5,
                bytes: Cow::Owned(vec![0; 100]),
            }),
            text: None,
            owner: Some(107),
        };

        let payload = read_payload_with_reader(&mock, 1024 * 1024);
        assert!(payload.is_none());
    }

    #[test]
    fn file_above_size_limit_not_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("big.png");
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            10,
            10,
            image::Rgb([0, 0, 0]),
        ));
        img.save_with_format(&path, image::ImageFormat::Png)
            .unwrap();

        let mock = MockClipboardReader {
            file_list: Some(vec![path]),
            image: None,
            text: None,
            owner: Some(108),
        };

        let payload = read_payload_with_reader(&mock, 10);
        assert!(payload.is_none());
    }

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
        let payload = ClipboardPayload::Text(value.clone());
        let fingerprint = payload.fingerprint();
        set_clipboard_payload(&payload, &fingerprint, &guard, &access).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let independent_reader = ClipboardAccess::default();
        let payload_read = independent_reader.read_payload(1024 * 1024);
        match payload_read {
            Some(ClipboardPayload::Text(text)) => assert_eq!(text, value),
            _ => panic!("Expected text payload from independent reader"),
        }
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
        let mut image_res = independent_reader.get_image();
        for _ in 0..5 {
            if image_res.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            image_res = independent_reader.get_image();
        }
        let image = image_res.unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.bytes.as_ref(), &[255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
