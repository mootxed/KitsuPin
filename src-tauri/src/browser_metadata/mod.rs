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
use tauri::Emitter;
use uuid::Uuid;

pub const RECEIPT_MATCH_WINDOW_MS: i64 = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserCopyEvent {
    #[serde(default = "Uuid::new_v4")]
    pub event_id: Uuid,
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
        if self.event_id.is_nil() {
            self.event_id = Uuid::new_v4();
        }
        Ok(self)
    }

    pub fn timestamp_millis(&self) -> Result<i64> {
        let dt = DateTime::parse_from_rfc3339(&self.timestamp)?;
        Ok(dt.timestamp_millis())
    }
}

#[derive(Debug, Clone)]
pub struct ClipUpsertReceipt {
    pub receipt_id: Uuid,
    pub clip_id: String,
    pub content_hash: String,
    pub normalized_length_bytes: usize,
    pub previous_last_copied_at: Option<i64>,
    pub previous_sort_key: Option<i64>,
    pub previous_copy_count: i64,
    pub resulting_last_copied_at: i64,
    pub resulting_sort_key: i64,
    pub resulting_copy_count: i64,
    pub copy_timestamp: i64,
}

#[derive(Default)]
pub struct MetadataBuffer {
    events: Mutex<VecDeque<(DateTime<Utc>, BrowserCopyEvent)>>,
    receipts: Mutex<VecDeque<(String, usize, ClipUpsertReceipt, bool)>>,
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
        drop(events);
        self.cleanup_stale(Utc::now());
        Ok(())
    }

    pub fn take_match(
        &self,
        hash: &str,
        length: usize,
        now: DateTime<Utc>,
    ) -> Option<BrowserCopyEvent> {
        let mut events = self.events.lock();
        events.retain(|(at, _)| now.signed_duration_since(*at).num_milliseconds().abs() <= 10_000);
        let pos = events.iter().rposition(|(at, e)| {
            e.content_hash.eq_ignore_ascii_case(hash)
                && e.content_length == length
                && now.signed_duration_since(*at).num_milliseconds().abs() <= 2_500
        })?;
        events.remove(pos).map(|(_, e)| e)
    }

    pub fn remove_event(&self, event_id: Uuid) {
        let mut events = self.events.lock();
        if let Some(pos) = events.iter().position(|(_, e)| e.event_id == event_id) {
            events.remove(pos);
        }
    }

    pub fn push_receipt(&self, hash: &str, length: usize, receipt: ClipUpsertReceipt) {
        let mut receipts = self.receipts.lock();
        receipts.push_back((hash.to_lowercase(), length, receipt, false));
        while receipts.len() > 64 {
            receipts.pop_front();
        }
        drop(receipts);
        self.cleanup_stale(Utc::now());
    }

    pub fn reserve_matching_pair(
        &self,
        allowed_delta_ms: i64,
    ) -> Option<(BrowserCopyEvent, ClipUpsertReceipt)> {
        let events = self.events.lock();
        let mut receipts = self.receipts.lock();

        let mut best_pair = None;
        let mut min_diff = i64::MAX;

        for (receipt_idx, (r_hash, r_len, receipt, reserved)) in receipts.iter().enumerate() {
            if *reserved {
                continue;
            }
            for (_at, event) in events.iter() {
                if r_hash.eq_ignore_ascii_case(&event.content_hash) && *r_len == event.content_length {
                    if let Ok(event_ts_ms) = event.timestamp_millis() {
                        let diff = (receipt.copy_timestamp - event_ts_ms).abs();
                        if diff <= allowed_delta_ms && diff < min_diff {
                            min_diff = diff;
                            best_pair = Some((receipt_idx, event.clone(), receipt.clone()));
                        }
                    }
                }
            }
        }

        if let Some((idx, event, receipt)) = best_pair {
            receipts[idx].3 = true;
            Some((event, receipt))
        } else {
            None
        }
    }

    pub fn acknowledge_pair(&self, event_id: Uuid, receipt_id: Uuid) {
        let mut events = self.events.lock();
        if let Some(pos) = events.iter().position(|(_, e)| e.event_id == event_id) {
            events.remove(pos);
        }
        drop(events);

        let mut receipts = self.receipts.lock();
        if let Some(pos) = receipts.iter().position(|(_, _, r, _)| r.receipt_id == receipt_id) {
            receipts.remove(pos);
        }
    }

    pub fn discard_pair(&self, event_id: Uuid, receipt_id: Uuid) {
        self.acknowledge_pair(event_id, receipt_id);
    }

    pub fn release_receipt(&self, receipt_id: Uuid) {
        let mut receipts = self.receipts.lock();
        if let Some((_, _, _, reserved)) =
            receipts.iter_mut().find(|(_, _, r, _)| r.receipt_id == receipt_id)
        {
            *reserved = false;
        }
    }

    pub fn reconcile_pending(
        &self,
        repo: &crate::persistence::Repository,
        app: Option<&tauri::AppHandle>,
    ) {
        while let Some((event, receipt)) = self.reserve_matching_pair(RECEIPT_MATCH_WINDOW_MS) {
            let receipt_id = receipt.receipt_id;
            let event_id = event.event_id;
            let hash = event.content_hash.clone();
            match repo.attach_metadata_with_receipt(&event, receipt) {
                Ok(Some(clip_id)) => {
                    self.acknowledge_pair(event_id, receipt_id);
                    log::info!("Late reconciliation: metadata attached to clip {clip_id}");
                    if let Some(app) = app {
                        let _ = app.emit("clips-changed", ());
                    }
                }
                Ok(None) => {
                    log::debug!(
                        "Late reconciliation: receipt/event pair no longer valid for hash {hash}; discarding pair"
                    );
                    self.discard_pair(event_id, receipt_id);
                }
                Err(e) => {
                    log::warn!(
                        "Late reconciliation error for hash {hash}: {e}; releasing receipt reservation"
                    );
                    self.release_receipt(receipt_id);
                    break;
                }
            }
        }
    }

    pub fn take_matching_receipt(
        &self,
        hash: &str,
        length: usize,
        browser_event_ts_ms: i64,
        allowed_delta_ms: i64,
    ) -> Option<ClipUpsertReceipt> {
        let mut receipts = self.receipts.lock();
        let mut best_idx = None;
        let mut min_diff = i64::MAX;

        for (idx, (h, l, r, reserved)) in receipts.iter().enumerate() {
            if !*reserved && h.eq_ignore_ascii_case(hash) && *l == length {
                let diff = (r.copy_timestamp - browser_event_ts_ms).abs();
                if diff <= allowed_delta_ms && diff < min_diff {
                    min_diff = diff;
                    best_idx = Some(idx);
                }
            }
        }

        if let Some(pos) = best_idx {
            receipts.remove(pos).map(|(_, _, r, _)| r)
        } else {
            None
        }
    }

    pub fn cleanup_stale(&self, now: DateTime<Utc>) {
        let now_ms = now.timestamp_millis();

        let mut events = self.events.lock();
        events.retain(|(_, e)| {
            if let Ok(ts) = e.timestamp_millis() {
                (now_ms - ts).abs() <= 10_000
            } else {
                false
            }
        });

        let mut receipts = self.receipts.lock();
        receipts.retain(|(_, _, r, reserved)| *reserved || (now_ms - r.copy_timestamp).abs() <= 10_000);
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("native.sock")
}

