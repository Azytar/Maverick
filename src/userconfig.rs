//! Safe, optional TOML configuration layered over Maverick's compiled defaults.
//!
//! Parses the file with `maverick-toml`, a strict, zero-dependency TOML-subset
//! parser (replacing `toml` + `serde`). Syntax failures reject the whole file
//! — the WM starts with compiled defaults and logs the offending line.
//! Semantic failures are isolated to the offending value or list entry and
//! never abort WM startup.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use maverick_toml::{parse, Event, ParseError, Value};
use x11rb::protocol::xproto::ModMask;

use crate::config::{compiled_config, Cfg, Rule};
use crate::log;
use crate::types::Action;

/// Accumulated config diagnostics, separated from logging so a caller can
/// decide what to do with them. The boot path and `reload_config` dump both
/// lists via `log::warn` and carry on (nothing is fatal at runtime, B10), while
/// `--check-config` inspects them to decide its exit code.
///
/// * `errors`   — a value that could not be applied at all: unknown keysym,
///   unknown action, binding conflict, workspace out of range, unknown
///   `window_type`, numeric value out of range, rule with no criteria.
/// * `warnings` — a value that was accepted-but-degraded: deprecated alias,
///   wrong-typed field, empty entry discarded, unknown theme.
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl Diagnostics {
    /// True when there is nothing worth reporting (used by the `--check-config`
    /// contract test and exit-code decision).
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty() && self.errors.is_empty()
    }
}

/// Emit every diagnostic via `log::warn` (the fail-safe runtime path keeps
/// going regardless — see B10).
pub fn dump_diagnostics(diag: &Diagnostics) {
    for w in &diag.warnings {
        log::warn!("config: {w}");
    }
    for e in &diag.errors {
        log::warn!("config: {e}");
    }
}
#[derive(Debug, Default)]
struct UserConfig {
    general: Option<GeneralCfg>,
    colors: Option<ColorsCfg>,
    keybindings: Vec<KeybindEntry>,
    rules: Vec<RuleEntry>,
    autostart: Option<AutostartCfg>,
    wallpaper: Option<WallpaperEntry>,
}

#[derive(Debug, Default)]
struct WallpaperEntry {
    path: Option<String>,
    mode: Option<String>,
}

#[derive(Debug, Default)]
struct GeneralCfg {
    border_width: Option<u32>,
    /// Legacy alias: sets both `gaps_inner` and `gaps_outer` at once.
    gaps: Option<u32>,
    gaps_inner: Option<u32>,
    gaps_outer: Option<u32>,
    smart_gaps: Option<bool>,
    corner_radius: Option<u32>,
    /// Named color-scheme preset (see `config::theme_palette`). Applied
    /// before `[colors]`, so an explicit `[colors]` entry always wins over
    /// whatever the theme sets.
    theme: Option<String>,
    n_tags: Option<usize>,
    /// New-style column width: fraction (0.1–1.0) of the workarea given to a
    /// freshly created column (B3). Replaces the old `default_col_width` (px)
    /// and `split_bias` (fraction) keys, which are kept as deprecated aliases.
    column_width: Option<f32>,
    /// Legacy alias: pixel column width. Converted to `column_width` using a
    /// 1920px workarea fallback; emits a deprecation warning (T5).
    default_col_width: Option<u32>,
    /// Legacy alias: fraction of the workarea for a new column. Maps directly
    /// onto `column_width`; emits a deprecation warning (T5).
    split_bias: Option<f32>,
    /// Accordion focus-expansion factor (0.0–0.9), see `Cfg::accordion_boost`.
    accordion_boost: Option<f32>,
    /// Overview film-strip minimum zoom (0.05–1.0), see `Cfg::overview_zoom_min`.
    overview_zoom_min: Option<f32>,
    /// Compositor (OpenGL/GLX) master switch, `[compositor].enabled`.
    compositor_enabled: Option<bool>,
    /// Scroll-camera spring stiffness, `[compositor].stiffness`.
    camera_stiffness: Option<f32>,
    /// Scroll-camera spring damping, `[compositor].damping`.
    camera_damping: Option<f32>,
    /// Auto-generate Super+1..n / Super+Shift+1..n workspace binds (T3).
    /// Defaults to `true`; when `false` no workspace binds are added.
    auto_workspace_binds: Option<bool>,
    focus_mouse: Option<bool>,
    warp_cursor: Option<bool>,
    tag_names: Option<Vec<String>>,
    /// Global policy for the client's map-time `_NET_WM_STATE`. `false` (the
    /// default) normalizes away a window's initial maximized/fullscreen request
    /// so it opens as a normal tile; `true` honours it. See `Cfg::honor_initial_state`.
    honor_initial_state: Option<bool>,
}

#[derive(Debug, Default)]
struct ColorsCfg {
    normal: Option<u32>,
    focused: Option<u32>,
    urgent: Option<u32>,
}

#[derive(Debug, Default)]
struct KeybindEntry {
    key: String,
    action: String,
}

#[derive(Debug, Default)]
struct RuleEntry {
    class: Option<String>,
    instance: Option<String>,
    window_type: Option<String>,
    title: Option<String>,
    float: bool,
    sticky: bool,
    workspace: Option<usize>,
    /// `[width, height]` in pixels for forced floating size.
    size: Option<[u32; 2]>,
    /// `[x, y]` relative to the monitor workarea origin, for forced position.
    position: Option<[i32; 2]>,
    /// 0.0-1.0. Out-of-range values are clamped by `manage::apply_rules` at
    /// use time, not rejected here — an opacity typo shouldn't discard an
    /// otherwise-valid rule.
    opacity: Option<f32>,
    border_width: Option<u32>,
    /// Apple/macOS-style: ignore the window's own requested
    /// maximized/fullscreen state at map time, forcing it to open as a
    /// normal tile. See `config::Rule::ignore_initial_state`.
    ignore_initial_state: bool,
    /// Per-rule override of the global `honor_initial_state` policy. `Some(true)`
    /// honours this window's map-time state; `Some(false)` normalizes it away;
    /// `None` defers to the global config. See `config::Rule::honor_initial_state`.
    honor_initial_state: Option<bool>,
    /// Refuse the app's own runtime fullscreen requests (F11 / EWMH).
    /// `Mod4+F` is unaffected. See `config::Rule::deny_fullscreen`.
    deny_fullscreen: bool,
    /// Real exclusive fullscreen, outside the ribbon, for games.
    /// See `config::Rule::true_fullscreen`.
    true_fullscreen: bool,
}

#[derive(Debug, Default)]
struct AutostartCfg {
    commands: Vec<Vec<String>>,
}

