use crate::capabilities::{update_clipboard_monitoring_status, CapabilityStatus};
use crate::clipboard::backends::{ClipboardMonitor, MonitorMode};
use crate::clipboard::session::SessionType;
use crate::clipboard::ClipboardGeneration;
use std::sync::Arc;

pub struct DisabledClipboardMonitor;

impl ClipboardMonitor for DisabledClipboardMonitor {
    fn name(&self) -> &'static str {
        "unknown-disabled"
    }

    fn session_type(&self) -> SessionType {
        SessionType::Unknown
    }

    fn supports_passive_monitoring(&self) -> bool {
        false
    }

    fn start(&self, _generation: Arc<ClipboardGeneration>) -> anyhow::Result<MonitorMode> {
        log::info!("Passive global clipboard monitoring disabled for unknown graphics session.");
        update_clipboard_monitoring_status(CapabilityStatus::Unavailable);
        Ok(MonitorMode::Disabled)
    }
}
