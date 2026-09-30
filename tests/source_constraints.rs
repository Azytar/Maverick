//! Constraints on the production sources that no other test can observe.
//!
//! Two fixes live in functions that need a live X connection, so the behaviour
//! they protect is only reachable from an integration test. Reverting either
//! one left the suite green, because the unit test next to it exercises the
//! *obligation* — what a re-derivation achieves, what going through `retarget`
//! costs — and not that this call site performs one.
//!
//! These tests close that gap the only way it can be closed without a display:
//! by constraining the source itself. That is weaker than exercising the code,
//! and it is deliberately so — a renamed function or a moved call would need
//! this updated, which is a visible cost rather than a silent one. What it buys
//! is that a *reverted* call is caught, which is the failure that matters.

use std::path::Path;
use std::sync::OnceLock;

/// The source of one module inside the `maverick` binary, excluding its
/// `#[cfg(test)]` block so the unit tests' own fixtures are never mistaken for
/// production writes. Panics if the file is not in the tree, which is the right
/// outcome: a constraint that silently skips is worse than no constraint.
fn production_source(rel: &str) -> &'static str {
    static CACHE: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = std::collections::HashMap::new();
        collect(&root, &mut out);
        out
    });
    map.get(rel)
        .unwrap_or_else(|| panic!("{rel} is not in the source tree"))
}

fn collect(dir: &Path, out: &mut std::collections::HashMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            out.insert(rel, strip_test_module(&text));
        }
    }
}

/// Cut the source at its `#[cfg(test)]` module, so nothing below this line —
/// fixtures, test-only helpers, the tests themselves — is visible to these
/// constraints.
fn strip_test_module(src: &str) -> String {
    match src.find("#[cfg(test)]") {
        Some(i) => src[..i].to_string(),
        None => src.to_string(),
    }
}

/// Every non-comment, non-string line containing `needle`, as `(line_no, text)`.
///
/// Crude on purpose: it errs towards *including* a line, so a constraint here
/// can only fail if the code really does contain the thing.
fn code_lines_containing(src: &str, needle: &str) -> Vec<(usize, String)> {
    src.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            // Comments discuss `camera.position` constantly and say nothing
            // about writing it, so they are not findings. Cut at the first
            // `//`; a
            // `//` inside a string literal would be a false cut, and there is
            // none in the code these tests look at.
            let code = raw.split("//").next().unwrap_or("").trim();
            (!code.is_empty() && code.contains(needle)).then(|| (i + 1, code.to_string()))
        })
        .collect()
}

/// Lines that assign through a `camera.position` field, as opposed to reading one
/// or comparing against it.
///
/// The comparison exclusion is what keeps this from flagging
/// `prev.rect != desired_rect`-style predicates: whitespace removed,
/// `x.position==y` and `x.position >= y` both contain `.position=`, so the char
/// after the `=` decides.
fn position_assignments(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (n, line) in code_lines_containing(src, ".position") {
        let packed = line.replace(' ', "");
        let Some(eq) = packed.find(".position=") else {
            continue;
        };
        let after = packed[eq + ".position=".len()..].chars().next();
        if matches!(after, Some('=') | Some('>') | Some('<')) {
            continue;
        }
        out.push((n, line));
    }
    out
}

/// A workspace's scroll offset must move through `Camera::retarget` or
/// `Camera::snap`, never through the public `position` field.
///
/// Both are the only sanctioned writers, and their contract is to refuse a
/// non-finite value: a raw field write bypasses that, and the offset then
/// reaches a `ConfigureWindow` as a NaN rect, which X11 silently drops while
/// the WM's bookkeeping moves on — a window whose `client.geom` no longer
/// matches anything on screen, so every click on it lands elsewhere.
#[test]
fn no_production_code_writes_the_camera_position_field() {
    // The pattern is any `.position` assignment on a camera, not the literal
    // `camera.position =`: the binding a caller happens to use (`cam.position`,
    // `ws.camera.position`, `monitors[i].workspaces[0].camera.position`) is
    // exactly what a bypass would look like, and a check that only matches one
    // spelling of it is not a check. There are no production `.position` writes
    // to allowlist - the only writers are `Camera::retarget` and `Camera::snap`,
    // which assign through `self`, and both live in `maverick-core`, not here.
    let offenders: Vec<String> = [
        "backend/x11/manage.rs",
        "backend/x11/events.rs",
        "backend/x11/render.rs",
        "backend/x11/struts.rs",
        "backend/x11/pointer.rs",
        "core/commands.rs",
        "core/layout.rs",
    ]
    .iter()
    .map(|f| (*f, production_source(f)))
    .flat_map(|(f, src)| {
        position_assignments(src)
            .into_iter()
            .filter(|(_, line)| line.contains("camera"))
            .map(move |(n, t)| format!("{f}:{n}: {t}"))
    })
    .collect();
    assert!(
        offenders.is_empty(),
        "camera.position must only be written by Camera::retarget/snap. Direct \
         writes: {offenders:?}"
    );
}

