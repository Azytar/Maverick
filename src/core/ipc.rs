// Pure IPC helpers: serialize WM state to JSON for the control socket, and
// parse the action names sent via `dispatch`. No X11, no side effects — this is
// the vocabulary the outside world (maverickctl, bars, scripts) speaks.
//
// Hand-rolled JSON serializer: `write!` onto the buffer avoids serde and the
// per-field `String` temporaries `format!` would allocate, consistent with the
// project's zero-extra-deps stance. Writing to a `String` is infallible (its
// `Write` impl only ever pushes, and `String` has no capacity limit to hit), so
// the `.unwrap()` on each `write!` below is a statement about a `Result` that
// cannot exist, not a swallowed failure.

use crate::config::Cfg;
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
    write!(
        s,
        "\"status\":\"{}\",",
        maverick_sys::json::json_escape(&state.status)
    )
    .unwrap();

    s.push_str("\"monitors\":[");
    for (mi, mon) in state.monitors.iter().enumerate() {
        if mi > 0 {
            s.push(',');
        }
        s.push('{');
        write!(s, "\"index\":{mi},").unwrap();
        // The screen rectangle, so a tool can report a session's resolution
        // without an X connection of its own. A session the manager created
        // knows it because it chose it; the user's own session can only be
        // learned from the window manager that measured it.
        write!(s, "\"screen\":[{},{}],", mon.screen.w, mon.screen.h).unwrap();
        write!(s, "\"workarea\":[{},{}],", mon.workarea.w, mon.workarea.h).unwrap();
        write!(s, "\"active_ws\":{},", mon.active_ws).unwrap();

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

    s.push('}');
    s
}

/// Canonical short name for a layout kind (used in JSON + `dispatch`).
pub fn layout_name(l: LayoutKind) -> &'static str {
    match l {
        LayoutKind::Column => "column",
    }
}

/// Answer a structured `query` request from the control socket (`maverickctl
/// query …`). Pure — no X11, no side effects. Returns a JSON document, or
/// `error unknown-query: <topic>` for topics it doesn't know.
pub fn query_json(state: &State, cfg: &Cfg, topic: &str) -> String {
    match topic {
        "state" => state_json(state, cfg),
        "workspaces" => workspaces_json(state, cfg),
        "tree" => tree_json(state),
        "focused" => focused_json(state),
        "inspect" => inspect_json(state, cfg),
        _ => format!("error unknown-query: {topic}"),
    }
}

