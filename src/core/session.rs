// maverick/src/core/session.rs
//
// Session persistence + recovery for Maverick.
//
// A "session" is the *logical* topology of the desktop that must survive a
// reload / restart: which windows live on which workspace, in which columns,
// their weights, which workspace is active, and what had focus. It is NOT the
// full `State` — everything that is runtime (XIDs as handles are fine to store
// as *identity keys*, but geometry, camera springs, zoom/overview animation,
// grid caches, compositor state and presenting windows are reconstructed, never
// trusted from disk).
//
// The pipeline is strictly staged, and each stage is observable:
//
//     PersistedSession            (versioned, serializable — pure data)
//         │  parse + schema check
//         ▼
//     PersistedSession            (decoded from file)
//         │  validate()           (internal invariants, no X11)
//         ▼
//     ValidatedSession
//         │  commit()             (reconcile against live X11 windows, build
//         │                        a fresh RuntimeState topology)
//         ▼
//     State                       (single atomic swap — the old State is
//                                  untouched until commit succeeds)
//
// A session file is NEVER trusted end-to-end: `parse_json` decodes it, the
// schema version is checked, `validate` rejects structurally impossible state,
// and `commit` re-checks every window reference against the windows that are
// actually alive on the X server. A failure at any stage leaves the current
// visible session intact (the caller decides whether to fall back to a
// config-only reload, but never applies a partially-built State).

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::str::FromStr;

use crate::config::Cfg;
use crate::core::layout::{fs_ctx, ideal_scroll};
use crate::types::{Column, LayoutKind, Monitor, Rect, State, WindowId, Workspace};

/// Schema version of the persisted session format.
///
/// Bump when the on-disk shape changes. The loader accepts exactly the
/// current version; anything else is reported as a *schema* error (old
/// versions are never silently "migrated" — see `docs/architecture/
/// session-lifecycle.md`).
pub const SESSION_SCHEMA_VERSION: u32 = 1;

const WINFLAG_FLOAT: u16 = 1 << 0;

/// The stage at which a session operation failed. Kept on every
/// `SessionError` so diagnostics can point at the exact phase instead of a
/// bare "reload failed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStage {
    /// Building a `PersistedSession` from the live `State`.
    Snapshot,
    /// Reading the session file from disk (missing/truncated/empty file).
    Read,
    /// Decoding the JSON document (syntax / truncation).
    Parse,
    /// The document parsed but the schema version is unsupported.
    Schema,
    /// Semantic validation of decoded structure failed.
    Validate,
    /// Building the runtime `State` topology from the validated session.
    Construct,
    /// Flipping the committed state into place.
    Commit,
}

impl SessionStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Read => "read",
            Self::Parse => "parse",
            Self::Schema => "schema",
            Self::Validate => "validate",
            Self::Construct => "construct",
            Self::Commit => "commit",
        }
    }
}

/// A failure in the session pipeline. The `stage` tells a log reader exactly
/// where the chain broke; the message describes what was wrong.
#[derive(Debug, Clone)]
pub struct SessionError {
    pub stage: SessionStage,
    pub message: String,
}

