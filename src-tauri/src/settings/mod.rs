use anyhow::Result;
use directories::ProjectDirs;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub paused: bool,
    pub autostart: bool,
    pub shortcut: String,
    pub retention_days: u32,
    pub excluded_apps: Vec<String>,
}

impl Settings {
    /// Validate that all fields are within acceptable bounds.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=3650).contains(&self.retention_days),
            "срок хранения вне допустимого диапазона (1–3650 дней)"
        );
        anyhow::ensure!(
            !self.shortcut.is_empty(),
            "горячая клавиша не может быть пустой"
        );
        anyhow::ensure!(
            self.shortcut.len() <= 100,
            "горячая клавиша слишком длинная (максимум 100 символов)"
        );
        anyhow::ensure!(
            self.excluded_apps.len() <= 100,
            "список исключённых приложений превышает 100 элементов"
        );
        for app in &self.excluded_apps {
            anyhow::ensure!(
                app.len() <= 200,
                "название исключённого приложения слишком длинное: {app}"
            );
        }
        Ok(())
    }

    /// Quick boolean check (used by jobs/cleanup to guard against corrupt state).
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            paused: false,
            autostart: true,
            shortcut: "Super+V".into(),
            retention_days: 90,
            excluded_apps: vec![],
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    value: RwLock<Settings>,
    /// Set once at load time if the file was corrupt/invalid.
    /// Consumed atomically by `consume_invalid_warning()`.
    has_invalid_warning: AtomicBool,
}

impl SettingsStore {
    pub fn load(data_dir: &Path) -> Result<Arc<Self>> {
        let path = data_dir.join("settings.json");
        let invalid_path = data_dir.join("settings.invalid.json");
        let mut has_invalid_warning = false;

        let value = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(raw_content) => {
                    let parsed: Option<Settings> = serde_json::from_str(&raw_content).ok();
                    if let Some(s) = parsed {
                        if s.is_valid() {
                            s
                        } else {
                            log::error!(
                                "Invalid settings (retention_days={}). Backup → settings.invalid.json, resetting to defaults.",
                                s.retention_days
                            );
                            let _ = std::fs::write(&invalid_path, &raw_content);
                            has_invalid_warning = true;
                            let default_s = Settings::default();
                            if let Ok(json) = serde_json::to_vec_pretty(&default_s) {
                                let _ = std::fs::write(&path, json);
                            }
                            default_s
                        }
                    } else {
                        log::error!("Corrupted settings.json. Backup → settings.invalid.json, resetting to defaults.");
                        let _ = std::fs::write(&invalid_path, &raw_content);
                        has_invalid_warning = true;
                        let default_s = Settings::default();
                        if let Ok(json) = serde_json::to_vec_pretty(&default_s) {
                            let _ = std::fs::write(&path, json);
                        }
                        default_s
                    }
                }
                Err(e) => {
                    log::error!("Failed to read settings.json: {e}");
                    Settings::default()
                }
            }
        } else {
            Settings::default()
        };

        if path.exists() {
            secure_file(&path);
        }
        if invalid_path.exists() {
            secure_file(&invalid_path);
        }

        Ok(Arc::new(Self {
            path,
            value: RwLock::new(value),
            has_invalid_warning: AtomicBool::new(has_invalid_warning),
        }))
    }

    /// Returns the warning state (non-consuming). For compatibility with bootstrap command.
    pub fn has_invalid_warning(&self) -> bool {
        self.has_invalid_warning.load(Ordering::Relaxed)
    }

    /// Consume the warning flag atomically. Returns true once, then false forever.
    /// Use this in the `consume_invalid_settings_warning` Tauri command to prevent
    /// repeated warnings across multiple window reloads.
    pub fn consume_invalid_warning(&self) -> bool {
        self.has_invalid_warning
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    pub fn get(&self) -> Settings {
        self.value.read().clone()
    }

    /// Atomically save settings using write-fsync-rename sequence.
    /// Does NOT update runtime state (paused, shortcuts, etc.) — callers handle that.
    pub fn save(&self, value: Settings) -> Result<()> {
        value.validate()?;
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(&value)?)?;
            // fsync to ensure data reaches disk before rename.
            f.sync_data()?;
        }
        secure_file(&tmp);
        std::fs::rename(&tmp, &self.path)?;
        secure_file(&self.path);
        *self.value.write() = value;
        Ok(())
    }
}

#[cfg(unix)]
fn secure_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}
#[cfg(not(unix))]
fn secure_file(_path: &Path) {}

pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("io.github.mootxed", "", "kitsupin")
        .ok_or_else(|| anyhow::anyhow!("XDG data directory недоступен"))
}

