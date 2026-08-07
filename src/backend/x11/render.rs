use super::*;
use x11rb::protocol::shape;

/// Approximate a rounded rectangle of size `w`×`h` with corner radius `r` as
/// a list of X11 `Rectangle`s: one full-width middle band, plus one 1px-tall
/// rectangle per row of each rounded corner (inset by the circle's chord at
/// that row). This is the same technique window managers have used for
/// XShape-based rounding for decades — O(r) rectangles, no external deps,
/// no compositor required. `r` is clamped so it can never exceed half of
/// either dimension.
fn rounded_rectangles(w: i32, h: i32, r: i32) -> Vec<Rectangle> {
    let r = r.clamp(0, w.min(h) / 2);
    if r <= 0 || w <= 0 || h <= 0 {
        return vec![Rectangle {
            x: 0,
            y: 0,
            width: w.max(0) as u16,
            height: h.max(0) as u16,
        }];
    }

    let mut rects = Vec::with_capacity(2 * r as usize + 1);
    rects.push(Rectangle {
        x: 0,
        y: r as i16,
        width: w as u16,
        height: (h - 2 * r).max(0) as u16,
    });

    for i in 0..r {
        // Row i (0 = outermost) sits `dy` pixels from the corner circle's
        // vertical center; the circle's horizontal chord at that row gives
        // how far to inset from the edge.
        let dy = r - i;
        let chord = ((r * r - dy * dy).max(0) as f64).sqrt() as i32;
        let inset = (r - chord).clamp(0, w / 2);
        let width = (w - 2 * inset).max(0) as u16;
        rects.push(Rectangle {
            x: inset as i16,
            y: i as i16,
            width,
            height: 1,
        });
        rects.push(Rectangle {
            x: inset as i16,
            y: (h - 1 - i) as i16,
            width,
            height: 1,
        });
    }
    rects
}

impl WindowManager {
    /// Apply (or clear) rounded corners on `win` via the Shape extension's
    /// bounding-shape mask. Only called when `corner_radius > 0` — with the
    /// default of `0` this codepath, and every X11 Shape request, never
    /// runs, so there's zero cost for users who don't opt in.
    pub(super) fn round_corners(&self, win: Window, outer_w: u32, outer_h: u32) {
        let r = self.engine.cfg.corner_radius as i32;
        let rects = rounded_rectangles(outer_w as i32, outer_h as i32, r);
        // Fire-and-forget, same rationale as apply_geom's configure_window:
        // this runs on every geometry change, a synchronous round-trip per
        // window would be unacceptable. Servers without the Shape extension
        // (essentially none — it's been near-universal since the 90s) just
        // silently ignore the request.
        let _ = shape::rectangles(
            &self.conn,
            shape::SO::SET,
            shape::SK::BOUNDING,
            ClipOrdering::UNSORTED,
            win,
            0,
            0,
            &rects,
        );
    }

    pub(super) fn arrange(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full(mon_idx, true, true)
    }

    /// P8/P11: arrange with optional `hide_offscreen` and restack.
    /// Lightweight arrange skips both — safe when only focus/row heights change.
    pub(super) fn arrange_full(
        &mut self,
        mon_idx: usize,
        do_hide: bool,
        do_restack: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() {
            return Ok(());
        }

        if do_hide && self.drag.is_none() {
            self.hide_offscreen(mon_idx)?;
        }

        // P10: reuse pre-allocated buffer
        arrange(
            &self.engine.state,
            mon_idx,
            &self.engine.cfg,
            &self.layout_registry,
            &mut self.placements_buf,
        );
        // Presentation layer: apply the fullscreen/maximized overlay.
        let mut buf = std::mem::take(&mut self.placements_buf);
        present(
            &self.engine.state,
            &self.engine.state.monitors[mon_idx],
            &mut buf,
        );
        for &(win, geom, bw) in &buf {
            self.apply_geom(win, geom, bw)?;
        }
        buf.clear();
        self.placements_buf = buf;
        // Overlay stacking: presented windows above tiles, popups of presented
        // windows above the overlay, focused window on top (or peek).
        self.stack_overlay(mon_idx);

        if do_restack && self.stack_dirty {
            self.stack_dirty = false;
            self.restack(mon_idx)?;
        }
        Ok(())
    }

