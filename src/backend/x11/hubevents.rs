// maverick/src/backend/x11/hubevents.rs
//
// The production subscriber of the typed `EventBus`. It renders domain events
// into the `subscribe` wire protocol of the control hub, so external
// `maverickctl subscribe` clients keep receiving the same `focus <id>` and
// `workspace <ws> <mon>` lines they always did — but now derived from the
// single, typed event stream instead of a hand-written string diff in
// `publish_state`.
//
// The sink dedupes on its last-known values: commands and the backend's own
// focus handling both announce the same transition, and the dedupe collapses
// the duplicates into one line while never dropping a *real* change.

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