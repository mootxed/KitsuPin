use crate::domain::normalize_domain;
use anyhow::Result;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCopyEvent {
    pub version: u8,
    pub event: String,
    pub content_hash: String,
    pub content_length: usize,
    pub domain: String,
    pub page_title: String,
    pub timestamp: String,
}

impl BrowserCopyEvent {
    pub fn validate(mut self) -> Result<Self> {
        anyhow::ensure!(
            self.version == 1 && self.event == "copy",
            "неподдерживаемое сообщение"
        );
        anyhow::ensure!(
            self.content_hash.len() == 64
                && self.content_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "некорректный hash"
        );
        anyhow::ensure!(
            self.content_length <= 1_000_000,
            "слишком большое содержимое"
        );
        self.domain =
            normalize_domain(&self.domain).ok_or_else(|| anyhow::anyhow!("некорректный домен"))?;
        self.page_title = self.page_title.trim().chars().take(500).collect();
        DateTime::parse_from_rfc3339(&self.timestamp)?;
        Ok(self)
    }
}

#[derive(Default)]
pub struct MetadataBuffer {
    events: Mutex<VecDeque<(DateTime<Utc>, BrowserCopyEvent)>>,
}
impl MetadataBuffer {
    pub fn push(&self, event: BrowserCopyEvent) -> Result<()> {
        let event = event.validate()?;
        let at = DateTime::parse_from_rfc3339(&event.timestamp)?.with_timezone(&Utc);
        let mut events = self.events.lock();
        events.push_back((at, event));
        while events.len() > 64 {
            events.pop_front();
        }
        Ok(())
    }
    pub fn take_match(
        &self,
        hash: &str,
        length: usize,
        now: DateTime<Utc>,
    ) -> Option<BrowserCopyEvent> {
        let mut events = self.events.lock();
        events.retain(|(at, _)| now.signed_duration_since(*at).num_milliseconds().abs() <= 5_000);
        let pos = events.iter().rposition(|(at, e)| {
            e.content_hash.eq_ignore_ascii_case(hash)
                && e.content_length == length
                && now.signed_duration_since(*at).num_milliseconds().abs() <= 2_500
        })?;
        events.remove(pos).map(|(_, e)| e)
    }
    pub fn remove_matching(&self, hash: &str, length: usize) {
        let mut events = self.events.lock();
        if let Some(pos) = events.iter().rposition(|(_, e)| {
            e.content_hash.eq_ignore_ascii_case(hash) && e.content_length == length
        }) {
            events.remove(pos);
        }
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("native.sock")
}

/// Start the Native Messaging Unix socket server.
///
/// Probes the existing socket before removing it:
/// - If connection succeeds → another server is active; returns Err.
/// - If connection fails   → stale socket; removes it, then binds.
///
/// The `reconcile_callback` is called for each validated BrowserCopyEvent so that
/// the late-reconciliation service can attach metadata to recently saved clips.
pub fn start_socket_server(
    path: PathBuf,
    buffer: Arc<MetadataBuffer>,
    reconcile_callback: Arc<dyn Fn(BrowserCopyEvent) + Send + Sync>,
) -> Result<()> {
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => {
                // A live server is already listening. Do NOT remove its socket.
                anyhow::bail!(
                    "Native Messaging socket {:?} уже занят другим активным процессом",
                    path
                );
            }
            Err(_) => {
                // Stale socket from a crashed process — safe to remove.
                log::info!("Removing stale native.sock at {:?}", path);
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => read_stream(s, &buffer, &*reconcile_callback),
                Err(e) => log::warn!("native socket: {e}"),
            }
        }
    });
    Ok(())
}

fn read_stream(
    stream: UnixStream,
    buffer: &MetadataBuffer,
    reconcile: &(impl Fn(BrowserCopyEvent) + ?Sized),
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    for line in BufReader::new(stream).lines().take(8) {
        match line {
            Ok(line) if line.len() <= 16_384 => {
                match serde_json::from_str::<BrowserCopyEvent>(&line) {
                    Ok(event) => match buffer.push(event.clone()) {
                        Ok(()) => reconcile(event),
                        Err(e) => log::warn!("Отклонено сообщение Chrome: {e}"),
                    },
                    Err(e) => log::warn!("Отклонён некорректный JSON Chrome: {e}"),
                }
            }
            _ => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_messages_and_matches_reliably() {
        let b = MetadataBuffer::default();
        let now = Utc::now();
        let e = BrowserCopyEvent {
            version: 1,
            event: "copy".into(),
            content_hash: "a".repeat(64),
            content_length: 5,
            domain: "WWW.Example.COM.".into(),
            page_title: " Page ".into(),
            timestamp: now.to_rfc3339(),
        };
        b.push(e).unwrap();
        assert!(b.take_match(&"b".repeat(64), 5, now).is_none());
        let found = b.take_match(&"a".repeat(64), 5, now).unwrap();
        assert_eq!(found.domain, "example.com");
        assert_eq!(found.page_title, "Page");
    }
    #[test]
    fn rejects_unknown_protocol() {
        let mut value = serde_json::json!({"version":1,"event":"exec","contentHash":"a".repeat(64),"contentLength":1,"domain":"example.com","pageTitle":"x","timestamp":Utc::now().to_rfc3339()});
        assert!(serde_json::from_value::<BrowserCopyEvent>(value.clone())
            .unwrap()
            .validate()
            .is_err());
        value["command"] = serde_json::json!("rm");
        assert!(serde_json::from_value::<BrowserCopyEvent>(value).is_err());
    }

    #[test]
    fn stale_socket_is_removed_and_fresh_bind_succeeds() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        // Create a stale socket file (no listener).
        let _ = UnixListener::bind(&path).unwrap();
        // Drop the listener so nothing is listening but the file remains.
        drop(UnixListener::bind(&path)); // second bind will fail; use the first.
                                         // Ensure file exists.
        assert!(path.exists());
        // Now call start_socket_server; should detect stale socket, remove, and bind.
        let buffer = Arc::new(MetadataBuffer::default());
        let cb = Arc::new(|_: BrowserCopyEvent| {});
        // We can't fully test thread binding here without a live process,
        // so just verify the file-probe logic doesn't panic.
        // The actual server start is best-effort in this unit test.
        let res = start_socket_server(path.clone(), buffer, cb);
        assert!(res.is_ok());
    }
}