/// A monitor's screen change invalidates every workspace's scroll target, so the
/// handler that adopts the new screen has to re-derive it before projecting.
///
/// This is the ordering rule `apply_dock_strut` already documents ("every
/// camera mutation must precede the projection") applied to the
/// path that was missing it. The unit test for the fix builds the scene and
/// re-derives the target *itself*, so it passes whether or not the handler does —
/// which is exactly what the sabotage audit found.
#[test]
fn the_monitor_change_handler_re_derives_the_camera_before_projecting() {
    let src = production_source("backend/x11/events.rs");
    let handler_start = src
        .find("fn handle_monitor_change")
        .unwrap_or_else(|| panic!("handle_monitor_change not found"));
    // The body runs to the end of the function; a generous window is fine, the
    // property is "the call is in this handler at all".
    let body = &src[handler_start..];

    let retarget = body.find("retarget_cameras(").unwrap_or_else(|| {
        panic!(
            "handle_monitor_change must call retarget_cameras: it adopts a new \\
             screen, which changes the workarea, which invalidates every camera \\
             target. Without the call a resolution change leaves the camera \\
             pointing into a ribbon that no longer exists, and the user is left \\
             with an empty desktop that nothing recovers."
        )
    });
    let first_arrange = body
        .find("self.arrange(i)")
        .expect("handle_monitor_change must arrange after adopting the new screen");
    assert!(
        retarget < first_arrange,
        "the retarget must precede the projection, or client.geom is written from \\
         a target that is already out of range"
    );
    // Both branches — the geometry-only one and the topology one — need it.
    let calls = code_lines_containing(body, "retarget_cameras(").len();
    assert!(
        calls >= 2,
        "both monitor-change branches change the workarea and both must retarget; \
         found {calls} call site(s)"
    );
}

// ── The input path never waits on the X server ──────────────────────────────
//
// A wheel notch used to cost ~80 X requests, a dozen of them round trips, all
// on the one thread that also drains the socket; at wheel rates the backlog grew
// without bound. The fixes are only durable if the *shape* is protected, and the
// shape is: no handler on the input path blocks for a reply, and nothing per
// event re-installs state that does not depend on the event.

/// The body of `fn name`, from its signature to the first line indented back to
/// the signature's own level (a closing brace). Panics if the function is gone,
/// for the same reason `production_source` does.
fn fn_body(src: &str, name: &str) -> String {
    let sig = format!("fn {name}(");
    let start = src
        .find(&sig)
        .unwrap_or_else(|| panic!("`{sig}` not found: renamed or moved, update this constraint"));
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let indent = src[line_start..].chars().take_while(|c| *c == ' ').count();
    let closer = format!("\n{}}}\n", " ".repeat(indent));
    let end = src[start..].find(&closer).map_or(src.len(), |i| start + i);
    src[start..end].to_string()
}

fn assert_no_blocking_reply(src: &str, func: &str, why: &str) {
    // `src` is either a whole module (then `func` names the function to slice
    // out) or an already-sliced body (then `func` is only the label).
    let body = if src.contains(&format!("fn {func}(")) {
        fn_body(src, func)
    } else {
        src.to_string()
    };
    for needle in [".reply()", ".check()", ".sync()", "get_input_focus", "get_property"] {
        let hits = code_lines_containing(&body, needle);
        assert!(
            hits.is_empty(),
            "`{func}` is on the input path and must not block on the X server, but it \
             contains `{needle}`: {hits:?}\n{why}"
        );
    }
}

