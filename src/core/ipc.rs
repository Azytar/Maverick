// maverick/src/core/ipc.rs
// Pure IPC helpers: serialize WM state to JSON for the control socket, and
// parse action names sent via `dispatch`. No X11, no side effects — this is
// the vocabulary the outside world (maverickctl, bars, scripts) speaks.

// This is a small hand-rolled JSON serializer; `push_str(&format!(...))` is the
// clearest idiom here and avoids a serde dependency (project keeps zero extra
// deps for the core).
#![allow(clippy::format_push_string)]

use crate::config::Cfg;
use crate::types::{Action, Dir, LayoutKind, State};

/// Minimal JSON string escaper (no serde dependency, matching the rest of the
/// project's zero-extra-deps stance). Escapes quotes, backslashes, and control
/// characters that would break a JSON document.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Serialize the live WM `State` into a compact JSON snapshot for external
/// tools. Includes per-monitor active workspace, focused window + title, and
/// per-workspace occupancy/layout. Deterministic field order so consumers can
/// diff snapshots cheaply.
pub fn state_json(state: &State, cfg: &Cfg) -> String {
    let mut s = String::with_capacity(512);
    s.push('{');

    s.push_str(&format!("\"sel_mon\":{},", state.sel_mon));
    s.push_str(&format!("\"focus_serial\":{},", state.focus_serial));
    s.push_str(&format!("\"status\":\"{}\",", esc(&state.status)));

    // monitors
    s.push_str("\"monitors\":[");
    for (mi, mon) in state.monitors.iter().enumerate() {
        if mi > 0 {
            s.push(',');
        }
        s.push('{');
        s.push_str(&format!("\"index\":{mi},"));
        s.push_str(&format!("\"active_ws\":{},", mon.active_ws));
        s.push_str(&format!("\"show_bar\":{},", mon.show_bar));

        // focused window + its title/class
        match mon.focused {
            Some(w) => {
                s.push_str(&format!("\"focused\":{w},"));
                if let Some(c) = state.clients.get(&w) {
                    s.push_str(&format!("\"focused_title\":\"{}\",", esc(&c.name)));
                    s.push_str(&format!("\"focused_class\":\"{}\",", esc(&c.class)));
                } else {
                    s.push_str("\"focused_title\":\"\",\"focused_class\":\"\",");
                }
            }
            None => s.push_str("\"focused\":null,\"focused_title\":\"\",\"focused_class\":\"\","),
        }

        // workspaces
        s.push_str("\"workspaces\":[");
        for (wi, ws) in mon.workspaces.iter().enumerate() {
            if wi > 0 {
                s.push(',');
            }
            let name = cfg.tag_names.get(wi).copied().unwrap_or("?");
            let n_wins: usize =
                ws.columns.iter().map(|c| c.windows.len()).sum::<usize>() + ws.floats.len();
            s.push('{');
            s.push_str(&format!("\"index\":{wi},"));
            s.push_str(&format!("\"name\":\"{}\",", esc(name)));
            s.push_str(&format!("\"active\":{},", wi == mon.active_ws));
            s.push_str(&format!("\"occupied\":{},", !ws.is_empty()));
            s.push_str(&format!("\"windows\":{n_wins},"));
            s.push_str(&format!("\"layout\":\"{}\"", layout_name(ws.layout)));
            s.push('}');
        }
        s.push(']');
        s.push('}');
    }
    s.push(']');

    s.push('}');
    s
}

/// Canonical short name for a layout kind (used in JSON + `dispatch`).
pub fn layout_name(l: LayoutKind) -> &'static str {
    match l {
        LayoutKind::Column => "column",
        LayoutKind::Grid => "grid",
    }
}

