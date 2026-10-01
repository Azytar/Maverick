//! Single source of truth for the *vocabulary* of actions — the canonical
//! name of every `Action` variant, and one parser that both the TOML config
//! and the IPC/`maverickctl` channels delegate to.
//!
//! Keeping the vocabulary in one place (and deriving `name()` via an exhaustive
//! `match` over `Action`) is what keeps the two channels from drifting: a new
//! `Action` variant without a name here is a compile error, not a silently
//! unreachable action.
//!
//! # Parser contract
//!
//! `parse` accepts both the canonical `verb:arg` form and legacy
//! dash-separated forms (`focus-left`). The `ArgKind` table defines the
//! machine-checkable contract for every verb; the round-trip test in `tests`
//! walks the table so no entry can be added without being parseable.
//!
//! # Invariants
//!
//! Every `Action` variant must have a `name()` entry. `ws_from` rejects
//! workspace index 0 (workspaces are 1-indexed in the config/protocol
//! vocabulary). Unknown or malformed input yields `None` — the caller logs and
//! ignores it rather than guessing.

use crate::types::{Action, Dir, LayoutKind, WindowId};

/// What argument shape an action verb accepts. Used by the `ACTIONS` table as
/// a machine-checkable contract of the vocabulary (see `tests` for the round
/// trip that exercises every entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// No argument expected.
    None,
    /// A `Dir` (`left`/`right`/`up`/`down`/`next`/`prev`).
    Dir,
    /// A `LayoutKind` (`column`).
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
/// `name()` for its variant — this table is the human-readable contract, and
/// `name()` (the exhaustive `match`) is the compile-time guard.
pub static ACTIONS: &[(&str, ArgKind)] = &[
    ("spawn", ArgKind::Cmd),
    ("kill", ArgKind::None),
    ("focus", ArgKind::Dir),
    ("move", ArgKind::Dir),
    ("toggle_float", ArgKind::None),
    ("toggle_fullscreen", ArgKind::None),
    ("toggle_maximize", ArgKind::None),
    ("set_layout", ArgKind::Layout),
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
/// `Action`: adding a variant without a name here is a compile error.
pub fn name(a: &Action) -> &'static str {
    match a {
        Action::Spawn(_) => "spawn",
        Action::Kill => "kill",
        Action::FocusDir(_) => "focus",
        Action::MoveDir(_) => "move",
        Action::ToggleFloat => "toggle_float",
        Action::ToggleFullscreen => "toggle_fullscreen",
        Action::ToggleMaximize => "toggle_maximize",
        Action::FocusWindow(..) => "focus_window",
        Action::MoveWindow(..) => "move_window",
        Action::CloseWindow(..) => "close_window",
        Action::ToggleFloatWindow(..) => "float_window",
        Action::ToggleFullscreenWindow(..) => "fullscreen_window",
        Action::SetLayout(_) => "set_layout",
        Action::GrowCol(_) => "grow_col",
        Action::GrowColPct(_) => "grow_col_pct",
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

/// Parse a window id as a control client writes it.
///
/// Both spellings an id appears in are accepted: hexadecimal (`0x42003`, what
/// `maverickctl window list` prints and what the EWMH conversation uses) and
/// plain decimal. Nothing else is — a trailing word, an empty argument, a
/// negative number all fail, because an id that parsed to *some* number would
/// address a window the caller did not name.
///
/// Zero is refused because it is not a window: X11 spells "no window" as 0, so
/// accepting it would turn a failed lookup into a request to act on nothing.
fn parse_window_id(s: &str) -> Option<WindowId> {
    let t = s.trim();
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => t.parse::<u32>().ok(),
    }?;
    (parsed != 0).then_some(parsed)
}

/// Split `"<dir> <id>"` into its parts, for the one window-targeted action that
/// takes a direction as well as a target.
fn dir_and_window(arg: &str) -> Option<(Dir, WindowId)> {
    let (dir, id) = arg.split_once(char::is_whitespace)?;
    Some((dir_from(dir)?, parse_window_id(id)?))
}

/// Parse a finite float, rejecting the values `f32::from_str` admits but a
/// geometry argument must never carry.
///
/// `parse::<f32>` accepts `NaN`, `inf` and any decimal that overflows to it, so
/// without this guard a spelling like `grow_col_pct:NaN` becomes a live action.
/// Every consumer of these numbers divides or scales them into a rect, and a
/// non-finite one either saturates to a meaningless `0`/`i32::MAX` cast or
/// poisons a stored weight. Rejecting here is the contract that a parsed
/// action's argument is a number a projection can use.
fn parse_finite_f32(s: &str) -> Option<f32> {
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Parse a signed percentage, with or without the sign character a user types.
///
/// `+10%`, `10`, `-5%` and `-5` all mean the same three things respectively;
/// the trailing `%` is stripped rather than required because a control client
/// that already rendered the number as a percentage should not have to
/// reproduce the decoration.
fn parse_percent(s: &str) -> Option<f32> {
    parse_finite_f32(s.trim().trim_end_matches('%').trim())
}

fn layout_from(s: &str) -> Option<LayoutKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "column" => Some(LayoutKind::Column),
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
    // predate the colon-separated TOML grammar and must keep parsing, so old
    // `maverickctl` invocations and user scripts do not break. Each is
    // equivalent to the canonical form below (`focus-left` == `focus:left`,
    // `shrink-col N` == `grow_col:-N`).
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
        "focus" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::FocusDir),
        "move" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::MoveDir),
        "toggle_float" => none_if_arg(has_arg, Action::ToggleFloat),
        "toggle_fullscreen" => none_if_arg(has_arg, Action::ToggleFullscreen),
        "toggle_maximize" => none_if_arg(has_arg, Action::ToggleMaximize),
        // Window-targeted verbs. Not in the `ACTIONS` table: their target is
        // resolved at dispatch time by whoever asks, so there is no fixed
        // argument to put in a TOML keymap — they exist for the control
        // channel, where the target is part of the request.
        "focus_window" => has_arg
            .then(|| parse_window_id(arg))
            .flatten()
            .map(Action::FocusWindow),
        "close_window" | "kill_window" => has_arg
            .then(|| parse_window_id(arg))
            .flatten()
            .map(Action::CloseWindow),
        "float_window" | "toggle_float_window" => has_arg
            .then(|| parse_window_id(arg))
            .flatten()
            .map(Action::ToggleFloatWindow),
        "fullscreen_window" | "toggle_fullscreen_window" => has_arg
            .then(|| parse_window_id(arg))
            .flatten()
            .map(Action::ToggleFullscreenWindow),
        "move_window" => has_arg
            .then(|| dir_and_window(arg))
            .flatten()
            .map(|(dir, win)| Action::MoveWindow(dir, win)),
        "grow_col_pct" => has_arg
            .then(|| parse_percent(arg))
            .flatten()
            .map(Action::GrowColPct),
        "set_layout" => has_arg
            .then(|| layout_from(arg))
            .flatten()
            .map(Action::SetLayout),
        "layout" => has_arg
            .then(|| layout_from(arg))
            .flatten()
            .map(Action::SetLayout),
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
        "focus_mon" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::FocusMon),
        "move_mon" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::MoveMon),
        "restart" => none_if_arg(has_arg, Action::Restart),
        "quit" => none_if_arg(has_arg, Action::Quit),
        "toggle_overview" => none_if_arg(has_arg, Action::ToggleOverview),
        "overview_nav" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::OverviewNav),
        "overview_enter" => none_if_arg(has_arg, Action::OverviewEnter),
        "viewport_zoom" => {
            let delta = if has_arg { parse_finite_f32(arg)? } else { 0.2 };
            Some(Action::ViewportZoom(delta))
        }
        "page_snap" => has_arg
            .then(|| dir_from(arg))
            .flatten()
            .map(Action::PageSnap),
        _ => None,
    }
}