    pub(super) fn hide_offscreen(
        &mut self,
        mon_idx: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = &mon.workspaces[mon.active_ws];

        // P12: reuse allocated buffers
        self.hide_ws_set.clear();
        self.hide_mon_vec.clear();
        self.hide_ws_set.extend(
            ws.columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .chain(ws.floats.iter().copied()),
        );
        // Sticky floats stay visible on every workspace of this monitor.
        self.hide_ws_set.extend(
            self.engine
                .state
                .clients
                .iter()
                .filter(|(_, c)| c.monitor == mon_idx && c.is_sticky())
                .map(|(w, _)| *w),
        );
        self.hide_mon_vec.extend(
            self.engine
                .state
                .clients
                .iter()
                .filter(|(_, c)| c.monitor == mon_idx)
                .map(|(w, _)| *w),
        );

        for win in self.hide_mon_vec.drain(..) {
            let in_ws = self.hide_ws_set.contains(&win);
            let client = match self.engine.state.clients.get_mut(&win) {
                Some(c) => c,
                None => continue,
            };
            if !in_ws && !client.wm_hidden {
                let w = client.geom.w.min(i32::MAX as u32) as i32;
                let off_x = w.saturating_add(200).saturating_neg();
                let _ = self
                    .conn
                    .configure_window(win, &ConfigureWindowAux::new().x(off_x));
                client.wm_hidden = true;
            } else if in_ws && client.wm_hidden {
                let gx = client.geom.x;
                let gy = client.geom.y;
                let _ = self
                    .conn
                    .configure_window(win, &ConfigureWindowAux::new().x(gx).y(gy));
                client.wm_hidden = false;
            }
        }
        Ok(())
    }

    pub(super) fn restack(&self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        self.stack_overlay(mon_idx);
        Ok(())
    }

    /// Unify stacking for a monitor's active workspace:
    ///
    /// 1. floats above tiled windows (base float layer);
    /// 2. the presentation overlay — every fullscreen/maximized window, in
    ///    focus order (most recently focused last → on top). Unlike the old
    ///    focus-only rule, all overlay windows stay presented while unfocused;
    /// 3. the focused window if it is *not* part of the overlay ("peek"): it
    ///    rises above the presented window so focus stays visible — but stays
    ///    below step 4, because a presented window's owned popups belong above;
    /// 4. floating popups/dialogs whose `WM_TRANSIENT_FOR` chain reaches a
    ///    presented window — always above that overlay, so a menu or file
    ///    dialog of a fullscreen app never hides behind it.
    ///
    /// Fire-and-forget `StackMode::ABOVE`: arrange/focus paths must not block.
    fn stack_overlay(&self, mon_idx: usize) {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = mon.ws();

        // 1. Base float layer.
        for &win in &ws.floats {
            if self.engine.state.clients.contains_key(&win) {
                self.raise(win);
            }
        }
        // Sticky floats ride above every workspace's tiles by definition —
        // include them in the base layer regardless of which workspace is
        // active.
        for (&win, c) in &self.engine.state.clients {
            if c.monitor == mon_idx && c.is_sticky() {
                self.raise(win);
            }
        }

        // 2. Presentation overlay, most-recently-focused last.
        let mut presented: Vec<WindowId> = ws
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .chain(ws.floats.iter().copied())
            .filter(|win| {
                self.engine
                    .state
                    .clients
                    .get(win)
                    .is_some_and(|c| c.is_fullscreen() || c.is_maximized())
            })
            .collect();
        presented.sort_by_key(|win| {
            mon.focus_stack
                .iter()
                .position(|&x| x == *win)
                .unwrap_or(0)
        });
        for &win in &presented {
            self.raise(win);
        }

        // 3. Peek: a focused plain tile is raised above the overlay so the user
        //    sees where focus sits without resizing the presented window.
        if !presented.is_empty() {
            if let Some(fw) = mon.focused {
                if !presented.contains(&fw) {
                    self.raise(fw);
                }
            }
        }

        // 4. Owned popups of the overlay: a float whose transient-parent chain
        //    reaches a presented window must sit above it.
        for &win in &ws.floats {
            if self.transient_of(win, &presented) {
                self.raise(win);
            }
        }
    }

