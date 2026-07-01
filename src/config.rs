// maverick/src/config.rs

use crate::types::{Action, Dir, LayoutKind};

#[derive(Debug, Clone)]
pub struct Cfg {
    pub border_w: u32,
    pub gaps: u32,
    pub bar_height: u32,
    pub top_bar: bool,
    pub n_tags: usize,
    pub default_col_w: u32, // default width of a new column
    pub split_bias: f32,    // how much extra height focused row gets (0.0-1.0)
    pub focus_mouse: bool,
    pub warp_cursor: bool,

    // Catppuccin Mocha
    pub col_normal: u32, // 0xRRGGBB
    pub col_focused: u32,
    pub col_urgent: u32,
    pub col_bar_bg: u32,
    pub col_bar_fg: u32,
    pub col_bar_sel: u32, // selected workspace highlight
    pub col_bar_occ: u32, // occupied workspace dot

    pub tag_names: Vec<&'static str>,
    pub keybinds: Vec<(u16, u32, Action)>,
    pub rules: Vec<Rule>,

    // ── Startup orchestration ─────────────────────────────────────────────────
    /// Compositor command — launched BEFORE the WM initialises so every window
    /// gets compositing from its very first rendered frame.
    /// Empty = no compositor.
    pub compositor: Vec<String>,
    /// Milliseconds to wait after the compositor spawns.
    /// Gives picom time to attach to the root before the WM starts tiling.
    pub compositor_delay_ms: u64,
    /// Optional WAV/OGG chime played once the WM is fully ready.
    /// Tried via paplay → pw-play → aplay. None = silent startup.
    pub startup_sound: Option<String>,
    /// Programs launched after compositor + WM are both ready.
    pub autostart: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub class: Option<&'static str>,
    pub title: Option<&'static str>,
    pub float: bool,
    pub ws: Option<usize>,
}

impl Rule {
    pub fn matches(&self, class: &str, title: &str) -> bool {
        self.class.is_none_or(|c| class.to_lowercase().contains(c))
            && self.title.is_none_or(|t| title.to_lowercase().contains(t))
    }
}

pub fn load_config() -> Cfg {
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
        (sup, k!(b'b'), Action::ToggleBar),
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
        (sup, k!(b'm'), Action::SetLayout(LayoutKind::Monocle)),
        (sup, k!(b'g'), Action::SetLayout(LayoutKind::Grid)),
        (sup, k!(b't'), Action::SetLayout(LayoutKind::Column)),
        // ── misc ──
        (shs, k!(b'q'), Action::QuitConfirm), // Mod4+Shift+Q — quit
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
        gaps: 6,
        bar_height: 22,
        top_bar: true,
        n_tags: 9,
        default_col_w: 700,
        split_bias: 0.6,
        focus_mouse: false,
        warp_cursor: false,

        // Catppuccin Mocha palette
        col_normal: 0x45475a,  // Surface1
        col_focused: 0x89b4fa, // Blue
        col_urgent: 0xf38ba8,  // Red
        col_bar_bg: 0x1e1e2e,  // Base
        col_bar_fg: 0xcdd6f4,  // Text
        col_bar_sel: 0x89b4fa, // Blue (selected ws)
        col_bar_occ: 0xa6e3a1, // Green (occupied ws)

        tag_names: ["1", "2", "3", "4", "5", "6", "7", "8", "9"].to_vec(),

        keybinds,

        rules: vec![
            Rule {
                class: Some("xdg-desktop-portal"),
                title: None,
                float: true,
                ws: None,
            },
            Rule {
                class: Some("gpick"),
                title: None,
                float: true,
                ws: None,
            },
            Rule {
                class: Some("pinentry"),
                title: None,
                float: true,
                ws: None,
            },
            Rule {
                class: None,
                title: Some("file upload"),
                float: true,
                ws: None,
            },
            Rule {
                class: None,
                title: Some("open file"),
                float: true,
                ws: None,
            },
            Rule {
                class: None,
                title: Some("save file"),
                float: true,
                ws: None,
            },
            Rule {
                class: None,
                title: Some("qt file dialog"),
                float: true,
                ws: None,
            },
        ],

        // ── Compositor ────────────────────────────────────────────────────────
        // Picom launches BEFORE the WM so compositing is active from frame 0.
        //   --vsync                  prevent screen tearing
        //   --fade-*                 smooth open/close animations
        //   --active/inactive-opacity per-focus transparency
        //   --no-fading-openclose    apps appear instantly, only fade on close
        compositor: vec![
            "picom".into(),
            "--vsync".into(),
            "--fade-in-step".into(),
            "0.030".into(),
            "--fade-out-step".into(),
            "0.030".into(),
            "--fade-delta".into(),
            "8".into(),
            "--no-fading-openclose".into(),
            "--active-opacity".into(),
            "1.0".into(),
            "--inactive-opacity".into(),
            "0.87".into(),
            "--frame-opacity".into(),
            "1.0".into(),
        ],
        // Give picom 180 ms to attach to the composite overlay before the WM
        // starts managing windows.
        compositor_delay_ms: 180,

        // Set to Some("/path/to/sound.wav") to enable a startup chime.
        startup_sound: None,

        // ── Autostart ─────────────────────────────────────────────────────────
        // Programs launched AFTER compositor + WM are ready so every window
        // benefits from compositing from its very first frame.
        // Each entry is a command + args: vec!["binary", "arg1", "arg2", ...]
        autostart: vec![
            vec!["setxkbmap".into(), "us".into(), "-variant".into(), "dvorak".into()],
            vec!["feh".into(), "--bg-fill".into(), "/path/to/wallpaper.png".into()],
            vec!["alacritty".into()],
        ],
    }
}
