//! Safe, optional TOML configuration layered over Maverick's compiled defaults.
//!
//! Parses the file with `maverick-toml`, a strict, zero-dependency TOML-subset
//! parser (replacing `toml` + `serde`). Syntax failures reject the whole file
//! — the WM starts with compiled defaults and logs the offending line.
//! Semantic failures are isolated to the offending value or list entry and
//! never abort WM startup.

use std::path::{Path, PathBuf};

use maverick_toml::{parse, Event, ParseError, Value};
use x11rb::protocol::xproto::ModMask;

use crate::config::{compiled_config, Cfg, Rule};
use crate::log;
use crate::types::{Action, Dir, LayoutKind};

#[derive(Debug, Default)]
struct UserConfig {
    general: Option<GeneralCfg>,
    colors: Option<ColorsCfg>,
    keybindings: Vec<KeybindEntry>,
    rules: Vec<RuleEntry>,
    autostart: Option<AutostartCfg>,
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
    default_col_width: Option<u32>,
    split_bias: Option<f32>,
    focus_mouse: Option<bool>,
    warp_cursor: Option<bool>,
    tag_names: Option<Vec<String>>,
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

/// Load the standard user config, always returning a usable configuration.
pub fn load_config() -> Cfg {
    let Some(path) = config_path() else {
        return compiled_config();
    };
    load_from_path(&path)
}

pub fn load_from_path(path: &Path) -> Cfg {
    let baseline = compiled_config();
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return baseline,
        Err(e) => {
            log::warn!(
                "config: cannot read '{}': {e}; using compiled defaults",
                path.display()
            );
            return baseline;
        }
    };

    let user = match parse_user(&source) {
        Ok(user) => user,
        Err(e) => {
            log::warn!(
                "config: invalid TOML in '{}' (line {}, {}); using compiled defaults",
                path.display(),
                e.line,
                e.kind
            );
            return baseline;
        }
    };

    merge_config(baseline, user)
}