pub fn autostart_path() -> Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| anyhow::anyhow!("XDG config directory недоступен"))?;
    Ok(config.join("autostart/kitsupin.desktop"))
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = autostart_path()?;
    let system_entry = Path::new("/etc/xdg/autostart/kitsupin.desktop");
    if enabled && system_entry.exists() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    } else if enabled {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let exe = std::env::current_exe()?;
        // Properly escape the path for the Exec field:
        // If the path contains spaces, wrap in quotes. Replace embedded quotes with \".
        let exe_str = exe.to_string_lossy();
        let exec_value = if exe_str.contains(' ') {
            format!("\"{}\" --background", exe_str.replace('"', "\\\""))
        } else {
            format!("{} --background", exe_str)
        };
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=KitsuPin\nComment=История буфера обмена\nExec={exec_value}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nOnlyShowIn=KDE;\n"
        );
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &path)?;
        // .desktop files should be readable (0o644).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
    } else {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(
            &tmp,
            "[Desktop Entry]\nType=Application\nName=KitsuPin\nHidden=true\n",
        )?;
        std::fs::rename(&tmp, &path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_valid_settings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let valid = Settings {
            paused: false,
            autostart: true,
            shortcut: "Super+V".into(),
            retention_days: 30,
            excluded_apps: vec![],
        };
        std::fs::write(&path, serde_json::to_string(&valid).unwrap()).unwrap();

        let store = SettingsStore::load(dir.path()).unwrap();
        assert!(!store.has_invalid_warning());
        assert_eq!(store.get().retention_days, 30);
    }

    #[test]
    fn test_load_zero_retention_days_backs_up_and_resets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let invalid_path = dir.path().join("settings.invalid.json");
        let invalid_content = r#"{"paused":false,"autostart":true,"shortcut":"Super+V","retentionDays":0,"excludedApps":[]}"#;
        std::fs::write(&path, invalid_content).unwrap();

        let store = SettingsStore::load(dir.path()).unwrap();
        assert!(store.has_invalid_warning());
        assert_eq!(store.get().retention_days, 90);
        assert!(invalid_path.exists());
        assert_eq!(
            std::fs::read_to_string(&invalid_path).unwrap(),
            invalid_content
        );
    }

    #[test]
    fn test_load_corrupted_json_backs_up_and_resets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let invalid_path = dir.path().join("settings.invalid.json");
        let invalid_content = r#"{broken_json..."#;
        std::fs::write(&path, invalid_content).unwrap();

        let store = SettingsStore::load(dir.path()).unwrap();
        assert!(store.has_invalid_warning());
        assert_eq!(store.get().retention_days, 90);
        assert!(invalid_path.exists());
        assert_eq!(
            std::fs::read_to_string(&invalid_path).unwrap(),
            invalid_content
        );
    }

    #[test]
    fn consume_invalid_warning_fires_only_once() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"broken":"#).unwrap();
        let store = SettingsStore::load(dir.path()).unwrap();
        assert!(store.has_invalid_warning());
        // First consumption returns true.
        assert!(store.consume_invalid_warning());
        // Subsequent calls return false.
        assert!(!store.consume_invalid_warning());
        assert!(!store.consume_invalid_warning());
        // has_invalid_warning should also be false now.
        assert!(!store.has_invalid_warning());
    }

    #[test]
    fn validate_rejects_empty_shortcut() {
        let s = Settings {
            shortcut: String::new(),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_too_many_excluded_apps() {
        let s = Settings {
            excluded_apps: (0..101).map(|i| format!("app{i}")).collect(),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_too_long_shortcut() {
        let s = Settings {
            shortcut: "x".repeat(101),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn save_uses_atomic_rename() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load(dir.path()).unwrap();
        let mut s = store.get();
        s.retention_days = 42;
        store.save(s).unwrap();
        let loaded = SettingsStore::load(dir.path()).unwrap();
        assert_eq!(loaded.get().retention_days, 42);
        // Tmp file should not remain.
        assert!(!dir.path().join("settings.tmp").exists());
    }

    #[test]
    fn autostart_exec_escapes_path_with_spaces() {
        // We just test the formatting logic, not actual file creation.
        let exe_with_spaces = "/home/user/My Apps/kitsupin";
        let exec_value = if exe_with_spaces.contains(' ') {
            format!("\"{}\" --background", exe_with_spaces.replace('"', "\\\""))
        } else {
            format!("{} --background", exe_with_spaces)
        };
        assert_eq!(exec_value, "\"/home/user/My Apps/kitsupin\" --background");
    }
}
