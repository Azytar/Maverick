// maverick/src/wm.rs
// Window manager core — niri-style columnar layout, real bar, clean coords.

use std::collections::BTreeMap;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use crate::backend::atoms::Atoms;
use crate::backend::bar::Bar;
use crate::config::Cfg;
use crate::core::Engine;
use crate::core::layout::{arrange, ideal_scroll};
use crate::log;
use crate::types::*;

pub static NEED_REGRAB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct WindowManager {
    conn:        RustConnection,
    screen_num:  usize,
    root:        Window,
    atoms:       Atoms,
    engine:      Engine,
    bar:         Bar,
    check_win:   Window,
    numlock:     u16,
    keymap:      BTreeMap<(u16, u32), crate::types::Action>,
    raw_keymap:  Vec<u32>,
    raw_kpk:     usize,
    raw_min:     u8,
    drag:        Option<DragState>,
    /// Bitmask of monitors whose bar needs a repaint (bit i = monitor i).
    /// Set by mark_bar(); consumed by flush_bars() once per event-loop iteration.
    bar_dirty:   u64,
}

#[derive(Debug, Clone)]
struct DragState {
    win:        Window,
    start_geom: Rect,
    ptr_x:      i32,
    ptr_y:      i32,
    resize:     bool,
}

impl WindowManager {
    pub fn new(cfg: Cfg) -> Result<Self, Box<dyn std::error::Error>> {
        let (conn, screen_num) = RustConnection::connect(None)?;
        let screen = &conn.setup().roots[screen_num];
        let root   = screen.root;
        let depth  = screen.root_depth;
        let visual = screen.root_visual;

        log::info!("maverick: X11 connected root={} {}x{}", root,
            screen.width_in_pixels, screen.height_in_pixels);

        let atoms = Atoms::new(&conn)?;
        check_no_other_wm(&conn, root)?;

        let bar = Bar::load(&conn)?;

        let monitors = detect_monitors(&conn, screen, &cfg)?;
        let mut engine = Engine::new(cfg);
        engine.state.monitors = monitors;

        // create EWMH check window
        let check_win = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT, check_win, root,
            -1, -1, 1, 1, 0,
            WindowClass::INPUT_OUTPUT, 0,
            &CreateWindowAux::new(),
        )?.check()?;

        let numlock = get_numlock(&conn)?;
        let keymap  = build_keymap(&engine.cfg);
        let (raw_keymap, raw_kpk, raw_min) = build_raw_keymap(&conn)?;

        let mut wm = WindowManager {
            conn, screen_num, root, atoms, engine, bar,
            check_win, numlock, keymap, raw_keymap, raw_kpk, raw_min,
            drag: None, bar_dirty: 0,
        };

        // Create bar windows
        for mon_idx in 0..wm.engine.state.monitors.len() {
            let (bar_h, top, scr_x, scr_w, scr_depth, scr_visual, bar_y, root) = {
                let m = &wm.engine.state.monitors[mon_idx];
                (wm.engine.cfg.bar_height, wm.engine.cfg.top_bar,
                 m.screen.x, m.screen.w,
                 depth, visual,
                 m.bar_y(), wm.root)
            };

            let bar_win = wm.conn.generate_id()?;
            wm.conn.create_window(
                scr_depth, bar_win, root,
                scr_x as i16, bar_y as i16,
                scr_w as u16, bar_h as u16,
                0, WindowClass::INPUT_OUTPUT, scr_visual,
                &CreateWindowAux::new()
                    .background_pixel(wm.engine.cfg.col_bar_bg)
                    .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS)
                    .override_redirect(1u32),
            )?.check()?;

            wm.conn.change_property32(
                PropMode::REPLACE, bar_win,
                wm.atoms.net_wm_window_type, AtomEnum::ATOM,
                &[wm.atoms.net_wm_window_type_dock],
            )?.check()?;

            let strut = if top {
                [0u32,0,bar_h,0, 0,0,0,0,
                 scr_x as u32,(scr_x+scr_w as i32) as u32, 0,0]
            } else {
                [0u32,0,0,bar_h, 0,0,0,0, 0,0,
                 scr_x as u32,(scr_x+scr_w as i32) as u32]
            };
            wm.conn.change_property32(
                PropMode::REPLACE, bar_win,
                wm.atoms.net_wm_strut_partial, AtomEnum::CARDINAL, &strut,
            )?.check()?;

            let gc = wm.conn.generate_id()?;
            wm.conn.create_gc(gc, bar_win, &CreateGCAux::new()
                .foreground(wm.engine.cfg.col_bar_fg)
                .background(wm.engine.cfg.col_bar_bg)
                .font(wm.bar.font_id),
            )?.check()?;

            wm.conn.map_window(bar_win)?.check()?;

