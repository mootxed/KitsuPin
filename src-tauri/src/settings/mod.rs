use anyhow::Result;
use directories::ProjectDirs;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
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
    pub fn is_valid(&self) -> bool {
        (1..=3650).contains(&self.retention_days)
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
    has_invalid_warning: bool,
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
                                "Invalid retention_days ({}). Saving backup to settings.invalid.json and resetting to 90 days.",
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
                        log::error!("Corrupted JSON in settings.json. Saving backup to settings.invalid.json and resetting to defaults.");
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
            has_invalid_warning,
        }))
    }
    pub fn has_invalid_warning(&self) -> bool {
        self.has_invalid_warning
    }
    pub fn get(&self) -> Settings {
        self.value.read().clone()
    }
    pub fn save(&self, value: Settings) -> Result<()> {
        anyhow::ensure!(
            value.is_valid(),
            "срок хранения вне диапазона"
        );
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&value)?)?;
        secure_file(&tmp);
        std::fs::rename(tmp, &self.path)?;
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
        std::fs::write(path,format!("[Desktop Entry]\nType=Application\nName=KitsuPin\nComment=История буфера обмена\nExec={} --background\nTerminal=false\nX-GNOME-Autostart-enabled=true\nOnlyShowIn=KDE;\n",exe.display()))?;
    } else {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(
            path,
            "[Desktop Entry]\nType=Application\nName=KitsuPin\nHidden=true\n",
        )?;
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
        assert_eq!(std::fs::read_to_string(&invalid_path).unwrap(), invalid_content);
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
        assert_eq!(std::fs::read_to_string(&invalid_path).unwrap(), invalid_content);
    }
}

