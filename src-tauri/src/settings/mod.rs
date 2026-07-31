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
}
impl SettingsStore {
    pub fn load(data_dir: &Path) -> Result<Arc<Self>> {
        let path = data_dir.join("settings.json");
        let value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        Ok(Arc::new(Self {
            path,
            value: RwLock::new(value),
        }))
    }
    pub fn get(&self) -> Settings {
        self.value.read().clone()
    }
    pub fn save(&self, value: Settings) -> Result<()> {
        anyhow::ensure!(
            (1..=3650).contains(&value.retention_days),
            "срок хранения вне диапазона"
        );
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&value)?)?;
        std::fs::rename(tmp, &self.path)?;
        *self.value.write() = value;
        Ok(())
    }
}
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("app", "pastily", "Pastily")
        .ok_or_else(|| anyhow::anyhow!("XDG data directory недоступен"))
}
pub fn autostart_path() -> Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| anyhow::anyhow!("XDG config directory недоступен"))?;
    Ok(config.join("autostart/pastily.desktop"))
}
pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = autostart_path()?;
    let system_entry = Path::new("/etc/xdg/autostart/pastily.desktop");
    if enabled && system_entry.exists() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    } else if enabled {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let exe = std::env::current_exe()?;
        std::fs::write(path,format!("[Desktop Entry]\nType=Application\nName=Pastily\nComment=История буфера обмена\nExec={} --background\nTerminal=false\nX-GNOME-Autostart-enabled=true\nOnlyShowIn=KDE;\n",exe.display()))?;
    } else {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(
            path,
            "[Desktop Entry]\nType=Application\nName=Pastily\nHidden=true\n",
        )?;
    }
    Ok(())
}
