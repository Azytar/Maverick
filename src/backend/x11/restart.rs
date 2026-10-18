//! Transient X11 property used only by our own re-exec, never ordinary startup.

use super::*;
use maverick_core::types::restart::{RestartState, MAX_WORDS};

pub(crate) const HANDOFF_ENV: &str = "MAVERICK_RESTART_PID";

pub(super) struct Handoff {
    pub layout: RestartState,
    geometry_changed: bool,
}

fn geometry_settings(cfg: &Cfg) -> [u32; 5] {
    [
        cfg.gaps_inner,
        cfg.gaps_outer,
        u32::from(cfg.smart_gaps),
        cfg.border_w,
        cfg.accordion_boost.to_bits(),
    ]
}

pub(super) fn take(
    conn: &XConn,
    root: Window,
    atom: Atom,
    marker: Option<&str>,
    cfg: &Cfg,
) -> Option<Handoff> {
    let expected = std::process::id().to_string();
    let reply = conn
        .get_property(true, root, atom, AtomEnum::CARDINAL, 0, MAX_WORDS as u32)
        .ok()?
        .reply()
        .ok()?;
    let _ = conn.delete_property(root, atom);
    if marker != Some(expected.as_str()) {
        return None;
    }
    let words: Vec<u32> = reply.value32()?.collect();
    let (&pid, words) = words.split_first()?;
    if pid != std::process::id() || reply.bytes_after != 0 {
        return None;
    }
    let (geometry, words) = words.split_first_chunk::<5>()?;
    let snapshot = RestartState::decode(words);
    if snapshot.is_none() {
        log::warn!("restart: invalid layout handoff; adopting windows normally");
    }
    snapshot.map(|layout| Handoff {
        layout,
        geometry_changed: *geometry != geometry_settings(cfg),
    })
}

impl WindowManager {
    pub(super) fn save_restart(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.flush_pending()?;
        let mut words = vec![std::process::id()];
        words.extend(geometry_settings(&self.engine.cfg));
        words.extend(
            RestartState::encode(&self.engine.state)
                .ok_or("layout exceeds restart handoff limit")?,
        );
        if words.len() > MAX_WORDS {
            return Err("layout exceeds restart handoff limit".into());
        }
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.maverick_restart,
                AtomEnum::CARDINAL,
                &words,
            )?
            .check()?;
        Ok(())
    }

    pub(super) fn restore_restart(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(snapshot) = self.restart_snapshot.take() else {
            return Ok(());
        };
        let changed = snapshot
            .layout
            .apply(&mut self.engine.state, self.engine.cfg.column_width)
            || snapshot.geometry_changed;
        if changed {
            for mi in 0..self.engine.state.monitors.len() {
                self.retarget_cameras(mi);
            }
        }
        self.execute(Effect::RefreshDesktops)?;
        let current = self
            .engine
            .state
            .mon()
            .map(|mon| mon.active_index())
            .unwrap_or(0);
        self.execute(Effect::SetCurrentDesktop(current))?;
        let wins: Vec<_> = self.engine.state.clients.keys().copied().collect();
        for win in wins {
            let client = &self.engine.state.clients[&win];
            let ws = self.engine.state.monitors[client.monitor]
                .view_index(client.workspace)
                .unwrap_or(0);
            self.execute(Effect::SetWindowDesktop { win, ws })?;
            self.sync_window_prefs(win);
            self.write_net_wm_state(win);
        }
        log::info!("restart: restored logical window layout");
        Ok(())
    }
}
