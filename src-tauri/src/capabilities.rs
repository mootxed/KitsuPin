use crate::clipboard::session::{detect_session_type_with_env, SessionType};
use serde::{Deserialize, Serialize};

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
        let caps =
            get_platform_capabilities_with_env(mock_env(&[("XDG_SESSION_TYPE", "wayland")]));
        assert_eq!(caps.session_type, SessionType::Wayland);
        assert!(!caps.global_clipboard_monitoring);
        assert!(!caps.global_shortcuts);
        assert!(caps.tray);
    }

    #[test]
    fn unknown_session_has_safe_fallback() {
        let caps = get_platform_capabilities_with_env(mock_env(&[]));
        assert_eq!(caps.session_type, SessionType::Unknown);
        assert!(!caps.global_clipboard_monitoring);
    }
}
