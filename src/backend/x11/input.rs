//! Root setup, XKB, key/button grabs.
//!
//! # Root window
//!
//! `setup_root` sets the root event mask to
//! `SUBSTRUCTURE_REDIRECT|SUBSTRUCTURE_NOTIFY|BUTTON_PRESS|
//! POINTER_MOTION|ENTER_WINDOW|STRUCTURE_NOTIFY|PROPERTY_CHANGE`,
//! publishes `_NET_SUPPORTED`, sets `_NET_SUPPORTING_WM_CHECK`
//! on `root+check_win`, sets the `b"maverick"` name, sets
//! `net_number`/`current_desktop`, grabs keys, sets up XKB,
//! and enables `RandR` `randr_select_input`.
//!
//! # XKB
//!
//! `setup_xkb` requests `NEW_KEYBOARD_NOTIFY|MAP_NOTIFY` and
//! `GROUP_STATE` events. The unified resolver reads all XKB key types
//! and symbols; keymap entries are normalised to lowercase.
//!
//! # Key grabs
//!
//! `plan_key_grabs` calls the same `resolve_key` path as `KeyPress`
//! dispatch. It derives the exact core state for every XKB level,
//! including `LevelThree`/`AltGr` and keypad `NumLock` selection.
//! `grab_buttons` installs a catch-all `SYNC`/`ASYNC` grab on managed
//! windows (pointer freeze until `AllowEvents`; keyboard must be
//! `ASYNC` or shortcuts freeze).
//!
//! # Modifiers
//!
//! `clean_mask` strips XKB group and configured lock bits. The
//! resolver separately records the exact level selector mask, including
//! real modifiers such as Mod5 for `LevelThree`.

use super::*;

// Observability macro for the `input-trace` feature.
// Defined only when its feature is on: every call site is behind the same
// `cfg`, so a build without the feature compiles none of them and has no
// use for the macro at all.
#[cfg(feature = "input-trace")]
macro_rules! itrace {
    ($($arg:tt)*) => {{
        eprintln!("[INPUT-TRACE] {}", format!($($arg)*));
    }};
}
// Observability macro for the `window-trace` feature.
// Defined only when its feature is on: every call site is behind the same
// `cfg`, so a build without the feature compiles none of them and has no
// use for the macro at all.
#[cfg(feature = "window-trace")]
macro_rules! wtrace {
    ($($arg:tt)*) => {{
        eprintln!("[WINDOW-TRACE] {}", format!($($arg)*));
    }};
}
impl WindowManager {
    pub(super) fn setup_root(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let a = &self.atoms;
        self.conn
            .change_window_attributes(
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
            )?
            .check()?;

        let supported = a.supported_list();
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_supported,
                AtomEnum::ATOM,
                &supported,
            )?
            .check()?;

