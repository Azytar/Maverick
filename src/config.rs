use crate::types::{Action, Dir, LayoutKind};

#[derive(Debug, Clone)]
pub struct Cfg {
    pub border_w: u32,
    /// Gap between windows within a column and between columns.
    pub gaps_inner: u32,
    /// Gap at the top/bottom screen edges (all 4 edges in Grid layout).
    pub gaps_outer: u32,
    /// Collapse gaps to 0 when a workspace has exactly one tiled window.
    pub smart_gaps: bool,
    /// Rounded corner radius in pixels, via X11 Shape. 0 disables.
    pub corner_radius: u32,
    pub n_tags: usize,
    pub default_col_w: u32, // default width of a new column
    pub split_bias: f32,    // how much extra height focused row gets (0.0-1.0)
    pub focus_mouse: bool,
    pub warp_cursor: bool,

    // Catppuccin Mocha
    pub col_normal: u32, // 0xRRGGBB
    pub col_focused: u32,
    pub col_urgent: u32,

    pub tag_names: Vec<String>,
    pub keybinds: Vec<(u16, u32, Action)>,
    pub rules: Vec<Rule>,

    /// Programs launched once the WM is ready. Compositor, bar, wallpaper,
    /// portals — maverick doesn't orchestrate any external tool specially,
    /// they're all just autostart entries.
    pub autostart: Vec<Vec<String>>,
}

