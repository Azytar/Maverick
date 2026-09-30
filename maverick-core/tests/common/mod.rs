//! Shared strategies and state builders for the `maverick-core` property suite.
//!
//! Every state produced by [`build`] is assembled exclusively from the public
//! core API (`Monitor::new`, `Workspace::add_tiled`, `Workspace::drop_into_column`,
//! `State::add_client`, …) so it is a state the window manager itself could have
//! produced. That is what makes `State::check_invariants` an oracle here rather
//! than a restatement of the code under test: a violation can only come from the
//! transition being exercised, never from the fixture being impossible.

#![allow(dead_code)]

use maverick_core::types::{
    Client, Dir, Edge, FullscreenPolicy, Monitor, PendingFocus, Rect, ReservedRegion, State,
    WinFlags, WindowId,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Primitive strategies
// ---------------------------------------------------------------------------

/// A screen coordinate.
///
/// The full `i32` range is kept in the domain because a secondary output can
/// legitimately sit at a near-limit origin, and the saturating edge helpers
/// exist precisely for that; the two bounded branches keep ordinary multi-head
/// layouts and the origin-0 case densely represented.
pub fn arb_coord() -> impl Strategy<Value = i32> {
    prop_oneof![any::<i32>(), -4096i32..=4096, 0i32..=4096]
}

/// A pixel extent. Zero is representable and meaningful (a fully reserved edge
/// collapses the workarea to zero width), so it stays in the domain.
pub fn arb_extent() -> impl Strategy<Value = u32> {
    prop_oneof![any::<u32>(), 0u32..=4096]
}

/// An arbitrary rectangle, including negative origins and degenerate extents.
pub fn arb_rect() -> impl Strategy<Value = Rect> {
    (arb_coord(), arb_coord(), arb_extent(), arb_extent())
        .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

/// A monitor-sized screen rect. Both axes are at least 1px: that is what RandR
/// reports, and a zero-extent screen would make point lookup untestable.
pub fn arb_screen() -> impl Strategy<Value = Rect> {
    (arb_coord(), arb_coord(), 1u32..=8192, 1u32..=8192)
        .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

/// A finite scroll offset. The small branches matter: layout scroll values live
/// near zero, and a domain made only of huge magnitudes would never exercise the
/// sub-pixel settle envelope.
pub fn arb_offset() -> impl Strategy<Value = f32> {
    prop_oneof![-1.0e6f32..=1.0e6, -4096.0f32..=4096.0, -1.0f32..=1.0]
}

/// A float that may be non-finite, for the domains that explicitly document how
/// poisoned input is handled.
pub fn arb_poisoned() -> impl Strategy<Value = f32> {
    prop_oneof![
        any::<f32>(),
        Just(f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
        -1.0e30f32..=1.0e30
    ]
}

/// A guaranteed non-finite float. `any::<f32>()` is useless for this: it yields
/// non-finite values only rarely, so a "the API must reject this" property built
/// on it would exercise the rejection path in a handful of cases out of 256.
pub fn arb_non_finite() -> impl Strategy<Value = f32> {
    prop_oneof![
        Just(f32::NAN),
        Just(-f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY)
    ]
}

/// An edge for a reservation.
pub fn arb_edge() -> impl Strategy<Value = Edge> {
    prop_oneof![
        Just(Edge::Top),
        Just(Edge::Bottom),
        Just(Edge::Left),
        Just(Edge::Right)
    ]
}

/// A trackable reservation: any owner, any edge, any thickness.
///
/// `thickness` is untrusted in the real system (any window may publish any
/// `_NET_WM_STRUT[_PARTIAL]` CARDINALs), so the whole `u32` range plus the
/// realistic dock range are both in the domain.
pub fn arb_region() -> impl Strategy<Value = ReservedRegion> {
    (
        any::<u32>(),
        arb_edge(),
        prop_oneof![0u32..=64, any::<u32>()],
    )
        .prop_map(|(owner, edge, thickness)| ReservedRegion {
            owner,
            edge,
            thickness,
        })
}

/// A set of reservations.
pub fn arb_regions() -> impl Strategy<Value = Vec<ReservedRegion>> {
    proptest::collection::vec(arb_region(), 0..6)
}

/// A movement direction, including the two (`Next`/`Prev`) that the ribbon
/// layout treats as a no-op.
pub fn arb_dir() -> impl Strategy<Value = Dir> {
    prop_oneof![
        Just(Dir::Next),
        Just(Dir::Prev),
        Just(Dir::Left),
        Just(Dir::Right),
        Just(Dir::Up),
        Just(Dir::Down)
    ]
}

// ---------------------------------------------------------------------------
// State construction
// ---------------------------------------------------------------------------

/// Optional features of a built state, as a bitmask. A bitmask keeps the
/// generator's combinator strategy simple and makes "this feature needs two
/// monitors" requirements explicit at the use site.
pub const F_RESERVE: u8 = 1 << 0;
/// A maximized client presented as the workarea overlay on the active workspace.
pub const F_MAXIMIZE: u8 = 1 << 1;
/// A fullscreen client under `FullscreenPolicy::True` (an out-of-ribbon overlay).
pub const F_OVERLAY: u8 = 1 << 2;
/// A deferred focus slot whose owner is a live presented overlay.
pub const F_DEFER: u8 = 1 << 3;
/// A mirrored X11 input focus naming a real client.
pub const F_XFOCUS: u8 = 1 << 4;
/// A `presented_maximize` left behind on a *non-active* workspace by a window
/// that has since moved to another monitor. Legal (invariant 9 only constrains
/// the active workspace) and exactly the stale reference `State::remove_client`
/// has to sweep.
pub const F_LEGACY_MAX: u8 = 1 << 5;
/// A window added to `State::clients` but not yet placed in the tree. The
/// checker documents these as legitimate (mid-manage, test scaffolding).
pub const F_ORPHAN: u8 = 1 << 6;

/// Shape of a state to build.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    /// Number of monitors (at least one).
    pub monitors: usize,
    /// Workspaces per monitor (at least one).
    pub tags: usize,
    /// Tiled windows, added right-scroll through `add_tiled`.
    pub tiled: usize,
    /// Floating windows, appended to `Workspace::floats`.
    pub floats: usize,
    /// Drag-and-drop operations that merge a window into column 0, so columns
    /// with more than one window (and hence real row focus) are generated too.
    pub merges: usize,
    /// Bitmask of the `F_*` features.
    pub features: u8,
}

/// The built state plus the identifiers the tests need to reason about it.
pub struct Built {
    /// The state itself.
    pub state: State,
    /// Every managed window id, in creation order.
    pub wins: Vec<WindowId>,
    /// The subset of [`Self::wins`] that is tiled (present in `columns`).
    pub tiled: Vec<WindowId>,
}

impl Built {
    /// Look a client up, panicking when the generator promised it exists.
    pub fn client(&self, win: WindowId) -> &Client {
        self.state
            .clients
            .get(&win)
            .expect("builder promised this window is managed")
    }
}

/// Move the monitor's logical focus, the way the single focus funnel does: the
/// monitor's focus and the active workspace's focus pointer must always name the
/// same window, because `apply_move_dir` reads the former while the layout and
/// the camera read the latter.
pub fn focus_logically(state: &mut State, mi: usize, win: WindowId) {
    state.monitors[mi].focused = Some(win);
    if let Some((ci, ri)) = state.monitors[mi].ws().index_of_window(win) {
        let ws = state.monitors[mi].ws_mut();
        ws.focus.column_idx = ci;
        ws.columns[ci].focused = ri;
    }
}

/// Build a state from `spec` through the public API only.
pub fn build(spec: &Spec) -> Built {
    let n_mon = spec.monitors.max(1);
    let n_tags = spec.tags.max(1);
    let mut st = State::new();
    for mi in 0..n_mon {
        // A non-overlapping grid, so a point lookup on the composed desktop has
        // exactly one answer and monitor-scoped properties stay unambiguous.
        let (x, y) = ((mi % 2) as i32 * 1920, (mi / 2) as i32 * 1080);
        st.monitors
            .push(Monitor::new(Rect::new(x, y, 1920, 1080), n_tags));
    }
    st.sel_mon = 0;

    let mut next_id = 0x400u32;
    let mut wins: Vec<WindowId> = Vec::new();
    let mut tiled: Vec<WindowId> = Vec::new();

    for k in 0..spec.tiled {
        let win = next_id;
        next_id += 1;
        let mi = k % n_mon;
        let ws_i = (k / n_mon) % n_tags;
        let mut c = Client::new(win, mi, ws_i);
        c.name = format!("tiled-{k}");
        c.class = "prop".into();
        c.geom = Rect::new(64 * (k as i32 % 8), 32, 320, 240);
        st.add_client(c);
        // Rotate the width fraction so the generated ribbon mixes narrow and
        // full-width columns instead of always re-inserting at 1.0.
        let width = [1.0f32, 0.5, 0.35, 0.25, 0.8][k % 5];
        st.monitors[mi].workspaces[ws_i].add_tiled(win, width);
        wins.push(win);
        tiled.push(win);
    }

    for k in 0..spec.floats {
        let win = next_id;
        next_id += 1;
        let mi = (k + 1) % n_mon;
        let ws_i = (k / 2) % n_tags;
        let mut c = Client::new(win, mi, ws_i);
        c.name = format!("float-{k}");
        // A float's geometry is WM-authoritative and may sit off-screen, which
        // is why the checker does not constrain it.
        c.geom = Rect::new(-40 * k as i32, 900, 200 + 10 * k as u32, 150);
        c.flags.set(WinFlags::FLOAT);
        st.add_client(c);
        st.monitors[mi].workspaces[ws_i].floats.push(win);
        wins.push(win);
    }

    // Drag windows into column 0 of their workspace so the generator produces
    // multi-window columns (row focus) as well as single-window ones. The
    // window is unlinked first: `drop_into_column` is a pure insert, matching
    // the backend, which removes the source placement before dropping.
    for k in 0..spec.merges {
        let Some(&win) = tiled.get(k + 1) else { break };
        let (mi, ws_i) = (st.clients[&win].monitor, st.clients[&win].workspace);
        let ws = &mut st.monitors[mi].workspaces[ws_i];
        if ws.columns.len() < 2 {
            break;
        }
        ws.remove_window(win);
        let pos = ws.columns[0].windows.len() % 3;
        ws.drop_into_column(0, win, pos);
    }

    if spec.features & F_ORPHAN != 0 {
        // Mid-manage orphan: known to the core, not yet placed in the tree.
        let win = next_id;
        let mut c = Client::new(win, 0, 0);
        c.name = "orphan".into();
        st.add_client(c);
        wins.push(win);
    }

    for mi in 0..n_mon {
        // Reservations on every edge from independent owners.
        if spec.features & F_RESERVE != 0 {
            for o in 0..2u32 {
                let edge = [Edge::Top, Edge::Left, Edge::Bottom, Edge::Right]
                    [(mi as u32 + o) as usize % 4];
                let thickness = [0u32, 1, 22, 40, 5000, u32::MAX][(o as usize + mi) % 6];
                st.monitors[mi].set_reserved_region(0x9_000 + mi as u32 * 8 + o, edge, thickness);
            }
        }

        // Per-monitor logical focus and MRU stack, drawn from the windows that
        // live on this monitor. The focus lands on the *active* workspace, which
        // is where the real focus path puts it after a workspace switch; a
        // monitor whose active workspace holds nothing stays focusless, as it
        // does after a switch to an empty workspace.
        let own: Vec<WindowId> = wins
            .iter()
            .copied()
            .filter(|&w| st.clients[&w].monitor == mi)
            .collect();
        if !own.is_empty() {
            // The focus funnel always leaves `mon.focused` and the active
            // workspace's focus pointer naming the same window, so the fixture
            // has to as well: a state where they disagree is only ever produced
            // by hand, and `apply_move_dir` reads one while the layout reads the
            // other.
            let active = st.monitors[mi].active_ws;
            let tiled_focus = st.monitors[mi].workspaces[active].focused_win();
            let float_focus = st.monitors[mi].workspaces[active].floats.last().copied();
            if let Some(w) = tiled_focus.or(float_focus) {
                focus_logically(&mut st, mi, w);
            }
            st.monitors[mi].focus_stack = own.iter().rev().copied().take(3).collect();
        }

        // A scrolled camera, so projections that depend on the ribbon offset
        // are not all exercised at 0.0.
        st.monitors[mi].workspaces[0].camera.retarget(120.0);
    }

    // The maximize overlay is presented only on the active workspace of a
    // monitor whose focused window is maximized on some axis.
    let mut maximize_win = None;
    if spec.features & F_MAXIMIZE != 0 {
        if let Some(&win) = tiled.first() {
            let (mi, ws_i) = (st.clients[&win].monitor, st.clients[&win].workspace);
            let c = st.clients.get_mut(&win).expect("tiled window is managed");
            c.flags.set(WinFlags::MAXIMIZED);
            st.monitors[mi].active_ws = ws_i;
            focus_logically(&mut st, mi, win);
            st.sync_presented_maximize(mi);
            maximize_win = Some(win);
        }
    }

    let mut overlay_win = None;
    if spec.features & F_OVERLAY != 0 {
        if let Some(&win) = tiled.get(1).or(tiled.first()) {
            let c = st.clients.get_mut(&win).expect("tiled window is managed");
            c.flags.set(WinFlags::FULLSCREEN);
            c.fullscreen_policy = FullscreenPolicy::True;
            overlay_win = Some(win);
        }
    }

    if spec.features & F_DEFER != 0 {
        // The deferral must be owned by a live presented overlay, so prefer the
        // true-fullscreen overlay and fall back to the maximize owner.
        let owner = overlay_win.or(maximize_win);
        if let Some(owner) = owner {
            let mi = st.clients[&owner].monitor;
            let workspace = st.clients[&owner].workspace;
            let target = tiled.iter().copied().find(|&w| w != owner);
            if let Some(window) = target {
                st.pending_focus = Some(PendingFocus {
                    window,
                    owner,
                    monitor: mi,
                    workspace,
                });
            }
        }
    }

    if spec.features & F_XFOCUS != 0 {
        st.x11_input_focus = tiled.first().copied();
    }

    // Applied after `sync_presented_maximize`, which clears the non-active
    // workspaces of a monitor it touches.
    if spec.features & F_LEGACY_MAX != 0 && n_mon >= 2 && n_tags >= 2 {
        if let Some(win) = maximize_win {
            let mi = st.clients[&win].monitor;
            let ws_i = st.clients[&win].workspace;
            let other = (mi + 1) % n_mon;
            if ws_i != 0 {
                st.monitors[other].workspaces[0].presented_maximize = Some(win);
            } else if st.monitors[other].workspaces.len() > 1 {
                st.monitors[other].workspaces[1].presented_maximize = Some(win);
            }
        }
    }

    Built {
        state: st,
        wins,
        tiled,
    }
}

/// A spec that is valid on its own, plus a random one, for properties that only
/// need "a state the WM could be in".
pub fn arb_spec() -> impl Strategy<Value = Spec> {
    (
        1usize..=3,
        1usize..=4,
        0usize..=6,
        0usize..=3,
        0usize..=3,
        0u8..=0b1111_1111,
    )
        .prop_map(|(monitors, tags, tiled, floats, merges, features)| Spec {
            monitors,
            tags,
            tiled,
            floats,
            merges,
            features,
        })
}