        // EWMH: set _NET_SUPPORTING_WM_CHECK on both root and check_win (once each)
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_supporting_wm_check,
                AtomEnum::WINDOW,
                &[self.check_win],
            )?
            .check()?;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.check_win,
                a.net_supporting_wm_check,
                AtomEnum::WINDOW,
                &[self.check_win],
            )?
            .check()?;

        self.conn
            .change_property8(
                PropMode::REPLACE,
                self.check_win,
                a.net_wm_name,
                a.utf8_string,
                b"maverick",
            )?
            .check()?;

        let n = self.engine.cfg.n_tags as u32;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_number_of_desktops,
                AtomEnum::CARDINAL,
                &[n],
            )?
            .check()?;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                a.net_current_desktop,
                AtomEnum::CARDINAL,
                &[0u32],
            )?
            .check()?;

        self.update_ewmh_desktops()?;
        self.grab_keys()?;
        self.setup_xkb();

        // Subscribe to RandR change events so hotplug / resolution changes are
        // handled even when the server does not deliver a root ConfigureNotify.
        // Ask for the screen, crtc, output and output-property changes only —
        // anything else (providers, leases) is irrelevant to us.
        use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
        let rr_mask = NotifyMask::from(
            u16::from(NotifyMask::SCREEN_CHANGE)
                | u16::from(NotifyMask::CRTC_CHANGE)
                | u16::from(NotifyMask::OUTPUT_CHANGE)
                | u16::from(NotifyMask::OUTPUT_PROPERTY),
        );
        let _ = self.conn.randr_select_input(self.root, rr_mask);
        Ok(())
    }

    pub(super) fn active_layout(&self) -> ActiveLayout<'_> {
        ActiveLayout {
            keysyms: &self.raw_keymap,
            min: self.raw_min,
            kpk: self.raw_kpk,
            xkb: self.xkb.as_ref(),
            group: self.xkb_group,
            numlock: self.numlock,
            scroll: self.scroll,
        }
    }

    /// Subscribe to XKB keyboard-change events. Best-effort: without XKB the WM
    /// still sees core `MappingNotify`, it just misses the remaps the server
    /// reports only through XKB.
    ///
    /// `StateNotify` is selected for `GROUP_STATE` changes: the resolver stores
    /// every XKB group, but a group switch still has to reach the passive-grab
    /// planner, and a rebuild is the only thing that can do that.
    ///
    /// `XkbGetMap(KEY_TYPES | KEY_SYMS)` is always read inside the fixed
    /// `Setup.min_keycode..=max_keycode` range. A server cannot change the
    /// keycode range of an established connection, so `XkbNewKeyboardNotify`'s
    /// range must never be used for the request.
    pub(super) fn setup_xkb(&self) {
        use x11rb::protocol::xkb::{ConnectionExt as _, EventType, SelectEventsAux, ID};

        crate::log::config_trace("xkb_init_start", format_args!("phase=subscription"));
        let supported = match self.conn.xkb_use_extension(1, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply.supported,
                Err(e) => {
                    log::info!("XKB: UseExtension failed ({e}) — core MappingNotify only");
                    crate::log::config_trace(
                        "xkb_init_end",
                        format_args!("phase=subscription status=extension_reply_failed error={e}"),
                    );
                    return;
                }
            },
            Err(e) => {
                log::info!("XKB: extension unavailable ({e}) — core MappingNotify only");
                crate::log::config_trace(
                    "xkb_init_end",
                    format_args!("phase=subscription status=extension_request_failed error={e}"),
                );
                return;
            }
        };
        if !supported {
            log::info!(
                "XKB: server reports the extension as unsupported — core MappingNotify only"
            );
            crate::log::config_trace(
                "xkb_init_end",
                format_args!("phase=subscription status=unsupported"),
            );
            return;
        }

        let events =
            EventType::NEW_KEYBOARD_NOTIFY | EventType::MAP_NOTIFY | EventType::STATE_NOTIFY;
        let res = self.conn.xkb_select_events(
            ID::USE_CORE_KBD.into(),
            0u16.into(),
            events,
            0u16.into(),
            0u16.into(),
            &SelectEventsAux::new(),
        );
        match res {
            Ok(cookie) => {
                if let Err(e) = cookie.check() {
                    log::info!("XKB: SelectEvents rejected ({e}) — core MappingNotify only");
                    crate::log::config_trace(
                        "xkb_init_end",
                        format_args!(
                            "phase=subscription status=rejected events={events:?} error={e}"
                        ),
                    );
                } else {
                    crate::log::config_trace(
                        "xkb_init_end",
                        format_args!("phase=subscription status=ok events={events:?}"),
                    );
                }
            }
            Err(e) => {
                log::info!("XKB: SelectEvents failed ({e}) — core MappingNotify only");
                crate::log::config_trace(
                    "xkb_init_end",
                    format_args!(
                        "phase=subscription status=request_failed events={events:?} error={e}"
                    ),
                );
            }
        }
    }

    /// Rebuild every key grab from the current config and keymap.
    ///
    /// Grabs and dispatch share `ActiveLayout::resolve_key`: XKB-backed
    /// layouts use key types/groups/levels, while the explicit fallback keeps
    /// the legacy core columns 0/1. Every planned mask is therefore validated
    /// through the same key translation used by `on_key`.
    pub(super) fn grab_keys(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        crate::log::config_trace(
            "grabs_start",
            format_args!(
                "kind=passive_key root={:#x} numlock={:#x} scroll={:#x}",
                self.root, self.numlock, self.scroll
            ),
        );
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);

        if self.raw_kpk == 0 && self.xkb.is_none() {
            crate::log::config_trace(
                "grabs_end",
                format_args!("kind=passive_key status=skipped_empty_keymap"),
            );
            return Ok(());
        }

        let binds: Vec<(u16, u32)> = self
            .engine
            .cfg
            .keybinds
            .iter()
            .map(|(mask, keysym, _)| (*mask, *keysym))
            .collect();
        let plan = plan_key_grabs(&binds, self.active_layout());

        // Diagnostics are collected, not logged inline: see the dedup at the
        // end of the function.
        let mut warnings: Vec<String> = Vec::new();
        for (mask, keysym) in &plan.missing {
            warnings.push(format!(
                "keybinding {}: that keysym does not exist in the current layout — ignored",
                bind_name(*mask, *keysym)
            ));
        }

        for (mask, keysym, code) in &plan.grabs {
            match self.conn.grab_key(
                true,
                self.root,
                (*mask).into(),
                *code,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            ) {
                Ok(cookie) => {
                    if let Err(e) = cookie.check() {
                        warnings.push(format!(
                            "keybinding {}: grab rejected ({}) — is another client already holding the shortcut?",
                            bind_name(*mask, *keysym),
                            x_error_kind(&e)
                        ));
                        crate::log::config_trace("key_grab", format_args!("kind=passive root={:#x} keycode={code} mask={mask:#x} keysym={keysym:#x} status=rejected error={e}", self.root));
                    } else {
                        crate::log::config_trace("key_grab", format_args!("kind=passive root={:#x} keycode={code} mask={mask:#x} keysym={keysym:#x} status=confirmed owner_events=true pointer_mode=ASYNC keyboard_mode=ASYNC", self.root));
                    }
                }
                Err(e) => {
                    warnings.push(format!(
                        "keybinding {}: grab request failed ({e})",
                        bind_name(*mask, *keysym)
                    ));
                    crate::log::config_trace("key_grab", format_args!("kind=passive root={:#x} keycode={code} mask={mask:#x} keysym={keysym:#x} status=request_failed error={e}", self.root));
                }
            }
        }

        // Grabs are rebuilt on every keyboard change, and a broken bind stays
        // broken across all of them: log the complaint when it appears (or
        // changes), not once per rebuild.
        if warnings != self.last_grab_warnings {
            for w in &warnings {
                log::warn!("{w}");
            }
            self.last_grab_warnings = warnings;
        }

        crate::log::config_trace(
            "grabs_end",
            format_args!(
                "kind=passive_key xkb={} xkb_group={} planned={} missing={} warnings={:?}",
                if self.xkb.is_some() {
                    "enabled"
                } else {
                    "core-fallback"
                },
                self.xkb_group,
                plan.grabs.len(),
                plan.missing.len(),
                self.last_grab_warnings
            ),
        );
        Ok(())
    }

    /// Install every passive button grab a managed window needs, once.
    ///
    /// The set depends on the window's existence and on the lock-modifier map
    /// (`refresh_keyboard` re-runs this when NumLock/ScrollLock move), and on
    /// nothing else — in particular not on which window has focus. It used to
    /// be re-issued from `focus()` and `unfocus()`: an ungrab plus ~18 grabs
    /// per call, several calls per wheel notch, for a result identical to what
    /// the window already had. Like dwm, the grabs are placed at manage time
    /// and left alone.
    ///
    /// What is grabbed, and why nothing more:
    ///
    /// * Buttons 1-3, any modifier, `SYNC`: click-to-focus. `on_button_press`
    ///   decides, then releases the frozen pointer (replay to the client, or
    ///   discard for a consumed gesture).
    /// * `Mod4` + buttons 1/3, `ASYNC`, with pointer motion: move/resize drag.
    /// * `Mod4` + buttons 4-7, `SYNC`: the wheel gesture that scrolls the
    ///   camera.
    ///
    /// A plain wheel notch (or a side button) is deliberately *not* grabbed.
    /// The handler only ever replayed it to the client, so grabbing it cost a
    /// freeze, a round trip through this single-threaded loop and a replay per
    /// notch for scrolling that never needed the WM — the dominant cost of
    /// ordinary scrolling under the old catch-all `AnyButton` grab.
    ///
    /// Grab order is preserved from the catch-all era: the server tries a
    /// window's grabs newest-first, so the `Mod4` grabs added last still win
    /// over the click grab for the same press.
    pub(super) fn grab_buttons(&self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_button(ButtonIndex::ANY, win, ModMask::ANY);
        let motion =
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;

        // SYNC on every window, focused or not: `on_button_press` releases the
        // pointer through `allow_events(REPLAY_POINTER)`, which the server
        // rejects with BadValue unless the pointer is actually frozen.
        //
        // keyboard_mode MUST be ASYNC. With SYNC/SYNC every matching press
        // freezes *both* devices, but `on_button_press` only ever issues a
        // pointer `AllowEvents` mode, so the keyboard would stay frozen for the
        // rest of the session — every shortcut dead.
        for btn in [ButtonIndex::M1, ButtonIndex::M2, ButtonIndex::M3] {
            let _ = self.conn.grab_button(
                false,
                win,
                EventMask::BUTTON_PRESS,
                GrabMode::SYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                btn,
                ModMask::ANY,
            );
        }

        #[cfg(feature = "input-trace")]
        itrace!(
            "grab_buttons win={:#x}: installed SYNC BUTTON_PRESS grab on buttons 1-3 (pointer FREEZES on each press until allow_events runs)",
            win
        );
        #[cfg(feature = "window-trace")]
        wtrace!(
            "grab_buttons win={:#x}: input-grab installed (focus-on-click path for clients that grab focus, e.g. Firefox/Minecraft)",
            win
        );

        // Mod4 + drag: ASYNC/ASYNC (a SYNC keyboard mode here would freeze the
        // keyboard the moment a Mod+drag starts, and nothing would release it).
        let sup: u16 = ModMask::M4.into();
        for extra in mod_variants(self.numlock, self.scroll) {
            let m = (sup | extra).into();
            for btn in [ButtonIndex::M1, ButtonIndex::M3] {
                let _ = self.conn.grab_button(
                    false,
                    win,
                    motion,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    btn,
                    m,
                );
            }
            // Mod4 + wheel (4 up, 5 down, 6 left, 7 right), SYNC pointer so the
            // press can be consumed (`ASYNC_POINTER`) instead of also scrolling
            // the client. Keyboard stays ASYNC for the reason above.
            for wheel in 4u8..=7 {
                let _ = self.conn.grab_button(
                    false,
                    win,
                    EventMask::BUTTON_PRESS,
                    GrabMode::SYNC,
                    GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    ButtonIndex::from(wheel),
                    m,
                );
            }
        }
        Ok(())
    }
}

/// Core-mapping lookup retained only for servers without XKB.
pub(crate) fn keysym_at_col(keysyms: &[u32], min: u8, kpk: usize, code: u8, col: usize) -> u32 {
    if kpk == 0 || code < min {
        return 0;
    }
    let idx_base = usize::from(code - min) * kpk;
    if idx_base >= keysyms.len() {
        return 0;
    }
    let col = col.min(kpk - 1);
    keysyms.get(idx_base + col).copied().unwrap_or(0)
}
