//! Compiled configuration baseline and policy gates.
//!
//! Authority for two questions: *what the defaults are* (`compiled_config`) and
//! *which compositor backend this build may run* (`validate_compositor_backend`,
//! `compositor_enabled`). `load_config` is a thin delegate to `userconfig`,
//! which owns file I/O, TOML tokenization and diagnostics.
//!
//! Boundary: owns no I/O, no X connection, and no atom interning. The
//! `Cfg` type family (`Cfg`, `CompositorCfg`, `AnimationsCfg`, `WallpaperCfg`,
//! `Rule`, `CompositorBackend`, `VsyncMode`) is a plain owned value that the
//! caller clones; the WM owns it for the session.
//!
//! # Invariants
//!
//! `compositor_enabled` is gated on both `Cfg::compositor.enabled` and the
//! absence of `MAVERICK_NO_COMPOSITOR`, so the env var can veto a config that
//! asks for the compositor. `validate_compositor_backend` is a no-op when
//! `compositor-opengl` is not compiled and when the compositor is disabled, and
//! otherwise rejects a requested backend that this build cannot provide. There is
//! no Vulkan backend: the feature that once named one selected no crate and has
//! been removed.

use std::path::Path;

use crate::types::{Action, Dir, LayoutKind};

/// Root window-manager configuration, built from [`compiled_config`] and an
/// optional TOML overlay via [`load_config`].
///
/// This is the owned value passed to `Engine::new` and replaced wholesale on
/// reload; every other consumer borrows it. The three list fields (`keybinds`,
/// `rules`, `autostart`) are the exception to the field-level overlay: a user
/// file that declares any of them replaces the compiled list outright, so a
/// user keymap starts from scratch instead of layering on top of this one.
/// Every scalar is a pixel count, a fraction of the workarea, or an enum-like
/// flag, as documented per field.
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
    /// Width of a freshly created column, as a fraction (0.1–1.0) of the
    /// workarea. Replaces the old `default_col_w` (pixels) and `split_bias`
    /// (fraction) keys, which are now deprecated aliases.
    pub column_width: f32,
    pub focus_mouse: bool,
    pub warp_cursor: bool,
    /// Accordion factor: extra fraction (0.0-0.9) the focused column expands
    /// when dynamic focus expansion is active. 0.0 disables it, so no column
    /// resizes just because focus moved — only `GrowColumn`/`ShrinkColumn`
    /// change a column's width. Default is 0.0: a non-zero boost makes the
    /// previously focused neighbor visibly shrink on every focus change, which
    /// reads as jitter rather than polish.
    pub accordion_boost: f32,
    /// Minimum zoom factor for the Overview film-strip.
    pub overview_zoom_min: f32,

    /// Compositor configuration (OpenGL/GLX). The WM tries to bring up GL on
    /// `CompositeGetOverlayWindow`; if GL is missing, the 3.3 context can't be
    /// created, or another compositor already owns the screen, it logs and
    /// silently falls back to the classic `ConfigureWindow` path.
    pub compositor: CompositorCfg,

    /// Animation configuration. Independent from the compositor: the compositor
    /// can run with `animations.enabled = false` for vsync without springs.
    pub animations: AnimationsCfg,

    /// Native wallpaper configuration (source + mode). `None` path ⇒ no native
    /// wallpaper (legacy root pixmap / transparent). Applied to `State.wallpaper`
    /// at startup; the compositor decodes/uploads it when GL is available.
    pub wallpaper: WallpaperCfg,

    // Catppuccin Mocha; also the `Default` baseline below and the values
    // `theme_palette` returns for the same preset. Stored as 0xRRGGBB.
    pub col_normal: u32,
    pub col_focused: u32,
    pub col_urgent: u32,

    pub tag_names: Vec<String>,
    pub keybinds: Vec<(u16, u32, Action)>,
    pub rules: Vec<Rule>,

    /// Programs launched once the WM is ready. Compositor, bar, wallpaper,
    /// portals — maverick doesn't orchestrate any external tool specially,
    /// they're all just autostart entries.
    pub autostart: Vec<Vec<String>>,

    /// Global policy for the client's map-time `_NET_WM_STATE`. When `false`
    /// (the default), every newly managed window's `_NET_WM_STATE_MAXIMIZED_*`
    /// / `_NET_WM_STATE_FULLSCREEN` set at map time is normalized away and the
    /// window opens as a normal tile. This is a WM invariant, not a per-app
    /// rule: it is what keeps apps that "remember" their last maximized/
    /// fullscreen geometry (and even unrelated clients such as Firefox forks)
    /// from opening as a fullscreen/maximized window the WM did not authorise.
    /// Set to `true` to honour whatever the client asks for at map time; a
    /// single rule may override this per-window via `Rule::honor_initial_state`.
    pub honor_initial_state: bool,
}