/// Start the Native Messaging Unix socket server.
pub fn start_socket_server(
    path: PathBuf,
    buffer: Arc<MetadataBuffer>,
    reconcile_callback: Arc<dyn Fn(BrowserCopyEvent) + Send + Sync>,
) -> Result<()> {
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => {
                anyhow::bail!(
                    "Native Messaging socket {:?} уже занят другим активным процессом",
                    path
                );
            }
            Err(_) => {
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
            event_id: Uuid::new_v4(),
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
    fn receipt_matching_uses_timestamp_delta_and_picks_closest() {
        let b = MetadataBuffer::default();
        let hash = "a".repeat(64);
        let now_ms = Utc::now().timestamp_millis();

        let r1 = ClipUpsertReceipt {
            receipt_id: Uuid::new_v4(),
            clip_id: "clip_1".into(),
            content_hash: hash.clone(),
            normalized_length_bytes: 10,
            previous_last_copied_at: Some(now_ms - 5000),
            previous_sort_key: Some(10),
            previous_copy_count: 1,
            resulting_last_copied_at: now_ms - 1500,
            resulting_sort_key: 11,
            resulting_copy_count: 2,
            copy_timestamp: now_ms - 1500,
        };

        let r2 = ClipUpsertReceipt {
            receipt_id: Uuid::new_v4(),
            clip_id: "clip_2".into(),
            content_hash: hash.clone(),
            normalized_length_bytes: 10,
            previous_last_copied_at: Some(now_ms - 3000),
            previous_sort_key: Some(20),
            previous_copy_count: 1,
            resulting_last_copied_at: now_ms - 300,
            resulting_sort_key: 21,
            resulting_copy_count: 2,
            copy_timestamp: now_ms - 300,
        };

        b.push_receipt(&hash, 10, r1);
        b.push_receipt(&hash, 10, r2);

        // Matching for event at now_ms - 200 should pick r2 (delta 100ms vs 1300ms)
        let matched = b
            .take_matching_receipt(&hash, 10, now_ms - 200, RECEIPT_MATCH_WINDOW_MS)
            .expect("should match r2");

        assert_eq!(matched.clip_id, "clip_2");

        // Subsequent match for event at now_ms - 1400 should pick r1
        let matched2 = b
            .take_matching_receipt(&hash, 10, now_ms - 1400, RECEIPT_MATCH_WINDOW_MS)
            .expect("should match r1");

        assert_eq!(matched2.clip_id, "clip_1");
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
        let _ = UnixListener::bind(&path).unwrap();
        drop(UnixListener::bind(&path));
        assert!(path.exists());
        let buffer = Arc::new(MetadataBuffer::default());
        let cb = Arc::new(|_: BrowserCopyEvent| {});
        let res = start_socket_server(path.clone(), buffer, cb);
        assert!(res.is_ok());
    }
}