    /// True when `win`'s `transient_parent` chain reaches any window in `roots`.
    /// Walks at most `MAX_TRANSIENT_DEPTH` links to survive popup-of-popup
    /// ownership without risking cycles.
    fn transient_of(&self, win: WindowId, roots: &[WindowId]) -> bool {
        const MAX_TRANSIENT_DEPTH: usize = 4;
        let mut cur = self
            .engine
            .state
            .clients
            .get(&win)
            .and_then(|c| c.transient_parent);
        for _ in 0..MAX_TRANSIENT_DEPTH {
            let Some(p) = cur else {
                return false;
            };
            if roots.contains(&p) {
                return true;
            }
            cur = self.engine.state.clients.get(&p).and_then(|c| c.transient_parent);
        }
        false
    }

    pub(super) fn apply_geom(
        &mut self,
        win: Window,
        geom: Rect,
        bw: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match self.engine.state.clients.get(&win) {
            Some(c) => c,
            None => return Ok(()),
        };
        if geom == client.geom && bw == client.border_w {
            return Ok(());
        }

        let _ = self.conn.configure_window(
            win,
            &ConfigureWindowAux::new()
                .x(geom.x)
                .y(geom.y)
                .width(geom.w)
                .height(geom.h)
                .border_width(bw),
        );

        let event = ConfigureNotifyEvent {
            response_type: CONFIGURE_NOTIFY_EVENT,
            sequence: 0,
            event: win,
            window: win,
            above_sibling: x11rb::NONE,
            x: geom.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            y: geom.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            width: geom.w.clamp(0, u16::MAX as u32) as u16,
            height: geom.h.clamp(0, u16::MAX as u32) as u16,
            border_width: bw.clamp(0, u16::MAX as u32) as u16,
            override_redirect: false,
        };
        // Fire-and-forget: no .check() here — this is called for every window
        // in arrange(), so a synchronous RTT per window is unacceptable.
        let _ = self
            .conn
            .send_event(false, win, EventMask::STRUCTURE_NOTIFY, event);

        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            c.geom = geom;
            c.border_w = bw;
        }

        if self.engine.cfg.corner_radius > 0 {
            self.round_corners(win, geom.w + 2 * bw, geom.h + 2 * bw);
        }

