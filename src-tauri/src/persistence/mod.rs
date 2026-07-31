use crate::domain::{
    classify, content_hash, normalize_content, normalize_domain, Category, Clip, ClipQuery,
    ContentType, NewClip,
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension, Row};
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
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_clip(&self, input: NewClip<'_>) -> Result<Clip> {
        let normalized = normalize_content(input.content);
        anyhow::ensure!(!normalized.is_empty(), "пустой Clipboard не сохраняется");
        anyhow::ensure!(normalized.len() <= 1_000_000, "текст превышает лимит 1 МБ");
        let domain = input.domain.and_then(normalize_domain);
        let title = input
            .page_title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(500).collect::<String>());
        let hash = content_hash(&normalized);
        let kind = classify(&normalized);
        let mut db = self.connection.lock();
        let tx = db.transaction()?;
        let existing: Option<String> = match domain.as_deref() {
            Some(d) => tx
                .query_row(
                    "SELECT id FROM clips WHERE normalized_content=?1 AND domain=?2",
                    params![normalized, d],
                    |r| r.get(0),
                )
                .optional()?,
            None => tx
                .query_row(
                    "SELECT id FROM clips WHERE normalized_content=?1 AND domain IS NULL",
                    params![normalized],
                    |r| r.get(0),
                )
                .optional()?,
        };
        let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
        let changed = tx.execute("UPDATE clips SET last_copied_at=?2, copy_count=copy_count+1, page_title=COALESCE(?3,page_title), sort_key=sort_key+1 WHERE id=?1", params![id, input.now, title])?;
        if changed == 0 {
            tx.execute("INSERT INTO clips(id,content,normalized_content,content_hash,content_type,domain,page_title,created_at,last_copied_at,copy_count,pinned,sort_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,1,0,0)", params![id, input.content, normalized, hash, kind.as_str(), domain, title, input.now])?;
        }
        let clip = load_clip(&tx, &id)?;
        tx.commit()?;
        Ok(clip)
    }

    pub fn list_clips(&self, query: &ClipQuery) -> Result<Vec<Clip>> {
        let db = self.connection.lock();
        let search = query.search.as_deref().unwrap_or("").trim().to_lowercase();
        let like = format!("%{}%", search.replace('%', "\\%").replace('_', "\\_"));
        let kind = query.content_type.map(ContentType::as_str);
        let limit = query.limit.unwrap_or(100).clamp(1, 200);
        let offset = query.offset.unwrap_or(0);
        let mut statement = db.prepare(
            "SELECT DISTINCT c.id FROM clips c LEFT JOIN clip_user_categories cc ON cc.clip_id=c.id LEFT JOIN user_categories uc ON uc.id=cc.category_id
             WHERE (?1='' OR pastily_lower(c.content) LIKE ?2 ESCAPE '\\' OR pastily_lower(COALESCE(c.domain,'')) LIKE ?2 ESCAPE '\\' OR pastily_lower(COALESCE(c.page_title,'')) LIKE ?2 ESCAPE '\\' OR pastily_lower(COALESCE(uc.name,'')) LIKE ?2 ESCAPE '\\')
             AND (?3 IS NULL OR c.content_type=?3) AND (?4 IS NULL OR c.domain=?4) AND (?5 IS NULL OR cc.category_id=?5)
             ORDER BY c.last_copied_at DESC LIMIT ?6 OFFSET ?7")?;
        let ids = statement
            .query_map(
                params![
                    search,
                    like,
                    kind,
                    query.domain,
                    query.category_id,
                    limit,
                    offset
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter().map(|id| load_clip(&db, id)).collect()
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
    pub fn cleanup(&self, days: u32, now: &str) -> Result<usize> {
        Ok(self.connection.lock().execute(
            "DELETE FROM clips WHERE pinned=0 AND julianday(last_copied_at) < julianday(?1, ?2)",
            params![now, format!("-{days} days")],
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
    pub fn create_category(&self, name: &str, color: &str, now: &str) -> Result<Category> {
        let (name, normalized_name) = normalize_category_name(name)?;
        validate_color(color)?;
        let category = Category {
            id: Uuid::new_v4().to_string(),
            name,
            color: color.into(),
            created_at: now.into(),
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
    pub fn assign_category(&self, clip: &str, category: &str, now: &str) -> Result<()> {
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
        "pastily_lower",
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
fn load_clip(db: &Connection, id: &str) -> Result<Clip> {
    let mut clip = db.query_row("SELECT id,content,content_type,domain,page_title,created_at,last_copied_at,copy_count,pinned FROM clips WHERE id=?1", [id], |r| {
        let kind: String = r.get(2)?;
        Ok(Clip { id:r.get(0)?,content:r.get(1)?,content_type:match kind.as_str(){"Links"=>ContentType::Links,"Email"=>ContentType::Email,"Numbers"=>ContentType::Numbers,_=>ContentType::Text},domain:r.get(3)?,page_title:r.get(4)?,created_at:r.get(5)?,last_copied_at:r.get(6)?,copy_count:r.get(7)?,pinned:r.get(8)?,categories:vec![] })
    })?;
    let mut s=db.prepare("SELECT uc.id,uc.name,uc.color,uc.created_at,uc.sort_order FROM user_categories uc JOIN clip_user_categories cc ON cc.category_id=uc.id WHERE cc.clip_id=?1 ORDER BY uc.sort_order,uc.name")?;
    clip.categories = s
        .query_map([id], row_category)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(clip)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn add<'a>(
        repo: &Repository,
        text: &'a str,
        domain: Option<&'a str>,
        title: Option<&'a str>,
        now: &'a str,
    ) -> Clip {
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
            "2026-01-01T00:00:00Z",
        );
        let b = add(
            &r,
            "hello",
            Some("github.com"),
            Some("B"),
            "2026-01-02T00:00:00Z",
        );
        let c = add(
            &r,
            "hello",
            Some("youtube.com"),
            None,
            "2026-01-03T00:00:00Z",
        );
        let d = add(&r, "hello", None, None, "2026-01-04T00:00:00Z");
        assert_eq!(a.id, b.id);
        assert_eq!(b.copy_count, 2);
        assert_eq!(b.page_title.as_deref(), Some("B"));
        assert_eq!(b.last_copied_at, "2026-01-02T00:00:00Z");
        assert_ne!(b.id, c.id);
        assert_ne!(c.id, d.id);
    }
    #[test]
    fn cleanup_preserves_pins() {
        let r = Repository::open_in_memory().unwrap();
        let old = add(&r, "old", None, None, "2025-01-01T00:00:00Z");
        let pin = add(&r, "pin", None, None, "2025-01-01T00:00:00Z");
        r.set_pinned(&pin.id, true).unwrap();
        assert_eq!(r.cleanup(90, "2026-01-01T00:00:00Z").unwrap(), 1);
        let all = r.list_clips(&ClipQuery::default()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, pin.id);
        assert_ne!(old.id, pin.id);
    }
    #[test]
    fn categories_are_many_to_many_and_delete_cascades_only_links() {
        let r = Repository::open_in_memory().unwrap();
        let clip = add(&r, "x", None, None, "2026-01-01T00:00:00Z");
        let a = r
            .create_category("Japanese", "#ffd166", "2026-01-01T00:00:00Z")
            .unwrap();
        let b = r
            .create_category("Saved", "#70c1b3", "2026-01-01T00:00:00Z")
            .unwrap();
        r.assign_category(&clip.id, &a.id, "now").unwrap();
        r.assign_category(&clip.id, &b.id, "now").unwrap();
        r.assign_category(&clip.id, &a.id, "now").unwrap();
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
            "2026-01-01T00:00:00Z",
        );
        let cat = r.create_category("Японский", "#123abc", "now").unwrap();
        r.assign_category(&c.id, &cat.id, "now").unwrap();
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
        assert!(r.create_category("яПОНСКИЙ", "#abcdef", "now").is_err());
        r.migrate().unwrap();
    }
}
