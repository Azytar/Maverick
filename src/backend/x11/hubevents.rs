//! `EventBus` subscriber: renders domain events to the control-hub wire protocol.
//!
//! Emits the two line-oriented events `maverickctl subscribe` understands:
//! `focus <id>` and `workspace <ws> <mon>`. Every other domain event is
//! dropped, so a subscriber sees a level-triggered summary rather than the
//! event firehose.
//!
//! Deduplication is required because the same transition can be published
//! twice — once by the command that caused it and once by the backend's own X11
//! handling — and a subscriber would otherwise see a duplicated line.
//!
//! Wire contract worth preserving: `focus 0` means "focus was cleared"
//! (`FocusChanged { to: None }`); there is no separate event for it.

use crate::core::event::{Event, EventHandler};
use crate::types::WindowId;
use maverick_sys::ControlHub;

pub struct HubEventSink {
    hub: ControlHub,
    last_focus: Option<WindowId>,
    /// Last (workspace, monitor) pair already emitted as a line.
    last_ws: Option<(usize, usize)>,
}

impl HubEventSink {
    pub fn new(hub: ControlHub) -> Self {
        Self {
            hub,
            last_focus: None,
            last_ws: None,
        }
    }
}

impl EventHandler for HubEventSink {
    fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::FocusChanged { to, .. } => {
                let id = to.unwrap_or(0);
                if self.last_focus != Some(id) {
                    self.last_focus = Some(id);
                    self.hub.emit(format!("focus {id}"));
                }
            }
            // Keyed on both fields: the same workspace number on another
            // monitor is a different state and must be re-emitted.
            Event::WorkspaceChanged { monitor, to, .. }
                if self.last_ws != Some((*to, *monitor)) =>
            {
                self.last_ws = Some((*to, *monitor));
                self.hub.emit(format!("workspace {to} {monitor}"));
            }
            _ => {}
        }
    }
}