/// Parse an action name from `dispatch <action>` into an `Action`.
///
/// Grammar (space-separated):
///   focus-left|right|up|down|next|prev
///   move-left|right|up|down|next|prev
///   kill | toggle-float | toggle-fullscreen | toggle-bar
///   layout column|grid | cycle-layout
///   grow-col `<px>` | shrink-col `<px>`
///   new-column | collapse-column
///   view `<n>` | move-to-ws `<n>`   (1-based workspace number)
///   focus-mon left|right|next|prev | move-mon left|right|next|prev
///   restart | quit
///   spawn `<cmd>` [args…]
///
/// Returns `None` for unknown/invalid input (the caller logs and ignores it).
pub fn parse_action(input: &str) -> Option<Action> {
    let mut it = input.split_whitespace();
    let verb = it.next()?;
    match verb {
        "focus-left" => Some(Action::FocusDir(Dir::Left)),
        "focus-right" => Some(Action::FocusDir(Dir::Right)),
        "focus-up" => Some(Action::FocusDir(Dir::Up)),
        "focus-down" => Some(Action::FocusDir(Dir::Down)),
        "focus-next" => Some(Action::FocusDir(Dir::Next)),
        "focus-prev" => Some(Action::FocusDir(Dir::Prev)),

        "move-left" => Some(Action::MoveDir(Dir::Left)),
        "move-right" => Some(Action::MoveDir(Dir::Right)),
        "move-up" => Some(Action::MoveDir(Dir::Up)),
        "move-down" => Some(Action::MoveDir(Dir::Down)),
        "move-next" => Some(Action::MoveDir(Dir::Next)),
        "move-prev" => Some(Action::MoveDir(Dir::Prev)),

        "kill" => Some(Action::Kill),
        "toggle-float" => Some(Action::ToggleFloat),
        "toggle-fullscreen" => Some(Action::ToggleFullscreen),
        "toggle-bar" => Some(Action::ToggleBar),

        "layout" => match it.next()? {
            "column" => Some(Action::SetLayout(LayoutKind::Column)),
            "grid" => Some(Action::SetLayout(LayoutKind::Grid)),
            _ => None,
        },
        "cycle-layout" => Some(Action::CycleLayout),

        "grow-col" => it.next()?.parse::<i32>().ok().map(Action::GrowCol),
        "shrink-col" => it
            .next()?
            .parse::<i32>()
            .ok()
            .map(|px| Action::GrowCol(-px)),

        "new-column" => Some(Action::NewColumn),
        "collapse-column" => Some(Action::CollapseColumn),

        "view" => parse_ws(it.next()?).map(Action::View),
        "move-to-ws" => parse_ws(it.next()?).map(Action::MoveToWs),

        "focus-mon" => parse_dir(it.next()?).map(Action::FocusMon),
        "move-mon" => parse_dir(it.next()?).map(Action::MoveMon),

        "restart" => Some(Action::Restart),
        "quit" => Some(Action::Quit),

        "spawn" => {
            let cmd: Vec<String> = it.map(std::string::ToString::to_string).collect();
            if cmd.is_empty() {
                None
            } else {
                Some(Action::Spawn(cmd))
            }
        }

        _ => None,
    }
}

/// Parse a 1-based workspace number into a 0-based index.
fn parse_ws(s: &str) -> Option<usize> {
    let n = s.parse::<usize>().ok()?;
    if n == 0 {
        None
    } else {
        Some(n - 1)
    }
}

fn parse_dir(s: &str) -> Option<Dir> {
    match s {
        "left" => Some(Dir::Left),
        "right" => Some(Dir::Right),
        "up" => Some(Dir::Up),
        "down" => Some(Dir::Down),
        "next" => Some(Dir::Next),
        "prev" => Some(Dir::Prev),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directional_actions() {
        assert!(matches!(
            parse_action("focus-left"),
            Some(Action::FocusDir(Dir::Left))
        ));
        assert!(matches!(
            parse_action("move-down"),
            Some(Action::MoveDir(Dir::Down))
        ));
    }

    #[test]
    fn parses_layout_and_ws() {
        assert!(matches!(
            parse_action("layout grid"),
            Some(Action::SetLayout(LayoutKind::Grid))
        ));
        // view is 1-based externally, 0-based internally.
        assert!(matches!(parse_action("view 3"), Some(Action::View(2))));
        assert!(parse_action("view 0").is_none());
    }

    #[test]
    fn parses_grow_shrink() {
        assert!(matches!(
            parse_action("grow-col 40"),
            Some(Action::GrowCol(40))
        ));
        assert!(matches!(
            parse_action("shrink-col 40"),
            Some(Action::GrowCol(-40))
        ));
    }

    #[test]
    fn parses_spawn_with_args() {
        match parse_action("spawn alacritty -e htop") {
            Some(Action::Spawn(cmd)) => {
                assert_eq!(cmd, vec!["alacritty", "-e", "htop"]);
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown() {
        assert!(parse_action("frobnicate").is_none());
        assert!(parse_action("").is_none());
        assert!(parse_action("layout bogus").is_none());
    }
}
