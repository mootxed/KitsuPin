use crate::capabilities::{update_clipboard_monitoring_status, CapabilityStatus};
use crate::clipboard::backends::{ClipboardMonitor, MonitorMode};
use crate::clipboard::session::SessionType;
use crate::clipboard::ClipboardGeneration;
use std::sync::Arc;

pub struct WaylandClipboardMonitor;

impl ClipboardMonitor for WaylandClipboardMonitor {
    fn name(&self) -> &'static str {
        "wayland-limited"
    }

    fn session_type(&self) -> SessionType {
        SessionType::Wayland
    }

    fn supports_passive_monitoring(&self) -> bool {
        false
    }

    fn start(
        &self,
        _generation: Arc<ClipboardGeneration>,
    ) -> anyhow::Result<MonitorMode> {
        log::info!(
            "Passive global clipboard monitoring disabled for Wayland session. KitsuPin running in limited support mode."
        );
        update_clipboard_monitoring_status(CapabilityStatus::Unavailable);
        Ok(MonitorMode::Disabled)
    }
}
