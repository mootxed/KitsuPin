use crate::domain::{
    classify, content_hash, normalize_content, normalize_domain, Category, ClipQuery,
    ClipSummary, ContentType, NewClip,
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{functions::FunctionFlags, params, Connection, Row};
use std::path::Path;
use uuid::Uuid;

const MIGRATION_1: &str = include_str!("../../migrations/001_initial.sql");

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
                tx.execute_batch("ALTER TABLE clips ADD COLUMN domain_key TEXT NOT NULL DEFAULT '';")?;
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
                tx.execute_batch("ALTER TABLE clips DROP COLUMN normalized_content;")?;
            }
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(3, datetime('now'))",
                [],
            )?;
        }
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

    pub fn list_clips(&self, query: &ClipQuery) -> Result<Vec<ClipSummary>> {
        let db = self.connection.lock();
        let search = query.search.as_deref().unwrap_or("").trim().to_lowercase();
        let like = format!("%{}%", search.replace('%', "\\%").replace('_', "\\_"));
        let fts_formatted = format_fts_query(&search);
        let kind = query.content_type.map(ContentType::as_str);
        let limit = query.limit.unwrap_or(100).clamp(1, 200);
        let offset = query.offset.unwrap_or(0);

        let mut statement = db.prepare(
            "SELECT DISTINCT c.id,
                    c.content,
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
                    OR kitsupin_lower(c.content) LIKE ?3 ESCAPE '\\'
                    OR kitsupin_lower(COALESCE(c.domain,'')) LIKE ?3 ESCAPE '\\'
                    OR kitsupin_lower(COALESCE(c.page_title,'')) LIKE ?3 ESCAPE '\\'
                    OR kitsupin_lower(COALESCE(uc.name,'')) LIKE ?3 ESCAPE '\\')
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
                    let content: String = r.get(1)?;
                    let preview = content.chars().take(300).collect::<String>();
                    let content_length = content.chars().count();
                    let kind_str: String = r.get(2)?;
                    let content_type = match kind_str.as_str() {
                        "Links" => ContentType::Links,
                        "Email" => ContentType::Email,
                        "Numbers" => ContentType::Numbers,
                        _ => ContentType::Text,
                    };
                    Ok(ClipSummary {
                        id: r.get(0)?,
                        preview,
                        content_length,
                        content_type,
                        domain: r.get(3)?,
                        page_title: r.get(4)?,
                        created_at: r.get(5)?,
                        last_copied_at: r.get(6)?,
                        copy_count: r.get(7)?,
                        pinned: r.get(8)?,
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
        let content: String = db.query_row(
            "SELECT content FROM clips WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(content)
    }

    pub fn touch_clip(&self, id: &str, now: i64) -> Result<String> {
        let db = self.connection.lock();
        let changed = db.execute(
            "UPDATE clips SET last_copied_at=?2, copy_count=copy_count+1, sort_key=sort_key+1 WHERE id=?1",
            params![id, now],
        )?;
        if changed == 0 {
            anyhow::bail!("карточка не найдена");
        }
        let content: String = db.query_row(
            "SELECT content FROM clips WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
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
        anyhow::ensure!((1..=3650).contains(&days), "Недопустимый срок хранения: {days}");
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
    let mut summary = db.query_row("SELECT id,content,content_type,domain,page_title,created_at,last_copied_at,copy_count,pinned FROM clips WHERE id=?1", [id], |r| {
        let content: String = r.get(1)?;
        let preview = content.chars().take(300).collect::<String>();
        let content_length = content.chars().count();
        let kind: String = r.get(2)?;
        Ok(ClipSummary { id:r.get(0)?,preview,content_length,content_type:match kind.as_str(){"Links"=>ContentType::Links,"Email"=>ContentType::Email,"Numbers"=>ContentType::Numbers,_=>ContentType::Text},domain:r.get(3)?,page_title:r.get(4)?,created_at:r.get(5)?,last_copied_at:r.get(6)?,copy_count:r.get(7)?,pinned:r.get(8)?,categories:vec![] })
    })?;
    let mut s = db.prepare("SELECT uc.id,uc.name,uc.color,uc.created_at,uc.sort_order FROM user_categories uc JOIN clip_user_categories cc ON cc.category_id=uc.id WHERE cc.clip_id=?1 ORDER BY uc.sort_order,uc.name")?;
    summary.categories = s
        .query_map([id], row_category)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let a = add(&r, "hello", Some("github.com"), Some("A"), 1_700_000_000_000);
        let b = add(&r, "hello", Some("github.com"), Some("B"), 1_700_000_001_000);
        let c = add(&r, "hello", Some("youtube.com"), None, 1_700_000_002_000);
        let d = add(&r, "hello", None, None, 1_700_000_003_000);
        let e = add(&r, "hello", None, Some("No Domain Title"), 1_700_000_004_000);
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
        if let Err(e) = r.set_pinned(&pin.id, true) {
            eprintln!("SET PINNED ERROR: {e:?}");
            panic!("{e:?}");
        }
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
        r.assign_category(&clip.id, &a.id, 1_700_000_000_000).unwrap();
        r.assign_category(&clip.id, &b.id, 1_700_000_000_000).unwrap();
        r.assign_category(&clip.id, &a.id, 1_700_000_000_000).unwrap();
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
        let cat = r.create_category("Японский", "#123abc", 1_700_000_000_000).unwrap();
        r.assign_category(&c.id, &cat.id, 1_700_000_000_000).unwrap();
        for q in ["ПРИВЕТ", "konn", "EXAMPLE", "study", "ЯПОНСКИЙ"] {
            assert_eq!(
                r.list_clips(&ClipQuery {
                    search: Some(q.into()),
                    ..Default::default()
                })
                .unwrap()
                .len(),
                1
            );
        }
        assert!(r.create_category("яПОНСКИЙ", "#abcdef", 1_700_000_000_000).is_err());
        r.migrate().unwrap();
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
        // Both added at exact same timestamp t. _second has sort_key 0 (inserted after).
        // Touch first item at timestamp t: last_copied_at = t, sort_key increments from 0 to 1.
        r.touch_clip(&_first.id, t).unwrap();
        let list = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(list[0].id, _first.id);
    }
}