impl Default for Cfg {
    /// Minimal config with no keybinds/rules — intended for tests and as a
    /// safe baseline. The real runtime config is built by `load_config`.
    fn default() -> Self {
        Cfg {
            border_w: 1,
            gaps_inner: 4,
            gaps_outer: 8,
            smart_gaps: false,
            corner_radius: 0,
            n_tags: 9,
            column_width: 0.6,
            focus_mouse: false,
            warp_cursor: false,
            accordion_boost: 0.0,
            overview_zoom_min: 0.25,
            compositor: CompositorCfg::default(),
            animations: AnimationsCfg::default(),
            wallpaper: WallpaperCfg::default(),
            col_normal: 0x45475a,
            col_focused: 0x89b4fa,
            col_urgent: 0xf38ba8,
            tag_names: (1..=9).map(|n| n.to_string()).collect(),
            keybinds: vec![],
            rules: vec![],
            autostart: vec![],
            honor_initial_state: false,
        }
    }
}

/// Swap-interval policy for the GL compositor. This is Maverick's own frame
/// pacing only: `fullscreen_bypass` hands a window back to the client, and
/// whatever vsync the application sets for itself is never touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsyncMode {
    /// `glXSwapInterval 1` – tear-free, blocks to vblank (default).
    On,
    /// No swap interval – immediate present (tearing allowed, lowest latency).
    Off,
    /// `GLX_EXT_swap_control_tear` with `-1` when available, else `1`. Best for VRR.
    Adaptive,
}

/// Backend selected for the compositor. The WM never assumes which GPU API is
/// available — the config is validated against compiled features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompositorBackend {
    #[default]
    OpenGl,
    Vulkan,
}

impl std::str::FromStr for CompositorBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "opengl" | "gl" | "glx" => Ok(Self::OpenGl),
            "vulkan" | "vk" => Ok(Self::Vulkan),
            other => Err(format!(
                "unknown compositor backend '{other}' (opengl|vulkan)"
            )),
        }
    }
}

impl std::fmt::Display for CompositorBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenGl => f.write_str("opengl"),
            Self::Vulkan => f.write_str("vulkan"),
        }
    }
}

/// Compositor (OpenGL/GLX) configuration, exposed as the `[compositor]` table in
/// the TOML (a deprecated `[general].compositor_enabled` alias also maps to
/// `enabled`). An absent table means "on with these defaults". `enabled = false`
/// — or the `MAVERICK_NO_COMPOSITOR` env var — means the compositor is never
/// attempted and the WM stays on the plain `ConfigureWindow` path, which also
/// keeps the X11 `Shape` corner-radius rounding available.
#[derive(Debug, Clone)]
pub struct CompositorCfg {
    /// Master switch. Default `true`: on by default, with automatic fallback.
    pub enabled: bool,
    /// Backend to use when the compositor is enabled.
    pub backend: CompositorBackend,
    /// When `true` (default), Maverick may step aside ("bypass") and let a single
    /// eligible fullscreen window on an output present itself directly, instead
    /// of compositing it, to cut latency/overhead for games and video players.
    /// The decision is made per-output by `crate::compositor_policy` and only
    /// fires when the scene is unambiguously safe (exactly one fullscreen window,
    /// nothing composited above it). Bypass NEVER changes an application's own
    /// `VSync` — it only removes Maverick's redirection of that one window. When
    /// `false`, Maverick always composites, even under fullscreen.
    pub fullscreen_bypass: bool,
    /// `VSync` mode for the GL compositor. `On` (default) = interval 1, `Off` = no
    /// vsync, `Adaptive` = `-1` (tear) when `GLX_EXT_swap_control_tear` is present.
    pub vsync: VsyncMode,
}