#[test]
fn the_input_path_handlers_never_block_on_a_reply() {
    let why = "Each round trip stalls the only thread that drains the socket; one per \
               event is what let a spinning wheel outrun the loop. Defer the work to \
               `flush_pending`, or serve it from state read at manage time.";
    let pointer = production_source("backend/x11/pointer.rs");
    // Only the part of `on_button_press` that runs for the wheel and for click-to-
    // focus. What follows it starts a drag, and a drag start legitimately waits
    // once for `GrabPointer`'s status (once per drag, not per notch).
    let press = fn_body(pointer, "on_button_press");
    let press = press
        .split("let client_win = self.find_client(e.event);")
        .next()
        .unwrap();
    assert_no_blocking_reply(press, "on_button_press (wheel path)", why);
    assert_no_blocking_reply(pointer, "scroll_camera_with_wheel", why);
    assert_no_blocking_reply(pointer, "apply_wheel_steps", why);
    let events = production_source("backend/x11/events.rs");
    assert_no_blocking_reply(events, "on_focus_out", why);
    assert_no_blocking_reply(events, "on_key", why);
}

#[test]
fn focus_changes_do_not_reinstall_button_grabs() {
    // The grab set is a function of the window and the modifier map, not of
    // focus. Re-issuing it per focus change cost ~70 requests per wheel notch.
    let render = production_source("backend/x11/render.rs");
    let hits = code_lines_containing(render, "grab_buttons(");
    assert!(
        hits.is_empty(),
        "render.rs must not (re)install button grabs on focus changes; they are \
         installed once in `manage` (and again by `refresh_keyboard` when the \
         lock-modifier map moves): {hits:?}"
    );
}

#[test]
fn a_plain_wheel_notch_is_not_grabbed() {
    // Buttons 4-7 are grabbed only together with Mod4. A catch-all SYNC grab
    // freezes the pointer and round-trips through the WM for every notch of
    // ordinary scrolling, which never needed the WM.
    let body = fn_body(production_source("backend/x11/input.rs"), "grab_buttons");
    assert!(
        !body.contains("ButtonIndex::ANY,\n                ModMask::ANY")
            && !body.contains("ButtonIndex::ANY, win, ModMask::ANY,"),
        "grab_buttons must not install an AnyButton/AnyModifier grab"
    );
    let wheel_grab = body
        .find("for wheel in 4u8..=7")
        .expect("the Mod4+wheel grab loop is gone");
    let anymod_click = body.find("ModMask::ANY,").expect("the click grab is gone");
    assert!(
        anymod_click < wheel_grab,
        "the wheel buttons must only be grabbed inside the Mod4 loop"
    );
}

#[test]
fn wm_protocols_are_served_from_a_cache() {
    let body = fn_body(production_source("backend/x11/ewmh.rs"), "has_protocol");
    assert!(
        body.contains("self.protocols"),
        "has_protocol must consult the per-window cache before asking the server"
    );
    let events = production_source("backend/x11/events.rs");
    assert!(
        !code_lines_containing(events, "self.atoms.wm_protocols").is_empty(),
        "on_property must invalidate the cache when WM_PROTOCOLS changes"
    );
}

#[test]
fn wheel_notches_are_queued_not_applied_inline() {
    let pointer = production_source("backend/x11/pointer.rs");
    let body = fn_body(pointer, "scroll_camera_with_wheel");
    assert!(
        body.contains("wheel_steps") && !body.contains("run_effects") && !body.contains("dispatch"),
        "scroll_camera_with_wheel must only record the notch; applying it inline is \
         one focus change and one arrange per notch"
    );
    // `mod.rs` carries an early `#[cfg(test)] mod tests;`, which would make
    // `production_source` drop everything after it, so read the file whole.
    let mod_rs = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/backend/x11/mod.rs"),
    )
    .unwrap();
    let flush = fn_body(&mod_rs, "flush_pending");
    assert!(
        flush.contains("apply_wheel_steps"),
        "flush_pending must apply the queued notches, once per turn"
    );
}

