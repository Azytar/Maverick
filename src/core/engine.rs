use crate::config::Cfg;
use crate::core::commands::{CollapseColumn, CycleLayout, FocusDirection, FocusMonitor, GrowColumn, KillWindow, MoveToWorkspace, MoveWindow, MoveWindowToMonitor, NewColumn, OverviewEnter, OverviewNav, PageSnap, Quit, Restart, SetLayout, Spawn, ToggleFloat, ToggleFullscreen, ToggleMaximize, ToggleOverview, ViewWorkspace, ViewportZoom, Command};
use crate::core::effect::Effect;
use crate::core::event::{Event, EventBus, EventHandler};
use crate::types::*;

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
        effects
    }

    /// Execute a batch of commands as ONE transaction. This is the answer to
    /// "macro publishes 50 times": N commands here coalesce into a single
    /// state publish, no matter how many mutate state or fire events.
    pub fn execute_batch(&mut self, commands: impl IntoIterator<Item = Box<dyn Command>>) -> Vec<Effect> {
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
        all
    }

    /// Canonical wire adapter: converts the serializable `Action` vocabulary
    /// (keymap, `maverickctl dispatch`, TOML) into typed commands. This is the
    /// single place that maps a wire action to a command — there is no second
    /// imperative path. Domain logic lives in the commands; the adapter only
    /// resolves the focused window when an action needs one.
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::CycleLayout => self.execute(CycleLayout),
            Action::SetLayout(lk) => self.execute(SetLayout(lk)),
            Action::FocusDir(dir) => self.execute(FocusDirection(dir)),
            Action::MoveDir(dir) => match self.state.monitors.get(self.state.sel_mon).and_then(|m| m.focused) {
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
        }
    }
}
