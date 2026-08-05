//! Safe, optional TOML configuration layered over Maverick's compiled defaults.
//!
//! Syntax/deserialization failures reject the whole file. Semantic failures are
//! isolated to the offending value or list entry and never abort WM startup.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use x11rb::protocol::xproto::ModMask;

use crate::config::{compiled_config, Cfg, Rule};
use crate::log;
use crate::types::{Action, Dir, LayoutKind};

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct UserConfig {
    #[serde(default)]
    general: Option<GeneralCfg>,
    #[serde(default)]
    colors: Option<ColorsCfg>,
    #[serde(default)]
    keybindings: Vec<KeybindEntry>,
    #[serde(default)]
    rules: Vec<RuleEntry>,
    #[serde(default)]
    autostart: Option<AutostartCfg>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct GeneralCfg {
    #[serde(default, alias = "border_w")]
    border_width: Option<u32>,
    #[serde(default)]
    gaps: Option<u32>,
    #[serde(default)]
    n_tags: Option<usize>,
    #[serde(default, alias = "default_col_w")]
    default_col_width: Option<u32>,
    #[serde(default)]
    split_bias: Option<f32>,
    #[serde(default)]
    focus_mouse: Option<bool>,
    #[serde(default)]
    warp_cursor: Option<bool>,
    #[serde(default)]
    tag_names: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ColorsCfg {
    #[serde(default, alias = "col_normal")]
    normal: Option<u32>,
    #[serde(default, alias = "col_focused")]
    focused: Option<u32>,
    #[serde(default, alias = "col_urgent")]
    urgent: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct KeybindEntry {
    #[serde(default)]
    key: String,
    #[serde(default)]
    action: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct RuleEntry {
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    float: bool,
    #[serde(default, alias = "ws")]
    workspace: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct AutostartCfg {
    #[serde(default, alias = "apps", alias = "programs")]
    commands: Vec<Vec<String>>,
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

    let user = match toml::from_str::<UserConfig>(&source) {
        Ok(user) => user,
        Err(e) => {
            log::warn!(
                "config: invalid TOML in '{}': {e}; using compiled defaults",
                path.display()
            );
            return baseline;
        }
    };

    merge_config(baseline, user)
}

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
        cfg.gaps = v;
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
                && entry.title.as_deref().is_none_or(str::is_empty)
            {
                log::warn!(
                    "config: discarded rule #{}: class and title are both empty",
                    index + 1
                );
                return None;
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
                title: entry.title.filter(|s| !s.is_empty()),
                float: entry.float,
                ws,
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
        assert_eq!(cfg.gaps, baseline.gaps);
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
        assert_eq!(cfg.gaps, 17);
        assert!(cfg
            .keybinds
            .iter()
            .any(|(_, key, action)| { *key == u32::from(b'q') && matches!(action, Action::Kill) }));
        assert_eq!(cfg.keybinds.len(), 19); // valid q plus 18 generated workspace binds
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn numeric_user_binding_suppresses_generated_numeric_bindings() {
        let user: UserConfig = toml::from_str(
            r#"
[[keybindings]]
key = "super+1"
action = "view:2"
"#,
        )
        .expect("valid TOML");
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
        let user: UserConfig = toml::from_str(
            r#"
[[rules]]
class = "Firefox"
float = true

[autostart]
commands = [["example", "--flag"]]
"#,
        )
        .expect("valid TOML");
        let cfg = merge_config(compiled_config(), user);
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].class.as_deref(), Some("Firefox"));
        assert_eq!(cfg.autostart, vec![vec!["example", "--flag"]]);
    }
}