impl Default for CompositorCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: CompositorBackend::OpenGl,
            fullscreen_bypass: true,
            vsync: VsyncMode::On,
        }
    }
}

/// Animation configuration, exposed as `[animations]` in the TOML (a deprecated
/// `[general].camera_stiffness`/`camera_damping` pair still maps here).
/// Independent from `[compositor]`: animations can be disabled while keeping
/// vsync, and vice versa.
#[derive(Debug, Clone)]
pub struct AnimationsCfg {
    /// Master switch for spring animations (scroll, zoom, accordion). Default `true`.
    pub enabled: bool,
    /// Spring stiffness for the scroll camera (see `Camera::step`). Higher =
    /// snappier. Default 220.
    pub stiffness: f32,
    /// Spring damping for the scroll camera. Higher = less overshoot. Default 30.
    pub damping: f32,
}

impl Default for AnimationsCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            stiffness: 220.0,
            damping: 30.0,
        }
    }
}

/// Native wallpaper configuration, exposed as `[wallpaper]` in the TOML.
/// `path = null` (the default) means no native wallpaper is set and the WM
/// leaves the root pixmap alone.
#[derive(Debug, Clone)]
pub struct WallpaperCfg {
    /// Path to a wallpaper image or GLSL shader. `None` ⇒ disabled.
    pub path: Option<String>,
    /// Mapping mode applied to the source.
    pub mode: crate::core::wallpaper::WallpaperMode,
}

impl Default for WallpaperCfg {
    fn default() -> Self {
        Self {
            path: None,
            mode: crate::core::wallpaper::WallpaperMode::Fill,
        }
    }
}

/// Window-matching rule evaluated at map time by `manage::apply_rules`.
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
    /// Apple/macOS-style: ignore whatever `_NET_WM_STATE_MAXIMIZED_*` /
    /// `_NET_WM_STATE_FULLSCREEN` the window requests at map time and force
    /// it to open as a normal tile instead. GTK apps (Firefox chief among
    /// them) are the classic offender: they remember being maximized from the
    /// last session and demand it back on every launch, which on a tiling WM
    /// just means "fill the workarea, no gaps, no matter what you tiled last".
    /// This rule is the per-window form of the global
    /// [`Cfg::honor_initial_state`] policy and always wins over it.
    pub ignore_initial_state: bool,
    /// Override the global default for this specific window. `Some(true)`
    /// honours the window's map-time `_NET_WM_STATE_MAXIMIZED_*` /
    /// `_NET_WM_STATE_FULLSCREEN`; `Some(false)` normalizes it away; `None`
    /// defers to `Cfg::honor_initial_state`. This is the escape hatch for an app
    /// that genuinely must launch maximized/fullscreen.
    pub honor_initial_state: Option<bool>,
    /// Refuse this app's *own* fullscreen requests at runtime. An EWMH
    /// `_NET_WM_STATE_FULLSCREEN` client message — what a browser's F11 sends —
    /// is dropped; the window stays tiled. The built-in `Mod4+Shift+F` is
    /// unaffected and still gives a normal tiled fullscreen, because that is the
    /// user asking, not the app. Where `ignore_initial_state` fires once at map
    /// time, this holds for the whole life of the window.
    pub deny_fullscreen: bool,
    /// Give this app real, exclusive fullscreen: an overlay covering the whole
    /// screen in any layout, outside the scrolling ribbon, with
    /// `_NET_WM_BYPASS_COMPOSITOR` set so the compositor stops redirecting it.
    /// For games and video players that manage their own vsync — Maverick does
    /// not touch frame pacing, it just stays out of the way. Wins over
    /// `deny_fullscreen` if both are set.
    pub true_fullscreen: bool,
}

