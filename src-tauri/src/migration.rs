use fs2::FileExt;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LegacyMigrationResult {
    NothingToMigrate,
    DirectoryMoved,
    LegacyDatabaseRestored,
    DatabasesMerged,
    ConflictPreserved,
}

fn get_xdg_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn get_xdg_config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

pub fn migrate_pastily_to_kitsupin() -> LegacyMigrationResult {
    let data_home = match get_xdg_data_home() {
        Some(path) => path,
        None => return LegacyMigrationResult::NothingToMigrate,
    };
    let config_home = get_xdg_config_home();

    migrate_pastily_to_kitsupin_at(&data_home, config_home.as_deref())
}

pub fn migrate_pastily_to_kitsupin_at(
    data_home: &Path,
    config_home: Option<&Path>,
) -> LegacyMigrationResult {
    let lock_path = data_home.join(".kitsupin-migration.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
            }
            let mut acquired = false;
            for _ in 0..30 {
                if file.try_lock_exclusive().is_ok() {
                    acquired = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            if !acquired {
                log::warn!("Migration lock held by another process; aborting migration after timeout");
                return LegacyMigrationResult::ConflictPreserved;
            }
            file
        }
        Err(e) => {
            log::error!("Failed to open migration lock at {:?}: {e}", lock_path);
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    let result = migrate_data_dir_at(data_home);

    if let Some(config_home) = config_home {
        migrate_autostart_at(config_home);
        migrate_native_host_manifests_at(config_home);
    }

    let _ = file.unlock();
    result
}

fn migrate_data_dir_at(data_home: &Path) -> LegacyMigrationResult {
    let old_dir = data_home.join("pastily");
    let new_dir = data_home.join("kitsupin");

    // Scenario A: pastily exists, kitsupin does not exist
    if old_dir.exists() && !new_dir.exists() {
        if let Some(old_db_path) = find_legacy_db(&old_dir) {
            let new_db = new_dir.join("kitsupin.sqlite3");
            let res = restore_legacy_db(&old_db_path, &new_db);
            if res == LegacyMigrationResult::ConflictPreserved {
                let _ = std::fs::remove_dir_all(&new_dir);
                return LegacyMigrationResult::ConflictPreserved;
            }
            if let Ok(entries) = std::fs::read_dir(&old_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let file_name = path.file_name().unwrap_or_default();
                    let file_name_str = file_name.to_string_lossy();
                    if !file_name_str.starts_with("pastily.sqlite3")
                        && !file_name_str.starts_with("kitsupin.sqlite3")
                    {
                        let dest = new_dir.join(file_name);
                        let _ = std::fs::copy(&path, &dest);
                    }
                }
            }
            return LegacyMigrationResult::DirectoryMoved;
        } else if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
            log::error!("Failed to rename pastily directory to kitsupin: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        } else {
            return LegacyMigrationResult::DirectoryMoved;
        }
    }

    if !old_dir.exists() && new_dir.exists() {
        normalize_db_names_in(&new_dir);
        return LegacyMigrationResult::NothingToMigrate;
    }

    if !old_dir.exists() && !new_dir.exists() {
        return LegacyMigrationResult::NothingToMigrate;
    }

    // Both old_dir and new_dir exist.
    let old_db = find_legacy_db(&old_dir);
    let new_db = new_dir.join("kitsupin.sqlite3");

    let Some(old_db_path) = old_db else {
        return LegacyMigrationResult::NothingToMigrate;
    };

    // Scenario B: pastily exists, kitsupin exists, kitsupin.sqlite3 missing
    if !new_db.exists() {
        return restore_legacy_db(&old_db_path, &new_db);
    }

    // Both databases exist. Check if new_db is empty.
    let new_clip_count = count_clips_safely(&new_db);
    let old_clip_count = count_clips_safely(&old_db_path);

    if new_clip_count == 0 {
        // Scenario C: kitsupin.sqlite3 is empty
        let backup_empty = new_dir.join("kitsupin.sqlite3.empty.bak");
        if let Err(e) = std::fs::rename(&new_db, &backup_empty) {
            log::error!("Failed to backup empty kitsupin DB: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
        let res = restore_legacy_db(&old_db_path, &new_db);
        return res;
    }

    if old_clip_count == 0 {
        // Old database is empty, nothing useful to import
        let backup_old = old_dir.join("pastily.sqlite3.empty.bak");
        let _ = backup_database_file(&old_db_path, &backup_old);
        return LegacyMigrationResult::NothingToMigrate;
    }

    // Scenario D: both databases contain user data
    merge_legacy_db_into_new(&old_db_path, &new_db)
}

fn find_legacy_db(dir: &Path) -> Option<PathBuf> {
    let p1 = dir.join("kitsupin.sqlite3");
    if p1.exists() {
        return Some(p1);
    }
    let p2 = dir.join("pastily.sqlite3");
    if p2.exists() {
        return Some(p2);
    }
    None
}

fn normalize_db_names_in(dir: &Path) {
    let old_db = dir.join("pastily.sqlite3");
    let new_db = dir.join("kitsupin.sqlite3");
    if old_db.exists() && !new_db.exists() {
        let _ = std::fs::rename(&old_db, &new_db);
    }

    let old_wal = dir.join("pastily.sqlite3-wal");
    let new_wal = dir.join("kitsupin.sqlite3-wal");
    if old_wal.exists() && !new_wal.exists() {
        let _ = std::fs::rename(&old_wal, &new_wal);
    }

    let old_shm = dir.join("pastily.sqlite3-shm");
    let new_shm = dir.join("kitsupin.sqlite3-shm");
    if old_shm.exists() && !new_shm.exists() {
        let _ = std::fs::rename(&old_shm, &new_shm);
    }
}

fn count_clips_safely(db_path: &Path) -> usize {
    let Ok(conn) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return 0;
    };

    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='clips')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);

    if !table_exists {
        return 0;
    }

    conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
        .unwrap_or(0)
}