/// The current top-level table the event stream is inside. `Plain` tables are
/// `[general]`-style singletons; `Rows` tables are `[[keybindings]]`/`[[rules]]`
/// arrays whose keys append to the most recent entry.
#[derive(Debug, Clone, Copy)]
enum Cur<'a> {
    Plain(&'a str),
    Row(&'a str),
}

/// Return Maverick's XDG config path without requiring it to exist.
pub fn config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("maverick/config.toml"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config/maverick/config.toml"))
}

/// Load the user config from `path` (when `None`, falls back to the standard
/// XDG path; if even that is missing, the compiled defaults are used). All
/// diagnostics are logged and startup proceeds regardless — config is never
/// fatal (B10).
pub fn load_config(path: Option<&Path>) -> Cfg {
    let Some(path) = path.map(Path::to_path_buf).or_else(config_path) else {
        return default_config();
    };
    let (cfg, diag) = load_from_path(&path);
    dump_diagnostics(&diag);
    cfg
}

/// The complete fail-safe default: the compiled baseline PLUS the auto-generated
/// numeric workspace keybindings (`Super+1..n` → view, `Super+Shift+1..n` → move).
///
/// `compiled_config()` itself stays pure — unit tests build it directly as a
/// baseline and expect no workspace binds. This helper exists so every fallback
/// path in the loader (no config file, missing file, unreadable file, or broken
/// TOML) still produces a WM whose workspaces are actually reachable. Previously
/// those paths returned `compiled_config()` directly, which ships zero workspace
/// binds, so the WM booted with `n_tags` workspaces it could never switch to.
fn default_config() -> Cfg {
    let mut cfg = compiled_config();
    append_numeric_keybindings(&mut cfg.keybinds, cfg.n_tags);
    cfg
}

/// Parse `path` into a `Cfg`, returning the config together with the full
/// `Diagnostics`. On a missing file the compiled defaults are returned with an
/// empty diagnostic (fail-safe by design); on a syntax error the defaults are
/// returned with the error recorded; on semantic issues the offending entries
/// are dropped and reported. Never panics and never returns `None`.
pub fn load_from_path(path: &Path) -> (Cfg, Diagnostics) {
    let baseline = default_config();
    let mut diag = Diagnostics::default();

    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (baseline, diag),
        Err(e) => {
            diag.errors.push(format!(
                "cannot read '{}': {e}; using compiled defaults",
                path.display()
            ));
            return (baseline, diag);
        }
    };

    let user = match parse_user(&source, &mut diag) {
        Ok(user) => user,
        Err(e) => {
            diag.errors.push(format!(
                "invalid TOML in '{}' (line {}, {}); using compiled defaults",
                path.display(),
                e.line,
                e.kind
            ));
            return (baseline, diag);
        }
    };

    let cfg = merge_config(baseline, user, &mut diag);
    (cfg, diag)
}

/// Consume the strict TOML-subset event stream and build the user model.
/// A `ParseError` here means the whole file is rejected by the caller.
/// Type-mismatch warnings discovered while mapping keys are accumulated into
/// `diag` (they are isolated to the offending entry, never fatal).
fn parse_user(source: &str, diag: &mut Diagnostics) -> Result<UserConfig, ParseError> {
    let mut user = UserConfig::default();
    let mut cur: Option<Cur<'_>> = None;

    for event in parse(source) {
        let event = event?;
        match event {
            Event::Section(name) => cur = Some(Cur::Plain(name)),
            Event::ArraySection(name) => {
                match name {
                    "keybindings" => user.keybindings.push(KeybindEntry::default()),
                    "rules" => user.rules.push(RuleEntry::default()),
                    _ => {}
                }
                cur = Some(Cur::Row(name));
            }
            Event::KeyValue(key, value) => match cur {
                Some(Cur::Plain("general")) => {
                    let g = user.general.get_or_insert_with(GeneralCfg::default);
                    apply_general_key(g, key, &value, diag);
                }
                Some(Cur::Plain("colors")) => {
                    let c = user.colors.get_or_insert_with(ColorsCfg::default);
                    apply_color_key(c, key, &value, diag);
                }
                Some(Cur::Plain("autostart")) => {
                    if matches!(key, "commands" | "apps" | "programs") {
                        user.autostart.get_or_insert_with(AutostartCfg::default);
                        if let Some(grid) = grid_strings(&value) {
                            user.autostart.as_mut().unwrap().commands = grid;
                        } else {
                            diag.warnings.push(format!(
                                "[autostart].{key} must be a list of string lists; ignoring it"
                            ));
                        }
                    }
                }
                Some(Cur::Plain("wallpaper")) => {
                    let w = user.wallpaper.get_or_insert_with(WallpaperEntry::default);
                    match key {
                        "path" => set_string(&mut w.path, key, &value, diag),
                        "mode" => set_string(&mut w.mode, key, &value, diag),
                        _ => {}
                    }
                }
                Some(Cur::Row("keybindings")) => {
                    if let Some(row) = user.keybindings.last_mut() {
                        apply_keybind_key(row, key, &value);
                    }
                }
                Some(Cur::Row("rules")) => {
                    if let Some(row) = user.rules.last_mut() {
                        apply_rule_key(row, key, &value, diag);
                    }
                }
                // Unknown sections and rows are ignored entirely — future
                // config keys must never break older WMs.
                _ => {}
            },
        }
    }
    Ok(user)
}

/// Map one `[general]` key onto the model. Aliases from the old serde schema
/// are resolved here; a value of the wrong type is skipped with a warning
/// (semantic isolation, like bad keybindings) instead of rejecting the file.
/// Type mismatches are recorded in `diag`.
fn apply_general_key(g: &mut GeneralCfg, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    match key {
        "border_width" | "border_w" => set_u32(&mut g.border_width, key, value, diag),
        "gaps" => set_u32(&mut g.gaps, key, value, diag),
        "gaps_inner" => set_u32(&mut g.gaps_inner, key, value, diag),
        "gaps_outer" => set_u32(&mut g.gaps_outer, key, value, diag),
        "smart_gaps" => set_bool(&mut g.smart_gaps, key, value, diag),
        "corner_radius" => set_u32(&mut g.corner_radius, key, value, diag),
        "theme" => set_string(&mut g.theme, key, value, diag),
        "n_tags" => set_usize(&mut g.n_tags, key, value, diag),
        "column_width" => set_f32(&mut g.column_width, key, value, diag),
        "default_col_width" | "default_col_w" => {
            set_u32(&mut g.default_col_width, key, value, diag)
        }
        "split_bias" => set_f32(&mut g.split_bias, key, value, diag),
        "accordion_boost" => set_f32(&mut g.accordion_boost, key, value, diag),
        "overview_zoom_min" => set_f32(&mut g.overview_zoom_min, key, value, diag),
        "auto_workspace_binds" => set_bool(&mut g.auto_workspace_binds, key, value, diag),
        "focus_mouse" => set_bool(&mut g.focus_mouse, key, value, diag),
        "warp_cursor" => set_bool(&mut g.warp_cursor, key, value, diag),
        "honor_initial_state" => set_bool(&mut g.honor_initial_state, key, value, diag),
        "compositor_enabled" => set_bool(&mut g.compositor_enabled, key, value, diag),
        "camera_stiffness" => set_f32(&mut g.camera_stiffness, key, value, diag),
        "camera_damping" => set_f32(&mut g.camera_damping, key, value, diag),
        "tag_names" => {
            if let Some(list) = value.as_str_list() {
                g.tag_names = Some(list.iter().map(|s| s.as_ref().to_string()).collect());
            } else {
                warn_bad(diag, key);
            }
        }
        _ => {}
    }
}

