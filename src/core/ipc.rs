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
use crate::core::wallpaper::{WallpaperMode, WallpaperSource};
use crate::types::{Action, LayoutKind, Rect, State, WindowId};
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
                    write!(
                        s,
                        "\"focused_title\":\"{}\",",
                        maverick_sys::json::json_escape(&c.name)
                    )
                    .unwrap();
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
            let name: &str = cfg.tag_names.get(wi).map(String::as_str).unwrap_or("?");
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

    // Native wallpaper (source + mode) — pure `State` data the compositor reads;
    // exposing it lets bars/status tools and tests observe the active wallpaper.
    s.push_str(",\"wallpaper\":{");
    let (kind, wpath) = match &state.wallpaper.source {
        WallpaperSource::None => ("none", String::new()),
        WallpaperSource::Image(p) => ("image", p.display().to_string()),
        WallpaperSource::Shader(p) => ("shader", p.display().to_string()),
        WallpaperSource::Video(_) => ("video", String::new()),
    };
    write!(
        s,
        "\"kind\":\"{kind}\",\"path\":\"{}\",",
        maverick_sys::json::json_escape(&wpath)
    )
    .unwrap();
    let mode = match state.wallpaper.mode {
        WallpaperMode::Fill => "fill",
        WallpaperMode::Fit => "fit",
        WallpaperMode::Stretch => "stretch",
        WallpaperMode::Center => "center",
    };
    write!(s, "\"mode\":\"{mode}\",\"rev\":{}", state.wallpaper_rev).unwrap();
    s.push('}');

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
            let name: &str = cfg.tag_names.get(wi).map(String::as_str).unwrap_or("?");
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
    write!(
        s,
        "\"class\":\"{}\",",
        maverick_sys::json::json_escape(class)
    )
    .unwrap();
    write!(
        s,
        "\"instance\":\"{}\",",
        maverick_sys::json::json_escape(instance)
    )
    .unwrap();
    write!(
        s,
        "\"title\":\"{}\",",
        maverick_sys::json::json_escape(title)
    )
    .unwrap();
    if let Some(c) = c {
        write!(s, "\"monitor\":{},", c.monitor).unwrap();
        write!(s, "\"workspace\":{},", c.workspace).unwrap();
        write!(s, "\"float\":{},", c.is_float()).unwrap();
        write!(s, "\"fullscreen\":{},", c.is_fullscreen()).unwrap();
        write!(s, "\"maximized\":{},", c.is_maximized()).unwrap();
        // The two EWMH axes are independent; `maximized` stays as the "any
        // axis" summary so existing bars keep working.
        write!(s, "\"maximized_vert\":{},", c.is_maximized_v()).unwrap();
        write!(s, "\"maximized_horiz\":{},", c.is_maximized_h()).unwrap();
        write!(s, "\"sticky\":{},", c.is_sticky()).unwrap();
        write!(
            s,
            "\"geom\":[{},{},{},{}]",
            c.geom.x, c.geom.y, c.geom.w, c.geom.h
        )
        .unwrap();
        // ── Fase 8 observability fields (non-semantic) ────────────────────────
        // `desired` = the last *desired* rect the core arranged this window to.
        // `applied` = `c.geom`, the WM-applied rect. `real` = the last rect the
        // client actually reported back via ConfigureNotify (X11 Real).
        // `focus` = logical focus (any monitor's `focused`). `x11_focus` = the
        // last X input focus the WM observed. `overlay` = this window is the
        // presented fullscreen/maximized overlay owner. `pending` = a deferred
        // focus request is outstanding for it. None of these drive layout.
        let rect_json = |r: Option<Rect>| match r {
            Some(r) => format!("[{},{},{},{}]", r.x, r.y, r.w, r.h),
            None => "null".to_string(),
        };
        let is_focus = state.monitors.iter().any(|m| m.focused == Some(id));
        let is_x11 = state.x11_input_focus == Some(id);
        let is_overlay = state.presented_overlay_owner(c.monitor) == Some(id);
        let is_pending = state.pending_focus.as_ref().map(|p| p.window) == Some(id);
        write!(
            s,
            ",\"desired\":{},\"applied\":[{},{},{},{}],\"real\":{},\"focus\":{},\"x11_focus\":{},\"overlay\":{},\"pending\":{}",
            rect_json(c.last_desired),
            c.geom.x, c.geom.y, c.geom.w, c.geom.h,
            rect_json(c.last_reported),
            is_focus,
            is_x11,
            is_overlay,
            is_pending
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
            write!(s, "\"scroll\":{},", ws.camera.position as i32).unwrap();
            s.push_str("\"columns\":[");
            for (ci, col) in ws.columns.iter().enumerate() {
                if ci > 0 {
                    s.push(',');
                }
                s.push('{');
                write!(s, "\"width\":{},", col.weight * (mon.workarea.w as f32)).unwrap();

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
            write!(
                s,
                "\"class\":\"{}\",",
                maverick_sys::json::json_escape(class)
            )
            .unwrap();
            write!(
                s,
                "\"title\":\"{}\",",
                maverick_sys::json::json_escape(title)
            )
            .unwrap();
            let (fl, fs, mx, st) = c
                .map(|c| {
                    (
                        c.is_float(),
                        c.is_fullscreen(),
                        c.is_maximized(),
                        c.is_sticky(),
                    )
                })
                .unwrap_or((false, false, false, false));
            write!(
                s,
                "\"float\":{fl},\"fullscreen\":{fs},\"maximized\":{mx},\"sticky\":{st}"
            )
            .unwrap();
            s.push('}');
        }
        None => s.push_str("{\"window\":null}"),
    }
    s
}

/// Parse an action name from `dispatch <action>` into an `Action`.
///
/// Delegates to the single shared vocabulary in `core::action` (the same one
/// the TOML config uses), so the IPC and config channels can never drift
/// apart again. See `core::action::parse` for the full grammar and the
/// accepted spellings (`focus-left` / `focus:left`, `grow-col 40` /
/// `grow_col:40`, …).
pub fn parse_action(input: &str) -> Option<Action> {
    crate::core::action::parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dir;

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
