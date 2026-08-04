use crate::capabilities::{update_clipboard_monitoring_status, CapabilityStatus};
use crate::clipboard::backends::{ClipboardMonitor, MonitorMode};
use crate::clipboard::session::SessionType;
use crate::clipboard::ClipboardGeneration;
use std::sync::Arc;
use std::time::Duration;

pub struct PollingClipboardMonitor {
    pub interval: Duration,
}

impl Default for PollingClipboardMonitor {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(350),
        }
    }
}

impl ClipboardMonitor for PollingClipboardMonitor {
    fn name(&self) -> &'static str {
        "polling-fallback"
    }

    fn session_type(&self) -> SessionType {
        SessionType::X11
    }

    fn supports_passive_monitoring(&self) -> bool {
        true
    }

    fn start(&self, _generation: Arc<ClipboardGeneration>) -> anyhow::Result<MonitorMode> {
        update_clipboard_monitoring_status(CapabilityStatus::Available);
        Ok(MonitorMode::Polling(self.interval))
    }
}
