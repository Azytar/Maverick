// maverick/src/backend/x11/mod.rs
// Window manager core — niri-style columnar layout, clean coords.

use std::collections::BTreeMap;

use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use crate::backend::atoms::Atoms;
use crate::config::Cfg;
use crate::core::layout::{arrange, ideal_scroll, Placements};
use crate::core::present::present;
use crate::core::{parse_action, state_json, Effect, Engine};
use crate::log;
use crate::types::*;

mod actions;
mod events;
mod ewmh;
mod hubevents;
mod input;
mod manage;
mod pointer;
mod render;
mod struts;
use pointer::DragState;

pub struct WindowManager {
    conn: RustConnection,
    screen_num: usize,
    root: Window,
    atoms: Atoms,
    pub engine: Engine,
    layout_registry: crate::core::layout::LayoutRegistry,
    check_win: Window,
    numlock: u16,
    keymap: BTreeMap<(u16, u32), crate::types::Action>,
    raw_keymap: Vec<u32>,
    raw_kpk: usize,
    raw_min: u8,
    drag: Option<DragState>,
    /// P5: Deferred _`NET_CLIENT_LIST` update. Set on manage/unmanage, flushed in event loop.
    client_list_dirty: bool,
    /// P9: Deferred restack — only restack when floats/fullscreen change.
    stack_dirty: bool,
    /// P12: Reusable buffers for `hide_offscreen` — avoids reallocation per arrange.
    hide_ws_set: std::collections::HashSet<Window>,
    hide_mon_vec: Vec<Window>,
    /// P10: Reusable placements buffer — avoids allocation per `arrange()` call.
    placements_buf: Placements,
    /// Rate-limit tracker for key repeat suppression (mods, keysym → last dispatch).
    last_key_times: std::collections::BTreeMap<(u16, u32), std::time::Instant>,
    /// Control socket server (identity + remote quit). None if it failed to start.
    control: Option<maverick_sys::ControlServer>,
    /// Instance name passed via --name (for identity/control).
    instance_name: String,
    /// Bridge to the control-socket thread: drains dispatched commands, publishes
    /// state snapshots, and emits events for `subscribe` clients.
    hub: Option<maverick_sys::ControlHub>,
    /// Last state snapshot published to the hub — avoids re-publishing identical
    /// JSON on every loop iteration.
    last_state_json: String,
    /// External dock windows we currently reserve space for, mapped to the
    /// monitor index whose `reserved_regions` hold their reservation. Used to
    /// remove the reservation exactly when the dock is destroyed/unmapped.
    docks: std::collections::HashMap<Window, usize>,
    /// When set, `EnterNotify`-driven focus (focus-follows-mouse) is ignored.
    /// Armed right after keyboard navigation and other programmatic focus
    /// changes so the pointer — parked over a tile edge — can't instantly undo
    /// the key-driven switch. Cleared by the first real `MotionNotify`.
    pointer_guard_until: Option<std::time::Instant>,
    /// Server time of the most recent input event (key/button/enter). Used to
    /// stamp ICCCM `WM_TAKE_FOCUS` messages with a real timestamp instead of
    /// `CurrentTime`, which a few strict toolkits (some Java/Emacs builds)
    /// refuse to act on.
    last_event_time: u32,
    /// Tiled window currently highlighted by the drag-to-tile preview (its
    /// border is painted `col_focused`). Reverted when the pointer moves away
    /// or the drag ends.
    drag_target: Option<Window>,
}

