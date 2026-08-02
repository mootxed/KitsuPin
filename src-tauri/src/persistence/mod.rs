use crate::{
    blob_store::BlobStore,
    domain::{
        classify, content_hash, normalize_content, normalize_domain, Category, ClipQuery,
        ClipSummary, ClipboardCopy, ClipboardPayload, ContentType, ImageMetadata, NewClip,
        NewImageClip, PayloadKind, StorageStats,
    },
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{functions::FunctionFlags, params, Connection, Row};
use std::{collections::HashSet, path::Path};
use uuid::Uuid;

const MIGRATION_1: &str = include_str!("../../migrations/001_initial.sql");

/// Window within which Chrome metadata can be reconciled to a clip (milliseconds).
#[allow(dead_code)]
pub const METADATA_RECONCILE_WINDOW_MS: i64 = 5_000;

type PriorClipState = (String, Option<i64>, Option<i64>, i64, String);

pub struct Repository {
    connection: Mutex<Connection>,
    blob_store: Option<BlobStore>,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path).context("не удалось открыть SQLite")?;
        configure_connection(&connection)?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        let _: String = connection.query_row("PRAGMA journal_mode=WAL;", [], |r| r.get(0))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path.exists() {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .context("не удалось установить права 0600 на БД")?;
            }
            if let Some(parent) = path.parent() {
                let db_name = path.file_name().unwrap_or_default().to_string_lossy();
                let wal = parent.join(format!("{db_name}-wal"));
                if wal.exists() {
                    std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600))
                        .context("не удалось установить права 0600 на WAL-файл")?;
                }
                let shm = parent.join(format!("{db_name}-shm"));
                if shm.exists() {
                    std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o600))
                        .context("не удалось установить права 0600 на SHM-файл")?;
                }
            }
        }
        let repo = Self {
            connection: Mutex::new(connection),
            blob_store: Some(BlobStore::new(
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("blobs"),
            )?),
        };
        repo.migrate()?;
        Ok(repo)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        configure_connection(&connection)?;
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        let repo = Self {
            connection: Mutex::new(connection),
            blob_store: None,
        };
        repo.migrate()?;
        Ok(repo)
    }

    #[cfg(test)]
    pub fn open_in_memory_with_blobs(blob_dir: &Path) -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        configure_connection(&connection)?;
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        let repo = Self {
            connection: Mutex::new(connection),
            blob_store: Some(BlobStore::new(blob_dir.to_path_buf())?),
        };
        repo.migrate()?;
        Ok(repo)
    }

    fn migrate(&self) -> Result<()> {
        let mut db = self.connection.lock();
        let tx = db.transaction()?;
        self.run_migrations(&tx)?;
        tx.commit()?;
        Ok(())
    }

    fn run_migrations(&self, tx: &rusqlite::Transaction) -> Result<()> {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);")?;

        // ── Migration 1: initial schema ───────────────────────────────────
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=1)",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            let clips_exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='clips')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !clips_exists {
                tx.execute_batch(MIGRATION_1)?;
            }
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(1, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 2: domain_key column + hash+domain_key dedup ────────
        let exists_2: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=2)",
            [],
            |r| r.get(0),
        )?;
        if !exists_2 {
            let has_domain_key: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('clips') WHERE name='domain_key')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !has_domain_key {
                tx.execute_batch(
                    "ALTER TABLE clips ADD COLUMN domain_key TEXT NOT NULL DEFAULT '';",
                )?;
            }
            tx.execute_batch("UPDATE clips SET domain_key = COALESCE(domain, '');")?;
            let has_norm: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('clips') WHERE name='normalized_content')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !has_norm {
                tx.execute_batch(
                    "DELETE FROM clips WHERE rowid NOT IN (
                        SELECT min(rowid) FROM clips GROUP BY content_hash, domain_key
                    );",
                )?;
                tx.execute_batch(
                    "CREATE UNIQUE INDEX IF NOT EXISTS idx_clips_hash_domain_key ON clips(content_hash, domain_key);",
                )?;
            }
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(2, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 3: drop normalized_content if it exists ─────────────
        // NOTE: On databases with UNIQUE(normalized_content, domain), ALTER TABLE DROP COLUMN
        // fails because the column is part of a constraint. Migration 6 handles this case
        // via a full table rebuild. We still attempt DROP COLUMN here for simpler cases
        // (no constraint), but skip silently if it fails — migration 6 will fix it.
        let exists_3: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=3)",
            [],
            |r| r.get(0),
        )?;
        if !exists_3 {
            let has_norm: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('clips') WHERE name='normalized_content')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if has_norm {
                // Try DROP COLUMN; if it fails (UNIQUE constraint), migration 6 will rebuild.
                let _ = tx.execute_batch("ALTER TABLE clips DROP COLUMN normalized_content;");
            }
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(3, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 4: FTS5 + triggers ──────────────────────────────────
        let exists_4: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=4)",
            [],
            |r| r.get(0),
        )?;
        if !exists_4 {
            tx.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(
                    content,
                    page_title,
                    content='clips',
                    tokenize='trigram'
                );
                INSERT OR IGNORE INTO clips_fts(rowid, content, page_title)
                SELECT rowid, content, COALESCE(page_title, '') FROM clips;

                CREATE TRIGGER IF NOT EXISTS clips_ai AFTER INSERT ON clips BEGIN
                  INSERT INTO clips_fts(rowid, content, page_title) VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                END;

                CREATE TRIGGER IF NOT EXISTS clips_ad AFTER DELETE ON clips BEGIN
                  INSERT INTO clips_fts(clips_fts, rowid, content, page_title) VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                END;

                CREATE TRIGGER IF NOT EXISTS clips_au AFTER UPDATE ON clips BEGIN
                  INSERT INTO clips_fts(clips_fts, rowid, content, page_title) VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                  INSERT INTO clips_fts(rowid, content, page_title) VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                END;",
            )?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(4, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 5: TEXT→INTEGER timestamps ──────────────────────────
        // Handles RFC3339 TEXT timestamps. INTEGER seconds are handled in migration 6.
        let exists_5: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=5)",
            [],
            |r| r.get(0),
        )?;
        if !exists_5 {
            tx.execute_batch(&format!(
                "UPDATE clips SET created_at = CASE WHEN typeof(created_at)='text' THEN COALESCE(unixepoch(created_at)*1000, {now}) ELSE created_at END,
                                  last_copied_at = CASE WHEN typeof(last_copied_at)='text' THEN COALESCE(unixepoch(last_copied_at)*1000, {now}) ELSE last_copied_at END;
                 UPDATE user_categories SET created_at = CASE WHEN typeof(created_at)='text' THEN COALESCE(unixepoch(created_at)*1000, {now}) ELSE created_at END;
                 UPDATE clip_user_categories SET created_at = CASE WHEN typeof(created_at)='text' THEN COALESCE(unixepoch(created_at)*1000, {now}) ELSE created_at END;",
                now = chrono::Utc::now().timestamp_millis()
            ))?;
            tx.execute_batch(
                "DROP INDEX IF EXISTS idx_clips_recency;
                 DROP INDEX IF EXISTS idx_clips_type_recency;
                 DROP INDEX IF EXISTS idx_clips_domain_recency;
                 CREATE INDEX IF NOT EXISTS idx_clips_recency ON clips(last_copied_at DESC, sort_key DESC);
                 CREATE INDEX IF NOT EXISTS idx_clips_type_recency ON clips(content_type, last_copied_at DESC, sort_key DESC);
                 CREATE INDEX IF NOT EXISTS idx_clips_domain_recency ON clips(domain, last_copied_at DESC, sort_key DESC);"
            )?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(5, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 6: Full safe table rebuild ──────────────────────────
        // Fixes:
        //   - normalized_content column stuck under UNIQUE constraint (DROP COLUMN failed in m3)
        //   - INTEGER-second timestamps treated as ms (pre-2001 dates)
        //   - Duplicate merging that lost pinned/categories/copy_count
        // Safe: runs inside existing transaction with PRAGMA foreign_keys=OFF.
        // On error: whole outer transaction rolls back, DB remains usable.
        let exists_6: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=6)",
            [],
            |r| r.get(0),
        )?;
        if !exists_6 {
            // Check whether normalized_content still exists (migration 3 DROP COLUMN may have failed).
            let has_norm: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('clips') WHERE name='normalized_content')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);

            if has_norm {
                // Full rebuild is required to remove the column and its constraints.
                log::info!(
                    "Migration 6: normalized_content still present — performing full table rebuild"
                );
                // Disable FK checks for the rebuild (we re-enable and check after).
                tx.execute_batch("PRAGMA foreign_keys=OFF;")?;

                // Threshold: timestamps smaller than Jan 1 2000 00:00:00 UTC in ms
                // are assumed to be in seconds and multiplied by 1000.
                const SECONDS_THRESHOLD: i64 = 946_684_800_000; // 2000-01-01 in ms

                // Create new clips table and new clip_user_categories table with current schema.
                tx.execute_batch(
                    "CREATE TABLE clips_new (
                        id TEXT PRIMARY KEY NOT NULL,
                        content TEXT NOT NULL,
                        content_hash TEXT NOT NULL,
                        content_type TEXT NOT NULL CHECK (content_type IN ('Text','Links','Email','Numbers')),
                        domain TEXT,
                        domain_key TEXT NOT NULL DEFAULT '',
                        page_title TEXT,
                        created_at INTEGER NOT NULL,
                        last_copied_at INTEGER NOT NULL,
                        copy_count INTEGER NOT NULL DEFAULT 1 CHECK (copy_count >= 1),
                        pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
                        sort_key INTEGER NOT NULL DEFAULT 0,
                        UNIQUE(content_hash, domain_key)
                    );
                    CREATE TABLE clip_user_categories_new (
                        clip_id TEXT NOT NULL REFERENCES clips_new(id) ON DELETE CASCADE,
                        category_id TEXT NOT NULL REFERENCES user_categories(id) ON DELETE CASCADE,
                        created_at INTEGER NOT NULL,
                        PRIMARY KEY (clip_id, category_id)
                    );",
                )?;

                // Migrate and deduplicate rows.
                tx.execute_batch(&format!(
                    "INSERT INTO clips_new(
                        id, content, content_hash, content_type,
                        domain, domain_key, page_title,
                        created_at, last_copied_at, copy_count, pinned, sort_key
                    )
                    SELECT
                        canonical_id,
                        canonical_content,
                        content_hash,
                        content_type,
                        domain,
                        COALESCE(domain, ''),
                        latest_title,
                        CASE
                            WHEN typeof(min_created)='text' THEN COALESCE(unixepoch(min_created)*1000, {fallback})
                            WHEN min_created < {threshold} AND min_created > 0 THEN min_created * 1000
                            ELSE min_created
                        END,
                        CASE
                            WHEN typeof(max_last)='text' THEN COALESCE(unixepoch(max_last)*1000, {fallback})
                            WHEN max_last < {threshold} AND max_last > 0 THEN max_last * 1000
                            ELSE max_last
                        END,
                        MIN(total_count, 2147483647),
                        max_pinned,
                        max_sort_key
                    FROM (
                        SELECT
                            content_hash,
                            COALESCE(domain, '') AS domain,
                            content_type,
                            MAX(pinned)  AS max_pinned,
                            MAX(sort_key) AS max_sort_key,
                            MIN(created_at) AS min_created,
                            MAX(last_copied_at) AS max_last,
                            TOTAL(copy_count) AS total_count,
                            (SELECT id FROM clips c2
                             WHERE c2.content_hash = clips.content_hash
                               AND COALESCE(c2.domain, '') = COALESCE(clips.domain, '')
                             ORDER BY c2.created_at ASC, c2.rowid ASC
                             LIMIT 1) AS canonical_id,
                            (SELECT content FROM clips c2
                             WHERE c2.content_hash = clips.content_hash
                               AND COALESCE(c2.domain, '') = COALESCE(clips.domain, '')
                             ORDER BY c2.created_at ASC, c2.rowid ASC
                             LIMIT 1) AS canonical_content,
                            (SELECT page_title FROM clips c2
                             WHERE c2.content_hash = clips.content_hash
                               AND COALESCE(c2.domain, '') = COALESCE(clips.domain, '')
                               AND c2.page_title IS NOT NULL AND c2.page_title != ''
                             ORDER BY c2.last_copied_at DESC, c2.rowid DESC
                             LIMIT 1) AS latest_title
                        FROM clips
                        GROUP BY content_hash, COALESCE(domain, '')
                    ) grouped;",
                    threshold = SECONDS_THRESHOLD,
                    fallback = chrono::Utc::now().timestamp_millis(),
                ))?;

                // Transfer category associations into clip_user_categories_new.
                tx.execute_batch(
                    "INSERT OR IGNORE INTO clip_user_categories_new(clip_id, category_id, created_at)
                     SELECT
                         (SELECT id FROM clips_new WHERE clips_new.content_hash = clips.content_hash
                          AND clips_new.domain_key = COALESCE(clips.domain, '')
                          LIMIT 1),
                         clip_user_categories.category_id,
                         clip_user_categories.created_at
                     FROM clip_user_categories
                     JOIN clips ON clips.id = clip_user_categories.clip_id
                     WHERE (SELECT id FROM clips_new WHERE clips_new.content_hash = clips.content_hash
                            AND clips_new.domain_key = COALESCE(clips.domain, '') LIMIT 1) IS NOT NULL;",
                )?;

                // Swap tables: drop old child table first, then old parent table.
                tx.execute_batch("DROP TABLE clip_user_categories;")?;
                tx.execute_batch("DROP TABLE clips;")?;
                tx.execute_batch(
                    "ALTER TABLE clip_user_categories_new RENAME TO clip_user_categories;",
                )?;
                tx.execute_batch("ALTER TABLE clips_new RENAME TO clips;")?;

                // Rebuild indices.
                tx.execute_batch(
                    "DROP INDEX IF EXISTS idx_clips_recency;
                     DROP INDEX IF EXISTS idx_clips_type_recency;
                     DROP INDEX IF EXISTS idx_clips_domain_recency;
                     DROP INDEX IF EXISTS idx_clips_hash_domain_key;
                     CREATE UNIQUE INDEX idx_clips_hash_domain_key ON clips(content_hash, domain_key);
                     CREATE INDEX idx_clips_recency ON clips(last_copied_at DESC, sort_key DESC);
                     CREATE INDEX idx_clips_type_recency ON clips(content_type, last_copied_at DESC, sort_key DESC);
                     CREATE INDEX idx_clips_domain_recency ON clips(domain, last_copied_at DESC, sort_key DESC);
                     CREATE INDEX IF NOT EXISTS idx_clip_categories_category ON clip_user_categories(category_id, clip_id);",
                )?;

                // Rebuild FTS if it exists.
                let has_fts: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='clips_fts')",
                    [],
                    |r| r.get(0),
                ).unwrap_or(false);
                if has_fts {
                    tx.execute_batch(
                        "DROP TRIGGER IF EXISTS clips_ai;
                         DROP TRIGGER IF EXISTS clips_ad;
                         DROP TRIGGER IF EXISTS clips_au;
                         INSERT INTO clips_fts(clips_fts) VALUES('rebuild');",
                    )?;
                    tx.execute_batch(
                        "CREATE TRIGGER clips_ai AFTER INSERT ON clips BEGIN
                           INSERT INTO clips_fts(rowid, content, page_title)
                           VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                         END;
                         CREATE TRIGGER clips_ad AFTER DELETE ON clips BEGIN
                           INSERT INTO clips_fts(clips_fts, rowid, content, page_title)
                           VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                         END;
                         CREATE TRIGGER clips_au AFTER UPDATE OF content, page_title ON clips BEGIN
                           INSERT INTO clips_fts(clips_fts, rowid, content, page_title)
                           VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                           INSERT INTO clips_fts(rowid, content, page_title)
                           VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                         END;",
                    )?;
                }

                // Verify foreign keys correctly using stmt.exists.
                let mut fk_stmt = tx.prepare("PRAGMA foreign_key_check")?;
                if fk_stmt.exists([])? {
                    anyhow::bail!("Migration 6: PRAGMA foreign_key_check failed after rebuild");
                }
            } else {
                // normalized_content already gone; just fix INTEGER-second timestamps.
                const SECONDS_THRESHOLD: i64 = 946_684_800_000;
                tx.execute_batch(&format!(
                    "UPDATE clips SET
                        created_at    = CASE WHEN created_at    > 0 AND created_at    < {t} THEN created_at    * 1000 ELSE created_at    END,
                        last_copied_at= CASE WHEN last_copied_at> 0 AND last_copied_at< {t} THEN last_copied_at* 1000 ELSE last_copied_at END;
                     UPDATE user_categories SET
                        created_at = CASE WHEN created_at > 0 AND created_at < {t} THEN created_at * 1000 ELSE created_at END;
                     UPDATE clip_user_categories SET
                        created_at = CASE WHEN created_at > 0 AND created_at < {t} THEN created_at * 1000 ELSE created_at END;",
                    t = SECONDS_THRESHOLD
                ))?;
            }

            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(6, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 7: FTS trigger only on indexed columns ─────────────
        // Fixes the overbroad clips_au that fired on every UPDATE (including
        // last_copied_at, copy_count, pinned, sort_key), causing unnecessary FTS writes.
        let exists_7: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=7)",
            [],
            |r| r.get(0),
        )?;
        if !exists_7 {
            let has_fts: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='clips_fts')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if has_fts {
                tx.execute_batch(
                    "DROP TRIGGER IF EXISTS clips_au;
                     CREATE TRIGGER clips_au AFTER UPDATE OF content, page_title ON clips BEGIN
                       INSERT INTO clips_fts(clips_fts, rowid, content, page_title)
                       VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                       INSERT INTO clips_fts(rowid, content, page_title)
                       VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                     END;",
                )?;
            }
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(7, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 8: FTS5 rebuild with trigram tokenizer + restore category index ──
        let exists_8: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=8)",
            [],
            |r| r.get(0),
        )?;
        if !exists_8 {
            tx.execute_batch(
                "DROP TRIGGER IF EXISTS clips_ai;
                 DROP TRIGGER IF EXISTS clips_ad;
                 DROP TRIGGER IF EXISTS clips_au;
                 DROP TABLE IF EXISTS clips_fts;

                 CREATE VIRTUAL TABLE clips_fts USING fts5(
                     content,
                     page_title,
                     content='clips',
                     tokenize='trigram'
                 );

                 INSERT INTO clips_fts(clips_fts) VALUES('rebuild');

                 CREATE TRIGGER clips_ai AFTER INSERT ON clips BEGIN
                   INSERT INTO clips_fts(rowid, content, page_title)
                   VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                 END;
                 CREATE TRIGGER clips_ad AFTER DELETE ON clips BEGIN
                   INSERT INTO clips_fts(clips_fts, rowid, content, page_title)
                   VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                 END;
                 CREATE TRIGGER clips_au AFTER UPDATE OF content, page_title ON clips BEGIN
                   INSERT INTO clips_fts(clips_fts, rowid, content, page_title)
                   VALUES('delete', old.rowid, old.content, COALESCE(old.page_title, ''));
                   INSERT INTO clips_fts(rowid, content, page_title)
                   VALUES (new.rowid, new.content, COALESCE(new.page_title, ''));
                 END;

                 CREATE INDEX IF NOT EXISTS idx_clip_categories_category ON clip_user_categories(category_id, clip_id);",
            )?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(8, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 9: legacy_imports ledger table ──────────────────────
        let exists_9: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=9)",
            [],
            |r| r.get(0),
        )?;
        if !exists_9 {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS legacy_imports (
                    id TEXT PRIMARY KEY,
                    source_fingerprint TEXT NOT NULL UNIQUE,
                    imported_at INTEGER NOT NULL,
                    source_path TEXT NOT NULL,
                    status TEXT NOT NULL
                );",
            )?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(9, datetime('now'))",
                [],
            )?;
        }

        // ── Migration 10: typed payloads + file-backed image blobs ────────
        let exists_10: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=10)",
            [],
            |r| r.get(0),
        )?;
        if !exists_10 {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS image_blobs (
                    hash TEXT PRIMARY KEY NOT NULL,
                    relative_path TEXT NOT NULL UNIQUE,
                    mime_type TEXT NOT NULL CHECK (mime_type IN ('image/png','image/jpeg','image/webp')),
                    width INTEGER NOT NULL CHECK (width > 0),
                    height INTEGER NOT NULL CHECK (height > 0),
                    size_bytes INTEGER NOT NULL CHECK (size_bytes > 0),
                    thumbnail_data_url TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                ALTER TABLE clips ADD COLUMN payload_kind TEXT NOT NULL DEFAULT 'text'
                    CHECK (payload_kind IN ('text','image'));
                ALTER TABLE clips ADD COLUMN blob_hash TEXT REFERENCES image_blobs(hash) ON DELETE RESTRICT;
                CREATE INDEX IF NOT EXISTS idx_clips_payload_recency
                    ON clips(payload_kind, last_copied_at DESC, sort_key DESC);
                CREATE INDEX IF NOT EXISTS idx_clips_blob_hash ON clips(blob_hash);",
            )?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(10, datetime('now'))",
                [],
            )?;
        }

        Ok(())
    }

    pub const SHORT_SEARCH_FALLBACK_LIMIT: usize = 5000;

    pub fn upsert_clip(
        &self,
        input: NewClip<'_>,
    ) -> Result<(
        ClipSummary,
        Option<crate::browser_metadata::ClipUpsertReceipt>,
    )> {
        let normalized = normalize_content(input.content);
        anyhow::ensure!(!normalized.is_empty(), "пустой Clipboard не сохраняется");
        anyhow::ensure!(normalized.len() <= 1_000_000, "текст превышает лимит 1 МБ");
        let domain = input.domain.and_then(normalize_domain);
        let domain_key = domain.as_deref().unwrap_or("");
        let title = input
            .page_title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(500).collect::<String>());
        let hash = content_hash(&normalized);
        let kind = classify(&normalized);
        let mut db = self.connection.lock();
        let tx = db.transaction()?;

        let prior_state: Option<PriorClipState> = if domain_key.is_empty() {
            match tx.query_row(
                "SELECT id, last_copied_at, sort_key, copy_count, content FROM clips WHERE content_hash=?1 AND domain_key=''",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            ) {
                Ok(row) => Some(row),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            }
        } else {
            None
        };

        let existing_id: Option<String> = if domain_key.is_empty() {
            prior_state.as_ref().map(|(id, _, _, _, _)| id.clone())
        } else {
            match tx.query_row(
                "SELECT id FROM clips WHERE content_hash=?1 AND domain_key=?2",
                params![hash, domain_key],
                |r| r.get(0),
            ) {
                Ok(id) => Some(id),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            }
        };

        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let changed = tx.execute(
            "UPDATE clips SET content=?4, last_copied_at=?2, copy_count=copy_count+1, page_title=COALESCE(?3,page_title), sort_key=sort_key+1 WHERE id=?1",
            params![id, input.now, title, input.content],
        )?;
        if changed == 0 {
            tx.execute(
                "INSERT INTO clips(id,content,content_hash,content_type,domain,domain_key,page_title,created_at,last_copied_at,copy_count,pinned,sort_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,1,0,0)",
                params![id, input.content, hash, kind.as_str(), domain, domain_key, title, input.now],
            )?;
        }
        let clip = load_clip_summary(&tx, &id)?;

        let (clip_id, prev_last, prev_sort, prev_count, prev_content) = match prior_state {
            Some((cid, last, sort, count, content)) => (cid, last, sort, count, Some(content)),
            None => (id.clone(), None, None, 0, None),
        };

        let resulting_sort = prev_sort.unwrap_or(0) + if prev_count > 0 { 1 } else { 0 };

        let receipt = if domain_key.is_empty() {
            Some(crate::browser_metadata::ClipUpsertReceipt {
                receipt_id: Uuid::new_v4(),
                clip_id,
                content_hash: hash,
                normalized_length_bytes: normalized.len(),
                previous_last_copied_at: prev_last,
                previous_sort_key: prev_sort,
                previous_copy_count: prev_count,
                previous_content: prev_content,
                resulting_last_copied_at: input.now,
                resulting_sort_key: resulting_sort,
                resulting_copy_count: clip.copy_count,
                copy_timestamp: input.now,
            })
        } else {
            None
        };

        tx.commit()?;
        Ok((clip, receipt))
    }

    pub fn upsert_image(
        &self,
        input: NewImageClip<'_>,
    ) -> Result<(
        ClipSummary,
        Option<crate::browser_metadata::ClipUpsertReceipt>,
    )> {
        let store = self
            .blob_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("blob-хранилище недоступно"))?;
        let prepared = store.prepare(input.image, input.max_image_bytes)?;
        let domain = input.domain.and_then(normalize_domain);
        let domain_key = domain.as_deref().unwrap_or("");
        let title = input
            .page_title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(500).collect::<String>());

        let mut db = self.connection.lock();
        let tx = db.transaction()?;
        let blob_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM image_blobs WHERE hash=?1)",
            [&prepared.hash],
            |row| row.get(0),
        )?;
        if !blob_exists {
            let current_bytes: i64 = tx.query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM image_blobs",
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                (current_bytes as u64).saturating_add(prepared.size_bytes)
                    <= input.max_storage_bytes,
                "хранилище изображений достигло лимита {} МБ",
                input.max_storage_bytes / (1024 * 1024)
            );
        }

        let (match_hash, match_len) = crate::domain::ClipboardPayload::Image(input.image.clone())
            .match_key()
            .expect("image payload match key");
        let created_file = store.persist(&prepared)?;
        let db_result = (|| -> Result<(
            ClipSummary,
            Option<crate::browser_metadata::ClipUpsertReceipt>,
        )> {
            let prior_state: Option<PriorClipState> = if domain_key.is_empty() {
                match tx.query_row(
                    "SELECT id, last_copied_at, sort_key, copy_count, content FROM clips WHERE content_hash=?1 AND domain_key=''",
                    params![match_hash],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                ) {
                    Ok(row) => Some(row),
                    Err(rusqlite::Error::QueryReturnedNoRows) => None,
                    Err(e) => return Err(e.into()),
                }
            } else {
                None
            };

            tx.execute(
                "INSERT OR IGNORE INTO image_blobs(
                    hash,relative_path,mime_type,width,height,size_bytes,thumbnail_data_url,created_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    prepared.hash,
                    prepared.relative_path,
                    prepared.mime_type,
                    prepared.width,
                    prepared.height,
                    prepared.size_bytes,
                    prepared.thumbnail_data_url,
                    input.now
                ],
            )?;

            let existing_id: Option<String> = if domain_key.is_empty() {
                prior_state.as_ref().map(|(id, _, _, _, _)| id.clone())
            } else {
                match tx.query_row(
                    "SELECT id FROM clips WHERE content_hash=?1 AND domain_key=?2",
                    params![match_hash, domain_key],
                    |row| row.get(0),
                ) {
                    Ok(id) => Some(id),
                    Err(rusqlite::Error::QueryReturnedNoRows) => None,
                    Err(error) => return Err(error.into()),
                }
            };
            let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let changed = tx.execute(
                "UPDATE clips SET last_copied_at=?2, copy_count=copy_count+1,
                    page_title=COALESCE(?3,page_title), sort_key=sort_key+1,
                    payload_kind='image', blob_hash=?4
                 WHERE id=?1",
                params![id, input.now, title, prepared.hash],
            )?;
            if changed == 0 {
                tx.execute(
                    "INSERT INTO clips(
                        id,content,content_hash,content_type,domain,domain_key,page_title,
                        created_at,last_copied_at,copy_count,pinned,sort_key,payload_kind,blob_hash
                     ) VALUES(?1,'',?2,'Text',?3,?4,?5,?6,?6,1,0,0,'image',?7)",
                    params![id, match_hash, domain, domain_key, title, input.now, prepared.hash],
                )?;
            }
            let summary = load_clip_summary(&tx, &id)?;

            let (clip_id, prev_last, prev_sort, prev_count, prev_content) = match prior_state {
                Some((cid, last, sort, count, content)) => (cid, last, sort, count, Some(content)),
                None => (id.clone(), None, None, 0, None),
            };

            let resulting_sort = prev_sort.unwrap_or(0) + if prev_count > 0 { 1 } else { 0 };

            let receipt = if domain_key.is_empty() {
                Some(crate::browser_metadata::ClipUpsertReceipt {
                    receipt_id: Uuid::new_v4(),
                    clip_id,
                    content_hash: match_hash,
                    normalized_length_bytes: match_len,
                    previous_last_copied_at: prev_last,
                    previous_sort_key: prev_sort,
                    previous_copy_count: prev_count,
                    previous_content: prev_content,
                    resulting_last_copied_at: summary.last_copied_at,
                    resulting_sort_key: resulting_sort,
                    resulting_copy_count: summary.copy_count,
                    copy_timestamp: input.now,
                })
            } else {
                None
            };

            tx.commit()?;
            Ok((summary, receipt))
        })();

        if db_result.is_err() && created_file {
            let _ = store.remove(&prepared.relative_path);
        }

        db_result
    }

    /// Attach browser metadata to a recently saved clip using its upsert receipt.
    pub fn attach_metadata_with_receipt(
        &self,
        event: &crate::browser_metadata::BrowserCopyEvent,
        receipt: crate::browser_metadata::ClipUpsertReceipt,
    ) -> Result<Option<String>> {
        let event_ts_ms = match event.timestamp_millis() {
            Ok(ts) => ts,
            Err(_) => chrono::Utc::now().timestamp_millis(),
        };
        let domain_key = &event.domain;
        let title = if event.page_title.is_empty() {
            None
        } else {
            Some(event.page_title.chars().take(500).collect::<String>())
        };

        let mut db = self.connection.lock();
        let tx = db.transaction()?;

        #[allow(clippy::type_complexity)]
        let candidate: Option<(String, i64, i64, i64, bool, String, String, i64, String)> = match tx.query_row(
            "SELECT id, copy_count, sort_key, created_at, pinned, content, content_type, last_copied_at, payload_kind
             FROM clips
             WHERE id=?1 AND content_hash=?2 AND domain_key=''
               AND copy_count=?3 AND last_copied_at=?4",
            params![
                receipt.clip_id,
                event.content_hash,
                receipt.resulting_copy_count,
                receipt.resulting_last_copied_at
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        ) {
            Ok(c) => Some(c),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                log::info!(
                    "Receipt for clip_id {} no longer matches DB state; skipping receipt",
                    receipt.clip_id
                );
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };

        let Some((
            temp_id,
            _temp_count,
            temp_sort,
            temp_created,
            temp_pinned,
            temp_content,
            temp_kind,
            _temp_last_copied_at,
            temp_payload_kind,
        )) = candidate
        else {
            return Ok(None);
        };

        if temp_payload_kind == "image" {
            if receipt.normalized_length_bytes != event.content_length {
                log::warn!(
                    "attach_metadata_with_receipt: image length mismatch ({} vs {}), skipping",
                    receipt.normalized_length_bytes,
                    event.content_length
                );
                return Ok(None);
            }
        } else {
            let normalized_len = normalize_content(&temp_content).len();
            if normalized_len != event.content_length {
                log::warn!(
                    "attach_metadata_with_receipt: hash match but normalized length mismatch ({} vs {}), skipping",
                    normalized_len,
                    event.content_length
                );
                return Ok(None);
            }
        }

        let existing: Option<(String, i64, i64, i64, bool)> = match tx.query_row(
            "SELECT id, copy_count, sort_key, created_at, pinned
             FROM clips WHERE content_hash=?1 AND domain_key=?2",
            params![event.content_hash, domain_key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            },
        ) {
            Ok(row) => Some(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };

        let is_new_copy = receipt.previous_copy_count == 0;

        let canonical_id = if is_new_copy {
            if let Some((
                existing_id,
                existing_count,
                existing_sort,
                existing_created,
                existing_pinned,
            )) = existing
            {
                let merged_count = existing_count.saturating_add(1);
                let merged_sort = existing_sort.max(temp_sort);
                let merged_pinned = existing_pinned || temp_pinned;
                let merged_created = existing_created.min(temp_created);

                tx.execute(
                    "UPDATE clips SET
                        copy_count=?2, sort_key=?3, pinned=?4, created_at=?5,
                        page_title=COALESCE(?6, page_title), last_copied_at=MAX(last_copied_at,?7),
                        content=?8
                     WHERE id=?1",
                    params![
                        existing_id,
                        merged_count,
                        merged_sort,
                        merged_pinned,
                        merged_created,
                        title,
                        event_ts_ms,
                        temp_content
                    ],
                )?;

                tx.execute(
                    "INSERT OR IGNORE INTO clip_user_categories(clip_id, category_id, created_at)
                     SELECT ?1, category_id, created_at FROM clip_user_categories WHERE clip_id=?2",
                    params![existing_id, temp_id],
                )?;

                tx.execute("DELETE FROM clips WHERE id=?1", [&temp_id])?;
                existing_id
            } else {
                tx.execute(
                    "UPDATE clips SET domain=?2, domain_key=?3, page_title=COALESCE(?4, page_title) WHERE id=?1",
                    params![temp_id, event.domain, domain_key, title],
                )?;
                temp_id
            }
        } else {
            if let Some(ref prev_content) = receipt.previous_content {
                tx.execute(
                    "UPDATE clips SET
                        copy_count = ?2,
                        sort_key = COALESCE(?3, sort_key),
                        last_copied_at = COALESCE(?4, last_copied_at),
                        content = ?5
                     WHERE id = ?1",
                    params![
                        temp_id,
                        receipt.previous_copy_count,
                        receipt.previous_sort_key,
                        receipt.previous_last_copied_at,
                        prev_content
                    ],
                )?;
            } else {
                tx.execute(
                    "UPDATE clips SET
                        copy_count = ?2,
                        sort_key = COALESCE(?3, sort_key),
                        last_copied_at = COALESCE(?4, last_copied_at)
                     WHERE id = ?1",
                    params![
                        temp_id,
                        receipt.previous_copy_count,
                        receipt.previous_sort_key,
                        receipt.previous_last_copied_at
                    ],
                )?;
            }

            if let Some((existing_id, _, _, _, _)) = existing {
                tx.execute(
                    "UPDATE clips SET
                        copy_count = copy_count + 1,
                        sort_key = MAX(sort_key, ?2),
                        last_copied_at = MAX(last_copied_at, ?3),
                        page_title = COALESCE(?4, page_title),
                        content = ?5
                     WHERE id=?1",
                    params![existing_id, temp_sort, event_ts_ms, title, temp_content],
                )?;
                existing_id
            } else {
                let new_id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO clips(id,content,content_hash,content_type,domain,domain_key,page_title,created_at,last_copied_at,copy_count,pinned,sort_key)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,0,?10)",
                    params![
                        new_id,
                        temp_content,
                        event.content_hash,
                        temp_kind,
                        event.domain,
                        domain_key,
                        title,
                        temp_created,
                        event_ts_ms,
                        temp_sort
                    ],
                )?;
                new_id
            }
        };

        tx.commit()?;
        Ok(Some(canonical_id))
    }

    pub fn list_clips(&self, query: &ClipQuery) -> Result<Vec<ClipSummary>> {
        let db = self.connection.lock();
        let search = query.search.as_deref().unwrap_or("").trim().to_lowercase();
        let _ = Self::SHORT_SEARCH_FALLBACK_LIMIT;
        let fts_formatted = format_fts_query(&search);
        let kind = query.content_type.map(ContentType::as_str);
        let payload_kind = query
            .payload_kind
            .or_else(|| if query.content_type.is_some() { Some(PayloadKind::Text) } else { None })
            .map(PayloadKind::as_str);
        let limit = query.limit.unwrap_or(100).clamp(1, 200);
        let offset = query.offset.unwrap_or(0);

        let search_len = search.chars().count();
        let is_short_query = !search.is_empty() && (search_len < 3 || fts_formatted.is_empty());
        let short_query_flag = if is_short_query { 1i32 } else { 0i32 };

        // Build domain/category LIKE pattern.
        let like = if search.is_empty() {
            String::new()
        } else {
            format!(
                "%{}%",
                search
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        };

        let mut statement = db.prepare(
            "SELECT DISTINCT c.id,
                    substr(c.content, 1, 300),
                    length(c.content),
                    c.content_type,
                    c.domain,
                    c.page_title,
                    c.created_at,
                    c.last_copied_at,
                    c.copy_count,
                    c.pinned,
                    c.payload_kind,
                    b.mime_type,
                    b.width,
                    b.height,
                    b.size_bytes,
                    b.thumbnail_data_url
             FROM clips c
             LEFT JOIN image_blobs b ON b.hash = c.blob_hash
             LEFT JOIN clip_user_categories cc ON cc.clip_id = c.id
             LEFT JOIN user_categories uc ON uc.id = cc.category_id
             WHERE (?1 = ''
                    OR (?2 != '' AND c.rowid IN (SELECT rowid FROM clips_fts WHERE clips_fts MATCH ?2))
                    OR (?9 = 1 AND c.id IN (SELECT id FROM clips ORDER BY last_copied_at DESC, sort_key DESC LIMIT 5000) AND (kitsupin_lower(c.content) LIKE ?3 ESCAPE '\\' OR kitsupin_lower(COALESCE(c.page_title, '')) LIKE ?3 ESCAPE '\\'))
                    OR (?3 != '' AND kitsupin_lower(COALESCE(c.domain,'')) LIKE ?3 ESCAPE '\\')
                    OR (?3 != '' AND kitsupin_lower(COALESCE(uc.name,'')) LIKE ?3 ESCAPE '\\'))
               AND (?4 IS NULL OR c.content_type = ?4)
               AND (?5 IS NULL OR c.domain = ?5)
               AND (?6 IS NULL OR cc.category_id = ?6)
               AND (?10 IS NULL OR c.payload_kind = ?10)
             ORDER BY c.last_copied_at DESC, c.sort_key DESC
             LIMIT ?7 OFFSET ?8",
        )?;

        let mut summaries = statement
            .query_map(
                params![
                    search,
                    fts_formatted,
                    like,
                    kind,
                    query.domain,
                    query.category_id,
                    limit,
                    offset,
                    short_query_flag,
                    payload_kind
                ],
                |r| {
                    let payload_kind_str: String = r.get(10)?;
                    let payload_kind = if payload_kind_str == "image" {
                        PayloadKind::Image
                    } else {
                        PayloadKind::Text
                    };
                    let image = if payload_kind == PayloadKind::Image {
                        Some(ImageMetadata {
                            mime_type: r.get(11)?,
                            width: r.get::<_, i64>(12)? as u32,
                            height: r.get::<_, i64>(13)? as u32,
                            size_bytes: r.get::<_, i64>(14)? as u64,
                            thumbnail_data_url: r.get(15)?,
                        })
                    } else {
                        None
                    };
                    let text_preview: String = r.get(1)?;
                    let preview = image
                        .as_ref()
                        .map(|meta| format!("Изображение {} × {}", meta.width, meta.height))
                        .unwrap_or(text_preview);
                    // SQLite length() counts bytes for BLOB but chars for TEXT.
                    // We store content as TEXT, so length() returns char count on most builds,
                    // but we also have the stored byte length. Use the stored byte length
                    // from substr query; is_truncated is determined by preview byte length.
                    let stored_len: i64 = r.get(2)?;
                    let preview_bytes = preview.len() as i64;
                    // SQLite substr(x,1,300) extracts up to 300 chars; compare char counts.
                    let preview_chars = preview.chars().count() as i64;
                    // stored_len here is length(c.content) in SQLite which for TEXT = chars.
                    let is_truncated =
                        payload_kind == PayloadKind::Text && stored_len > preview_chars;
                    let kind_str: String = r.get(3)?;
                    let content_type = match kind_str.as_str() {
                        "Links" => ContentType::Links,
                        "Email" => ContentType::Email,
                        "Numbers" => ContentType::Numbers,
                        _ => ContentType::Text,
                    };
                    let _ = preview_bytes; // suppress unused warning
                    Ok(ClipSummary {
                        id: r.get(0)?,
                        preview,
                        content_length: stored_len as usize,
                        is_truncated,
                        content_type,
                        payload_kind,
                        image,
                        domain: r.get(4)?,
                        page_title: r.get(5)?,
                        created_at: r.get(6)?,
                        last_copied_at: r.get(7)?,
                        copy_count: r.get(8)?,
                        pinned: r.get(9)?,
                        categories: Vec::new(),
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);

        if summaries.is_empty() {
            return Ok(summaries);
        }

        let clip_ids: Vec<String> = summaries.iter().map(|s| s.id.clone()).collect();
        let placeholders = clip_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql_cats = format!(
            "SELECT cc.clip_id, uc.id, uc.name, uc.color, uc.created_at, uc.sort_order
             FROM user_categories uc
             JOIN clip_user_categories cc ON cc.category_id = uc.id
             WHERE cc.clip_id IN ({placeholders})
             ORDER BY uc.sort_order, uc.name"
        );
        let mut cat_stmt = db.prepare(&sql_cats)?;
        let cat_rows = cat_stmt
            .query_map(rusqlite::params_from_iter(clip_ids.iter()), |r| {
                let clip_id: String = r.get(0)?;
                let cat = Category {
                    id: r.get(1)?,
                    name: r.get(2)?,
                    color: r.get(3)?,
                    created_at: r.get(4)?,
                    sort_order: r.get(5)?,
                };
                Ok((clip_id, cat))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        use std::collections::HashMap;
        let mut cat_map: HashMap<String, Vec<Category>> = HashMap::new();
        for (clip_id, cat) in cat_rows {
            cat_map.entry(clip_id).or_default().push(cat);
        }

        for summary in &mut summaries {
            if let Some(cats) = cat_map.remove(&summary.id) {
                summary.categories = cats;
            }
        }

        Ok(summaries)
    }

    pub fn get_clip_content(&self, id: &str) -> Result<String> {
        let db = self.connection.lock();
        let (content, payload_kind): (String, String) = db.query_row(
            "SELECT content,payload_kind FROM clips WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        anyhow::ensure!(
            payload_kind == "text",
            "карточка содержит изображение, а не текст"
        );
        Ok(content)
    }

    pub fn get_clip_for_copy(&self, id: &str) -> Result<ClipboardCopy> {
        let (payload_kind, content, relative_path): (String, String, Option<String>) =
            self.connection.lock().query_row(
                "SELECT c.payload_kind,c.content,b.relative_path
             FROM clips c LEFT JOIN image_blobs b ON b.hash=c.blob_hash WHERE c.id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        if payload_kind == "image" {
            let store = self
                .blob_store
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("blob-хранилище недоступно"))?;
            let relative_path = relative_path
                .ok_or_else(|| anyhow::anyhow!("у изображения отсутствует blob-файл"))?;
            let payload = ClipboardPayload::Image(store.load(&relative_path)?);
            Ok(ClipboardCopy {
                fingerprint: payload.fingerprint(),
                payload,
            })
        } else {
            let payload = ClipboardPayload::Text(content);
            Ok(ClipboardCopy {
                fingerprint: payload.fingerprint(),
                payload,
            })
        }
    }

    pub fn get_image_data_url(&self, id: &str) -> Result<String> {
        let (relative_path, mime_type): (String, String) = self.connection.lock().query_row(
            "SELECT b.relative_path,b.mime_type FROM clips c
             JOIN image_blobs b ON b.hash=c.blob_hash
             WHERE c.id=?1 AND c.payload_kind='image'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        self.blob_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("blob-хранилище недоступно"))?
            .read_data_url(&relative_path, &mime_type)
    }

    pub fn image_file_path(&self, id: &str) -> Result<std::path::PathBuf> {
        let relative_path: String = self.connection.lock().query_row(
            "SELECT b.relative_path FROM clips c JOIN image_blobs b ON b.hash=c.blob_hash
             WHERE c.id=?1 AND c.payload_kind='image'",
            [id],
            |r| r.get(0),
        )?;
        self.blob_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("blob-хранилище недоступно"))?
            .absolute_path(&relative_path)
    }

    pub fn storage_stats(&self) -> Result<StorageStats> {
        let (count, bytes): (i64, i64) = self.connection.lock().query_row(
            "SELECT COUNT(*),COALESCE(SUM(size_bytes),0) FROM image_blobs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(StorageStats {
            image_count: count as u64,
            image_bytes: bytes as u64,
            orphan_files_removed: 0,
        })
    }

    pub fn cleanup_orphan_blobs(&self) -> Result<usize> {
        let db = self.connection.lock();
        let mut statement = db.prepare("SELECT relative_path FROM image_blobs")?;
        let referenced = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        drop(statement);
        drop(db);
        self.blob_store
            .as_ref()
            .map(|store| store.cleanup_orphans(&referenced))
            .transpose()
            .map(|count| count.unwrap_or(0))
    }

    /// Update copy statistics for a clip. Call this AFTER successfully writing to clipboard.
    pub fn mark_clip_copied(&self, id: &str, now: i64) -> Result<()> {
        let db = self.connection.lock();
        let changed = db.execute(
            "UPDATE clips SET last_copied_at=?2, copy_count=copy_count+1, sort_key=sort_key+1 WHERE id=?1",
            params![id, now],
        )?;
        if changed == 0 {
            anyhow::bail!("карточка не найдена: {id}");
        }
        Ok(())
    }

    /// Legacy: update clip stats and return content in one call.
    /// Kept for compatibility; prefer get_clip_content + mark_clip_copied.
    #[allow(dead_code)]
    pub fn touch_clip(&self, id: &str, now: i64) -> Result<String> {
        let db = self.connection.lock();
        let changed = db.execute(
            "UPDATE clips SET last_copied_at=?2, copy_count=copy_count+1, sort_key=sort_key+1 WHERE id=?1",
            params![id, now],
        )?;
        if changed == 0 {
            anyhow::bail!("карточка не найдена");
        }
        let content: String =
            db.query_row("SELECT content FROM clips WHERE id=?1", [id], |r| r.get(0))?;
        Ok(content)
    }

    pub fn delete_clip(&self, id: &str) -> Result<()> {
        let mut db = self.connection.lock();
        let tx = db.transaction()?;
        let blob: Option<(String, String)> = match tx.query_row(
            "SELECT b.hash,b.relative_path FROM clips c JOIN image_blobs b ON b.hash=c.blob_hash WHERE c.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(value) => Some(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => return Err(error.into()),
        };
        tx.execute("DELETE FROM clips WHERE id=?1", [id])?;
        let orphan_path = if let Some((hash, relative_path)) = blob {
            let refs: i64 = tx.query_row(
                "SELECT COUNT(*) FROM clips WHERE blob_hash=?1",
                [&hash],
                |r| r.get(0),
            )?;
            if refs == 0 {
                tx.execute("DELETE FROM image_blobs WHERE hash=?1", [&hash])?;
                Some(relative_path)
            } else {
                None
            }
        } else {
            None
        };
        tx.commit()?;
        drop(db);
        if let (Some(store), Some(path)) = (&self.blob_store, orphan_path) {
            if let Err(error) = store.remove(&path) {
                log::warn!("не удалось удалить потерянный blob {path}: {error}");
            }
        }
        Ok(())
    }
    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        self.connection.lock().execute(
            "UPDATE clips SET pinned=?2 WHERE id=?1",
            params![id, pinned],
        )?;
        Ok(())
    }
    pub fn clear_unpinned(&self) -> Result<usize> {
        self.delete_matching("pinned=0", [])
    }

    pub fn clear_unpinned_images(&self) -> Result<usize> {
        self.delete_matching("pinned=0 AND payload_kind='image'", [])
    }
    pub fn cleanup(&self, days: u32, now_ms: i64) -> Result<usize> {
        anyhow::ensure!(
            (1..=3650).contains(&days),
            "Недопустимый срок хранения: {days}"
        );
        let retention_ms = (days as i64) * 86_400_000;
        let cutoff_ms = now_ms.saturating_sub(retention_ms);
        self.delete_matching("pinned=0 AND last_copied_at < ?1", params![cutoff_ms])
    }

    fn delete_matching<P>(&self, predicate: &str, parameters: P) -> Result<usize>
    where
        P: rusqlite::Params,
    {
        let ids = {
            let db = self.connection.lock();
            let mut statement = db.prepare(&format!("SELECT id FROM clips WHERE {predicate}"))?;
            let ids = statement
                .query_map(parameters, |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        for id in &ids {
            self.delete_clip(id)?;
        }
        Ok(ids.len())
    }

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let db = self.connection.lock();
        let mut s = db.prepare("SELECT id,name,color,created_at,sort_order FROM user_categories ORDER BY sort_order,name COLLATE NOCASE")?;
        let categories = s
            .query_map([], row_category)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(categories)
    }
    pub fn create_category(&self, name: &str, color: &str, now: i64) -> Result<Category> {
        let (name, normalized_name) = normalize_category_name(name)?;
        validate_color(color)?;
        let category = Category {
            id: Uuid::new_v4().to_string(),
            name,
            color: color.into(),
            created_at: now,
            sort_order: 0,
        };
        self.connection.lock().execute("INSERT INTO user_categories(id,name,normalized_name,color,created_at,sort_order) VALUES(?1,?2,?3,?4,?5,0)", params![category.id, category.name, normalized_name, category.color, category.created_at])?;
        Ok(category)
    }
    pub fn update_category(&self, id: &str, name: &str, color: &str) -> Result<()> {
        let (name, normalized_name) = normalize_category_name(name)?;
        validate_color(color)?;
        self.connection.lock().execute(
            "UPDATE user_categories SET name=?2,normalized_name=?3,color=?4 WHERE id=?1",
            params![id, name, normalized_name, color],
        )?;
        Ok(())
    }
    pub fn delete_category(&self, id: &str) -> Result<()> {
        self.connection
            .lock()
            .execute("DELETE FROM user_categories WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn assign_category(&self, clip: &str, category: &str, now: i64) -> Result<()> {
        self.connection.lock().execute("INSERT OR IGNORE INTO clip_user_categories(clip_id,category_id,created_at) VALUES(?1,?2,?3)", params![clip,category,now])?;
        Ok(())
    }
    pub fn unassign_category(&self, clip: &str, category: &str) -> Result<()> {
        self.connection.lock().execute(
            "DELETE FROM clip_user_categories WHERE clip_id=?1 AND category_id=?2",
            params![clip, category],
        )?;
        Ok(())
    }
}

fn format_fts_query(search: &str) -> String {
    let words: Vec<&str> = search
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    if words.is_empty() {
        String::new()
    } else {
        words
            .iter()
            .map(|w| format!("\"{}\"*", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn is_valid_hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn validate_color(value: &str) -> Result<()> {
    anyhow::ensure!(
        is_valid_hex_color(value),
        "цвет должен быть в формате #RRGGBB"
    );
    Ok(())
}
fn normalize_category_name(value: &str) -> Result<(String, String)> {
    let name = value.trim();
    anyhow::ensure!(
        !name.is_empty() && name.chars().count() <= 60,
        "название категории должно содержать 1–60 символов"
    );
    Ok((name.to_owned(), name.to_lowercase()))
}
fn configure_connection(connection: &Connection) -> Result<()> {
    connection.create_scalar_function(
        "kitsupin_lower",
        1,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS,
        |context| {
            let value = context.get::<String>(0)?;
            Ok(value.to_lowercase())
        },
    )?;
    Ok(())
}
fn row_category(row: &Row<'_>) -> rusqlite::Result<Category> {
    Ok(Category {
        id: row.get(0)?,
        name: row.get(1)?,
        color: row.get(2)?,
        created_at: row.get(3)?,
        sort_order: row.get(4)?,
    })
}

fn load_clip_summary(db: &Connection, id: &str) -> Result<ClipSummary> {
    let mut summary = db.query_row(
        "SELECT c.id, substr(c.content,1,300), length(c.content), c.content_type,
                c.domain, c.page_title, c.created_at, c.last_copied_at, c.copy_count, c.pinned,
                c.payload_kind, b.mime_type, b.width, b.height, b.size_bytes, b.thumbnail_data_url
         FROM clips c LEFT JOIN image_blobs b ON b.hash=c.blob_hash WHERE c.id=?1",
        [id],
        |r| {
            let payload_kind = if r.get::<_, String>(10)? == "image" {
                PayloadKind::Image
            } else {
                PayloadKind::Text
            };
            let image = if payload_kind == PayloadKind::Image {
                Some(ImageMetadata {
                    mime_type: r.get(11)?,
                    width: r.get::<_, i64>(12)? as u32,
                    height: r.get::<_, i64>(13)? as u32,
                    size_bytes: r.get::<_, i64>(14)? as u64,
                    thumbnail_data_url: r.get(15)?,
                })
            } else {
                None
            };
            let text_preview: String = r.get(1)?;
            let preview = image
                .as_ref()
                .map(|meta| format!("Изображение {} × {}", meta.width, meta.height))
                .unwrap_or(text_preview);
            let stored_len: i64 = r.get(2)?;
            let preview_chars = preview.chars().count() as i64;
            let is_truncated = payload_kind == PayloadKind::Text && stored_len > preview_chars;
            let kind: String = r.get(3)?;
            Ok(ClipSummary {
                id: r.get(0)?,
                preview,
                content_length: stored_len as usize,
                is_truncated,
                content_type: match kind.as_str() {
                    "Links" => ContentType::Links,
                    "Email" => ContentType::Email,
                    "Numbers" => ContentType::Numbers,
                    _ => ContentType::Text,
                },
                payload_kind,
                image,
                domain: r.get(4)?,
                page_title: r.get(5)?,
                created_at: r.get(6)?,
                last_copied_at: r.get(7)?,
                copy_count: r.get(8)?,
                pinned: r.get(9)?,
                categories: vec![],
            })
        },
    )?;
    let mut s = db.prepare("SELECT uc.id,uc.name,uc.color,uc.created_at,uc.sort_order FROM user_categories uc JOIN clip_user_categories cc ON cc.category_id=uc.id WHERE cc.clip_id=?1 ORDER BY uc.sort_order,uc.name")?;
    summary.categories = s
        .query_map([id], row_category)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ClipboardPayload, ImagePayload, NewImageClip, PayloadKind};
    use rusqlite::Connection;

    fn add<'a>(
        repo: &Repository,
        text: &'a str,
        domain: Option<&'a str>,
        title: Option<&'a str>,
        now: i64,
    ) -> ClipSummary {
        repo.upsert_clip(NewClip {
            content: text,
            domain,
            page_title: title,
            now,
        })
        .unwrap()
        .0
    }

    fn add_with_receipt<'a>(
        repo: &Repository,
        text: &'a str,
        domain: Option<&'a str>,
        title: Option<&'a str>,
        now: i64,
    ) -> (
        ClipSummary,
        Option<crate::browser_metadata::ClipUpsertReceipt>,
    ) {
        repo.upsert_clip(NewClip {
            content: text,
            domain,
            page_title: title,
            now,
        })
        .unwrap()
    }

    fn image(red: u8) -> ImagePayload {
        ImagePayload {
            width: 2,
            height: 1,
            rgba: vec![red, 0, 0, 255, 0, 255, 0, 255],
            source_mime: None,
            source_bytes: None,
        }
    }

    fn add_image(
        repo: &Repository,
        image: &ImagePayload,
        domain: Option<&str>,
        now: i64,
    ) -> ClipSummary {
        repo.upsert_image(NewImageClip {
            image,
            domain,
            page_title: Some("Image source"),
            now,
            max_image_bytes: 1024 * 1024,
            max_storage_bytes: 10 * 1024 * 1024,
        })
        .unwrap()
        .0
    }

    #[test]
    fn image_blobs_are_deduplicated_and_copy_back_as_rgba() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::open_in_memory_with_blobs(&temp.path().join("blobs")).unwrap();
        let payload = image(255);
        let first = add_image(&repo, &payload, None, 1_000);
        let second = add_image(&repo, &payload, None, 2_000);
        assert_eq!(first.id, second.id);
        assert_eq!(second.copy_count, 2);
        assert_eq!(second.payload_kind, PayloadKind::Image);
        assert_eq!(repo.storage_stats().unwrap().image_count, 1);
        let copy = repo.get_clip_for_copy(&first.id).unwrap();
        assert_eq!(
            copy.fingerprint,
            ClipboardPayload::Image(payload.clone()).fingerprint()
        );
        match copy.payload {
            ClipboardPayload::Image(restored) => assert_eq!(restored.rgba, payload.rgba),
            ClipboardPayload::Text(_) => panic!("expected image payload"),
        }
    }

    #[test]
    fn deleting_last_image_reference_removes_blob_file() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::open_in_memory_with_blobs(&temp.path().join("blobs")).unwrap();
        let payload = image(100);
        let first = add_image(&repo, &payload, Some("one.example"), 1_000);
        let second = add_image(&repo, &payload, Some("two.example"), 2_000);
        let path = repo.image_file_path(&first.id).unwrap();
        assert!(path.exists());
        repo.delete_clip(&first.id).unwrap();
        assert!(path.exists());
        repo.delete_clip(&second.id).unwrap();
        assert!(!path.exists());
        assert_eq!(repo.storage_stats().unwrap().image_count, 0);
    }

    #[test]
    fn image_storage_limit_rejects_without_leaving_orphan() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::open_in_memory_with_blobs(&temp.path().join("blobs")).unwrap();
        let payload = image(75);
        let result = repo.upsert_image(NewImageClip {
            image: &payload,
            domain: None,
            page_title: None,
            now: 1_000,
            max_image_bytes: 1024 * 1024,
            max_storage_bytes: 1,
        });
        assert!(result.is_err());
        assert_eq!(repo.storage_stats().unwrap().image_count, 0);
        assert_eq!(repo.cleanup_orphan_blobs().unwrap(), 0);
    }

    #[test]
    fn clearing_images_preserves_pinned_image_and_blob() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::open_in_memory_with_blobs(&temp.path().join("blobs")).unwrap();
        let pinned = add_image(&repo, &image(20), None, 1_000);
        let unpinned = add_image(&repo, &image(40), None, 2_000);
        let pinned_path = repo.image_file_path(&pinned.id).unwrap();
        let unpinned_path = repo.image_file_path(&unpinned.id).unwrap();
        repo.set_pinned(&pinned.id, true).unwrap();
        assert_eq!(repo.clear_unpinned_images().unwrap(), 1);
        assert!(pinned_path.exists());
        assert!(!unpinned_path.exists());
        assert_eq!(repo.storage_stats().unwrap().image_count, 1);
    }

    #[test]
    fn deduplicates_only_same_content_and_domain_and_updates_fields() {
        let r = Repository::open_in_memory().unwrap();
        let a = add(
            &r,
            "hello",
            Some("github.com"),
            Some("A"),
            1_700_000_000_000,
        );
        let b = add(
            &r,
            "hello",
            Some("github.com"),
            Some("B"),
            1_700_000_001_000,
        );
        let c = add(&r, "hello", Some("youtube.com"), None, 1_700_000_002_000);
        let d = add(&r, "hello", None, None, 1_700_000_003_000);
        let e = add(
            &r,
            "hello",
            None,
            Some("No Domain Title"),
            1_700_000_004_000,
        );
        assert_eq!(a.id, b.id);
        assert_eq!(b.copy_count, 2);
        assert_eq!(b.page_title.as_deref(), Some("B"));
        assert_eq!(b.last_copied_at, 1_700_000_001_000);
        assert_ne!(b.id, c.id);
        assert_ne!(c.id, d.id);
        assert_eq!(d.id, e.id);
        assert_eq!(e.copy_count, 2);
    }

    #[test]
    fn cleanup_preserves_pins() {
        let r = Repository::open_in_memory().unwrap();
        let old = add(&r, "old", None, None, 1_000_000_000_000);
        let pin = add(&r, "pin", None, None, 1_000_000_000_000);
        r.set_pinned(&pin.id, true).unwrap();
        let now_ms = 1_000_000_000_000 + 100 * 86_400_000;
        assert_eq!(r.cleanup(90, now_ms).unwrap(), 1);
        let all = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, pin.id);
        assert_ne!(old.id, pin.id);
    }

    #[test]
    fn categories_are_many_to_many_and_delete_cascades_only_links() {
        let r = Repository::open_in_memory().unwrap();
        let clip = add(&r, "x", None, None, 1_700_000_000_000);
        let a = r
            .create_category("Japanese", "#ffd166", 1_700_000_000_000)
            .unwrap();
        let b = r
            .create_category("Saved", "#70c1b3", 1_700_000_000_000)
            .unwrap();
        r.assign_category(&clip.id, &a.id, 1_700_000_000_000)
            .unwrap();
        r.assign_category(&clip.id, &b.id, 1_700_000_000_000)
            .unwrap();
        r.assign_category(&clip.id, &a.id, 1_700_000_000_000)
            .unwrap();
        assert_eq!(
            r.list_clips(&ClipQuery::default()).unwrap()[0]
                .categories
                .len(),
            2
        );
        r.delete_category(&a.id).unwrap();
        assert_eq!(
            r.list_clips(&ClipQuery::default()).unwrap()[0]
                .categories
                .len(),
            1
        );
    }

    #[test]
    fn migration_is_idempotent_and_searches_all_fields() {
        let r = Repository::open_in_memory().unwrap();
        let c = add(
            &r,
            "Привет, Konnichiwa",
            Some("example.com"),
            Some("Study page"),
            1_700_000_000_000,
        );
        let cat = r
            .create_category("Японский", "#123abc", 1_700_000_000_000)
            .unwrap();
        r.assign_category(&c.id, &cat.id, 1_700_000_000_000)
            .unwrap();
        for q in ["ПРИВЕТ", "konn", "EXAMPLE", "study", "ЯПОНСКИЙ"] {
            assert_eq!(
                r.list_clips(&ClipQuery {
                    search: Some(q.into()),
                    ..Default::default()
                })
                .unwrap()
                .len(),
                1,
                "query '{q}' should return 1 result"
            );
        }
        assert!(r
            .create_category("яПОНСКИЙ", "#abcdef", 1_700_000_000_000)
            .is_err());
        r.migrate().unwrap();
    }

    #[test]
    fn mark_clip_copied_updates_stats_without_returning_content() {
        let r = Repository::open_in_memory().unwrap();
        let clip = add(&r, "Original Text", None, None, 1_700_000_000_000);
        r.mark_clip_copied(&clip.id, 1_700_000_100_000).unwrap();
        let updated = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(updated[0].last_copied_at, 1_700_000_100_000);
        assert_eq!(updated[0].copy_count, 2);
    }

    #[test]
    fn touch_clip_updates_copied_stats_and_returns_stored_content() {
        let r = Repository::open_in_memory().unwrap();
        let clip = add(&r, "Original Text", None, None, 1_700_000_000_000);
        let content = r.touch_clip(&clip.id, 1_700_000_100_000).unwrap();
        assert_eq!(content, "Original Text");
        let updated = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(updated[0].last_copied_at, 1_700_000_100_000);
        assert_eq!(updated[0].copy_count, 2);
    }

    #[test]
    fn sort_key_acts_as_secondary_sort_criterion() {
        let r = Repository::open_in_memory().unwrap();
        let t = 1_700_000_000_000;
        let _first = add(&r, "first item", None, None, t);
        let _second = add(&r, "second item", None, None, t);
        r.touch_clip(&_first.id, t).unwrap();
        let list = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(list[0].id, _first.id);
    }

    #[test]
    fn is_truncated_reflects_whether_preview_is_complete() {
        let r = Repository::open_in_memory().unwrap();
        let short = add(&r, "short text", None, None, 1_700_000_000_000);
        let long_text = "x".repeat(400);
        let long = add(&r, &long_text, None, None, 1_700_000_001_000);
        let clips = r.list_clips(&ClipQuery::default()).unwrap();
        let short_s = clips.iter().find(|c| c.id == short.id).unwrap();
        let long_s = clips.iter().find(|c| c.id == long.id).unwrap();
        assert!(!short_s.is_truncated);
        assert!(long_s.is_truncated);
        assert_eq!(long_s.preview.chars().count(), 300);
        assert_eq!(long_s.content_length, 400);
    }

    fn make_browser_event(
        hash: &str,
        length: usize,
        domain: &str,
        title: Option<&str>,
        timestamp_ms: i64,
    ) -> crate::browser_metadata::BrowserCopyEvent {
        let dt = chrono::DateTime::from_timestamp_millis(timestamp_ms).unwrap();
        crate::browser_metadata::BrowserCopyEvent {
            event_id: uuid::Uuid::new_v4(),
            version: 1,
            event: "copy".into(),
            content_hash: hash.into(),
            content_length: length,
            domain: domain.into(),
            page_title: title.unwrap_or("").into(),
            timestamp: dt.to_rfc3339(),
        }
    }

    #[test]
    fn attach_metadata_reconciles_after_clipboard_save() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "hello reconcile";
        let normalized = normalize_content(text);
        let hash = content_hash(&normalized);
        let byte_len = normalized.len();
        let (_clip, receipt) = add_with_receipt(&r, text, None, None, now);

        let event = make_browser_event(
            &hash,
            byte_len,
            "example.com",
            Some("Example Page"),
            now + 200,
        );
        let result = r
            .attach_metadata_with_receipt(&event, receipt.unwrap())
            .unwrap();
        assert!(result.is_some());

        let clips = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].domain.as_deref(), Some("example.com"));
        assert_eq!(clips[0].page_title.as_deref(), Some("Example Page"));
    }

    #[test]
    fn attach_metadata_merges_with_existing_domain_clip() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "merge test content";
        let normalized = normalize_content(text);
        let hash = content_hash(&normalized);
        let byte_len = normalized.len();

        let existing = add(
            &r,
            text,
            Some("example.com"),
            Some("Old Title"),
            now - 10_000,
        );
        let (_temp, receipt) = add_with_receipt(&r, text, None, None, now);

        let cat = r.create_category("Test", "#aabbcc", now).unwrap();
        r.assign_category(&_temp.id, &cat.id, now).unwrap();

        let event =
            make_browser_event(&hash, byte_len, "example.com", Some("New Title"), now + 100);
        let result = r
            .attach_metadata_with_receipt(&event, receipt.unwrap())
            .unwrap();
        assert_eq!(result.as_deref(), Some(existing.id.as_str()));

        let clips = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].id, existing.id);
        assert_eq!(clips[0].copy_count, 2);
        assert_eq!(clips[0].categories.len(), 1);
    }

    #[test]
    fn attach_metadata_rejects_wrong_hash() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "some content";
        let normalized = normalize_content(text);
        let byte_len = normalized.len();
        let (_clip, receipt) = add_with_receipt(&r, text, None, None, now);

        let event = make_browser_event(&"a".repeat(64), byte_len, "example.com", None, now + 100);
        let result = r
            .attach_metadata_with_receipt(&event, receipt.unwrap())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn attach_metadata_rejects_wrong_length() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "length mismatch test";
        let normalized = normalize_content(text);
        let hash = content_hash(&normalized);
        let (_clip, receipt) = add_with_receipt(&r, text, None, None, now);

        let event = make_browser_event(&hash, 9999, "example.com", None, now + 100);
        let result = r
            .attach_metadata_with_receipt(&event, receipt.unwrap())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_scenario_a_metadata_before_clipboard() {
        use crate::browser_metadata::MetadataBuffer;
        use chrono::Utc;
        let buffer = MetadataBuffer::default();
        let repo = Repository::open_in_memory().unwrap();

        let text = "metadata before clipboard content";
        let norm = normalize_content(text);
        let hash = content_hash(&norm);
        let len = norm.len();
        let now = Utc::now();
        let now_ms = now.timestamp_millis();

        let event = make_browser_event(
            &hash,
            len,
            "chrome.example.com",
            Some("Chrome Title"),
            now_ms,
        );
        buffer.push(event).unwrap();

        let receipt = buffer.take_matching_receipt(&hash, len, now_ms, 2000);
        assert!(receipt.is_none(), "No receipt should exist yet");

        let matched_event = buffer.take_match(&hash, len, now).unwrap();
        let (clip, _) = repo
            .upsert_clip(NewClip {
                content: text,
                domain: Some(&matched_event.domain),
                page_title: Some(&matched_event.page_title),
                now: now_ms,
            })
            .unwrap();

        assert_eq!(clip.domain.as_deref(), Some("chrome.example.com"));
        assert_eq!(clip.page_title.as_deref(), Some("Chrome Title"));
        assert!(buffer.take_match(&hash, len, now).is_none());
    }

    #[test]
    fn test_scenario_b_metadata_after_clipboard() {
        use crate::browser_metadata::MetadataBuffer;
        let buffer = MetadataBuffer::default();
        let repo = Repository::open_in_memory().unwrap();

        let text = "metadata after clipboard content";
        let norm = normalize_content(text);
        let hash = content_hash(&norm);
        let len = norm.len();
        let now_ms = chrono::Utc::now().timestamp_millis();

        let (_clip, receipt) = repo
            .upsert_clip(NewClip {
                content: text,
                domain: None,
                page_title: None,
                now: now_ms,
            })
            .unwrap();

        let receipt = receipt.unwrap();
        buffer.push_receipt(&hash, len, receipt.clone());

        let event = make_browser_event(
            &hash,
            len,
            "after.example.com",
            Some("After Title"),
            now_ms + 100,
        );
        let matched_receipt = buffer
            .take_matching_receipt(&hash, len, now_ms + 100, 2000)
            .unwrap();

        let attached_id = repo
            .attach_metadata_with_receipt(&event, matched_receipt)
            .unwrap();
        assert_eq!(attached_id.as_deref(), Some(receipt.clip_id.as_str()));

        let clips = repo.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].domain.as_deref(), Some("after.example.com"));
    }

    #[test]
    fn test_scenario_c_terminal_then_chrome_same_text() {
        use crate::browser_metadata::MetadataBuffer;
        let buffer = MetadataBuffer::default();
        let repo = Repository::open_in_memory().unwrap();

        let text = "terminal vs chrome content";
        let norm = normalize_content(text);
        let hash = content_hash(&norm);
        let len = norm.len();
        let t_terminal = 1_700_000_000_000i64;

        let (_clip_term, receipt_term) = repo
            .upsert_clip(NewClip {
                content: text,
                domain: None,
                page_title: None,
                now: t_terminal,
            })
            .unwrap();
        let r_term = receipt_term.unwrap();
        buffer.push_receipt(&hash, len, r_term);

        let t_chrome = t_terminal + 10_000;
        let receipt_match = buffer.take_matching_receipt(&hash, len, t_chrome, 2000);
        assert!(
            receipt_match.is_none(),
            "Old terminal receipt must not match distant Chrome event"
        );

        let clips = repo.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(clips[0].domain, None);
    }

    #[test]
    fn test_scenario_d_stale_receipt_rejected() {
        let repo = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "stale receipt content";
        let norm = normalize_content(text);
        let hash = content_hash(&norm);
        let len = norm.len();

        let (_clip, receipt) = repo
            .upsert_clip(NewClip {
                content: text,
                domain: None,
                page_title: None,
                now,
            })
            .unwrap();
        let stale_receipt = receipt.unwrap();

        let _ = repo.mark_clip_copied(&stale_receipt.clip_id, now + 5000);

        let event = make_browser_event(&hash, len, "example.com", None, now + 100);
        let res = repo
            .attach_metadata_with_receipt(&event, stale_receipt)
            .unwrap();
        assert!(
            res.is_none(),
            "Stale receipt must be rejected if DB state changed"
        );
    }

    #[test]
    fn test_reconcile_crlf_and_whitespace() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;

        let test_cases = ["  日本語\r\n  ", "  Привет\r\n", "\r\n  Emoji 🚀 Test \r\n"];

        for text in test_cases {
            let normalized = normalize_content(text);
            let hash = content_hash(&normalized);
            let byte_len = normalized.len();

            let (_clip, receipt) = add_with_receipt(&r, text, None, None, now);
            let event = make_browser_event(&hash, byte_len, "example.org", Some("Title"), now + 50);
            let res = r
                .attach_metadata_with_receipt(&event, receipt.unwrap())
                .unwrap();
            assert!(
                res.is_some(),
                "reconciliation should succeed for normalized text: {}",
                text
            );
        }
    }

    #[test]
    fn test_copy_receipt_rollback() {
        let r = Repository::open_in_memory().unwrap();
        let t_old = 1_700_000_000_000i64;

        let text = "multi copy clip";
        let normalized = normalize_content(text);
        let hash = content_hash(&normalized);

        for i in 0..5 {
            let _ = add_with_receipt(&r, text, None, None, t_old + i * 1000);
        }

        let t_new = 1_700_000_100_000i64;
        let (_, receipt) = add_with_receipt(&r, text, None, None, t_new);

        let event = make_browser_event(
            &hash,
            normalized.len(),
            "github.com",
            Some("GitHub Page"),
            t_new,
        );
        let canon = r
            .attach_metadata_with_receipt(&event, receipt.unwrap())
            .unwrap();
        assert!(canon.is_some());

        let clips = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(clips.len(), 2);

        let github_clip = clips
            .iter()
            .find(|c| c.domain.as_deref() == Some("github.com"))
            .unwrap();
        let domainless_clip = clips.iter().find(|c| c.domain.is_none()).unwrap();

        assert_eq!(github_clip.copy_count, 1);
        assert_eq!(github_clip.last_copied_at, t_new);

        assert_eq!(domainless_clip.copy_count, 5);
        assert_eq!(domainless_clip.last_copied_at, t_old + 4000);

        assert_eq!(github_clip.copy_count, 1);
        assert_eq!(github_clip.last_copied_at, t_new);

        assert_eq!(domainless_clip.copy_count, 5);
        assert_eq!(domainless_clip.last_copied_at, t_old + 4000);
    }

    #[test]
    fn test_short_search_query() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        add(&r, "Learning JS today", None, None, now);
        add(&r, "The 猫 is soft", None, None, now + 100);
        add(&r, "Building AI agent", None, None, now + 200);

        for q in ["JS", "猫", "AI", "日本"] {
            let res = r
                .list_clips(&ClipQuery {
                    search: Some(q.into()),
                    ..Default::default()
                })
                .unwrap();

            if q == "日本" {
                assert_eq!(res.len(), 0);
            } else {
                assert_eq!(res.len(), 1, "Short query '{}' should match 1 clip", q);
            }
        }
    }

    #[test]
    fn migration6_handles_integer_second_timestamps() {
        // Simulate a DB with INTEGER-second timestamps (pre-2000 in ms scale).
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // Create schema without migrations to simulate legacy state.
        conn.execute_batch(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
             CREATE TABLE clips (
                 id TEXT PRIMARY KEY NOT NULL,
                 content TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 content_type TEXT NOT NULL,
                 domain TEXT,
                 domain_key TEXT NOT NULL DEFAULT '',
                 page_title TEXT,
                 created_at INTEGER NOT NULL,
                 last_copied_at INTEGER NOT NULL,
                 copy_count INTEGER NOT NULL DEFAULT 1,
                 pinned INTEGER NOT NULL DEFAULT 0,
                 sort_key INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(content_hash, domain_key)
             );
             CREATE TABLE user_categories (
                 id TEXT PRIMARY KEY NOT NULL,
                 name TEXT NOT NULL,
                 normalized_name TEXT NOT NULL UNIQUE,
                 color TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 sort_order INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE clip_user_categories (
                 clip_id TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
                 category_id TEXT NOT NULL REFERENCES user_categories(id) ON DELETE CASCADE,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (clip_id, category_id)
             );
             INSERT INTO schema_migrations VALUES(1,datetime('now'));
             INSERT INTO schema_migrations VALUES(2,datetime('now'));
             INSERT INTO schema_migrations VALUES(3,datetime('now'));
             INSERT INTO schema_migrations VALUES(4,datetime('now'));
             INSERT INTO schema_migrations VALUES(5,datetime('now'));
             -- Insert clip with INTEGER-second timestamp (Unix seconds, ~2023).
             INSERT INTO clips VALUES('id1','hello world','aabbcc','Text',NULL,'',NULL,1700000000,1700000000,1,0,0);",
        ).unwrap();

        let repo = Repository {
            connection: Mutex::new(conn),
            blob_store: None,
        };
        repo.migrate().unwrap();

        let db = repo.connection.lock();
        let ts: i64 = db
            .query_row("SELECT created_at FROM clips WHERE id='id1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // 1700000000 seconds × 1000 = 1700000000000 ms
        assert_eq!(
            ts, 1_700_000_000_000,
            "timestamp should be converted to milliseconds"
        );
    }

    #[test]
    fn fts_search_cyrillic_and_japanese() {
        let r = Repository::open_in_memory().unwrap();
        add(
            &r,
            "Привет мир на русском языке",
            None,
            Some("Русская страница"),
            1_700_000_000_000,
        );
        add(
            &r,
            "日本語のテキスト",
            None,
            Some("Japanese content"),
            1_700_000_001_000,
        );
        add(&r, "unrelated english text", None, None, 1_700_000_002_000);

        let ru = r
            .list_clips(&ClipQuery {
                search: Some("Привет".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(ru.len(), 1, "Cyrillic search should find Russian clip");

        let ja = r
            .list_clips(&ClipQuery {
                search: Some("日本語".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            ja.len(),
            1,
            "Japanese search by content should find Japanese clip"
        );

        let en = r
            .list_clips(&ClipQuery {
                search: Some("english".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(en.len(), 1, "English search should find English clip");
    }

    #[test]
    fn full_legacy_pastily_v1_migration_preserves_categories_and_pins() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();

        conn.execute_batch(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
             INSERT INTO schema_migrations VALUES(1, '2023-01-01T00:00:00Z');

             CREATE TABLE clips (
                 id TEXT PRIMARY KEY NOT NULL,
                 content TEXT NOT NULL,
                 normalized_content TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 content_type TEXT NOT NULL,
                 domain TEXT,
                 page_title TEXT,
                 created_at TEXT NOT NULL,
                 last_copied_at TEXT NOT NULL,
                 copy_count INTEGER NOT NULL DEFAULT 1,
                 pinned INTEGER NOT NULL DEFAULT 0,
                 sort_key INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(normalized_content, domain)
             );

             CREATE TABLE user_categories (
                 id TEXT PRIMARY KEY NOT NULL,
                 name TEXT NOT NULL,
                 normalized_name TEXT NOT NULL UNIQUE,
                 color TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 sort_order INTEGER NOT NULL DEFAULT 0
             );

             CREATE TABLE clip_user_categories (
                 clip_id TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
                 category_id TEXT NOT NULL REFERENCES user_categories(id) ON DELETE CASCADE,
                 created_at TEXT NOT NULL,
                 PRIMARY KEY (clip_id, category_id)
             );

             INSERT INTO user_categories VALUES('cat1', 'Work', 'work', '#123456', '2023-05-01T10:00:00Z', 0);
             INSERT INTO clips VALUES('id1', 'hello world', 'hello world', '5eb63bbbe01eeed093cb22bb8f5acdc3', 'Text', NULL, 'Title 1', '2023-05-01T12:00:00Z', '2023-05-01T12:00:00Z', 5, 1, 10);
             INSERT INTO clips VALUES('id2', 'hello world', 'hello world', '5eb63bbbe01eeed093cb22bb8f5acdc3', 'Text', NULL, 'Title 2', '2023-05-02T12:00:00Z', '2023-05-02T14:00:00Z', 3, 0, 15);
             INSERT INTO clip_user_categories VALUES('id2', 'cat1', '2023-05-02T12:00:00Z');",
        ).unwrap();

        let repo = Repository {
            connection: Mutex::new(conn),
            blob_store: None,
        };
        repo.migrate().unwrap();

        let db = repo.connection.lock();

        let mut fk_stmt = db.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(
            !fk_stmt.exists([]).unwrap(),
            "Foreign key check should pass with 0 errors"
        );

        let has_norm: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('clips') WHERE name='normalized_content')",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(!has_norm, "normalized_content column must be removed");

        let clips: Vec<(String, i64, i64, bool, i64)> = db
            .prepare("SELECT id, copy_count, created_at, pinned, last_copied_at FROM clips")
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(
            clips.len(),
            1,
            "Duplicate clips should be merged into 1 canonical clip"
        );
        let (canonical_id, copy_count, created_at, pinned, last_copied) = &clips[0];
        assert_eq!(canonical_id, "id1");
        assert_eq!(*copy_count, 8, "copy_count should be summed (5 + 3 = 8)");
        assert!(*pinned, "pinned status should be preserved (max(1, 0) = 1)");
        assert!(
            *created_at > 1_600_000_000_000,
            "created_at TEXT must be converted to Unix ms"
        );
        assert!(
            *last_copied > 1_600_000_000_000,
            "last_copied_at TEXT must be converted to Unix ms"
        );

        let cats: Vec<(String, String)> = db
            .prepare("SELECT clip_id, category_id FROM clip_user_categories")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(
            cats.len(),
            1,
            "Category link must be preserved during migration"
        );
        assert_eq!(
            cats[0].0, "id1",
            "Category link must point to canonical clip ID"
        );
        assert_eq!(cats[0].1, "cat1");
    }

    #[test]
    fn fts_search_special_chars_do_not_crash() {
        let r = Repository::open_in_memory().unwrap();
        add(&r, "test content", None, None, 1_700_000_000_000);
        // Queries that could break naive FTS injection.
        for q in ["%", "_", "\"", "'", "AND", "OR", "NOT", "NEAR", ".", "---"] {
            let _ = r
                .list_clips(&ClipQuery {
                    search: Some(q.into()),
                    ..Default::default()
                })
                .unwrap(); // Should not panic or error.
        }
    }

    #[test]
    fn fts_trigger_not_fired_on_copy_count_update() {
        let r = Repository::open_in_memory().unwrap();
        let clip = add(&r, "trigger test", None, None, 1_700_000_000_000);
        // Touching a clip should NOT update FTS (only copy stats change).
        // We verify by checking FTS rowcount stays consistent.
        let db = r.connection.lock();
        let count_before: i64 = db
            .query_row("SELECT COUNT(*) FROM clips_fts", [], |r| r.get(0))
            .unwrap_or(0);
        drop(db);
        r.mark_clip_copied(&clip.id, 1_700_000_100_000).unwrap();
        let db = r.connection.lock();
        let count_after: i64 = db
            .query_row("SELECT COUNT(*) FROM clips_fts", [], |r| r.get(0))
            .unwrap_or(0);
        // FTS count should not have increased (no delete+insert cycle).
        assert_eq!(
            count_before, count_after,
            "FTS should not be updated on copy_count change"
        );
    }

    #[test]
    fn search_escapes_backslashes_properly() {
        let r = Repository::open_in_memory().unwrap();
        let clip1 = add(
            &r,
            "path\\to\\file",
            Some("example.com"),
            None,
            1_700_000_000_000,
        );
        let clip2 = add(
            &r,
            "unrelated content",
            Some("example.com"),
            None,
            1_700_000_001_000,
        );

        let res = r
            .list_clips(&ClipQuery {
                search: Some("path\\to".into()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, clip1.id);
        assert_ne!(res[0].id, clip2.id);
    }

    #[test]
    fn test_exact_content_preservation_on_receipt_reconciliation() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000;

        // 1. Initial temp copy "foo"
        let (c1, r1) = r
            .upsert_clip(NewClip {
                content: "foo",
                domain: None,
                page_title: None,
                now,
            })
            .unwrap();
        let receipt1 = r1.unwrap();
        assert_eq!(receipt1.previous_copy_count, 0);

        // 2. Second temp copy with exact text "foo " (same hash)
        let (c2, r2) = r
            .upsert_clip(NewClip {
                content: "foo ",
                domain: None,
                page_title: None,
                now: now + 1000,
            })
            .unwrap();
        let receipt2 = r2.unwrap();
        assert_eq!(c1.id, c2.id);
        assert_eq!(receipt2.previous_copy_count, 1);
        assert_eq!(receipt2.previous_content.as_deref(), Some("foo"));

        // Temp clip content is updated to "foo "
        let content = r.get_clip_content(&c1.id).unwrap();
        assert_eq!(content, "foo ");

        // 3. Late Chrome metadata arrives for second copy (receipt2)
        let event = crate::browser_metadata::BrowserCopyEvent {
            event_id: Uuid::new_v4(),
            version: 1,
            event: "copy".into(),
            content_hash: receipt2.content_hash.clone(),
            content_length: normalize_content("foo ").len(),
            domain: "example.com".into(),
            page_title: "Foo Page".into(),
            timestamp: chrono::DateTime::from_timestamp_millis(now + 1000)
                .unwrap()
                .to_rfc3339(),
        };

        let domain_id = r
            .attach_metadata_with_receipt(&event, receipt2)
            .unwrap()
            .unwrap();

        // 4. Verify domain clip was created with exact content "foo "
        let domain_content = r.get_clip_content(&domain_id).unwrap();
        assert_eq!(domain_content, "foo ");

        // 5. Verify temp clip was rolled back to original content "foo"
        let temp_content = r.get_clip_content(&c1.id).unwrap();
        assert_eq!(temp_content, "foo");
    }

    #[test]
    fn test_image_clip_late_reconciliation_via_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let r = Repository::open_in_memory_with_blobs(&temp.path().join("blobs")).unwrap();
        let now = 1_700_000_000_000;
        let image = crate::domain::ImagePayload {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            source_mime: Some("image/png".into()),
            source_bytes: None,
        };
        let (hash, len) = crate::domain::ClipboardPayload::Image(image.clone())
            .match_key()
            .unwrap();

        // Save image without domain metadata -> creates receipt
        let (summary, receipt_opt) = r
            .upsert_image(NewImageClip {
                image: &image,
                domain: None,
                page_title: None,
                now,
                max_image_bytes: 10 * 1024 * 1024,
                max_storage_bytes: 50 * 1024 * 1024,
            })
            .unwrap();

        assert_eq!(summary.domain, None);
        assert_eq!(summary.payload_kind, crate::domain::PayloadKind::Image);
        let receipt = receipt_opt.expect("missing domain image upsert generates a receipt");
        assert_eq!(receipt.normalized_length_bytes, len);

        // Delayed Chrome metadata event arrives
        let event = crate::browser_metadata::BrowserCopyEvent {
            event_id: Uuid::new_v4(),
            version: 1,
            event: "copy".into(),
            content_hash: hash,
            content_length: len,
            domain: "chrome.org".into(),
            page_title: "Chrome Image".into(),
            timestamp: chrono::DateTime::from_timestamp_millis(now).unwrap().to_rfc3339(),
        };

        let attached_id = r
            .attach_metadata_with_receipt(&event, receipt)
            .unwrap()
            .expect("metadata successfully attached");

        let updated = r
            .list_clips(&ClipQuery::default())
            .unwrap()
            .into_iter()
            .find(|c| c.id == attached_id)
            .unwrap();

        assert_eq!(updated.domain.as_deref(), Some("chrome.org"));
        assert_eq!(updated.page_title.as_deref(), Some("Chrome Image"));
        assert_eq!(updated.payload_kind, crate::domain::PayloadKind::Image);
    }

    #[test]
    fn test_text_filter_excludes_image_clips() {
        let temp = tempfile::tempdir().unwrap();
        let r = Repository::open_in_memory_with_blobs(&temp.path().join("blobs")).unwrap();
        let now = 1_700_000_000_000;

        // Save a text clip
        r.upsert_clip(NewClip {
            content: "Hello World",
            domain: None,
            page_title: None,
            now,
        })
        .unwrap();

        // Save an image clip
        let image = crate::domain::ImagePayload {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            source_mime: Some("image/png".into()),
            source_bytes: None,
        };
        r.upsert_image(NewImageClip {
            image: &image,
            domain: None,
            page_title: None,
            now: now + 100,
            max_image_bytes: 10 * 1024 * 1024,
            max_storage_bytes: 50 * 1024 * 1024,
        })
        .unwrap();

        // Query with contentType = Text (payload_kind not specified)
        let text_filtered = r
            .list_clips(&ClipQuery {
                content_type: Some(crate::domain::ContentType::Text),
                payload_kind: None,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(text_filtered.len(), 1);
        assert_eq!(text_filtered[0].payload_kind, crate::domain::PayloadKind::Text);
        assert_eq!(text_filtered[0].preview, "Hello World");

        // Query with payload_kind = Image
        let image_filtered = r
            .list_clips(&ClipQuery {
                payload_kind: Some(crate::domain::PayloadKind::Image),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(image_filtered.len(), 1);
        assert_eq!(image_filtered[0].payload_kind, crate::domain::PayloadKind::Image);
    }
}
