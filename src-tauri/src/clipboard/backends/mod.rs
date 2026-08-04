pub mod disabled;
pub mod wayland;
pub mod x11;

use crate::clipboard::session::{detect_session_type, SessionType};
use crate::clipboard::ClipboardGeneration;
use std::sync::{mpsc::Receiver, Arc};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardNotification {
    Changed {
        sequence: u64,
    },
    X11Changed {
        sequence: u64,
        owner: u32,
        timestamp: u32,
    },
}

impl ClipboardNotification {
    #[allow(dead_code)]
    pub fn sequence(&self) -> u64 {
        match self {
            ClipboardNotification::Changed { sequence } => *sequence,
            ClipboardNotification::X11Changed { sequence, .. } => *sequence,
        }
    }
}

#[derive(Debug)]
pub enum MonitorMode {
    EventDriven(Receiver<ClipboardNotification>),
    Polling(Duration),
    Disabled,
}

pub trait ClipboardMonitor: Send + Sync {
    fn name(&self) -> &'static str;
    fn session_type(&self) -> SessionType;
    #[allow(dead_code)]
    fn supports_passive_monitoring(&self) -> bool;
    fn start(&self, generation: Arc<ClipboardGeneration>) -> anyhow::Result<MonitorMode>;
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
