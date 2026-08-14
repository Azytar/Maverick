use super::*;

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

    /// Subscribe to XKB keyboard-change events. Best-effort: without XKB the WM
    /// still sees core `MappingNotify`, it just misses the remaps the server
    /// reports only through XKB.
    ///
    /// Note what is *not* selected: `StateNotify`. Under the strict-group-1
    /// policy the active group is irrelevant to grabs and dispatch, so
    /// subscribing would mean a full ungrab/regrab on every layout toggle for
    /// no behavioural gain.
    ///
    /// The keymap itself is still read with core `GetKeyboardMapping`, always
    /// clamped to `Setup.min_keycode..=max_keycode`: a server cannot change the
    /// keycode range of an established connection, and asking outside it is a
    /// `BadValue` — so the range carried by `XkbNewKeyboardNotify` must never be
    /// used for the request.
    pub(super) fn setup_xkb(&self) {
        use x11rb::protocol::xkb::{ConnectionExt as _, EventType, SelectEventsAux, ID};

        let supported = match self.conn.xkb_use_extension(1, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply.supported,
                Err(e) => {
                    log::info!("XKB: UseExtension failed ({e}) — core MappingNotify only");
                    return;
                }
            },
            Err(e) => {
                log::info!("XKB: extension unavailable ({e}) — core MappingNotify only");
                return;
            }
        };
        if !supported {
            log::info!(
                "XKB: server reports the extension as unsupported — core MappingNotify only"
            );
            return;
        }

        let events = EventType::NEW_KEYBOARD_NOTIFY | EventType::MAP_NOTIFY;
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
                }
            }
            Err(e) => log::info!("XKB: SelectEvents failed ({e}) — core MappingNotify only"),
        }
    }

    /// Rebuild every key grab from the current config and keymap.
    ///
    /// Grabs and dispatch must agree on which keysym a keycode "is", so both
    /// sides work on group 1 and share the same keysym-directed fallback (see
    /// `plan_key_grabs`). Anything grabbed here that `on_key` could not resolve
    /// would be a key stolen from the focused application, not a no-op.
    pub(super) fn grab_keys(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);
        self.code_bindings.clear();

        // P7: Use cached keyboard mapping instead of fetching it again
        let kpk = self.raw_kpk;
        if kpk == 0 {
            return Ok(());
        }
        let min = self.raw_min;

        let binds: Vec<(u16, u32)> = self
            .engine
            .cfg
            .keybinds
            .iter()
            .map(|(mask, keysym, _)| (*mask, *keysym))
            .collect();
        let plan = plan_key_grabs(&self.raw_keymap, min, kpk, &binds);

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
            // Base variant, checked. A rejection means another client already
            // owns the shortcut and the bind is simply dead — worth a warning,
            // and one round-trip per bind is an acceptable price on
            // startup/reload/mapping change.
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
                    }
                }
                Err(e) => warnings.push(format!(
                    "keybinding {}: grab request failed ({e})",
                    bind_name(*mask, *keysym)
                )),
            }

            // NumLock/CapsLock variants. Unchecked: they share the base
            // variant's destination, so a check would cost a round-trip without
            // adding information. Repeats are skipped — `mod_variants` yields
            // the same mask twice when NumLock is unmapped, and a duplicate
            // grab is a `BadAccess` that would show up as a phantom conflict.
            let mut done: Vec<u16> = vec![0];
            for extra in mod_variants(self.numlock) {
                if done.contains(&extra) {
                    continue;
                }
                done.push(extra);
                let _ = self.conn.grab_key(
                    true,
                    self.root,
                    (mask | extra).into(),
                    *code,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                );
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

        self.code_bindings = plan.code_bindings;
        Ok(())
    }

    pub(super) fn grab_buttons(
        &self,
        win: Window,
        _focused: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_button(ButtonIndex::ANY, win, ModMask::ANY);
        let motion =
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;

        // SYNC grab on ALL windows (not just unfocused).
        // Without this, allow_events(ReplayPointer) in on_button_press fails with
        // BadValue because pointer is not frozen → process::exit(1).
        //
        // keyboard_mode MUST be ASYNC here. With SYNC/SYNC, every matching
        // ButtonPress freezes *both* devices, but on_button_press only calls
        // allow_events(REPLAY_POINTER) — never a keyboard AllowEvents mode — so
        // the keyboard stayed frozen after clicking any managed window. That's
        // what broke shortcuts (and the app's own key input) for clients like
        // Firefox/Minecraft that grab focus on click.
        let _ = self.conn.grab_button(
            false,
            win,
            EventMask::BUTTON_PRESS,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            ButtonIndex::ANY,
            ModMask::ANY,
        );

        #[cfg(feature = "input-trace")]
        itrace!(
            "grab_buttons win={:#x}: installed SYNC BUTTON_PRESS grab (pointer FREEZES on every ButtonPress until allow_events runs)",
            win
        );
        #[cfg(feature = "window-trace")]
        wtrace!(
            "grab_buttons win={:#x}: input-grab installed (focus-on-click path for clients that grab focus, e.g. Firefox/Minecraft)",
            win
        );

        // keyboard_mode MUST be ASYNC here too, for the same reason as the
        // catch-all grab above: on_button_press never calls allow_events with
        // a keyboard mode, so a SYNC keyboard grab here freezes the keyboard
        // (all shortcuts, including focus-move and spawn keybinds) the moment
        // the user does a Mod+drag (move/resize) on any window, and it never
        // gets released.
        let sup: u16 = ModMask::M4.into();
        for extra in mod_variants(self.numlock) {
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
        }
        Ok(())
    }

    pub(super) fn keycode_to_keysym(
        &self,
        code: u8,
        _state: u16,
    ) -> Result<u32, Box<dyn std::error::Error>> {
        // B6: resolve the key by its column-0 keysym and let the Shift/Lock
        // modifiers travel only in the keymap's modifier mask. A shifted symbol
        // such as `Mod4+Shift+bracketleft` now resolves to the same entry a user
        // binds by name (`Mod4+Shift+bracketleft`) instead of mismatching it.
        Ok(self.keysym_at_col(code, 0))
    }

    /// Raw keysym lookup at a given keycode column (0 = unshifted). Used both for
    /// the column-0 primary lookup and the `on_key` fallback to the shifted
    /// column, so legacy behaviour (where the shifted keysym was the only one
    /// considered) still works when nothing matches column 0.
    pub(crate) fn keysym_at_col(&self, code: u8, col: usize) -> u32 {
        if self.raw_kpk == 0 {
            return 0;
        }
        if code < self.raw_min {
            return 0;
        }
        let idx_base = (code - self.raw_min) as usize * self.raw_kpk;
        if idx_base >= self.raw_keymap.len() {
            return 0;
        }
        let col = col.min(self.raw_kpk.saturating_sub(1));
        self.raw_keymap.get(idx_base + col).copied().unwrap_or(0)
    }
}