/// An argumentless verb rejects a stray argument instead of silently ignoring
/// it, so a typo in a user's keymap surfaces as "unknown action" rather than as
/// a command that quietly does something else.
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
                ArgKind::Layout => ":column".to_string(),
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

    /// A window id arrives from a control client in whichever spelling it was
    /// printed in. Both must resolve to the same window, and everything else
    /// must fail: an id that parsed to *some* number would address a window the
    /// caller did not name.
    #[test]
    fn window_ids_parse_in_every_spelling_a_client_prints() {
        for text in ["0x42003", "0X42003", "270339"] {
            assert_eq!(parse_window_id(text), Some(0x42003), "{text}");
        }
        for bad in [
            "",
            "  ",
            "0x",
            "0xZZ",
            "-1",
            "0",
            "0x0",
            "42003 42004",
            "42003x",
            "twelve",
        ] {
            assert_eq!(parse_window_id(bad), None, "{bad:?} must not parse");
        }
    }

    /// Zero is X11's spelling of "no window"; accepting it would turn a failed
    /// lookup into a request to act on nothing.
    #[test]
    fn a_zero_window_id_is_refused() {
        assert_eq!(parse_window_id("0"), None);
        assert_eq!(parse_window_id("0x0"), None);
        assert!(parse("focus_window 0").is_none());
    }

    #[test]
    fn window_targeted_actions_round_trip() {
        let cases: &[(Action, &str)] = &[
            (Action::FocusWindow(0x42003), "focus_window:0x42003"),
            (Action::CloseWindow(0x42003), "close_window:0x42003"),
            (Action::ToggleFloatWindow(0x42003), "float_window:0x42003"),
            (
                Action::ToggleFullscreenWindow(0x42003),
                "fullscreen_window:0x42003",
            ),
            (
                Action::MoveWindow(Dir::Left, 0x42003),
                "move_window:left 0x42003",
            ),
        ];
        for (action, text) in cases {
            assert_eq!(parse(text), Some(action.clone()), "{text}");
            assert_eq!(name(action), text.split([':', ' ']).next().expect("verb"));
        }
    }

    /// A control client and a keybinding reach the same `Action`, so an alias
    /// is a convenience, not a second behaviour.
    #[test]
    fn window_targeted_aliases_name_the_same_action() {
        for (alias, canonical) in [
            ("kill_window:0x1", "close_window:0x1"),
            ("toggle_float_window:0x1", "float_window:0x1"),
            ("toggle_fullscreen_window:0x1", "fullscreen_window:0x1"),
        ] {
            assert_eq!(parse(alias), parse(canonical), "{alias}");
        }
    }

    /// A percentage is what a user means by "ten percent wider", and both the
    /// sign a user types and the `%` decoration are optional decoration.
    #[test]
    fn percentages_parse_with_or_without_their_decoration() {
        for (text, want) in [
            ("+10%", 10.0),
            ("10%", 10.0),
            ("10", 10.0),
            ("-5%", -5.0),
            ("-5", -5.0),
            (" 0.5%", 0.5),
        ] {
            let input = format!("grow_col_pct:{text}");
            assert_eq!(parse(&input), Some(Action::GrowColPct(want)), "{input}");
        }
        for bad in ["", "%", "ten%", "+%"] {
            let input = format!("grow_col_pct:{bad}");
            assert_eq!(parse(&input), None, "{input:?} must not parse");
        }
        // The colon form and the space form are the same request.
        assert_eq!(parse("grow_col_pct +10%"), parse("grow_col_pct:+10%"));
    }

    /// `move_window` is the one window-targeted verb that takes a direction too,
    /// and both halves have to be present and valid.
    #[test]
    fn move_window_needs_a_direction_and_an_id() {
        assert_eq!(
            parse("move_window:right 0x2a"),
            Some(Action::MoveWindow(Dir::Right, 0x2a))
        );
        for bad in [
            "move_window 0x2a",
            "move_window:sideways 0x2a",
            "move_window:right",
            "move_window:right 0",
        ] {
            assert_eq!(parse(bad), None, "{bad:?} must not parse");
        }
    }

    /// These verbs are not in the `ACTIONS` table on purpose — a keymap has no
    /// place to put a target resolved at dispatch time — so the table-driven
    /// tests must not start expecting them.
    #[test]
    fn window_targeted_verbs_are_not_keymap_actions() {
        for (verb, sample) in [
            ("focus_window", "0x1"),
            ("close_window", "0x1"),
            ("float_window", "0x1"),
            ("fullscreen_window", "0x1"),
            ("move_window", "left 0x1"),
            ("grow_col_pct", "10%"),
        ] {
            assert!(
                !ACTIONS.iter().any(|(name, _)| *name == verb),
                "{verb} must not be offered as a keymap action"
            );
            let input = format!("{verb}:{sample}");
            assert!(parse(&input).is_some(), "{input} must parse");
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
        assert!(matches!(parse("shrink-col 40"), Some(Action::GrowCol(-40))));
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
        // A sample of the overlap between the two channels.
        assert_eq!(parse("view 3"), parse("view:3"));
        assert_eq!(parse("grow-col 40"), parse("grow_col:40"));
        assert_eq!(parse("spawn alacritty"), parse("spawn:alacritty"));
        assert_eq!(parse("layout column"), parse("set_layout:column"));
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

    /// `f32::from_str` admits `NaN`, `inf` and overflowing decimals. Every
    /// consumer of these arguments scales or divides them into a rect, so a
    /// non-finite value must never become a live action — the request is
    /// malformed, not a request to change nothing.
    #[test]
    fn a_non_finite_float_argument_is_rejected() {
        for s in [
            "viewport_zoom:NaN",
            "viewport_zoom:nan",
            "viewport_zoom:inf",
            "viewport_zoom:-inf",
            "viewport_zoom:Infinity",
            "viewport_zoom:1e400",
            "viewport_zoom:-1e400",
            "grow_col_pct:NaN",
            "grow_col_pct:inf",
            "grow_col_pct:-inf",
            "grow_col_pct:1e400",
            "grow_col_pct:nan%",
        ] {
            assert!(
                parse(s).is_none(),
                "{s:?} must be rejected, not parsed into an action"
            );
        }
    }

    /// The guard above must not reject magnitudes an X11 wire can carry: the
    /// rejection is about finiteness, not about size.
    #[test]
    fn a_large_but_finite_float_argument_is_still_accepted() {
        assert_eq!(
            parse("viewport_zoom:1000"),
            Some(Action::ViewportZoom(1000.0)),
            "a finite magnitude is a real request; range is the command's business"
        );
        assert_eq!(parse("grow_col_pct:1000"), Some(Action::GrowColPct(1000.0)));
        assert_eq!(parse("viewport_zoom:0.2"), Some(Action::ViewportZoom(0.2)));
        assert_eq!(
            parse("viewport_zoom:-1.5"),
            Some(Action::ViewportZoom(-1.5))
        );
    }
}