fn ensure_foreign_keys_valid(conn: &Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check;")?;
    if stmt.exists([])? {
        anyhow::bail!("foreign key check returned violations");
    }
    Ok(())
}

fn ensure_integrity_ok(conn: &Connection) -> anyhow::Result<()> {
    let res: String = conn.query_row("PRAGMA integrity_check;", [], |r| r.get(0))?;
    if res != "ok" {
        anyhow::bail!("integrity check failed with output: {res}");
    }
    Ok(())
}

fn backup_database_file(src_db_path: &Path, dst_backup_path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst_backup_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temp_backup = dst_backup_path.with_extension("tmp_bak");
    if temp_backup.exists() {
        let _ = std::fs::remove_file(&temp_backup);
    }

    let src_conn = Connection::open_with_flags(
        src_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    let mut dst_conn = Connection::open(&temp_backup)?;
    let backup_res =
        rusqlite::backup::Backup::new(&src_conn, &mut dst_conn).and_then(|b| b.step(-1));

    if let Err(e) = backup_res {
        let _ = std::fs::remove_file(&temp_backup);
        anyhow::bail!("Backup API failed for {:?}: {e}", src_db_path);
    }

    ensure_integrity_ok(&dst_conn)?;
    drop(dst_conn);
    drop(src_conn);

    if let Ok(file) = std::fs::File::open(&temp_backup) {
        let _ = file.sync_all();
    }

    std::fs::rename(&temp_backup, dst_backup_path)?;

    let _ = std::fs::remove_file(src_db_path);
    let src_str = src_db_path.to_string_lossy();
    let wal = PathBuf::from(format!("{src_str}-wal"));
    if wal.exists() {
        let _ = std::fs::remove_file(&wal);
    }
    let shm = PathBuf::from(format!("{src_str}-shm"));
    if shm.exists() {
        let _ = std::fs::remove_file(&shm);
    }

    Ok(())
}

fn compute_db_fingerprint(db_path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(db_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn ensure_legacy_imports_table(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS legacy_imports (
            id TEXT PRIMARY KEY,
            source_fingerprint TEXT NOT NULL UNIQUE,
            imported_at INTEGER NOT NULL,
            source_path TEXT NOT NULL,
            status TEXT NOT NULL
        );",
    )?;
    Ok(())
}

fn restore_legacy_db(old_db_path: &Path, target_db_path: &Path) -> LegacyMigrationResult {
    let target_dir = match target_db_path.parent() {
        Some(d) => d,
        None => return LegacyMigrationResult::ConflictPreserved,
    };
    if let Err(e) = std::fs::create_dir_all(target_dir) {
        log::error!("Failed to create directory {:?}: {e}", target_dir);
        return LegacyMigrationResult::ConflictPreserved;
    }

    let importing_path = target_dir.join("kitsupin.sqlite3.importing");
    if importing_path.exists() {
        let _ = std::fs::remove_file(&importing_path);
    }

    // Step 1: Open source DB read-only
    let src_conn = match Connection::open_with_flags(
        old_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to open legacy DB {:?}: {e}", old_db_path);
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    // Step 2: Use SQLite Backup API into importing_path
    let mut dst_conn = match Connection::open(&importing_path) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to create destination DB {:?}: {e}", importing_path);
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    let backup_res =
        rusqlite::backup::Backup::new(&src_conn, &mut dst_conn).and_then(|b| b.step(-1));

    if let Err(e) = backup_res {
        log::error!("SQLite backup failed for {:?}: {e}", old_db_path);
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    // Step 3: Run integrity_check and foreign_key_check on imported DB
    if let Err(e) = ensure_integrity_ok(&dst_conn) {
        log::error!(
            "Integrity check failed for imported DB at {:?}: {e}",
            importing_path
        );
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    if let Err(e) = ensure_foreign_keys_valid(&dst_conn) {
        log::error!(
            "Foreign key check failed for imported DB at {:?}: {e}",
            importing_path
        );
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    drop(dst_conn);
    drop(src_conn);

    if let Ok(file) = std::fs::File::open(&importing_path) {
        let _ = file.sync_all();
    }

    // Step 4: Atomic rename importing file to final DB target
    if let Err(e) = std::fs::rename(&importing_path, target_db_path) {
        log::error!("Failed to rename importing DB to final path: {e}");
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    // Ensure target database opens & migrates correctly with current schema
    if let Err(e) = crate::persistence::Repository::open(target_db_path) {
        log::error!("Target database failed schema migration after restore: {e:?}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    // Preserve old database as backup
    let old_backup = old_db_path.with_extension("sqlite3.migrated.bak");
    if let Err(e) = backup_database_file(old_db_path, &old_backup) {
        log::error!("Failed to backup old DB: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    LegacyMigrationResult::LegacyDatabaseRestored
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let pragma_sql = format!("PRAGMA table_info({})", table);
    let mut stmt = match conn.prepare(&pragma_sql) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let cols = stmt.query_map([], |row| row.get::<_, String>(1));
    if let Ok(iter) = cols {
        for col_name in iter.flatten() {
            if col_name.eq_ignore_ascii_case(column) {
                return true;
            }
        }
    }
    false
}

fn parse_legacy_timestamp(value: rusqlite::types::ValueRef, fallback_ms: i64) -> i64 {
    use rusqlite::types::ValueRef;
    const SECONDS_THRESHOLD: i64 = 946_684_800_000; // 2000-01-01 in ms
    match value {
        ValueRef::Integer(i) => {
            if i > 0 && i < SECONDS_THRESHOLD {
                i.saturating_mul(1000)
            } else {
                i
            }
        }
        ValueRef::Real(f) => {
            let i = f as i64;
            if i > 0 && i < SECONDS_THRESHOLD {
                i.saturating_mul(1000)
            } else {
                i
            }
        }
        ValueRef::Text(bytes) => {
            let s = match std::str::from_utf8(bytes) {
                Ok(s) => s.trim(),
                Err(_) => {
                    log::warn!("Invalid UTF-8 in legacy timestamp");
                    return fallback_ms;
                }
            };
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                dt.timestamp_millis()
            } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                dt.and_utc().timestamp_millis()
            } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
                dt.and_utc().timestamp_millis()
            } else if let Ok(i) = s.parse::<i64>() {
                if i > 0 && i < SECONDS_THRESHOLD {
                    i.saturating_mul(1000)
                } else {
                    i
                }
            } else {
                log::warn!("Could not parse legacy timestamp TEXT format");
                fallback_ms
            }
        }
        _ => fallback_ms,
    }
}

struct ImportedClip {
    legacy_id: String,
    content: String,
    #[allow(dead_code)]
    normalized_content: String,
    content_hash: String,
    content_type: String,
    domain: Option<String>,
    domain_key: String,
    page_title: Option<String>,
    created_at_ms: i64,
    last_copied_at_ms: i64,
    copy_count: i64,
    pinned: bool,
    sort_key: i64,
}

struct ImportedCategory {
    legacy_id: String,
    name: String,
    normalized_name: String,
    color: String,
    created_at_ms: i64,
    sort_order: i64,
}

struct ImportedCategoryLink {
    legacy_clip_id: String,
    legacy_category_id: String,
    created_at_ms: i64,
}

#[derive(Debug)]
struct LegacyMergeReport {
    legacy_clips_read: usize,
    canonical_clips_touched: usize,
    legacy_categories_read: usize,
    canonical_categories_touched: usize,
    links_read: usize,
    links_imported: usize,
    duplicate_clips_merged: usize,
    already_imported: bool,
}

fn merge_legacy_db_into_new(old_db_path: &Path, new_db_path: &Path) -> LegacyMigrationResult {
    // 1. Open and migrate new DB first via Repository::open
    if let Err(e) = crate::persistence::Repository::open(new_db_path) {
        log::error!("Failed to open/migrate target DB before merge: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    let fingerprint = match compute_db_fingerprint(old_db_path) {
        Ok(fp) => fp,
        Err(e) => {
            log::error!("Failed to compute fingerprint of old DB {:?}: {e}", old_db_path);
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    // 2. Open source legacy DB
    let src_conn = match Connection::open_with_flags(
        old_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to open legacy DB for merge: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    // 3. Open target DB connection for merge
    let mut dst_conn = match Connection::open(new_db_path) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to open new DB connection for merge: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    if let Err(e) = dst_conn.pragma_update(None, "foreign_keys", "ON") {
        log::error!("Failed to enable foreign keys on target DB: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }
    let _ = dst_conn.busy_timeout(std::time::Duration::from_secs(3));

    let report = match perform_legacy_merge(&src_conn, &mut dst_conn, &fingerprint, old_db_path) {
        Ok(rep) => rep,
        Err(e) => {
            log::error!("Legacy DB merge failed (transaction rolled back): {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    if let Err(e) = ensure_foreign_keys_valid(&dst_conn) {
        log::error!("Foreign key check failed after merge: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }
    if let Err(e) = ensure_integrity_ok(&dst_conn) {
        log::error!("Integrity check failed after merge: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    drop(dst_conn);
    drop(src_conn);

    if let Err(e) = crate::persistence::Repository::open(new_db_path) {
        log::error!("Failed to re-open target DB after merge: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    let old_backup = old_db_path.with_extension("sqlite3.migrated.bak");
    if let Err(e) = backup_database_file(old_db_path, &old_backup) {
        log::error!("Failed to backup old DB after merge: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    if !report.already_imported {
        log::info!(
            "Legacy merge completed successfully: clips read={}, canonical touched={}, categories read={}, canonical touched={}, links read={}, imported={}, duplicates merged={}",
            report.legacy_clips_read,
            report.canonical_clips_touched,
            report.legacy_categories_read,
            report.canonical_categories_touched,
            report.links_read,
            report.links_imported,
            report.duplicate_clips_merged
        );
    }

    LegacyMigrationResult::DatabasesMerged
}

fn perform_legacy_merge(
    src_conn: &Connection,
    dst_conn: &mut Connection,
    fingerprint: &str,
    source_path: &Path,
) -> anyhow::Result<LegacyMergeReport> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    ensure_legacy_imports_table(dst_conn)?;

    let already_imported: bool = dst_conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM legacy_imports WHERE source_fingerprint = ?1 AND status = 'completed')",
            params![fingerprint],
            |r| r.get(0),
        )
        .unwrap_or(false);

    if already_imported {
        log::info!(
            "Legacy DB fingerprint {} was already imported; skipping duplicate data merge",
            fingerprint
        );
        return Ok(LegacyMergeReport {
            legacy_clips_read: 0,
            canonical_clips_touched: 0,
            legacy_categories_read: 0,
            canonical_categories_touched: 0,
            links_read: 0,
            links_imported: 0,
            duplicate_clips_merged: 0,
            already_imported: true,
        });
    }

    let has_clips = has_column(src_conn, "clips", "id");
    if !has_clips {
        anyhow::bail!("Legacy DB has no clips table");
    }

    let has_domain_key = has_column(src_conn, "clips", "domain_key");

    let clips_sql = if has_domain_key {
        "SELECT id, content, content_type, domain, domain_key, page_title, created_at, last_copied_at, copy_count, pinned, sort_key FROM clips"
    } else {
        "SELECT id, content, content_type, domain, page_title, created_at, last_copied_at, copy_count, pinned, sort_key FROM clips"
    };

    let mut clips_stmt = src_conn.prepare(clips_sql)?;
    let legacy_clips_raw = clips_stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let content: String = row.get(1)?;
            let domain: Option<String> = row.get(3).ok();

            let (page_title, created_val, copied_val, count_val, pinned_val, sort_val) =
                if has_domain_key {
                    (
                        row.get::<_, Option<String>>(5)?,
                        row.get_ref(6)?,
                        row.get_ref(7)?,
                        row.get::<_, Option<i64>>(8)?.unwrap_or(1),
                        row.get::<_, Option<i64>>(9)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(10)?.unwrap_or(0),
                    )
                } else {
                    (
                        row.get::<_, Option<String>>(4)?,
                        row.get_ref(5)?,
                        row.get_ref(6)?,
                        row.get::<_, Option<i64>>(7)?.unwrap_or(1),
                        row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(9)?.unwrap_or(0),
                    )
                };

            let created_at_ms = parse_legacy_timestamp(created_val, now_ms);
            let last_copied_at_ms = parse_legacy_timestamp(copied_val, now_ms);

            Ok((
                id,
                content,
                domain,
                page_title,
                created_at_ms,
                last_copied_at_ms,
                count_val,
                pinned_val != 0,
                sort_val,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut imported_clips = Vec::with_capacity(legacy_clips_raw.len());
    for (id, raw_content, raw_domain, raw_title, created_ms, copied_ms, count, pinned, sort_key) in
        legacy_clips_raw
    {
        let norm_content = crate::domain::normalize_content(&raw_content);
        if norm_content.is_empty() {
            continue;
        }
        let hash = crate::domain::content_hash(&norm_content);
        let kind = crate::domain::classify(&norm_content).as_str().to_string();
        let norm_dom = raw_domain
            .as_deref()
            .and_then(crate::domain::normalize_domain);
        let dom_key = norm_dom.as_deref().unwrap_or("").to_string();
        let page_title = raw_title
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(500).collect());

        imported_clips.push(ImportedClip {
            legacy_id: id,
            content: raw_content,
            normalized_content: norm_content,
            content_hash: hash,
            content_type: kind,
            domain: norm_dom,
            domain_key: dom_key,
            page_title,
            created_at_ms: created_ms,
            last_copied_at_ms: copied_ms,
            copy_count: count.max(1),
            pinned,
            sort_key,
        });
    }

    let has_categories = has_column(src_conn, "user_categories", "id");
    let mut imported_categories = Vec::new();
    if has_categories {
        let has_sort_order = has_column(src_conn, "user_categories", "sort_order");

        let cat_sql = if has_sort_order {
            "SELECT id, name, color, created_at, sort_order FROM user_categories"
        } else {
            "SELECT id, name, color, created_at FROM user_categories"
        };

        let mut cat_stmt = src_conn.prepare(cat_sql)?;
        let cats_raw = cat_stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let color: String = row.get(2)?;
                let created_ref = row.get_ref(3)?;
                let created_ms = parse_legacy_timestamp(created_ref, now_ms);
                let sort_order: i64 = if has_sort_order { row.get(4)? } else { 0 };
                Ok((id, name, color, created_ms, sort_order))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        for (id, name, color, created_ms, sort_order) in cats_raw {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                continue;
            }
            let norm_name = trimmed.to_lowercase();
            let safe_color = if crate::persistence::is_valid_hex_color(&color) {
                color
            } else {
                log::warn!("Invalid legacy category color '{}'; using fallback #6b7280", color);
                "#6b7280".to_string()
            };
            imported_categories.push(ImportedCategory {
                legacy_id: id,
                name: trimmed.to_string(),
                normalized_name: norm_name,
                color: safe_color,
                created_at_ms: created_ms,
                sort_order,
            });
        }
    }

    let has_links = has_column(src_conn, "clip_user_categories", "clip_id");
    let mut imported_links = Vec::new();
    if has_links {
        let mut link_stmt = src_conn
            .prepare("SELECT clip_id, category_id, created_at FROM clip_user_categories")?;
        let links_raw = link_stmt
            .query_map([], |row| {
                let clip_id: String = row.get(0)?;
                let category_id: String = row.get(1)?;
                let created_ref = row.get_ref(2)?;
                let created_ms = parse_legacy_timestamp(created_ref, now_ms);
                Ok((clip_id, category_id, created_ms))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        for (clip_id, category_id, created_ms) in links_raw {
            imported_links.push(ImportedCategoryLink {
                legacy_clip_id: clip_id,
                legacy_category_id: category_id,
                created_at_ms: created_ms,
            });
        }
    }

    let tx = dst_conn.transaction()?;

    let legacy_clips_read = imported_clips.len();
    let legacy_categories_read = imported_categories.len();
    let links_read = imported_links.len();

    let mut category_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut canonical_categories_touched = 0;

    for cat in &imported_categories {
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM user_categories WHERE normalized_name=?1",
                params![cat.normalized_name],
                |r| r.get(0),
            )
            .optional()?;

        let canonical_id = match existing {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO user_categories(id, name, normalized_name, color, created_at, sort_order) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, cat.name, cat.normalized_name, cat.color, cat.created_at_ms, cat.sort_order],
                )?;
                canonical_categories_touched += 1;
                id
            }
        };
        category_map.insert(cat.legacy_id.clone(), canonical_id);
    }

    let mut clip_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut canonical_clips_touched = 0;
    let mut duplicate_clips_merged = 0;

    for clip in &imported_clips {
        #[allow(clippy::type_complexity)]
        let existing: Option<(String, i64, bool, i64, i64, Option<String>, i64)> = tx
            .query_row(
                "SELECT id, copy_count, pinned, last_copied_at, created_at, page_title, sort_key FROM clips WHERE content_hash=?1 AND domain_key=?2",
                params![clip.content_hash, clip.domain_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?;

        if let Some((
            existing_id,
            existing_count,
            existing_pinned,
            existing_last,
            existing_created,
            existing_title,
            existing_sort,
        )) = existing
        {
            let merged_count = existing_count.saturating_add(clip.copy_count);
            let merged_pinned = existing_pinned || clip.pinned;
            let merged_last = existing_last.max(clip.last_copied_at_ms);
            let merged_created = existing_created.min(clip.created_at_ms);
            let merged_sort = existing_sort.max(clip.sort_key);

            let merged_title =
                if clip.last_copied_at_ms > existing_last && clip.page_title.is_some() {
                    clip.page_title.clone()
                } else {
                    existing_title.or_else(|| clip.page_title.clone())
                };

            tx.execute(
                "UPDATE clips SET copy_count=?2, pinned=?3, last_copied_at=?4, created_at=?5, sort_key=?6, page_title=?7 WHERE id=?1",
                params![existing_id, merged_count, merged_pinned, merged_last, merged_created, merged_sort, merged_title],
            )?;

            clip_map.insert(clip.legacy_id.clone(), existing_id);
            duplicate_clips_merged += 1;
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO clips(id, content, content_hash, content_type, domain, domain_key, page_title, created_at, last_copied_at, copy_count, pinned, sort_key)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    id,
                    clip.content,
                    clip.content_hash,
                    clip.content_type,
                    clip.domain,
                    clip.domain_key,
                    clip.page_title,
                    clip.created_at_ms,
                    clip.last_copied_at_ms,
                    clip.copy_count,
                    clip.pinned,
                    clip.sort_key,
                ],
            )?;
            clip_map.insert(clip.legacy_id.clone(), id);
            canonical_clips_touched += 1;
        }
    }

    let mut links_imported = 0;
    for link in &imported_links {
        if let (Some(canonical_clip_id), Some(canonical_cat_id)) = (
            clip_map.get(&link.legacy_clip_id),
            category_map.get(&link.legacy_category_id),
        ) {
            tx.execute(
                "INSERT OR IGNORE INTO clip_user_categories(clip_id, category_id, created_at) VALUES(?1, ?2, ?3)",
                params![canonical_clip_id, canonical_cat_id, link.created_at_ms],
            )?;
            links_imported += 1;
        }
    }

    let import_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO legacy_imports(id, source_fingerprint, imported_at, source_path, status) VALUES(?1, ?2, ?3, ?4, 'completed')",
        params![import_id, fingerprint, now_ms, source_path.to_string_lossy()],
    )?;

    {
        let mut fk_stmt = tx.prepare("PRAGMA foreign_key_check;")?;
        if fk_stmt.exists([])? {
            anyhow::bail!("Foreign key check failed inside transaction during legacy merge");
        }
    }

    tx.commit()?;

    Ok(LegacyMergeReport {
        legacy_clips_read,
        canonical_clips_touched,
        legacy_categories_read,
        canonical_categories_touched,
        links_read,
        links_imported,
        duplicate_clips_merged,
        already_imported: false,
    })
}

fn migrate_autostart_at(config_home: &Path) {
    let autostart_dir = config_home.join("autostart");
    let old_entry = autostart_dir.join("pastily.desktop");
    let new_entry = autostart_dir.join("kitsupin.desktop");

    if old_entry.exists() {
        if !new_entry.exists() {
            if let Ok(content) = std::fs::read_to_string(&old_entry) {
                let updated = content
                    .replace("Pastily", "KitsuPin")
                    .replace("pastily", "kitsupin");
                let _ = std::fs::write(&new_entry, updated);
            } else {
                let _ = std::fs::rename(&old_entry, &new_entry);
            }
        }
        let _ = std::fs::remove_file(&old_entry);
    }
}

fn migrate_native_host_manifests_at(config_home: &Path) {
    let browser_dirs = [
        config_home.join("google-chrome/NativeMessagingHosts"),
        config_home.join("chromium/NativeMessagingHosts"),
    ];

    for manifest_dir in browser_dirs {
        let old_manifest = manifest_dir.join("app.pastily.native.json");
        let new_manifest = manifest_dir.join("io.github.mootxed.kitsupin.native.json");

        if old_manifest.exists() {
            if !new_manifest.exists() {
                if let Ok(content) = std::fs::read_to_string(&old_manifest) {
                    let updated = content
                        .replace("app.pastily.native", "io.github.mootxed.kitsupin.native")
                        .replace("Pastily", "KitsuPin")
                        .replace("pastily", "kitsupin");
                    let _ = std::fs::write(&new_manifest, updated);
                }
            }
            let _ = std::fs::remove_file(&old_manifest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_real_pastily_v1_db(path: &Path, clips: &[(&str, &str, &str, &str, &str, i64, i64)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE clips (
                id TEXT PRIMARY KEY,
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
                id TEXT PRIMARY KEY,
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
                PRIMARY KEY(clip_id, category_id)
            );",
        )
        .unwrap();

        for (id, content, domain, created_at, last_copied, copy_count, pinned) in clips {
            let norm = crate::domain::normalize_content(content);
            let hash = crate::domain::content_hash(&norm);
            conn.execute(
                "INSERT INTO clips(id, content, normalized_content, content_hash, content_type, domain, page_title, created_at, last_copied_at, copy_count, pinned, sort_key)
                 VALUES(?1, ?2, ?3, ?4, 'Text', ?5, 'Old Title', ?6, ?7, ?8, ?9, 0)",
                params![id, content, norm, hash, if domain.is_empty() { None } else { Some(*domain) }, created_at, last_copied, copy_count, pinned],
            )
            .unwrap();
        }
    }

    fn create_current_kitsupin_db(path: &Path, clips: &[(&str, &str, bool, i64)]) {
        let repo = crate::persistence::Repository::open(path).unwrap();
        for (id, content, pinned, copy_count) in clips {
            let now = 1_700_000_000_000;
            let (summary, _) = repo
                .upsert_clip(crate::domain::NewClip {
                    content,
                    domain: None,
                    page_title: Some("New Title"),
                    now,
                })
                .unwrap();

            if *pinned {
                repo.set_pinned(&summary.id, true).unwrap();
            }
            if *copy_count > 1 {
                for _ in 1..*copy_count {
                    let _ = repo.mark_clip_copied(&summary.id, now);
                }
            }
            let _ = id;
        }
    }

    #[test]
    fn test_scenario_a_directory_moved() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let old_db = old_dir.join("pastily.sqlite3");
        create_real_pastily_v1_db(
            &old_db,
            &[(
                "c1",
                "hello pastily",
                "",
                "2023-01-01T00:00:00Z",
                "2023-01-01T00:00:00Z",
                1,
                0,
            )],
        );

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::DirectoryMoved);

        let new_db = data_home.join("kitsupin/kitsupin.sqlite3");
        assert!(new_db.exists());
        assert!(!old_dir.exists());
    }

    #[test]
    fn test_scenario_b_kitsupin_dir_exists_no_db() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");
        std::fs::create_dir_all(&new_dir).unwrap();

        let old_db = old_dir.join("pastily.sqlite3");
        create_real_pastily_v1_db(
            &old_db,
            &[(
                "c1",
                "test item",
                "",
                "2023-01-01T00:00:00Z",
                "2023-01-01T00:00:00Z",
                2,
                1,
            )],
        );

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::LegacyDatabaseRestored);

        let new_db = new_dir.join("kitsupin.sqlite3");
        assert!(new_db.exists());
        assert_eq!(count_clips_safely(&new_db), 1);
    }

    #[test]
    fn test_scenario_c_kitsupin_db_exists_empty() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");

        let old_db = old_dir.join("pastily.sqlite3");
        create_real_pastily_v1_db(
            &old_db,
            &[(
                "c1",
                "old item",
                "",
                "2023-01-01T00:00:00Z",
                "2023-01-01T00:00:00Z",
                3,
                1,
            )],
        );

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_current_kitsupin_db(&new_db, &[]); // Empty DB

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::LegacyDatabaseRestored);

        assert_eq!(count_clips_safely(&new_db), 1);
    }

    #[test]
    fn test_scenario_d_real_pastily_v1_merge_with_current_kitsupin() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");

        let old_db = old_dir.join("pastily.sqlite3");
        create_real_pastily_v1_db(
            &old_db,
            &[
                (
                    "c1",
                    "shared content",
                    "",
                    "2023-01-01T10:00:00Z",
                    "2023-01-01T10:00:00Z",
                    2,
                    1,
                ),
                (
                    "c2",
                    "old only",
                    "",
                    "2023-01-02T10:00:00Z",
                    "2023-01-02T10:00:00Z",
                    1,
                    0,
                ),
            ],
        );

        // Add category and category link to legacy db
        {
            let conn = Connection::open(&old_db).unwrap();
            conn.execute(
                "INSERT INTO user_categories(id, name, normalized_name, color, created_at, sort_order)
                 VALUES('cat_old', 'Work', 'work', '#ff0000', '2023-01-01T00:00:00Z', 1)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO clip_user_categories(clip_id, category_id, created_at)
                 VALUES('c1', 'cat_old', '2023-01-01T10:00:00Z')",
                [],
            )
            .unwrap();
        }

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_current_kitsupin_db(
            &new_db,
            &[
                ("c1_new", "shared content", false, 5),
                ("c3", "new only", true, 1),
            ],
        );

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::DatabasesMerged);

        assert_eq!(count_clips_safely(&new_db), 3);

        let repo = crate::persistence::Repository::open(&new_db).unwrap();
        let clips = repo
            .list_clips(&crate::domain::ClipQuery::default())
            .unwrap();
        let shared = clips
            .iter()
            .find(|c| c.preview.contains("shared content"))
            .unwrap();
        assert!(shared.pinned);
        assert!(shared.copy_count >= 7);

        // Verify category Work was imported and linked to shared clip
        let categories = repo.list_categories().unwrap();
        let work_cat = categories
            .iter()
            .find(|c| c.name == "Work")
            .expect("category Work imported");
        assert_eq!(work_cat.color, "#ff0000");

        let conn = Connection::open(&new_db).unwrap();
        let category_link_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clip_user_categories WHERE clip_id = ?1 AND category_id = ?2",
                params![shared.id, work_cat.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(category_link_count, 1);

        // Verify foreign key check passes on merged database
        let mut fk_stmt = conn.prepare("PRAGMA foreign_key_check;").unwrap();
        assert!(!fk_stmt.exists([]).unwrap());
    }

    #[test]
    fn test_corrupted_legacy_row_rolls_back_transaction() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");

        let old_db = old_dir.join("pastily.sqlite3");
        std::fs::create_dir_all(&old_dir).unwrap();

        // Create corrupt old_db missing required clips table
        {
            let conn = Connection::open(&old_db).unwrap();
            conn.execute_batch("CREATE TABLE invalid_table (id INT);").unwrap();
        }

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_current_kitsupin_db(&new_db, &[("c1_valid", "valid initial clip", false, 1)]);

        let new_count_before = count_clips_safely(&new_db);

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::ConflictPreserved);

        // Target database must remain untouched
        assert_eq!(count_clips_safely(&new_db), new_count_before);
        // Source database file must remain intact
        assert!(old_db.exists());
    }

    #[test]
    fn test_corrupted_category_color_fallback() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");

        let old_db = old_dir.join("pastily.sqlite3");
        create_real_pastily_v1_db(
            &old_db,
            &[(
                "c1",
                "color test item",
                "",
                "2023-01-01T00:00:00Z",
                "2023-01-01T00:00:00Z",
                1,
                0,
            )],
        );

        {
            let conn = Connection::open(&old_db).unwrap();
            conn.execute(
                "INSERT INTO user_categories(id, name, normalized_name, color, created_at, sort_order)
                 VALUES('cat_bad', 'Hacked', 'hacked', 'red\" style=\"bad', '2023-01-01T00:00:00Z', 1)",
                [],
            )
            .unwrap();
        }

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_current_kitsupin_db(&new_db, &[("c1_valid", "valid clip", false, 1)]);

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::DatabasesMerged);

        let repo = crate::persistence::Repository::open(&new_db).unwrap();
        let categories = repo.list_categories().unwrap();
        let hacked = categories
            .iter()
            .find(|c| c.name == "Hacked")
            .expect("category Hacked imported");
        assert_eq!(hacked.color, "#6b7280");
    }

    #[test]
    fn test_idempotent_merge_with_ledger() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");

        let old_db = old_dir.join("pastily.sqlite3");
        create_real_pastily_v1_db(
            &old_db,
            &[(
                "c1",
                "shared content",
                "",
                "2023-01-01T10:00:00Z",
                "2023-01-01T10:00:00Z",
                3,
                0,
            )],
        );

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_current_kitsupin_db(&new_db, &[("c1_new", "shared content", false, 5)]);

        let res1 = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res1, LegacyMigrationResult::DatabasesMerged);

        let repo = crate::persistence::Repository::open(&new_db).unwrap();
        let clips1 = repo.list_clips(&crate::domain::ClipQuery::default()).unwrap();
        let shared1 = clips1.iter().find(|c| c.preview.contains("shared content")).unwrap();
        let copy_count_after_first_merge = shared1.copy_count;
        assert_eq!(copy_count_after_first_merge, 8); // 5 + 3 = 8

        // Re-create old_db with same content to simulate repeated launch before backup cleanup or duplicate run
        create_real_pastily_v1_db(
            &old_db,
            &[(
                "c1",
                "shared content",
                "",
                "2023-01-01T10:00:00Z",
                "2023-01-01T10:00:00Z",
                3,
                0,
            )],
        );

        let res2 = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res2, LegacyMigrationResult::DatabasesMerged);

        let clips2 = repo.list_clips(&crate::domain::ClipQuery::default()).unwrap();
        let shared2 = clips2.iter().find(|c| c.preview.contains("shared content")).unwrap();
        assert_eq!(shared2.copy_count, 8); // Must stay 8, NOT increment to 11
    }

    #[test]
    fn test_lock_failure_aborts_migration() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let lock_path = data_home.join(".kitsupin-migration.lock");
        std::fs::create_dir_all(data_home).unwrap();

        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        lock_file.lock_exclusive().unwrap();

        let old_dir = data_home.join("pastily");
        create_real_pastily_v1_db(
            &old_dir.join("pastily.sqlite3"),
            &[(
                "c1",
                "test",
                "",
                "2023-01-01T00:00:00Z",
                "2023-01-01T00:00:00Z",
                1,
                0,
            )],
        );

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::ConflictPreserved);

        // Ensure old directory was not moved
        assert!(old_dir.exists());
    }

    #[test]
    fn test_idempotent_repeated_runs() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();

        let res1 = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res1, LegacyMigrationResult::NothingToMigrate);

        let res2 = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res2, LegacyMigrationResult::NothingToMigrate);
    }
}
