use crate::{
    browser_metadata::MetadataBuffer,
    domain::{content_hash, normalize_content, NewClip, OwnCopyGuard},
    persistence::Repository,
};
use arboard::Clipboard;
use chrono::Utc;
use parking_lot::Mutex;
use std::{
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
}

pub fn set_clipboard(
    content: &str,
    guard: &OwnCopyGuard,
    access: &ClipboardAccess,
) -> anyhow::Result<()> {
    let normalized = normalize_content(content);
    let token = guard.mark_pending(&normalized);
    match access.with(|clipboard| clipboard.set_text(content.to_owned())) {
        Ok(()) => {
            guard.commit(token);
            Ok(())
        }
        Err(error) => {
            guard.cancel(token);
            Err(error.into())
        }
    }
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
) {
    std::thread::spawn(move || {
        let notifications = match x11_notifications() {
            Ok(receiver) => Some(receiver),
            Err(error) => {
                log::warn!("XFixes unavailable, using Clipboard polling fallback: {error}");
                None
            }
        };
        let mut last_hash = access
            .read_text()
            .map(|text| content_hash(&normalize_content(&text)))
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
            let Some(text) = access.read_text() else {
                continue;
            };
            let normalized = normalize_content(&text);
            if normalized.is_empty() {
                continue;
            }
            let hash = content_hash(&normalized);
            let is_same = hash == last_hash;
            last_hash.clone_from(&hash);

            if paused.load(Ordering::Relaxed) {
                continue;
            }
            if !from_event && is_same {
                continue;
            }
            if guard.should_suppress(&normalized) {
                continue;
            }
            let now = Utc::now();
            let browser = metadata.take_match(&hash, normalized.len(), now);
            let input = NewClip {
                content: &text,
                domain: browser.as_ref().map(|event| event.domain.as_str()),
                page_title: browser.as_ref().map(|event| event.page_title.as_str()),
                now: now.timestamp_millis(),
            };
            match repo.upsert_clip(input) {
                Ok((_, receipt)) => {
                    if let Some(r) = receipt {
                        metadata.push_receipt(&hash, normalized.len(), r);
                    }
                    let _ = app.emit("clips-changed", ());
                }
                Err(error) => log::error!("Не удалось сохранить Clipboard: {error}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
