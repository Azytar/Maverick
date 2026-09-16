//! `EventBus` tipado: el pegamento entre el dominio y sus observadores.
//!
//! What owns: `Event` (domain facts), `CommandReport` (`Effect`s + optional
//! `Event`), `EventHandler` trait, and `EventBus` (publish/subscribe).
//!
//! Exposes: `Event` variants (`WindowMapped`, `FocusChanged`,
//! `WorkspaceChanged`, …), `CommandReport::new`/`with_event`, `EventHandler`,
//! `EventBus::subscribe`/`publish`.
//!
//! Leaves to others: mutation (`Command::execute`) and X11/GL (`Effect`
//! execution). Handlers never mutate back into the command path.
//!
//! Invariants: `Command → Event → Effect` ordering; a `Command` declares its
//! own `Event` but never knows its consumers; `Event`s are semantic (not X11
//! calls). Exists to make extension cheap: new consumers subscribe instead of
//! polling `State`.
//!
//! Modelo (según la auditoría): `Command → Domain Event → Effect`.
//!
//! - Un `Command` muta `State`/`Cfg`, produce los `Effect` que el backend
//!   ejecutará, Y declara (opcionalmente) el **evento de dominio** que
//!   representa. El comando conoce SU evento, pero jamás a sus consumidores.
//! - El `Engine` publica ese evento en el `EventBus`.
//! - Cualquiera puede suscribirse: renderer, IPC, futuras barras, hooks,
//!   logs, tests. Los consumidores no saben qué comando lo originó; solo
//!   reaccionan al hecho.
//!
//! Regla del compás: solo existe porque reduce el coste de extender. Un
//! consumidor nuevo (p. ej. una barra) se suscribe y recibe cambios
//! incrementales en lugar de tener que sondear el estado completo.

use crate::core::effect::Effect;
use crate::types::WindowId;

/// Eventos de dominio. Son hechos observables del estado del WM, NO llamadas
/// a X11. Granularidad semántica (no imperativa).
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
    /// The native wallpaper source/mode changed (set/clear/mode).
    WallpaperChanged,
}

/// Lo que devuelve `Command::execute`: los efectos para el backend y el
/// (opcional) evento de dominio que se debe publicar.
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

/// A suscriber of domain events. Consumers implement this and react to the
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

    pub fn publish(&mut self, event: &Event) {
        for h in &mut self.handlers {
            h.on_event(event);
        }
    }
}