/// Map one `[colors]` key onto the model.
fn apply_color_key(c: &mut ColorsCfg, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    match key {
        "normal" | "col_normal" => set_u32(&mut c.normal, key, value, diag),
        "focused" | "col_focused" => set_u32(&mut c.focused, key, value, diag),
        "urgent" | "col_urgent" => set_u32(&mut c.urgent, key, value, diag),
        _ => {}
    }
}

/// Map one `[[keybindings]]` key onto the current row.
fn apply_keybind_key(row: &mut KeybindEntry, key: &str, value: &Value<'_>) {
    match key {
        "key" => row.key = value.as_str().map_or_else(String::new, str::to_string),
        "action" => row.action = value.as_str().map_or_else(String::new, str::to_string),
        _ => {}
    }
}

/// Map one `[[rules]]` key onto the current row.
fn apply_rule_key(row: &mut RuleEntry, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    match key {
        "class" => set_string(&mut row.class, key, value, diag),
        "instance" => set_string(&mut row.instance, key, value, diag),
        "window_type" | "type" => set_string(&mut row.window_type, key, value, diag),
        "title" => set_string(&mut row.title, key, value, diag),
        "float" => row.float = value.as_bool().unwrap_or(false),
        "sticky" => row.sticky = value.as_bool().unwrap_or(false),
        "workspace" | "ws" => set_usize(&mut row.workspace, key, value, diag),
        "size" => {
            if let Some([w, h]) = int_pair(value) {
                row.size = Some([w as u32, h as u32]);
            } else {
                warn_bad(diag, key);
            }
        }
        "position" => {
            if let Some([x, y]) = int_pair(value) {
                row.position = Some([x as i32, y as i32]);
            } else {
                warn_bad(diag, key);
            }
        }
        "opacity" => set_f32(&mut row.opacity, key, value, diag),
        "border_width" | "border_w" => set_u32(&mut row.border_width, key, value, diag),
        "ignore_initial_state" | "no_initial_state" | "no_maximize" => {
            row.ignore_initial_state = value.as_bool().unwrap_or(false);
        }
        "deny_fullscreen" | "no_fullscreen" => {
            row.deny_fullscreen = value.as_bool().unwrap_or(false);
        }
        "true_fullscreen" | "exclusive_fullscreen" => {
            row.true_fullscreen = value.as_bool().unwrap_or(false);
        }
        _ => {}
    }
}

// ── typed value helpers ────────────────────────────────────────────────────

/// Parse a two-element integer array, e.g. `size`/`position`.
fn int_pair(value: &Value<'_>) -> Option<[i64; 2]> {
    let list = value.as_int_list()?;
    Some([*list.first()?, *list.get(1)?])
}

fn grid_strings(value: &Value<'_>) -> Option<Vec<Vec<String>>> {
    Some(
        value
            .as_grid()?
            .iter()
            .map(|row| row.iter().map(|s| s.as_ref().to_string()).collect())
            .collect(),
    )
}

/// Assign `key`'s `u32` value into `slot`, warning (via `diag`) when the value
/// has the wrong type (the key is then left untouched).
fn set_u32(slot: &mut Option<u32>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_u32() {
        *slot = Some(v);
    } else {
        warn_bad(diag, key);
    }
}

fn set_bool(slot: &mut Option<bool>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_bool() {
        *slot = Some(v);
    } else {
        warn_bad(diag, key);
    }
}

fn set_string(slot: &mut Option<String>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_str() {
        *slot = Some(v.to_string());
    } else {
        warn_bad(diag, key);
    }
}

/// Assign `key`'s float value into `slot`. Accepts a decimal float *or* an
/// integer literal (e.g. `column_width = 1`) and coerces it to `f32` — the
/// strict TOML-subset parser keeps the two types separate, but config should
/// not force a trailing `.0` on integer-valued fractions.
fn set_f32(slot: &mut Option<f32>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_f64() {
        *slot = Some(v as f32);
    } else if let Some(i) = value.as_i64() {
        *slot = Some(i as f32);
    } else {
        warn_bad(diag, key);
    }
}

fn set_usize(slot: &mut Option<usize>, key: &str, value: &Value<'_>, diag: &mut Diagnostics) {
    if let Some(v) = value.as_u32() {
        *slot = Some(v as usize);
    } else {
        warn_bad(diag, key);
    }
}

fn warn_bad(diag: &mut Diagnostics, key: &str) {
    diag.warnings.push(format!(
        "value for '{key}' has an unexpected type; ignoring it"
    ));
}

// ── merge ──────────────────────────────────────────────────────────────────

fn merge_config(mut cfg: Cfg, user: UserConfig, diag: &mut Diagnostics) -> Cfg {
    let auto_ws = user
        .general
        .as_ref()
        .is_none_or(|g| g.auto_workspace_binds.unwrap_or(true));

    if let Some(general) = user.general {
        apply_general(&mut cfg, general, diag);
    }
    if let Some(colors) = user.colors {
        apply_colors(&mut cfg, colors, diag);
    }

    if !user.keybindings.is_empty() {
        cfg.keybinds = parse_keybindings(&user.keybindings, cfg.n_tags, auto_ws, diag);
    } else if auto_ws {
        // No user keybindings: still synthesize the workspace 1..n binds
        // (Super+1..n view, Super+Shift+1..n move) so a config that only sets
        // `[general]` options still gets a usable keymap (B1/T3).
        append_numeric_keybindings(&mut cfg.keybinds, cfg.n_tags);
    }
    if !user.rules.is_empty() {
        cfg.rules = parse_rules(user.rules, cfg.n_tags, diag);
    }
    if let Some(autostart) = user.autostart {
        cfg.autostart = autostart
            .commands
            .into_iter()
            .filter(|cmd| {
                if cmd.first().is_some_and(|bin| !bin.trim().is_empty()) {
                    true
                } else {
                    diag.warnings
                        .push("discarded empty autostart command".into());
                    false
                }
            })
            .collect();
    }
    if let Some(wp) = user.wallpaper {
        apply_wallpaper(&mut cfg, wp, diag);
    }

    normalize_tag_names(&mut cfg);
    cfg
}

