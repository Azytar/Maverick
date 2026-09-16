//! `EventBus` subscriber: renders domain events to the
//! control-hub wire protocol.
//!
//! Deduplicates events (commands + backend both fire
//! the same events) and emits `focus <id>` and
//! `workspace <ws> <mon>` lines for `maverickctl subscribe`.
//!
//! # Invariants
//!
//! - `last_focus`/`last_ws` prevent duplicate events.
//! - Only focus/workspace events are forwarded; other
//!   events are ignored.

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
