// maverick/src/core/action.rs
// Single source of truth for the *vocabulary* of actions — the canonical name
// of every `Action` variant, and one parser that both the TOML config and the
// IPC/`maverickctl` channels delegate to.
//
// Keeping the vocabulary in one place (and deriving `name()` via an exhaustive
// `match` over `Action`) is what prevents the two channels from drifting apart
// again: if a new `Action` variant is added without a name here, this module
// stops compiling (B2/B8 guard).

use crate::types::{Action, Dir, LayoutKind};

/// What argument shape an action verb accepts. Used by the `ACTIONS` table as
/// a machine-checkable contract of the vocabulary (see `tests` for the round
/// trip that exercises every entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// No argument expected.
    None,
    /// A `Dir` (`left`/`right`/`up`/`down`/`next`/`prev`).
    Dir,
    /// A `LayoutKind` (`column`/`grid`).
    Layout,
    /// A signed integer (`i32`).
    I32,
    /// An optional float magnitude; a bare verb defaults to a sensible step.
    F32Opt,
    /// A 1-based workspace number (1 → index 0).
    Ws,
    /// The rest of the line is a free-form command + args.
    Cmd,
}

/// The canonical action vocabulary. Every entry's name must also be returned by
/// `name()` for its variant — this table is the human-readable contract; `name`
/// (the exhaustive `match`) is the compile-time guard.
pub static ACTIONS: &[(&str, ArgKind)] = &[
    ("spawn", ArgKind::Cmd),
    ("kill", ArgKind::None),
    ("focus", ArgKind::Dir),
    ("move", ArgKind::Dir),
    ("toggle_float", ArgKind::None),
    ("toggle_fullscreen", ArgKind::None),
    ("toggle_maximize", ArgKind::None),
    ("set_layout", ArgKind::Layout),
    ("cycle_layout", ArgKind::None),
    ("grow_col", ArgKind::I32),
    ("new_column", ArgKind::None),
    ("collapse_column", ArgKind::None),
    ("view", ArgKind::Ws),
    ("move_to_ws", ArgKind::Ws),
    ("focus_mon", ArgKind::Dir),
    ("move_mon", ArgKind::Dir),
    ("restart", ArgKind::None),
    ("quit", ArgKind::None),
    ("toggle_overview", ArgKind::None),
    ("overview_nav", ArgKind::Dir),
    ("overview_enter", ArgKind::None),
    ("viewport_zoom", ArgKind::F32Opt),
    ("page_snap", ArgKind::Dir),
];

/// Canonical `snake_case` name of an `Action` (no argument). Exhaustive over
/// `Action`: adding a variant without a name here is a compile error, which is
/// exactly the mechanism that stops B2/B8 from recurring.
pub fn name(a: &Action) -> &'static str {
    match a {
        Action::Spawn(_) => "spawn",
        Action::Kill => "kill",
        Action::FocusDir(_) => "focus",
        Action::MoveDir(_) => "move",
        Action::ToggleFloat => "toggle_float",
        Action::ToggleFullscreen => "toggle_fullscreen",
        Action::ToggleMaximize => "toggle_maximize",
        Action::SetLayout(_) => "set_layout",
        Action::CycleLayout => "cycle_layout",
        Action::GrowCol(_) => "grow_col",
        Action::NewColumn => "new_column",
        Action::CollapseColumn => "collapse_column",
        Action::View(_) => "view",
        Action::MoveToWs(_) => "move_to_ws",
        Action::FocusMon(_) => "focus_mon",
        Action::MoveMon(_) => "move_mon",
        Action::Restart => "restart",
        Action::Quit => "quit",
        Action::ToggleOverview => "toggle_overview",
        Action::OverviewNav(_) => "overview_nav",
        Action::OverviewEnter => "overview_enter",
        Action::ViewportZoom(_) => "viewport_zoom",
        Action::PageSnap(_) => "page_snap",
    }
}

fn dir_from(s: &str) -> Option<Dir> {
    match s.trim().to_ascii_lowercase().as_str() {
        "left" => Some(Dir::Left),
        "right" => Some(Dir::Right),
        "up" => Some(Dir::Up),
        "down" => Some(Dir::Down),
        "next" => Some(Dir::Next),
        "prev" => Some(Dir::Prev),
        _ => None,
    }
}