            wm.engine.state.monitors[mon_idx].bar_win = Some(bar_win);
            wm.engine.state.monitors[mon_idx].bar_gc  = Some(gc);
        }

        wm.setup_root()?;
        wm.scan_windows()?;

        for i in 0..wm.engine.state.monitors.len() {
            wm.arrange(i)?;
            wm.draw_bar(i);
        }

        wm.conn.flush()?;
        log::info!("maverick ready");
        Ok(wm)
    }

    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        while self.engine.state.running {
            // ── flush phase ─────────────────────────────────────────────────────
            // Draw any bars that were marked dirty by the previous event batch.
            // This runs BEFORE blocking on the next event, so all X11 output
            // from both event dispatch and bar drawing is flushed in one shot.
            self.flush_bars()?;
            self.conn.flush()?;

            // ── wait phase ───────────────────────────────────────────────────────
            // Block until at least one event arrives.
            let ev = self.conn.wait_for_event()?;
            self.dispatch(ev)?;

            // ── drain phase ──────────────────────────────────────────────────────
            // Non-blocking: process every event already in the socket buffer.
            // Firefox/pavucontrol can queue 100+ PropertyNotify events in a burst;
            // draining them here means bar_dirty is set once, not 100 times.
            loop {
                match self.conn.poll_for_event()? {
                    Some(ev) => self.dispatch(ev)?,
                    None     => break,
                }
            }
            // Loop back → flush_bars() redraws bar exactly once for the whole batch.
        }
        Ok(())
    }

    fn dispatch(&mut self, ev: x11rb::protocol::Event) -> Result<(), Box<dyn std::error::Error>> {
        // Check for pending regrab request (from SIGCONT handler)
        if NEED_REGRAB.swap(false, std::sync::atomic::Ordering::SeqCst) {
            log::info!("Regrabbing keys after SIGCONT");
            if let Err(e) = self.grab_keys() {
                log::warn!("Failed to regrab keys: {}", e);
            }
        }
        match ev {
            Event::ButtonPress(e)     => self.on_button_press(e)?,
            Event::ButtonRelease(e)   => self.on_button_release(e)?,
            Event::ClientMessage(e)   => self.on_client_message(e)?,
            Event::ConfigureNotify(e) => self.on_configure_notify(e)?,
            Event::ConfigureRequest(e)=> self.on_configure_request(e)?,
            Event::DestroyNotify(e)   => self.on_destroy(e)?,
            Event::EnterNotify(e)     => self.on_enter(e)?,
            Event::Expose(e)          => self.on_expose(e)?,
            Event::FocusIn(e)         => self.on_focus_in(e)?,
            Event::KeyPress(e)        => self.on_key(e)?,
            Event::MappingNotify(e)   => self.on_mapping(e)?,
            Event::MapRequest(e)      => self.on_map_request(e)?,
            Event::MotionNotify(e)    => self.on_motion(e)?,
            Event::PropertyNotify(e)  => self.on_property(e)?,
            Event::UnmapNotify(e)     => self.on_unmap(e)?,
            _ => {}
        }
        Ok(())
    }

    // ── Setup ──────────────────────────────────────────────────────────────────

    fn setup_root(&self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        self.conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::SUBSTRUCTURE_REDIRECT
                | EventMask::SUBSTRUCTURE_NOTIFY
                | EventMask::BUTTON_PRESS
                | EventMask::POINTER_MOTION
                | EventMask::ENTER_WINDOW
                | EventMask::STRUCTURE_NOTIFY
                | EventMask::PROPERTY_CHANGE,
            ),
        )?.check()?;

        let supported = a.supported_list();
        self.conn.change_property32(PropMode::REPLACE, self.root,
            a.net_supported, AtomEnum::ATOM, &supported)?.check()?;

        // EWMH: set _NET_SUPPORTING_WM_CHECK on both root and check_win (once each)
        self.conn.change_property32(PropMode::REPLACE, self.root,
            a.net_supporting_wm_check, AtomEnum::WINDOW, &[self.check_win])?.check()?;
        self.conn.change_property32(PropMode::REPLACE, self.check_win,
            a.net_supporting_wm_check, AtomEnum::WINDOW, &[self.check_win])?.check()?;

        self.conn.change_property8(PropMode::REPLACE, self.check_win,
            a.net_wm_name, a.utf8_string, b"maverick")?.check()?;

        let n = self.engine.cfg.n_tags as u32;
        self.conn.change_property32(PropMode::REPLACE, self.root,
            a.net_number_of_desktops, AtomEnum::CARDINAL, &[n])?.check()?;
        self.conn.change_property32(PropMode::REPLACE, self.root,
            a.net_current_desktop, AtomEnum::CARDINAL, &[0u32])?.check()?;

        self.update_ewmh_desktops()?;
        self.grab_keys()?;
        Ok(())
    }

    fn grab_keys(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);

        let setup = self.conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        
        // Use u16 arithmetic to avoid u8 overflow when max - min + 1 == 256
        let count = (max as u16 - min as u16 + 1) as u8;
        let map = match self.conn.get_keyboard_mapping(min, count)?.reply() {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Failed to get keyboard mapping: {}", e);
                return Ok(());
            }
        };
        
        let kpk = map.keysyms_per_keycode as usize;
        if kpk == 0 { 
            log::warn!("No keysyms per keycode");
            return Ok(()); 
        }

        for (mask, keysym, _) in &self.engine.cfg.keybinds {
            for code in keysym_to_codes(&map, min, max, kpk, *keysym) {
                for extra in mod_variants(self.numlock) {
                    let _ = self.conn.grab_key(
                        true, self.root,
                        (mask | extra).into(),
                        code, GrabMode::ASYNC, GrabMode::ASYNC,
                    );
                }
            }
        }
        Ok(())
    }

    fn grab_buttons(&self, win: Window, focused: bool) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_button(ButtonIndex::ANY, win, ModMask::ANY);
        let motion = EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;

        if !focused {
            let _ = self.conn.grab_button(false, win, EventMask::BUTTON_PRESS,
                GrabMode::SYNC, GrabMode::SYNC, x11rb::NONE, x11rb::NONE,
                ButtonIndex::ANY, ModMask::ANY);
        }

        let sup: u16 = ModMask::M4.into();
        for extra in mod_variants(self.numlock) {
            let m = (sup | extra).into();
            for btn in [ButtonIndex::M1, ButtonIndex::M3] {
                let _ = self.conn.grab_button(false, win, motion,
                    GrabMode::ASYNC, GrabMode::SYNC, x11rb::NONE, x11rb::NONE,
                    btn, m);
            }
        }
        Ok(())
    }

    fn scan_windows(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let tree = match self.conn.query_tree(self.root)?.reply() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to query window tree: {}", e);
                return Ok(());
            }
        };
        
        let mut wins = Vec::with_capacity(tree.children.len());
        for &w in &tree.children {
            if let Ok(a) = self.conn.get_window_attributes(w)?.reply() {
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

    // ── Window management ─────────────────────────────────────────────────────

    fn manage(&mut self, win: Window, attrs: &GetWindowAttributesReply)
        -> Result<(), Box<dyn std::error::Error>>
    {
        if attrs.override_redirect {
            let _ = self.conn.map_window(win);
            return Ok(());
        }
        if self.engine.state.clients.contains_key(&win) { return Ok(()); }

        let geom_r = match self.conn.get_geometry(win)?.reply() {
            Ok(g) => g, 
            Err(e) => {
                log::warn!("Failed to get geometry for window {}: {}", win, e);
                return Ok(());
            }
        };
        let geom = Rect::new(geom_r.x as i32, geom_r.y as i32,
                             geom_r.width as u32, geom_r.height as u32);

        let mon_idx = self.engine.state.sel_mon;
        let ws_idx  = self.engine.state.monitors[mon_idx].active_ws;

        let mut client = Client::new(win, mon_idx, ws_idx);
        client.geom       = geom;
        client.saved_geom = geom;
        client.border_w   = self.engine.cfg.border_w;

        let _ = self.read_title(&mut client);
        let _ = self.read_class(&mut client);
        let _ = self.read_window_type(&mut client);

        if client.is_unmanaged {
            let _ = self.conn.map_window(win);
            return Ok(());
        }

        let _ = self.read_wm_hints(&mut client);
        let _ = self.read_size_hints(&mut client);

        // transient → inherit parent workspace
        if let Some(parent) = self.transient_for(win)? {
            if let Some(pc) = self.engine.state.clients.get(&parent) {
                client.workspace = pc.workspace;
                client.monitor   = pc.monitor;
                client.flags.set(WinFlags::FLOAT);
            }
        }

        self.apply_rules(&mut client);
        self.detect_portal(&mut client);

        // configure border
        let _ = self.conn.configure_window(win,
            &ConfigureWindowAux::new().border_width(client.border_w));
        let _ = self.conn.change_window_attributes(win,
            &ChangeWindowAttributesAux::new()
                .border_pixel(self.engine.cfg.col_normal)
                .event_mask(EventMask::ENTER_WINDOW
                    | EventMask::FOCUS_CHANGE
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::STRUCTURE_NOTIFY));

        self.grab_buttons(win, false)?;

        let bw = client.border_w;
        let _ = self.conn.change_property32(PropMode::REPLACE, win,
            self.atoms.net_frame_extents, AtomEnum::CARDINAL, &[bw,bw,bw,bw]);
        let _ = self.set_wm_state(win, 1);

        // place into workspace structure
        let ws_i   = client.workspace;
        let mon_i  = client.monitor;
        let is_fl  = client.is_float();

        self.engine.state.add_client(client);

        if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
            if is_fl {
                self.engine.state.monitors[mon_i].workspaces[ws_i].floats.push(win);
            } else {
                let dw = self.engine.cfg.default_col_w;
                let workarea_w = self.engine.state.monitors[mon_i].workarea.w;
                let mut ws = self.engine.state.monitors[mon_i].workspaces[ws_i].clone();
                ws = ws.add_tiled(win, dw, workarea_w);
                self.engine.state.monitors[mon_i].workspaces[ws_i] = ws;
            }
        }

        self.update_client_list()?;

        // Inform EWMH-aware taskbars (polybar, eww, etc.) which desktop this window is on.
        let _ = self.conn.change_property32(PropMode::REPLACE, win,
            self.atoms.net_wm_desktop, AtomEnum::CARDINAL, &[ws_i as u32]);

        let _ = self.conn.map_window(win);

        // scroll & arrange
        {
            let scroll = ideal_scroll(&self.engine.state.monitors[mon_i], &self.engine.cfg);
            self.engine.state.monitors[mon_i].workspaces[ws_i].scroll = scroll;
        }
        self.arrange(mon_i)?;
        self.focus(Some(win))?;

        Ok(())
    }

    fn unmanage(&mut self, win: Window, destroyed: bool) -> Result<(), Box<dyn std::error::Error>> {
        // 1. Si ya fue removido (ej: doble evento Unmap + Destroy), salimos silenciosamente.
        let client = match self.engine.state.remove_client(win) {
            Some(c) => c,
            None => return Ok(()),
        };

        if !destroyed {
            let _ = self.conn.configure_window(win,
                &ConfigureWindowAux::new().border_width(client.old_border_w));
            let _ = self.set_wm_state(win, 0);
            let _ = self.conn.ungrab_button(ButtonIndex::ANY, win, ModMask::ANY);
        }

        self.update_client_list()?;
        let mon_i = client.monitor;

        // 2. Evita el pánico si el monitor ya no existe tras un hotplug.
        if mon_i < self.engine.state.monitors.len() {
            let scroll = ideal_scroll(&self.engine.state.monitors[mon_i], &self.engine.cfg);
            let ws_i = client.workspace;
            if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
                self.engine.state.monitors[mon_i].workspaces[ws_i].scroll = scroll;
            }
            let _ = self.arrange(mon_i);
            let _ = self.focus_best(mon_i);
        }
        Ok(())
    }

    // ── Layout ─────────────────────────────────────────────────────────────────

    fn arrange(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() { return Ok(()); }

        self.hide_offscreen(mon_idx)?;

        let placements = arrange(&self.engine.state, mon_idx, &self.engine.cfg);
        for (win, geom, bw) in placements {
            self.apply_geom(win, geom, bw)?;
        }

        self.restack(mon_idx)?;
        self.draw_bar(mon_idx);
        Ok(())
    }

    fn hide_offscreen(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws  = &mon.workspaces[mon.active_ws];

        // Build a HashSet of windows in the active workspace — O(N) lookup instead of O(N²)
        let all_in_ws: std::collections::HashSet<Window> = ws.columns.iter()
            .flat_map(|c| c.windows.iter().copied())
            .chain(ws.floats.iter().copied())
            .collect();

        let all_on_mon: Vec<Window> = self.engine.state.clients.iter()
            .filter(|(_, c)| c.monitor == mon_idx)
            .map(|(w, _)| *w)
            .collect();

        for win in all_on_mon {
            let in_ws = all_in_ws.contains(&win);
            let client = match self.engine.state.clients.get_mut(&win) { Some(c) => c, None => continue };
            if !in_ws && !client.wm_hidden {
                let off_x = -(client.geom.w as i32 + 200);
                let _ = self.conn.configure_window(win,
                    &ConfigureWindowAux::new().x(off_x));
                client.wm_hidden = true;
            } else if in_ws && client.wm_hidden {
                let gx = client.geom.x;
                let gy = client.geom.y;
                let _ = self.conn.configure_window(win,
                    &ConfigureWindowAux::new().x(gx).y(gy));
                client.wm_hidden = false;
            }
        }
        Ok(())
    }

    fn restack(&self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws  = mon.ws();

        // 1. Raise floats above tiled
        for &win in &ws.floats {
            if self.engine.state.clients.contains_key(&win) {
                let _ = self.conn.configure_window(win,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            }
        }

        // 2. Raise fullscreen windows above everything
        for &win in &mon.focus_stack {
            if let Some(c) = self.engine.state.clients.get(&win) {
                if c.is_fullscreen() {
                    let _ = self.conn.configure_window(win,
                        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
                }
            }
        }
        Ok(())
    }

    fn apply_geom(&mut self, win: Window, geom: Rect, bw: u32)
        -> Result<(), Box<dyn std::error::Error>>
    {
        let client = match self.engine.state.clients.get(&win) { Some(c) => c, None => return Ok(()) };
        if geom == client.geom && bw == client.border_w { return Ok(()); }

        let _ = self.conn.configure_window(win, &ConfigureWindowAux::new()
            .x(geom.x).y(geom.y)
            .width(geom.w).height(geom.h)
            .border_width(bw));

        let event = ConfigureNotifyEvent {
            response_type: CONFIGURE_NOTIFY_EVENT,
            sequence: 0, event: win, window: win,
            above_sibling: x11rb::NONE,
            x: geom.x as i16, y: geom.y as i16,
            width: geom.w as u16, height: geom.h as u16,
            border_width: bw as u16,
            override_redirect: false,
        };
        // Fire-and-forget: no .check() here — this is called for every window
        // in arrange(), so a synchronous RTT per window is unacceptable.
        let _ = self.conn.send_event(false, win, EventMask::STRUCTURE_NOTIFY, event);

        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            c.geom    = geom;
            c.border_w = bw;
        }
        Ok(())
    }

    // ── Focus ──────────────────────────────────────────────────────────────────

    fn focus(&mut self, win: Option<Window>) -> Result<(), Box<dyn std::error::Error>> {
        let valid_win = win.filter(|w| self.engine.state.clients.contains_key(w));

        // unfocus previous
        let prev = self.engine.state.monitors[self.engine.state.sel_mon].focused;
        if prev != valid_win {
            if let Some(pw) = prev {
                if self.engine.state.clients.contains_key(&pw) {
                    self.unfocus(pw)?;
                }
            }
        }

        if let Some(w) = valid_win {
            let client = match self.engine.state.clients.get(&w) { Some(c) => c, None => return self.focus(None) };
            if client.no_focus() { return Ok(()); }

            let mon_i  = client.monitor;
            let geom   = client.geom;
            let wants  = client.wants_input;

            self.engine.state.sel_mon = mon_i;

            // set X11 input focus
            if wants {
                let _ = self.conn.set_input_focus(InputFocus::PARENT, w, x11rb::CURRENT_TIME);
            } else {
                let _ = self.conn.set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME);
            }
            if self.has_protocol(w, self.atoms.wm_take_focus)? {
                self.send_proto(w, self.atoms.wm_take_focus)?;
            }

            // focused border color
            let col = if self.engine.state.clients.get(&w).map(|c| c.flags.has(WinFlags::URGENT)).unwrap_or(false) {
                self.engine.cfg.col_urgent
            } else {
                self.engine.cfg.col_focused
            };
            let _ = self.conn.change_window_attributes(w,
                &ChangeWindowAttributesAux::new().border_pixel(col));
            self.grab_buttons(w, true)?;

            let serial = self.engine.state.next_serial();
            if let Some(c) = self.engine.state.clients.get_mut(&w) {
                c.focus_serial = serial;
                c.flags.clear(WinFlags::URGENT);
            }

            let mon = &mut self.engine.state.monitors[mon_i];
            mon.focused = Some(w);
            mon.focus_stack.retain(|&x| x != w);
            mon.focus_stack.push(w);

            let _ = self.conn.change_property32(PropMode::REPLACE, self.root,
                self.atoms.net_active_window, AtomEnum::WINDOW, &[w]);

            if self.engine.cfg.warp_cursor {
                let _ = self.conn.warp_pointer(x11rb::NONE, w,
                    0,0,0,0, (geom.w/2) as i16, (geom.h/2) as i16);
            }
        } else {
            // Only clear the focused window on the currently selected monitor.
            // Other monitors keep their own focused state independently.
            let sel = self.engine.state.sel_mon;
            if sel < self.engine.state.monitors.len() {
                self.engine.state.monitors[sel].focused = None;
            }
            let _ = self.conn.set_input_focus(InputFocus::POINTER_ROOT, self.root, x11rb::CURRENT_TIME);
            let _ = self.conn.change_property32(PropMode::REPLACE, self.root,
                self.atoms.net_active_window, AtomEnum::WINDOW, &[x11rb::NONE]);
        }

        Ok(())
    }

    fn unfocus(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let col = self.engine.cfg.col_normal;
        let _ = self.conn.change_window_attributes(win,
            &ChangeWindowAttributesAux::new().border_pixel(col));
        let _ = self.grab_buttons(win, false);
        Ok(())
    }

    fn focus_best(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() { return Ok(()); }

        let ws_idx = self.engine.state.monitors[mon_idx].active_ws;
        if ws_idx >= self.engine.state.monitors[mon_idx].workspaces.len() { return Ok(()); }

        let candidate = {
            let mon     = &self.engine.state.monitors[mon_idx];
            let col_win = mon.workspaces[ws_idx].focused_win();
            let from_stack = mon.focus_stack.iter().rev()
                .find(|&&w| {
                    self.engine.state.clients.get(&w)
                        .map(|c| c.workspace == ws_idx)
                        .unwrap_or(false)
                })
                .copied();
            col_win.or(from_stack)
        };
        self.focus(candidate)
    }

    // ── Actions ────────────────────────────────────────────────────────────────

    fn do_action(&mut self, action: Action) -> Result<(), Box<dyn std::error::Error>> {
        match action {
            Action::Spawn(cmd) => self.spawn(&cmd),
            Action::Kill => {
                if let Some(w) = self.engine.state.monitors[self.engine.state.sel_mon].focused {
                    self.kill(w)?;
                }
            }
            Action::FocusDir(dir) => self.focus_dir(dir)?,
            Action::MoveDir(dir)  => self.move_dir(dir)?,
            Action::ToggleFloat   => self.toggle_float()?,
            Action::ToggleFullscreen => self.toggle_fullscreen()?,
            Action::ToggleBar => {
                let mi = self.engine.state.sel_mon;
                self.engine.state.monitors[mi].show_bar ^= true;
                self.engine.state.monitors[mi].recalc_workarea(self.engine.cfg.bar_height);
                if let Some(bw) = self.engine.state.monitors[mi].bar_win {
                    if self.engine.state.monitors[mi].show_bar {
                        let _ = self.conn.map_window(bw);
                    } else {
                        let _ = self.conn.unmap_window(bw);
                    }
                }
                self.arrange(mi)?;
            }
            Action::SetLayout(lk) => {
                self.engine.state.layout = lk;
                // Global layout → rearrange every monitor so they stay in sync.
                for mi in 0..self.engine.state.monitors.len() {
                    self.arrange(mi)?;
                }
            }
            Action::CycleLayout => {
                self.engine.state.layout = match self.engine.state.layout {
                    LayoutKind::Column  => LayoutKind::Monocle,
                    LayoutKind::Monocle => LayoutKind::Grid,
                    LayoutKind::Grid    => LayoutKind::Column,
                };
                for mi in 0..self.engine.state.monitors.len() {
                    self.arrange(mi)?;
                }
            }
            Action::GrowCol(px) => self.grow_col(px)?,
            Action::NewColumn   => self.new_column()?,
            Action::CollapseColumn => self.collapse_col()?,
            Action::View(ws_idx) => self.view_ws(ws_idx)?,
            Action::MoveToWs(ws_idx) => self.move_to_ws(ws_idx)?,
            Action::FocusMon(dir) => self.focus_mon(dir)?,
            Action::MoveMon(dir)  => self.move_mon(dir)?,
            Action::Restart => {
                // exec() replaces the current process image without forking,
                // so there's no race where two maverick instances fight over
                // X11 grabs simultaneously.
                use std::os::unix::process::CommandExt;
                if let Ok(exe) = std::env::current_exe() {
                    let err = std::process::Command::new(exe).exec();
                    log::error!("restart exec failed: {err}");
                }
                self.engine.state.running = false;
            }
            Action::Quit => { self.engine.state.running = false; }
            Action::QuitConfirm => {
                if self.show_quit_dialog()? {
                    self.engine.state.running = false;
                }
            }
        }
        Ok(())
    }

    /// Small native X11 confirmation dialog: "Quit maverick?".
    /// Returns true if the user confirmed (Y / Enter), false if cancelled (N / Esc).
    /// Uses a blocking event mini-loop — acceptable for a modal one-shot dialog.
    fn show_quit_dialog(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let mi     = self.engine.state.sel_mon
            .min(self.engine.state.monitors.len().saturating_sub(1));
        let screen = self.engine.state.monitors[mi].screen;

        // Dialog geometry: centred on the current monitor
        let dw: u16 = 370;
        let dh: u16 = 96;
        let dx = screen.x as i16 + (screen.w as i16 - dw as i16) / 2;
        let dy = screen.y as i16 + (screen.h as i16 - dh as i16) / 2;

        let bg     = self.engine.cfg.col_bar_bg;
        let fg     = self.engine.cfg.col_bar_fg;
        let accent = self.engine.cfg.col_focused;
        let dim    = (fg & 0xfefefe) >> 1; // ~50% brightness of fg for the hint line

        // ── Create dialog window ──────────────────────────────────────────────
        let setup  = self.conn.setup();
        let screen = &setup.roots[self.screen_num];
        let depth  = screen.root_depth;
        let visual = screen.root_visual;

        let win = self.conn.generate_id()?;
        self.conn.create_window(
            depth, win, self.root,
            dx, dy, dw, dh,
            1,                          // 1px border
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .override_redirect(1)   // WM ignores it — including maverick itself
                .background_pixel(bg)
                .border_pixel(accent)
                .event_mask(EventMask::EXPOSURE | EventMask::KEY_PRESS),
        )?.check()?;

        // ── Load X11 core font ("fixed" is always present) ────────────────────
        let font = self.conn.generate_id()?;
        self.conn.open_font(font, b"fixed")?.check()?;

        let gc = self.conn.generate_id()?;
        self.conn.create_gc(gc, win, &CreateGCAux::new()
            .foreground(fg)
            .background(bg)
            .font(font)
            .graphics_exposures(0),
        )?.check()?;

        self.conn.map_window(win)?.check()?;
        self.conn.configure_window(win,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))?.check()?;

        // Modal: grab keyboard so no keybind fires while dialog is visible
        self.conn.grab_keyboard(
            false, win, x11rb::CURRENT_TIME,
            GrabMode::ASYNC, GrabMode::ASYNC,
        )?.reply()?;

        self.conn.flush()?;

        // ── Draw helper ───────────────────────────────────────────────────────
        let paint = |conn: &RustConnection| -> Result<(), Box<dyn std::error::Error>> {
            // Accent header bar (22 px)
            conn.change_gc(gc, &ChangeGCAux::new().foreground(accent))?;
            conn.poly_fill_rectangle(win, gc,
                &[Rectangle { x: 0, y: 0, width: dw, height: 22 }])?;

            // Header label ("🦅 maverick" in ASCII-safe form)
            conn.change_gc(gc, &ChangeGCAux::new().foreground(bg))?;
            conn.image_text8(win, gc, 10, 15, b"maverick")?;

            // Body background
            conn.change_gc(gc, &ChangeGCAux::new().foreground(bg))?;
            conn.poly_fill_rectangle(win, gc,
                &[Rectangle { x: 0, y: 22, width: dw, height: dh - 22 }])?;

            // Main question
            conn.change_gc(gc, &ChangeGCAux::new().foreground(fg))?;
            conn.image_text8(win, gc, 14, 50, b"Seguro que quieres salir de maverick?")?;

            // Hint line
            conn.change_gc(gc, &ChangeGCAux::new().foreground(dim))?;
            conn.image_text8(win, gc, 14, 78, b"[Y / Enter] Si      [N / Escape] Cancelar")?;

            conn.flush()?;
            Ok(())
        };

        paint(&self.conn)?;

        // ── Event mini-loop ───────────────────────────────────────────────────
        let confirmed;
        'dlg: loop {
            match self.conn.wait_for_event()? {
                Event::Expose(e) if e.count == 0 => { paint(&self.conn)?; }

                Event::KeyPress(e) => match e.detail {
                    // Y key (keycode 29) or Return (36) or KP_Enter (104)
                    29 | 36 | 104 => { confirmed = true;  break 'dlg; }
                    // N key (57) or Escape (9)
                    57 | 9        => { confirmed = false; break 'dlg; }
                    _ => {}
                },

                _ => {}
            }
        }

        // ── Cleanup ───────────────────────────────────────────────────────────
        let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = self.conn.destroy_window(win);
        let _ = self.conn.close_font(font);
        let _ = self.conn.free_gc(gc);
        self.conn.flush()?;

        Ok(confirmed)
    }

    fn spawn(&self, cmd: &[String]) {
        if cmd.is_empty() { return; }
        let _ = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    fn kill(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        if self.has_protocol(win, self.atoms.wm_delete_window)? {
            self.send_proto(win, self.atoms.wm_delete_window)?;
        } else {
            let _ = self.conn.kill_client(win);
        }
        Ok(())
    }

    fn focus_dir(&mut self, dir: Dir) -> Result<(), Box<dyn std::error::Error>> {
        let mi    = self.engine.state.sel_mon;
        let ws_i  = self.engine.state.monitors[mi].active_ws;
        let ws    = &self.engine.state.monitors[mi].workspaces[ws_i];

        match dir {
            Dir::Left | Dir::Right => {
                let n = ws.columns.len();
                if n == 0 { return Ok(()); }
                let new_ci = if dir == Dir::Left {
                    (ws.focus.column_idx + n - 1) % n
                } else {
                    (ws.focus.column_idx + 1) % n
                };
                self.engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = new_ci;
                let win = self.engine.state.monitors[mi].workspaces[ws_i]
                    .columns[new_ci].focused_win();
                let scroll = ideal_scroll(&self.engine.state.monitors[mi], &self.engine.cfg);
                self.engine.state.monitors[mi].workspaces[ws_i].scroll = scroll;
                self.arrange(mi)?;
                self.focus(win)?;
            }
            Dir::Up | Dir::Down => {
                let ci = ws.focus.column_idx;
                if ci >= ws.columns.len() { return Ok(()); }
                let col = &ws.columns[ci];
                let n   = col.windows.len();
                if n == 0 { return Ok(()); }
                let new_ri = if dir == Dir::Up {
                    (col.focused + n - 1) % n
                } else {
                    (col.focused + 1) % n
                };
                self.engine.state.monitors[mi].workspaces[ws_i].columns[ci].focused = new_ri;
                let win = self.engine.state.monitors[mi].workspaces[ws_i]
                    .columns[ci].windows[new_ri];
                self.arrange(mi)?;
                self.focus(Some(win))?;
            }
            Dir::Next | Dir::Prev => {
                // fallback: cycle focus stack
                let focused = self.engine.state.monitors[mi].focused;
                let stack = self.engine.state.monitors[mi].focus_stack.clone();
                if stack.is_empty() { return Ok(()); }
                let new_win = match focused {
                    Some(fw) => {
                        let pos = stack.iter().position(|&w| w == fw).unwrap_or(0);
                        let n = stack.len();
                        let ni = if dir == Dir::Next { (pos+1)%n } else { (pos+n-1)%n };
                        stack[ni]
                    }
                    None => stack[0],
                };
                self.focus(Some(new_win))?;
            }
        }
        Ok(())
    }

    fn move_dir(&mut self, dir: Dir) -> Result<(), Box<dyn std::error::Error>> {
        let mi      = self.engine.state.sel_mon;
        let ws_i    = self.engine.state.monitors[mi].active_ws;
        let focused = match self.engine.state.monitors[mi].focused { Some(w) => w, None => return Ok(()) };

        let default_col_w = self.engine.cfg.default_col_w;
        if !self.engine.state.apply_move_dir(dir, default_col_w) {
            return Ok(()); // float, boundary no-op, etc.
        }

        let scroll = ideal_scroll(&self.engine.state.monitors[mi], &self.engine.cfg);
        self.engine.state.monitors[mi].workspaces[ws_i].scroll = scroll;
        self.arrange(mi)?;
        self.focus(Some(focused))?;
        Ok(())
    }
    fn toggle_float(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mi   = self.engine.state.sel_mon;
        let ws_i = self.engine.state.monitors[mi].active_ws;
        let win  = match self.engine.state.monitors[mi].focused { Some(w) => w, None => return Ok(()) };

        let is_float = self.engine.state.clients.get(&win).map(|c| c.is_float()).unwrap_or(false);

        if is_float {
            // Un-float: move into tiled
            let ws = self.engine.state.monitors[mi].workspaces[ws_i].clone();
            let ws = ws.remove_window(win);
            let dw = self.engine.cfg.default_col_w;
            let workarea_w = self.engine.state.monitors[mi].workarea.w;
            self.engine.state.monitors[mi].workspaces[ws_i] = ws.add_tiled(win, dw, workarea_w);
            
            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                c.flags.clear(WinFlags::FLOAT);
            }
        } else {
            // Float: remove from columns
            let ws = self.engine.state.monitors[mi].workspaces[ws_i].clone();
            let mut ws = ws.remove_window(win);
            ws.floats.push(win);
            self.engine.state.monitors[mi].workspaces[ws_i] = ws;

            if let Some(c) = self.engine.state.clients.get_mut(&win) {
                c.flags.set(WinFlags::FLOAT);
            }
        }

        self.arrange(mi)?;
        Ok(())
    }

    fn toggle_fullscreen(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let win = match self.engine.state.monitors[self.engine.state.sel_mon].focused {
            Some(w) => w, None => return Ok(()),
        };
        let currently = self.engine.state.clients.get(&win).map(|c| c.is_fullscreen()).unwrap_or(false);
        self.set_fullscreen(win, !currently)
    }

    fn set_fullscreen(&mut self, win: Window, fs: bool) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            if fs == c.is_fullscreen() { return Ok(()); }
            if fs {
                c.flags.set(WinFlags::FULLSCREEN);
                c.saved_geom    = c.geom;
                c.old_border_w  = c.border_w;
                c.border_w      = 0;
                // Force geom to a sentinel so apply_geom doesn't skip the X11 call
                c.geom          = Rect::default();
            } else {
                c.flags.clear(WinFlags::FULLSCREEN);
                c.border_w = c.old_border_w;
                // Reset geom so apply_geom doesn't skip the restore
                c.geom = Rect::default();
            }
            let mut state_atoms = Vec::new();
            if c.is_fullscreen() { state_atoms.push(self.atoms.net_wm_state_fullscreen); }
            let _ = self.conn.change_property32(PropMode::REPLACE, win,
                self.atoms.net_wm_state, AtomEnum::ATOM, &state_atoms);
        }
        let mi = self.engine.state.clients.get(&win).map(|c| c.monitor).unwrap_or(0);
        self.arrange(mi)?;
        // Raise fullscreen windows above everything else
        if self.engine.state.clients.get(&win).map(|c| c.is_fullscreen()).unwrap_or(false) {
            let _ = self.conn.configure_window(win,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        Ok(())
    }

    fn grow_col(&mut self, px: i32) -> Result<(), Box<dyn std::error::Error>> {
        let mi   = self.engine.state.sel_mon;
        let ws_i = self.engine.state.monitors[mi].active_ws;
        let ci   = self.engine.state.monitors[mi].workspaces[ws_i].focus.column_idx;
        let workarea_w = self.engine.state.monitors[mi].workarea.w;
        if let Some(col) = self.engine.state.monitors[mi].workspaces[ws_i].columns.get_mut(ci) {
            let new_w = (col.width as i32 + px).max(100) as u32;
            col.width = new_w.min(workarea_w - 100);
        }
        let scroll = ideal_scroll(&self.engine.state.monitors[mi], &self.engine.cfg);
        self.engine.state.monitors[mi].workspaces[ws_i].scroll = scroll;
        self.arrange(mi)?;
        Ok(())
    }

    fn new_column(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mi   = self.engine.state.sel_mon;
        let ws_i = self.engine.state.monitors[mi].active_ws;
        let win  = match self.engine.state.monitors[mi].focused { Some(w) => w, None => return Ok(()) };

        if self.engine.state.clients.get(&win).map(|c| c.is_float()).unwrap_or(true) {
            return Ok(());
        }

        let dw = self.engine.cfg.default_col_w;
        let ws = self.engine.state.monitors[mi].workspaces[ws_i].clone();
        let mut ws = ws.remove_window(win);

        let ci = ws.focus.column_idx;
        let ins_pos = (ci + 1).min(ws.columns.len());

        let mut new_col = crate::types::Column::new(dw);
        new_col.windows.push(win);
        ws.columns.insert(ins_pos, new_col);
        ws.focus.column_idx = ins_pos;

        // Commit workspace first so ideal_scroll reads the updated column structure.
        // Computing scroll on the stale monitor reference (old workspace) gave wrong centering.
        self.engine.state.monitors[mi].workspaces[ws_i] = ws;
        let scroll = ideal_scroll(&self.engine.state.monitors[mi], &self.engine.cfg);
        self.engine.state.monitors[mi].workspaces[ws_i].scroll = scroll;

        self.arrange(mi)?;
        self.focus(Some(win))?;
        Ok(())
    }

    fn collapse_col(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mi   = self.engine.state.sel_mon;
        let ws_i = self.engine.state.monitors[mi].active_ws;

        let mut ws = self.engine.state.monitors[mi].workspaces[ws_i].clone();
        let ci = ws.focus.column_idx;
        if ws.columns.len() < 2 || ci == 0 || ci >= ws.columns.len() { return Ok(()); }

        let target = ci - 1;

        // Drain the source column in one move (O(n)) instead of retain-per-element (O(n²))
        let wins: Vec<Window> = std::mem::take(&mut ws.columns[ci].windows);
        ws.columns[target].windows.extend(wins);

        ws.columns.retain(|c| !c.windows.is_empty());
        ws.focus.column_idx = target.min(ws.columns.len().saturating_sub(1));
        // Sync window_idx with col.focused
        if let Some(col) = ws.columns.get(ws.focus.column_idx) {
            ws.focus.window_idx = col.focused.min(col.windows.len().saturating_sub(1));
        }

        let scroll = ideal_scroll(&self.engine.state.monitors[mi], &self.engine.cfg);
        ws.scroll = scroll;
        self.engine.state.monitors[mi].workspaces[ws_i] = ws;
        self.arrange(mi)?;
        Ok(())
    }

    fn view_ws(&mut self, ws_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let mi = self.engine.state.sel_mon;
        if ws_idx >= self.engine.state.monitors[mi].workspaces.len() { return Ok(()); }
        if ws_idx == self.engine.state.monitors[mi].active_ws { return Ok(()); }

        self.engine.state.monitors[mi].active_ws = ws_idx;

        // sync scroll
        let scroll = ideal_scroll(&self.engine.state.monitors[mi], &self.engine.cfg);
        self.engine.state.monitors[mi].workspaces[ws_idx].scroll = scroll;

        let _ = self.conn.change_property32(PropMode::REPLACE, self.root,
            self.atoms.net_current_desktop, AtomEnum::CARDINAL, &[ws_idx as u32]);

        self.arrange(mi)?;
        self.focus_best(mi)?;
        Ok(())
    }

    fn move_to_ws(&mut self, ws_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let mi  = self.engine.state.sel_mon;
        let win = match self.engine.state.monitors[mi].focused { Some(w) => w, None => return Ok(()) };
        let src_ws = match self.engine.state.clients.get(&win) { Some(c) => c.workspace, None => return Ok(()) };
        if src_ws == ws_idx || ws_idx >= self.engine.state.monitors[mi].workspaces.len() { return Ok(()); }

        let is_float = self.engine.state.clients.get(&win).map(|c| c.is_float()).unwrap_or(false);

        let dw = self.engine.cfg.default_col_w;

        // 1. Remove window from source workspace
        let mut src_workspace = self.engine.state.monitors[mi].workspaces[src_ws].clone();
        src_workspace = src_workspace.remove_window(win);
        self.engine.state.monitors[mi].workspaces[src_ws] = src_workspace;

        // 2. Remove from focus_stack so focus_best doesn't try to re-focus it
        self.engine.state.monitors[mi].focus_stack.retain(|&w| w != win);
        if self.engine.state.monitors[mi].focused == Some(win) {
            self.engine.state.monitors[mi].focused =
                self.engine.state.monitors[mi].focus_stack.last().copied();
        }

        // 3. Add to destination workspace
        let mut dst_workspace = self.engine.state.monitors[mi].workspaces[ws_idx].clone();
        if is_float {
            dst_workspace.floats.push(win);
        } else {
            let workarea_w = self.engine.state.monitors[mi].workarea.w;
            dst_workspace = dst_workspace.add_tiled(win, dw, workarea_w);
        }
        self.engine.state.monitors[mi].workspaces[ws_idx] = dst_workspace;

        // 4. Update client metadata and EWMH _NET_WM_DESKTOP
        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            c.workspace = ws_idx;
        }
        let _ = self.conn.change_property32(PropMode::REPLACE, win,
            self.atoms.net_wm_desktop, AtomEnum::CARDINAL, &[ws_idx as u32]);

        self.arrange(mi)?;
        self.focus_best(mi)?;
        Ok(())
    }

    fn focus_mon(&mut self, dir: Dir) -> Result<(), Box<dyn std::error::Error>> {
        let n = self.engine.state.monitors.len();
        if n <= 1 { return Ok(()); }
        let cur = self.engine.state.sel_mon;
        let new = match dir {
            Dir::Next => (cur + 1) % n,
            Dir::Prev => (cur + n - 1) % n,
            _         => (cur + 1) % n,
        };
        if let Some(fw) = self.engine.state.monitors[cur].focused { self.unfocus(fw)?; }
        self.engine.state.sel_mon = new;
        self.focus_best(new)?;
        Ok(())
    }

    fn move_mon(&mut self, dir: Dir) -> Result<(), Box<dyn std::error::Error>> {
        let n   = self.engine.state.monitors.len();
        if n <= 1 { return Ok(()); }
        let mi  = self.engine.state.sel_mon;
        let win = match self.engine.state.monitors[mi].focused { Some(w) => w, None => return Ok(()) };
        let new_mi = match dir {
            Dir::Next => (mi + 1) % n,
            Dir::Prev => (mi + n - 1) % n,
            _         => (mi + 1) % n,
        };

        let src_ws = self.engine.state.clients.get(&win).map(|c| c.workspace).unwrap_or(0);
        let is_float = self.engine.state.clients.get(&win).map(|c| c.is_float()).unwrap_or(false);

        let dw = self.engine.cfg.default_col_w;
        
        // 1. Remover ventana del workspace origen en el monitor actual
        let mut src_workspace = self.engine.state.monitors[mi].workspaces[src_ws].clone();
        src_workspace = src_workspace.remove_window(win);
        self.engine.state.monitors[mi].workspaces[src_ws] = src_workspace;
        
        // 2. Agregar ventana al mismo workspace en el monitor destino
        let mut dst_workspace = self.engine.state.monitors[new_mi].workspaces[src_ws].clone();
        if is_float {
            dst_workspace.floats.push(win);
        } else {
            let workarea_w = self.engine.state.monitors[new_mi].workarea.w;
            dst_workspace = dst_workspace.add_tiled(win, dw, workarea_w);
        }
        self.engine.state.monitors[new_mi].workspaces[src_ws] = dst_workspace;
        // Remove from source monitor's focus_stack before moving
        self.engine.state.monitors[mi].focus_stack.retain(|&w| w != win);
        if self.engine.state.monitors[mi].focused == Some(win) {
            self.engine.state.monitors[mi].focused =
                self.engine.state.monitors[mi].focus_stack.last().copied();
        }

        self.engine.state.monitors[new_mi].focus_stack.push(win);

        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            c.monitor   = new_mi;
            c.workspace = src_ws;
        }

        self.arrange(mi)?;
        self.arrange(new_mi)?;
        self.engine.state.sel_mon = new_mi;
        self.focus(Some(win))?;
        Ok(())
    }

    // ── Event handlers ─────────────────────────────────────────────────────────

    fn on_map_request(&mut self, e: MapRequestEvent) -> Result<(), Box<dyn std::error::Error>> {
        let attrs = match self.conn.get_window_attributes(e.window)?.reply() {
            Ok(a) => a, 
            Err(err) => {
                log::debug!("Failed to get attributes for window {}: {}", e.window, err);
                return Ok(());
            }
        };
        if !attrs.override_redirect && !self.engine.state.clients.contains_key(&e.window) {
            if let Err(err) = self.manage(e.window, &attrs) {
                log::warn!("Failed to manage window {} on map request: {}", e.window, err);
            }
        }
        Ok(())
    }

    fn on_destroy(&mut self, e: DestroyNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if self.engine.state.clients.contains_key(&e.window) {
            self.unmanage(e.window, true)?;
        }
        Ok(())
    }

    fn on_unmap(&mut self, e: UnmapNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if !self.engine.state.clients.contains_key(&e.window) { return Ok(()); }
        if e.response_type & 0x80 != 0 {
            let _ = self.set_wm_state(e.window, 0);
        } else {
            self.unmanage(e.window, false)?;
        }
        Ok(())
    }

    fn on_configure_request(&mut self, e: ConfigureRequestEvent) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(client) = self.engine.state.clients.get(&e.window) {
            if !client.is_float() && !client.is_fullscreen() {
                let geom = client.geom;
                let bw   = client.border_w;
                let ev = ConfigureNotifyEvent {
                    response_type: CONFIGURE_NOTIFY_EVENT,
                    sequence: 0, event: e.window, window: e.window,
                    above_sibling: x11rb::NONE,
                    x: geom.x as i16, y: geom.y as i16,
                    width: geom.w as u16, height: geom.h as u16,
                    border_width: bw as u16, override_redirect: false,
                };
                let _ = self.conn.send_event(false, e.window, EventMask::STRUCTURE_NOTIFY, ev);
                return Ok(());
            }
        }
        // floating or unmanaged: honor the request
        let mut aux = ConfigureWindowAux::new();
        if e.value_mask.contains(ConfigWindow::X)            { aux = aux.x(e.x as i32); }
        if e.value_mask.contains(ConfigWindow::Y)            { aux = aux.y(e.y as i32); }
        if e.value_mask.contains(ConfigWindow::WIDTH)        { aux = aux.width(e.width as u32); }
        if e.value_mask.contains(ConfigWindow::HEIGHT)       { aux = aux.height(e.height as u32); }
        if e.value_mask.contains(ConfigWindow::BORDER_WIDTH) { aux = aux.border_width(e.border_width as u32); }
        if e.value_mask.contains(ConfigWindow::STACK_MODE)   { aux = aux.stack_mode(e.stack_mode); }
        let _ = self.conn.configure_window(e.window, &aux);

        if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
            if e.value_mask.contains(ConfigWindow::X) { c.geom.x = e.x as i32; }
            if e.value_mask.contains(ConfigWindow::Y) { c.geom.y = e.y as i32; }
            if e.value_mask.contains(ConfigWindow::WIDTH)  { c.geom.w = e.width as u32; }
            if e.value_mask.contains(ConfigWindow::HEIGHT) { c.geom.h = e.height as u32; }
        }
        Ok(())
    }

    fn on_configure_notify(&mut self, e: ConfigureNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.window == self.root {
            // Monitor change — re-detect topology and redistribute clients
            let setup = self.conn.setup();
            let screen = &setup.roots[self.screen_num];
            let new_mons = detect_monitors(&self.conn, screen, &self.engine.cfg)?;
            if new_mons.len() != self.engine.state.monitors.len() {
                log::info!("monitor topology changed ({} -> {})",
                    self.engine.state.monitors.len(), new_mons.len());

                // Collect all managed windows before replacing the monitor vec.
                let old_clients: Vec<Window> = self.engine.state.clients.keys().copied().collect();

                // Replace monitors with fresh ones (empty workspaces).
                self.engine.state.monitors = new_mons;

                // Clamp sel_mon so no code tries to index a monitor that no longer exists.
                let n_mons = self.engine.state.monitors.len();
                self.engine.state.sel_mon = self.engine.state.sel_mon
                    .min(n_mons.saturating_sub(1));

                // Re-assign every client to monitor 0 / workspace 0 and
                // insert it into the column/float structure.
                let dw = self.engine.cfg.default_col_w;
                for win in old_clients {
                    // Update client metadata
                    if let Some(c) = self.engine.state.clients.get_mut(&win) {
                        c.monitor   = 0;
                        c.workspace = 0;
                    }
                    // Re-insert into the workspace structure
                    let is_float = self.engine.state.clients.get(&win)
                        .map(|c| c.is_float()).unwrap_or(false);
                    let workarea_w = self.engine.state.monitors[0].workarea.w;
                    if is_float {
                        self.engine.state.monitors[0].workspaces[0].floats.push(win);
                    } else {
                        let mut ws = self.engine.state.monitors[0].workspaces[0].clone();
                        ws = ws.add_tiled(win, dw, workarea_w);
                        self.engine.state.monitors[0].workspaces[0] = ws;
                    }
                }

                for i in 0..self.engine.state.monitors.len() {
                    self.arrange(i)?;
                }
            }
        }
        Ok(())
    }

    fn on_property(&mut self, e: PropertyNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.window == self.root && e.atom == u32::from(AtomEnum::WM_NAME) {
            self.update_status()?;
            return Ok(());
        }
        if e.state == Property::DELETE { return Ok(()); }

        if self.engine.state.clients.contains_key(&e.window) {
            let win = e.window;
            let bar_relevant;
            if e.atom == self.atoms.net_wm_name || e.atom == u32::from(AtomEnum::WM_NAME) {
                self.refresh_title(win)?;
                bar_relevant = true;
            } else if e.atom == u32::from(AtomEnum::WM_HINTS) {
                self.refresh_hints(win)?;
                bar_relevant = true;
            } else {
                // Other property changes (size hints, ICCCM state, etc.) don't
                // affect the bar — skip the redraw to avoid thrashing during
                // Firefox page loads, GTK tooltip updates, etc.
                bar_relevant = false;
            }
            if bar_relevant {
                let mi = self.engine.state.clients.get(&win).map(|c| c.monitor).unwrap_or(0);
                self.draw_bar(mi);
            }
        }
        Ok(())
    }

    fn on_client_message(&mut self, e: ClientMessageEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.type_ == self.atoms.net_wm_state {
            let data = e.data.as_data32();
            let action = data[0];
            let a1     = data[1];
            let a2     = data[2];
            let fs_atom = self.atoms.net_wm_state_fullscreen;
            if a1 == fs_atom || a2 == fs_atom {
                let cur = self.engine.state.clients.get(&e.window).map(|c| c.is_fullscreen()).unwrap_or(false);
                let new_fs = match action { 0 => false, 1 => true, _ => !cur };
                if new_fs != cur { self.set_fullscreen(e.window, new_fs)?; }
            }
            let urg = self.atoms.net_wm_state_demands_attention;
            if a1 == urg || a2 == urg {
                if let Some(c) = self.engine.state.clients.get_mut(&e.window) {
                    c.flags.set(WinFlags::URGENT);
                }
                let mi = self.engine.state.clients.get(&e.window).map(|c| c.monitor).unwrap_or(0);
                self.draw_bar(mi);
            }
        } else if e.type_ == self.atoms.net_current_desktop {
            let ws = e.data.as_data32()[0] as usize;
            self.view_ws(ws)?;
        } else if e.type_ == self.atoms.net_active_window {
            if let Some(c) = self.engine.state.clients.get(&e.window) {
                let ws_i  = c.workspace;
                let mon_i = c.monitor;
                // Guard against stale client metadata after a hotplug.
                if mon_i < self.engine.state.monitors.len()
                    && ws_i < self.engine.state.monitors[mon_i].workspaces.len()
                {
                    self.engine.state.monitors[mon_i].active_ws = ws_i;
                    self.arrange(mon_i)?;
                    let win = e.window;
                    self.focus(Some(win))?;
                }
            }
        } else if e.type_ == self.atoms.net_close_window {
            self.kill(e.window)?;
        }
        Ok(())
    }

    fn on_key(&mut self, e: KeyPressEvent) -> Result<(), Box<dyn std::error::Error>> {
        let ksym = self.keycode_to_keysym(e.detail, u16::from(e.state))?;
        let ksym = normalize_ksym(ksym);
        let mods = clean_mask(u16::from(e.state), self.numlock);
        if let Some(action) = self.keymap.get(&(mods, ksym)).cloned() {
            self.do_action(action)?;
        }
        Ok(())
    }

    fn on_button_press(&mut self, e: ButtonPressEvent) -> Result<(), Box<dyn std::error::Error>> {
        // ── Bar click: switch workspace on the clicked monitor ───────────────
        for mon_i in 0..self.engine.state.monitors.len() {
            if self.engine.state.monitors[mon_i].bar_win == Some(e.event) {
                if let Some(ws_i) = self.bar.tag_at_x(e.event_x, &self.engine.cfg.tag_names) {
                    if ws_i < self.engine.state.monitors[mon_i].workspaces.len() {
                        // Switch focus to clicked monitor if different.
                        if mon_i != self.engine.state.sel_mon {
                            if let Some(fw) = self.engine.state.monitors[self.engine.state.sel_mon].focused {
                                let _ = self.unfocus(fw);
                            }
                            self.engine.state.sel_mon = mon_i;
                        }
                        self.engine.state.monitors[mon_i].active_ws = ws_i;
                        let scroll = ideal_scroll(&self.engine.state.monitors[mon_i], &self.engine.cfg);
                        self.engine.state.monitors[mon_i].workspaces[ws_i].scroll = scroll;
                        self.arrange(mon_i)?;
                        self.focus_best(mon_i)?;
                    }
                }
                // Bar is override_redirect and has no passive grab — no allow_events needed.
                return Ok(());
            }
        }

        let mi = self.engine.state.mon_at(e.root_x as i32, e.root_y as i32);
        if mi != self.engine.state.sel_mon {
            if let Some(fw) = self.engine.state.monitors[self.engine.state.sel_mon].focused {
                self.unfocus(fw)?;
            }
            self.engine.state.sel_mon = mi;
        }

        let mut replay = false;
        if let Some(cw) = self.find_client(e.event) {
            if self.engine.state.monitors[mi].focused != Some(cw) {
                self.focus(Some(cw))?;
                self.restack(mi)?;
                replay = true;
            }
        } else if e.event == self.root {
            self.focus(None)?;
            replay = true;
        }

        let sup: u16 = ModMask::M4.into();
        let clean = clean_mask(u16::from(e.state), self.numlock);
        if clean == sup {
            if let Some(cw) = self.find_client(e.event) {
                if let Some(c) = self.engine.state.clients.get(&cw) {
                    let geom = c.geom;
                    let is_resize = e.detail == ButtonIndex::M3.into();
                    // Attempt to grab the pointer; if another client holds the grab we skip.
                    // We set drag state AFTER a successful grab (not before) to avoid leaving
                    // a stale DragState if grab fails.
                    let grab_ok = self.conn.grab_pointer(
                        false, self.root,
                        EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                        GrabMode::ASYNC, GrabMode::ASYNC,
                        x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME,
                    ).ok()
                     .and_then(|cookie| cookie.reply().ok())
                     .map(|reply| u8::from(reply.status) == 0) // GrabStatus::SUCCESS == 0
                     .unwrap_or(false);

                    if grab_ok {
                        self.drag = Some(DragState {
                            win: cw, start_geom: geom,
                            ptr_x: e.root_x as i32, ptr_y: e.root_y as i32,
                            resize: is_resize,
                        });
                    }
                }
            }
        }

        self.conn.allow_events(
            if replay { Allow::REPLAY_POINTER } else { Allow::SYNC_POINTER },
            e.time,
        )?.check()?;
        Ok(())
    }

    fn on_button_release(&mut self, _e: ButtonReleaseEvent) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(drag) = self.drag.take() {
            self.conn.ungrab_pointer(x11rb::CURRENT_TIME)?.check()?;
            let mi  = self.engine.state.sel_mon;
            let win = drag.win;

            // If on_motion set the FLOAT flag but the window is still in a column,
            // promote it to ws.floats now so arrange() treats it as a float and
            // doesn't retile it back to its column position.
            let is_float = self.engine.state.clients.get(&win)
                .map(|c| c.is_float()).unwrap_or(false);
            if is_float {
                let ws_i = self.engine.state.monitors[mi].active_ws;
                let in_floats = self.engine.state.monitors[mi].workspaces[ws_i]
                    .floats.contains(&win);
                if !in_floats {
                    // Remove from column structure, add to floats
                    let mut ws = self.engine.state.monitors[mi].workspaces[ws_i].clone();
                    ws = ws.remove_window(win);  // removes from columns only (already not in floats)
                    ws.floats.push(win);
                    self.engine.state.monitors[mi].workspaces[ws_i] = ws;
                }
            }

            self.arrange(mi)?;
        }
        Ok(())
    }

    fn on_motion(&mut self, e: MotionNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(drag) = self.drag.clone() {
            let dx = e.root_x as i32 - drag.ptr_x;
            let dy = e.root_y as i32 - drag.ptr_y;

            let geom = if drag.resize {
                Rect::new(drag.start_geom.x, drag.start_geom.y,
                    (drag.start_geom.w as i32 + dx).max(50) as u32,
                    (drag.start_geom.h as i32 + dy).max(50) as u32)
            } else {
                Rect::new(drag.start_geom.x + dx, drag.start_geom.y + dy,
                    drag.start_geom.w, drag.start_geom.h)
            };

            if let Some(c) = self.engine.state.clients.get(&drag.win) {
                let bw = c.border_w;
                self.apply_geom(drag.win, geom, bw)?;
            }
            if let Some(c) = self.engine.state.clients.get_mut(&drag.win) {
                c.geom = geom;
                c.flags.set(WinFlags::FLOAT);
            }
        } else if self.engine.cfg.focus_mouse {
            if let Some(cw) = self.find_client(e.event) {
                if self.engine.state.monitors[self.engine.state.sel_mon].focused != Some(cw) {
                    self.focus(Some(cw))?;
                }
            }
        }
        Ok(())
    }

    fn on_enter(&mut self, e: EnterNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.mode != NotifyMode::NORMAL || e.detail == NotifyDetail::INFERIOR { return Ok(()); }
        if self.engine.cfg.focus_mouse {
            if let Some(cw) = self.find_client(e.event) {
                if self.engine.state.monitors[self.engine.state.sel_mon].focused != Some(cw) {
                    self.focus(Some(cw))?;
                }
            }
        }
        Ok(())
    }

    fn on_expose(&mut self, e: ExposeEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.count == 0 {
            for mi in 0..self.engine.state.monitors.len() {
                if self.engine.state.monitors[mi].bar_win == Some(e.window) {
                    self.draw_bar(mi);
                    break;
                }
            }
        }
        Ok(())
    }

    fn on_focus_in(&mut self, e: FocusInEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.mode != NotifyMode::NORMAL || e.detail == NotifyDetail::INFERIOR { return Ok(()); }
        let focused = self.engine.state.monitors[self.engine.state.sel_mon].focused;
        if let (Some(fw), Some(cw)) = (focused, self.find_client(e.event)) {
            if cw != fw { let _ = self.set_focus_x(fw); }
        }
        Ok(())
    }

    fn on_mapping(&mut self, e: MappingNotifyEvent) -> Result<(), Box<dyn std::error::Error>> {
        if e.request == Mapping::KEYBOARD || e.request == Mapping::MODIFIER {
            self.numlock = get_numlock(&self.conn)?;
            let (km, kpk, min) = build_raw_keymap(&self.conn)?;
            self.raw_keymap = km;
            self.raw_kpk    = kpk;
            self.raw_min    = min;
            self.grab_keys()?;
        }
        Ok(())
    }

    // ── Bar ────────────────────────────────────────────────────────────────────

    /// Mark monitor `mi` as needing a bar repaint.
    /// Actual drawing is deferred to flush_bars(), called once per event batch.
    #[inline]
    fn mark_bar(&mut self, mi: usize) {
        if mi < 64 { self.bar_dirty |= 1u64 << mi; }
    }

    /// Mark all monitors dirty (e.g. on status/layout change).
    #[inline]
    fn mark_all_bars(&mut self) {
        let n = self.engine.state.monitors.len().min(64);
        if n == 64 { self.bar_dirty = u64::MAX; }
        else       { self.bar_dirty |= (1u64 << n) - 1; }
    }

    /// Paint every dirty bar. Called once at the top of each event-loop iteration,
    /// after all pending events have been drained from the socket.
    fn flush_bars(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.bar_dirty == 0 { return Ok(()); }
        let dirty = self.bar_dirty;
        self.bar_dirty = 0;
        let n = self.engine.state.monitors.len().min(64);
        for mi in 0..n {
            if dirty & (1u64 << mi) != 0 {
                self.bar.draw(&self.conn, &self.engine.state, mi, &self.engine.cfg)?;
            }
        }
        Ok(())
    }

    /// Kept for call-sites that already exist in the code.
    /// All calls now just mark dirty; flush_bars() handles the actual paint.
    #[inline]
    fn draw_bar(&mut self, mi: usize) { self.mark_bar(mi); }

    fn update_status(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let prop = self.conn.get_property(
            false, self.root, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256,
        )?.reply()?;
        self.engine.state.status = String::from_utf8_lossy(&prop.value).into_owned();
        self.mark_all_bars();
        Ok(())
    }

    // ── EWMH ───────────────────────────────────────────────────────────────────

    fn update_ewmh_desktops(&self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        let n = self.engine.cfg.n_tags as u32;

        self.conn.change_property32(PropMode::REPLACE, self.root,
            a.net_number_of_desktops, AtomEnum::CARDINAL, &[n])?.check()?;

        let mut names = Vec::new();
        for name in &self.engine.cfg.tag_names {
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
        self.conn.change_property8(PropMode::REPLACE, self.root,
            a.net_desktop_names, a.utf8_string, &names)?.check()?;

        self.conn.change_property32(PropMode::REPLACE, self.root,
            a.net_current_desktop, AtomEnum::CARDINAL, &[0u32])?.check()?;
        Ok(())
    }

    fn update_client_list(&self) -> Result<(), Box<dyn std::error::Error>> {
        let wins: Vec<u32> = self.engine.state.clients.keys().copied().collect();
        self.conn.change_property32(PropMode::REPLACE, self.root,
            self.atoms.net_client_list, AtomEnum::WINDOW, &wins)?.check()?;
        Ok(())
    }

    fn set_wm_state(&self, win: Window, state: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.conn.change_property32(PropMode::REPLACE, win,
            self.atoms.wm_state, self.atoms.wm_state,
            &[state, x11rb::NONE])?.check()?;
        Ok(())
    }

    // ── Window property readers ────────────────────────────────────────────────

    fn read_title(&self, c: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
        let prop = self.conn.get_property(
            false, c.window, self.atoms.net_wm_name, self.atoms.utf8_string, 0, 256,
        )?.reply()?;
        if !prop.value.is_empty() {
            c.name = String::from_utf8_lossy(&prop.value).into_owned();
            return Ok(());
        }
        let prop2 = self.conn.get_property(
            false, c.window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256,
        )?.reply()?;
        c.name = String::from_utf8_lossy(&prop2.value).into_owned();
        Ok(())
    }

    fn read_class(&self, c: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
        let prop = match self.conn.get_property(
            false, c.window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256,
        )?.reply() {
            Ok(p) => p,
            Err(_) => return Ok(()), // WM_CLASS may not exist for all windows
        };
        let s = String::from_utf8_lossy(&prop.value);
        let mut parts = s.split('\0');
        c.instance = parts.next().unwrap_or("").to_string();
        c.class    = parts.next().unwrap_or("").to_string();
        Ok(())
    }

    fn read_window_type(&self, c: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
        let prop = match self.conn.get_property(
            false, c.window, self.atoms.net_wm_window_type, AtomEnum::ATOM, 0, 32,
        )?.reply() {
            Ok(p) => p,
            Err(_) => return Ok(()), // Property may not exist
        };

        if prop.type_ == u32::from(AtomEnum::ATOM) {
            let atoms: Vec<u32> = prop.value32().map(|i| i.collect()).unwrap_or_default();
            for a in atoms {
                if a == self.atoms.net_wm_window_type_desktop
                    || a == self.atoms.net_wm_window_type_dock
                {
                    c.is_unmanaged = true;
                }
                if a == self.atoms.net_wm_window_type_dialog
                    || a == self.atoms.net_wm_window_type_utility
                    || a == self.atoms.net_wm_window_type_menu
                    || a == self.atoms.net_wm_window_type_toolbar
                    || a == self.atoms.net_wm_window_type_splash
                {
                    c.flags.set(WinFlags::FLOAT);
                    c.is_dialog = true;
                }
            }
        }

        let sp = match self.conn.get_property(
            false, c.window, self.atoms.net_wm_state, AtomEnum::ATOM, 0, 32,
        )?.reply() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        if sp.type_ == u32::from(AtomEnum::ATOM) {
            let atoms: Vec<u32> = sp.value32().map(|i| i.collect()).unwrap_or_default();
            for a in atoms {
                if a == self.atoms.net_wm_state_fullscreen {
                    c.flags.set(WinFlags::FULLSCREEN);
                }
                // Modal windows (Chromium cert dialogs, GTK file choosers, etc.)
                // must float so they're not swallowed into the column layout.
                if a == self.atoms.net_wm_state_modal {
                    c.flags.set(WinFlags::FLOAT);
                    c.is_dialog = true;
                }
            }
        }
        Ok(())
    }

    fn read_wm_hints(&self, c: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
        let prop = match self.conn.get_property(
            false, c.window, self.atoms.wm_hints, self.atoms.wm_hints, 0, 9,
        )?.reply() {
            Ok(p) => p,
            Err(_) => return Ok(()), // WM_HINTS may not exist
        };
        if let Some(vals) = prop.value32() {
            let v: Vec<u32> = vals.collect();
            if !v.is_empty() {
                if v[0] & 1 != 0 && v.len() > 1 {
                    if v[1] == 0 { c.flags.set(WinFlags::NO_FOCUS); c.wants_input = false; }
                    else         { c.wants_input = true; }
                }
                if v[0] & 256 != 0 { c.flags.set(WinFlags::URGENT); }
            }
        }
        Ok(())
    }

    fn read_size_hints(&self, c: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
        let prop = match self.conn.get_property(
            false, c.window,
            AtomEnum::WM_NORMAL_HINTS, AtomEnum::WM_SIZE_HINTS, 0, 18,
        )?.reply() {
            Ok(p) => p,
            Err(_) => return Ok(()), // Size hints may not exist
        };
        if let Some(vals) = prop.value32() {
            let v: Vec<u32> = vals.collect();
            if v.len() >= 18 {
                let f = v[0];
                let h = &mut c.hints;
                if f & 16  != 0 { h.min_w = v[9]  as i32; h.min_h = v[10] as i32; }
                if f & 32  != 0 { h.max_w = v[11] as i32; h.max_h = v[12] as i32; }
                if f & 64  != 0 { h.inc_w = v[13] as i32; h.inc_h = v[14] as i32; }
                if f & 128 != 0 {
                    let denom = v[16].max(1);
                    h.min_aspect = v[15] as f32 / denom as f32;
                    h.max_aspect = v[17] as f32 / denom as f32;
                }
                if f & 256 != 0 { h.base_w = v[7] as i32; h.base_h = v[8] as i32; }
                h.valid = true;
                if h.max_w > 0 && h.max_h > 0 && h.max_w == h.min_w && h.max_h == h.min_h {
                    c.flags.set(WinFlags::FIXED);
                    c.flags.set(WinFlags::FLOAT);
                }
            }
        }
        Ok(())
    }

    fn apply_rules(&self, c: &mut Client) {
        for rule in &self.engine.cfg.rules {
            if rule.matches(&c.class, &c.name) {
                if rule.float { c.flags.set(WinFlags::FLOAT); }
                if let Some(ws) = rule.ws {
                    let mi = c.monitor;
                    if mi < self.engine.state.monitors.len()
                        && ws < self.engine.state.monitors[mi].workspaces.len()
                    {
                        c.workspace = ws;
                    }
                }
            }
        }
    }

    fn detect_portal(&self, c: &mut Client) {
        let float_classes = ["xdg-desktop-portal", "flameshot", "gpick", "pinentry", "screenkey"];
        let float_titles  = ["file upload", "open file", "save file", "file chooser",
                              "qt file dialog", "choose file", "select file"];
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

    fn transient_for(&self, win: Window) -> Result<Option<Window>, Box<dyn std::error::Error>> {
        let prop = self.conn.get_property(
            false, win, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1,
        )?.reply()?;
        Ok(prop.value32()
            .and_then(|mut v| v.next())
            .filter(|&w| w != 0 && w != self.root))
    }

    fn refresh_title(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            let mut tmp = c.clone();
            self.read_title(&mut tmp)?;
            if let Some(c2) = self.engine.state.clients.get_mut(&win) { c2.name = tmp.name; }
        }
        Ok(())
    }

    fn refresh_hints(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            let mut tmp = c.clone();
            self.read_wm_hints(&mut tmp)?;
            if let Some(c2) = self.engine.state.clients.get_mut(&win) { c2.flags = tmp.flags; }
        }
        Ok(())
    }

    // ── Helpers ────────────────────────────────────────────────────────────────

    fn find_client(&self, mut win: Window) -> Option<Window> {
        if self.engine.state.clients.contains_key(&win) { return Some(win); }
        loop {
            let tree = self.conn.query_tree(win).ok()?.reply().ok()?;
            let parent = tree.parent;
            if parent == self.root || parent == win || parent == x11rb::NONE { return None; }
            win = parent;
            if self.engine.state.clients.contains_key(&win) { return Some(win); }
        }
    }

    fn has_protocol(&self, win: Window, proto: u32) -> Result<bool, Box<dyn std::error::Error>> {
        let prop = self.conn.get_property(
            false, win, self.atoms.wm_protocols, AtomEnum::ATOM, 0, 32,
        )?.reply();
        Ok(prop.ok()
            .and_then(|p| p.value32().map(|mut v| v.any(|x| x == proto)))
            .unwrap_or(false))
    }

    fn send_proto(&self, win: Window, proto: u32) -> Result<(), Box<dyn std::error::Error>> {
        let ev = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32, sequence: 0, window: win,
            type_: self.atoms.wm_protocols,
            data: ClientMessageData::from([proto, x11rb::CURRENT_TIME, 0, 0, 0]),
        };
        let _ = self.conn.send_event(false, win, EventMask::NO_EVENT, ev);
        Ok(())
    }

    fn set_focus_x(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.set_input_focus(InputFocus::PARENT, win, x11rb::CURRENT_TIME);
        Ok(())
    }

    fn keycode_to_keysym(&self, code: u8, state: u16) -> Result<u32, Box<dyn std::error::Error>> {
        if self.raw_kpk == 0 { return Ok(0); }
        if code < self.raw_min { return Ok(0); }
        let idx_base = (code - self.raw_min) as usize * self.raw_kpk;
        if idx_base >= self.raw_keymap.len() { return Ok(0); }
        let shift = state & u16::from(ModMask::SHIFT) != 0;
        let lock  = state & u16::from(ModMask::LOCK)  != 0;
        let col   = if shift ^ lock { 1 } else { 0 };
        let col   = col.min(self.raw_kpk.saturating_sub(1));
        Ok(self.raw_keymap.get(idx_base + col).copied().unwrap_or(0))
    }

    pub fn cleanup(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);
        let _ = self.conn.destroy_window(self.check_win);
        for mon in &self.engine.state.monitors {
            if let (Some(bw), Some(gc)) = (mon.bar_win, mon.bar_gc) {
                let _ = self.conn.free_gc(gc);
                let _ = self.conn.destroy_window(bw);
            }
        }
        self.conn.flush()?;
        Ok(())
    }
}

