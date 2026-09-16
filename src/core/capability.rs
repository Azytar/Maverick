//! Capability Layer: la API pública de **lectura** de Maverick.
//!
//! What owns: `Query<'a>` (borrow of `&State`) and `WindowInfo` — stable,
//! read-only projections for bars/hooks/tests. No mutation, no handles.
//!
//! Exposes: `Query` (`monitor_count`, `selected_monitor`, `active_workspace`,
//! `workspace_count`, `current_layout`, `focused_window`, `visible_windows`,
//! `window`/`windows`) and `WindowInfo` (decoupled from internal `Client`).
//!
//! Leaves to others: all mutation via `Engine::execute(Command)`; X11/GL;
//! layout math; event publishing. Internal `State`/`Monitor`/`Workspace`/
//! `Client` remain private and free to evolve without breaking consumers.
//!
//! Invariants: read-only (`&self` only), three-consumer rule (each query must
//! serve a bar, a hook, and a test), clamping on selected monitor.
//!
//! Una barra, un hook o una herramienta externa NO debería navegar por el
//! `State`, `Monitor`, `Workspace` o `Client` internos — esos pueden cambiar
//! en cualquier versión. En su lugar pregunta a esta capa consultas estables:
//!
//! ```ignore
//! let q = engine.query();
//! q.active_workspace();   // → ¿qué workspace está visible?
//! q.focused_window();     // → ¿qué ventana tiene el foco?
//! q.visible_windows();    // → ¿qué ventanas se ven ahora?
//! q.current_layout();     // → ¿qué layout está activo?
//! ```
//!
//! Es una capa de **lectura solamente**: no hay ningún método `&mut self`.
//! Escribir se hace exclusivamente vía `Engine::execute(Command)`. Así los
//! programas externos no dependen del modelo interno y el escritor tiene un
//! único camino de entrada.
//!
//! Regla del compás: cada consulta aquí paga por su existencia si sirve a una
//! barra, un hook y un test a la vez (tres consumidores). No añadimos consultas
//! "por si acaso".

use crate::types::{LayoutKind, State, WindowId};

/// Información pública y estable de una ventana. Deliberadamente desacoplada
/// del `Client` interno para que el modelo interno pueda evolucionar sin
/// romper a los consumidores externos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub class: String,
    pub instance: String,
    pub floating: bool,
    pub fullscreen: bool,
    pub workspace: usize,
    pub monitor: usize,
}

/// Vista de solo lectura sobre el estado del WM. Pide prestado `&State` y
/// expone únicamente consultas estables — nunca mutación.
pub struct Query<'a> {
    state: &'a State,
}

impl<'a> Query<'a> {
    /// Borrow `State` for read-only queries. No mutation path exists on `Query`.
    pub fn new(state: &'a State) -> Self {
        Self { state }
    }

    // ── Monitores ───────────────────────────────────────────────────────────

    /// Number of live monitors.
    pub fn monitor_count(&self) -> usize {
        self.state.monitors.len()
    }

    /// Selected monitor index, clamped to `monitor_count - 1`.
    pub fn selected_monitor(&self) -> usize {
        self.state
            .sel_mon
            .min(self.monitor_count().saturating_sub(1))
    }

    // ── Workspace activo ────────────────────────────────────────────────────

    /// Active workspace index on the selected monitor.
    pub fn active_workspace(&self) -> usize {
        self.state
            .monitors
            .get(self.selected_monitor())
            .map_or(0, |m| m.active_ws)
    }

    /// Number of workspaces on the selected monitor.
    pub fn workspace_count(&self) -> usize {
        self.state
            .monitors
            .get(self.selected_monitor())
            .map_or(0, |m| m.workspaces.len())
    }

    // ── Layout ──────────────────────────────────────────────────────────────

    /// Layout active on the selected monitor's active workspace.
    pub fn current_layout(&self) -> LayoutKind {
        self.state
            .monitors
            .get(self.selected_monitor())
            .and_then(|m| m.workspaces.get(m.active_ws))
            .map_or(LayoutKind::Column, |w| w.layout)
    }

    // ── Foco ────────────────────────────────────────────────────────────────

    /// Focused window on the selected monitor, if any.
    pub fn focused_window(&self) -> Option<WindowId> {
        self.state
            .monitors
            .get(self.selected_monitor())
            .and_then(|m| m.focused)
    }

    // ── Ventanas visibles ───────────────────────────────────────────────────

    /// IDs de todas las ventanas del workspace activo (tiled + floating).
    pub fn visible_windows(&self) -> Vec<WindowId> {
        let mi = self.selected_monitor();
        let Some(m) = self.state.monitors.get(mi) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(w) = m.workspaces.get(m.active_ws) {
            for col in &w.columns {
                out.extend(col.windows.iter().copied());
            }
            out.extend(w.floats.iter().copied());
        }
        out
    }

    /// Información pública de una ventana concreta, si existe.
    pub fn window(&self, id: WindowId) -> Option<WindowInfo> {
        let c = self.state.clients.get(&id)?;
        Some(WindowInfo {
            id: c.window,
            title: c.name.clone(),
            class: c.class.clone(),
            instance: c.instance.clone(),
            floating: c.is_float(),
            fullscreen: c.is_fullscreen(),
            workspace: c.workspace,
            monitor: c.monitor,
        })
    }

    /// Información pública de todas las ventanas gestionadas.
    pub fn windows(&self) -> Vec<WindowInfo> {
        self.state
            .clients
            .values()
            .filter_map(|c| self.window(c.window))
            .collect()
    }
}