impl Rule {
    /// True when every criterion matches. An absent criterion is a wildcard;
    /// `class`, `instance` and `title` match as a case-insensitive substring
    /// (in both directions, so `fire` matches `Firefox`), while `window_type`
    /// must equal one of the window's reported `_NET_WM_WINDOW_TYPE` values,
    /// case-insensitively.
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
/// Build the compiled baseline: the values Maverick ships with, used as the
/// starting point a user TOML overlays and as the fallback whenever no file
/// exists or it fails to load. It carries the default keybind map, the built-in
/// dialog/portal rules and the portal autostart entries, but no generated
/// workspace binds (see `userconfig::default_config`).
pub fn compiled_config() -> Cfg {
    // X11 `ModMask` bit positions, spelled out so the table below reads without
    // cross-referencing x11rb: Mod4/Super = 1<<6, Shift = 1<<0,
    // Control = 1<<2.
    const MOD4: u16 = 1 << 6;
    const SHIFT: u16 = 1 << 0;
    const CONTROL: u16 = 1 << 2;
    let sup: u16 = MOD4;
    let shs: u16 = MOD4 | SHIFT;
    let sct: u16 = MOD4 | CONTROL;

    // X11 keysym values.
    const XK_RETURN: u32 = 0xff0d;
    const XK_SPACE: u32 = 0x0020;
    const XK_F5: u32 = 0xffc2;
    const XK_TAB: u32 = 0xff09;
    const XK_EQUAL: u32 = 0x003d;
    const XK_MINUS: u32 = 0x002d;
    const XK_BRACKETRIGHT: u32 = 0x005d;
    const XK_BRACKETLEFT: u32 = 0x005b;
    // Printable keysyms are their own lowercase ASCII codepoint.
    macro_rules! k {
        ($c:literal) => {
            $c as u32
        };
    }

    let keybinds: Vec<(u16, u32, Action)> = vec![
        // spawn
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
        // window state
        (shs, k!(b'c'), Action::Kill), // Mod4+Shift+C — close focused window
        (shs, XK_SPACE, Action::ToggleFloat),
        (shs, k!(b'f'), Action::ToggleFullscreen),
        (shs, k!(b'm'), Action::ToggleMaximize),
        // focus navigation
        (sup, k!(b'h'), Action::FocusDir(Dir::Left)),
        (sup, k!(b'l'), Action::FocusDir(Dir::Right)),
        (sup, k!(b'j'), Action::FocusDir(Dir::Down)),
        (sup, k!(b'k'), Action::FocusDir(Dir::Up)),
        // window movement
        (shs, k!(b'h'), Action::MoveDir(Dir::Left)),
        (shs, k!(b'l'), Action::MoveDir(Dir::Right)),
        (shs, k!(b'j'), Action::MoveDir(Dir::Down)),
        (shs, k!(b'k'), Action::MoveDir(Dir::Up)),
        // column ops
        (shs, XK_RETURN, Action::NewColumn),
        (sct, k!(b'h'), Action::GrowCol(-50)),
        (sct, k!(b'l'), Action::GrowCol(50)),
        (sct, k!(b'j'), Action::CollapseColumn),
        // layout
        (sup, k!(b't'), Action::SetLayout(LayoutKind::Column)),
        // session, monitor and restart
        // Mod4+Shift+Q quits Maverick immediately without confirmation.
        // The shutdown path runs the normal teardown: ask clients to close,
        // wait up to SHUTDOWN_BUDGET, force-kill remaining, then cleanup.
        (shs, k!(b'q'), Action::Quit),
        (shs, k!(b'r'), Action::Restart),
        (sup, XK_F5, Action::Restart),
        (sup, XK_TAB, Action::FocusMon(Dir::Next)),
        (shs, XK_TAB, Action::MoveMon(Dir::Next)),
        // overview (semantic-zoom film strip)
        (sup, k!(b'o'), Action::ToggleOverview),
        (sup, k!(b'n'), Action::OverviewNav(Dir::Right)),
        (shs, k!(b'o'), Action::OverviewNav(Dir::Left)),
        (sup, k!(b'e'), Action::OverviewEnter),
        // viewport (zoom-in inspection + page-snap scrolling)
        (sup, XK_EQUAL, Action::ViewportZoom(0.2)), // Mod4+=  zoom viewport in
        (sup, XK_MINUS, Action::ViewportZoom(-0.2)), // Mod4+-  zoom viewport out (back to Normal at 1.0)
        (sup, XK_BRACKETRIGHT, Action::PageSnap(Dir::Right)), // Mod4+]  scroll one page right
        (sup, XK_BRACKETLEFT, Action::PageSnap(Dir::Left)), // Mod4+[  scroll one page left
    ];

    Cfg {
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
            // No per-`WM_CLASS` rule is needed for GTK's "remember I was
            // maximized" tantrum: map-time `_NET_WM_STATE` is normalized for
            // *every* client by default (see `Cfg::honor_initial_state`), so
            // Firefox and its forks open as normal tiles. To let a client's
            // launch state stick, set `[general] honor_initial_state = true` or
            // opt in one app with `[[rules]] honor_initial_state = true`.
            //
            // Runtime fullscreen is honoured by default too; `deny_fullscreen`
            // remains available as an explicit opt-in for apps whose own
            // F11/EWMH fullscreen you want refused while `Mod4+Shift+F` keeps
            // working.
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

        // Programs launched once the WM is ready. Each entry is a command plus
        // its argv: vec!["binary", "arg1", "arg2", ...]. Nothing here is
        // special-cased, the compositor included — they are all just spawned.
        autostart: vec![
            // Absolute paths on purpose: these are not on $PATH by convention
            // (Arch installs them under /usr/lib). Without them, GTK/portal
            // file pickers (e.g. a browser upload dialog) fail to open.
            vec!["/usr/lib/xdg-desktop-portal-gtk".into()],
            vec!["/usr/lib/xdg-desktop-portal".into()],
            // External compositor, e.g.:
            // vec!["picom".into(), "--vsync".into()],
            // Status bar: maverick reserves screen space for it automatically
            // via _NET_WM_STRUT_PARTIAL (see backend/x11/struts.rs), so tiled
            // windows never overlap it. E.g.:
            // vec!["polybar".into(), "main".into()],
            // Wallpaper, e.g.:
            // vec!["feh".into(), "--bg-fill".into(), "/path/to/wallpaper.png".into()],
        ],
        ..Default::default()
    }
}

/// Whether the compositor should be attempted at startup. `true` only when the
/// `[compositor]` config opts in *and* the `MAVERICK_NO_COMPOSITOR` env var is
/// not set. The actual GL probe/fallback happens later; this is the policy gate.
pub fn compositor_enabled(cfg: &Cfg) -> bool {
    cfg.compositor.enabled && std::env::var_os("MAVERICK_NO_COMPOSITOR").is_none()
}

/// Whether spring animations should run. When false, the WM snaps directly to
/// the target (no interpolation) and never requests animation frames.
pub fn animations_enabled(cfg: &Cfg) -> bool {
    cfg.animations.enabled
}

/// Validate that the requested compositor backend was compiled in. Returns an
/// actionable error when `backend = "vulkan"` is configured but the binary was
/// built without `compositor-vulkan`.
pub fn validate_compositor_backend(cfg: &Cfg) -> Result<(), String> {
    // A binary with no compositor backend compiled in *is* the no-compositor
    // build (dwm-style). There the `[compositor]` table is inert — the WM
    // always runs on the classic X11 path — so the section is simply ignored:
    // nothing to validate, no error, no warning.
    if !cfg!(feature = "compositor-opengl") {
        return Ok(());
    }
    // The backend is never consulted when the compositor is off (`enabled =
    // false` or `MAVERICK_NO_COMPOSITOR`): validating it then only produces a
    // spurious error for a setting that has no effect.
    if !compositor_enabled(cfg) {
        return Ok(());
    }
    match cfg.compositor.backend {
        CompositorBackend::OpenGl => {
            if cfg!(feature = "compositor-opengl") {
                Ok(())
            } else {
                Err(
                    "compositor.backend = \"opengl\" requested but this binary was built without \
                     the `compositor-opengl` feature; rebuild with `--features compositor-opengl` \
                     or set `backend = \"vulkan\"` if that feature is available, or disable the \
                     compositor with `[compositor] enabled = false`"
                        .to_string(),
                )
            }
        }
        CompositorBackend::Vulkan => Err(
            "compositor.backend = \"vulkan\" requested but this Maverick has no Vulkan \
             backend: the feature was a placeholder that never selected a crate, and \
             it has been removed. Set `backend = \"opengl\"`, or disable the compositor \
             with `[compositor] enabled = false`"
                .to_string(),
        ),
    }
}

/// Build the runtime config: the compiled baseline, with an optional user
/// TOML layered on top. `path` overrides the default XDG location (used by the
/// `--config` CLI flag). A missing or invalid TOML never prevents startup —
/// see `userconfig` for the fail-safe loading rules.
pub fn load_config(path: Option<&Path>) -> Cfg {
    crate::userconfig::load_config(path)
}

/// Named color-theme presets for `[general].theme` in the TOML config.
/// Returns `(normal, focused, urgent)` as `0xRRGGBB`, or `None` for an unknown
/// name (the caller then keeps the compiled colors and warns).
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
    use super::compiled_config;
    use super::{Action, Rule};

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

