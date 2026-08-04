use crate::capabilities::{get_platform_capabilities, PlatformCapabilities};
use crate::clipboard::session::{detect_session_type, SessionType};

pub trait ClipboardBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn session_type(&self) -> SessionType;
    fn capabilities(&self) -> PlatformCapabilities;
    fn supports_passive_monitoring(&self) -> bool;
}

pub struct X11ClipboardBackend;

impl ClipboardBackend for X11ClipboardBackend {
    fn name(&self) -> &'static str {
        "x11-xfixes"
    }

    fn session_type(&self) -> SessionType {
        SessionType::X11
    }

    fn capabilities(&self) -> PlatformCapabilities {
        get_platform_capabilities()
    }

    fn supports_passive_monitoring(&self) -> bool {
        true
    }
}

pub struct WaylandClipboardBackend;

impl ClipboardBackend for WaylandClipboardBackend {
    fn name(&self) -> &'static str {
        "wayland-limited"
    }

    fn session_type(&self) -> SessionType {
        SessionType::Wayland
    }

    fn capabilities(&self) -> PlatformCapabilities {
        get_platform_capabilities()
    }

    fn supports_passive_monitoring(&self) -> bool {
        false
    }
}

pub struct FallbackClipboardBackend;

impl ClipboardBackend for FallbackClipboardBackend {
    fn name(&self) -> &'static str {
        "unknown-fallback"
    }

    fn session_type(&self) -> SessionType {
        SessionType::Unknown
    }

    fn capabilities(&self) -> PlatformCapabilities {
        get_platform_capabilities()
    }

    fn supports_passive_monitoring(&self) -> bool {
        false
    }
}

pub fn select_backend() -> Box<dyn ClipboardBackend> {
    match detect_session_type() {
        SessionType::X11 => Box::new(X11ClipboardBackend),
        SessionType::Wayland => Box::new(WaylandClipboardBackend),
        SessionType::Unknown => Box::new(FallbackClipboardBackend),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_backend_instantiates_backend() {
        let backend = select_backend();
        assert!(!backend.name().is_empty());
    }
}