// ── Free functions ─────────────────────────────────────────────────────────────

fn check_no_other_wm(conn: &RustConnection, root: Window)
    -> Result<(), Box<dyn std::error::Error>>
{
    conn.change_window_attributes(root,
        &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
    )?.check().map_err(|_| "another WM is already running")?;
    conn.flush()?;
    Ok(())
}

fn detect_monitors(
    conn: &RustConnection,
    screen: &Screen,
    cfg: &Cfg,
) -> Result<Vec<Monitor>, Box<dyn std::error::Error>> {
    use x11rb::protocol::randr::ConnectionExt as _;
    let bh = cfg.bar_height;
    let top = cfg.top_bar;
    let nt  = cfg.n_tags;

    if let Ok(reply) = conn.randr_get_monitors(screen.root, true)?.reply() {
        if !reply.monitors.is_empty() {
            return Ok(reply.monitors.iter().map(|m| {
                let r = Rect::new(m.x as i32, m.y as i32, m.width as u32, m.height as u32);
                Monitor::new(r, bh, top, nt)
            }).collect());
        }
    }
    let r = Rect::new(0, 0, screen.width_in_pixels as u32, screen.height_in_pixels as u32);
    Ok(vec![Monitor::new(r, bh, top, nt)])
}

fn build_keymap(cfg: &Cfg) -> BTreeMap<(u16, u32), Action> {
    cfg.keybinds.iter().map(|(m, k, a)| ((*m, *k), a.clone())).collect()
}