    #[test]
    fn compiled_config_normalizes_initial_state_for_all_clients() {
        // No per-`WM_CLASS` workaround may come back: normalizing map-time
        // `_NET_WM_STATE` is a global invariant (`Cfg::honor_initial_state` +
        // `manage::apply_rules`), not something a rule should patch per app.
        let has_firefox = super::compiled_config()
            .rules
            .into_iter()
            .any(|r| r.class.as_deref() == Some("firefox"));
        assert!(
            !has_firefox,
            "no per-application hack: normalization is a global invariant",
        );
        assert!(
            !super::compiled_config().honor_initial_state,
            "compiled default must normalize map-time client state",
        );
    }

    #[test]
    fn compiled_config_binds_mod4_shift_q_to_quit() {
        // The built-in `Mod4+Shift+Q` must be a direct quit, not a shell-out to
        // `maverickctl quit --confirm`, which would put a prompt and a second
        // process on the quit path.
        let cfg = compiled_config();
        const SUPER: u16 = 1 << 6;
        const SHIFT: u16 = 1 << 0;
        let shs = SUPER | SHIFT;
        let q = u32::from(b'q');
        let matched = cfg.keybinds.iter().find(|(m, k, _a)| *m == shs && *k == q);
        assert_eq!(
            matched,
            Some(&(shs, q, Action::Quit)),
            "Mod4+Shift+Q must resolve to Action::Quit"
        );
        assert!(
            !cfg
                .keybinds
                .iter()
                .any(|(_, _, a)| matches!(a, Action::Spawn(cmd) if cmd.first().is_some_and(|b| b == "maverickctl"))),
            "compiled config must not spawn maverickctl for the quit binding"
        );
    }

    #[test]
    fn compiled_config_binds_mod4_shift_r_to_restart() {
        let cfg = compiled_config();
        const SUPER: u16 = 1 << 6;
        const SHIFT: u16 = 1 << 0;
        let shs = SUPER | SHIFT;
        let r = u32::from(b'r');
        assert!(
            cfg.keybinds
                .iter()
                .any(|(m, k, a)| *m == shs && *k == r && matches!(a, Action::Restart)),
            "Mod4+Shift+R must still restart"
        );
    }
}
