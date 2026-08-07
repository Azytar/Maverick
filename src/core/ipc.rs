// maverick/src/core/ipc.rs
// Pure IPC helpers: serialize WM state to JSON for the control socket, and
// parse action names sent via `dispatch`. No X11, no side effects — this is
// the vocabulary the outside world (maverickctl, bars, scripts) speaks.

// This is a small hand-rolled JSON serializer; `write!` onto the buffer avoids
// serde and the per-field `String` temporaries that `format!` would allocate,
// consistent with the project's zero-extra-deps stance. Writing to a `String`
// is infallible (the `Write` impl for `String` never errors), so `.unwrap()`
// here is safe and is suppressed with `clippy::unwrap_used`.
#![allow(clippy::unwrap_used, clippy::map_unwrap_or)]

use crate::config::Cfg;
use crate::types::{Action, Dir, LayoutKind, State, WindowId};
use std::fmt::Write;

/// Serialize the live WM `State` into a compact JSON snapshot for external
/// tools. Includes per-monitor active workspace, focused window + title, and
/// per-workspace occupancy/layout. Deterministic field order so consumers can
/// diff snapshots cheaply.
pub fn state_json(state: &State, cfg: &Cfg) -> String {
    // Estimate capacity to avoid reallocations: ~512 base + ~1024 bytes/monitor.
    let mut s = String::with_capacity(512 + state.monitors.len() * 1024);
    s.push('{');

    write!(s, "\"sel_mon\":{},", state.sel_mon).unwrap();
    write!(s, "\"focus_serial\":{},", state.focus_serial).unwrap();
    write!(
        s,
        "\"status\":\"{}\",",
        maverick_sys::json::json_escape(&state.status)
    )
    .unwrap();

    // monitors
    s.push_str("\"monitors\":[");
    for (mi, mon) in state.monitors.iter().enumerate() {
        if mi > 0 {
            s.push(',');
        }
        s.push('{');
        write!(s, "\"index\":{mi},").unwrap();
        write!(s, "\"active_ws\":{},", mon.active_ws).unwrap();

        // focused window + its title/class
        match mon.focused {
            Some(w) => {
                write!(s, "\"focused\":{w},").unwrap();
                if let Some(c) = state.clients.get(&w) {
                    write!(s, "\"focused_title\":\"{}\",", maverick_sys::json::json_escape(&c.name)).unwrap();
                    write!(
                        s,
                        "\"focused_class\":\"{}\",",
                        maverick_sys::json::json_escape(&c.class)
                    )
                    .unwrap();
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
            let name: &str = cfg.tag_names
                .get(wi)
                .map(String::as_str)
                .unwrap_or("?");
            let n_wins: usize =
                ws.columns.iter().map(|c| c.windows.len()).sum::<usize>() + ws.floats.len();
            s.push('{');
            write!(s, "\"index\":{wi},").unwrap();
            write!(s, "\"name\":\"{}\",", maverick_sys::json::json_escape(name)).unwrap();
            write!(s, "\"active\":{},", wi == mon.active_ws).unwrap();
            write!(s, "\"occupied\":{},", !ws.is_empty()).unwrap();
            write!(s, "\"windows\":{n_wins},").unwrap();
            write!(s, "\"layout\":\"{}\"", layout_name(ws.layout)).unwrap();
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

/// Answer a structured `query` request from the control socket (`maverick-msg
/// -j query …`). Pure — no X11, no side effects. Returns a JSON document, or
/// `error unknown-query: <topic>` for topics it doesn't know.
pub fn query_json(state: &State, cfg: &Cfg, topic: &str) -> String {
    match topic {
        "state" => state_json(state, cfg),
        "workspaces" => workspaces_json(state, cfg),
        "tree" => tree_json(state),
        "focused" => focused_json(state),
        _ => format!("error unknown-query: {topic}"),
    }
}

/// `query workspaces` — one entry per workspace per monitor: identity, layout,
/// occupancy and the exact window ids it holds (bars feed on this without
/// parsing the whole state snapshot).
fn workspaces_json(state: &State, cfg: &Cfg) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(256 + state.monitors.len() * 512);
    s.push('{');
    write!(s, "\"sel_mon\":{},", state.sel_mon).unwrap();
    s.push_str("\"monitors\":[");
    for (mi, mon) in state.monitors.iter().enumerate() {
        if mi > 0 {
            s.push(',');
        }
        s.push('{');
        write!(s, "\"index\":{mi},").unwrap();
        write!(s, "\"active_ws\":{},", mon.active_ws).unwrap();
        s.push_str("\"workspaces\":[");
        for (wi, ws) in mon.workspaces.iter().enumerate() {
            if wi > 0 {
                s.push(',');
            }
            let name: &str = cfg
                .tag_names
                .get(wi)
                .map(String::as_str)
                .unwrap_or("?");
            s.push('{');
            write!(s, "\"index\":{wi},").unwrap();
            write!(s, "\"name\":\"{}\",", maverick_sys::json::json_escape(name)).unwrap();
            write!(s, "\"active\":{},", wi == mon.active_ws).unwrap();
            write!(s, "\"occupied\":{},", !ws.is_empty()).unwrap();
            write!(s, "\"layout\":\"{}\",", layout_name(ws.layout)).unwrap();
            s.push_str("\"windows\":[");
            let mut first = true;
            for w in ws
                .columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .chain(ws.floats.iter().copied())
            {
                if !first {
                    s.push(',');
                }
                write!(s, "{w}").unwrap();
                first = false;
            }
            s.push_str("]}");
        }
        s.push_str("]}");
    }
    s.push_str("]}");
    s
}

/// Serialize one window entry for the tree query.
fn window_obj(s: &mut String, id: WindowId, state: &State) {
    use std::fmt::Write;
    let c = state.clients.get(&id);
    let (class, instance, title) = c
        .map(|c| (c.class.as_str(), c.instance.as_str(), c.name.as_str()))
        .unwrap_or(("", "", ""));
    write!(s, "{{\"id\":{id},").unwrap();
    write!(s, "\"class\":\"{}\",", maverick_sys::json::json_escape(class)).unwrap();
    write!(
        s,
        "\"instance\":\"{}\",",
        maverick_sys::json::json_escape(instance)
    )
    .unwrap();
    write!(s, "\"title\":\"{}\",", maverick_sys::json::json_escape(title)).unwrap();
    if let Some(c) = c {
        write!(s, "\"monitor\":{},", c.monitor).unwrap();
        write!(s, "\"workspace\":{},", c.workspace).unwrap();
        write!(s, "\"float\":{},", c.is_float()).unwrap();
        write!(s, "\"fullscreen\":{},", c.is_fullscreen()).unwrap();
        write!(s, "\"maximized\":{},", c.is_maximized()).unwrap();
        write!(s, "\"sticky\":{},", c.is_sticky()).unwrap();
        write!(
            s,
            "\"geom\":[{},{},{},{}]",
            c.geom.x, c.geom.y, c.geom.w, c.geom.h
        )
        .unwrap();
    }
    s.push('}');
}

/// `query tree` — the full in-memory tiling tree: monitors → workspaces →
/// columns → windows (with their live geometry and state). Feeds custom
/// taskbars/Alt+Tab UIs that need the actual hierarchy, not just counts.
fn tree_json(state: &State) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(512 + state.monitors.len() * 1024);
    s.push('{');
    write!(s, "\"sel_mon\":{},", state.sel_mon).unwrap();
    s.push_str("\"monitors\":[");
    for (mi, mon) in state.monitors.iter().enumerate() {
        if mi > 0 {
            s.push(',');
        }
        s.push('{');
        write!(s, "\"index\":{mi},").unwrap();
        write!(s, "\"active_ws\":{},", mon.active_ws).unwrap();
        s.push_str("\"workspaces\":[");
        for (wi, ws) in mon.workspaces.iter().enumerate() {
            if wi > 0 {
                s.push(',');
            }
            s.push('{');
            write!(s, "\"index\":{wi},").unwrap();
            write!(s, "\"layout\":\"{}\",", layout_name(ws.layout)).unwrap();
            write!(s, "\"scroll\":{},", ws.scroll).unwrap();
            s.push_str("\"columns\":[");
            for (ci, col) in ws.columns.iter().enumerate() {
                if ci > 0 {
                    s.push(',');
                }
                s.push('{');
                write!(s, "\"width\":{},", col.width).unwrap();
                write!(s, "\"focused\":{},", col.focused).unwrap();
                s.push_str("\"windows\":[");
                for (i, w) in col.windows.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    window_obj(&mut s, *w, state);
                }
                s.push_str("]}");
            }
            s.push_str("],\"floats\":[");
            for (i, w) in ws.floats.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                window_obj(&mut s, *w, state);
            }
            s.push_str("]}");
        }
        s.push_str("]}");
    }
    s.push_str("]}");
    s
}

/// `query focused` — the focused window of the selected monitor (or null).
fn focused_json(state: &State) -> String {
    use std::fmt::Write;
    let mon = state
        .monitors
        .get(state.sel_mon.min(state.monitors.len().saturating_sub(1)));
    let mut s = String::with_capacity(160);
    match mon.and_then(|m| m.focused) {
        Some(w) => {
            let c = state.clients.get(&w);
            let (class, title) = c
                .map(|c| (c.class.as_str(), c.name.as_str()))
                .unwrap_or(("", ""));
            write!(s, "{{\"window\":{w},").unwrap();
            write!(s, "\"class\":\"{}\",", maverick_sys::json::json_escape(class)).unwrap();
            write!(s, "\"title\":\"{}\",", maverick_sys::json::json_escape(title)).unwrap();
            let (fl, fs, mx, st) = c
                .map(|c| (c.is_float(), c.is_fullscreen(), c.is_maximized(), c.is_sticky()))
                .unwrap_or((false, false, false, false));
            write!(s, "\"float\":{fl},\"fullscreen\":{fs},\"maximized\":{mx},\"sticky\":{st}").unwrap();
            s.push('}');
        }
        None => s.push_str("{\"window\":null}"),
    }
    s
}

/// Parse an action name from `dispatch <action>` into an `Action`.
///
/// Grammar (space-separated):
///   focus-left|right|up|down|next|prev
///   move-left|right|up|down|next|prev
///   kill | toggle-float | toggle-fullscreen
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
