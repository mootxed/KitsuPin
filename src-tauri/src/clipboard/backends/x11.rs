use crate::capabilities::{update_clipboard_monitoring_status, CapabilityStatus};
use crate::clipboard::backends::{ClipboardMonitor, ClipboardNotification, MonitorMode};
use crate::clipboard::session::SessionType;
use crate::clipboard::ClipboardGeneration;
use std::sync::{mpsc, Arc};
use std::time::Duration;
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::{ConnectionExt as XfixesExt, SelectionEventMask},
        xproto::ConnectionExt as XprotoExt,
        Event,
    },
};

pub struct X11ClipboardMonitor;

impl X11ClipboardMonitor {
    fn try_start_xfixes(
        &self,
        generation: Arc<ClipboardGeneration>,
    ) -> anyhow::Result<mpsc::Receiver<ClipboardNotification>> {
        let (sender, receiver) = mpsc::channel();
        let (connection, screen_number) = x11rb::connect(None)?;
        connection.xfixes_query_version(5, 0)?.reply()?;
        let clipboard = connection.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
        let root = connection.setup().roots[screen_number].root;
        connection.xfixes_select_selection_input(
            root,
            clipboard,
            SelectionEventMask::SET_SELECTION_OWNER,
        )?;
        connection.flush()?;

        std::thread::spawn(move || loop {
            match connection.wait_for_event() {
                Ok(Event::XfixesSelectionNotify(event)) if event.selection == clipboard => {
                    let sequence = generation.next();
                    let notification = ClipboardNotification::X11Changed {
                        sequence,
                        owner: event.owner,
                        timestamp: event.timestamp,
                    };
                    if sender.send(notification).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!("XFixes Clipboard notifications stopped: {error}");
                    update_clipboard_monitoring_status(CapabilityStatus::Degraded(
                        "XFixes недоступен, используется polling каждые 350 мс".to_string(),
                    ));
                    break;
                }
            }
        });

        Ok(receiver)
    }
}

impl ClipboardMonitor for X11ClipboardMonitor {
    fn name(&self) -> &'static str {
        "x11-xfixes"
    }

    fn session_type(&self) -> SessionType {
        SessionType::X11
    }

    fn start(&self, generation: Arc<ClipboardGeneration>) -> anyhow::Result<MonitorMode> {
        match self.try_start_xfixes(generation) {
            Ok(receiver) => {
                update_clipboard_monitoring_status(CapabilityStatus::Available);
                Ok(MonitorMode::EventDriven(receiver))
            }
            Err(error) => {
                log::warn!("X11 XFixes initialization failed: {error}. Falling back to polling.");
                update_clipboard_monitoring_status(CapabilityStatus::Degraded(
                    "XFixes недоступен, используется polling каждые 350 мс".to_string(),
                ));
                Ok(MonitorMode::Polling(Duration::from_millis(350)))
            }
        }
    }
}
