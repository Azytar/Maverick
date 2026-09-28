//! Constraints on the production sources that no other test can observe.
//!
//! Two of this campaign's fixes live in functions that need a live X connection,
//! so the behaviour they protect is only reachable from an integration test. The
//! sabotage audit showed the obvious result: reverting either fix left the suite
//! green, because the unit test next to it exercises the *obligation* — what a
//! re-derivation achieves, what going through `retarget` costs — and not that
//! this call site performs one.
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
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .collect()
}

/// A window's scroll target must move through `Camera::retarget`, never through
/// the public `target` field.
///
/// `retarget` is the only sanctioned writer of the destination, and its contract
/// includes dropping stale momentum when the destination actually moves. Writing
/// the field instead keeps that momentum, and the effect is a camera that travels
/// further in the direction it was already going *after* its destination has been
/// placed behind it.
///
/// The concrete case this exists for: `unmanage` re-derives the destination for
/// the shorter ribbon when a window closes, and that routinely happens while the
/// spring is in flight — a user holding a scroll key who closes a window. It was
/// `cam.target = scroll`, and the unit test next to the fix exercises `retarget`
/// against a raw field write on the primitive, so it passed either way.
#[test]
fn no_production_code_writes_the_camera_target_field() {
    let offenders: Vec<String> = [
        "backend/x11/manage.rs",
        "backend/x11/events.rs",
        "backend/x11/render.rs",
        "backend/x11/struts.rs",
        "backend/x11/pointer.rs",
        "backend/x11/manage.rs",
        "core/commands.rs",
        "core/layout.rs",
    ]
    .iter()
    .map(|f| (*f, production_source(f)))
    .flat_map(|(f, src)| {
        code_lines_containing(src, "camera.target =")
            .into_iter()
            .map(move |(n, t)| format!("{f}:{n}: {t}"))
    })
    .collect();
    assert!(
        offenders.is_empty(),
        "camera.target must only be written by Camera::retarget. Direct writes: \\
         {offenders:?}"
    );
}

/// A monitor's screen change invalidates every workspace's scroll target, so the
/// handler that adopts the new screen has to re-derive it before projecting.
///
/// This is the ordering rule `apply_dock_strut` already documents ("every
/// `camera.target` mutation must precede the settled projection") applied to the
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