fn layout_from(s: &str) -> Option<LayoutKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "column" => Some(LayoutKind::Column),
        "grid" => Some(LayoutKind::Grid),
        _ => None,
    }
}

/// Parse a 1-based workspace number into a 0-based index (`0` is rejected).
fn ws_from(s: &str) -> Option<usize> {
    let n = s.trim().parse::<usize>().ok()?;
    if n == 0 {
        None
    } else {
        Some(n - 1)
    }
}

/// Split a raw input into its verb and argument on the first `:` or
/// whitespace. If neither is present the whole input is the verb and the
/// argument is empty.
fn split_verb_arg(input: &str) -> (&str, &str) {
    let bytes = input.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' || b.is_ascii_whitespace() {
            return (&input[..i], &input[i + 1..]);
        }
    }
    (input, "")
}

/// Parse an action name shared by both the TOML config (`focus:left`,
/// `grow_col:-50`, `spawn:cmd`, …) and the IPC/control-socket channel
/// (`focus-left`, `grow-col 40`, `spawn cmd`, …). All spellings resolve to the
/// same `Action`. Returns `None` for unknown/invalid input (the caller logs and
/// ignores it).
pub fn parse(input: &str) -> Option<Action> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // Legacy fused IPC verbs: `focus-left`, `move-down`, `shrink-col N`. These
    // predate the colon-separated TOML grammar and must keep working so old
    // `maverickctl` invocations and scripts don't break.
    if let Some(rest) = input.strip_prefix("focus-") {
        return dir_from(rest).map(Action::FocusDir);
    }
    if let Some(rest) = input.strip_prefix("move-") {
        return dir_from(rest).map(Action::MoveDir);
    }
    if let Some(rest) = input.strip_prefix("shrink-col") {
        let n = rest.trim().parse::<i32>().ok()?;
        return Some(Action::GrowCol(-n));
    }

    // Colon/space separated: `verb:arg` or `verb arg`.
    let (raw_verb, raw_arg) = split_verb_arg(input);
    let verb = raw_verb.to_ascii_lowercase().replace('-', "_");
    let arg = raw_arg.trim();
    let has_arg = !arg.is_empty();

    match verb.as_str() {
        "spawn" => {
            if !has_arg {
                return None;
            }
            let command: Vec<String> = arg.split_whitespace().map(str::to_string).collect();
            if command.is_empty() {
                None
            } else {
                Some(Action::Spawn(command))
            }
        }
        "kill" => none_if_arg(has_arg, Action::Kill),
        "focus" => has_arg.then(|| dir_from(arg)).flatten().map(Action::FocusDir),
        "move" => has_arg.then(|| dir_from(arg)).flatten().map(Action::MoveDir),
        "toggle_float" => none_if_arg(has_arg, Action::ToggleFloat),
        "toggle_fullscreen" => none_if_arg(has_arg, Action::ToggleFullscreen),
        "toggle_maximize" => none_if_arg(has_arg, Action::ToggleMaximize),
        "set_layout" => has_arg
            .then(|| layout_from(arg))
            .flatten()
            .map(Action::SetLayout),
        "layout" => has_arg
            .then(|| layout_from(arg))
            .flatten()
            .map(Action::SetLayout),
        "cycle_layout" => none_if_arg(has_arg, Action::CycleLayout),
        "grow_col" => has_arg
            .then(|| arg.parse::<i32>().ok())
            .flatten()
            .map(Action::GrowCol),
        "new_column" => none_if_arg(has_arg, Action::NewColumn),
        "collapse_column" => none_if_arg(has_arg, Action::CollapseColumn),
        "view" => has_arg.then(|| ws_from(arg)).flatten().map(Action::View),
        "move_to_ws" => has_arg
            .then(|| ws_from(arg))
            .flatten()
            .map(Action::MoveToWs),
        "focus_mon" => has_arg.then(|| dir_from(arg)).flatten().map(Action::FocusMon),
        "move_mon" => has_arg.then(|| dir_from(arg)).flatten().map(Action::MoveMon),
        "restart" => none_if_arg(has_arg, Action::Restart),
        "quit" => none_if_arg(has_arg, Action::Quit),
        "toggle_overview" => none_if_arg(has_arg, Action::ToggleOverview),
        "overview_nav" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::OverviewNav),
        "overview_enter" => none_if_arg(has_arg, Action::OverviewEnter),
        "viewport_zoom" => {
            let delta = if has_arg {
                arg.parse::<f32>().ok()?
            } else {
                0.2
            };
            Some(Action::ViewportZoom(delta))
        }
        "page_snap" => has_arg.then(|| dir_from(arg)).flatten().map(Action::PageSnap),
        _ => None,
    }
}

