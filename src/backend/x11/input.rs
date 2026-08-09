use super::*;

impl WindowManager {
    pub(super) fn setup_root(&self) -> Result<(), Box<dyn std::error::Error>> {
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

    pub(super) fn grab_keys(&self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.conn.ungrab_key(0u8, self.root, ModMask::ANY);

        // P7: Use cached keyboard mapping instead of fetching it again
        let kpk = self.raw_kpk;
        if kpk == 0 {
            return Ok(());
        }
        let min = self.raw_min;

        for (mask, keysym, _) in &self.engine.cfg.keybinds {
            for code in keysym_to_codes(&self.raw_keymap, min, kpk, *keysym) {
                for extra in mod_variants(self.numlock) {
                    let _ = self.conn.grab_key(
                        true,
                        self.root,
                        (mask | extra).into(),
                        code,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    );
                }
            }
        }
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
