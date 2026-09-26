//! `maverickctl window …`, `process`-free window control, `camera`, `resize`
//! and `layout`: the semantic surface an agent drives a session through.
//!
//! Every operation here leaves through the control socket as an *action*, so it
//! goes through `Action → DesiredState → Reconciler` exactly as a keypress
//! does. That is not a stylistic choice: a tool that issued `XMoveWindow` itself
//! would be a second authority whose idea of the layout could not be reconciled
//! with the window manager's, and the divergence would only show up as a
//! window that is in the wrong place until something else moved it.
//!
//! # Addressing a window
//!
//! A window is addressed by its X11 id, which is stable for the window's
//! lifetime and is what every other X tool reports. A human name (class,
//! instance, or part of a title) is also accepted, because typing a hex id is
//! not a reasonable thing to ask of a person — but the id is always what is
//! *acted on*, and an ambiguous name is refused with the candidates listed
//! rather than resolved to a guess.

use crate::json::Json;

use super::session::{available_sessions, truncate};
use super::{print_usage, session_target, Ctl};

/// One managed window, flattened out of the tree query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// X11 window id.
    pub id: u32,
    /// The client's `_NET_WM_PID`, when it published one. This is the link
    /// from a window to a process in the session's process list, and the only
    /// one the X server vouches for.
    pub pid: Option<u32>,
    /// `WM_CLASS` class.
    pub class: String,
    /// `WM_CLASS` instance name.
    pub instance: String,
    /// `_NET_WM_NAME`.
    pub title: String,
    /// Index of the monitor it is on.
    pub monitor: usize,
    /// Index of the workspace it is tiled in.
    pub workspace: usize,
    /// Column index within that workspace.
    pub column: usize,
    /// Index within its column.
    pub index: usize,
    /// Whether it is floating.
    pub floating: bool,
    /// Whether it is fullscreen.
    pub fullscreen: bool,
    /// Whether it is maximized.
    pub maximized: bool,
    /// Whether it is the focused window of its monitor.
    pub focused: bool,
    /// Its last requested geometry.
    pub geometry: (i32, i32, u32, u32),
}

