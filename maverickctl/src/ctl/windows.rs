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

use crate::client;
use maverick_sys::json::Json;

use super::session::{available_sessions, split_session_and_rest, truncate};
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
///
/// The keys are the ones `core::ipc::tree_json` actually writes. The geometry
/// is a single four-element `geom` array rather than four scalars, and the
/// title is `title` — reading a schema that resembles it silently produces a
/// window with no title and no geometry, which looks like a window manager bug
/// rather than a reader bug. The test fixture below is a document captured from
/// a running session for that reason.
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
    let geom = w.get("geom").map(Json::as_array).unwrap_or(&[]);
    let coord = |i: usize| geom.get(i).and_then(Json::as_f64).unwrap_or(0.0);
    WindowInfo {
        id,
        pid,
        class: w.str_field("class").to_string(),
        instance: w.str_field("instance").to_string(),
        title: w.str_field("title").to_string(),
        monitor: monitor_of(w, monitor),
        workspace: workspace_of(w, workspace),
        column,
        index,
        floating: w.bool_field("float"),
        fullscreen: w.bool_field("fullscreen"),
        maximized: w.bool_field("maximized"),
        // The window's own `focus` is authoritative; the monitor's `focused`
        // slot is the fallback for a document that predates it.
        focused: w.bool_field("focus") || (mon_focus != 0 && mon_focus == id),
        geometry: (
            coord(0) as i32,
            coord(1) as i32,
            coord(2) as u32,
            coord(3) as u32,
        ),
    }
}

/// A window's own `monitor`, or the one the walk found it on.
fn monitor_of(w: &Json, walked: usize) -> usize {
    match w.get("monitor").and_then(Json::as_u64) {
        Some(m) => m as usize,
        None => walked,
    }
}

/// A window's own `workspace`, or the one the walk found it on.
fn workspace_of(w: &Json, walked: usize) -> usize {
    match w.get("workspace").and_then(Json::as_u64) {
        Some(ws) => ws as usize,
        None => walked,
    }
}

/// Resolve a window selector to a window id.
///
/// Precedence, and the reason for it: an id is a fact, a name is a guess. A
/// selector that parses as an id *and* names a live window is that window; a
/// selector that parses as an id and names nothing is an error rather than a
/// name search, because "0x999" was never a class.
///
/// Past that, the name is matched exactly against the class, the instance and
/// the title, and only then as a substring — and at *both* stages a match that
/// several windows satisfy is refused with the ids listed. Taking the first
/// would be a coin flip between the user's own windows, and the whole reason
/// stable ids are printed in the first place is so a caller can pick one.
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
    let fields: [fn(&WindowInfo) -> &String; 3] = [|w| &w.instance, |w| &w.class, |w| &w.title];
    let exact: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| fields.iter().any(|f| f(w).eq_ignore_ascii_case(sel)))
        .collect();
    match exact.as_slice() {
        [only] => return Ok(only.id),
        [] => {}
        many => {
            return Err(ambiguous(sel, "matches", many));
        }
    }
    let lower = sel.to_ascii_lowercase();
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
        many => Err(ambiguous(sel, "matches", many)),
    }
}

/// The refusal for a selector more than one window satisfies.
fn ambiguous(sel: &str, verb: &str, many: &[&WindowInfo]) -> String {
    format!(
        "'{sel}' {verb} {} windows — use the id:\n\n{}",
        many.len(),
        candidates(many)
    )
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
    Ok((view.sid.clone(), tree_of(&name, &view.sid)?))
}

/// The windows `sid` manages, read from its `tree` query.
///
/// A refusal arrives as a successful exchange carrying an `error …` body, so it
/// is classified before the document is parsed. Letting one reach the parse
/// reported it as "the window tree was not valid JSON" — telling the user the
/// instance sent nonsense when it had in fact said no, and discarding the reason
/// it gave for refusing.
fn tree_of(name: &str, sid: &str) -> Result<Vec<WindowInfo>, String> {
    let tree = client::query(sid, "tree")
        .map_err(|e| format!("cannot read the window tree of '{name}': {e}"))?;
    if let Some(why) = super::refusal(&tree) {
        return Err(format!("cannot read the window tree of '{name}': {why}"));
    }
    let tree = maverick_sys::json::parse(&tree)
        .ok_or_else(|| format!("the window tree of '{name}' was not valid JSON"))?;
    Ok(flatten_windows(&tree))
}