/// Consume the strict TOML-subset event stream and build the user model.
/// A `ParseError` here means the whole file is rejected by the caller.
fn parse_user(source: &str) -> Result<UserConfig, ParseError> {
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
                    apply_general_key(g, key, &value);
                }
                Some(Cur::Plain("colors")) => {
                    let c = user.colors.get_or_insert_with(ColorsCfg::default);
                    apply_color_key(c, key, &value);
                }
                Some(Cur::Plain("autostart")) => {
                    if matches!(key, "commands" | "apps" | "programs") {
                        user.autostart.get_or_insert_with(AutostartCfg::default);
                        if let Some(grid) = grid_strings(&value) {
                            user.autostart.as_mut().unwrap().commands = grid;
                        } else {
                            log::warn!("config: [autostart].{key} must be a list of string lists; ignoring it");
                        }
                    }
                }
                Some(Cur::Row("keybindings")) => {
                    if let Some(row) = user.keybindings.last_mut() {
                        apply_keybind_key(row, key, &value);
                    }
                }
                Some(Cur::Row("rules")) => {
                    if let Some(row) = user.rules.last_mut() {
                        apply_rule_key(row, key, &value);
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
fn apply_general_key(g: &mut GeneralCfg, key: &str, value: &Value<'_>) {
    match key {
        "border_width" | "border_w" => set_u32(&mut g.border_width, key, value),
        "gaps" => set_u32(&mut g.gaps, key, value),
        "gaps_inner" => set_u32(&mut g.gaps_inner, key, value),
        "gaps_outer" => set_u32(&mut g.gaps_outer, key, value),
        "smart_gaps" => set_bool(&mut g.smart_gaps, key, value),
        "corner_radius" => set_u32(&mut g.corner_radius, key, value),
        "theme" => set_string(&mut g.theme, key, value),
        "n_tags" => set_usize(&mut g.n_tags, key, value),
        "default_col_width" | "default_col_w" => set_u32(&mut g.default_col_width, key, value),
        "split_bias" => set_f32(&mut g.split_bias, key, value),
        "focus_mouse" => set_bool(&mut g.focus_mouse, key, value),
        "warp_cursor" => set_bool(&mut g.warp_cursor, key, value),
        "tag_names" => {
            if let Some(list) = value.as_str_list() {
                g.tag_names = Some(list.iter().map(|s| s.as_ref().to_string()).collect());
            } else {
                warn_bad(key);
            }
        }
        _ => {}
    }
}

/// Map one `[colors]` key onto the model.
fn apply_color_key(c: &mut ColorsCfg, key: &str, value: &Value<'_>) {
    match key {
        "normal" | "col_normal" => set_u32(&mut c.normal, key, value),
        "focused" | "col_focused" => set_u32(&mut c.focused, key, value),
        "urgent" | "col_urgent" => set_u32(&mut c.urgent, key, value),
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
fn apply_rule_key(row: &mut RuleEntry, key: &str, value: &Value<'_>) {
    match key {
        "class" => set_string(&mut row.class, key, value),
        "instance" => set_string(&mut row.instance, key, value),
        "window_type" | "type" => set_string(&mut row.window_type, key, value),
        "title" => set_string(&mut row.title, key, value),
        "float" => row.float = value.as_bool().unwrap_or(false),
        "sticky" => row.sticky = value.as_bool().unwrap_or(false),
        "workspace" | "ws" => set_usize(&mut row.workspace, key, value),
        "size" => {
            if let Some([w, h]) = int_pair(value) {
                row.size = Some([w as u32, h as u32]);
            } else {
                warn_bad(key);
            }
        }
        "position" => {
            if let Some([x, y]) = int_pair(value) {
                row.position = Some([x as i32, y as i32]);
            } else {
                warn_bad(key);
            }
        }
        "opacity" => set_f32(&mut row.opacity, key, value),
        "border_width" | "border_w" => set_u32(&mut row.border_width, key, value),
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

/// Assign `key`'s `u32` value into `slot`, warning when the value has the
/// wrong type (the key is then left untouched).
fn set_u32(slot: &mut Option<u32>, key: &str, value: &Value<'_>) {
    if let Some(v) = value.as_u32() {
        *slot = Some(v);
    } else {
        warn_bad(key);
    }
}

fn set_bool(slot: &mut Option<bool>, key: &str, value: &Value<'_>) {
    if let Some(v) = value.as_bool() {
        *slot = Some(v);
    } else {
        warn_bad(key);
    }
}

fn set_string(slot: &mut Option<String>, key: &str, value: &Value<'_>) {
    if let Some(v) = value.as_str() {
        *slot = Some(v.to_string());
    } else {
        warn_bad(key);
    }
}

fn set_f32(slot: &mut Option<f32>, key: &str, value: &Value<'_>) {
    if let Some(v) = value.as_f64() {
        *slot = Some(v as f32);
    } else {
        warn_bad(key);
    }
}

fn set_usize(slot: &mut Option<usize>, key: &str, value: &Value<'_>) {
    if let Some(v) = value.as_u32() {
        *slot = Some(v as usize);
    } else {
        warn_bad(key);
    }
}

fn warn_bad(key: &str) {
    log::warn!("config: value for '{key}' has an unexpected type; ignoring it");
}

// ── merge ──────────────────────────────────────────────────────────────────

fn merge_config(mut cfg: Cfg, user: UserConfig) -> Cfg {
    if let Some(general) = user.general {
        apply_general(&mut cfg, general);
    }
    if let Some(colors) = user.colors {
        apply_colors(&mut cfg, colors);
    }

    if !user.keybindings.is_empty() {
        cfg.keybinds = parse_keybindings(&user.keybindings, cfg.n_tags);
    }
    if !user.rules.is_empty() {
        cfg.rules = parse_rules(user.rules, cfg.n_tags);
    }
    if let Some(autostart) = user.autostart {
        cfg.autostart = autostart
            .commands
            .into_iter()
            .filter(|cmd| {
                if cmd.first().is_some_and(|bin| !bin.trim().is_empty()) {
                    true
                } else {
                    log::warn!("config: discarded empty autostart command");
                    false
                }
            })
            .collect();
    }

    normalize_tag_names(&mut cfg);
    cfg
}

fn apply_general(cfg: &mut Cfg, general: GeneralCfg) {
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
                log::warn!("config: general.theme '{name}' is not a known preset; ignoring it");
            }
        }
    }
    if let Some(v) = general.n_tags {
        if (1..=9).contains(&v) {
            cfg.n_tags = v;
        } else {
            log::warn!("config: general.n_tags must be between 1 and 9; ignoring {v}");
        }
    }
    if let Some(v) = general.default_col_width {
        if v > 0 {
            cfg.default_col_w = v;
        } else {
            log::warn!("config: general.default_col_width must be greater than zero");
        }
    }
    if let Some(v) = general.split_bias {
        if (0.0..=1.0).contains(&v) {
            cfg.split_bias = v;
        } else {
            log::warn!("config: general.split_bias must be between 0.0 and 1.0; ignoring {v}");
        }
    }
    if let Some(v) = general.focus_mouse {
        cfg.focus_mouse = v;
    }
    if let Some(v) = general.warp_cursor {
        cfg.warp_cursor = v;
    }
    if let Some(names) = general.tag_names {
        if names.is_empty() || names.iter().any(String::is_empty) {
            log::warn!("config: general.tag_names must contain non-empty names; ignoring it");
        } else {
            cfg.tag_names = names;
        }
    }
}

fn apply_colors(cfg: &mut Cfg, colors: ColorsCfg) {
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

fn normalize_tag_names(cfg: &mut Cfg) {
    cfg.tag_names.truncate(cfg.n_tags);
    while cfg.tag_names.len() < cfg.n_tags {
        cfg.tag_names.push((cfg.tag_names.len() + 1).to_string());
    }
}

fn parse_rules(entries: Vec<RuleEntry>, n_tags: usize) -> Vec<Rule> {
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if entry.class.as_deref().is_none_or(str::is_empty)
                && entry.instance.as_deref().is_none_or(str::is_empty)
                && entry.window_type.as_deref().is_none_or(str::is_empty)
                && entry.title.as_deref().is_none_or(str::is_empty)
            {
                log::warn!(
                    "config: discarded rule #{}: class, instance, window_type and title are all empty",
                    index + 1
                );
                return None;
            }
            if let Some(wt) = entry.window_type.as_deref() {
                const KNOWN_TYPES: [&str; 8] = [
                    "normal",
                    "desktop",
                    "dock",
                    "toolbar",
                    "menu",
                    "utility",
                    "splash",
                    "dialog",
                ];
                if !KNOWN_TYPES.contains(&wt.to_ascii_lowercase().as_str()) {
                    log::warn!(
                        "config: discarded rule #{}: unknown window_type '{wt}'",
                        index + 1
                    );
                    return None;
                }
            }
            let ws = match entry.workspace {
                Some(ws) if ws == 0 || ws > n_tags => {
                    log::warn!(
                        "config: discarded rule #{}: workspace {ws} is outside 1..={n_tags}",
                        index + 1
                    );
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
            })
        })
        .collect()
}

fn parse_keybindings(entries: &[KeybindEntry], n_tags: usize) -> Vec<(u16, u32, Action)> {
    let mut parsed = Vec::new();
    let mut has_numeric = false;

    for entry in entries {
        let Some((mods, keysym)) = keybind_from_str(&entry.key) else {
            log::warn!(
                "config: discarded keybinding '{}': invalid key combination",
                entry.key
            );
            continue;
        };
        let Some(action) = action_from_str(&entry.action) else {
            log::warn!(
                "config: discarded keybinding '{}': invalid action '{}'",
                entry.key,
                entry.action
            );
            continue;
        };
        if !action_workspace_is_valid(&action, n_tags) {
            log::warn!(
                "config: discarded keybinding '{}': action workspace is outside 1..={n_tags}",
                entry.key
            );
            continue;
        }
        has_numeric |= entry.key.chars().any(|c| c.is_ascii_digit());
        parsed.push((mods, keysym, action));
    }

    if !has_numeric {
        append_numeric_keybindings(&mut parsed, n_tags);
    }
    parsed
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
        keybinds.push((sup, keysym, Action::View(ws)));
        keybinds.push((shift_sup, keysym, Action::MoveToWs(ws)));
    }
}

/// Convert the supported, layout-independent key names to X11 keysyms.
pub fn keysym_from_name(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    if lower.len() == 1 {
        let byte = lower.as_bytes()[0];
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            return Some(u32::from(byte));
        }
    }
    match lower.as_str() {
        "return" | "enter" => Some(0xff0d),
        "space" => Some(0x0020),
        "tab" => Some(0xff09),
        "f1" => Some(0xffbe),
        "f2" => Some(0xffbf),
        "f3" => Some(0xffc0),
        "f4" => Some(0xffc1),
        "f5" => Some(0xffc2),
        "f6" => Some(0xffc3),
        "f7" => Some(0xffc4),
        "f8" => Some(0xffc5),
        "f9" => Some(0xffc6),
        "f10" => Some(0xffc7),
        "f11" => Some(0xffc8),
        "f12" => Some(0xffc9),
        _ => None,
    }
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

/// Parse the TOML action vocabulary (`spawn:...`, `focus:left`, `view:2`, etc.).
pub fn action_from_str(input: &str) -> Option<Action> {
    let input = input.trim();
    let (name, argument) = input
        .split_once(':')
        .map_or((input, None), |(name, arg)| (name, Some(arg.trim())));
    let name = name.trim().to_ascii_lowercase().replace('-', "_");

    match name.as_str() {
        "spawn" => {
            let command: Vec<String> = argument?
                .split_whitespace()
                .map(std::string::ToString::to_string)
                .collect();
            (!command.is_empty()).then_some(Action::Spawn(command))
        }
        "kill" if argument.is_none() => Some(Action::Kill),
        "toggle_float" if argument.is_none() => Some(Action::ToggleFloat),
        "toggle_fullscreen" if argument.is_none() => Some(Action::ToggleFullscreen),
        "cycle_layout" if argument.is_none() => Some(Action::CycleLayout),
        "new_column" if argument.is_none() => Some(Action::NewColumn),
        "collapse_column" if argument.is_none() => Some(Action::CollapseColumn),
        "restart" if argument.is_none() => Some(Action::Restart),
        "quit" if argument.is_none() => Some(Action::Quit),
        "focus" => parse_dir(argument?).map(Action::FocusDir),
        "move" => parse_dir(argument?).map(Action::MoveDir),
        "focus_mon" => parse_dir(argument?).map(Action::FocusMon),
        "move_mon" => parse_dir(argument?).map(Action::MoveMon),
        "layout" => match argument?.to_ascii_lowercase().as_str() {
            "column" => Some(Action::SetLayout(LayoutKind::Column)),
            "grid" => Some(Action::SetLayout(LayoutKind::Grid)),
            _ => None,
        },
        "grow_col" => argument?.parse::<i32>().ok().map(Action::GrowCol),
        "view" => parse_workspace(argument?).map(Action::View),
        "move_to_ws" => parse_workspace(argument?).map(Action::MoveToWs),
        _ => None,
    }
}

fn parse_dir(input: &str) -> Option<Dir> {
    match input.trim().to_ascii_lowercase().as_str() {
        "left" => Some(Dir::Left),
        "right" => Some(Dir::Right),
        "up" => Some(Dir::Up),
        "down" => Some(Dir::Down),
        "next" => Some(Dir::Next),
        "prev" => Some(Dir::Prev),
        _ => None,
    }
}

fn parse_workspace(input: &str) -> Option<usize> {
    input.trim().parse::<usize>().ok()?.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a TOML string straight into the user model (replaces the old
    /// `toml::from_str` in tests — same fail-fast on syntax errors).
    fn parse_string(source: &str) -> UserConfig {
        parse_user(source).expect("valid TOML")
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
        let cfg = load_from_path(&path);
        assert_eq!(cfg.keybinds.len(), compiled_config().keybinds.len());
        assert_eq!(cfg.rules.len(), compiled_config().rules.len());
    }

    #[test]
    fn broken_toml_uses_entire_compiled_config() {
        let path = write_temp("[general\ngaps = nope");
        let cfg = load_from_path(&path);
        let baseline = compiled_config();
        assert_eq!(cfg.gaps_inner, baseline.gaps_inner);
        assert_eq!(cfg.gaps_outer, baseline.gaps_outer);
        assert_eq!(cfg.keybinds.len(), baseline.keybinds.len());
        assert_eq!(cfg.rules.len(), baseline.rules.len());
        let _ = std::fs::remove_file(path);
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
        let cfg = load_from_path(&path);
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
    fn numeric_user_binding_suppresses_generated_numeric_bindings() {
        let user = parse_string(
            r#"
[[keybindings]]
key = "super+1"
action = "view:2"
"#,
        );
        let cfg = merge_config(compiled_config(), user);
        assert_eq!(cfg.keybinds.len(), 1);
        assert!(matches!(cfg.keybinds[0].2, Action::View(1)));
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
        let cfg = merge_config(compiled_config(), user);
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].class.as_deref(), Some("Firefox"));
        assert_eq!(cfg.autostart, vec![vec!["example", "--flag"]]);
    }

    #[test]
    fn theme_preset_fills_colors_but_explicit_colors_win() {
        let user = parse_string(
            r#"
[general]
theme = "nord"
"#,
        );
        let cfg = merge_config(compiled_config(), user);
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
        let cfg = merge_config(compiled_config(), user);
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
        let cfg = merge_config(compiled_config(), user);
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
        let cfg = merge_config(compiled_config(), user);
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
        let cfg = merge_config(compiled_config(), user);
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
        let cfg = merge_config(compiled_config(), user);
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].opacity, Some(0.9));
        assert_eq!(cfg.rules[0].border_w, Some(0));
    }

    #[test]
    fn rule_hex_and_decimal_colors_are_equivalent() {
        let user = parse_string(
            r"
[colors]
normal = 0x112233
focused = 1122867
",
        );
        let cfg = merge_config(compiled_config(), user);
        assert_eq!(cfg.col_normal, 0x112233);
        assert_eq!(cfg.col_focused, 1122867);
    }
}
