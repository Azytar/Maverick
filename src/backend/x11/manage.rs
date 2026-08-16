use super::*;
use crate::core::layout::fs_ctx;

// ── input-trace instrumentation (feature `input-trace`) ───────────────────────
#[cfg(feature = "input-trace")]
#[allow(unused_macros)]
macro_rules! itrace {
    ($($arg:tt)*) => {{
        eprintln!("[INPUT-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "input-trace"))]
#[allow(unused_macros)]
macro_rules! itrace {
    ($($arg:tt)*) => {{}};
}

// ── window-trace instrumentation (feature `window-trace`) ─────────────────────
#[cfg(feature = "window-trace")]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{
        eprintln!("[WINDOW-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "window-trace"))]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{}};
}

impl WindowManager {
    pub(super) fn scan_windows(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let tree = match self.conn.query_tree(self.root)?.reply() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to query window tree: {}", e);
                return Ok(());
            }
        };

        // P13: Pipeline window attributes — fire all requests, then collect replies.
        let mut cookies: Vec<(Window, _)> = Vec::with_capacity(tree.children.len());
        for &w in &tree.children {
            if let Ok(c) = self.conn.get_window_attributes(w) {
                cookies.push((w, c));
            }
        }
        let mut wins = Vec::with_capacity(cookies.len());
        for (w, c) in cookies {
            if let Ok(a) = c.reply() {
                wins.push((w, a));
            }
        }

        for (w, a) in wins {
            if !a.override_redirect && a.map_state == MapState::VIEWABLE {
                if let Err(e) = self.manage(w, &a) {
                    log::warn!("Failed to manage window {}: {}", w, e);
                }
            }
        }
        Ok(())
    }

    pub(super) fn manage(
        &mut self,
        win: Window,
        attrs: &GetWindowAttributesReply,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if attrs.override_redirect {
            let _ = self.conn.map_window(win);
            return Ok(());
        }
        if self.engine.state.clients.contains_key(&win) {
            return Ok(());
        }

        let geom_r = match self.conn.get_geometry(win)?.reply() {
            Ok(g) => g,
            Err(e) => {
                log::warn!("Failed to get geometry for window {}: {}", win, e);
                return Ok(());
            }
        };
        let geom = Rect::new(
            geom_r.x as i32,
            geom_r.y as i32,
            geom_r.width as u32,
            geom_r.height as u32,
        );

        let mon_idx = self.engine.state.sel_mon;
        let ws_idx = self.engine.state.monitors[mon_idx].active_ws;

        let mut client = Client::new(win, mon_idx, ws_idx);
        client.geom = geom;
        client.saved_geom = geom;
        client.border_w = self.engine.cfg.border_w;

        let mut unmanaged = false;

        // P1: Pipeline all property reads — fire all requests before any reply.
        // Scoped so Cookies (which borrow self.conn) are dropped before &mut self calls below.
        {
            let c_title_net = self.conn.get_property(
                false,
                win,
                self.atoms.net_wm_name,
                self.atoms.utf8_string,
                0,
                256,
            )?;
            let c_title_wm =
                self.conn
                    .get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)?;
            let c_class =
                self.conn
                    .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?;
            let c_wtype = self.conn.get_property(
                false,
                win,
                self.atoms.net_wm_window_type,
                AtomEnum::ATOM,
                0,
                32,
            )?;
            let c_wstate = self.conn.get_property(
                false,
                win,
                self.atoms.net_wm_state,
                AtomEnum::ATOM,
                0,
                32,
            )?;
            let c_hints = self.conn.get_property(
                false,
                win,
                self.atoms.wm_hints,
                self.atoms.wm_hints,
                0,
                9,
            )?;
            let c_size = self.conn.get_property(
                false,
                win,
                AtomEnum::WM_NORMAL_HINTS,
                AtomEnum::WM_SIZE_HINTS,
                0,
                18,
            )?;

            // Process title (net_wm_name with WM_NAME fallback)
            if let Ok(ref prop) = c_title_net.reply() {
                if !prop.value.is_empty() {
                    client.name = String::from_utf8_lossy(&prop.value).into_owned();
                }
            }
            if client.name.is_empty() {
                if let Ok(ref prop) = c_title_wm.reply() {
                    client.name = String::from_utf8_lossy(&prop.value).into_owned();
                }
            }

            // Process class
            if let Ok(ref prop) = c_class.reply() {
                let s = String::from_utf8_lossy(&prop.value);
                let mut parts = s.split('\0');
                client.instance = parts.next().unwrap_or("").to_string();
                client.class = parts.next().unwrap_or("").to_string();
            }

            // Process window type
            if let Ok(ref prop) = c_wtype.reply() {
                if prop.type_ == u32::from(AtomEnum::ATOM) {
                    let atoms: Vec<u32> = prop
                        .value32()
                        .map(std::iter::Iterator::collect)
                        .unwrap_or_default();
                    for a in atoms {
                        if let Some(n) = self.window_type_name(a) {
                            client.window_types.push(n.to_string());
                        }
                        if a == self.atoms.net_wm_window_type_desktop
                            || a == self.atoms.net_wm_window_type_dock
                        {
                            client.is_unmanaged = true;
                        }
                        if a == self.atoms.net_wm_window_type_dialog
                            || a == self.atoms.net_wm_window_type_utility
                            || a == self.atoms.net_wm_window_type_menu
                            || a == self.atoms.net_wm_window_type_toolbar
                            || a == self.atoms.net_wm_window_type_splash
                        {
                            client.flags.set(WinFlags::FLOAT);
                        }
                    }
                }
            }

            if client.is_unmanaged {
                if let Err(e) = self.conn.map_window(win) {
                    log::warn!("failed to map window {}: {}", win, e);
                }
                unmanaged = true;
            }

            // Process window state
            if let Ok(ref sp) = c_wstate.reply() {
                if sp.type_ == u32::from(AtomEnum::ATOM) {
                    let atoms: Vec<u32> = sp
                        .value32()
                        .map(std::iter::Iterator::collect)
                        .unwrap_or_default();
                    for a in atoms {
                        if a == self.atoms.net_wm_state_fullscreen {
                            client.flags.set(WinFlags::FULLSCREEN);
                        }
                        if a == self.atoms.net_wm_state_maximized_vert {
                            client.flags.set(WinFlags::MAXIMIZED_V);
                        }
                        if a == self.atoms.net_wm_state_maximized_horiz {
                            client.flags.set(WinFlags::MAXIMIZED_H);
                        }
                        if a == self.atoms.net_wm_state_modal {
                            client.flags.set(WinFlags::FLOAT);
                        }
                    }
                }
            }

            // Process WM hints
            if let Ok(ref prop) = c_hints.reply() {
                if let Some(vals) = prop.value32() {
                    let v: Vec<u32> = vals.collect();
                    if !v.is_empty() {
                        if v[0] & 1 != 0 && v.len() > 1 {
                            if v[1] == 0 {
                                client.flags.set(WinFlags::NO_FOCUS);
                                client.wants_input = false;
                            } else {
                                client.wants_input = true;
                            }
                        }
                        if v[0] & 256 != 0 {
                            client.flags.set(WinFlags::URGENT);
                        }
                    }
                }
            }

            // Process size hints
            if let Ok(ref prop) = c_size.reply() {
                if let Some(vals) = prop.value32() {
                    let v: Vec<u32> = vals.collect();
                    if v.len() >= 18 {
                        let f = v[0];
                        let h = &mut client.hints;
                        if f & 16 != 0 {
                            h.min_w = v[9] as i32;
                            h.min_h = v[10] as i32;
                        }
                        if f & 32 != 0 {
                            h.max_w = v[11] as i32;
                            h.max_h = v[12] as i32;
                        }
                        if f & 64 != 0 {
                            h.inc_w = v[13] as i32;
                            h.inc_h = v[14] as i32;
                        }
                        if f & 128 != 0 {
                            let denom = v[16].max(1);
                            h.min_aspect = v[15] as f32 / denom as f32;
                            h.max_aspect = v[17] as f32 / denom as f32;
                        }
                        if f & 256 != 0 {
                            h.base_w = v[7] as i32;
                            h.base_h = v[8] as i32;
                        }
                        h.valid = true;
                        if h.max_w > 0 && h.max_h > 0 && h.max_w == h.min_w && h.max_h == h.min_h {
                            client.flags.set(WinFlags::FIXED);
                            client.flags.set(WinFlags::FLOAT);
                        }
                    }
                }
            }
        } // cookies dropped here

        if unmanaged {
            // Docks reserve screen space via _NET_WM_STRUT[_PARTIAL].
            self.apply_dock_strut(win)?;
            return Ok(());
        }

        // transient → inherit parent workspace, and remember the parent's real
        // (stored) geometry so we can center on it below — never on whatever X
        // reports for the parent's *live* position, which can be its off-screen
        // hidden coordinates if a workspace switch is racing this manage() call
        // (see hide_offscreen() in render.rs).
        let mut transient_parent_geom: Option<Rect> = None;
        if let Some(parent) = self.transient_for(win)? {
            // Always record the intended parent, even if it isn't managed yet —
            // a popup (KakaoTalk / Telegram / file pickers) can map *before* its
            // owner. We relink the child once the parent appears
            // (`relink_pending_transients`), inheriting the right
            // monitor/workspace instead of stranding it on the focused one.
            client.transient_parent = Some(parent);
            if let Some(pc) = self.engine.state.clients.get(&parent) {
                client.workspace = pc.workspace;
                client.monitor = pc.monitor;
                client.flags.set(WinFlags::FLOAT);
                transient_parent_geom = Some(pc.geom);
            } else {
                // Parent not managed yet. Defer, but still treat the child as a
                // float — a transient popup is always a float, and we don't want
                // it inserted into a ribbon column in the meantime.
                client.flags.set(WinFlags::FLOAT);
                self.engine.state.pending_transients.push(win);
            }
        }

        // Restore float/geometry persisted by a previous Maverick instance
        // (a `--replace` or in-place restart). Applies only when the atom says
        // the window was floating — tiled windows are re-tiled by our engine.
        let restored_geom = {
            let (was_float, geom) = self.read_float_prefs(win);
            if was_float {
                client.flags.set(WinFlags::FLOAT);
            }
            geom
        };

        self.apply_rules(&mut client);
        self.detect_portal(&mut client);

        // Apply per-rule opacity, if any. _NET_WM_WINDOW_OPACITY is a 32-bit
        // cardinal in the range 0 (transparent) – 0xFFFFFFFF (opaque). A
        // compositor (picom, etc.) reads this property; without one it's a
        // no-op, which is why we never reject a rule that sets it.
        if let Some(op) = client.opacity {
            let alpha = (op.clamp(0.0, 1.0) * u32::MAX as f32) as u32;
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                win,
                self.atoms.net_wm_window_opacity,
                AtomEnum::CARDINAL,
                &[alpha],
            );
        }

        // Center floating windows ourselves rather than trusting the raw X
        // geometry captured above (`geom`/`geom_r`): toolkits position dialogs
        // relative to their parent's *current on-screen* placement, which is
        // wrong if the parent happens to be hidden off-screen mid-switch, and
        // portal-spawned pickers (no real WM_TRANSIENT_FOR) get no placement
        // help from X at all. Width/height from the original request are kept
        // — only position is recomputed.
        if client.is_float() {
            let target = if let Some(pg) = transient_parent_geom {
                Rect::new(
                    pg.x + (pg.w as i32 - client.geom.w as i32) / 2,
                    pg.y + (pg.h as i32 - client.geom.h as i32) / 2,
                    client.geom.w,
                    client.geom.h,
                )
            } else if client.monitor < self.engine.state.monitors.len() {
                let wa = self.engine.state.monitors[client.monitor].workarea;
                Rect::new(
                    wa.x + (wa.w as i32 - client.geom.w as i32) / 2,
                    wa.y + (wa.h as i32 - client.geom.h as i32) / 2,
                    client.geom.w,
                    client.geom.h,
                )
            } else {
                client.geom
            };
            if client.monitor < self.engine.state.monitors.len() {
                let wa = self.engine.state.monitors[client.monitor].workarea;
                let cx = target
                    .x
                    .clamp(wa.x, (wa.x + wa.w as i32 - target.w as i32).max(wa.x));
                let cy = target
                    .y
                    .clamp(wa.y, (wa.y + wa.h as i32 - target.h as i32).max(wa.y));
                client.geom = Rect::new(cx, cy, target.w, target.h);
            } else {
                client.geom = target;
            }
            client.saved_geom = client.geom;
        }

        // Rule geometry (forced size/position) overrides the auto-centering.
        self.apply_rule_geometry(&mut client);

        // A restored (persisted) geometry wins over every heuristic — it is
        // the user's explicit floating position from before the restart.
        if let Some(g) = restored_geom {
            client.geom = g;
            client.saved_geom = g;
        }

        // configure border
        let _ = self.conn.configure_window(
            win,
            &ConfigureWindowAux::new().border_width(client.border_w),
        );
        let _ = self.conn.change_window_attributes(
            win,
            &ChangeWindowAttributesAux::new()
                .border_pixel(self.engine.cfg.col_normal)
                .event_mask(
                    EventMask::ENTER_WINDOW
                        | EventMask::FOCUS_CHANGE
                        | EventMask::PROPERTY_CHANGE
                        | EventMask::STRUCTURE_NOTIFY,
                ),
        );

        self.grab_buttons(win, false)?;

        let bw = client.border_w;
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            win,
            self.atoms.net_frame_extents,
            AtomEnum::CARDINAL,
            &[bw, bw, bw, bw],
        );
        let _ = self.set_wm_state(win, 1);

        // place into workspace structure
        let ws_i = client.workspace;
        let mon_i = client.monitor;
        let is_fl = client.is_float();

        self.engine.state.add_client(client);

        if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
            if is_fl {
                self.engine.state.monitors[mon_i].workspaces[ws_i]
                    .floats
                    .push(win);
                self.stack_dirty = true;
            } else {
                // Capture emptiness BEFORE adding so we know whether to snap.
                let was_empty = self.engine.state.monitors[mon_i].workspaces[ws_i].is_empty();
                self.engine.state.monitors[mon_i].workspaces[ws_i]
                    .add_tiled(win, self.engine.cfg.column_width);
                let wa = self.engine.state.monitors[mon_i].workarea;
                let fs = fs_ctx(
                    &self.engine.state.clients,
                    &self.engine.state.monitors[mon_i].workspaces[ws_i],
                    self.engine.state.monitors[mon_i].screen,
                );
                let scroll = ideal_scroll(
                    &self.engine.state.monitors[mon_i].workspaces[ws_i],
                    &self.engine.cfg,
                    wa,
                    fs,
                );
                let cam = &mut self.engine.state.monitors[mon_i].workspaces[ws_i].camera;
                if was_empty {
                    cam.snap(scroll)
                } else {
                    cam.target = scroll
                }
            }
        }

        self.client_list_dirty = true;

        // Persist float status + geometry so a restart leaves exactly this.
        self.sync_window_prefs(win);

        // Announce the new client on the typed EventBus (the sink narrates it
        // to `subscribe` clients). `Window` and `WindowId` share the u32 id
        // space, so no conversion is needed at this edge.
        self.engine
            .notify(crate::core::event::Event::WindowMapped(win));

        // Inform EWMH-aware taskbars (polybar, eww, etc.) which desktop this window is on.
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            win,
            self.atoms.net_wm_desktop,
            AtomEnum::CARDINAL,
            &[ws_i as u32],
        );

        let _ = self.conn.map_window(win);

        // The camera is already positioned just above (the `was_empty ? snap :
        // target` branch keeps the focused column visible without teleporting
        // when other windows already exist, so the open-window scroll animates
        // via the spring). A second unconditional `snap` here would kill that
        // animation, so arrange directly (bug C5).
        self.arrange(mon_i)?;

        // Presentation-aware focus policy (EWMH focus stealing): a new window
        // must never yank input away from a live fullscreen/maximized overlay.
        // The *decision* is pure and lives in core (`decide_manage_focus`): a
        // window owned by the presented overlay (dialog) takes focus; anything
        // else is deferred behind the overlay via the global `pending_focus`
        // slot, which carries the exact monitor/workspace/owner context so a
        // deferred window is never orphaned when the overlay tears down on a
        // non-active workspace or non-selected monitor.
        let intent = crate::core::commands::decide_manage_focus(&self.engine.state, win);
        match intent {
            crate::core::commands::ManageFocusIntent::Defer {
                owner,
                monitor,
                workspace,
            } => {
                if let Some(c) = self.engine.state.clients.get_mut(&win) {
                    c.flags.set(WinFlags::URGENT);
                }
                self.write_net_wm_state(win);
                // Keep `mon.focused` pointing at the overlay so logical == real.
                // Record the deferral in the global slot with full context.
                #[cfg(feature = "input-trace")]
                itrace!(
                    "manage() -> DEFER win={:#x} behind overlay {:#x} (mon={}, ws={}); X input focus stays on overlay",
                    win, owner, monitor, workspace
                );
                self.engine.state.pending_focus = Some(crate::types::PendingFocus {
                    window: win,
                    owner,
                    monitor,
                    workspace,
                });
                return Ok(());
            }
            crate::core::commands::ManageFocusIntent::Focus(_) => {
                // Unmanaged-by-overlay window: focus it through the sole X sink.
                self.focus(Some(win))?;
                #[cfg(feature = "window-trace")]
                wtrace!(
                    "manage() -> FOCUS win={:#x} mon={} ws={} (transient_parent={:?})",
                    win,
                    self.engine.state.clients.get(&win).map_or(0, |c| c.monitor),
                    self.engine
                        .state
                        .clients
                        .get(&win)
                        .map_or(0, |c| c.workspace),
                    self.engine
                        .state
                        .clients
                        .get(&win)
                        .and_then(|c| c.transient_parent)
                );
                self.relink_pending_transients()?;
            }
        }

        // A popup (KakaoTalk / Telegram / file picker) may have mapped *before*
        // the window we just managed — its owner. Relink any deferred transients
        // whose parent is now present so they land on the right monitor/workspace
        // instead of stranding on whatever was focused at their map time.
        self.relink_pending_transients()?;

        Ok(())
    }

    pub(super) fn unmanage(
        &mut self,
        win: Window,
        destroyed: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 1. If already removed (e.g. double Unmap + Destroy event), exit silently.
        let client = match self.engine.state.remove_client(win) {
            Some(c) => c,
            None => return Ok(()),
        };
        // Drop the reconciler's record of this window so a remap re-emits a full
        // configure (its previous applied rect is no longer valid).
        self.applied.forget(win);

        // Announce the departure on the typed EventBus.
        self.engine
            .notify(crate::core::event::Event::WindowUnmapped(win));

        if !destroyed {
            let _ = self.conn.configure_window(
                win,
                &ConfigureWindowAux::new().border_width(client.old_border_w),
            );
            let _ = self.set_wm_state(win, 0);
            let _ = self.conn.ungrab_button(ButtonIndex::ANY, win, ModMask::ANY);
        }

        self.client_list_dirty = true;
        let mon_i = client.monitor;

        // Clear any dangling reference to the just-removed window in the
        // per-monitor stacking caches. `stack_overlay` reads these and would
        // otherwise issue a configure/raise on a dead WindowId (only a swallowed
        // BadWindow, but avoidable). The window belonged to `mon_i`, so only
        // that monitor's entries can name it.
        if self.fs_covering.get(&mon_i) == Some(&Some(win)) {
            self.fs_covering.remove(&mon_i);
        }
        if let Some(order) = self.last_stack_order.get_mut(&mon_i) {
            order.retain(|&w| w != win);
        }

        // 2. Avoid panic if the monitor no longer exists after a hotplug.
        if mon_i < self.engine.state.monitors.len() {
            let ws_i = client.workspace;
            if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
                let wa = self.engine.state.monitors[mon_i].workarea;
                let fs = fs_ctx(
                    &self.engine.state.clients,
                    &self.engine.state.monitors[mon_i].workspaces[ws_i],
                    self.engine.state.monitors[mon_i].screen,
                );
                let scroll = ideal_scroll(
                    &self.engine.state.monitors[mon_i].workspaces[ws_i],
                    &self.engine.cfg,
                    wa,
                    fs,
                );
                let now_empty = self.engine.state.monitors[mon_i].workspaces[ws_i].is_empty();
                let cam = &mut self.engine.state.monitors[mon_i].workspaces[ws_i].camera;
                if now_empty {
                    cam.snap(0.0)
                } else {
                    cam.target = scroll
                }
            }
            let _ = self.arrange(mon_i);
            // `remove_client` (above) may have already cleared the global deferral
            // slot because this window was its `owner` (the presented overlay) or
            // its `window` (the deferred target). Snapshot the slot *before* that so
            // we can still consume the deferred window deterministically when the
            // overlay owner is destroyed — that is exactly the orphan-fix path.
            let pending_snapshot = self.engine.state.pending_focus;
            if mon_i == self.engine.state.sel_mon {
                let aws = self.engine.state.monitors[mon_i].active_ws;
                // If the window just destroyed was the overlay that created the
                // deferral, focus the deferred window now (consume). This is keyed
                // on the overlay's own monitor and works regardless of whether the
                // overlay's workspace is the active one.
                let deferred = match pending_snapshot {
                    Some(pf)
                        if pf.owner == win
                            && pf.monitor == mon_i
                            && self.engine.state.clients.contains_key(&pf.window) =>
                    {
                        Some(pf.window)
                    }
                    _ => None,
                };
                if let Some(p) = deferred {
                    self.engine.state.pending_focus = None;
                    self.focus(Some(p))?;
                } else if let Some(p) = crate::core::commands::consume_pending_focus(
                    &mut self.engine.state,
                    mon_i,
                    aws,
                    Some(win),
                ) {
                    self.focus(Some(p))?;
                } else {
                    self.focus_best(mon_i)?;
                }
            } else {
                // A non-selected monitor: apply the logical focus only, never the
                // real-X sink (that runs for `sel_mon`). Route through the core
                // helper so `mon.focused`/`focus_stack` are only mutated by the
                // single logical funnel. When the destroyed window was this
                // monitor's presented overlay, consume the deferral onto it.
                let deferred = match pending_snapshot {
                    Some(pf)
                        if pf.owner == win
                            && pf.monitor == mon_i
                            && self.engine.state.clients.contains_key(&pf.window) =>
                    {
                        Some(pf.window)
                    }
                    _ => None,
                };
                if let Some(p) = deferred {
                    self.engine.state.pending_focus = None;
                    crate::core::commands::focus_logical_on(&mut self.engine.state, mon_i, p);
                } else if let Some(c) = self.engine.state.best_focus(mon_i) {
                    crate::core::commands::focus_logical_on(&mut self.engine.state, mon_i, c);
                }
            }
        }
        Ok(())
    }

    pub(super) fn apply_rules(&self, c: &mut Client) {
        // Global invariant (INV-B): a window's map-time `_NET_WM_STATE` is
        // normalized away unless it is explicitly honoured. The default is to
        // open as a normal tile — only an app that genuinely must launch
        // maximized/fullscreen opts in, either globally via
        // `[general].honor_initial_state = true` or per-window via a rule's
        // `honor_initial_state`. This is what keeps apps that "remember" their
        // last maximized/fullscreen geometry (Firefox and its forks included)
        // from opening as a fullscreen/maximized window the WM did not authorise,
        // and it removes the need for per-`WM_CLASS` rules.
        let mut honor_initial = self.engine.cfg.honor_initial_state;
        for rule in &self.engine.cfg.rules {
            if rule.matches(&c.class, &c.instance, &c.window_types, &c.name) {
                if let Some(h) = rule.honor_initial_state {
                    honor_initial = h;
                }
                if rule.float {
                    c.flags.set(WinFlags::FLOAT);
                }
                if rule.sticky {
                    // Sticky always implies floating: a sticky tile would fight
                    // the tiling geometry of every workspace.
                    c.flags.set(WinFlags::FLOAT);
                    c.flags.set(WinFlags::STICKY);
                }
                if rule.ignore_initial_state {
                    // A rule can still force the old behaviour explicitly.
                    honor_initial = false;
                }
            }
        }
        if !honor_initial {
            // Undo whatever `_NET_WM_STATE_MAXIMIZED_*`/`_FULLSCREEN`
            // manage() already set from the window's own map-time
            // request (see the property-parsing pass above, which
            // runs before apply_rules). The window falls back to a
            // normal tile like every other new client.
            c.flags.clear(WinFlags::MAXIMIZED_V);
            c.flags.clear(WinFlags::MAXIMIZED_H);
            c.flags.clear(WinFlags::FULLSCREEN);
            // `c` isn't in `state.clients` yet at this point in
            // manage() (added further down), so `write_net_wm_state`
            // — which reads flags back out of `state.clients` — can't
            // be used here. Strip the atoms directly instead, so the
            // window's own `_NET_WM_STATE` matches the tile we're
            // about to give it rather than still claiming maximized.
            let mut atoms: Vec<u32> = self
                .conn
                .get_property(
                    false,
                    c.window,
                    self.atoms.net_wm_state,
                    AtomEnum::ATOM,
                    0,
                    32,
                )
                .ok()
                .and_then(|ck| ck.reply().ok())
                .map(|r| {
                    r.value32()
                        .map(Iterator::collect::<Vec<u32>>)
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            atoms.retain(|&a| {
                a != self.atoms.net_wm_state_fullscreen
                    && a != self.atoms.net_wm_state_maximized_vert
                    && a != self.atoms.net_wm_state_maximized_horiz
            });
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                c.window,
                self.atoms.net_wm_state,
                AtomEnum::ATOM,
                &atoms,
            );
        }
    }

    /// Apply rule-driven geometry (forced size/position) to a floating client.
    /// Run *after* the auto-centering pass so a rule position wins over the
    /// "center on parent/workarea" heuristic, and the size is clamped into the
    /// monitor's workarea instead of being trusted blindly.
    pub(super) fn apply_rule_geometry(&self, c: &mut Client) {
        if !c.is_float() {
            return;
        }
        for rule in &self.engine.cfg.rules {
            if !rule.matches(&c.class, &c.instance, &c.window_types, &c.name) {
                continue;
            }
            let wa = self
                .engine
                .state
                .monitors
                .get(c.monitor)
                .map_or(Rect::new(0, 0, 800, 600), |m| m.workarea);
            if let Some((w, h)) = rule.size {
                c.geom.w = w;
                c.geom.h = h;
            }
            if let Some((x, y)) = rule.position {
                c.geom.x = wa.x + x;
                c.geom.y = wa.y + y;
            }
            // Clamp fully inside the workarea (allow full-workarea sizes).
            let max_x = (wa.x + wa.w as i32 - c.geom.w as i32).max(wa.x);
            let max_y = (wa.y + wa.h as i32 - c.geom.h as i32).max(wa.y);
            c.geom.x = c.geom.x.clamp(wa.x, max_x);
            c.geom.y = c.geom.y.clamp(wa.y, max_y);
            c.geom.w = c.geom.w.min(wa.w);
            c.geom.h = c.geom.h.min(wa.h);
            c.saved_geom = c.geom;
        }
    }

    /// Map a `_NET_WM_WINDOW_TYPE` atom to its lowercase name (for window
    /// rules), or `None` for the undocumented types we don't advertise.
    pub(super) fn window_type_name(&self, atom: u32) -> Option<&'static str> {
        let a = &self.atoms;
        if atom == a.net_wm_window_type_desktop {
            Some("desktop")
        } else if atom == a.net_wm_window_type_dock {
            Some("dock")
        } else if atom == a.net_wm_window_type_toolbar {
            Some("toolbar")
        } else if atom == a.net_wm_window_type_menu {
            Some("menu")
        } else if atom == a.net_wm_window_type_utility {
            Some("utility")
        } else if atom == a.net_wm_window_type_splash {
            Some("splash")
        } else if atom == a.net_wm_window_type_dialog {
            Some("dialog")
        } else {
            None
        }
    }

    /// Read Maverick's private float-persistence atoms. Returns `(was_float,
    /// restored_geometry)`. `was_float` is 0/absent when the window was tiled.
    fn read_float_prefs(&self, win: Window) -> (bool, Option<Rect>) {
        let was_float = self
            .conn
            .get_property(
                false,
                win,
                self.atoms.maverick_float,
                AtomEnum::CARDINAL,
                0,
                1,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|p| p.value32().map(|mut v| v.next().unwrap_or(0) == 1))
            .unwrap_or(false);
        if !was_float {
            return (false, None);
        }
        let geom = self
            .conn
            .get_property(
                false,
                win,
                self.atoms.maverick_geom,
                AtomEnum::CARDINAL,
                0,
                4,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|p| p.value32().map(std::iter::Iterator::collect::<Vec<u32>>))
            .filter(|v| v.len() == 4)
            .map(|v| Rect::new(v[0] as i32, v[1] as i32, v[2], v[3]));
        (true, geom)
    }

    /// Write (or clear) the private persistence atoms for `win` to mirror its
    /// current float state and floating geometry. Fire-and-forget: two
    /// property requests on manage/toggle paths.
    pub(super) fn sync_window_prefs(&self, win: Window) {
        let Some(c) = self.engine.state.clients.get(&win) else {
            return;
        };
        if c.is_float() && !c.is_unmanaged {
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                win,
                self.atoms.maverick_float,
                AtomEnum::CARDINAL,
                &[1],
            );
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                win,
                self.atoms.maverick_geom,
                AtomEnum::CARDINAL,
                &[
                    c.geom.x.max(0) as u32,
                    c.geom.y.max(0) as u32,
                    c.geom.w,
                    c.geom.h,
                ],
            );
        } else {
            let _ = self.conn.delete_property(win, self.atoms.maverick_float);
            let _ = self.conn.delete_property(win, self.atoms.maverick_geom);
        }
    }

    pub(super) fn detect_portal(&self, c: &mut Client) {
        let float_classes = [
            "xdg-desktop-portal",
            "flameshot",
            "gpick",
            "pinentry",
            "screenkey",
        ];
        let float_titles = [
            "file upload",
            "open file",
            "save file",
            "file chooser",
            "qt file dialog",
            "choose file",
            "select file",
        ];
        let cl = c.class.to_lowercase();
        let ti = c.name.to_lowercase();
        if float_classes.iter().any(|fc| cl.contains(fc))
            || float_titles.iter().any(|ft| ti.contains(ft))
        {
            c.flags.set(WinFlags::FLOAT);
            if cl.contains("flameshot") {
                c.border_w = 0;
            }
        }
    }

    pub(super) fn transient_for(
        &self,
        win: Window,
    ) -> Result<Option<Window>, Box<dyn std::error::Error>> {
        let prop = self
            .conn
            .get_property(
                false,
                win,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;
        Ok(prop
            .value32()
            .and_then(|mut v| v.next())
            .filter(|&w| w != 0 && w != self.root))
    }

    /// Re-home every deferred transient whose `WM_TRANSIENT_FOR` parent is now
    /// managed. Called at the end of `manage` (so managing the parent triggers
    /// it) and is idempotent across calls. Each relinked child inherits the
    /// parent's monitor + workspace, is re-floated, re-centered on the parent,
    /// and re-painted; children still waiting stay in the queue.
    fn relink_pending_transients(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let pending = std::mem::take(&mut self.engine.state.pending_transients);
        let mut still_waiting = Vec::new();
        let mut touched_mons = std::collections::HashSet::new();
        for child in pending {
            let parent = match self.engine.state.clients.get(&child) {
                Some(c) => match c.transient_parent {
                    Some(p) => p,
                    None => continue,
                },
                None => continue,
            };
            let Some(pc) = self.engine.state.clients.get(&parent) else {
                // Parent still not here — keep waiting.
                still_waiting.push(child);
                continue;
            };
            let (pmon, pws, pgeom) = (pc.monitor, pc.workspace, pc.geom);
            if let Some(c) = self.engine.state.clients.get_mut(&child) {
                let from = c.monitor;
                // Detach from the monitor/workspace where it wrongly landed.
                if from < self.engine.state.monitors.len()
                    && c.workspace < self.engine.state.monitors[from].workspaces.len()
                {
                    let ws = &mut self.engine.state.monitors[from].workspaces[c.workspace];
                    ws.floats.retain(|&w| w != child);
                    self.engine.state.monitors[from]
                        .focus_stack
                        .retain(|&w| w != child);
                }
                c.monitor = pmon;
                c.workspace = pws;
                c.flags.set(WinFlags::FLOAT);
                // Center on the parent's *stored* geometry (see the transient
                // note in `manage`): the parent may currently be hidden off-
                // screen during a workspace switch, and we never read its live
                // X position for exactly that reason.
                let cg = c.geom;
                c.geom = Rect::new(
                    pgeom.x + ((pgeom.w as i32).saturating_sub(cg.w as i32)) / 2,
                    pgeom.y + ((pgeom.h as i32).saturating_sub(cg.h as i32)) / 2,
                    cg.w,
                    cg.h,
                );
                c.saved_geom = c.geom;
                c.geometry_dirty = true;
                if pmon < self.engine.state.monitors.len()
                    && pws < self.engine.state.monitors[pmon].workspaces.len()
                {
                    self.engine.state.monitors[pmon].workspaces[pws]
                        .floats
                        .push(child);
                    self.engine.state.monitors[pmon].focus_stack.push(child);
                }
                touched_mons.insert(from);
                touched_mons.insert(pmon);
            }
        }
        self.engine.state.pending_transients = still_waiting;
        for m in touched_mons {
            if m < self.engine.state.monitors.len() {
                self.arrange(m)?;
            }
        }
        Ok(())
    }

    pub(super) fn refresh_title(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        // Inline read without cloning the entire Client
        let name = read_title_value(&self.conn, win, &self.atoms)?;
        if let Some(name) = name {
            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                c.name = name;
            }
        }
        Ok(())
    }

    pub(super) fn refresh_hints(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let hints = read_wm_hints_value(&self.conn, win)?;
        if let Some((no_focus, wants_input, urgent)) = hints {
            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                if no_focus {
                    c.flags.set(WinFlags::NO_FOCUS);
                }
                c.wants_input = wants_input;
                if urgent {
                    c.flags.set(WinFlags::URGENT);
                }
            }
        }
        Ok(())
    }

    pub(super) fn find_client(&self, mut win: Window) -> Option<Window> {
        if self.engine.state.clients.contains_key(&win) {
            return Some(win);
        }
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(win) {
                return None;
            }
            let tree = self.conn.query_tree(win).ok()?.reply().ok()?;
            let parent = tree.parent;
            if parent == self.root || parent == win || parent == x11rb::NONE {
                return None;
            }
            win = parent;
            if self.engine.state.clients.contains_key(&win) {
                return Some(win);
            }
        }
    }

    pub(super) fn set_fullscreen(
        &mut self,
        win: Window,
        fs: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // X11-only: reflect the core's desired fullscreen state in the EWMH hint
        // and the compositor bypass hint. ALL logical state — the `FULLSCREEN`
        // flag, border width, `fs_snapshot` and the scroll `camera` — is now
        // owned by the `ToggleFullscreen` Command, so this handler must not
        // mutate `State` at all. The Command has already set the flag (so
        // `write_net_wm_state` below reads it correctly), snapshotted/restored
        // the geometry, retargeted the camera and emitted `ArrangeMonitor`.
        if !self.engine.state.clients.contains_key(&win) {
            return Ok(());
        }
        // `_NET_WM_BYPASS_COMPOSITOR` = 2 tells an external compositor (picom,
        // etc.) to skip its effect pass while the window is fullscreen, resuming
        // the moment it leaves. Without it a fullscreen video/game still gets
        // redirected + per-frame shadow work in picom → input lag and frame drops.
        if fs {
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                win,
                self.atoms.net_wm_bypass_compositor,
                AtomEnum::CARDINAL,
                &[2],
            );
        } else {
            let _ = self
                .conn
                .delete_property(win, self.atoms.net_wm_bypass_compositor);
        }
        // `write_net_wm_state` reads the `FULLSCREEN` flag (set by the Command)
        // and pushes the matching `_NET_WM_STATE_FULLSCREEN` atom.
        self.write_net_wm_state(win);
        Ok(())
    }

    /// Toggle the maximized presentation state of `win`, per axis.
    ///
    /// `vert`/`horiz` are `None` for "leave this axis alone" — EWMH clients
    /// routinely request only one of `_NET_WM_STATE_MAXIMIZED_VERT` /
    /// `_..._HORZ`, and folding both into a single boolean silently promoted a
    /// vertical maximize into a full one. Mirrors `set_fullscreen`, but a
    /// maximized window fills the monitor's `workarea` (bar/docks respected) on
    /// the requested axes and keeps its border — the geometry itself is
    /// produced by `core::present`, so here we only manage flags + EWMH + raise.
    pub(super) fn set_maximized(
        &mut self,
        win: Window,
        _vert: Option<bool>,
        _horiz: Option<bool>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.engine.state.clients.contains_key(&win) {
            return Ok(());
        }
        // Logical state (MAXIMIZED_V/H, saved_geom, geom, geometry_dirty,
        // presented_maximize) is now owned by `apply_maximize` (core). This sink
        // only reflects it on X11.
        self.write_net_wm_state(win);
        let mi = self.engine.state.clients.get(&win).map_or(0, |c| c.monitor);
        self.arrange(mi)?;
        if self
            .engine
            .state
            .monitors
            .get(mi)
            .and_then(|m| m.workspaces.get(m.active_ws))
            .and_then(|ws| ws.presented_maximize)
            == Some(win)
        {
            let _ = self
                .conn
                .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        Ok(())
    }

    /// Rewrite `_NET_WM_STATE` for `win` from the client's current flags, so
    /// every active EWMH state (fullscreen, maximized, demands-attention, …) is
    /// advertised consistently. Setting a single state must not clobber the
    /// others.
    pub(super) fn write_net_wm_state(&self, win: Window) {
        let Some(c) = self.engine.state.clients.get(&win) else {
            return;
        };
        let mut state_atoms: Vec<u32> = self
            .conn
            .get_property(false, win, self.atoms.net_wm_state, AtomEnum::ATOM, 0, 32)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| {
                r.value32()
                    .map(Iterator::collect::<Vec<u32>>)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        state_atoms.retain(|&a| {
            a != self.atoms.net_wm_state_fullscreen
                && a != self.atoms.net_wm_state_maximized_vert
                && a != self.atoms.net_wm_state_maximized_horiz
                && a != self.atoms.net_wm_state_demands_attention
        });
        if c.is_fullscreen() {
            state_atoms.push(self.atoms.net_wm_state_fullscreen);
        }
        if c.is_maximized_v() {
            state_atoms.push(self.atoms.net_wm_state_maximized_vert);
        }
        if c.is_maximized_h() {
            state_atoms.push(self.atoms.net_wm_state_maximized_horiz);
        }
        if c.flags.has(WinFlags::URGENT) {
            state_atoms.push(self.atoms.net_wm_state_demands_attention);
        }
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            win,
            self.atoms.net_wm_state,
            AtomEnum::ATOM,
            &state_atoms,
        );
    }
}