impl SessionError {
    pub fn new(stage: SessionStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
    pub fn snapshot(m: impl Into<String>) -> Self {
        Self::new(SessionStage::Snapshot, m)
    }
    pub fn read(m: impl Into<String>) -> Self {
        Self::new(SessionStage::Read, m)
    }
    pub fn parse(m: impl Into<String>) -> Self {
        Self::new(SessionStage::Parse, m)
    }
    pub fn schema(m: impl Into<String>) -> Self {
        Self::new(SessionStage::Schema, m)
    }
    pub fn validate(m: impl Into<String>) -> Self {
        Self::new(SessionStage::Validate, m)
    }
    pub fn construct(m: impl Into<String>) -> Self {
        Self::new(SessionStage::Construct, m)
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session {}: {}", self.stage.as_str(), self.message)
    }
}

impl Error for SessionError {}

// ─── The persisted model ─────────────────────────────────────────────────────

/// What a Maverick session persists: the logical workspace topology of every
/// monitor, plus the monitor that was selected and the per-monitor active
/// workspace / focus. No screen geometry, no client geometry, no animation
/// state — those are runtime and are reconstructed on restore.
#[derive(Debug, Clone, PartialEq)]
pub struct PersistedSession {
    pub version: u32,
    /// `CARGO_PKG_VERSION` of the writer — purely diagnostic.
    pub app_version: String,
    /// Logical per-monitor topology. Indexes map onto the *live* monitor list
    /// at restore time (monitors are a runtime, X11-derived resource).
    pub monitors: Vec<PersistedMonitor>,
    /// Selected monitor index (clamped against the live monitor count on
    /// restore).
    pub sel_mon: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedMonitor {
    /// One logical workspace per configured tag. `len()` must equal the
    /// restore-time `n_tags` (validated after expansion/clamping).
    pub workspaces: Vec<PersistedWorkspace>,
    pub active_ws: usize,
    /// The monitor's focused window id, or `None`. Stored as an identity key
    /// only — `commit` re-checks it against the live window set.
    pub focused: Option<WindowId>,
    pub focus_stack: Vec<WindowId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedWorkspace {
    pub tag: u32,
    pub layout: LayoutKind,
    pub columns: Vec<PersistedColumn>,
    /// Floating window ids, in stacking (z) order.
    pub floats: Vec<WindowId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedColumn {
    /// Window ids, top to bottom.
    pub windows: Vec<WindowId>,
    /// Column width as a fraction of the workarea (independent, true-scroll).
    pub weight: f32,
    /// Row inside the column that had focus.
    pub focused: usize,
}

/// `PersistedSession` that passed `validate()`.
///
/// Carrying the validated value as its own type keeps the "archivo → State"
/// path non-existent: callers must pass through validation before they can
/// even hold a `ValidatedSession`, and the type system stops a validated
/// session from being confused with a raw (possibly corrupt) `PersistedSession`.
#[derive(Debug, Clone)]
pub struct ValidatedSession {
    inner: PersistedSession,
}

// ─── Snapshotting: State → PersistedSession ──────────────────────────────────

impl PersistedSession {
    /// Build a versioned snapshot of the *logical* topology of `state`.
    ///
    /// Focus references are filtered to the live client set *and* to ids that
    /// actually appear somewhere on the monitor's workspace tree, so a stale
    /// `mon.focused` / `focus_stack` left behind by a missed UnmapNotify is
    /// pruned here — this is the first line of recovery for "focus points at a
    /// window that no longer exists".
    ///
    /// Column/floats membership is NOT filtered: if the live state already
    /// contains a phantom id there, `validate` flags it and the reload falls
    /// back to a config-only path (never applying a corrupt topology).
    pub fn snapshot(state: &State) -> Self {
        let mut monitors: Vec<PersistedMonitor> = Vec::with_capacity(state.monitors.len());
        for mon in &state.monitors {
            let mut ws_set: HashSet<WindowId> = HashSet::new();
            let workspaces = mon
                .workspaces
                .iter()
                .map(|ws| {
                    let mut cols: Vec<PersistedColumn> = Vec::with_capacity(ws.columns.len());
                    for c in &ws.columns {
                        ws_set.extend(c.windows.iter().copied());
                        cols.push(PersistedColumn {
                            windows: c.windows.clone(),
                            weight: c.weight,
                            focused: c.focused,
                        });
                    }
                    ws_set.extend(ws.floats.iter().copied());
                    PersistedWorkspace {
                        tag: ws.tag,
                        layout: ws.layout,
                        columns: cols,
                        floats: ws.floats.clone(),
                    }
                })
                .collect();
            let live = |w: WindowId| state.clients.contains_key(&w);
            let focused = mon.focused.filter(|&w| live(w) && ws_set.contains(&w));
            let focus_stack = mon
                .focus_stack
                .iter()
                .copied()
                .filter(|&w| live(w) && ws_set.contains(&w))
                .collect();
            monitors.push(PersistedMonitor {
                workspaces,
                active_ws: mon.active_ws.min(mon.workspaces.len().saturating_sub(1)),
                focused,
                focus_stack,
            });
        }
        Self {
            version: SESSION_SCHEMA_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            monitors,
            sel_mon: state.sel_mon.min(state.monitors.len().saturating_sub(1)),
        }
    }

    /// Validate this snapshot's *internal* invariants without any reference to
    /// the X server. On success returns a `ValidatedSession`; on failure a
    /// stage-`Validate` error naming the exact violation.
    pub fn validate(&self) -> Result<ValidatedSession, SessionError> {
        let fail = |msg: String| SessionError::validate(msg);

        // ── schema ──
        if self.version == 0 || self.version > SESSION_SCHEMA_VERSION {
            return Err(SessionError::schema(format!(
                "unknown session schema version {} (this build only understands ≤{})",
                self.version, SESSION_SCHEMA_VERSION
            )));
        }
        if self.version != SESSION_SCHEMA_VERSION {
            return Err(SessionError::schema(format!(
                "session schema version {} is older than the supported {}; \
                 refusing to auto-migrate (restore the file or start fresh)",
                self.version, SESSION_SCHEMA_VERSION
            )));
        }

        // ── monitors ──
        if self.sel_mon >= self.monitors.len() && !self.monitors.is_empty() {
            return Err(fail(format!(
                "sel_mon {} out of range for {} monitor(s)",
                self.sel_mon,
                self.monitors.len()
            )));
        }
        if self.monitors.is_empty() {
            // An empty session is valid (nothing to restore).
            return Ok(ValidatedSession {
                inner: self.clone(),
            });
        }

        // Global uniqueness: a window id may appear in exactly one workspace
        // slot across the whole session.
        let mut seen: HashSet<WindowId> = HashSet::new();
        let mut dup: Option<WindowId> = None;

        for (mi, mon) in self.monitors.iter().enumerate() {
            if mon.workspaces.is_empty() {
                return Err(fail(format!("monitor {mi} has no workspaces")));
            }
            if mon.active_ws >= mon.workspaces.len() {
                return Err(fail(format!(
                    "monitor {mi} active_ws {} out of range ({} workspace(s))",
                    mon.active_ws,
                    mon.workspaces.len()
                )));
            }
            // Collect the ids on this monitor (for the focus checks).
            let mut mon_set: HashSet<WindowId> = HashSet::new();
            let mut mon_dup: Option<WindowId> = None;
            for (wi, ws) in mon.workspaces.iter().enumerate() {
                // Workspace tag must match its index (a stray tag is a sign of
                // a hand-edited/corrupt file).
                if ws.tag as usize != wi {
                    return Err(fail(format!(
                        "monitor {mi} workspace {wi} carries tag {}",
                        ws.tag
                    )));
                }
                if ws.columns.is_empty() && !ws.floats.is_empty() {
                    // fine — floats-only workspace
                }
                for (ci, col) in ws.columns.iter().enumerate() {
                    if col.windows.is_empty() {
                        return Err(fail(format!(
                            "monitor {mi} workspace {wi} column {ci} is empty"
                        )));
                    }
                    if col.focused >= col.windows.len() {
                        return Err(fail(format!(
                            "monitor {mi} workspace {wi} column {ci} focused {} out of range ({})",
                            col.focused,
                            col.windows.len()
                        )));
                    }
                    let w = col.weight;
                    if !w.is_finite() || w <= 0.0 || w > 1.0001 {
                        return Err(fail(format!(
                            "monitor {mi} workspace {wi} column {ci} weight {w} outside (0,1]"
                        )));
                    }
                    for &id in &col.windows {
                        if !seen.insert(id) && dup.is_none() {
                            dup = Some(id);
                        }
                        if !mon_set.insert(id) && mon_dup.is_none() {
                            mon_dup = Some(id);
                        }
                    }
                }
                for &id in &ws.floats {
                    if !seen.insert(id) && dup.is_none() {
                        dup = Some(id);
                    }
                    if !mon_set.insert(id) && mon_dup.is_none() {
                        mon_dup = Some(id);
                    }
                }
            }
            if let Some(id) = mon_dup {
                return Err(fail(format!(
                    "monitor {mi} contains window {id:#x} more than once"
                )));
            }
            if let Some(w) = mon.focused {
                if !mon_set.contains(&w) {
                    return Err(fail(format!(
                        "monitor {mi} focused window {w:#x} is not in any workspace slot"
                    )));
                }
            }
            for &w in &mon.focus_stack {
                if !mon_set.contains(&w) {
                    return Err(fail(format!(
                        "monitor {mi} focus stack references {w:#x}, not in any workspace slot"
                    )));
                }
            }
            // Focus stack must be a permutation without itself duplicates.
            let mut stack_seen: HashSet<WindowId> = HashSet::new();
            for &w in &mon.focus_stack {
                if !stack_seen.insert(w) {
                    return Err(fail(format!(
                        "monitor {mi} focus stack contains {w:#x} twice"
                    )));
                }
            }
        }
        if let Some(id) = dup {
            return Err(fail(format!(
                "window {id:#x} appears in more than one workspace slot"
            )));
        }
        Ok(ValidatedSession {
            inner: self.clone(),
        })
    }
}

/// Which slot a window is placed in after restore.
#[derive(Debug, Clone, Copy)]
struct Placement {
    mon: usize,
    ws: usize,
    is_float: bool,
}

impl ValidatedSession {
    /// Reconcile the validated session against the *live* X11 window set and
    /// rebuild `state`'s workspace topology on the container the caller is
    /// working on. The caller is expected to work on a clone and swap it in
    /// (see `commit_state`) so a failure here never mutates the visible State.
    ///
    /// Reconciliation policy:
    ///   * windows in the session that are live → restored exactly as persisted;
    ///   * windows in the session that are gone → dropped (no phantom windows);
    ///   * live windows the session never mentioned → re-homed to the
    ///     (monitor, workspace) their `Client` already claims, so nothing
    ///     visible is lost when restoring an older file;
    ///   * unmanaged clients (docks) are not in `state.clients` and therefore
    ///     absent from `live_ids` — they manage themselves.
    ///
    /// Persisted monitor index maps onto the live monitor list by index; extra
    /// persisted monitors (unplugged) are dropped and live monitors without a
    /// persisted twin get empty workspaces.
    ///
    /// Runtime fields are always reset: camera snaps to the focused column's
    /// home, `grid_snapshot`/`pending_focus` are cleared, and
    /// `presented_maximize` is re-derived from the restored focus. No geometry
    /// from the file is ever used.
    pub fn commit(
        &self,
        state: &mut State,
        live_ids: &HashSet<WindowId>,
        n_tags: usize,
        cfg: &Cfg,
    ) -> Result<(), SessionError> {
        let n_tags = n_tags.max(1);
        let persisted = &self.inner;
        let n_mon = state.monitors.len();
        if n_mon != persisted.monitors.len() {
            crate::log::debug!(
                "session restore: {} persisted monitor(s) mapped onto {} live monitor(s)",
                persisted.monitors.len(),
                n_mon
            );
        }

        // ── build the id → (mon, ws, float) placement table ──
        let mut placements: HashMap<WindowId, Placement> = HashMap::new();
        for (mi, pmon) in persisted.monitors.iter().enumerate().take(n_mon) {
            for (wi, pws) in pmon.workspaces.iter().enumerate().take(n_tags) {
                for col in &pws.columns {
                    for &id in &col.windows {
                        if live_ids.contains(&id) {
                            placements.insert(
                                id,
                                Placement {
                                    mon: mi,
                                    ws: wi,
                                    is_float: false,
                                },
                            );
                        }
                    }
                }
                for &id in &pws.floats {
                    if live_ids.contains(&id) {
                        placements.insert(
                            id,
                            Placement {
                                mon: mi,
                                ws: wi,
                                is_float: true,
                            },
                        );
                    }
                }
            }
        }
        // Live windows the session never knew about: keep where their Client
        // already lives, clamping indices to the current geometry.
        let clamp_mon = |m: usize| m.min(n_mon.saturating_sub(1));
        for (&id, c) in state.clients.iter() {
            if !placements.contains_key(&id) {
                placements.insert(
                    id,
                    Placement {
                        mon: clamp_mon(c.monitor),
                        ws: c.workspace.min(n_tags - 1),
                        is_float: c.is_float(),
                    },
                );
            }
        }

        // ── rebuild every monitor's workspace tree from the placements ──
        let mut new_monitors: Vec<Monitor> = Vec::with_capacity(n_mon);
        for mi in 0..n_mon {
            let pmon = persisted.monitors.get(mi);
            let screen = state.monitors[mi].screen;
            let reserved_regions = state.monitors[mi].reserved_regions.clone();
            let reserved = state.monitors[mi].reserved;

            let mut workspaces: Vec<Workspace> = Vec::with_capacity(n_tags);
            for wi in 0..n_tags {
                let mut ws = Workspace::new(wi as u32);
                if let Some(pws) = pmon.and_then(|p| p.workspaces.get(wi)) {
                    ws.layout = pws.layout;
                    // Persisted columns → live-filtered columns.
                    let cols: Vec<Column> = pws
                        .columns
                        .iter()
                        .filter_map(|pc| {
                            let wins: Vec<WindowId> = pc
                                .windows
                                .iter()
                                .copied()
                                .filter(|id| {
                                    live_ids.contains(id)
                                        && placements.get(id).is_some_and(|p| {
                                            p.mon == mi && p.ws == wi && !p.is_float
                                        })
                                })
                                .collect();
                            if wins.is_empty() {
                                return None;
                            }
                            let focused = pc.focused.min(wins.len() - 1);
                            let weight = if pc.weight.is_finite() && pc.weight > 0.0 {
                                pc.weight.min(1.0)
                            } else {
                                0.5
                            };
                            Some(Column {
                                windows: wins,
                                weight,
                                focused,
                                boost: 0.0,
                            })
                        })
                        .collect();
                    ws.columns = cols;
                    // Persisted floats → live-filtered floats (deduped,
                    // excluded ids already tiled).
                    let mut floats: Vec<WindowId> = Vec::new();
                    let mut seen_float: HashSet<WindowId> = HashSet::new();
                    for &id in &pws.floats {
                        if !live_ids.contains(&id) {
                            continue;
                        }
                        let float_here = placements.get(&id).is_some_and(|p| {
                            p.mon == mi && p.ws == wi && p.is_float
                        });
                        if float_here && seen_float.insert(id) {
                            floats.push(id);
                        }
                    }
                    ws.floats = floats;
                    ws.focus.column_idx = ws.columns.len().saturating_sub(1);
                    ws.cleanup_empty_columns();
                }
                workspaces.push(ws);
            }

            // Default-homed windows that fell outside any persisted workspace
            // get tiled/floated onto their recorded (mi, wi).
            for (&id, p) in placements.iter() {
                if p.mon != mi {
                    continue;
                }
                if p.ws >= workspaces.len() {
                    continue;
                }
                let ws = &mut workspaces[p.ws];
                let already = ws.columns.iter().any(|c| c.windows.contains(&id))
                    || ws.floats.contains(&id);
                if already {
                    continue;
                }
                if let Some(c) = state.clients.get(&id) {
                    if c.is_float() || p.is_float {
                        ws.floats.push(id);
                    } else {
                        ws.add_tiled(id, cfg.column_width);
                    }
                }
            }

            // active workspace: persisted when present & in range, else 0.
            let active_ws = pmon
                .map(|p| p.active_ws.min(workspaces.len().saturating_sub(1)))
                .unwrap_or(0);
            // focused / focus stack: persisted, live, and on this monitor.
            let mon_set: HashSet<WindowId> = workspaces
                .iter()
                .flat_map(|ws| {
                    ws.columns
                        .iter()
                        .flat_map(|c| c.windows.iter().copied())
                        .chain(ws.floats.iter().copied())
                })
                .collect();
            let focused = pmon
                .and_then(|p| p.focused)
                .filter(|w| live_ids.contains(w) && mon_set.contains(w));
            let mut focus_stack: Vec<WindowId> = Vec::new();
            if let Some(p) = pmon {
                let mut seen_stack: HashSet<WindowId> = HashSet::new();
                for w in &p.focus_stack {
                    if live_ids.contains(w) && mon_set.contains(w) && seen_stack.insert(*w) {
                        focus_stack.push(*w);
                    }
                }
            }

            let mut mon = Monitor {
                screen,
                workarea: state.monitors[mi].workarea,
                reserved_regions,
                reserved,
                workspaces,
                active_ws,
                focused,
                focus_stack,
                layout_dirty: true,
            };
            mon.recalc_geometry();
            new_monitors.push(mon);
        }

        // ── single commit point: swap the whole monitor list ──
        state.monitors = new_monitors;
        state.sel_mon = persisted.sel_mon.min(state.monitors.len().saturating_sub(1));

        // ── rewrite every client's placement to match the restored topology ──
        // (Does not touch geometry, flags except FLOAT, or presentation state —
        // those stay the live window's own.)
        for (&id, p) in placements.iter() {
            let Some(c) = state.clients.get_mut(&id) else {
                continue;
            };
            if p.mon >= state.monitors.len() || p.ws >= state.monitors[p.mon].workspaces.len() {
                continue;
            }
            c.monitor = p.mon;
            c.workspace = p.ws;
            if p.is_float {
                c.flags.set(WINFLAG_FLOAT);
            } else {
                c.flags.clear(WINFLAG_FLOAT);
            }
        }

        // ── derived state that must stay in lock-step ──
        for mi in 0..state.monitors.len() {
            state.sync_presented_maximize(mi);
        }
        self.snap_cameras(state, cfg);
        Ok(())
    }

    /// Snap every workspace camera to the home scroll of its focused column,
    /// so a restored topology never opens in a scroll position that hides the
    /// focused content (deterministic, independent of the previous session's
    /// spring state).
    fn snap_cameras(&self, state: &mut State, cfg: &Cfg) {
        for mi in 0..state.monitors.len() {
            let wa = state.monitors[mi].workarea;
            let screen = state.monitors[mi].screen;
            let ws_i = state.monitors[mi].active_ws;
            for wi in 0..state.monitors[mi].workspaces.len() {
                let ws = &mut state.monitors[mi].workspaces[wi];
                if ws.layout != LayoutKind::Column {
                    ws.grid_snapshot = None;
                    continue;
                }
                let fs = fs_ctx(&state.clients, ws, screen);
                let home = ideal_scroll(ws, cfg, wa, fs);
                ws.camera.snap(home);
                let focus_i = ws.focus.column_idx;
                for (i, col) in ws.columns.iter_mut().enumerate() {
                    col.boost = if wi == ws_i && i == focus_i { 1.0 } else { 0.0 };
                }
            }
        }
    }

    /// The validated (normalized) inner session, for tests and diagnostics.
    #[cfg(test)]
    pub fn inner(&self) -> &PersistedSession {
        &self.inner
    }
}

/// Run `restore` on a working clone of `state` and, on success, swap it in.
/// This is the atomic-commit contract: `op` must return `Ok(())` only after
/// the topology is fully built, and the swap happens exactly once, so a failed
/// restore can never leave `state` partially reconstructed.
pub fn commit_state<F>(state: &mut State, op: F) -> Result<(), SessionError>
where
    F: FnOnce(&mut State) -> Result<(), SessionError>,
{
    let mut work = state.clone();
    op(&mut work)?;
    std::mem::swap(state, &mut work);
    Ok(())
}

// ─── JSON (de)serialization ──────────────────────────────────────────────────
//
// Hand-rolled, deterministic, zero-dependency. The serializer writes a fixed
// field order so files are cheap to diff; the parser is a small but real JSON
// parser (objects, arrays, strings with escapes, numbers, true/false/null) with
// strict syntax and lenient unknown-key handling for forward compatibility.

/// Minimal JSON value used only to decode session files.
#[derive(Debug, Clone, PartialEq)]
enum Jv {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Jv>),
    Obj(Vec<(String, Jv)>),
}

impl Jv {
    fn get(&self, key: &str) -> Option<&Jv> {
        match self {
            Self::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn as_usize(&self) -> Option<usize> {
        let n = self.as_f64()?;
        if n.fract() != 0.0 || n < 0.0 || n > u32::MAX as f64 {
            return None;
        }
        Some(n as usize)
    }
    fn as_u32(&self) -> Option<u32> {
        let n = self.as_f64()?;
        if n.fract() != 0.0 || n < 0.0 || n > u32::MAX as f64 {
            return None;
        }
        Some(n as u32)
    }
    fn as_i32(&self) -> Option<i32> {
        let n = self.as_f64()?;
        if n.fract() != 0.0 {
            return None;
        }
        let v = n as i32;
        if v as f64 != n {
            return None;
        }
        Some(v)
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn as_arr(&self) -> Option<&[Jv]> {
        match self {
            Self::Arr(v) => Some(v),
            _ => None,
        }
    }
    fn as_obj(&self) -> Option<&[(String, Jv)]> {
        match self {
            Self::Obj(f) => Some(f),
            _ => None,
        }
    }
}

impl PersistedSession {
    /// Serialize to deterministic, versioned JSON.
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(1024);
        s.push('{');
        s.push_str(&format!(
            "\"version\":{},\"app_version\":\"{}\",",
            self.version,
            maverick_sys::json::json_escape(&self.app_version)
        ));
        s.push_str(&format!("\"sel_mon\":{},", self.sel_mon));
        s.push_str("\"monitors\":[");
        for (mi, mon) in self.monitors.iter().enumerate() {
            if mi > 0 {
                s.push(',');
            }
            s.push('{');
            s.push_str(&format!("\"active_ws\":{},", mon.active_ws));
            match mon.focused {
                Some(w) => s.push_str(&format!("\"focused\":{w},")),
                None => s.push_str("\"focused\":null,"),
            }
            s.push_str("\"focus_stack\":[");
            for (i, w) in mon.focus_stack.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!("{w}"));
            }
            s.push_str("],");
            s.push_str("\"workspaces\":[");
            for (wi, ws) in mon.workspaces.iter().enumerate() {
                if wi > 0 {
                    s.push(',');
                }
                s.push('{');
                s.push_str(&format!("\"tag\":{},", ws.tag));
                s.push_str(&format!(
                    "\"layout\":\"{}\",",
                    layout_json_name(ws.layout)
                ));
                s.push_str("\"columns\":[");
                for (ci, col) in ws.columns.iter().enumerate() {
                    if ci > 0 {
                        s.push(',');
                    }
                    s.push('{');
                    s.push_str("\"windows\":[");
                    for (i, w) in col.windows.iter().enumerate() {
                        if i > 0 {
                            s.push(',');
                        }
                        s.push_str(&format!("{w}"));
                    }
                    s.push_str("],");
                    write_float(&mut s, "weight", col.weight);
                    s.push(',');
                    s.push_str(&format!("\"focused\":{}", col.focused));
                    s.push('}');
                }
                s.push_str("],");
                s.push_str("\"floats\":[");
                for (i, w) in ws.floats.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(&format!("{w}"));
                }
                s.push_str("]}");
            }
            s.push_str("]}");
        }
        s.push_str("]}");
        s
    }

    /// Decode a session file. Reports the failure stage:
    ///   Parse  — the file is not a well-formed JSON document (truncated,
    ///             stray bytes, wrong root type);
    ///   Schema — well-formed JSON but `version` is missing/zero/unsupported.
    /// Semantic checks (`validate`) are a separate later stage.
    pub fn parse_json(input: &str) -> Result<PersistedSession, SessionError> {
        let mut p = Parser {
            bytes: input.as_bytes(),
            i: 0,
        };
        let root = p.parse_value().map_err(|e| {
            SessionError::parse(format!("{} at byte {}", e.msg, e.pos.min(input.len())))
        })?;
        p.ws();
        if p.i < p.bytes.len() {
            return Err(SessionError::parse(format!(
                "trailing bytes after JSON document at byte {}",
                p.i
            )));
        }
        let obj = root
            .as_obj()
            .ok_or_else(|| SessionError::parse("session root is not a JSON object"))?;
        let _ = obj;

        let version = root
            .get("version")
            .and_then(Jv::as_f64)
            .ok_or_else(|| SessionError::schema("missing/invalid \"version\""))?;
        if version.fract() != 0.0 || version <= 0.0 {
            return Err(SessionError::schema(format!(
                "non-integer session version {version}"
            )));
        }
        let session = decode_session(&root)?;
        Session::check(Ok(&session))?;
        Ok(session)
    }
}

/// Validate the version before decoding the rest of the body, so an unknown
/// future schema fails with a distinct, actionable message instead of a
/// generic parse error.
struct Session;

impl Session {
    fn check(decoded: &Result<PersistedSession, SessionError>) -> Result<(), SessionError> {
        match decoded {
            Ok(s) if s.version == 0 || s.version > SESSION_SCHEMA_VERSION => Err(
                SessionError::schema(format!(
                    "unknown session schema version {} (this build understands ≤{})",
                    s.version, SESSION_SCHEMA_VERSION
                )),
            ),
            Ok(_) => Ok(()),
            Err(e) => Err(e.clone()),
        }
    }
}

fn decode_session(root: &Jv) -> Result<PersistedSession, SessionError> {
    let version = root
        .get("version")
        .and_then(Jv::as_usize)
        .ok_or_else(|| SessionError::schema("missing \"version\""))?
        as u32;
    let app_version = root
        .get("app_version")
        .and_then(Jv::as_str)
        .unwrap_or("unknown")
        .to_string();
    let sel_mon = root
        .get("sel_mon")
        .and_then(Jv::as_usize)
        .unwrap_or(0);

    let monitors = root
        .get("monitors")
        .and_then(Jv::as_arr)
        .ok_or_else(|| {
            SessionError::parse("missing or non-array \"monitors\"")
        })?;
    let mut out = Vec::with_capacity(monitors.len());
    for (mi, m) in monitors.iter().enumerate() {
        let active_ws = m
            .get("active_ws")
            .and_then(Jv::as_usize)
            .ok_or_else(|| SessionError::parse(format!("monitor {mi}: missing \"active_ws\"")))?;
        let focused = match m.get("focused") {
            Some(Jv::Null) | None => None,
            Some(v) => Some(
                v.as_u32()
                    .ok_or_else(|| SessionError::parse(format!("monitor {mi}: bad \"focused\"")))?,
            ),
        };
        let mut focus_stack = Vec::new();
        if let Some(arr) = m.get("focus_stack").and_then(Jv::as_arr) {
            for (i, v) in arr.iter().enumerate() {
                focus_stack.push(
                    v.as_u32().ok_or_else(|| {
                        SessionError::parse(format!(
                            "monitor {mi}: focus_stack[{i}] is not an integer"
                        ))
                    })?,
                );
            }
        }
        let wss = m.get("workspaces").and_then(Jv::as_arr).ok_or_else(|| {
            SessionError::parse(format!("monitor {mi}: missing \"workspaces\""))
        })?;
        let mut workspaces = Vec::with_capacity(wss.len());
        for (wi, wsv) in wss.iter().enumerate() {
            let tag = wsv
                .get("tag")
                .and_then(Jv::as_u32)
                .ok_or_else(|| SessionError::parse(format!("monitor {mi} ws {wi}: missing tag")))?;
            let layout = match wsv.get("layout").and_then(Jv::as_str) {
                Some("grid") => LayoutKind::Grid,
                Some("column") | None => LayoutKind::Column,
                Some(other) => {
                    return Err(SessionError::parse(format!(
                        "monitor {mi} ws {wi}: unknown layout \"{other}\""
                    )))
                }
            };
            let mut columns = Vec::new();
            if let Some(arr) = wsv.get("columns").and_then(Jv::as_arr) {
                for (ci, c) in arr.iter().enumerate() {
                    let windows = c
                        .get("windows")
                        .and_then(Jv::as_arr)
                        .ok_or_else(|| {
                            SessionError::parse(format!(
                                "monitor {mi} ws {wi} col {ci}: missing \"windows\""
                            ))
                        })?;
                    let windows: Vec<WindowId> = windows
                        .iter()
                        .enumerate()
                        .map(|(ri, v)| {
                            v.as_u32().ok_or_else(|| {
                                SessionError::parse(format!(
                                    "monitor {mi} ws {wi} col {ci} row {ri}: bad window id"
                                ))
                            })
                        })
                        .collect::<Result<_, _>>()?;
                    let weight = c
                        .get("weight")
                        .and_then(Jv::as_f64)
                        .ok_or_else(|| {
                            SessionError::parse(format!(
                                "monitor {mi} ws {wi} col {ci}: missing \"weight\""
                            ))
                        })? as f32;
                    let focused = c
                        .get("focused")
                        .and_then(Jv::as_usize)
                        .ok_or_else(|| {
                            SessionError::parse(format!(
                                "monitor {mi} ws {wi} col {ci}: missing \"focused\""
                            ))
                        })?;
                    columns.push(PersistedColumn {
                        windows,
                        weight,
                        focused,
                    });
                }
            }
            let floats = wsv
                .get("floats")
                .and_then(Jv::as_arr)
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(i, v)| {
                            v.as_u32().ok_or_else(|| {
                                SessionError::parse(format!(
                                    "monitor {mi} ws {wi} floats[{i}]: bad window id"
                                ))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            workspaces.push(PersistedWorkspace {
                tag,
                layout,
                columns,
                floats,
            });
        }
        out.push(PersistedMonitor {
            workspaces,
            active_ws,
            focused,
            focus_stack,
        });
    }
    Ok(PersistedSession {
        version,
        app_version,
        monitors: out,
        sel_mon,
    })
}

fn layout_json_name(l: LayoutKind) -> &'static str {
    match l {
        LayoutKind::Column => "column",
        LayoutKind::Grid => "grid",
    }
}

/// Append `"name":<float>` avoiding a trailing `.0` where possible.
fn write_float(s: &mut String, name: &str, v: f32) {
    s.push_str(&format!("\"{name}\":{v}"));
}

// ─── Minimal JSON parser ─────────────────────────────────────────────────────

struct Parser<'a> {
    bytes: &'a [u8],
    i: usize,
}

struct ParseErr {
    msg: &'static str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while let Some(b) = self.bytes.get(self.i) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<Jv, ParseErr> {
        self.ws();
        let b = *self
            .bytes
            .get(self.i)
            .ok_or(ParseErr { msg: "unexpected end of input", pos: self.i })?;
        match b {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => Ok(Jv::Str(self.parse_string()?)),
            b't' => {
                self.expect_lit("true")?;
                Ok(Jv::Bool(true))
            }
            b'f' => {
                self.expect_lit("false")?;
                Ok(Jv::Bool(false))
            }
            b'n' => {
                self.expect_lit("null")?;
                Ok(Jv::Null)
            }
            b'-' | b'0'..=b'9' => Ok(Jv::Num(self.parse_number()?)),
            _ => Err(ParseErr {
                msg: "unexpected character",
                pos: self.i,
            }),
        }
    }

    fn expect_lit(&mut self, lit: &str) -> Result<(), ParseErr> {
        if self.bytes[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(ParseErr {
                msg: "invalid literal",
                pos: self.i,
            })
        }
    }

    fn parse_object(&mut self) -> Result<Jv, ParseErr> {
        self.i += 1; // consume '{'
        let mut fields = Vec::new();
        self.ws();
        if self.bytes.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Jv::Obj(fields));
        }
        loop {
            self.ws();
            if self.bytes.get(self.i) != Some(&b'"') {
                return Err(ParseErr {
                    msg: "expected object key string",
                    pos: self.i,
                });
            }
            let key = self.parse_string()?;
            self.ws();
            if self.bytes.get(self.i) != Some(&b':') {
                return Err(ParseErr {
                    msg: "expected ':' after object key",
                    pos: self.i,
                });
            }
            self.i += 1;
            let value = self.parse_value()?;
            fields.push((key, value));
            self.ws();
            match self.bytes.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Jv::Obj(fields));
                }
                _ => {
                    return Err(ParseErr {
                        msg: "expected ',' or '}' in object",
                        pos: self.i,
                    })
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<Jv, ParseErr> {
        self.i += 1; // consume '['
        let mut items = Vec::new();
        self.ws();
        if self.bytes.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Jv::Arr(items));
        }
        loop {
            let v = self.parse_value()?;
            items.push(v);
            self.ws();
            match self.bytes.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                    if self.bytes.get(self.i) == Some(&b']') {
                        return Err(ParseErr {
                            msg: "trailing comma in array",
                            pos: self.i,
                        });
                    }
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Jv::Arr(items));
                }
                _ => {
                    return Err(ParseErr {
                        msg: "expected ',' or ']' in array",
                        pos: self.i,
                    })
                }
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, ParseErr> {
        self.i += 1; // consume opening quote
        let mut out = String::new();
        loop {
            let b = *self
                .bytes
                .get(self.i)
                .ok_or(ParseErr { msg: "unterminated string", pos: self.i })?;
            match b {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let esc = *self
                        .bytes
                        .get(self.i)
                        .ok_or(ParseErr { msg: "unterminated escape", pos: self.i })?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if self.i + 4 > self.bytes.len() {
                                return Err(ParseErr {
                                    msg: "truncated \\u escape",
                                    pos: self.i,
                                });
                            }
                            let hex = std::str::from_utf8(&self.bytes[self.i..self.i + 4])
                                .map_err(|_| ParseErr {
                                    msg: "invalid \\u escape",
                                    pos: self.i,
                                })?;
                            let cp = u32::from_str_radix(hex, 16)
                                .map_err(|_| ParseErr {
                                    msg: "invalid \\u escape",
                                    pos: self.i,
                                })?;
                            self.i += 4;
                            // Surrogate pairs: straight code points only —
                            // our own serializer never emits them.
                            if let Some(c) = char::from_u32(cp) {
                                out.push(c);
                            } else {
                                out.push('\u{FFFD}');
                            }
                        }
                        other => {
                            out.push('\\');
                            out.push(other as char);
                        }
                    }
                }
                b if b < 0x20 => {
                    return Err(ParseErr {
                        msg: "control character in string",
                        pos: self.i,
                    })
                }
                _ => {
                    // Decode UTF-8 sequence.
                    let len = utf8_len(b);
                    let end = self.i + len;
                    if end > self.bytes.len() {
                        return Err(ParseErr {
                            msg: "truncated UTF-8 sequence",
                            pos: self.i,
                        });
                    }
                    let s = std::str::from_utf8(&self.bytes[self.i..end]).map_err(|_| {
                        ParseErr {
                            msg: "invalid UTF-8",
                            pos: self.i,
                        }
                    })?;
                    out.push_str(s);
                    self.i = end;
                }
            }
        }
    }

    fn parse_number(&mut self) -> Result<f64, ParseErr> {
        let start = self.i;
        if self.bytes.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        // integer part
        match self.bytes.get(self.i) {
            Some(b'0') => {
                self.i += 1;
            }
            Some(b'1'..=b'9') => {
                while matches!(self.bytes.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            _ => {
                return Err(ParseErr {
                    msg: "invalid number",
                    pos: start,
                })
            }
        }
        if self.bytes.get(self.i) == Some(&b'.') {
            self.i += 1;
            let digits = self.i;
            while matches!(self.bytes.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == digits {
                return Err(ParseErr {
                    msg: "invalid number: no fraction digits",
                    pos: start,
                });
            }
        }
        if matches!(self.bytes.get(self.i), Some(b'e') | Some(b'E')) {
            self.i += 1;
            if matches!(self.bytes.get(self.i), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            let digits = self.i;
            while matches!(self.bytes.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == digits {
                return Err(ParseErr {
                    msg: "invalid number: no exponent digits",
                    pos: start,
                });
            }
        }
        std::str::from_utf8(&self.bytes[start..self.i])
            .map_err(|_| ParseErr {
                msg: "invalid number bytes",
                pos: start,
            })?
            .parse::<f64>()
            .map_err(|_| ParseErr {
                msg: "number out of range",
                pos: start,
            })
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1 // invalid lead byte: consume one; from_utf8 will report it
    }
}

impl FromStr for Jv {
    type Err = SessionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut p = Parser {
            bytes: s.as_bytes(),
            i: 0,
        };
        let v = p
            .parse_value()
            .map_err(|e| SessionError::parse(format!("{} at byte {}", e.msg, e.pos)))?;
        p.ws();
        if p.i < s.len() {
            return Err(SessionError::parse(format!(
                "trailing bytes at byte {}",
                p.i
            )));
        }
        Ok(v)
    }
}

impl From<Rect> for Jv {
    fn from(r: Rect) -> Self {
        Jv::Arr(vec![
            Jv::Num(r.x as f64),
            Jv::Num(r.y as f64),
            Jv::Num(r.w as f64),
            Jv::Num(r.h as f64),
        ])
    }
}

/// Free helper used by the WM: decode + schema-check + validate a session file
/// string, returning the `ValidatedSession` (or a stage-specific error).
pub fn load_and_validate(input: &str) -> Result<ValidatedSession, SessionError> {
    let parsed = PersistedSession::parse_json(input)?;
    parsed.validate()
}

#[cfg(test)]
mod tests;