fn apply_general(cfg: &mut Cfg, general: GeneralCfg, diag: &mut Diagnostics) {
    if let Some(v) = general.border_width {
        cfg.border_w = v;
    }
    if let Some(v) = general.gaps {
        cfg.gaps_inner = v;
        cfg.gaps_outer = v;
    }
    if let Some(v) = general.gaps_inner {
        cfg.gaps_inner = v;
    }
    if let Some(v) = general.gaps_outer {
        cfg.gaps_outer = v;
    }
    if let Some(v) = general.smart_gaps {
        cfg.smart_gaps = v;
    }
    if let Some(v) = general.corner_radius {
        cfg.corner_radius = v;
    }
    if let Some(name) = &general.theme {
        match crate::config::theme_palette(name) {
            Some((normal, focused, urgent)) => {
                cfg.col_normal = normal;
                cfg.col_focused = focused;
                cfg.col_urgent = urgent;
            }
            None => {
                diag.warnings.push(format!(
                    "general.theme '{name}' is not a known preset; ignoring it"
                ));
            }
        }
    }
    if let Some(v) = general.n_tags {
        if (1..=9).contains(&v) {
            cfg.n_tags = v;
        } else {
            diag.errors.push(format!(
                "general.n_tags must be between 1 and 9; ignoring {v}"
            ));
        }
    }
    // New-style column width (fraction of the workarea).
    if let Some(v) = general.column_width {
        if (0.1..=1.0).contains(&v) {
            cfg.column_width = v;
        } else {
            diag.errors.push(format!(
                "general.column_width must be between 0.1 and 1.0; ignoring {v}"
            ));
        }
    }
    // Legacy `default_col_width` / `default_col_w` (pixels) → fraction, using a
    // 1920px workarea fallback (no monitor is available at parse time).
    if let Some(px) = general.default_col_width {
        if px > 0 {
            cfg.column_width = (px as f32 / 1920.0).clamp(0.1, 1.0);
            diag.warnings.push(
                "general.default_col_width is deprecated; use [general].column_width \
                 (fraction 0.1–1.0)"
                    .into(),
            );
        } else {
            diag.errors
                .push("general.default_col_width must be greater than zero".into());
        }
    }
    // Legacy `split_bias` (fraction) → column_width.
    if let Some(v) = general.split_bias {
        if (0.0..=1.0).contains(&v) {
            cfg.column_width = v.clamp(0.1, 1.0);
            diag.warnings.push(
                "general.split_bias is deprecated; use [general].column_width \
                 (fraction 0.1–1.0)"
                    .into(),
            );
        } else {
            diag.errors.push(format!(
                "general.split_bias must be between 0.0 and 1.0; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.accordion_boost {
        if (0.0..=0.9).contains(&v) {
            cfg.accordion_boost = v;
        } else {
            diag.errors.push(format!(
                "general.accordion_boost must be between 0.0 and 0.9; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.overview_zoom_min {
        if (0.05..=1.0).contains(&v) {
            cfg.overview_zoom_min = v;
        } else {
            diag.errors.push(format!(
                "general.overview_zoom_min must be between 0.05 and 1.0; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.focus_mouse {
        cfg.focus_mouse = v;
    }
    if let Some(v) = general.warp_cursor {
        cfg.warp_cursor = v;
    }
    if let Some(v) = general.compositor_enabled {
        cfg.compositor.enabled = v;
    }
    if let Some(v) = general.camera_stiffness {
        if v > 0.0 {
            cfg.compositor.stiffness = v;
        } else {
            diag.errors.push(format!(
                "general.camera_stiffness must be > 0; ignoring {v}"
            ));
        }
    }
    if let Some(v) = general.camera_damping {
        if v > 0.0 {
            cfg.compositor.damping = v;
        } else {
            diag.errors
                .push(format!("general.camera_damping must be > 0; ignoring {v}"));
        }
    }
    if let Some(names) = general.tag_names {
        if names.is_empty() || names.iter().any(String::is_empty) {
            diag.warnings
                .push("general.tag_names must contain non-empty names; ignoring it".into());
        } else {
            cfg.tag_names = names;
        }
    }
    if let Some(v) = general.honor_initial_state {
        cfg.honor_initial_state = v;
    }
}

fn apply_colors(cfg: &mut Cfg, colors: ColorsCfg, _diag: &mut Diagnostics) {
    if let Some(v) = colors.normal {
        cfg.col_normal = v;
    }
    if let Some(v) = colors.focused {
        cfg.col_focused = v;
    }
    if let Some(v) = colors.urgent {
        cfg.col_urgent = v;
    }
}

/// Expand a leading `~` (or `~/`) to `$HOME` so `path = "~/img/wp.png"` in the
/// config resolves as users expect (TOML strings are literal; the shell does
/// not expand them). Anything else is returned unchanged.
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return home.to_string_lossy().into_owned();
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Apply the `[wallpaper]` section. A `path` sets the native wallpaper source
/// (image/shader inferred by extension); `mode` overrides the mapping mode.
/// Only validated values are written — a bad `mode` is reported and ignored.
fn apply_wallpaper(cfg: &mut Cfg, wp: WallpaperEntry, diag: &mut Diagnostics) {
    if let Some(path) = wp.path {
        // TOML strings are literal; expand a leading `~`/`~/` so
        // `path = "~/img/wp.png"` resolves as users expect.
        cfg.wallpaper.path = Some(expand_tilde(&path));
    }
    if let Some(mode) = wp.mode {
        match crate::core::wallpaper::WallpaperMode::from_str(&mode) {
            Ok(m) => cfg.wallpaper.mode = m,
            Err(e) => diag
                .warnings
                .push(format!("[wallpaper].mode: {e}; keeping default (fill)")),
        }
    }
}

fn normalize_tag_names(cfg: &mut Cfg) {
    cfg.tag_names.truncate(cfg.n_tags);
    while cfg.tag_names.len() < cfg.n_tags {
        cfg.tag_names.push((cfg.tag_names.len() + 1).to_string());
    }
}

fn parse_rules(entries: Vec<RuleEntry>, n_tags: usize, diag: &mut Diagnostics) -> Vec<Rule> {
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if entry.class.as_deref().is_none_or(str::is_empty)
                && entry.instance.as_deref().is_none_or(str::is_empty)
                && entry.window_type.as_deref().is_none_or(str::is_empty)
                && entry.title.as_deref().is_none_or(str::is_empty)
            {
                diag.errors.push(format!(
                    "discarded rule #{}: class, instance, window_type and title are all empty",
                    index + 1
                ));
                return None;
            }
            if let Some(wt) = entry.window_type.as_deref() {
                const KNOWN_TYPES: [&str; 8] = [
                    "normal", "desktop", "dock", "toolbar", "menu", "utility", "splash", "dialog",
                ];
                if !KNOWN_TYPES.contains(&wt.to_ascii_lowercase().as_str()) {
                    diag.errors.push(format!(
                        "discarded rule #{}: unknown window_type '{wt}'",
                        index + 1
                    ));
                    return None;
                }
            }
            let ws = match entry.workspace {
                Some(ws) if ws == 0 || ws > n_tags => {
                    diag.errors.push(format!(
                        "discarded rule #{}: workspace {ws} is outside 1..={n_tags}",
                        index + 1
                    ));
                    return None;
                }
                Some(ws) => Some(ws - 1),
                None => None,
            };
            Some(Rule {
                class: entry.class.filter(|s| !s.is_empty()),
                instance: entry.instance.filter(|s| !s.is_empty()),
                window_type: entry
                    .window_type
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_lowercase()),
                title: entry.title.filter(|s| !s.is_empty()),
                float: entry.float,
                sticky: entry.sticky,
                ws,
                size: entry.size.map(|[w, h]| (w, h)),
                position: entry.position.map(|[x, y]| (x, y)),
                opacity: entry.opacity,
                border_w: entry.border_width,
                ignore_initial_state: entry.ignore_initial_state,
                deny_fullscreen: entry.deny_fullscreen,
                true_fullscreen: entry.true_fullscreen,
                honor_initial_state: entry.honor_initial_state,
            })
        })
        .collect()
}

/// Parse `[[keybindings]]` rows into the `(mods, keysym, action)` list.
///
/// Workspace binds (Super+1..n view, Super+Shift+1..n move) are auto-generated
/// in the *free* slots whenever `[general].auto_workspace_binds` is true (the
/// default) — they never clobber a user bind that already occupies that
/// combination (B1). Duplicate `(mods, keysym)` pairs in the user's own list
/// resolve first-wins, with the loser reported via `diag` (B7).
fn parse_keybindings(
    entries: &[KeybindEntry],
    n_tags: usize,
    auto_workspace_binds: bool,
    diag: &mut Diagnostics,
) -> Vec<(u16, u32, Action)> {
    let mut parsed: Vec<(u16, u32, Action)> = Vec::new();

    for entry in entries {
        let Some((mods, keysym)) = keybind_from_str(&entry.key) else {
            diag.errors.push(format!(
                "discarded keybinding '{}': invalid key combination",
                entry.key
            ));
            continue;
        };
        let Some(action) = action_from_str(&entry.action) else {
            diag.errors.push(format!(
                "discarded keybinding '{}': invalid action '{}'",
                entry.key, entry.action
            ));
            continue;
        };
        if !action_workspace_is_valid(&action, n_tags) {
            diag.errors.push(format!(
                "discarded keybinding '{}': action workspace is outside 1..={n_tags}",
                entry.key
            ));
            continue;
        }
        // First-wins on duplicate combinations; later entries are dropped (B7).
        if parsed.iter().any(|(m, k, _)| *m == mods && *k == keysym) {
            diag.errors.push(format!(
                "keybinding '{}' (mods={mods:#x}, keysym={keysym:#x}) duplicates an earlier \
                 bind and was ignored; keeping {}",
                entry.key,
                action_name_of(&parsed, mods, keysym)
            ));
            continue;
        }
        parsed.push((mods, keysym, action));
    }

    if auto_workspace_binds {
        append_numeric_keybindings(&mut parsed, n_tags);
    }
    parsed
}

/// Human-readable name of the action already bound to `(mods, keysym)`, for the
/// conflict diagnostic.
fn action_name_of(list: &[(u16, u32, Action)], mods: u16, keysym: u32) -> String {
    list.iter()
        .find(|(m, k, _)| *m == mods && *k == keysym)
        .map_or_else(String::new, |(_, _, a)| {
            crate::core::action::name(a).to_string()
        })
}

fn action_workspace_is_valid(action: &Action, n_tags: usize) -> bool {
    match action {
        Action::View(ws) | Action::MoveToWs(ws) => *ws < n_tags,
        _ => true,
    }
}

fn append_numeric_keybindings(keybinds: &mut Vec<(u16, u32, Action)>, n_tags: usize) {
    let sup = u16::from(ModMask::M4);
    let shift_sup = sup | u16::from(ModMask::SHIFT);
    for ws in 0..n_tags.min(9) {
        let keysym = b'1' as u32 + ws as u32;
        let view = (sup, keysym);
        let move_to = (shift_sup, keysym);
        // Only fill the slots the user hasn't already claimed; a user bind on
        // e.g. Super+1 wins and the generated View(0) is skipped (B1).
        if !keybinds.iter().any(|(m, k, _)| (*m, *k) == view) {
            keybinds.push((view.0, view.1, Action::View(ws)));
        }
        if !keybinds.iter().any(|(m, k, _)| (*m, *k) == move_to) {
            keybinds.push((move_to.0, move_to.1, Action::MoveToWs(ws)));
        }
    }
}

/// Curated, layout-independent key name → X11 keysym table (B5). This is the
/// vocabulary TOML keybindings may use by name; anything not listed here can
/// still be expressed with the raw `0x<hex>` escape. Letters (`a`–`z`) and
/// digits (`0`–`9`) are handled by computation, so they are not listed here.
///
/// Order is not significant (lookup is a linear scan, which is fine — this runs
/// only at config load); the only requirement is that every keysym the compiled
/// defaults rely on has a name here (see `keysym_name_exists` + the contract
/// test).
pub static KEYSYMS: &[(&str, u32)] = &[
    // ── ASCII symbol keys ──
    ("ampersand", 0x26),
    ("apostrophe", 0x27),
    ("asciicircum", 0x5e),
    ("asciitilde", 0x7e),
    ("asterisk", 0x2a),
    ("at", 0x40),
    ("backslash", 0x5c),
    ("backspace", 0xff08),
    ("bar", 0x7c),
    ("braceleft", 0x7b),
    ("braceright", 0x7d),
    ("bracketleft", 0x5b),
    ("bracketright", 0x5d),
    ("colon", 0x3a),
    ("comma", 0x2c),
    ("delete", 0xffff),
    ("dollar", 0x24),
    ("down", 0xff54),
    ("end", 0xff57),
    ("equal", 0x3d),
    ("escape", 0xff1b),
    ("exclam", 0x21),
    ("greater", 0x3e),
    ("grave", 0x60),
    ("home", 0xff50),
    ("insert", 0xff63),
    ("left", 0xff51),
    ("less", 0x3c),
    ("menu", 0xff67),
    ("minus", 0x2d),
    ("next", 0xff56),
    ("numbersign", 0x23),
    ("parenleft", 0x28),
    ("parenright", 0x29),
    ("pause", 0xff13),
    ("percent", 0x25),
    ("period", 0x2e),
    ("plus", 0x2b),
    ("print", 0xff61),
    ("prior", 0xff55),
    ("question", 0x3f),
    ("quotedbl", 0x22),
    ("right", 0xff53),
    ("scroll_lock", 0xff14),
    ("semicolon", 0x3b),
    ("slash", 0x2f),
    ("space", 0x20),
    ("tab", 0xff09),
    ("underscore", 0x5f),
    ("up", 0xff52),
    // ── function keys ──
    ("f1", 0xffbe),
    ("f2", 0xffbf),
    ("f3", 0xffc0),
    ("f4", 0xffc1),
    ("f5", 0xffc2),
    ("f6", 0xffc3),
    ("f7", 0xffc4),
    ("f8", 0xffc5),
    ("f9", 0xffc6),
    ("f10", 0xffc7),
    ("f11", 0xffc8),
    ("f12", 0xffc9),
    // ── enter / aliases ──
    ("return", 0xff0d),
    ("enter", 0xff0d),
    // navigation aliases
    ("pageup", 0xff55),
    ("pagedown", 0xff56),
    // ── keypad ──
    ("kp_0", 0xffb0),
    ("kp_1", 0xffb1),
    ("kp_2", 0xffb2),
    ("kp_3", 0xffb3),
    ("kp_4", 0xffb4),
    ("kp_5", 0xffb5),
    ("kp_6", 0xffb6),
    ("kp_7", 0xffb7),
    ("kp_8", 0xffb8),
    ("kp_9", 0xffb9),
    ("kp_enter", 0xff8d),
    ("kp_add", 0xffab),
    ("kp_subtract", 0xffad),
    ("kp_multiply", 0xffaa),
    ("kp_divide", 0xffaf),
    ("kp_decimal", 0xffae),
    // ── XF86 multimedia / brightness ──
    ("xf86audioraisevolume", 0x1008ff13),
    ("audioraisevolume", 0x1008ff13),
    ("xf86audiolowervolume", 0x1008ff11),
    ("audiolowervolume", 0x1008ff11),
    ("xf86audiomute", 0x1008ff12),
    ("audiomute", 0x1008ff12),
    ("xf86audioplay", 0x1008ff14),
    ("audioplay", 0x1008ff14),
    ("xf86audiostop", 0x1008ff15),
    ("audiostop", 0x1008ff15),
    ("xf86audionext", 0x1008ff17),
    ("audionext", 0x1008ff17),
    ("xf86audioprev", 0x1008ff16),
    ("audioprev", 0x1008ff16),
    ("xf86monbrightnessup", 0x1008ff02),
    ("monbrightnessup", 0x1008ff02),
    ("xf86monbrightnessdown", 0x1008ff03),
    ("monbrightnessdown", 0x1008ff03),
];

/// Convert a supported key name to an X11 keysym. Accepts:
/// - a single `a`–`z` letter or `0`–`9` digit (computed),
/// - any name in `KEYSYMS`,
/// - a raw `0x<hex>` escape for keysyms not in the table.
pub fn keysym_from_name(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    // Raw keysym escape.
    if let Some(hex) = lower.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).ok();
    }
    // Single ASCII letter / digit key.
    if lower.len() == 1 {
        let byte = lower.as_bytes()[0];
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            return Some(u32::from(byte));
        }
    }
    KEYSYMS.iter().find(|(n, _)| *n == lower).map(|(_, k)| *k)
}

/// True when `ksym` is reachable by name (a letter/digit or a `KEYSYMS` entry).
/// Used by the contract test that every compiled-default keysym is expressible.
#[cfg(test)]
fn keysym_name_exists(ksym: u32) -> bool {
    (ksym as u8).is_ascii_lowercase()
        || (ksym as u8).is_ascii_digit()
        || KEYSYMS.iter().any(|(_, k)| *k == ksym)
}

/// Reverse of `keysym_from_name`, for diagnostics only. Falls back to the raw
/// `0x<hex>` escape, which is itself valid config syntax — so whatever this
/// prints can be pasted straight back into a keybinding.
pub fn keysym_name(ksym: u32) -> String {
    if let Some((name, _)) = KEYSYMS.iter().find(|(_, k)| *k == ksym) {
        return (*name).to_string();
    }
    if ksym < 0x80 {
        let byte = ksym as u8;
        if byte.is_ascii_alphanumeric() {
            return (byte as char).to_string();
        }
    }
    format!("0x{ksym:x}")
}

/// Render a modifier mask the way a user writes it in the TOML
/// (`Super+Shift`). Returns an empty string for a bind with no modifiers.
pub fn mods_name(mask: u16) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    for (bit, name) in [
        (u16::from(ModMask::M4), "Super"),
        (u16::from(ModMask::CONTROL), "Control"),
        (u16::from(ModMask::M1), "Alt"),
        (u16::from(ModMask::SHIFT), "Shift"),
    ] {
        if mask & bit != 0 {
            parts.push(name);
        }
    }
    parts.join("+")
}

fn keybind_from_str(input: &str) -> Option<(u16, u32)> {
    let parts: Vec<_> = input
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let (key, modifiers) = parts.split_last()?;
    let keysym = keysym_from_name(key)?;
    let mut mask = 0;
    for modifier in modifiers {
        let bit = match modifier.to_ascii_lowercase().as_str() {
            "super" | "mod4" => u16::from(ModMask::M4),
            "shift" => u16::from(ModMask::SHIFT),
            "control" | "ctrl" => u16::from(ModMask::CONTROL),
            "alt" | "mod1" => u16::from(ModMask::M1),
            _ => return None,
        };
        if mask & bit != 0 {
            return None;
        }
        mask |= bit;
    }
    Some((mask, keysym))
}

/// Parse the TOML action vocabulary (`spawn:...`, `focus:left`, `view:2`, …).
///
/// Delegates to the single shared vocabulary in `core::action` (the same one
/// the IPC channel uses), so a new action is automatically available in both
/// places and can never diverge again (B2/B8). The TOML form is colon
/// separated (`focus:left`); the IPC form is dash/space separated
/// (`focus-left`) — both resolve to the same `Action`.
pub fn action_from_str(input: &str) -> Option<Action> {
    crate::core::action::parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dir;

    /// Parse a TOML string straight into the user model (replaces the old
    /// `toml::from_str` in tests — same fail-fast on syntax errors).
    fn parse_string(source: &str) -> UserConfig {
        let mut d = Diagnostics::default();
        parse_user(source, &mut d).expect("valid TOML")
    }

    fn write_temp(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "maverick-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::write(&path, contents).expect("write temporary config");
        path
    }

    #[test]
    fn missing_file_uses_entire_compiled_config() {
        let path = std::env::temp_dir().join("maverick-config-definitely-missing.toml");
        let _ = std::fs::remove_file(&path);
        let (cfg, _diag) = load_from_path(&path);
        assert_eq!(cfg.keybinds.len(), default_config().keybinds.len());
        assert_eq!(cfg.rules.len(), compiled_config().rules.len());
    }

    #[test]
    fn broken_toml_uses_entire_compiled_config() {
        let path = write_temp("[general\ngaps = nope");
        let (cfg, _diag) = load_from_path(&path);
        let baseline = default_config();
        assert_eq!(cfg.gaps_inner, baseline.gaps_inner);
        assert_eq!(cfg.gaps_outer, baseline.gaps_outer);
        assert_eq!(cfg.keybinds.len(), baseline.keybinds.len());
        assert_eq!(cfg.rules.len(), baseline.rules.len());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fallback_config_has_reachable_workspace_binds() {
        // Regression: the fail-safe baseline (no config file / missing /
        // unreadable / broken TOML) must still produce a WM whose workspaces
        // are reachable. Previously it returned `compiled_config()`, which
        // contains zero workspace keybindings, so the WM booted with
        // `n_tags` workspaces it could never switch to.
        let path = std::env::temp_dir().join(format!(
            "maverick-config-missing-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_file(&path);
        let (cfg, _diag) = load_from_path(&path);
        let sup = u16::from(ModMask::M4);
        let shift_sup = sup | u16::from(ModMask::SHIFT);
        for i in 0..cfg.n_tags.min(9) {
            let ksym = b'1' as u32 + i as u32;
            assert!(
                cfg.keybinds.iter().any(|(m, k, a)| {
                    *m == sup && *k == ksym && matches!(a, &Action::View(v) if v == i)
                }),
                "fallback config missing View({i}) on Super+{}",
                i + 1
            );
            assert!(
                cfg.keybinds.iter().any(|(m, k, a)| {
                    *m == shift_sup
                        && *k == ksym
                        && matches!(a, &Action::MoveToWs(v) if v == i)
                }),
                "fallback config missing MoveToWs({i}) on Super+Shift+{}",
                i + 1
            );
        }
    }

    #[test]
    fn invalid_binding_is_dropped_without_losing_valid_file_values() {
        let path = write_temp(
            r#"
[general]
gaps = 17

[[keybindings]]
key = "super+not-a-key"
action = "grow_col:abc"

[[keybindings]]
key = "super+q"
action = "kill"
"#,
        );
        let (cfg, _diag) = load_from_path(&path);
        assert_eq!(cfg.gaps_inner, 17);
        assert_eq!(cfg.gaps_outer, 17);
        assert!(cfg
            .keybinds
            .iter()
            .any(|(_, key, action)| { *key == u32::from(b'q') && matches!(action, Action::Kill) }));
        assert_eq!(cfg.keybinds.len(), 19); // valid q plus 18 generated workspace binds
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn user_numeric_bind_keeps_other_auto_binds() {
        // B1/T3: a user binding on a digit slot only claims that slot; the other
        // auto-generated workspace binds survive (no full suppression). The
        // claimed slot keeps the user's action, not the generated `view:0`.
        let user = parse_string(
            r#"
[[keybindings]]
key = "super+1"
action = "view:2"
"#,
        );
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        // 1 user bind + 17 remaining generated (the other 8 super-view binds
        // plus the 9 super+shift move binds; super+1's generated view is skipped).
        assert_eq!(cfg.keybinds.len(), 18, "other auto binds must survive");
        let sup = u16::from(ModMask::M4);
        assert!(
            cfg.keybinds
                .iter()
                .any(|(m, k, a)| *m == sup && *k == b'1' as u32 && matches!(a, Action::View(1))),
            "user's claimed slot keeps view:2"
        );
        assert!(
            !cfg.keybinds.iter().any(|(m, k, a)| {
                *m == sup && *k == b'1' as u32 && matches!(a, Action::View(0))
            }),
            "generated view:0 for the claimed slot must be suppressed"
        );
        assert!(
            cfg.keybinds
                .iter()
                .any(|(m, k, a)| *m == sup && *k == b'2' as u32 && matches!(a, Action::View(1))),
            "a non-claimed slot keeps its generated bind"
        );
    }

    #[test]
    fn auto_workspace_binds_false_disables_generation() {
        let user = parse_string(
            r"
[general]
auto_workspace_binds = false
",
        );
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        assert!(!cfg.keybinds.iter().any(|(_, k, a)| {
            (b'1'..=b'9').contains(&(*k as u8))
                && matches!(a, Action::View(_) | Action::MoveToWs(_))
        }));
    }

    #[test]
    fn n_tags_limits_generated_workspace_binds() {
        let user = parse_string(
            r"
[general]
n_tags = 3
",
        );
        let mut diag = Diagnostics::default();
        let cfg = merge_config(compiled_config(), user, &mut diag);
        let numeric = cfg
            .keybinds
            .iter()
            .filter(|(_, k, a)| {
                (b'1'..=b'3').contains(&(*k as u8))
                    && matches!(a, Action::View(_) | Action::MoveToWs(_))
            })
            .count();
        assert_eq!(numeric, 6);
        assert_eq!(cfg.n_tags, 3);
    }

    #[test]
    fn parses_supported_keys_and_actions() {
        assert_eq!(keysym_from_name("F12"), Some(0xffc9));
        assert_eq!(keysym_from_name("z"), Some(u32::from(b'z')));
        assert_eq!(keysym_from_name("unknown"), None);
        assert!(matches!(
            action_from_str("focus:left"),
            Some(Action::FocusDir(Dir::Left))
        ));
        assert!(matches!(
            action_from_str("grow_col:-50"),
            Some(Action::GrowCol(-50))
        ));
        assert!(action_from_str("grow_col:abc").is_none());
        assert!(action_from_str("spawn:").is_none());
    }

    #[test]
    fn list_sections_replace_compiled_lists() {
        let user = parse_string(
            r#"
[[rules]]
class = "Firefox"
float = true

[autostart]
commands = [["example", "--flag"]]
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].class.as_deref(), Some("Firefox"));
        assert_eq!(cfg.autostart, vec![vec!["example", "--flag"]]);
    }

    #[test]
    fn rule_fullscreen_policy_parses_and_all_aliases_agree() {
        // Deny — refuse the app's own runtime fullscreen (F11/EWMH), but leave
        // `Mod4+F` working.
        for key in ["deny_fullscreen", "no_fullscreen"] {
            let user = parse_string(&format!("[[rules]]\nclass = \"firefox\"\n{key} = true\n"));
            let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
            assert_eq!(cfg.rules.len(), 1);
            assert!(
                cfg.rules[0].deny_fullscreen,
                "'{key}' must set Rule::deny_fullscreen"
            );
            assert!(!cfg.rules[0].true_fullscreen);
        }
        // True — exclusive, ribbon-free fullscreen for games. `True` wins over
        // `Deny` when both are set.
        for key in ["true_fullscreen", "exclusive_fullscreen"] {
            let user = parse_string(&format!(
                "[[rules]]\nclass = \"game\"\n{key} = true\ndeny_fullscreen = true\n"
            ));
            let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
            assert_eq!(cfg.rules.len(), 1);
            assert!(
                cfg.rules[0].true_fullscreen,
                "'{key}' must set Rule::true_fullscreen"
            );
            assert!(cfg.rules[0].deny_fullscreen);
        }
        // Defaults stay false.
        let user = parse_string("[[rules]]\nclass = \"firefox\"\n");
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert!(!cfg.rules[0].deny_fullscreen);
        assert!(!cfg.rules[0].true_fullscreen);
    }

    #[test]
    fn rule_ignore_initial_state_parses_and_all_aliases_agree() {
        for key in ["ignore_initial_state", "no_initial_state", "no_maximize"] {
            let user = parse_string(&format!("[[rules]]\nclass = \"firefox\"\n{key} = true\n"));
            let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
            assert_eq!(cfg.rules.len(), 1);
            assert!(
                cfg.rules[0].ignore_initial_state,
                "'{key}' must set Rule::ignore_initial_state"
            );
        }
        // Default (key absent) stays false.
        let user = parse_string("[[rules]]\nclass = \"firefox\"\n");
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert!(!cfg.rules[0].ignore_initial_state);
    }

    #[test]
    fn theme_preset_fills_colors_but_explicit_colors_win() {
        let user = parse_string(
            r#"
[general]
theme = "nord"
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        let (normal, focused, urgent) = crate::config::theme_palette("nord").unwrap();
        assert_eq!(cfg.col_normal, normal);
        assert_eq!(cfg.col_focused, focused);
        assert_eq!(cfg.col_urgent, urgent);

        // [colors] applied after [general], so it overrides the theme.
        let user = parse_string(
            r#"
[general]
theme = "nord"

[colors]
focused = 0x00ff00
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.col_focused, 0x00ff00);
        assert_eq!(cfg.col_normal, normal); // untouched field still from the theme
    }

    #[test]
    fn unknown_theme_name_is_ignored() {
        let baseline = compiled_config();
        let user = parse_string(
            r#"
[general]
theme = "not-a-real-theme"
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.col_normal, baseline.col_normal);
        assert_eq!(cfg.col_focused, baseline.col_focused);
    }

    #[test]
    fn gaps_legacy_alias_sets_both_inner_and_outer() {
        let user = parse_string(
            r"
[general]
gaps = 20
",
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.gaps_inner, 20);
        assert_eq!(cfg.gaps_outer, 20);
    }

    #[test]
    fn gaps_inner_outer_can_be_set_independently() {
        let user = parse_string(
            r"
[general]
gaps_inner = 4
gaps_outer = 12
smart_gaps = true
corner_radius = 10
",
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.gaps_inner, 4);
        assert_eq!(cfg.gaps_outer, 12);
        assert!(cfg.smart_gaps);
        assert_eq!(cfg.corner_radius, 10);
    }

    #[test]
    fn rule_opacity_and_border_width_are_parsed() {
        let user = parse_string(
            r#"
[[rules]]
class = "mpv"
float = true
opacity = 0.9
border_width = 0
"#,
        );
        let cfg = merge_config(compiled_config(), user, &mut Diagnostics::default());
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].opacity, Some(0.9));
        assert_eq!(cfg.rules[0].border_w, Some(0));
    }

    #[test]
    fn shipped_example_config_parses() {
        // The example at config/config.toml must always parse and exercise the
        // documented features (themes, hex colors, viewport keybinds, rules
        // with deny/true fullscreen, autostart grid).
        let (cfg, _diag) = load_from_path(Path::new("config/config.toml"));
        assert!(!cfg.keybinds.is_empty(), "keybindings must parse");
        // Normalization of client map-time state is now a global invariant, so
        // the example neither ships a per-firefox `deny_fullscreen` rule nor a
        // `firefox` rule at all; it must instead leave `honor_initial_state`
        // at its default (false) and still carry the exclusive-fullscreen rule.
        assert!(
            !cfg.honor_initial_state,
            "default must normalize initial state"
        );
        assert!(!cfg
            .rules
            .iter()
            .any(|r| r.class.as_deref() == Some("firefox")));
        assert!(cfg
            .rules
            .iter()
            .any(|r| r.class.as_deref() == Some("steam") && r.true_fullscreen));
        assert_eq!(cfg.col_focused, 0x89b4fa);
        // The example must NOT autostart a second compositor alongside the
        // built-in one (that combination breaks compositing).
        assert!(!cfg
            .autostart
            .iter()
            .any(|c| c.first().is_some_and(|b| b == "picom")));
        // Numeric workspace binds are auto-restored when no digit keybinds exist.
        assert!(cfg
            .keybinds
            .iter()
            .any(|(_, k, a)| { *k == b'1' as u32 && matches!(a, crate::types::Action::View(0)) }));
    }

    #[test]
    fn every_compiled_default_keysym_is_named() {
        // B5: every keysym the compiled config binds must be expressible by name
        // (a letter/digit or a `KEYSYMS` entry) so it can actually be written in
        // config.toml. A missing entry here means a default bind is literally
        // unconfigurable — add it to `KEYSYMS`.
        let cfg = compiled_config();
        for (_mods, ksym, action) in &cfg.keybinds {
            assert!(
                keysym_name_exists(*ksym),
                "keysym {ksym:#x} used by action {action:?} has no name in KEYSYMS"
            );
        }
    }
}