impl Default for Cfg {
    /// Minimal config with no keybinds/rules — intended for tests and as a
    /// safe baseline. The real runtime config is built by `load_config`.
    fn default() -> Self {
        Cfg {
            border_w: 2,
            gaps_inner: 6,
            gaps_outer: 6,
            smart_gaps: false,
            corner_radius: 0,
            n_tags: 9,
            default_col_w: 700,
            split_bias: 0.6,
            focus_mouse: false,
            warp_cursor: false,
            col_normal: 0x45475a,
            col_focused: 0x89b4fa,
            col_urgent: 0xf38ba8,
            tag_names: (1..=9).map(|n| n.to_string()).collect(),
            keybinds: vec![],
            rules: vec![],
            autostart: vec![],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Rule {
    pub class: Option<String>,
    /// `WM_CLASS` instance part (e.g. `firefox`, `xterm`). Matched like class.
    pub instance: Option<String>,
    /// `_NET_WM_WINDOW_TYPE` atom name to match (lowercase): `dialog`,
    /// `utility`, `menu`, `toolbar`, `splash`, `desktop`, `dock`, `normal`.
    /// An empty/absent value means "any type".
    pub window_type: Option<String>,
    pub title: Option<String>,
    pub float: bool,
    /// Sticky float: always visible on every workspace of its monitor.
    pub sticky: bool,
    pub ws: Option<usize>,
    /// Forced floating size, in pixels — first priority, then the WM centering.
    pub size: Option<(u32, u32)>,
    /// Forced floating position, relative to the monitor's workarea origin.
    pub position: Option<(i32, i32)>,
    /// 0.0-1.0. Written at manage time as `_NET_WM_WINDOW_OPACITY` (no-op
    /// without a compositor). Applies to tiled and floating windows alike.
    pub opacity: Option<f32>,
    /// Override border width for this app — floating windows only;
    /// tiled/column geometry keeps one uniform border across the layout.
    pub border_w: Option<u32>,
}

impl Rule {
    /// Everything-criteria match. Any `None` criterion is a wildcard; every
    /// present one must be substring-occur in the client's data (all compared
    /// case-insensitively, on both sides, like the original class/title rule).
    pub fn matches(&self, class: &str, instance: &str, types: &[String], title: &str) -> bool {
        let class_lower = class.to_lowercase();
        let instance_lower = instance.to_lowercase();
        let title_lower = title.to_lowercase();
        self.class
            .as_deref()
            .is_none_or(|c| class_lower.contains(&c.to_lowercase()))
            && self
                .instance
                .as_deref()
                .is_none_or(|i| instance_lower.contains(&i.to_lowercase()))
            && self.window_type.as_deref().is_none_or(|t| {
                let t = t.to_lowercase();
                types.iter().any(|ty| ty == &t)
            })
            && self
                .title
                .as_deref()
                .is_none_or(|t| title_lower.contains(&t.to_lowercase()))
    }
}
/// Build the compiled baseline configuration — the exact same values Maverick
/// has always shipped with. This is the fallback whenever no user TOML exists
/// or it fails to load, and the starting point that a valid TOML overrides.
pub fn compiled_config() -> Cfg {
    use x11rb::protocol::xproto::ModMask;

    let sup: u16 = ModMask::M4.into();
    let shs: u16 = u16::from(ModMask::M4) | u16::from(ModMask::SHIFT);
    let sct: u16 = u16::from(ModMask::M4) | u16::from(ModMask::CONTROL);

    // XK_ keysym constants (X11 keysym values)
    const XK_RETURN: u32 = 0xff0d;
    const XK_SPACE: u32 = 0x0020;
    const XK_F5: u32 = 0xffc2;
    const XK_TAB: u32 = 0xff09;
    // letter keysyms: lowercase ascii
    macro_rules! k {
        ($c:literal) => {
            $c as u32
        };
    }

    let mut keybinds: Vec<(u16, u32, Action)> = vec![
        // ── spawn ──
        (sup, XK_RETURN, Action::Spawn(vec!["alacritty".into()])),
        (
            shs,
            k!(b'p'),
            Action::Spawn(vec!["rofi".into(), "-show".into(), "run".into()]),
        ),
        (
            sup,
            k!(b'p'),
            Action::Spawn(vec!["rofi".into(), "-show".into(), "drun".into()]),
        ),
        // ── window ops ──
        (shs, k!(b'c'), Action::Kill), // Mod4+Shift+C — close focused window
        (shs, XK_SPACE, Action::ToggleFloat),
        (shs, k!(b'f'), Action::ToggleFullscreen),
        // ── focus navigation ──
        (sup, k!(b'h'), Action::FocusDir(Dir::Left)),
        (sup, k!(b'l'), Action::FocusDir(Dir::Right)),
        (sup, k!(b'j'), Action::FocusDir(Dir::Down)),
        (sup, k!(b'k'), Action::FocusDir(Dir::Up)),
        // ── window movement ──
        (shs, k!(b'h'), Action::MoveDir(Dir::Left)),
        (shs, k!(b'l'), Action::MoveDir(Dir::Right)),
        (shs, k!(b'j'), Action::MoveDir(Dir::Down)),
        (shs, k!(b'k'), Action::MoveDir(Dir::Up)),
        // ── column ops ──
        (shs, XK_RETURN, Action::NewColumn),
        (sct, k!(b'h'), Action::GrowCol(-50)),
        (sct, k!(b'l'), Action::GrowCol(50)),
        (sct, k!(b'j'), Action::CollapseColumn),
        // ── layout ──
        (sup, XK_SPACE, Action::CycleLayout),
        (sup, k!(b'g'), Action::SetLayout(LayoutKind::Grid)),
        (sup, k!(b't'), Action::SetLayout(LayoutKind::Column)),
        // ── misc ──
        // Mod4+Shift+Q asks maverickctl for confirmation instead of quitting
        // outright, so a stray keypress can't kill the session. The raw
        // Action::Quit still exists and is reachable via the control socket.
        (
            shs,
            k!(b'q'),
            Action::Spawn(vec![
                "maverickctl".to_string(),
                "quit".to_string(),
                "--confirm".to_string(),
            ]),
        ),
        (shs, k!(b'r'), Action::Restart),
        (sup, XK_F5, Action::Restart),
        (sup, XK_TAB, Action::FocusMon(Dir::Next)),
        (shs, XK_TAB, Action::MoveMon(Dir::Next)),
    ];

    // ── workspace keybinds: Super+1..9 view, Super+Shift+1..9 move ──
    let ws_keys: [(u32, usize); 9] = [
        (k!(b'1'), 0),
        (k!(b'2'), 1),
        (k!(b'3'), 2),
        (k!(b'4'), 3),
        (k!(b'5'), 4),
        (k!(b'6'), 5),
        (k!(b'7'), 6),
        (k!(b'8'), 7),
        (k!(b'9'), 8),
    ];
    for (ksym, ws) in ws_keys {
        keybinds.push((sup, ksym, Action::View(ws)));
        keybinds.push((shs, ksym, Action::MoveToWs(ws)));
    }

        Cfg {
        border_w: 2,
        gaps_inner: 6,
        gaps_outer: 6,
        smart_gaps: false,
        corner_radius: 0,
        n_tags: 9,
        default_col_w: 700,
        split_bias: 0.6,
        focus_mouse: false,
        warp_cursor: false,

        // Catppuccin Mocha palette
        col_normal: 0x45475a,  // Surface1
        col_focused: 0x89b4fa, // Blue
        col_urgent: 0xf38ba8,  // Red

        tag_names: (1..=9).map(|n| n.to_string()).collect(),

        keybinds,

                rules: vec![
            Rule {
                class: Some("xdg-desktop-portal".into()),
                title: None,
                float: true,
                ws: None,
                ..Default::default()
            },
            Rule {
                class: Some("gpick".into()),
                title: None,
                float: true,
                ws: None,
                ..Default::default()
            },
            Rule {
                class: Some("pinentry".into()),
                title: None,
                float: true,
                ws: None,
                ..Default::default()
            },
            Rule {
                class: None,
                title: Some("file upload".into()),
                float: true,
                ws: None,
                ..Default::default()
            },
            Rule {
                class: None,
                title: Some("open file".into()),
                float: true,
                ws: None,
                ..Default::default()
            },
            Rule {
                class: None,
                title: Some("save file".into()),
                float: true,
                ws: None,
                ..Default::default()
            },
            Rule {
                class: None,
                title: Some("qt file dialog".into()),
                float: true,
                ws: None,
                ..Default::default()
            },
        ],

        // ── Autostart ─────────────────────────────────────────────────────────
        // Programs launched once the WM is ready. Each entry is a command +
        // args: vec!["binary", "arg1", "arg2", ...]. maverick doesn't treat
        // any of these specially — compositor included — it just spawns them.
        autostart: vec![
            // Not on $PATH by convention — Arch installs these under /usr/lib.
            // Without them, GTK/portal-based file pickers (e.g. browser upload
            // dialogs) silently fail to open.
            vec!["/usr/lib/xdg-desktop-portal-gtk".into()],
            vec!["/usr/lib/xdg-desktop-portal".into()],
            // Compositor, e.g.:
            // vec!["picom".into(), "--vsync".into()],
            // Launch an external status bar here. maverick reserves screen
            // space for it automatically via _NET_WM_STRUT_PARTIAL (see
            // backend/x11/struts.rs), so windows never overlap it. Example:
            // vec!["polybar".into(), "main".into()],
            // Set your own wallpaper here, e.g.:
            // vec!["feh".into(), "--bg-fill".into(), "/path/to/wallpaper.png".into()],
        ],
    }
}

/// Build the runtime config: the compiled baseline, with an optional user
/// TOML (`$XDG_CONFIG_HOME/maverick/config.toml`) layered on top. A missing or
/// invalid TOML never prevents startup — see `userconfig` for the fail-safe
/// loading rules.
pub fn load_config() -> Cfg {
    crate::userconfig::load_config()
}

/// Named color-theme presets for `[general].theme` in the TOML config.
/// Returns `(normal, focused, urgent)` as `0xRRGGBB`.
/// Returns `None` for an unknown theme name.
pub fn theme_palette(name: &str) -> Option<(u32, u32, u32)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "catppuccin-mocha" => (0x45475a, 0x89b4fa, 0xf38ba8),
        "catppuccin-latte" => (0xd9d9e0, 0x1e90ff, 0xd30066),
        "gruvbox" => (0xfbf1c7, 0x4c79a6, 0xea6962),
        "nord" => (0x4c566a, 0x81a1c1, 0xa3be8c),
        "dracula" => (0x282a36, 0xbd93f9, 0xff5555),
        "everforest" => (0x3c434e, 0x7fbbb3, 0xdb7070),
        "solarized" => (0x837c73, 0x268bd2, 0xdc322f),
        _ => return None,
    })
}

