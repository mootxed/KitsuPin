use fs2::FileExt;
use std::path::PathBuf;

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


pub fn migrate_pastily_to_kitsupin() {
    let data_home = match get_xdg_data_home() {
        Some(path) => path,
        None => return,
    };

    let lock_path = data_home.join(".kitsupin-migration.lock");
    let _migration_lock = match std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(file) => {
            if file.lock_exclusive().is_err() {
                return;
            }
            Some(file)
        }
        Err(_) => None,
    };

    migrate_data_dir_at(&data_home);

    if let Some(config_home) = get_xdg_config_home() {
        migrate_autostart_at(&config_home);
        migrate_native_host_manifests_at(&config_home);
    }
}

fn migrate_data_dir_at(data_home: &PathBuf) {
    let old_dir = data_home.join("pastily");
    let new_dir = data_home.join("kitsupin");

    if old_dir.exists() && !new_dir.exists() {
        if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
            log::warn!("Failed to rename pastily data dir to kitsupin: {e}");
        }
    } else if old_dir.exists() && new_dir.exists() {
        // If new_dir was created but doesn't have database, move from old_dir
        let new_db = new_dir.join("kitsupin.sqlite3");
        let old_db_pastily = old_dir.join("pastily.sqlite3");
        let old_db_kitsupin = old_dir.join("kitsupin.sqlite3");

        if !new_db.exists() {
            if old_db_kitsupin.exists() {
                let _ = std::fs::rename(&old_db_kitsupin, &new_db);
            } else if old_db_pastily.exists() {
                let _ = std::fs::rename(&old_db_pastily, &new_db);
            }
        }
    }

    if new_dir.exists() {
        // Rename database files if old names exist within new_dir
        let old_db = new_dir.join("pastily.sqlite3");
        let new_db = new_dir.join("kitsupin.sqlite3");
        if old_db.exists() && !new_db.exists() {
            let _ = std::fs::rename(&old_db, &new_db);
        }

        let old_wal = new_dir.join("pastily.sqlite3-wal");
        let new_wal = new_dir.join("kitsupin.sqlite3-wal");
        if old_wal.exists() && !new_wal.exists() {
            let _ = std::fs::rename(&old_wal, &new_wal);
        }

        let old_shm = new_dir.join("pastily.sqlite3-shm");
        let new_shm = new_dir.join("kitsupin.sqlite3-shm");
        if old_shm.exists() && !new_shm.exists() {
            let _ = std::fs::rename(&old_shm, &new_shm);
        }
    }
}

fn migrate_autostart_at(config_home: &PathBuf) {
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

fn migrate_native_host_manifests_at(config_home: &PathBuf) {
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

    #[test]
    fn test_migration_paths() {
        // Simple test to ensure functions run without panic in missing dirs
        migrate_pastily_to_kitsupin();
    }

    #[test]
    fn test_pastily_directory_migration() {
        let temp_dir = TempDir::new().unwrap();
        let data_home = temp_dir.path().to_path_buf();
        let old_dir = data_home.join("pastily");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("pastily.sqlite3"), b"test db data").unwrap();

        migrate_data_dir_at(&data_home);

        let new_dir = data_home.join("kitsupin");
        assert!(new_dir.exists(), "kitsupin directory should be created via rename");
        assert!(!old_dir.exists(), "pastily directory should be moved");
        assert!(new_dir.join("kitsupin.sqlite3").exists(), "pastily.sqlite3 should be renamed to kitsupin.sqlite3");
    }
}

