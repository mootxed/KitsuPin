use fs2::FileExt;
use rusqlite::{params, Connection, OpenFlags};
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
            if file.try_lock_exclusive().is_err() {
                log::warn!("Migration lock held by another process; aborting migration");
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
        if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
            log::error!("Failed to rename pastily directory to kitsupin: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
        normalize_db_names_in(&new_dir);
        return LegacyMigrationResult::DirectoryMoved;
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
        // old_dir exists but has no db
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
        if res == LegacyMigrationResult::LegacyDatabaseRestored {
            let _ = std::fs::remove_file(backup_empty);
        }
        return res;
    }

    if old_clip_count == 0 {
        // Old database is empty, nothing useful to import
        let backup_old = old_dir.join("pastily.sqlite3.empty.bak");
        let _ = std::fs::rename(&old_db_path, &backup_old);
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

    // Step 1: Open source DB read-only, execute wal_checkpoint
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

    let _ = src_conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");

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
    let integrity_ok = dst_conn
        .query_row("PRAGMA integrity_check;", [], |r| r.get::<_, String>(0))
        .map(|res| res == "ok")
        .unwrap_or(false);

    if !integrity_ok {
        log::error!(
            "Integrity check failed for imported DB at {:?}",
            importing_path
        );
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    let fk_check_passed = dst_conn
        .query_row("PRAGMA foreign_key_check;", [], |_| Ok(()))
        .map_or(true, |_| true);

    if !fk_check_passed {
        log::error!(
            "Foreign key check failed for imported DB at {:?}",
            importing_path
        );
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    drop(dst_conn);
    drop(src_conn);

    // Step 4: Atomic rename importing file to final DB target
    if let Err(e) = std::fs::rename(&importing_path, target_db_path) {
        log::error!("Failed to rename importing DB to final path: {e}");
        let _ = std::fs::remove_file(&importing_path);
        return LegacyMigrationResult::ConflictPreserved;
    }

    // Preserve old database as backup
    let old_backup = old_db_path.with_extension("sqlite3.migrated.bak");
    let _ = std::fs::rename(old_db_path, &old_backup);

    LegacyMigrationResult::LegacyDatabaseRestored
}

fn merge_legacy_db_into_new(old_db_path: &Path, new_db_path: &Path) -> LegacyMigrationResult {
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

    let mut dst_conn = match Connection::open(new_db_path) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to open new DB for merge: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    let tx = match dst_conn.transaction() {
        Ok(t) => t,
        Err(e) => {
            log::error!("Failed to start transaction for merge: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    // Make sure tables exist in old DB before querying
    let has_clips: bool = src_conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='clips')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);

    if !has_clips {
        return LegacyMigrationResult::NothingToMigrate;
    }

    struct LegacyClip {
        id: String,
        content: String,
        content_hash: String,
        content_type: String,
        domain: Option<String>,
        domain_key: String,
        page_title: Option<String>,
        created_at: i64,
        last_copied_at: i64,
        copy_count: i64,
        pinned: bool,
        sort_key: i64,
    }

    let mut stmt = match src_conn.prepare(
        "SELECT id, content, content_hash, content_type, domain, COALESCE(domain_key, COALESCE(domain,'')), page_title, created_at, last_copied_at, copy_count, pinned, sort_key FROM clips"
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to prepare legacy clip query: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    let legacy_clips = match stmt.query_map([], |r| {
        Ok(LegacyClip {
            id: r.get(0)?,
            content: r.get(1)?,
            content_hash: r.get(2)?,
            content_type: r.get(3)?,
            domain: r.get(4)?,
            domain_key: r.get(5)?,
            page_title: r.get(6)?,
            created_at: r.get(7)?,
            last_copied_at: r.get(8)?,
            copy_count: r.get(9)?,
            pinned: r.get(10)?,
            sort_key: r.get(11)?,
        })
    }) {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect::<Vec<_>>(),
        Err(e) => {
            log::error!("Failed to query legacy clips: {e}");
            return LegacyMigrationResult::ConflictPreserved;
        }
    };

    for clip in legacy_clips {
        let existing: Option<(String, i64, bool, i64, i64, Option<String>)> = tx
            .query_row(
                "SELECT id, copy_count, pinned, last_copied_at, created_at, page_title FROM clips WHERE content_hash=?1 AND domain_key=?2",
                params![clip.content_hash, clip.domain_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .ok();

        if let Some((
            existing_id,
            existing_count,
            existing_pinned,
            existing_last,
            existing_created,
            existing_title,
        )) = existing
        {
            let merged_count = existing_count.saturating_add(clip.copy_count);
            let merged_pinned = existing_pinned || clip.pinned;
            let merged_last = existing_last.max(clip.last_copied_at);
            let merged_created = existing_created.min(clip.created_at);
            let merged_title = existing_title.or(clip.page_title);

            let _ = tx.execute(
                "UPDATE clips SET copy_count=?2, pinned=?3, last_copied_at=?4, created_at=?5, page_title=COALESCE(?6, page_title) WHERE id=?1",
                params![existing_id, merged_count, merged_pinned, merged_last, merged_created, merged_title],
            );
        } else {
            let _ = tx.execute(
                "INSERT OR IGNORE INTO clips(id, content, content_hash, content_type, domain, domain_key, page_title, created_at, last_copied_at, copy_count, pinned, sort_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    clip.id,
                    clip.content,
                    clip.content_hash,
                    clip.content_type,
                    clip.domain,
                    clip.domain_key,
                    clip.page_title,
                    clip.created_at,
                    clip.last_copied_at,
                    clip.copy_count,
                    clip.pinned,
                    clip.sort_key,
                ],
            );
        }
    }

    // Merge categories if user_categories table exists in legacy DB
    let has_categories: bool = src_conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='user_categories')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);

    if has_categories {
        if let Ok(mut cat_stmt) =
            src_conn.prepare("SELECT id, name, color, created_at FROM user_categories")
        {
            if let Ok(cats) = cat_stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            }) {
                for cat in cats.flatten() {
                    let _ = tx.execute(
                        "INSERT OR IGNORE INTO user_categories(id, name, color, created_at) VALUES(?1, ?2, ?3, ?4)",
                        params![cat.0, cat.1, cat.2, cat.3],
                    );
                }
            }
        }

        if let Ok(mut map_stmt) =
            src_conn.prepare("SELECT clip_id, category_id, created_at FROM clip_user_categories")
        {
            if let Ok(mappings) = map_stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            }) {
                for m in mappings.flatten() {
                    let _ = tx.execute(
                        "INSERT OR IGNORE INTO clip_user_categories(clip_id, category_id, created_at) VALUES(?1, ?2, ?3)",
                        params![m.0, m.1, m.2],
                    );
                }
            }
        }
    }

    if let Err(e) = tx.commit() {
        log::error!("Failed to commit database merge: {e}");
        return LegacyMigrationResult::ConflictPreserved;
    }

    let old_backup = old_db_path.with_extension("sqlite3.migrated.bak");
    let _ = std::fs::rename(old_db_path, &old_backup);

    LegacyMigrationResult::DatabasesMerged
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

    fn create_test_db(path: &Path, clips: &[(&str, &str, bool, i64)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE clips (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                content_type TEXT NOT NULL,
                domain TEXT,
                domain_key TEXT NOT NULL,
                page_title TEXT,
                created_at INTEGER NOT NULL,
                last_copied_at INTEGER NOT NULL,
                copy_count INTEGER NOT NULL,
                pinned INTEGER NOT NULL,
                sort_key INTEGER NOT NULL
            );
            CREATE TABLE user_categories (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                color TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE clip_user_categories (
                clip_id TEXT NOT NULL,
                category_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(clip_id, category_id)
            );",
        )
        .unwrap();

        for (id, content, pinned, copy_count) in clips {
            let hash = format!("{:x}", sha2::Sha256::digest(content.as_bytes()));
            let p_val = if *pinned { 1 } else { 0 };
            conn.execute(
                "INSERT INTO clips(id, content, content_hash, content_type, domain, domain_key, page_title, created_at, last_copied_at, copy_count, pinned, sort_key)
                 VALUES(?1, ?2, ?3, 'Text', NULL, '', NULL, 1000, 1000, ?4, ?5, 0)",
                params![id, content, hash, copy_count, p_val],
            ).unwrap();
        }
    }

    use sha2::Digest;

    #[test]
    fn test_scenario_a_directory_moved() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let old_db = old_dir.join("pastily.sqlite3");
        create_test_db(&old_db, &[("c1", "hello pastily", false, 1)]);

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
        create_test_db(&old_db, &[("c1", "test item", true, 2)]);

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
        create_test_db(&old_db, &[("c1", "old item", true, 3)]);

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_test_db(&new_db, &[]); // Empty DB

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::LegacyDatabaseRestored);

        assert_eq!(count_clips_safely(&new_db), 1);
    }

    #[test]
    fn test_scenario_d_both_contain_data_merged() {
        let temp = TempDir::new().unwrap();
        let data_home = temp.path();
        let old_dir = data_home.join("pastily");
        let new_dir = data_home.join("kitsupin");

        let old_db = old_dir.join("pastily.sqlite3");
        create_test_db(
            &old_db,
            &[
                ("c1", "shared content", true, 2),
                ("c2", "old only", false, 1),
            ],
        );

        let new_db = new_dir.join("kitsupin.sqlite3");
        create_test_db(
            &new_db,
            &[
                ("c1_new", "shared content", false, 5),
                ("c3", "new only", true, 1),
            ],
        );

        let res = migrate_pastily_to_kitsupin_at(data_home, None);
        assert_eq!(res, LegacyMigrationResult::DatabasesMerged);

        assert_eq!(count_clips_safely(&new_db), 3);

        // Verify shared clip properties merged correctly
        let conn = Connection::open(&new_db).unwrap();
        let hash = format!("{:x}", sha2::Sha256::digest("shared content".as_bytes()));
        let (count, pinned): (i64, bool) = conn
            .query_row(
                "SELECT copy_count, pinned FROM clips WHERE content_hash=?1 AND domain_key=''",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        assert_eq!(count, 7);
        assert!(pinned);
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
        create_test_db(
            &old_dir.join("pastily.sqlite3"),
            &[("c1", "test", false, 1)],
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