impl WindowManager {
    fn dispatch(&mut self, ev: x11rb::protocol::Event) -> Result<(), Box<dyn std::error::Error>> {
        match ev {
            Event::ButtonPress(e) => self.on_button_press(e)?,
            Event::ButtonRelease(e) => self.on_button_release(e)?,
            Event::ClientMessage(e) => self.on_client_message(e)?,
            Event::ConfigureNotify(e) => self.on_configure_notify(e)?,
            Event::ConfigureRequest(e) => self.on_configure_request(e)?,
            Event::DestroyNotify(e) => self.on_destroy(e)?,
            Event::EnterNotify(e) => self.on_enter(e)?,
            Event::KeyPress(e) => self.on_key(e)?,
            Event::MappingNotify(e) => self.on_mapping(e)?,
            Event::MapRequest(e) => self.on_map_request(e)?,
            Event::MotionNotify(e) => self.on_motion(e)?,
            Event::PropertyNotify(e) => self.on_property(e)?,
            Event::UnmapNotify(e) => self.on_unmap(e)?,
            // RandR change events (config/grab selected in `setup_root`): both
            // the 1.5 `NotifyEvent` (crtc/output changes) and the classic
            // `ScreenChangeNotifyEvent` funnel into the same re-detect handler as
            // a root ConfigureNotify would.
            Event::RandrNotify(_) | Event::RandrScreenChangeNotify(_) => {
                self.handle_monitor_change()?
            }
            _ => {}
        }
        Ok(())
    }
    pub fn cleanup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);

        // Restore root event mask: remove SUBSTRUCTURE_REDIRECT so that
        // the next WM doesn't fail on startup.
        let _ = self.conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        );

        // Ungrab buttons on all managed windows
        for win in self.engine.state.clients.keys() {
            let _ = self
                .conn
                .ungrab_button(ButtonIndex::ANY, *win, ModMask::ANY);
        }

        let _ = self
            .conn
            .delete_property(self.root, self.atoms.net_supporting_wm_check);
        let _ = self
            .conn
            .delete_property(self.root, self.atoms.net_active_window);
        let _ = self
            .conn
            .delete_property(self.root, self.atoms.net_client_list);
        let _ = self.conn.destroy_window(self.check_win);

        self.conn.flush()?;

        // Tear down the control socket + identity ficha so external tools stop
        // listing this (now dead) instance. The ControlServer thread stops when
        // its handle is dropped at the end of the process; explicitly remove the
        // on-disk meta here.
        if !self.instance_name.is_empty() {
            maverick_sys::identity::cleanup_meta(&self.instance_name);
        }
        drop(self.control.take());
        Ok(())
    }
    fn run_once(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::io::AsRawFd;

        // ── signal phase ─────────────────────────────────────────────────────────
        // SIGCONT (resume from stop) requests a key regrab; SIGTERM requests quit.
        // Both are set by the maverick-sys signal handlers (the only unsafe code).
        if maverick_sys::need_regrab() {
            if let Err(e) = self.grab_keys() {
                log::warn!("Failed to regrab keys: {e}");
            } else {
                maverick_sys::clear_regrab();
            }
        }
        if maverick_sys::quit_requested() {
            maverick_sys::clear_quit();
            self.engine.state.running = false;
            return Ok(());
        }

        // ── flush phase ─────────────────────────────────────────────────────────
        // Drain the deferred _NET_CLIENT_LIST update (if any manage/unmanage
        // marked it dirty) before blocking on the next event, so all X11
        // output from the previous event batch is flushed in one shot.
        self.flush_client_list()?;
        self.conn.flush()?;

        // ── wait phase ─────────────────────────────────────────────────────────
        // Block on the X socket but wake every 100ms so control-socket commands
        // (dispatch/quit/restart) are drained even when no X events arrive.
        let fd = self.conn.stream().as_raw_fd();
        maverick_sys::wait_readable(fd, std::time::Duration::from_millis(100));

        // ── drain phase ─────────────────────────────────────────────────────────
        // Non-blocking: process every event already in the socket buffer.
        // Firefox/pavucontrol can queue 100+ PropertyNotify events in a burst;
        // draining them here means bar_dirty is set once, not 100 times.
        while let Some(ev) = self.conn.poll_for_event()? {
            self.dispatch(ev)?;
        }

        // ── control phase ────────────────────────────────────────────────────────
        // Execute any commands from the control socket, then publish state.
        self.drain_control()?;
        self.publish_state();

        // Loop back → flush_client_list() rewrites _NET_CLIENT_LIST at most once per batch.
        Ok(())
    }
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        while self.engine.state.running {
            if let Err(e) = self.run_once() {
                return if is_x11_connection_loss(&*e) {
                    log::info!("maverick: X11 connection lost (X server disconnected)");
                    Ok(())
                } else {
                    Err(e)
                };
            }
        }
        Ok(())
    }
    pub fn new(cfg: Cfg, replace: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let (conn, screen_num) = RustConnection::connect(None)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let visual = screen.root_visual;

        log::info!(
            "maverick: X11 connected root={} {}x{}",
            root,
            screen.width_in_pixels,
            screen.height_in_pixels
        );

        let atoms = Atoms::new(&conn)?;
        if replace {
            if !claim_screen_replacing(&conn, root, &atoms)? {
                return Err(
                    "another WM is running and did not yield the screen (use --replace only when one is present)".into(),
                );
            }
            log::info!("maverick: replaced the previous WM (--replace)");
        } else {
            check_no_other_wm(&conn, root)?;
        }

        let monitors = detect_monitors(&conn, screen, &cfg)?;
        let mut engine = Engine::new(cfg);
        engine.state.monitors = monitors;

        // create EWMH check window
        let check_win = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            check_win,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )?
        .check()?;

        let ks = fetch_keyboard_state(&conn)?;
        let (raw_keymap, raw_kpk, raw_min, numlock) = (ks.keysyms, ks.kpk, ks.min, ks.numlock);
        let keymap = build_keymap(&engine.cfg);

        let mut wm = WindowManager {
            conn,
            screen_num,
            root,
            atoms,
            engine,
            layout_registry: crate::core::layout::LayoutRegistry::new(),
            check_win,
            numlock,
            keymap,
            raw_keymap,
            raw_kpk,
            raw_min,
            drag: None,
            client_list_dirty: false,
            stack_dirty: false,
            hide_ws_set: std::collections::HashSet::with_capacity(32),
            hide_mon_vec: Vec::with_capacity(64),
            placements_buf: Placements::with_capacity(32),
            last_key_times: std::collections::BTreeMap::new(),
            control: None,
            instance_name: String::new(),
            hub: None,
            last_state_json: String::new(),
            docks: std::collections::HashMap::new(),
            pointer_guard_until: None,
            last_event_time: 0,
            drag_target: None,
        };

        let _ = (depth, visual);

        wm.setup_root()?;
        wm.scan_windows()?;

        for i in 0..wm.engine.state.monitors.len() {
            wm.arrange(i)?;
        }

        wm.conn.flush()?;
        log::info!("maverick ready");
        Ok(wm)
    }
}

