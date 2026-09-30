//! Typed `EventBus`: the glue between the domain and its observers.
//!
//! Flow: `Command → Domain Event → Effect`.
//!
//! - A `Command` mutates `State`/`Cfg`, produces the `Effect`s the backend will
//!   execute, and *declares* (optionally) the domain event that represents what
//!   it did. The command knows its own event, never its consumers.
//! - The `Engine` publishes that event on the `EventBus`.
//! - Anyone may subscribe — renderer, IPC, future bars, hooks, logs, tests. A
//!   consumer reacts to the fact without knowing which command caused it.
//!
//! Events are semantic facts, not X11 calls, and handlers never mutate state
//! back into the command path. The bus exists to make extension cheap: a new
//! consumer subscribes instead of polling `State`. It is the only reason the
//! indirection is worth it, so a component that cannot be phrased as a
//! "something happened" fact does not get an event.

use crate::core::effect::Effect;
use crate::types::WindowId;

/// Domain events: observable facts about WM state, NOT X11 calls. Granularity
/// is semantic, never imperative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A window just entered the managed set (`MapRequest` handled).
    WindowMapped(WindowId),
    /// A window left the managed set (destroyed/unmapped/withdrawn).
    WindowUnmapped(WindowId),
    /// Keyboard/directional focus moved between windows.
    FocusChanged {
        from: Option<WindowId>,
        to: Option<WindowId>,
    },
    /// The active workspace changed on a monitor.
    WorkspaceChanged {
        monitor: usize,
        from: usize,
        to: usize,
    },
    /// A workspace's layout changed.
    LayoutChanged { monitor: usize, workspace: usize },
    /// A window was moved (within a workspace, between rooms, or to a monitor).
    WindowMoved(WindowId),
    /// A window's floating state flipped.
    FloatToggled(WindowId),
    /// A window's fullscreen state flipped.
    FullscreenToggled { win: WindowId, on: bool },
    /// A window's maximized state flipped.
    MaximizeToggled { win: WindowId, on: bool },
    /// The inner/outer gaps changed globally.
    GapsChanged,
    /// The default border width changed globally.
    BorderChanged,
    /// The WM is about to quit.
    SessionQuit,
    /// The WM is about to re-exec itself.
    SessionRestart,
}

/// What `Command::execute` returns: the effects for the backend plus the
/// optional domain event to publish.
#[derive(Debug)]
pub struct CommandReport {
    pub effects: Vec<Effect>,
    pub event: Option<Event>,
}

impl CommandReport {
    pub fn new(effects: Vec<Effect>) -> Self {
        Self {
            effects,
            event: None,
        }
    }

    pub fn with_event(effects: Vec<Effect>, event: Event) -> Self {
        Self {
            effects,
            event: Some(event),
        }
    }
}

/// A subscriber of domain events. Consumers implement this and react to the
/// facts they care about; they never mutate state back into the command path.
pub trait EventHandler {
    fn on_event(&mut self, event: &Event);
}

/// Typed publish/subscribe bus owned by the `Engine`.
#[derive(Default)]
pub struct EventBus {
    handlers: Vec<Box<dyn EventHandler>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&mut self, handler: Box<dyn EventHandler>) {
        self.handlers.push(handler);
    }

    /// Notify every handler, in subscription order, on the publishing thread.
    pub fn publish(&mut self, event: &Event) {
        for h in &mut self.handlers {
            h.on_event(event);
        }
    }
}