/// Dispatch `action` to `sid`, reporting a refusal under `context`.
///
/// The reply to a `dispatch` is a queue receipt, and the only other thing it can
/// be is a refusal: an over-long line, a full command queue, an instance that has
/// begun restarting. Checking only the transport turned those into a completed
/// window operation — the id printed, `--json` promising an action that was never
/// applied, and exit status 0 — which is exactly what a caller running the same
/// command from a script cannot detect.
fn dispatch(sid: &str, action: &str, context: &str) -> Result<(), String> {
    let reply = client::dispatch(sid, action).map_err(|e| format!("{context}: {e}"))?;
    match super::refusal(&reply) {
        Some(why) => Err(format!("{context}: {why}")),
        None => Ok(()),
    }
}

/// `maverickctl window …`
pub fn run(c: &mut Ctl, args: &[String]) -> Result<bool, String> {
    // As in the session group: globals are lifted to the front of `args`, so
    // the verb is the first positional, not the first argument.
    let verb = c
        .positionals
        .first()
        .map(|&i| args[i].as_str())
        .unwrap_or("list");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match verb {
        "list" | "ls" => {
            window_list(c, rest)?;
        }
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
///
/// Returns the error rather than printing it: a command that cannot produce the
/// data it was asked for has failed, and only the caller's exit status can say
/// so. The sibling verbs below already propagate, and a listing that reported
/// "session 'x' does not exist" with status 0 made `--json` a silent empty
/// stream that a script could not distinguish from a session with no windows.
fn window_list(c: &Ctl, args: &[String]) -> Result<bool, String> {
    let (sid, windows) = windows_of(c, args)?;
    if c.json {
        let items: Vec<String> = windows.iter().map(window_json).collect();
        println!(
            "{{\"session\":{},\"windows\":[{}]}}",
            maverick_sys::json::json_quote(&sid),
            items.join(",")
        );
        return Ok(true);
    }
    if windows.is_empty() {
        println!("No managed windows in session '{sid}'.");
        return Ok(true);
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
    Ok(true)
}

fn window_json(w: &WindowInfo) -> String {
    let num = |v: u32| v.to_string();
    format!(
        "{{\"id\":{},\"id_hex\":\"{:#x}\",\"pid\":{},\"class\":{},\"instance\":{},\"title\":{},\"monitor\":{},\"workspace\":{},\"column\":{},\"index\":{},\"floating\":{},\"fullscreen\":{},\"maximized\":{},\"focused\":{},\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}",
        w.id,
        w.id,
        w.pid.map_or_else(|| "null".to_string(), num),
        maverick_sys::json::json_quote(&w.class),
        maverick_sys::json::json_quote(&w.instance),
        maverick_sys::json::json_quote(&w.title),
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
    // The selector is the first positional *after* the session — same rule as
    // every other window verb, and the reason `window inspect debug firefox`
    // inspects `firefox` and not a window called "debug".
    let selector = window_selector(c, args).unwrap_or_else(|| {
        // No selector: the focused window is what a user means by "the window"
        // when they do not name one.
        windows
            .iter()
            .find(|w| w.focused)
            .map(|w| w.class.clone())
            .unwrap_or_default()
    });
    if selector.is_empty() {
        return Err(format!(
            "no window in session '{sid}' to inspect\n\n{}",
            available_sessions()
        ));
    }
    let id = resolve_window(&windows, &selector)?;
    let w = windows
        .iter()
        .find(|w| w.id == id)
        .expect("resolve_window returned an id from this list");

    // The window manager's own view of the same window, so the two are
    // reported together: what the tool sees and what the WM believes.
    let live = client::query(&sid, "inspect")
        .ok()
        .and_then(|j| maverick_sys::json::parse(&j));
    if c.json {
        let doc = window_json(w);
        let mut fields = match maverick_sys::json::parse(&doc) {
            Some(Json::Obj(f)) => f,
            _ => Vec::new(),
        };
        if let Some(v) = &live {
            for key in ["layout", "sel_mon"] {
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

/// The window a verb names, from the arguments that follow it.
///
/// `args` is everything *after* the verb, so its first positional is the
/// session and the second is the window. Directions are excluded because a
/// direction is the verb's own argument, never the target, and a `--flag` is
/// excluded because a program's flags must never be read as a name.
pub(crate) fn window_selector(c: &Ctl, args: &[String]) -> Option<String> {
    let mut positionals = args
        .iter()
        .filter(|a| !a.starts_with('-') && !is_dir(a) && !c.is_own_flag(a));
    // Skip the session.
    let _ = positionals.next();
    positionals.next().cloned()
}

/// Resolve the window, then dispatch the action.
fn act(c: &Ctl, args: &[String], verb: &str, op: WindowOp) -> Result<(), String> {
    let name = session_target(c, args)?;
    let view = crate::session::resolve(&name).map_err(|e| e.to_string())?;
    let windows = tree_of(&name, &view.sid)?;

    // The window selector is the first positional *after* the session, never
    // the session itself: `window focus debug firefox` addresses `firefox`, and
    // reading the first positional as the selector would ask the window manager
    // to act on a session name.
    let selector = window_selector(c, args).unwrap_or_else(|| "focused".to_string());
    let id = if selector == "focused" {
        windows
            .iter()
            .find(|w| w.focused)
            .map(|w| w.id)
            .ok_or_else(|| format!("no window is focused in session '{name}'"))?
    } else {
        resolve_window(&windows, &selector)?
    };

    dispatch(
        &view.sid,
        &op.action(id),
        &format!("{verb} failed for {id:#x}"),
    )?;
    if c.json {
        println!(
            "{{\"session\":{},\"window\":{},\"action\":{}}}",
            maverick_sys::json::json_quote(&name),
            id,
            maverick_sys::json::json_quote(&op.action(id))
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
    dispatch(
        &view.sid,
        &format!("focus:{dir}"),
        &format!("camera {dir} failed"),
    )?;
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
    // The amount is the first positional *after* the session: a signed
    // percentage starts with `-` and must not be mistaken for one of this
    // tool's own flags.
    let (_, rest) =
        split_session_and_rest(c, args).ok_or_else(|| "resize needs a session".to_string())?;
    let amount = rest
        .iter()
        .map(|a| a.trim())
        .find(|a| !a.is_empty())
        .map(str::to_string)
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
    dispatch(&view.sid, &action, &format!("resize {amount} failed"))?;
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
    dispatch(
        &view.sid,
        &format!("layout:{kind}"),
        &format!("layout {kind} failed"),
    )?;
    report(c, &name, &format!("layout {kind}"));
    Ok(())
}

fn report(c: &Ctl, session_name: &str, action: &str) {
    if c.json {
        println!(
            "{{\"session\":{},\"action\":{}}}",
            maverick_sys::json::json_quote(session_name),
            maverick_sys::json::json_quote(action)
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
        // Ambiguity is refused at the *exact* stage too, not only on a
        // substring: three windows can all be exactly "xterm", and taking the
        // first would be a coin flip between the user's own windows.
        let err = resolve_window(&windows, "fox").expect_err("ambiguous");
        assert!(err.contains("matches 2 windows"), "{err}");
        assert!(err.contains("0x1"), "the candidates must be listed: {err}");
        // A substring that is unique is fine — that is what substring matching
        // is for.
        assert_eq!(resolve_window(&windows, "Navi"), Ok(1));

        // And an exact name several windows share is refused the same way,
        // listing every candidate.
        let same = vec![w(1, "xterm", "xterm", ""), w(2, "xterm", "xterm", "")];
        let err = resolve_window(&same, "xterm").expect_err("ambiguous");
        assert!(err.contains("matches 2 windows"), "{err}");
        assert!(err.contains("0x1") && err.contains("0x2"), "{err}");
        // An id is the way out, and it is always unambiguous.
        assert_eq!(resolve_window(&same, "0x2"), Ok(2));
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
    /// which are in no column.
    ///
    /// The fixture is a document captured from a running session, not one
    /// written to match the reader: an earlier version of this test used a
    /// schema that merely *resembled* the real one, and it passed while the
    /// tool reported every window as having no title and no geometry — which
    /// reads as a window manager bug, not a reader bug. The keys are `title`
    /// and a four-element `geom` array; anything that changes them has to
    /// change this fixture with it.
    #[test]
    fn flattening_reads_the_hierarchy_including_floats() {
        let tree = maverick_sys::json::parse(
            r#"{"sel_mon":0,"monitors":[{"index":0,"active_ws":0,"focused":2,
                 "workspaces":[{"index":0,"layout":"column","columns":[
                    {"width":640.0,"focused":0,"windows":[
                        {"id":1,"pid":100,"class":"alacritty","instance":"alacritty","title":"zsh","monitor":0,"workspace":0,"float":false,"fullscreen":false,"maximized":false,"sticky":false,"geom":[0,8,640,750],"focus":false,"overlay":false},
                        {"id":2,"pid":0,"class":"Zed","instance":"Zed","title":"main.rs","monitor":0,"workspace":0,"float":false,"fullscreen":false,"maximized":false,"sticky":false,"geom":[640,8,320,720],"focus":true,"overlay":false}]},
                    {"width":320.0,"focused":0,"windows":[
                        {"id":3,"pid":300,"class":"firefox","instance":"firefox","title":"M","monitor":0,"workspace":0,"float":false,"fullscreen":false,"maximized":false,"sticky":false,"geom":[960,8,320,720],"focus":false,"overlay":false}]}],"floats":[
                    {"id":4,"pid":400,"class":"mpv","instance":"mpv","title":"video","monitor":0,"workspace":0,"float":true,"fullscreen":true,"maximized":false,"sticky":false,"geom":[100,100,320,240],"focus":false,"overlay":true}]}]}]}"#,
        )
        .expect("valid tree");
        let windows = flatten_windows(&tree);
        assert_eq!(windows.len(), 4);

        assert_eq!(windows[0].id, 1);
        assert_eq!(windows[0].pid, Some(100));
        assert_eq!(windows[0].class, "alacritty");
        assert_eq!(windows[0].instance, "alacritty");
        assert_eq!(windows[0].title, "zsh", "the title key is `title`");
        assert_eq!(windows[0].geometry, (0, 8, 640, 750), "geometry is `geom`");
        assert_eq!(windows[0].column, 0);
        assert_eq!(windows[0].index, 0);

        // The second window in the first column.
        assert_eq!(windows[1].id, 2);
        assert_eq!(windows[1].column, 0);
        assert_eq!(windows[1].index, 1);
        assert_eq!(windows[1].pid, None, "pid 0 is 'no pid', never pid 1");
        assert!(windows[1].focused, "the window says so itself");
        assert_eq!(windows[1].geometry, (640, 8, 320, 720));

        assert_eq!(windows[2].column, 1);

        // A float is in no column, and reports its state.
        assert_eq!(windows[3].id, 4);
        assert!(windows[3].floating);
        assert!(windows[3].fullscreen);
        assert_eq!(windows[3].title, "video");
    }

    /// A window's own `monitor`/`workspace` win over the walk's position, so a
    /// document that disagrees with itself reports the window's own answer.
    #[test]
    fn a_windows_own_placement_beats_the_walk_position() {
        let tree = maverick_sys::json::parse(
            r#"{"sel_mon":0,"monitors":[{"index":0,"active_ws":0,"focused":0,
                 "workspaces":[{"index":0,"columns":[{"width":1.0,"focused":0,"windows":[
                    {"id":1,"pid":7,"class":"a","instance":"a","title":"t","monitor":1,"workspace":3,"geom":[0,0,10,10]}]}],"floats":[]}]}]}"#,
        )
        .expect("valid");
        let w = &flatten_windows(&tree)[0];
        assert_eq!(w.monitor, 1);
        assert_eq!(w.workspace, 3);
    }

    /// An empty or malformed tree must yield no windows rather than a panic: it
    /// arrives off a socket from a process that may be shutting down.
    #[test]
    fn flattening_a_missing_tree_yields_nothing() {
        for doc in ["{}", r#"{"monitors":[]}"#, "not json"] {
            let tree = maverick_sys::json::parse(doc).unwrap_or(maverick_sys::json::Json::Null);
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
