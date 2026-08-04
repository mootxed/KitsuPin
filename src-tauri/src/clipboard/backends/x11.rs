use crate::capabilities::{update_clipboard_monitoring_status, CapabilityStatus};
use crate::clipboard::backends::{ClipboardMonitor, ClipboardNotification};
use crate::clipboard::session::SessionType;
use crate::clipboard::ClipboardGeneration;
use std::sync::{mpsc, Arc};
use std::sync::mpsc::Receiver;
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::{ConnectionExt as XfixesExt, SelectionEventMask},
        xproto::ConnectionExt as XprotoExt,
        Event,
    },
};

pub struct X11ClipboardMonitor;

impl ClipboardMonitor for X11ClipboardMonitor {
    fn name(&self) -> &'static str {
        "x11-xfixes"
    }

    fn session_type(&self) -> SessionType {
        SessionType::X11
    }

    fn supports_passive_monitoring(&self) -> bool {
        true
    }

    fn start(
        &self,
        generation: Arc<ClipboardGeneration>,
    ) -> anyhow::Result<Option<Receiver<ClipboardNotification>>> {
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

        update_clipboard_monitoring_status(CapabilityStatus::Available);

        std::thread::spawn(move || loop {
            match connection.wait_for_event() {
                Ok(Event::XfixesSelectionNotify(event)) if event.selection == clipboard => {
                    let sequence = generation.next();
                    let notification = ClipboardNotification {
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
                    update_clipboard_monitoring_status(CapabilityStatus::Failed(error.to_string()));
                    break;
                }
            }
        });

        Ok(Some(receiver))
    }
}
