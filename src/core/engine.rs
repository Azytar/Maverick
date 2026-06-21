use crate::types::*;
use crate::core::events::AppEvent;
use crate::core::commands::Command;
use crate::config::Cfg;

pub struct Engine {
    pub state: State,
    pub cfg:   Cfg,
}

impl Engine {
    pub fn new(cfg: Cfg) -> Self {
        Self { state: State::new(), cfg }
    }

    pub fn process_event(&mut self, event: AppEvent) -> Vec<Command> {
        let mut cmds = Vec::new();
        match event {
            AppEvent::WindowCreated(win) => {
                let mi = self.state.sel_mon;
                let ws_i = self.state.monitors[mi].active_ws;
                let workarea_w = self.state.monitors[mi].workarea.w;

                // Add to workspace column structure first
                let mut ws = self.state.monitors[mi].workspaces[ws_i].clone();
                ws = ws.add_tiled(win, self.cfg.default_col_w, workarea_w);
                self.state.monitors[mi].workspaces[ws_i] = ws;

                // Register in clients map — layout::arrange looks windows up here.
                // Without this, arrange iterates ws.columns, finds win, does
                // state.clients.get(&win) → None → skips it → zero MoveResize cmds.
                let mut client = Client::new(win, mi, ws_i);
                client.border_w = self.cfg.border_w;
                self.state.add_client(client);

                self.arrange(mi, &mut cmds);
            }

            // Pure-state actions that don't require an X11 connection.
            // Used by unit tests and by the core/backend separation boundary.
            AppEvent::ActionTriggered(action) => {
                match action {
                    Action::ToggleBar => {
                        let mi = self.state.sel_mon;
                        if mi < self.state.monitors.len() {
                            self.state.monitors[mi].show_bar ^= true;
                            cmds.push(Command::UpdateBar(mi));
                        }
                    }
                    Action::CycleLayout => {
                        self.state.layout = match self.state.layout {
                            LayoutKind::Column  => LayoutKind::Monocle,
                            LayoutKind::Monocle => LayoutKind::Grid,
                            LayoutKind::Grid    => LayoutKind::Column,
                        };
                        for mi in 0..self.state.monitors.len() {
                            self.arrange(mi, &mut cmds);
                        }
                    }
                    Action::SetLayout(lk) => {
                        self.state.layout = lk;
                        for mi in 0..self.state.monitors.len() {
                            self.arrange(mi, &mut cmds);
                        }
                    }
                    _ => {}
                }
            }

            _ => {}
        }
        cmds
    }

    pub fn arrange(&mut self, mi: usize, cmds: &mut Vec<Command>) {
        let placements = crate::core::layout::arrange(&self.state, mi, &self.cfg);
        for (win, rect, bw) in placements {
            cmds.push(Command::MoveResize { win, geom: rect, border_w: bw });
        }
    }
}
