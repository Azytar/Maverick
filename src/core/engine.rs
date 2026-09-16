//! Central mutation entry point — the only writer of `State`/`Cfg`.
//!
//! What owns: `Engine` (`State` + `Cfg` + `EventBus`), and the wiring that maps
//! a wire `Action` into typed `Command`s via `dispatch`.
//!
//! Exposes: `Engine::new`, `apply_camera_cfg`, `subscribe`/`notify`, `query`,
//! `execute` (single command), `execute_batch` (coalesced transaction), and
//! `dispatch` (canonical wire → command adapter).
//!
//! Leaves to others: all X11/GL side-effects (owned by `Backend::execute` over
//! `Effect`s), layout projection (`layout::arrange`), presentation overlays
//! (`present::present_into`), and the `AppliedState`/reconciler diff.
//!
//! Invariants: every mutation funnels through `execute`/`execute_batch`, which
//! publish exactly one `Event` stream and append a centralized `pending_focus`
//! safety net (`reconcile_pending_focus_after_transition`) before the debug-only
//! `assert_invariants`. Batches coalesce effects/events into a single publish.

use crate::config::Cfg;
use crate::core::commands::{
    CollapseColumn, Command, FocusDirection, FocusMonitor, GrowColumn, KillWindow, MoveToWorkspace,
    MoveWindow, MoveWindowToMonitor, NewColumn, OverviewEnter, OverviewNav, PageSnap, Quit,
    Restart, SetLayout, SetWallpaper, Spawn, ToggleFloat, ToggleFullscreen, ToggleMaximize,
    ToggleOverview, ViewWorkspace, ViewportZoom,
};
use crate::core::effect::Effect;
use crate::core::event::{Event, EventBus, EventHandler};
use crate::types::*;

/// Central state machine: owns `State` + `Cfg` and is the sole path that
/// mutates them. Backends observe via `Effect`s; bars/tests via `EventBus`.
pub struct Engine {
    pub state: State,
    pub cfg: Cfg,
    bus: EventBus,
}

impl Engine {
    pub fn new(cfg: Cfg) -> Self {
        Self {
            state: State::new(),
            cfg,
            bus: EventBus::new(),
        }
    }

    /// Push the configured scroll-camera spring constants
    /// (`Cfg::animations.stiffness` / `Cfg::animations.damping`) into every
    /// workspace camera. `Camera::new` can't take them at construction — a
    /// workspace is built from its tag alone and `Monitor::reconcile_workspaces`
    /// creates fresh ones on hotplug — so the configured values reach the
    /// runtime scroll physics here. Call after every (re)build of the
    /// monitor/workspace set: startup, config reload, and `RandR` hotplug.
    pub fn apply_camera_cfg(&mut self) {
        // Every spring value coming from config is sanitized against the
        // real stability region of the integrator (see `sanitize_spring`) —
        // a NaN/inf or zero/negative stiffness from a config file can never
        // reach the physics.
        let (stiffness, damping) =
            sanitize_spring(self.cfg.animations.stiffness, self.cfg.animations.damping);
        for mon in &mut self.state.monitors {
            for ws in &mut mon.workspaces {
                ws.camera.stiffness = stiffness;
                ws.camera.damping = damping;
            }
        }
    }

    /// Subscribe a handler to domain events. This is the seam where bars,
    /// the IPC hub, hooks, and tests observe what changed — without knowing
    /// which command caused it.
    pub fn subscribe(&mut self, handler: Box<dyn EventHandler>) {
        self.bus.subscribe(handler);
    }

    /// Publish a domain event that did NOT originate from a `Command` — e.g. a
    /// pointer-driven focus change, or a window entering/leaving the managed
    /// set from the backend's own X11 handling. Subscribers then see exactly
    /// one event stream no matter who caused the transition: commands announce
    /// their own events through `execute`, and the backend announces the rest
    /// here.
    pub fn notify(&mut self, ev: Event) {
        self.bus.publish(&ev);
    }