// Arrange emits ConfigureWindow only for changed windows.
//
// One column step legitimately moves every visible window — the ribbon scrolls
// by rewriting the camera and re-projecting, so each window gets one
// ConfigureWindow plus its one synthetic ConfigureNotify. What must never come
// back is *amplification*: re-running the projection with identical geometry
// must cost zero X requests. That holds only while every geometry write goes
// through the `AppliedState` diff — `reconcile` for the arrange path,
// `apply_geom` for the out-of-band sinks — and `emit_geometry` stays the
// single writer pairing one ConfigureWindow with its one ConfigureNotify.
//
// The entry point is `arrange_full`, and it owns the whole cycle: project,
// diff, emit. The constraint fails closed if it ever stops diffing.
#[test]
fn arrange_emits_geometry_only_through_the_reconciler() {
    let render = production_source("backend/x11/render.rs");
    // No direct X geometry write on the arrange entry path: geometry leaves
    // only via `emit_geometry`, fed with the diff's effects. (Stack-only
    // `configure_window`s live in `raise`/`stack_overlay`, not here.)
    let arrange = fn_body(render, "arrange_full");
    for needle in ["configure_window", "send_event"] {
        assert!(
            code_lines_containing(&arrange, needle).is_empty(),
            "arrange_full must not issue `{needle}` directly; \
             every geometry write goes through the reconciler's effects"
        );
    }
    // Without the diff the projection would re-emit every window on every
    // pass, which is the amplification this rule exists to prevent.
    assert!(
        !code_lines_containing(&arrange, "reconcile(").is_empty(),
        "arrange_full must diff Desired vs Applied through `reconcile`; \
         emitting the projection without the diff is one ConfigureWindow per \
         window per pass"
    );
    // The out-of-band sinks (hide/re-show, float settle, client requests)
    // share the same gate: diff first, emit only on change.
    let apply = fn_body(render, "apply_geom");
    assert!(
        !code_lines_containing(&apply, "applied.diff").is_empty(),
        "apply_geom must diff against `AppliedState` before emitting; \
         it is the reconciler's gate for every non-arrange geometry write"
    );
    assert!(
        code_lines_containing(&apply, "configure_window").is_empty(),
        "apply_geom must not call `configure_window` directly; \
         `emit_geometry` is the single writer"
    );
    // The single writer pairs exactly one ConfigureWindow with its one
    // synthetic ConfigureNotify: SendEvent is 1:1 with a legitimate move,
    // not an independent storm.
    let emit = fn_body(render, "emit_geometry");
    assert_eq!(
        code_lines_containing(&emit, "configure_window").len(),
        1,
        "emit_geometry must hold the only `configure_window` on the geometry path"
    );
    assert_eq!(
        code_lines_containing(&emit, "send_event").len(),
        1,
        "emit_geometry pairs one synthetic ConfigureNotify with each real configure"
    );
}

/// The X11 backend must not decide anything from a window's identity.
///
/// `WM_CLASS` and `_NET_WM_NAME` are free-form strings the application chooses,
/// so reading them is reading an untrusted string; folding one into a
/// case-insensitive matching key in the backend turns that string into a layout
/// decision the user cannot see, cannot override, and cannot turn off. A window
/// floats for a property it published (`_NET_WM_WINDOW_TYPE`, `_NET_WM_STATE`,
/// `WM_NORMAL_HINTS`, `WM_TRANSIENT_FOR`), for a `[[rules]]` entry, or because
/// a previous session left it that way — and for nothing else.
///
/// Per-application policy belongs in `compiled_config().rules`, where a user's
/// `[[rules]]` replaces it wholesale. That substitution has to happen through
/// `Rule::matches`; the backend may only *read* the strings and hand them over.
#[test]
fn the_backend_never_matches_on_a_windows_identity() {
    static BACKEND: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    for (rel, src) in BACKEND.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = std::collections::HashMap::new();
        collect(&root, &mut out);
        out
    }) {
        if !rel.starts_with("backend/") {
            continue;
        }
        for needle in [
            ".class.to_lowercase()",
            ".instance.to_lowercase()",
            ".name.to_lowercase()",
            ".title.to_lowercase()",
        ] {
            let hits = code_lines_containing(src, needle);
            assert!(
                hits.is_empty(),
                "{rel}:{} case-folds a window's identity into a matching key (`{needle}`). \
                 Per-application policy belongs in `config::compiled_config().rules`; \
                 the backend decides from the client's own X11 properties and from `Cfg::rules`.",
                hits[0].0,
            );
        }
    }
}