// ── Free functions ─────────────────────────────────────────────────────────────

/// Interpret a strut vector as a single (edge, thickness). Both `_NET_WM_STRUT`
/// (4 values) and `_NET_WM_STRUT_PARTIAL` (12 values) start with
/// `[left, right, top, bottom]`; we take the first non-zero edge.
fn strut_edge(v: &[u32]) -> Option<(Edge, u32)> {
    let (left, right, top, bottom) = (v[0], v[1], v[2], v[3]);
    if top > 0 {
        Some((Edge::Top, top))
    } else if bottom > 0 {
        Some((Edge::Bottom, bottom))
    } else if left > 0 {
        Some((Edge::Left, left))
    } else if right > 0 {
        Some((Edge::Right, right))
    } else {
        None
    }
}

fn is_x11_connection_loss(e: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        e.downcast_ref::<ConnectionError>(),
        Some(ConnectionError::IoError(_))
    )
}

fn check_no_other_wm(
    conn: &RustConnection,
    root: Window,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
    )?
    .check()
    .map_err(|_| "another WM is already running")?;
    conn.flush()?;
    Ok(())
}

fn grab_substructure(conn: &RustConnection, root: Window) -> bool {
    match conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
    ) {
        Ok(cookie) => cookie.check().is_ok(),
        Err(_) => false,
    }
}