/// Fetch the full keysym table once and store it for zero-RTT lookups in on_key.
fn build_raw_keymap(conn: &RustConnection) -> Result<(Vec<u32>, usize, u8), Box<dyn std::error::Error>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    // Use u16 arithmetic to avoid u8 overflow when max - min + 1 == 256
    let count = (max as u16 - min as u16 + 1) as u8;
    let map = conn.get_keyboard_mapping(min, count)?.reply()?;
    let kpk = map.keysyms_per_keycode as usize;
    Ok((map.keysyms.to_vec(), kpk, min))
}

fn get_numlock(conn: &RustConnection) -> Result<u16, Box<dyn std::error::Error>> {
    let modmap = conn.get_modifier_mapping()?.reply()?;
    let setup  = conn.setup();
    let min    = setup.min_keycode;
    let max    = setup.max_keycode;
    // Use u16 arithmetic to avoid u8 overflow when max - min + 1 == 256
    let count  = (max as u16 - min as u16 + 1) as u8;
    let keymap = conn.get_keyboard_mapping(min, count)?.reply()?;
    let kpk    = keymap.keysyms_per_keycode as usize;
    if kpk == 0 { return Ok(0); }
    let kpm    = modmap.keycodes_per_modifier() as usize;
    if kpm == 0 { return Ok(0); }
    const XK_NUM_LOCK: u32 = 0xff7f;
    for (i, codes) in modmap.keycodes.chunks(kpm).enumerate() {
        for &code in codes {
            if code == 0 || code < min || code > max { continue; }
            let idx = (code - min) as usize * kpk;
            for j in 0..kpk {
                if keymap.keysyms[idx + j] == XK_NUM_LOCK { return Ok(1 << i); }
            }
        }
    }
    Ok(0)
}

fn keysym_to_codes(
    map: &x11rb::protocol::xproto::GetKeyboardMappingReply,
    min: u8, max: u8, kpk: usize, keysym: u32,
) -> Vec<u8> {
    map.keysyms.chunks(kpk).enumerate()
        .filter(|(_, syms)| syms.contains(&keysym))
        .map(|(i, _)| min + i as u8)
        .filter(|&c| c <= max)
        .collect()
}

fn mod_variants(numlock: u16) -> [u16; 4] {
    let lock = u16::from(ModMask::LOCK);
    [0, numlock, lock, numlock | lock]
}

#[inline]
fn normalize_ksym(k: u32) -> u32 {
    if (0x41..=0x5a).contains(&k) { k + 0x20 } else { k }
}

#[inline]
fn clean_mask(state: u16, numlock: u16) -> u16 {
    let lock: u16 = ModMask::LOCK.into();
    state & !(numlock | lock)
        & (u16::from(ModMask::SHIFT)
            | u16::from(ModMask::CONTROL)
            | u16::from(ModMask::M1)
            | u16::from(ModMask::M2)
            | u16::from(ModMask::M3)
            | u16::from(ModMask::M4)
            | u16::from(ModMask::M5))
}