/// Flatten the `tree` query into one list of windows.
///
/// The tree is the authoritative hierarchy (monitor → workspace → column →
/// window), and the column/index of a window are only knowable from it — so
/// this walks the document rather than asking the window manager to re-derive
/// a flat list that would be a second description of the same state.
pub fn flatten_windows(tree: &Json) -> Vec<WindowInfo> {
    let mut out = Vec::new();
    for mon in tree.get("monitors").map(Json::as_array).unwrap_or(&[]) {
        let monitor = mon.num_field("index") as usize;
        let mon_focus = mon.num_field("focused") as u32;
        for ws in mon.get("workspaces").map(Json::as_array).unwrap_or(&[]) {
            let workspace = ws.num_field("index") as usize;
            for (column, col) in ws
                .get("columns")
                .map(Json::as_array)
                .unwrap_or(&[])
                .iter()
                .enumerate()
            {
                for (index, win) in col
                    .get("windows")
                    .map(Json::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                {
                    out.push(window_of(win, monitor, workspace, column, index, mon_focus));
                }
            }
            for win in ws.get("floats").map(Json::as_array).unwrap_or(&[]) {
                out.push(window_of(win, monitor, workspace, 0, 0, mon_focus));
            }
        }
    }
    out
}

/// One window entry of the tree, with the position the walk found it at.
fn window_of(
    w: &Json,
    monitor: usize,
    workspace: usize,
    column: usize,
    index: usize,
    mon_focus: u32,
) -> WindowInfo {
    let id = w.num_field("id") as u32;
    let pid = match w.get("pid") {
        // 0 is X11's "no window"/"no process" spelling; a client that published
        // a real pid never publishes 0, and treating it as pid 1 would point at
        // init.
        Some(Json::Num(n)) if *n > 0.0 => Some(*n as u32),
        _ => None,
    };
    WindowInfo {
        id,
        pid,
        class: w.str_field("class").to_string(),
        instance: w.str_field("instance").to_string(),
        title: w.str_field("name").to_string(),
        monitor,
        workspace,
        column,
        index,
        floating: w.bool_field("float"),
        fullscreen: w.bool_field("fullscreen"),
        maximized: w.bool_field("maximized"),
        focused: mon_focus != 0 && mon_focus == id,
        geometry: (
            w.num_field("x") as i32,
            w.num_field("y") as i32,
            w.num_field("w") as u32,
            w.num_field("h") as u32,
        ),
    }
}

/// Resolve a window selector to a window id.
///
/// Precedence, and the reason for it: an id is a fact, a name is a guess. A
/// selector that parses as an id *and* names a live window is that window; a
/// selector that parses as an id and names nothing is an error rather than a
/// name search, because "0x999" was never a class. Otherwise the selector is
/// matched against class, instance and title, exact before substring, and a
/// substring that matches more than one window is refused with the ids listed —
/// picking one would be a coin flip with the user's windows on it.
pub fn resolve_window(windows: &[WindowInfo], selector: &str) -> Result<u32, String> {
    let sel = selector.trim();
    if let Some(id) = parse_id(sel) {
        return windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.id)
            .ok_or_else(|| {
                format!(
                    "no window {} in this session ({})",
                    sel,
                    plural(windows.len())
                )
            });
    }
    let lower = sel.to_ascii_lowercase();
    let exact = |field: fn(&WindowInfo) -> &String| {
        windows
            .iter()
            .find(|w| field(w).eq_ignore_ascii_case(sel))
            .map(|w| w.id)
    };
    for field in [
        (|w: &WindowInfo| &w.instance) as fn(&WindowInfo) -> &String,
        |w: &WindowInfo| &w.class,
        |w: &WindowInfo| &w.title,
    ] {
        if let Some(id) = exact(field) {
            return Ok(id);
        }
    }
    let matches: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| {
            w.class.to_ascii_lowercase().contains(&lower)
                || w.instance.to_ascii_lowercase().contains(&lower)
                || w.title.to_ascii_lowercase().contains(&lower)
        })
        .collect();
    match matches.as_slice() {
        [only] => Ok(only.id),
        [] => Err(format!(
            "no window matching '{sel}' ({})\n\n{}",
            plural(windows.len()),
            candidates(&windows.iter().collect::<Vec<_>>())
        )),
        many => Err(format!(
            "'{sel}' matches {} windows — use the id:\n\n{}",
            many.len(),
            candidates(many)
        )),
    }
}

fn parse_id(s: &str) -> Option<u32> {
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }?;
    (parsed != 0).then_some(parsed)
}

fn plural(n: usize) -> String {
    if n == 1 {
        "1 window".to_string()
    } else {
        format!("{n} windows")
    }
}