    /// Read-only public view of the WM for external consumers (bars, hooks,
    /// tests). Never write through this — write via `execute(Command)`.
    pub fn query(&self) -> crate::core::capability::Query<'_> {
        crate::core::capability::Query::new(&self.state)
    }

    /// Execute a single command: applies it to `State`/`Cfg`, publishes its
    /// domain event, and returns the effects for the backend. A single user
    /// gesture maps to one command, so one state publish here is correct.
    ///
    /// Safety net: before returning, reconciles any `pending_focus` whose overlay
    /// owner is no longer presented (invariant #8c) via
    /// `reconcile_pending_focus_after_transition`, appending `FocusWindow` if no
    /// such effect already exists, then checks `assert_invariants` in debug.
    pub fn execute(&mut self, mut cmd: impl Command) -> Vec<Effect> {
        let report = cmd.execute(&mut self.state, &mut self.cfg);
        if let Some(ev) = &report.event {
            self.bus.publish(ev);
        }
        let mut effects = report.effects;
        // Ensure sync IPC subscribers get a fresh snapshot after a mutation.
        if !effects.is_empty() && !effects.iter().any(|e| matches!(e, Effect::PublishIpcState)) {
            effects.push(Effect::PublishIpcState);
        }
        // Centralized safety net: resolve any `pending_focus` whose overlay owner
        // is no longer presented (per #8c) right before the debug-only invariant
        // check, so no transition can leave a transient #8c violation.
        if let Some(w) =
            crate::core::commands::reconcile_pending_focus_after_transition(&mut self.state)
        {
            // Match only `Some`: a command that already emitted
            // `FocusWindow(None)` (e.g. `ViewWorkspace` with no focus target)
            // must NOT suppress the reconciled `Some(w)` — otherwise the
            // logical focus the safety net just installed never reaches X.
            if !effects
                .iter()
                .any(|e| matches!(e, Effect::FocusWindow(Some(_))))
            {
                effects.push(Effect::FocusWindow(Some(w)));
            }
        }
        #[cfg(debug_assertions)]
        self.state.assert_invariants();
        effects
    }

    /// Execute a batch of commands as ONE transaction. This is the answer to
    /// "macro publishes 50 times": N commands here coalesce into a single
    /// state publish, no matter how many mutate state or fire events.
    ///
    /// Safety net: same `pending_focus` reconciliation as `execute` — after all
    /// commands have run and events have been published, any orphaned
    /// `pending_focus` (owner no longer presented) is resolved and a
    /// `FocusWindow` appended if needed, before `assert_invariants`.
    pub fn execute_batch(
        &mut self,
        commands: impl IntoIterator<Item = Box<dyn Command>>,
    ) -> Vec<Effect> {
        let mut all = Vec::new();
        let mut dirty = false;
        let mut events = Vec::new();
        for mut cmd in commands {
            let report = cmd.execute(&mut self.state, &mut self.cfg);
            if let Some(event) = report.event {
                dirty = true;
                events.push(event);
            }
            if !report.effects.is_empty() {
                dirty = true;
                all.extend(report.effects);
            }
        }
        // Publish domain events after all commands ran, so observers see a
        // coherent final state rather than intermediate snapshots.
        for ev in &events {
            self.bus.publish(ev);
        }
        if dirty && !all.iter().any(|e| matches!(e, Effect::PublishIpcState)) {
            all.push(Effect::PublishIpcState);
        }
        // Centralized safety net: resolve any `pending_focus` whose overlay owner
        // is no longer presented (per #8c) right before the debug-only invariant
        // check, so no transition can leave a transient #8c violation.
        if let Some(w) =
            crate::core::commands::reconcile_pending_focus_after_transition(&mut self.state)
        {
            // Same `Some`-only rule as `execute` (see above).
            if !all
                .iter()
                .any(|e| matches!(e, Effect::FocusWindow(Some(_))))
            {
                all.push(Effect::FocusWindow(Some(w)));
            }
        }
        #[cfg(debug_assertions)]
        self.state.assert_invariants();
        all
    }

    /// Canonical wire adapter: converts the serializable `Action` vocabulary
    /// (keymap, `maverickctl dispatch`, TOML) into typed commands. This is the
    /// single place that maps a wire action to a command — there is no second
    /// imperative path. Domain logic lives in the commands; the adapter only
    /// resolves the focused window when an action needs one.
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::SetLayout(lk) => self.execute(SetLayout(lk)),
            Action::FocusDir(dir) => self.execute(FocusDirection(dir)),
            Action::MoveDir(dir) => match self
                .state
                .monitors
                .get(self.state.sel_mon)
                .and_then(|m| m.focused)
            {
                Some(w) => self.execute(MoveWindow(w, dir)),
                None => vec![],
            },
            Action::View(ws_idx) => self.execute(ViewWorkspace(ws_idx)),
            Action::MoveToWs(ws_idx) => self.execute(MoveToWorkspace(ws_idx)),
            Action::GrowCol(px) => self.execute(GrowColumn(px)),
            Action::NewColumn => self.execute(NewColumn),
            Action::CollapseColumn => self.execute(CollapseColumn),
            Action::FocusMon(dir) => self.execute(FocusMonitor(dir)),
            Action::MoveMon(dir) => {
                let mi = self.state.sel_mon;
                if mi >= self.state.monitors.len() {
                    return vec![];
                }
                let win = match self.state.monitors.get(mi).and_then(|m| m.focused) {
                    Some(w) => w,
                    None => return vec![],
                };
                self.execute(MoveWindowToMonitor(win, dir))
            }
            Action::Kill => {
                let mi = self.state.sel_mon;
                if let Some(w) = self.state.monitors.get(mi).and_then(|m| m.focused) {
                    self.execute(KillWindow(w))
                } else {
                    vec![]
                }
            }
            Action::Spawn(cmd) => self.execute(Spawn(cmd)),
            Action::Quit => self.execute(Quit),
            Action::Restart => self.execute(Restart),
            Action::ToggleFloat => self.execute(ToggleFloat),
            Action::ToggleFullscreen => {
                let mi = self.state.sel_mon;
                if let Some(win) = self.state.monitors.get(mi).and_then(|m| m.focused) {
                    self.execute(ToggleFullscreen(Some(win)))
                } else {
                    vec![]
                }
            }
            Action::ToggleMaximize => {
                let mi = self.state.sel_mon;
                if let Some(win) = self.state.monitors.get(mi).and_then(|m| m.focused) {
                    self.execute(ToggleMaximize(Some(win)))
                } else {
                    vec![]
                }
            }
            Action::ToggleOverview => self.execute(ToggleOverview),
            Action::OverviewNav(dir) => self.execute(OverviewNav(dir)),
            Action::OverviewEnter => self.execute(OverviewEnter),
            Action::ViewportZoom(delta) => self.execute(ViewportZoom(delta)),
            Action::PageSnap(dir) => self.execute(PageSnap(dir)),
            Action::Wallpaper(cmd) => self.execute(SetWallpaper(cmd)),
        }
    }
}