#[cfg(test)]
mod rule_tests {
    use super::Rule;

    /// Builder sugar for the tests: criterion helpers on a default Rule.
    trait RuleEx {
        fn class(self, c: &str) -> Self;
        fn instance(self, i: &str) -> Self;
        fn window_type(self, t: &str) -> Self;
        fn title(self, t: &str) -> Self;
    }
    impl RuleEx for Rule {
        fn class(mut self, c: &str) -> Self {
            self.class = Some(c.to_string());
            self
        }
        fn instance(mut self, i: &str) -> Self {
            self.instance = Some(i.to_string());
            self
        }
        fn window_type(mut self, t: &str) -> Self {
            self.window_type = Some(t.to_string());
            self
        }
        fn title(mut self, t: &str) -> Self {
            self.title = Some(t.to_string());
            self
        }
    }

    fn base() -> Rule {
        Rule::default()
    }

    #[test]
    fn no_criteria_matches_anything() {
        assert!(base().matches("Firefox", "", &["normal".into()], "My Blog"));
        assert!(base().matches("", "", &[], ""));
    }

    #[test]
    fn class_match_is_substring_case_insensitive() {
        let r = base().class("fire");
        assert!(r.matches("Firefox", "", &[], ""));
        assert!(!r.matches("chrome", "", &[], ""));
    }

    #[test]
    fn instance_match_uses_wm_class_instance_part() {
        let r = base().instance("term");
        assert!(r.matches("Alacritty", "xterm", &[], ""));
        assert!(!r.matches("Alacritty", "foot", &[], ""));
    }

    #[test]
    fn window_type_match_is_exact_and_lowercase() {
        let dialog = "dialog".to_string();
        let normal = "normal".to_string();
        let r = base().window_type("dialog");
        assert!(r.matches("x", "", &[dialog], ""));
        assert!(!r.matches("x", "", &[normal], ""));
        assert!(!r.matches("x", "", &[], ""));
    }

    #[test]
    fn title_match_is_substring_case_insensitive() {
        let r = base().title("find");
        assert!(r.matches("x", "", &[], "Search & Find"));
        assert!(!r.matches("x", "", &[], "Notepad"));
    }

    #[test]
    fn all_criteria_must_hold_together() {
        let r = base()
            .class("fire")
            .instance("navig")
            .window_type("email")
            .title("gmail");
        assert!(r.matches("Firefox", "Navigator", &["email".to_string()], "Gmail"));
        assert!(!r.matches("Firefox", "Navigator", &["normal".to_string()], "Gmail"));
        assert!(!r.matches("Chrome", "Navigator", &["email".to_string()], "Gmail"));
    }
}