/// `--replace` handover dance (dwm-style): try to grab
/// `SUBSTRUCTURE_REDIRECT` directly; if another WM holds it, find its
/// `_NET_SUPPORTING_WM_CHECK` window (EWMH 1.4 §WM Attributes) and politely
/// send it `WM_DELETE_WINDOW`, then retry the grab until it succeeds or the
/// timeout expires. The previous WM is never `SIGKILL`ed — it takes whatever
/// path its own `WM_DELETE` handler chooses, which is always a clean exit for
/// real WMs.
fn claim_screen_replacing(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
) -> Result<bool, Box<dyn std::error::Error>> {
    use x11rb::protocol::xproto::{ClientMessageData, ClientMessageEvent};

    if grab_substructure(conn, root) {
        return Ok(true);
    }
    log::info!("another WM owns the screen; asking it to leave");
    const ATTEMPTS: usize = 20;
    const SLEEP_MS: u64 = 150;
    for _ in 0..ATTEMPTS {
        if let Ok(cookie) = conn.get_property(
            false,
            root,
            atoms.net_supporting_wm_check,
            AtomEnum::WINDOW,
            0,
            1,
        ) {
            if let Ok(reply) = cookie.reply() {
                if let Some(win) = reply.value32().and_then(|mut v| v.next()) {
                    if win != 0 && win != x11rb::NONE {
                        let ev = ClientMessageEvent {
                            response_type: CLIENT_MESSAGE_EVENT,
                            format: 32,
                            sequence: 0,
                            window: win,
                            type_: atoms.wm_protocols,
                            data: ClientMessageData::from([
                                atoms.wm_delete_window,
                                x11rb::CURRENT_TIME,
                                0,
                                0,
                                0,
                            ]),
                        };
                        let _ = conn.send_event(false, win, EventMask::NO_EVENT, ev);
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
        if grab_substructure(conn, root) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn detect_monitors(
    conn: &RustConnection,
    screen: &Screen,
    cfg: &Cfg,
) -> Result<Vec<Monitor>, Box<dyn std::error::Error>> {
    use x11rb::protocol::randr::ConnectionExt as _;
    let nt = cfg.n_tags;

    if let Ok(reply) = conn.randr_get_monitors(screen.root, true)?.reply() {
        if !reply.monitors.is_empty() {
            return Ok(reply
                .monitors
                .iter()
                .map(|m| {
                    let r = Rect::new(m.x as i32, m.y as i32, m.width as u32, m.height as u32);
                    Monitor::new(r, nt)
                })
                .collect());
        }
    }
    let r = Rect::new(
        0,
        0,
        screen.width_in_pixels as u32,
        screen.height_in_pixels as u32,
    );
    Ok(vec![Monitor::new(r, nt)])
}

fn build_keymap(cfg: &Cfg) -> BTreeMap<(u16, u32), Action> {
    cfg.keybinds
        .iter()
        .map(|(m, k, a)| ((*m, *k), a.clone()))
        .collect()
}

/// Result of a pipelined keyboard+modifier state fetch.
struct KeyboardState {
    keysyms: Vec<u32>,
    kpk: usize,
    min: u8,
    numlock: u16,
}

/// P2: Pipelined keyboard+modifier state — fire both requests, then collect both replies.
/// 2 RTTs → 1.
fn fetch_keyboard_state(
    conn: &RustConnection,
) -> Result<KeyboardState, Box<dyn std::error::Error>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let count = (max as u16 - min as u16 + 1) as u8;

    let c_kb = conn.get_keyboard_mapping(min, count)?;
    let c_mod = conn.get_modifier_mapping()?;

    let map = c_kb.reply()?;
    let kpk = map.keysyms_per_keycode as usize;
    let keysyms = map.keysyms.clone();

    let numlock = if let Ok(modmap) = c_mod.reply() {
        let kpm = modmap.keycodes_per_modifier() as usize;
        compute_numlock(&modmap.keycodes, kpm, &keysyms, kpk, min, max)
    } else {
        0
    };

    Ok(KeyboardState {
        keysyms,
        kpk,
        min,
        numlock,
    })
}

/// Search for `NumLock` keysym in the modifier mapping.
fn compute_numlock(
    keycodes: &[u8],
    kpm: usize,
    keysyms: &[u32],
    kpk: usize,
    min: u8,
    max: u8,
) -> u16 {
    if kpk == 0 || kpm == 0 {
        return 0;
    }
    const XK_NUM_LOCK: u32 = 0xff7f;
    for (i, codes) in keycodes.chunks(kpm).enumerate() {
        for &code in codes {
            if code == 0 || code < min || code > max {
                continue;
            }
            let idx = (code - min) as usize * kpk;
            if (0..kpk).any(|j| keysyms[idx + j] == XK_NUM_LOCK) {
                return 1 << i;
            }
        }
    }
    0
}

fn keysym_to_codes(keysyms: &[u32], min: u8, kpk: usize, keysym: u32) -> Vec<u8> {
    keysyms
        .chunks(kpk)
        .enumerate()
        .filter(|(_, syms)| syms.contains(&keysym))
        .map(|(i, _)| min + i as u8)
        .collect()
}

/// Read a window title without needing a mutable Client reference.
/// P14: Fire both `net_wm_name` and `WM_NAME` requests before reading any reply.
fn read_title_value(
    conn: &RustConnection,
    win: Window,
    atoms: &Atoms,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let c_net = conn.get_property(false, win, atoms.net_wm_name, atoms.utf8_string, 0, 256);
    let c_wm = conn.get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256);

    if let Ok(c) = c_net {
        if let Ok(ref prop) = c.reply() {
            if !prop.value.is_empty() {
                return Ok(Some(String::from_utf8_lossy(&prop.value).into_owned()));
            }
        }
    }
    if let Ok(c) = c_wm {
        if let Ok(ref prop) = c.reply() {
            return Ok(Some(String::from_utf8_lossy(&prop.value).into_owned()));
        }
    }
    Ok(None)
}

type WmHints = (bool, bool, bool); // no_focus, wants_input, urgent

/// Read `WM_HINTS` flags without needing a mutable Client reference.
fn read_wm_hints_value(
    conn: &RustConnection,
    win: Window,
) -> Result<Option<WmHints>, Box<dyn std::error::Error>> {
    if let Ok(c) = conn.get_property(false, win, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9) {
        if let Ok(ref prop) = c.reply() {
            if let Some(vals) = prop.value32() {
                let v: Vec<u32> = vals.collect();
                if !v.is_empty() {
                    let no_focus = v[0] & 1 != 0 && v.len() > 1 && v[1] == 0;
                    let wants_input = if v[0] & 1 != 0 && v.len() > 1 {
                        v[1] != 0
                    } else {
                        true
                    };
                    let urgent = v[0] & 256 != 0;
                    return Ok(Some((no_focus, wants_input, urgent)));
                }
            }
        }
    }
    Ok(None)
}

#[inline]
fn mod_variants(numlock: u16) -> [u16; 4] {
    let lock = u16::from(ModMask::LOCK);
    [0, numlock, lock, numlock | lock]
}

#[inline]
fn normalize_ksym(k: u32) -> u32 {
    if (0x41..=0x5a).contains(&k) {
        k + 0x20
    } else {
        k
    }
}

#[inline]
fn clean_mask(state: u16, numlock: u16) -> u16 {
    let lock: u16 = ModMask::LOCK.into();
    state
        & !(numlock | lock)
        & (u16::from(ModMask::SHIFT)
            | u16::from(ModMask::CONTROL)
            | u16::from(ModMask::M1)
            | u16::from(ModMask::M2)
            | u16::from(ModMask::M3)
            | u16::from(ModMask::M4)
            | u16::from(ModMask::M5))
}
