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

use std::sync::atomic::{AtomicI64, Ordering};

static LAST_EXTENSION_HANDSHAKE_AT: AtomicI64 = AtomicI64::new(0);

static LAST_BROWSER_COPY_METADATA_AT: AtomicI64 = AtomicI64::new(0);

pub const HANDSHAKE_TTL_MS: i64 = 45_000;

pub fn record_extension_handshake_received() {
    LAST_EXTENSION_HANDSHAKE_AT.store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
}

pub fn record_copy_metadata_received() {
    LAST_BROWSER_COPY_METADATA_AT.store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
}

pub fn get_last_extension_handshake_at() -> Option<i64> {
    let ts = LAST_EXTENSION_HANDSHAKE_AT.load(Ordering::Relaxed);
    if ts > 0 {
        Some(ts)
    } else {
        None
    }
}

pub fn get_last_browser_copy_metadata_at() -> Option<i64> {
    let ts = LAST_BROWSER_COPY_METADATA_AT.load(Ordering::Relaxed);
    if ts > 0 {
        Some(ts)
    } else {
        None
    }
}

pub fn get_last_message_at() -> Option<i64> {
    match (
        get_last_extension_handshake_at(),
        get_last_browser_copy_metadata_at(),
    ) {
        (Some(h), Some(c)) => Some(h.max(c)),
        (Some(h), None) => Some(h),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

pub fn is_handshake_active() -> bool {
    let now = chrono::Utc::now().timestamp_millis();
    let handshake_recent = get_last_extension_handshake_at()
        .map(|ts| (now - ts).abs() <= HANDSHAKE_TTL_MS)
        .unwrap_or(false);
    let copy_recent = get_last_browser_copy_metadata_at()
        .map(|ts| (now - ts).abs() <= HANDSHAKE_TTL_MS)
        .unwrap_or(false);
    handshake_recent || copy_recent
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
    pub previous_content: Option<String>,
    pub resulting_last_copied_at: i64,
    pub resulting_sort_key: i64,
    pub resulting_copy_count: i64,
    pub copy_timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct BufferedEvent {
    pub at: DateTime<Utc>,
    pub event: BrowserCopyEvent,
    pub reserved: bool,
}

#[derive(Default)]
pub struct MetadataBuffer {
    events: Mutex<VecDeque<BufferedEvent>>,
    receipts: Mutex<VecDeque<(String, usize, ClipUpsertReceipt, bool)>>,
}

impl MetadataBuffer {
    pub fn push(&self, event: BrowserCopyEvent) -> Result<()> {
        let event = event.validate()?;
        let at = DateTime::parse_from_rfc3339(&event.timestamp)?.with_timezone(&Utc);
        record_copy_metadata_received();
        let mut events = self.events.lock();
        events.push_back(BufferedEvent {
            at,
            event,
            reserved: false,
        });
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
        events.retain(|e| {
            e.reserved || now.signed_duration_since(e.at).num_milliseconds().abs() <= 10_000
        });
        let pos = events.iter().rposition(|e| {
            !e.reserved
                && e.event.content_hash.eq_ignore_ascii_case(hash)
                && e.event.content_length == length
                && now.signed_duration_since(e.at).num_milliseconds().abs() <= 2_500
        })?;
        events.remove(pos).map(|e| e.event)
    }

    pub fn remove_event(&self, event_id: Uuid) {
        let mut events = self.events.lock();
        if let Some(pos) = events.iter().position(|e| e.event.event_id == event_id) {
            events.remove(pos);
        }
    }

    pub fn release_event(&self, event_id: Uuid) {
        let mut events = self.events.lock();
        if let Some(e) = events.iter_mut().find(|e| e.event.event_id == event_id) {
            e.reserved = false;
        }
    }

    pub fn remove_receipt(&self, receipt_id: Uuid) {
        let mut receipts = self.receipts.lock();
        if let Some(pos) = receipts
            .iter()
            .position(|(_, _, r, _)| r.receipt_id == receipt_id)
        {
            receipts.remove(pos);
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
        let mut events = self.events.lock();
        let mut receipts = self.receipts.lock();

        let mut best_pair: Option<(usize, usize, BrowserCopyEvent, ClipUpsertReceipt)> = None;
        let mut min_diff = i64::MAX;

        for (receipt_idx, (r_hash, r_len, receipt, r_reserved)) in receipts.iter().enumerate() {
            if *r_reserved {
                continue;
            }
            for (event_idx, buffered) in events.iter().enumerate() {
                if buffered.reserved {
                    continue;
                }
                let event = &buffered.event;
                if r_hash.eq_ignore_ascii_case(&event.content_hash)
                    && *r_len == event.content_length
                {
                    if let Ok(event_ts_ms) = event.timestamp_millis() {
                        let diff = (receipt.copy_timestamp - event_ts_ms).abs();
                        if diff <= allowed_delta_ms && diff < min_diff {
                            min_diff = diff;
                            best_pair =
                                Some((event_idx, receipt_idx, event.clone(), receipt.clone()));
                        }
                    }
                }
            }
        }

        if let Some((e_idx, r_idx, event, receipt)) = best_pair {
            events[e_idx].reserved = true;
            receipts[r_idx].3 = true;
            Some((event, receipt))
        } else {
            None
        }
    }

    pub fn acknowledge_pair(&self, event_id: Uuid, receipt_id: Uuid) {
        self.remove_event(event_id);
        self.remove_receipt(receipt_id);
    }

    pub fn discard_pair(&self, event_id: Uuid, receipt_id: Uuid) {
        self.acknowledge_pair(event_id, receipt_id);
    }

    pub fn release_receipt(&self, receipt_id: Uuid) {
        let mut receipts = self.receipts.lock();
        if let Some((_, _, _, reserved)) = receipts
            .iter_mut()
            .find(|(_, _, r, _)| r.receipt_id == receipt_id)
        {
            *reserved = false;
        }
    }

    pub fn reconcile_pending(
        &self,
        repo: &crate::persistence::Repository,
        notify: Option<&dyn Fn()>,
    ) {
        while let Some((event, receipt)) = self.reserve_matching_pair(RECEIPT_MATCH_WINDOW_MS) {
            let receipt_id = receipt.receipt_id;
            let event_id = event.event_id;
            let hash = event.content_hash.clone();
            match repo.attach_metadata_with_receipt(&event, receipt) {
                Ok(Some(clip_id)) => {
                    self.acknowledge_pair(event_id, receipt_id);
                    log::info!("Late reconciliation: metadata attached to clip {clip_id}");
                    if let Some(notify) = notify {
                        notify();
                    }
                }
                Ok(None) => {
                    log::debug!(
                        "Late reconciliation: receipt no longer valid for hash {hash}; removing receipt and unreserving event"
                    );
                    self.remove_receipt(receipt_id);
                    self.release_event(event_id);
                }
                Err(e) => {
                    log::warn!(
                        "Late reconciliation error for hash {hash}: {e}; releasing reservations"
                    );
                    self.release_receipt(receipt_id);
                    self.release_event(event_id);
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
        events.retain(|e| {
            e.reserved || (now_ms - e.event.timestamp_millis().unwrap_or(0)).abs() <= 10_000
        });

        let mut receipts = self.receipts.lock();
        receipts
            .retain(|(_, _, r, reserved)| *reserved || (now_ms - r.copy_timestamp).abs() <= 10_000);
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
                Ok(s) => {
                    let buffer = Arc::clone(&buffer);
                    let reconcile_callback = Arc::clone(&reconcile_callback);
                    std::thread::spawn(move || {
                        read_stream(s, &buffer, &*reconcile_callback);
                    });
                }
                Err(e) => log::warn!("native socket: {e}"),
            }
        }
    });
    Ok(())
}

fn read_stream(
    mut stream: UnixStream,
    buffer: &MetadataBuffer,
    reconcile: &(impl Fn(BrowserCopyEvent) + ?Sized),
) {
    use std::io::Write;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in BufReader::new(reader_stream).lines().take(8) {
        match line {
            Ok(line) if line.len() <= 16_384 => {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    let event_type = val.get("event").and_then(|v| v.as_str()).unwrap_or("");
                    if event_type == "status" {
                        record_extension_handshake_received();
                        log::info!("Extension handshake accepted via Native Host socket");
                        let _ = stream.write_all(b"{\"ok\":true,\"accepted\":true}\n");
                        let _ = stream.flush();
                    } else if event_type == "copy" {
                        match serde_json::from_value::<BrowserCopyEvent>(val) {
                            Ok(event) => {
                                let hash_prefix = if event.content_hash.len() >= 8 {
                                    &event.content_hash[..8]
                                } else {
                                    &event.content_hash
                                };
                                log::info!(
                                    "Browser copy event accepted: id={}, domain={}, hash_prefix={}, len={}",
                                    event.event_id, event.domain, hash_prefix, event.content_length
                                );
                                match buffer.push(event.clone()) {
                                    Ok(()) => {
                                        reconcile(event);
                                        let _ = stream.write_all(b"{\"ok\":true,\"accepted\":true}\n");
                                        let _ = stream.flush();
                                    }
                                    Err(e) => {
                                        log::warn!("Отклонено событие Chrome: {e}");
                                        let _ = stream.write_all(b"{\"ok\":false,\"error\":\"validation_failed\"}\n");
                                        let _ = stream.flush();
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("Отклонён некорректный JSON copy Chrome: {e}");
                                let _ = stream.write_all(b"{\"ok\":false,\"error\":\"invalid_payload\"}\n");
                                let _ = stream.flush();
                            }
                        }
                    } else {
                        log::warn!("Неизвестный тип события Chrome: {event_type}");
                        let _ = stream.write_all(b"{\"ok\":false,\"error\":\"unknown_event\"}\n");
                        let _ = stream.flush();
                    }
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
            previous_content: None,
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
            previous_content: None,
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

    #[test]
    fn reservation_locks_both_event_and_receipt_and_blocks_take_match() {
        let b = MetadataBuffer::default();
        let hash = "a".repeat(64);
        let now = Utc::now();
        let now_ms = now.timestamp_millis();

        let e = BrowserCopyEvent {
            event_id: Uuid::new_v4(),
            version: 1,
            event: "copy".into(),
            content_hash: hash.clone(),
            content_length: 5,
            domain: "example.com".into(),
            page_title: "Title".into(),
            timestamp: now.to_rfc3339(),
        };

        let r = ClipUpsertReceipt {
            receipt_id: Uuid::new_v4(),
            clip_id: "clip_1".into(),
            content_hash: hash.clone(),
            normalized_length_bytes: 5,
            previous_last_copied_at: None,
            previous_sort_key: None,
            previous_copy_count: 0,
            previous_content: None,
            resulting_last_copied_at: now_ms,
            resulting_sort_key: 1,
            resulting_copy_count: 1,
            copy_timestamp: now_ms,
        };

        b.push(e.clone()).unwrap();
        b.push_receipt(&hash, 5, r.clone());

        // Reserving pair should lock both
        let (reserved_event, reserved_receipt) = b
            .reserve_matching_pair(RECEIPT_MATCH_WINDOW_MS)
            .expect("should reserve pair");

        assert_eq!(reserved_event.event_id, e.event_id);
        assert_eq!(reserved_receipt.receipt_id, r.receipt_id);

        // Reserved event cannot be stolen by take_match
        assert!(b.take_match(&hash, 5, now).is_none());

        // Subsequent reserve_matching_pair returns None because pair is reserved
        assert!(b.reserve_matching_pair(RECEIPT_MATCH_WINDOW_MS).is_none());

        // Releasing event reservation makes it available for take_match again
        b.release_event(e.event_id);
        let matched = b
            .take_match(&hash, 5, now)
            .expect("should take match after release");
        assert_eq!(matched.event_id, e.event_id);
    }

    #[test]
    fn unreserving_event_keeps_it_for_next_receipt_on_stale_receipt_cleanup() {
        let b = MetadataBuffer::default();
        let hash = "a".repeat(64);
        let now = Utc::now();
        let now_ms = now.timestamp_millis();

        let e = BrowserCopyEvent {
            event_id: Uuid::new_v4(),
            version: 1,
            event: "copy".into(),
            content_hash: hash.clone(),
            content_length: 5,
            domain: "example.com".into(),
            page_title: "Title".into(),
            timestamp: now.to_rfc3339(),
        };

        let r_stale = ClipUpsertReceipt {
            receipt_id: Uuid::new_v4(),
            clip_id: "clip_stale".into(),
            content_hash: hash.clone(),
            normalized_length_bytes: 5,
            previous_last_copied_at: None,
            previous_sort_key: None,
            previous_copy_count: 0,
            previous_content: None,
            resulting_last_copied_at: now_ms - 100,
            resulting_sort_key: 1,
            resulting_copy_count: 1,
            copy_timestamp: now_ms - 100,
        };

        b.push(e.clone()).unwrap();
        b.push_receipt(&hash, 5, r_stale.clone());

        let (reserved_event, reserved_receipt) =
            b.reserve_matching_pair(RECEIPT_MATCH_WINDOW_MS).unwrap();

        // Simulate stale receipt handling (Ok(None)): remove receipt, release event reservation
        b.remove_receipt(reserved_receipt.receipt_id);
        b.release_event(reserved_event.event_id);

        // Push new correct receipt
        let r_fresh = ClipUpsertReceipt {
            receipt_id: Uuid::new_v4(),
            clip_id: "clip_fresh".into(),
            content_hash: hash.clone(),
            normalized_length_bytes: 5,
            previous_last_copied_at: None,
            previous_sort_key: None,
            previous_copy_count: 0,
            previous_content: None,
            resulting_last_copied_at: now_ms,
            resulting_sort_key: 2,
            resulting_copy_count: 1,
            copy_timestamp: now_ms,
        };
        b.push_receipt(&hash, 5, r_fresh.clone());

        // Event should match with fresh receipt
        let (e2, r2) = b.reserve_matching_pair(RECEIPT_MATCH_WINDOW_MS).unwrap();
        assert_eq!(e2.event_id, e.event_id);
        assert_eq!(r2.receipt_id, r_fresh.receipt_id);
    }

    #[test]
    fn copy_event_activates_connection_status_and_socket_acknowledges() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test_ack.sock");
        let buffer = Arc::new(MetadataBuffer::default());
        let cb = Arc::new(|_: BrowserCopyEvent| {});

        start_socket_server(sock_path.clone(), buffer, cb).expect("start server");

        let mut client = UnixStream::connect(&sock_path).expect("connect to server");
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // 1. Send status probe
        let status_json = serde_json::json!({
            "version": 1,
            "event": "status",
            "timestamp": Utc::now().to_rfc3339()
        }).to_string() + "\n";

        client.write_all(status_json.as_bytes()).unwrap();
        client.flush().unwrap();

        let mut reader = BufReader::new(&client);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).unwrap();

        let resp: serde_json::Value = serde_json::from_str(&resp_line).unwrap();
        assert_eq!(resp.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(resp.get("accepted").and_then(|v| v.as_bool()), Some(true));

        // 2. Send copy event
        let copy_json = serde_json::json!({
            "eventId": Uuid::new_v4().to_string(),
            "version": 1,
            "event": "copy",
            "contentHash": "a".repeat(64),
            "contentLength": 10,
            "domain": "example.com",
            "pageTitle": "Test",
            "timestamp": Utc::now().to_rfc3339()
        }).to_string() + "\n";

        let mut client2 = UnixStream::connect(&sock_path).expect("connect client 2");
        client2.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        client2.write_all(copy_json.as_bytes()).unwrap();
        client2.flush().unwrap();

        let mut reader2 = BufReader::new(&client2);
        let mut resp_line2 = String::new();
        reader2.read_line(&mut resp_line2).unwrap();

        let resp2: serde_json::Value = serde_json::from_str(&resp_line2).unwrap();
        assert_eq!(resp2.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(resp2.get("accepted").and_then(|v| v.as_bool()), Some(true));

        // Verify connection active
        assert!(is_handshake_active());
    }

    #[test]
    fn socket_server_rejects_invalid_copy_payload() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test_invalid.sock");
        let buffer = Arc::new(MetadataBuffer::default());
        let cb = Arc::new(|_: BrowserCopyEvent| {});

        start_socket_server(sock_path.clone(), buffer, cb).unwrap();

        let mut client = UnixStream::connect(&sock_path).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // Send invalid copy payload (bad hash)
        let invalid_copy = serde_json::json!({
            "version": 1,
            "event": "copy",
            "contentHash": "short",
            "contentLength": 10,
            "domain": "example.com",
            "pageTitle": "Test",
            "timestamp": Utc::now().to_rfc3339()
        }).to_string() + "\n";

        client.write_all(invalid_copy.as_bytes()).unwrap();
        client.flush().unwrap();

        let mut reader = BufReader::new(&client);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).unwrap();

        let resp: serde_json::Value = serde_json::from_str(&resp_line).unwrap();
        assert_eq!(resp.get("ok").and_then(|v| v.as_bool()), Some(false));
    }
}
