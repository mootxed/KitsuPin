use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionType {
    X11,
    Wayland,
    Unknown,
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionType::X11 => write!(f, "x11"),
            SessionType::Wayland => write!(f, "wayland"),
            SessionType::Unknown => write!(f, "unknown"),
        }
    }
}

/// Detects the graphical session type from environment variables.
///
/// Order of evaluation:
/// 1. `XDG_SESSION_TYPE`: if "wayland" -> Wayland; if "x11" -> X11
/// 2. `WAYLAND_DISPLAY`: if set -> Wayland
/// 3. `DISPLAY`: if set -> X11
/// 4. Otherwise -> Unknown
pub fn detect_session_type_with_env(fetch_env: impl Fn(&str) -> Option<String>) -> SessionType {
    if let Some(xdg_type) = fetch_env("XDG_SESSION_TYPE") {
        let normalized = xdg_type.to_lowercase();
        if normalized.contains("wayland") {
            return SessionType::Wayland;
        }
        if normalized.contains("x11") {
            return SessionType::X11;
        }
    }

    if fetch_env("WAYLAND_DISPLAY").is_some() {
        return SessionType::Wayland;
    }

    if fetch_env("DISPLAY").is_some() {
        return SessionType::X11;
    }

    SessionType::Unknown
}

/// Detects session type using actual system environment.
pub fn detect_session_type() -> SessionType {
    detect_session_type_with_env(|key| std::env::var(key).ok())
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
    fn detects_wayland_from_xdg_session_type() {
        let env = mock_env(&[("XDG_SESSION_TYPE", "wayland")]);
        assert_eq!(detect_session_type_with_env(env), SessionType::Wayland);
    }

    #[test]
    fn detects_x11_from_xdg_session_type() {
        let env = mock_env(&[("XDG_SESSION_TYPE", "x11")]);
        assert_eq!(detect_session_type_with_env(env), SessionType::X11);
    }

    #[test]
    fn detects_wayland_from_wayland_display_fallback() {
        let env = mock_env(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert_eq!(detect_session_type_with_env(env), SessionType::Wayland);
    }

    #[test]
    fn detects_x11_from_display_fallback() {
        let env = mock_env(&[("DISPLAY", ":0")]);
        assert_eq!(detect_session_type_with_env(env), SessionType::X11);
    }

    #[test]
    fn prefers_xdg_session_type_over_display() {
        let env = mock_env(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")]);
        assert_eq!(detect_session_type_with_env(env), SessionType::Wayland);
    }

    #[test]
    fn returns_unknown_when_no_env_vars() {
        let env = mock_env(&[]);
        assert_eq!(detect_session_type_with_env(env), SessionType::Unknown);
    }
}