/// The windows as a list a user can pick from, with their ids.
fn candidates(windows: &[&WindowInfo]) -> String {
    if windows.is_empty() {
        return "  (no windows)\n".to_string();
    }
    windows
        .iter()
        .take(20)
        .map(|w| {
            format!(
                "  {:#x}  {}{}{}",
                w.id,
                w.class,
                if w.pid.is_some_and(|p| p != 0) {
                    format!(" (pid {})", w.pid.unwrap_or(0))
                } else {
                    String::new()
                },
                if w.title.is_empty() {
                    String::new()
                } else {
                    format!("  {}", w.title)
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The session's windows, or an error naming the session.
fn windows_of(c: &Ctl, args: &[String]) -> Result<(String, Vec<WindowInfo>), String> {
    let name = session_target(c, args)?;
    let view = crate::session::resolve(&name).map_err(|e| e.to_string())?;
    let tree = crate::control::query(&view.sid, "tree")
        .map_err(|e| format!("cannot read the window tree of '{name}': {e}"))?;
    let tree = crate::json::parse(&tree)
        .ok_or_else(|| format!("the window tree of '{name}' was not valid JSON"))?;
    Ok((view.sid, flatten_windows(&tree)))
}

/// `maverickctl window …`
pub fn run(c: &mut Ctl, args: &[String]) -> Result<bool, String> {
    let verb = args.first().map(String::as_str).unwrap_or("list");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match verb {
        "list" | "ls" => window_list(c, rest),
        "inspect" | "info" => window_inspect(c, rest)?,
        "focus" => act(c, rest, "focus", WindowOp::Focus)?,
        "close" | "kill" => act(c, rest, "close", WindowOp::Close)?,
        "float" => act(c, rest, "float", WindowOp::Float)?,
        "fullscreen" => act(c, rest, "fullscreen", WindowOp::Fullscreen)?,
        "move" => {
            let Some(dir) = rest.iter().find(|a| !a.starts_with('-') && is_dir(a)) else {
                return Err("window move needs a direction\n\n  try: maverickctl window move debug 0x42003 right".into());
            };
            act(c, rest, &format!("move {dir}"), WindowOp::Move(dir.clone()))?
        }
        "help" | "-h" | "--help" => print_usage(super::Usage::Windows),
        other => return Err(format!("unknown window command '{other}'")),
    }
    Ok(true)
}

/// `maverickctl window list <session>`
fn window_list(c: &Ctl, args: &[String]) {
    let (sid, windows) = match windows_of(c, args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    if c.json {
        let items: Vec<String> = windows.iter().map(window_json).collect();
        println!(
            "{{\"session\":{},\"windows\":[{}]}}",
            crate::json::json_quote(&sid),
            items.join(",")
        );
        return;
    }
    if windows.is_empty() {
        println!("No managed windows in session '{sid}'.");
        return;
    }
    println!(
        "{:<12} {:<8} {:<16} {:<5} {:<5} {:<4} TITLE",
        "ID", "PID", "CLASS", "WS", "COL", "FS",
    );
    for w in &windows {
        println!(
            "{:<12} {:<8} {:<16} {:<5} {:<4} {:<4} {}",
            format!("{:#010x}", w.id),
            w.pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
            truncate(&w.class, 16),
            w.workspace,
            w.column,
            if w.fullscreen { "yes" } else { "-" },
            truncate(&w.title, 40),
        );
    }
    println!("\n{} window(s).", windows.len());
}

fn window_json(w: &WindowInfo) -> String {
    let num = |v: u32| v.to_string();
    format!(
        "{{\"id\":{},\"id_hex\":\"{:#x}\",\"pid\":{},\"class\":{},\"instance\":{},\"title\":{},\"monitor\":{},\"workspace\":{},\"column\":{},\"index\":{},\"floating\":{},\"fullscreen\":{},\"maximized\":{},\"focused\":{},\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}",
        w.id,
        w.id,
        w.pid.map_or_else(|| "null".to_string(), num),
        crate::json::json_quote(&w.class),
        crate::json::json_quote(&w.instance),
        crate::json::json_quote(&w.title),
        w.monitor,
        w.workspace,
        w.column,
        w.index,
        w.floating,
        w.fullscreen,
        w.maximized,
        w.focused,
        w.geometry.0,
        w.geometry.1,
        w.geometry.2,
        w.geometry.3,
    )
}

/// `maverickctl window inspect <session> <window>`
fn window_inspect(c: &Ctl, args: &[String]) -> Result<(), String> {
    let (sid, windows) = windows_of(c, args)?;
    let selector = args
        .iter()
        .find(|a| !a.starts_with('-') && !a.is_empty() && !is_dir(a))
        .cloned()
        .or_else(|| {
            // No selector: the focused window is what a user means by "the
            // window" when they do not name one.
            windows.iter().find(|w| w.focused).map(|w| w.class.clone())
        })
        .ok_or_else(|| {
            format!(
                "no window in session '{sid}' to inspect\n\n{}",
                available_sessions()
            )
        })?;
    let id = resolve_window(&windows, &selector)?;
    let w = windows
        .iter()
        .find(|w| w.id == id)
        .expect("resolve_window returned an id from this list");

    // The window manager's own view of the same window, so the two are
    // reported together: what the tool sees and what the WM believes.
    let live = crate::control::query(&sid, "inspect")
        .ok()
        .and_then(|j| crate::json::parse(&j));
    if c.json {
        let doc = window_json(w);
        let mut fields = match crate::json::parse(&doc) {
            Some(Json::Obj(f)) => f,
            _ => Vec::new(),
        };
        if let Some(v) = &live {
            for key in ["layout", "compositor", "sel_mon"] {
                if let Some(part) = v.get(key) {
                    fields.push((format!("maverick_{key}"), part.clone()));
                }
            }
        }
        println!("{}", Json::Obj(fields).to_json());
        return Ok(());
    }
    println!("WINDOW");
    println!("  {:<14}{:#010x}", "id:", w.id);
    println!(
        "  {:<14}{}",
        "pid:",
        w.pid.map_or_else(|| "-".to_string(), |p| p.to_string())
    );
    println!("  {:<14}{}", "class:", display_or_dash(&w.class));
    println!("  {:<14}{}", "instance:", display_or_dash(&w.instance));
    println!("  {:<14}{}", "title:", display_or_dash(&w.title));
    println!(
        "  {:<14}{},{} {}x{}",
        "geometry:", w.geometry.0, w.geometry.1, w.geometry.2, w.geometry.3
    );
    println!("\nMAVERICK");
    println!(
        "  {:<14}{}",
        "state:",
        if w.floating { "floating" } else { "tiled" }
    );
    println!("  {:<14}{}", "focused:", yes_no(w.focused));
    println!("  {:<14}{}", "fullscreen:", yes_no(w.fullscreen));
    println!("  {:<14}{}", "maximized:", yes_no(w.maximized));
    println!("  {:<14}{}", "monitor:", w.monitor);
    println!("  {:<14}{}", "workspace:", w.workspace);
    println!("  {:<14}{}", "column:", w.column);
    if let Some(v) = live.as_ref() {
        if let Some(l) = v.get("layout") {
            if let Some(cam) = l.get("camera").and_then(Json::as_f64) {
                println!("  {:<14}{cam:.3}", "camera:");
            }
        }
    }
    Ok(())
}

/// The window operations, as the action each one dispatches.
enum WindowOp {
    Focus,
    Close,
    Float,
    Fullscreen,
    Move(String),
}

impl WindowOp {
    /// The action line for a window id.
    ///
    /// The window-targeted verbs, so every one of these goes through the same
    /// command the corresponding keybinding uses — see `core::action` for the
    /// grammar and the aliases.
    fn action(&self, id: u32) -> String {
        match self {
            Self::Focus => format!("focus_window {id:#x}"),
            Self::Close => format!("close_window {id:#x}"),
            Self::Float => format!("float_window {id:#x}"),
            Self::Fullscreen => format!("fullscreen_window {id:#x}"),
            Self::Move(dir) => format!("move_window {} {id:#x}", dir),
        }
    }
}

/// Resolve the window, then dispatch the action.
fn act(c: &Ctl, args: &[String], verb: &str, op: WindowOp) -> Result<(), String> {
    let name = session_target(c, args)?;
    let view = crate::session::resolve(&name).map_err(|e| e.to_string())?;
    let tree = crate::control::query(&view.sid, "tree")
        .map_err(|e| format!("cannot read the window tree of '{name}': {e}"))?;
    let tree = crate::json::parse(&tree)
        .ok_or_else(|| format!("the window tree of '{name}' was not valid JSON"))?;
    let windows = flatten_windows(&tree);

    // A selector of "focused" is the one name that is not a substring, and it
    // is the most common intent: act on what the user is looking at.
    let selector = args
        .iter()
        .find(|a| !a.starts_with('-') && !a.is_empty() && !is_dir(a) && *a != verb)
        .cloned()
        .unwrap_or_else(|| "focused".to_string());
    let id = if selector == "focused" {
        windows
            .iter()
            .find(|w| w.focused)
            .map(|w| w.id)
            .ok_or_else(|| format!("no window is focused in session '{name}'"))?
    } else {
        resolve_window(&windows, &selector)?
    };

    crate::control::dispatch(&view.sid, &op.action(id))
        .map_err(|e| format!("{verb} failed for {id:#x}: {e}"))?;
    if c.json {
        println!(
            "{{\"session\":{},\"window\":{},\"action\":{}}}",
            crate::json::json_quote(&name),
            id,
            crate::json::json_quote(&op.action(id))
        );
    } else {
        println!("{id:#x}: {verb}");
    }
    Ok(())
}

fn is_dir(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "left" | "right" | "up" | "down" | "next" | "prev"
    )
}

fn display_or_dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

// ── camera / resize / layout ─────────────────────────────────────────────────

/// `maverickctl camera <session> <left|right|up|down>`
///
/// Maverick's camera is the scroll position of the ribbon, and it follows the
/// focus: focusing the column to the left *is* moving the camera left. So this
/// is `focus:<dir>` on the wire, and it is deliberately not a second layout
/// engine in a tool.
pub fn camera(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let dir = args.iter().find(|a| is_dir(a)).cloned().ok_or_else(|| {
        "camera needs a direction\n\n  try: maverickctl camera debug right".to_string()
    })?;
    let view = crate::session::resolve(&name).map_err(|e| e.to_string())?;
    crate::control::dispatch(&view.sid, &format!("focus:{dir}"))
        .map_err(|e| format!("camera {dir} failed: {e}"))?;
    report(c, &name, &format!("camera {dir}"));
    Ok(())
}

/// `maverickctl resize <session> <+10%|-10%|40>`
///
/// A percentage is the default because it is what a user means and it needs no
/// knowledge of the workarea; a plain number is pixels, for a caller that
/// knows better. Both end up as one action on the wire.
pub fn resize(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let amount = args
        .iter()
        .find(|a| !a.starts_with('-') || a.starts_with('+') || a.starts_with('-'))
        .and_then(|a| {
            let t = a.trim();
            if t.is_empty() || t == name {
                None
            } else {
                Some(t.to_string())
            }
        })
        .ok_or_else(|| {
            "resize needs an amount\n\n  try: maverickctl resize debug +10%".to_string()
        })?;
    let view = crate::session::resolve(&name).map_err(|e| e.to_string())?;
    // A trailing `%` means a fraction of the workarea; a bare number means
    // pixels. Both are one action on the wire, and the choice is the caller's
    // — spelled explicitly rather than guessed, because "10" as a percentage
    // and "10" as pixels are ten very different windows.
    let action = if let Some(pct) = amount.strip_suffix('%') {
        if pct.is_empty() || pct.parse::<f32>().is_err() {
            return Err(format!("'{amount}' is not a percentage"));
        }
        format!("grow_col_pct {pct}")
    } else if amount.parse::<i32>().is_ok() {
        format!("grow_col {amount}")
    } else {
        return Err(format!(
            "'{amount}' is neither a pixel count nor a percentage\n\n  try: maverickctl resize {name} +10%"
        ));
    };
    crate::control::dispatch(&view.sid, &action)
        .map_err(|e| format!("resize {amount} failed: {e}"))?;
    report(c, &name, &action);
    Ok(())
}

/// `maverickctl layout <session> <column>`
pub fn layout(c: &Ctl, args: &[String]) -> Result<(), String> {
    let name = session_target(c, args)?;
    let kind = args
        .iter()
        .find(|a| a.eq_ignore_ascii_case("column"))
        .cloned()
        .ok_or_else(|| {
            "layout needs a kind\n\n  try: maverickctl layout debug column".to_string()
        })?;
    let view = crate::session::resolve(&name).map_err(|e| e.to_string())?;
    crate::control::dispatch(&view.sid, &format!("layout:{kind}"))
        .map_err(|e| format!("layout {kind} failed: {e}"))?;
    report(c, &name, &format!("layout {kind}"));
    Ok(())
}

fn report(c: &Ctl, session_name: &str, action: &str) {
    if c.json {
        println!(
            "{{\"session\":{},\"action\":{}}}",
            crate::json::json_quote(session_name),
            crate::json::json_quote(action)
        );
    } else {
        println!("{action}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u32, class: &str, instance: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            pid: None,
            class: class.into(),
            instance: instance.into(),
            title: title.into(),
            monitor: 0,
            workspace: 0,
            column: 0,
            index: 0,
            floating: false,
            fullscreen: false,
            maximized: false,
            focused: false,
            geometry: (0, 0, 0, 0),
        }
    }

    /// An id is a fact. A selector that *looks* like an id and names no window
    /// must be an error, not a name search — nobody types `0x999` meaning a
    /// class.
    #[test]
    fn an_id_selector_addresses_exactly_that_window() {
        let windows = vec![
            w(0x42003, "Zed", "Zed", "main.rs"),
            w(0x99, "firefox", "firefox", "M"),
        ];
        assert_eq!(resolve_window(&windows, "0x42003"), Ok(0x42003));
        assert_eq!(
            resolve_window(&windows, "270339"),
            Ok(0x42003),
            "decimal too"
        );
        let err = resolve_window(&windows, "0x999").expect_err("no such window");
        assert!(err.contains("no window"), "{err}");
    }

    /// Exact beats substring, and a name that is ambiguous is refused with the
    /// ids — the user then picks, instead of the tool flipping a coin between
    /// two of their windows.
    #[test]
    fn names_resolve_exactly_or_not_at_all() {
        let windows = vec![
            w(1, "firefox", "Navigator", "GitHub"),
            w(2, "firefox", "firefox", "GitHub — Maverick"),
            w(3, "Zed", "Zed", "main.rs"),
        ];
        // The class matches two, but the instance is unique.
        assert_eq!(resolve_window(&windows, "Zed"), Ok(3));
        assert_eq!(resolve_window(&windows, "zed"), Ok(3), "case-insensitive");
        assert_eq!(resolve_window(&windows, "Navigator"), Ok(1));
        assert_eq!(resolve_window(&windows, "main.rs"), Ok(3));
        // An exact *instance* beats an exact class: two windows can share a
        // class, but the instance is the more specific of the two.
        assert_eq!(resolve_window(&windows, "firefox"), Ok(2));
        // Ambiguity is only reached through a substring, and it is refused
        // rather than guessed.
        let err = resolve_window(&windows, "fox").expect_err("ambiguous");
        assert!(err.contains("matches 2 windows"), "{err}");
        assert!(err.contains("0x1"), "the candidates must be listed: {err}");
        // A substring that is unique is fine — that is what substring matching
        // is for.
        assert_eq!(resolve_window(&windows, "Navi"), Ok(1));
    }

    #[test]
    fn an_unmatched_name_lists_what_is_there() {
        let windows = vec![w(1, "firefox", "Navigator", "GitHub")];
        let err = resolve_window(&windows, "nothing-like-this").expect_err("no match");
        assert!(err.contains("no window matching"), "{err}");
        assert!(err.contains("0x1"), "{err}");
    }

    /// The tree is the only place the column and index of a window are knowable,
    /// so the flattening has to walk the real hierarchy — including floats,
    /// which are not in any column.
    #[test]
    fn flattening_reads_the_hierarchy_including_floats() {
        let tree = crate::json::parse(
            r#"{"sel_mon":0,"monitors":[{"index":0,"active_ws":0,"focused":2,
                 "workspaces":[{"index":0,"layout":"column","columns":[
                    {"width":640.0,"focused":0,"windows":[
                        {"id":1,"pid":100,"class":"alacritty","instance":"alacritty","name":"zsh"},
                        {"id":2,"pid":0,"class":"Zed","instance":"Zed","name":"main.rs","x":0,"y":0,"w":640,"h":480}]},
                    {"width":320.0,"focused":0,"windows":[
                        {"id":3,"pid":300,"class":"firefox","instance":"firefox","name":"M"}]}],"floats":[
                    {"id":4,"pid":400,"class":"mpv","instance":"mpv","name":"video","float":true,"fullscreen":true}]}]}]}"#,
        )
        .expect("valid tree");
        let windows = flatten_windows(&tree);
        assert_eq!(windows.len(), 4);
        assert_eq!(windows[0].id, 1);
        assert_eq!(windows[0].pid, Some(100));
        assert_eq!(windows[0].column, 0);
        assert_eq!(windows[0].index, 0);
        // The second window in the first column.
        assert_eq!(windows[1].id, 2);
        assert_eq!(windows[1].column, 0);
        assert_eq!(windows[1].index, 1);
        // pid 0 is "no pid", never pid 1.
        assert_eq!(windows[1].pid, None);
        assert_eq!(windows[1].geometry, (0, 0, 640, 480));
        assert!(windows[1].focused, "the monitor's focused window");
        assert_eq!(windows[2].column, 1);
        // A float is not in a column, and reports its state.
        assert_eq!(windows[3].id, 4);
        assert!(windows[3].floating);
        assert!(windows[3].fullscreen);
    }

    /// An empty or malformed tree must yield no windows rather than a panic: it
    /// arrives off a socket from a process that may be shutting down.
    #[test]
    fn flattening_a_missing_tree_yields_nothing() {
        for doc in ["{}", r#"{"monitors":[]}"#, "not json"] {
            let tree = crate::json::parse(doc).unwrap_or(crate::json::Json::Null);
            assert!(flatten_windows(&tree).is_empty(), "{doc}");
        }
    }

    /// Every window operation must dispatch the *window-targeted* verb, so a
    /// tool and a keypress reach the same command.
    #[test]
    fn every_window_op_becomes_a_targeted_action() {
        assert_eq!(WindowOp::Focus.action(0x42), "focus_window 0x42");
        assert_eq!(WindowOp::Close.action(0x42), "close_window 0x42");
        assert_eq!(WindowOp::Float.action(0x42), "float_window 0x42");
        assert_eq!(WindowOp::Fullscreen.action(0x42), "fullscreen_window 0x42");
        assert_eq!(
            WindowOp::Move("left".into()).action(0x42),
            "move_window left 0x42"
        );
        // And every one of them is a verb the action parser accepts, which is
        // the property that would otherwise only be caught at runtime.
        for op in [
            WindowOp::Focus,
            WindowOp::Close,
            WindowOp::Float,
            WindowOp::Fullscreen,
            WindowOp::Move("right".into()),
        ] {
            let action = op.action(0x42003);
            assert!(
                action_verbs_would_parse(&action),
                "'{action}' must be a verb the window manager understands"
            );
        }
    }

    /// The window manager's own parser is not reachable from this crate, so the
    /// grammar it accepts is spelled out here: `<verb> <id>` and, for a move,
    /// `<verb> <dir> <id>` in hex.
    fn action_verbs_would_parse(action: &str) -> bool {
        let mut parts = action.split_whitespace();
        let verb = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        let id_ok = |s: &str| {
            s.strip_prefix("0x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .is_some_and(|v| v != 0)
        };
        match verb {
            "focus_window" | "close_window" | "float_window" | "fullscreen_window" => {
                rest.len() == 1 && id_ok(rest[0])
            }
            "move_window" => rest.len() == 2 && is_dir(rest[0]) && id_ok(rest[1]),
            _ => false,
        }
    }

    #[test]
    fn directions_are_recognised_case_insensitively() {
        for d in ["left", "Left", "RIGHT", "up", "down", "next", "prev"] {
            assert!(is_dir(d), "{d}");
        }
        for d in ["sideways", "", "float"] {
            assert!(!is_dir(d), "{d}");
        }
    }
}