/// Helper: an argumentless action accepts only when no argument was supplied.
#[inline]
fn none_if_arg(has_arg: bool, action: Action) -> Option<Action> {
    if has_arg {
        None
    } else {
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Action;

    #[test]
    fn every_canonical_name_parses() {
        for (verb, kind) in ACTIONS {
            let sample = match kind {
                ArgKind::None => String::new(),
                ArgKind::Dir => ":left".to_string(),
                ArgKind::Layout => ":grid".to_string(),
                ArgKind::I32 => ":-50".to_string(),
                ArgKind::F32Opt => ":0.2".to_string(),
                ArgKind::Ws => ":2".to_string(),
                ArgKind::Cmd => ":alacritty -e htop".to_string(),
            };
            let input = format!("{verb}{sample}");
            assert!(
                parse(&input).is_some(),
                "canonical action '{input}' must parse"
            );
        }
    }

    #[test]
    fn round_trip_arg_free_variants() {
        let samples = [
            Action::Kill,
            Action::ToggleFloat,
            Action::ToggleFullscreen,
            Action::ToggleMaximize,
            Action::CycleLayout,
            Action::NewColumn,
            Action::CollapseColumn,
            Action::Restart,
            Action::Quit,
            Action::ToggleOverview,
            Action::OverviewEnter,
        ];
        for a in samples {
            assert_eq!(
                parse(name(&a)),
                Some(a.clone()),
                "name->parse round trip failed for {a:?}",
            );
        }
    }

    #[test]
    fn legacy_ipc_aliases_still_parse() {
        assert!(matches!(
            parse("focus-left"),
            Some(Action::FocusDir(Dir::Left))
        ));
        assert!(matches!(
            parse("move-down"),
            Some(Action::MoveDir(Dir::Down))
        ));
        assert!(matches!(
            parse("shrink-col 40"),
            Some(Action::GrowCol(-40))
        ));
        // And they match the colon TOML form exactly.
        assert_eq!(
            parse("focus-left"),
            parse("focus:left"),
            "IPC focus-left must equal TOML focus:left"
        );
        assert_eq!(
            parse("move-up"),
            parse("move:up"),
            "IPC move-up must equal TOML move:up"
        );
        assert_eq!(
            parse("shrink-col 50"),
            parse("grow_col:-50"),
            "IPC shrink-col must equal TOML grow_col:-N"
        );
    }

    #[test]
    fn toml_and_ipc_yeargent_same_results() {
        // A sampling that the two channels agree everywhere they overlap.
        assert_eq!(parse("view 3"), parse("view:3"));
        assert_eq!(parse("grow-col 40"), parse("grow_col:40"));
        assert_eq!(parse("spawn alacritty"), parse("spawn:alacritty"));
        assert_eq!(parse("layout grid"), parse("set_layout:grid"));
        assert_eq!(parse("focus_mon next"), parse("focus_mon:next"));
        assert_eq!(parse("page_snap right"), parse("page_snap:right"));
        assert_eq!(parse("toggle_overview"), parse("toggle_overview"));
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        assert!(parse("frobnicate").is_none());
        assert!(parse("").is_none());
        assert!(parse("layout bogus").is_none());
        assert!(parse("view 0").is_none());
        assert!(parse("grow_col:abc").is_none());
        assert!(parse("spawn:").is_none());
    }
}