/// Facts the backend knows and `State` cannot.
///
/// `query inspect` — what this window manager is, in one document.
///
/// The topic exists because "what is this session doing" spans three questions
/// that no existing answer covered together: how many windows it manages and
/// how they are arranged (which the tree query answers as a tree, not as
/// totals), where the camera is scrolled to, and the resolution and workarea
/// the selected monitor is using.
/// `maverickctl inspect` renders this plus what the session manager knows.
pub fn inspect_json(state: &State, cfg: &Cfg) -> String {
    use std::fmt::Write;
    let mi = state.sel_mon.min(state.monitors.len().saturating_sub(1));
    let mon = state.monitors.get(mi);
    let ws = mon.map(crate::types::Monitor::ws);

    let managed: usize = state.clients.len();
    let floating: usize = state.clients.values().filter(|c| c.is_float()).count();
    let fullscreen: usize = state.clients.values().filter(|c| c.is_fullscreen()).count();
    let maximized: usize = state.clients.values().filter(|c| c.is_maximized()).count();
    let columns = ws.map_or(0, |w| w.columns.len());
    let cameras = state
        .monitors
        .iter()
        .flat_map(|m| m.workspaces.iter())
        .count();

    let mut s = String::with_capacity(512);
    s.push('{');
    write!(s, "\"sel_mon\":{mi},").unwrap();
    write!(s, "\"tags\":{},", cfg.n_tags).unwrap();
    write!(
        s,
        "\"windows\":{{\"total\":{managed},\"floating\":{floating},\"fullscreen\":{fullscreen},\"maximized\":{maximized}}},",
    )
    .unwrap();
    write!(
        s,
        "\"layout\":{{\"type\":\"{}\",\"columns\":{columns},\"cameras\":{cameras},\"camera\":{}}},",
        layout_name(ws.map_or(crate::types::LayoutKind::Column, |w| w.layout)),
        mon.map_or(0.0, |m| m.ws().camera.position),
    )
    .unwrap();
    if let Some(mon) = mon {
        write!(
            s,
            "\"monitor\":{{\"index\":{mi},\"screen\":[{},{}],\"workarea\":[{},{}],\"active_ws\":{},\"focused\":{}}}",
            mon.screen.w,
            mon.screen.h,
            mon.workarea.w,
            mon.workarea.h,
            mon.active_ws,
            mon.focused.map_or("null".to_string(), |w| w.to_string()),
        )
        .unwrap();
    } else {
        s.push_str("\"monitor\":null");
    }
    s.push('}');
    s
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
        // The window → process link (`_NET_WM_PID`), `null` when the client
        // never set it. Lets `maverickctl process`/`window inspect` tie a window
        // to `/proc/<pid>` without walking the display's process tree.
        match c.pid {
            Some(p) => write!(s, "\"pid\":{p},").unwrap(),
            None => s.push_str("\"pid\":null,"),
        }
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
        // Observability-only block: none of these fields feeds back into layout.
        // `desired` = the last *desired* rect the core arranged this window to.
        // `applied` = `c.geom`, the WM-applied rect. `real` = the last rect the
        // client actually reported back via ConfigureNotify (X11 Real).
        // `focus` = logical focus (any monitor's `focused`). `x11_focus` = the
        // last X input focus the WM observed. `overlay` = this window is the
        // presented fullscreen/maximized overlay owner. `pending` = a deferred
        // focus request is outstanding for it.
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
            // A poisoned camera can hold a non-finite `position`. Emit 0 rather
            // than a bare `NaN`, which is not valid JSON and would break every
            // consumer parsing this snapshot.
            let scroll = if ws.camera.position.is_finite() {
                ws.camera.position as i32
            } else {
                0
            };
            write!(s, "\"scroll\":{scroll},").unwrap();
            s.push_str("\"columns\":[");
            for (ci, col) in ws.columns.iter().enumerate() {
                if ci > 0 {
                    s.push(',');
                }
                s.push('{');
                // Weight × workarea width, i.e. the on-screen column width. Same
                // non-finite guard as the scroll above.
                let width = if col.weight.is_finite() {
                    col.weight * (mon.workarea.w as f32)
                } else {
                    0.0
                };
                write!(s, "\"width\":{width},").unwrap();

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
/// the TOML config uses), so the IPC and config channels cannot drift apart.
/// See `core::action::parse` for the full grammar and the accepted spellings
/// (`focus-left` / `focus:left`, `grow-col 40` / `grow_col:40`, …).
pub fn parse_action(input: &str) -> Option<Action> {
    crate::core::action::parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dir;

    /// Every control document the WM publishes is read by tools, so each has to
    /// parse. `maverickctl window list` and `inspect` are built on this: a
    /// malformed document is not a degraded view, it is no view.
    fn parses(doc: &str) -> maverick_sys::json::Json {
        maverick_sys::json::parse(doc).unwrap_or_else(|| panic!("document does not parse: {doc}"))
    }

    /// `inspect` is the one document assembled from two sources — `State` and
    /// the backend's own facts — so both halves have to appear, and the
    #[test]
    fn the_inspect_document_reports_totals_layout_and_no_fiction() {
        use crate::types::Client;
        let mut state = crate::types::State::new();
        state.monitors.push(crate::types::Monitor::new(
            crate::types::Rect::new(0, 0, 1920, 1080),
            9,
        ));
        let mi = 0;
        let ws_i = state.monitors[mi].active_ws;
        for (win, float) in [(1u32, false), (2, false), (3, true)] {
            state.add_client(Client::new(win, mi, ws_i));
            if float {
                state.monitors[mi].workspaces[ws_i].floats.push(win);
                state
                    .clients
                    .get_mut(&win)
                    .expect("added")
                    .flags
                    .set(crate::types::WinFlags::FLOAT);
            } else {
                state.monitors[mi].workspaces[ws_i].add_tiled(win, 1.0);
            }
        }
        let cfg = crate::config::Cfg::default();
        let doc = inspect_json(&state, &cfg);
        let v = parses(&doc);

        let windows = v.get("windows").expect("windows totals");
        assert_eq!(windows.num_field("total"), 3);
        assert_eq!(windows.num_field("floating"), 1);
        assert_eq!(windows.num_field("fullscreen"), 0);
        let layout = v.get("layout").expect("layout");
        assert_eq!(layout.str_field("type"), "column");
        assert_eq!(
            layout.num_field("columns"),
            2,
            "two tiled columns, one float"
        );
        // And the monitor the user can actually see.
        let mon = v.get("monitor").expect("monitor");
        assert_eq!(
            mon.get("screen").map(|s| s.as_array().len()),
            Some(2),
            "the screen size is what makes `main` reportable: {doc}"
        );
    }

    /// A window manager with no monitor must still answer, rather than
    /// panicking on the way to a document a tool is blocked waiting for.
    #[test]
    fn the_inspect_document_survives_an_empty_state() {
        let state = crate::types::State::new();
        let doc = inspect_json(&state, &crate::config::Cfg::default());
        let v = parses(&doc);
        assert_eq!(v.num_field("sel_mon"), 0);
        assert_eq!(v.get("monitor"), Some(&maverick_sys::json::Json::Null));
        assert_eq!(v.get("windows").expect("windows").num_field("total"), 0);
    }

    /// The state snapshot is what every client polls, so the screen size added
    /// for resolution reporting has to be there for every monitor and survive a
    /// parse — a client reporting a session's resolution reads exactly this.
    #[test]
    fn the_state_snapshot_carries_each_monitors_screen_size() {
        use crate::types::Monitor;
        let mut state = crate::types::State::new();
        state
            .monitors
            .push(Monitor::new(crate::types::Rect::new(0, 0, 1920, 1080), 9));
        state
            .monitors
            .push(Monitor::new(crate::types::Rect::new(1920, 0, 1280, 720), 9));
        let v = parses(&state_json(&state, &crate::config::Cfg::default()));
        let mons = v.get("monitors").expect("monitors").as_array();
        assert_eq!(mons.len(), 2);
        assert_eq!(mons[0].get("screen").expect("screen").as_array().len(), 2);
        assert_eq!(mons[1].get("screen").expect("screen").as_array().len(), 2);
        assert_eq!(mons[1].num_field("index"), 1);
    }

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
            parse_action("layout column"),
            Some(Action::SetLayout(LayoutKind::Column))
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
