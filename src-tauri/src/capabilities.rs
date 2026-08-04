use crate::clipboard::session::{detect_session_type_with_env, SessionType};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilities {
    pub session_type: SessionType,
    pub global_clipboard_monitoring: bool,
    pub image_clipboard: bool,
    pub global_shortcuts: bool,
    pub tray: bool,
    pub monitoring_mode_description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "status", content = "message", rename_all = "camelCase")]
pub enum CapabilityStatus {
    Available,
    Degraded(String),
    Unavailable,
    Failed(String),
    NotTested,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapabilities {
    pub platform: PlatformCapabilities,
    pub clipboard_monitoring: CapabilityStatus,
    pub shortcut: CapabilityStatus,
    pub tray: CapabilityStatus,
}

static RUNTIME_CAPABILITIES: OnceLock<RwLock<RuntimeCapabilities>> = OnceLock::new();

pub fn get_platform_capabilities_with_env(
    fetch_env: impl Fn(&str) -> Option<String>,
) -> PlatformCapabilities {
    let session_type = detect_session_type_with_env(fetch_env);
    match session_type {
        SessionType::X11 => PlatformCapabilities {
            session_type,
            global_clipboard_monitoring: true,
            image_clipboard: true,
            global_shortcuts: true,
            tray: true,
            monitoring_mode_description:
                "X11 активное фоновое отслеживание буфера обмена (XFixes)".into(),
        },
        SessionType::Wayland => PlatformCapabilities {
            session_type,
            global_clipboard_monitoring: false,
            image_clipboard: true,
            global_shortcuts: false,
            tray: true,
            monitoring_mode_description:
                "Ограниченный режим Wayland: фоновый мониторинг буфера обмена ограничен политиками безопасности сессии".into(),
        },
        SessionType::Unknown => PlatformCapabilities {
            session_type,
            global_clipboard_monitoring: false,
            image_clipboard: true,
            global_shortcuts: false,
            tray: true,
            monitoring_mode_description:
                "Неизвестная графическая среда: фоновое отслеживание отключено".into(),
        },
    }
}

pub fn get_platform_capabilities() -> PlatformCapabilities {
    get_platform_capabilities_with_env(|k| std::env::var(k).ok())
}

pub fn get_runtime_capabilities() -> RuntimeCapabilities {
    let lock = RUNTIME_CAPABILITIES.get_or_init(|| {
        let platform = get_platform_capabilities();
        RwLock::new(RuntimeCapabilities {
            platform,
            clipboard_monitoring: CapabilityStatus::NotTested,
            shortcut: CapabilityStatus::NotTested,
            tray: CapabilityStatus::NotTested,
        })
    });
    lock.read().clone()
}

pub fn update_shortcut_status(status: CapabilityStatus) {
    let lock = RUNTIME_CAPABILITIES.get_or_init(|| {
        let platform = get_platform_capabilities();
        RwLock::new(RuntimeCapabilities {
            platform,
            clipboard_monitoring: CapabilityStatus::NotTested,
            shortcut: CapabilityStatus::NotTested,
            tray: CapabilityStatus::NotTested,
        })
    });
    lock.write().shortcut = status;
}

pub fn update_clipboard_monitoring_status(status: CapabilityStatus) {
    let lock = RUNTIME_CAPABILITIES.get_or_init(|| {
        let platform = get_platform_capabilities();
        RwLock::new(RuntimeCapabilities {
            platform,
            clipboard_monitoring: CapabilityStatus::NotTested,
            shortcut: CapabilityStatus::NotTested,
            tray: CapabilityStatus::NotTested,
        })
    });
    lock.write().clipboard_monitoring = status;
}

pub fn update_tray_status(status: CapabilityStatus) {
    let lock = RUNTIME_CAPABILITIES.get_or_init(|| {
        let platform = get_platform_capabilities();
        RwLock::new(RuntimeCapabilities {
            platform,
            clipboard_monitoring: CapabilityStatus::NotTested,
            shortcut: CapabilityStatus::NotTested,
            tray: CapabilityStatus::NotTested,
        })
    });
    lock.write().tray = status;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mock_env(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let env_map: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| env_map.get(key).cloned()
    }

    #[test]
    fn x11_capabilities_have_full_monitoring() {
        let caps = get_platform_capabilities_with_env(mock_env(&[("XDG_SESSION_TYPE", "x11")]));
        assert_eq!(caps.session_type, SessionType::X11);
        assert!(caps.global_clipboard_monitoring);
        assert!(caps.global_shortcuts);
    }

    #[test]
    fn wayland_capabilities_have_limited_monitoring() {
        let caps = get_platform_capabilities_with_env(mock_env(&[("XDG_SESSION_TYPE", "wayland")]));
        assert_eq!(caps.session_type, SessionType::Wayland);
        assert!(!caps.global_clipboard_monitoring);
        assert!(!caps.global_shortcuts);
        assert!(caps.tray);
    }

    #[test]
    fn runtime_capabilities_tracks_status_updates() {
        update_shortcut_status(CapabilityStatus::Failed(
            "Key registration error".to_string(),
        ));
        let runtime = get_runtime_capabilities();
        assert_eq!(
            runtime.shortcut,
            CapabilityStatus::Failed("Key registration error".to_string())
        );

        update_clipboard_monitoring_status(CapabilityStatus::Degraded(
            "XFixes недоступен, используется polling каждые 350 мс".to_string(),
        ));
        let runtime = get_runtime_capabilities();
        assert_eq!(
            runtime.clipboard_monitoring,
            CapabilityStatus::Degraded(
                "XFixes недоступен, используется polling каждые 350 мс".to_string()
            )
        );
    }
}