        Ok(())
    }

    /// Raise `win` above all its siblings (`TopLevel`). Fire-and-forget: called
    /// from arrange/focus paths where a synchronous RTT per window is not acceptable.
    fn raise(&self, win: Window) {
        let _ = self
            .conn
            .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    }

    pub(super) fn focus(&mut self, win: Option<Window>) -> Result<(), Box<dyn std::error::Error>> {
        let valid_win = win.filter(|w| self.engine.state.clients.contains_key(w));

        // The presentation overlay (core::present) is independent of focus, so
        // a focus change never recomputes or re-sizes geometry. The only thing
        // focus influences is stacking: a focused presented window rises above
        // the other presented ones, and a focused plain tile "peeks" above the
        // overlay (see focus() below).
        let prev_focused = self
            .engine
            .state
            .monitors
            .get(self.engine.state.sel_mon)
            .and_then(|m| m.focused);

        if let Some(w) = valid_win {
            // P6: Single lookup — extract everything we need
            let (mon_i, geom, wants, urgent) = {
                let c = match self.engine.state.clients.get(&w) {
                    Some(c) => c,
                    None => return self.focus(None),
                };
                if c.no_focus() {
                    return Ok(());
                }
                (
                    c.monitor,
                    c.geom,
                    c.wants_input,
                    c.flags.has(WinFlags::URGENT),
                )
            };

            // Guard against stale client.monitor after hotplug
            if mon_i >= self.engine.state.monitors.len() {
                return Ok(());
            }

            // unfocus previous — only if we're actually about to focus the new one
            if prev_focused != valid_win {
                if let Some(pw) = prev_focused {
                    if self.engine.state.clients.contains_key(&pw) {
                        self.unfocus(pw)?;
                    }
                }
            }

            self.engine.state.sel_mon = mon_i;

            // set X11 input focus
            if wants {
                let _ = self
                    .conn
                    .set_input_focus(InputFocus::PARENT, w, x11rb::CURRENT_TIME);
            } else {
                let _ = self
                    .conn
                    .set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME);
            }
            if self.has_protocol(w, self.atoms.wm_take_focus)? {
                self.send_proto(w, self.atoms.wm_take_focus, self.last_event_time)?;
            }

            // focused border color
            let col = if urgent {
                self.engine.cfg.col_urgent
            } else {
                self.engine.cfg.col_focused
            };
            let _ = self
                .conn
                .change_window_attributes(w, &ChangeWindowAttributesAux::new().border_pixel(col));
            self.grab_buttons(w, true)?;

            let serial = self.engine.state.next_serial();
            let was_urgent = if let Some(c) = self.engine.state.clients.get_mut(&w) {
                // Consume the urgency flag so its border color and the
                // `_NET_WM_STATE` demands-attention atom don't stick once the
                // window is actually focused.
                let was = c.flags.has(WinFlags::URGENT);
                if was {
                    c.flags.clear(WinFlags::URGENT);
                }
                c.focus_serial = serial;
                was
            } else {
                false
            };
            if was_urgent {
                self.write_net_wm_state(w);
            }

            let mon = &mut self.engine.state.monitors[mon_i];
            mon.focused = Some(w);
            mon.focus_stack.retain(|&x| x != w);
            mon.focus_stack.push(w);

            // Overlay stacking (presented / popups-of-presented / peek).
            self.stack_overlay(mon_i);

            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[w],
            );

            if self.engine.cfg.warp_cursor {
                let _ = self.conn.warp_pointer(
                    x11rb::NONE,
                    w,
                    0,
                    0,
                    0,
                    0,
                    (geom.w / 2) as i16,
                    (geom.h / 2) as i16,
                );
            }
        } else {
            // Only clear the focused window on the currently selected monitor.
            // Other monitors keep their own focused state independently.
            let sel = self.engine.state.sel_mon;
            if sel < self.engine.state.monitors.len() {
                if let Some(pw) = self.engine.state.monitors[sel].focused {
                    if self.engine.state.clients.contains_key(&pw) {
                        self.unfocus(pw)?;
                    }
                }
                self.engine.state.monitors[sel].focused = None;
            }
            let _ =
                self.conn
                    .set_input_focus(InputFocus::POINTER_ROOT, self.root, x11rb::CURRENT_TIME);
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[x11rb::NONE],
            );
        }

        // Announce the transition on the typed EventBus. Every focus move —
        // pointer clicks, button grabs, EnterNotify, manage/unmanage re-focus,
        // commands — funnels through `focus`, so this is the single choke point.
        // `Window` and `WindowId` are both `u32` aliases, so the values pass
        // straight through to the core's id space.
        if prev_focused != valid_win {
            self.engine.notify(crate::core::event::Event::FocusChanged {
                from: prev_focused,
                to: valid_win,
            });
        }

        Ok(())
    }

    pub(super) fn unfocus(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let col = self.engine.cfg.col_normal;
        let _ = self
            .conn
            .change_window_attributes(win, &ChangeWindowAttributesAux::new().border_pixel(col));
        let _ = self.grab_buttons(win, false);
        Ok(())
    }

    pub(super) fn focus_best(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let candidate = self.engine.state.best_focus(mon_idx);
        self.focus(candidate)
    }
}
