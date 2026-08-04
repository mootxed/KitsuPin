pub mod disabled;
pub mod wayland;
pub mod x11;

use crate::clipboard::session::{detect_session_type, SessionType};
use crate::clipboard::ClipboardGeneration;
use std::sync::{mpsc::Receiver, Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardNotification {
    pub sequence: u64,
    pub owner: u32,
    pub timestamp: u32,
}

pub trait ClipboardMonitor: Send + Sync {
    fn name(&self) -> &'static str;
    fn session_type(&self) -> SessionType;
    fn supports_passive_monitoring(&self) -> bool;
    fn start(
        &self,
        generation: Arc<ClipboardGeneration>,
    ) -> anyhow::Result<Option<Receiver<ClipboardNotification>>>;
}

pub fn select_monitor() -> Box<dyn ClipboardMonitor> {
    match detect_session_type() {
        SessionType::X11 => Box::new(x11::X11ClipboardMonitor),
        SessionType::Wayland => Box::new(wayland::WaylandClipboardMonitor),
        SessionType::Unknown => Box::new(disabled::DisabledClipboardMonitor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_monitor_instantiates_monitor() {
        let monitor = select_monitor();
        assert!(!monitor.name().is_empty());
    }
}
