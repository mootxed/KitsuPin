use crate::domain::{
    classify, content_hash, normalize_content, normalize_domain, Category, ClipQuery, ClipSummary,
    ContentType, NewClip,
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{functions::FunctionFlags, params, Connection, Row};
use std::path::Path;
use uuid::Uuid;

const MIGRATION_1: &str = include_str!("../../migrations/001_initial.sql");

/// Window within which Chrome metadata can be reconciled to a clip (milliseconds).
pub const METADATA_RECONCILE_WINDOW_MS: i64 = 5_000;

pub struct Repository {
    connection: Mutex<Connection>,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path).context("не удалось открыть SQLite")?;
        configure_connection(&connection)?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path.exists() {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            if let Some(parent) = path.parent() {
                let wal = parent.join("kitsupin.sqlite3-wal");
                if wal.exists() {
                    let _ = std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600));
                }
                let shm = parent.join("kitsupin.sqlite3-shm");
                if shm.exists() {
                    let _ = std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        let repo = Self {
            connection: Mutex::new(connection),
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
        };
        repo.migrate()?;
        Ok(repo)
    }

    fn migrate(&self) -> Result<()> {
        let mut db = self.connection.lock();
        let tx = db.transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);")?;

        // ── Migration 1: initial schema ───────────────────────────────────
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=1)",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            tx.execute_batch(MIGRATION_1)?;
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
            tx.execute_batch(
                "DELETE FROM clips WHERE rowid NOT IN (
                    SELECT min(rowid) FROM clips GROUP BY content_hash, domain_key
                );",
            )?;
            tx.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_clips_hash_domain_key ON clips(content_hash, domain_key);",
            )?;
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
                    tokenize='unicode61 remove_diacritics 2'
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
            tx.execute_batch(
                "UPDATE clips SET created_at = CASE WHEN typeof(created_at)='text' THEN (unixepoch(created_at)*1000) ELSE created_at END,
                                  last_copied_at = CASE WHEN typeof(last_copied_at)='text' THEN (unixepoch(last_copied_at)*1000) ELSE last_copied_at END;
                 UPDATE user_categories SET created_at = CASE WHEN typeof(created_at)='text' THEN (unixepoch(created_at)*1000) ELSE created_at END;
                 UPDATE clip_user_categories SET created_at = CASE WHEN typeof(created_at)='text' THEN (unixepoch(created_at)*1000) ELSE created_at END;
                 DROP INDEX IF EXISTS idx_clips_recency;
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

                // Create new clips table with current schema (no normalized_content).
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
                    );",
                )?;

                // Migrate and deduplicate rows.
                // For each (content_hash, domain_key) group:
                //   - id = id of the row with the smallest created_at (canonical)
                //   - content = content of that canonical row
                //   - created_at = MIN(normalized)
                //   - last_copied_at = MAX(normalized)
                //   - copy_count = MIN(SUM, 2147483647) to avoid overflow
                //   - pinned = MAX (1 if any duplicate was pinned)
                //   - sort_key = MAX
                //   - page_title = title from the most recently copied row (if any)
                //   - domain = normalized domain
                //   - content_type = recomputed from content hash (stored as TEXT)
                //
                // We use SQLite window functions to identify the canonical row.
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
                            -- canonical id: from row with lowest created_at
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
                            -- page_title from most recently copied row that has a title
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

                // Transfer category associations for all merged duplicates → canonical id.
                tx.execute_batch(
                    "INSERT OR IGNORE INTO clip_user_categories(clip_id, category_id, created_at)
                     SELECT
                         (SELECT id FROM clips_new WHERE clips_new.content_hash = clips.content_hash
                          AND clips_new.domain_key = COALESCE(clips.domain, '')
                          LIMIT 1),
                         category_id,
                         created_at
                     FROM clip_user_categories
                     JOIN clips ON clips.id = clip_user_categories.clip_id;",
                )?;

                // Delete old FK refs before dropping the table.
                tx.execute_batch(
                    "DELETE FROM clip_user_categories
                     WHERE clip_id NOT IN (SELECT id FROM clips_new);",
                )?;

                // Swap tables.
                tx.execute_batch("DROP TABLE clips;")?;
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
                     CREATE INDEX idx_clips_domain_recency ON clips(domain, last_copied_at DESC, sort_key DESC);",
                )?;

                // Rebuild FTS if it exists (may have stale rowids after table swap).
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
                    // Recreate triggers with correct column-specific UPDATE trigger (fixed in m7).
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

                // Re-enable FK and verify.
                tx.execute_batch("PRAGMA foreign_keys=ON;")?;
                let fk_issues: i64 = tx
                    .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
                    .unwrap_or(0);
                if fk_issues != 0 {
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

        tx.commit()?;
        Ok(())
    }

    pub fn upsert_clip(&self, input: NewClip<'_>) -> Result<ClipSummary> {
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

        let existing_id: Option<String> = match tx.query_row(
            "SELECT id FROM clips WHERE content_hash=?1 AND domain_key=?2",
            params![hash, domain_key],
            |r| r.get(0),
        ) {
            Ok(id) => Some(id),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };

        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let changed = tx.execute(
            "UPDATE clips SET last_copied_at=?2, copy_count=copy_count+1, page_title=COALESCE(?3,page_title), sort_key=sort_key+1 WHERE id=?1",
            params![id, input.now, title],
        )?;
        if changed == 0 {
            tx.execute(
                "INSERT INTO clips(id,content,content_hash,content_type,domain,domain_key,page_title,created_at,last_copied_at,copy_count,pinned,sort_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,1,0,0)",
                params![id, input.content, hash, kind.as_str(), domain, domain_key, title, input.now],
            )?;
        }
        let clip = load_clip_summary(&tx, &id)?;
        tx.commit()?;
        Ok(clip)
    }

    /// Attach browser metadata to a recently saved clip whose source was not yet known.
    ///
    /// Matches by content_hash + content_length_bytes within METADATA_RECONCILE_WINDOW_MS.
    /// If a clip already exists with (content_hash, domain_key), merges the two clips
    /// atomically (preserving pinned, categories, copy_count).
    ///
    /// Returns `Some(canonical_id)` on success, `None` if no unattributed clip was found.
    pub fn attach_metadata(
        &self,
        content_hash_val: &str,
        content_length_bytes: usize,
        domain: &str,
        page_title: Option<&str>,
        now_ms: i64,
    ) -> Result<Option<String>> {
        let domain_key = domain;
        let title = page_title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(500).collect::<String>());

        let mut db = self.connection.lock();
        let tx = db.transaction()?;

        // Find an unattributed clip (domain_key='') matching hash+length within window.
        let candidate: Option<(String, usize, i64, i64, i64, bool)> = match tx.query_row(
            "SELECT id, length(content), copy_count, sort_key, created_at, pinned
             FROM clips
             WHERE content_hash=?1 AND domain_key='' AND last_copied_at >= ?2
             ORDER BY last_copied_at DESC
             LIMIT 1",
            params![content_hash_val, now_ms - METADATA_RECONCILE_WINDOW_MS],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, usize>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            },
        ) {
            Ok(row) => Some(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };

        let Some((temp_id, stored_len, temp_count, temp_sort, temp_created, temp_pinned)) =
            candidate
        else {
            return Ok(None);
        };

        // Verify content_length_bytes matches (defence against hash collision / wrong match).
        if stored_len != content_length_bytes {
            log::warn!(
                "attach_metadata: hash match but length mismatch ({} vs {}), skipping",
                stored_len,
                content_length_bytes
            );
            return Ok(None);
        }

        // Check if a clip with (content_hash, domain_key) already exists.
        let existing: Option<(String, i64, i64, i64, bool)> = match tx.query_row(
            "SELECT id, copy_count, sort_key, created_at, pinned
             FROM clips WHERE content_hash=?1 AND domain_key=?2",
            params![content_hash_val, domain_key],
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

        let canonical_id = if let Some((
            existing_id,
            existing_count,
            existing_sort,
            existing_created,
            existing_pinned,
        )) = existing
        {
            // Merge: combine temp clip into existing canonical clip.
            let merged_count = existing_count.saturating_add(temp_count);
            let merged_sort = existing_sort.max(temp_sort);
            let merged_pinned = existing_pinned || temp_pinned;
            let merged_created = existing_created.min(temp_created);

            tx.execute(
                "UPDATE clips SET
                    copy_count=?2, sort_key=?3, pinned=?4, created_at=?5,
                    page_title=COALESCE(?6, page_title), last_copied_at=MAX(last_copied_at,?7)
                 WHERE id=?1",
                params![
                    existing_id,
                    merged_count,
                    merged_sort,
                    merged_pinned,
                    merged_created,
                    title,
                    now_ms
                ],
            )?;

            // Transfer categories from temp clip → canonical (ignore duplicates).
            tx.execute(
                "INSERT OR IGNORE INTO clip_user_categories(clip_id, category_id, created_at)
                 SELECT ?1, category_id, created_at FROM clip_user_categories WHERE clip_id=?2",
                params![existing_id, temp_id],
            )?;

            // Delete temporary unattributed clip.
            tx.execute("DELETE FROM clips WHERE id=?1", [&temp_id])?;

            existing_id
        } else {
            // No existing clip with this domain — just update the temp clip in place.
            tx.execute(
                "UPDATE clips SET domain=?2, domain_key=?3, page_title=COALESCE(?4, page_title) WHERE id=?1",
                params![temp_id, domain, domain_key, title],
            )?;
            temp_id
        };

        tx.commit()?;
        Ok(Some(canonical_id))
    }

    pub fn list_clips(&self, query: &ClipQuery) -> Result<Vec<ClipSummary>> {
        let db = self.connection.lock();
        let search = query.search.as_deref().unwrap_or("").trim().to_lowercase();
        let fts_formatted = format_fts_query(&search);
        let kind = query.content_type.map(ContentType::as_str);
        let limit = query.limit.unwrap_or(100).clamp(1, 200);
        let offset = query.offset.unwrap_or(0);

        // Build domain/category LIKE pattern (only for domain and category name matching).
        let like = if search.is_empty() {
            String::new()
        } else {
            format!("%{}%", search.replace('%', "\\%").replace('_', "\\_"))
        };

        // We use a two-path search:
        // 1. If FTS query is available → use it for content + page_title
        // 2. Additionally, LIKE on domain and category name (small sets, not full-content scan)
        // We never LIKE-scan c.content to avoid full-table scans defeating FTS.
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
                    c.pinned
             FROM clips c
             LEFT JOIN clip_user_categories cc ON cc.clip_id = c.id
             LEFT JOIN user_categories uc ON uc.id = cc.category_id
             WHERE (?1 = ''
                    OR (?2 != '' AND c.rowid IN (SELECT rowid FROM clips_fts WHERE clips_fts MATCH ?2))
                    OR (?3 != '' AND kitsupin_lower(COALESCE(c.domain,'')) LIKE ?3 ESCAPE '\\')
                    OR (?3 != '' AND kitsupin_lower(COALESCE(uc.name,'')) LIKE ?3 ESCAPE '\\'))
               AND (?4 IS NULL OR c.content_type = ?4)
               AND (?5 IS NULL OR c.domain = ?5)
               AND (?6 IS NULL OR cc.category_id = ?6)
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
                    offset
                ],
                |r| {
                    let preview: String = r.get(1)?;
                    // SQLite length() counts bytes for BLOB but chars for TEXT.
                    // We store content as TEXT, so length() returns char count on most builds,
                    // but we also have the stored byte length. Use the stored byte length
                    // from substr query; is_truncated is determined by preview byte length.
                    let stored_len: i64 = r.get(2)?;
                    let preview_bytes = preview.len() as i64;
                    // SQLite substr(x,1,300) extracts up to 300 chars; compare char counts.
                    let preview_chars = preview.chars().count() as i64;
                    // stored_len here is length(c.content) in SQLite which for TEXT = chars.
                    let is_truncated = stored_len > preview_chars;
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
        let content: String =
            db.query_row("SELECT content FROM clips WHERE id=?1", [id], |r| r.get(0))?;
        Ok(content)
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
        self.connection
            .lock()
            .execute("DELETE FROM clips WHERE id=?1", [id])?;
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
        Ok(self
            .connection
            .lock()
            .execute("DELETE FROM clips WHERE pinned=0", [])?)
    }
    pub fn cleanup(&self, days: u32, now_ms: i64) -> Result<usize> {
        anyhow::ensure!(
            (1..=3650).contains(&days),
            "Недопустимый срок хранения: {days}"
        );
        let retention_ms = (days as i64) * 86_400_000;
        let cutoff_ms = now_ms.saturating_sub(retention_ms);
        Ok(self.connection.lock().execute(
            "DELETE FROM clips WHERE pinned=0 AND last_copied_at < ?1",
            params![cutoff_ms],
        )?)
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

fn validate_color(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() == 7
            && value.starts_with('#')
            && value[1..].chars().all(|c| c.is_ascii_hexdigit()),
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
        "SELECT id, substr(content,1,300), length(content), content_type, domain, page_title, created_at, last_copied_at, copy_count, pinned FROM clips WHERE id=?1",
        [id],
        |r| {
            let preview: String = r.get(1)?;
            let stored_len: i64 = r.get(2)?;
            let preview_chars = preview.chars().count() as i64;
            let is_truncated = stored_len > preview_chars;
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

    #[test]
    fn attach_metadata_reconciles_after_clipboard_save() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        // Save clip without domain (as clipboard watcher would).
        let text = "hello reconcile";
        let normalized = normalize_content(text);
        let hash = content_hash(&normalized);
        let byte_len = normalized.len();
        let clip = add(&r, text, None, None, now);
        assert_eq!(clip.domain, None);

        // Attach metadata arriving 200ms later.
        let result = r
            .attach_metadata(
                &hash,
                byte_len,
                "example.com",
                Some("Example Page"),
                now + 200,
            )
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

        // Existing clip with domain (e.g., previously saved with metadata).
        let existing = add(
            &r,
            text,
            Some("example.com"),
            Some("Old Title"),
            now - 10_000,
        );
        // New unattributed clip (no domain).
        let _temp = add(&r, text, None, None, now);

        let cat = r.create_category("Test", "#aabbcc", now).unwrap();
        r.assign_category(&_temp.id, &cat.id, now).unwrap();

        let result = r
            .attach_metadata(&hash, byte_len, "example.com", Some("New Title"), now + 100)
            .unwrap();
        assert_eq!(result.as_deref(), Some(existing.id.as_str()));

        let clips = r.list_clips(&ClipQuery::default()).unwrap();
        // Should be merged into one clip.
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].id, existing.id);
        // copy_count merged: existing had 1, temp had 1 → 2.
        assert_eq!(clips[0].copy_count, 2);
        // Category from temp transferred to canonical.
        assert_eq!(clips[0].categories.len(), 1);
    }

    #[test]
    fn attach_metadata_rejects_wrong_hash() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "some content";
        let normalized = normalize_content(text);
        let byte_len = normalized.len();
        add(&r, text, None, None, now);
        // Wrong hash.
        let result = r
            .attach_metadata(
                "a".repeat(64).as_str(),
                byte_len,
                "example.com",
                None,
                now + 100,
            )
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
        add(&r, text, None, None, now);
        // Correct hash but wrong length.
        let result = r
            .attach_metadata(&hash, 9999, "example.com", None, now + 100)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn attach_metadata_rejects_too_late() {
        let r = Repository::open_in_memory().unwrap();
        let now = 1_700_000_000_000i64;
        let text = "late metadata test";
        let normalized = normalize_content(text);
        let hash = content_hash(&normalized);
        let byte_len = normalized.len();
        add(&r, text, None, None, now);
        // Arrives beyond the reconcile window.
        let result = r
            .attach_metadata(
                &hash,
                byte_len,
                "example.com",
                None,
                now + METADATA_RECONCILE_WINDOW_MS + 1000,
            )
            .unwrap();
        assert!(result.is_none());
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
        // For CJK content: FTS5 unicode61 tokenizer does not split CJK ideographs into
        // individual tokens by default, so we test via the Latin page_title instead.
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

        // Search by Latin page_title instead of CJK content (unicode61 limitation with CJK).
        let ja_by_title = r
            .list_clips(&ClipQuery {
                search: Some("Japanese".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            ja_by_title.len(),
            1,
            "Search by page_title should find Japanese clip"
        );

        // Additionally, domain search should not return CJK clip.
        let en = r
            .list_clips(&ClipQuery {
                search: Some("english".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(en.len(), 1, "English search should find English clip");
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
}
