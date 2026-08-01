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
    migrate_data_dir();
    migrate_autostart();
    migrate_native_host_manifests();
}

fn migrate_data_dir() {
    let data_home = match get_xdg_data_home() {
        Some(path) => path,
        None => return,
    };

    let old_dir = data_home.join("pastily");
    let new_dir = data_home.join("kitsupin");

    if old_dir.exists() && !new_dir.exists() {
        if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
            log::warn!("Failed to rename pastily data dir to kitsupin: {e}");
            return;
        }
    }

    if new_dir.exists() {
        // Rename database files if old names exist
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

        // Clean up old socket if it exists
        let old_sock = new_dir.join("native.sock");
        if old_sock.exists() {
            let _ = std::fs::remove_file(old_sock);
        }
    }
}

fn migrate_autostart() {
    let config_home = match get_xdg_config_home() {
        Some(path) => path,
        None => return,
    };

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

fn migrate_native_host_manifests() {
    let config_home = match get_xdg_config_home() {
        Some(path) => path,
        None => return,
    };

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

    #[test]
    fn test_migration_paths() {
        // Simple test to ensure functions run without panic in missing dirs
        migrate_pastily_to_kitsupin();
    }
}
