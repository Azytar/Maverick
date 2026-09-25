//! Properties of dock/panel reservations and the geometry derived from them.
//!
//! `ReservedRegion` is the trackable per-dock reservation, `ReservedArea` the
//! collapsed per-edge total, and `Monitor::workarea` the rectangle the layout is
//! allowed to use. The binding contract — stated in the crate docs as "not
//! machine-checked but equally binding" — is that the workarea is *always* the
//! screen minus the collapsed reserved totals, for any reservation any window
//! cares to publish and any screen geometry RandR reports.

mod common;

use common::{arb_coord, arb_edge, arb_region, arb_regions, arb_screen};
use maverick_core::types::{Edge, Monitor, Rect, ReservedArea, ReservedRegion, State};
use proptest::prelude::*;

// Per-edge totals only ever add: a reservation can never be undone by adding
// another one, and the collapse of the same set of regions must not depend on
// the order the docks registered in.
//
// Thickness values are untrusted in the real system (any window may publish
// any `_NET_WM_STRUT[_PARTIAL]` CARDINALs), so the collapse has to saturate
// rather than wrap.
proptest! {
    #[test]
    fn per_edge_totals_only_grow_and_do_not_depend_on_order(
        regions in arb_regions(),
        extra in arb_region(),
    ) {
        let base = ReservedArea::from_regions(&regions);
        let more = ReservedArea::from_regions(&{
            let mut v = regions.clone();
            v.push(extra);
            v
        });
        prop_assert!(more.top >= base.top, "adding a reservation shrank the top total");
        prop_assert!(more.bottom >= base.bottom, "adding a reservation shrank the bottom total");
        prop_assert!(more.left >= base.left, "adding a reservation shrank the left total");
        prop_assert!(more.right >= base.right, "adding a reservation shrank the right total");
        for r in &regions {
            // A total always accounts for at least the whole of any single
            // reservation on that edge, however the rest summed out.
            let one = ReservedArea::from_regions(&[*r]);
            match r.edge {
                Edge::Top => prop_assert!(base.top >= one.top),
                Edge::Bottom => prop_assert!(base.bottom >= one.bottom),
                Edge::Left => prop_assert!(base.left >= one.left),
                Edge::Right => prop_assert!(base.right >= one.right),
            }
        }
        let mut reversed = regions;
        reversed.reverse();
        prop_assert_eq!(base, ReservedArea::from_regions(&reversed), "collapse is order dependent");
    }
}

// A workarea is empty exactly when nothing reserves space, and an empty
// reservation set collapses to the zeroed area.
proptest! {
    #[test]
    fn an_empty_area_means_no_reservation_reserves_anything(regions in arb_regions()) {
        let area = ReservedArea::from_regions(&regions);
        prop_assert_eq!(area.is_empty(), regions.iter().all(|r| r.thickness == 0));
        prop_assert_eq!(ReservedArea::from_regions(&[]), ReservedArea::default());
    }
}

// The workarea is always inside its own screen, and never larger than it.
//
// A strut wider than the screen (a stale dock after a resolution change, or a
// lying one) collapses the workarea onto the screen's edge rather than moving
// the origin outwards, and an origin near the `i32` limit must not wrap.
proptest! {
    #[test]
    fn workarea_never_escapes_its_screen(screen in arb_screen(), regions in arb_regions()) {
        let mut m = Monitor::new(screen, 1);
        m.set_reserved_regions(0xF00D, &edge_thicknesses(&regions));
        prop_assert!(workarea_within(&m), "{:?} is not inside {:?}", m.workarea, m.screen);
    }
}

// More reservation never yields more usable area.
//
// Tiles are laid out inside the workarea, so a lying dock that could make the
// workarea grow would push windows off its own screen instead of merely hiding
// them. Both edges of the axis are compared together: a slide on the far edge
// has to shrink the area just as a dock on the near edge does, which only
// shows up when the two subtractions are exercised as a pair.
proptest! {
    #[test]
    fn more_reservation_never_yields_a_larger_workarea(
        screen in arb_screen(),
        near in arb_edge(),
        near_thin in 0u32..=2000,
        near_thick in 0u32..=u32::MAX,
        far_thin in 0u32..=2000,
        far_thick in 0u32..=u32::MAX,
    ) {
        let (near_thin, near_thick) = if near_thin <= near_thick {
            (near_thin, near_thick)
        } else {
            (near_thick, near_thin)
        };
        let (far_thin, far_thick) = if far_thin <= far_thick {
            (far_thin, far_thick)
        } else {
            (far_thick, far_thin)
        };
        let far = match near {
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
        };
        let mut thin = Monitor::new(screen, 1);
        thin.set_reserved_regions(1, &[(near, near_thin), (far, far_thin)]);
        let mut thick = Monitor::new(screen, 1);
        thick.set_reserved_regions(1, &[(near, near_thick), (far, far_thick)]);

        let vertical = matches!(near, Edge::Top | Edge::Bottom);
        if vertical {
            prop_assert!(thick.workarea.h <= thin.workarea.h, "more reservation grew the height");
            prop_assert!(thick.workarea.y >= thin.workarea.y, "more reservation moved the origin up");
        } else {
            prop_assert!(thick.workarea.w <= thin.workarea.w, "more reservation grew the width");
            prop_assert!(thick.workarea.x >= thin.workarea.x, "more reservation moved the origin left");
        }
    }
}

// `reserved` and `workarea` stay derived from `reserved_regions` through every
// mutation the backend can perform, in any order.
//
// `reserved_regions` is the single source of truth; the collapsed totals and the
// workarea are caches of it, and a cache that drifts would tile windows into a
// dock.
proptest! {
    #[test]
    fn reservation_mutations_keep_the_derived_geometry_in_sync(
        screen in arb_screen(),
        ops in proptest::collection::vec(arb_reservation_op(), 0..8),
    ) {
        let mut m = Monitor::new(screen, 3);
        let pristine = m.workarea;
        for op in &ops {
            match op {
                ReservationOp::Set {
                    owner,
                    edge,
                    thickness,
                } => {
                    let (owner, edge, thickness) = (*owner, *edge, *thickness);
                    m.set_reserved_region(owner, edge, thickness);
                    if thickness == 0 {
                        prop_assert!(
                            !m.reserved_regions.iter().any(|r| r.owner == owner),
                            "a zero thickness is a removal, not a region"
                        );
                    } else {
                        let owned: Vec<&ReservedRegion> =
                            m.reserved_regions.iter().filter(|r| r.owner == owner).collect();
                        prop_assert_eq!(owned.len(), 1, "the per-edge form must replace, not stack");
                    }
                }
                ReservationOp::SetMany { owner, regions } => {
                    m.set_reserved_regions(*owner, &edge_thicknesses(regions));
                    let owned: Vec<&ReservedRegion> = m
                        .reserved_regions
                        .iter()
                        .filter(|r| r.owner == *owner)
                        .collect();
                    let expected = regions.iter().filter(|r| r.thickness > 0).count();
                    prop_assert_eq!(owned.len(), expected, "the owner did not take its whole edge set");
                }
                ReservationOp::Remove { owner } => {
                    let had = m.reserved_regions.iter().any(|r| r.owner == *owner);
                    prop_assert_eq!(m.remove_reserved_region(*owner), had, "removal reported the wrong outcome");
                }
            }
            prop_assert_eq!(
                m.reserved,
                ReservedArea::from_regions(&m.reserved_regions),
                "the collapsed totals drifted from their regions"
            );
            prop_assert!(workarea_within(&m), "{:?} is not inside {:?}", m.workarea, m.screen);
        }
        // Undoing every reservation restores the full screen.
        m.reserved_regions.clear();
        m.recalc_geometry();
        prop_assert_eq!(m.workarea, pristine);
        prop_assert!(m.reserved.is_empty());
    }
}

// Registering and then removing the same owner is a round trip.
//
// Docks appear and disappear constantly (panel reload, monitor hotplug); if the
// removal left the workarea shrunken, every later layout would keep a phantom
// strip of unusable screen.
proptest! {
    #[test]
    fn registering_and_removing_a_dock_restores_the_workarea(
        screen in arb_screen(),
        owner in any::<u32>(),
        regions in proptest::collection::vec((arb_edge(), 1u32..=5000), 1..4),
    ) {
        let mut m = Monitor::new(screen, 2);
        let before = m.workarea;
        m.set_reserved_regions(owner, &regions);
        prop_assert!(workarea_within(&m), "{:?} is not inside {:?}", m.workarea, m.screen);
        prop_assert!(m.remove_reserved_region(owner), "the dock was not registered");
        prop_assert_eq!(m.workarea, before, "the workarea did not recover");
        prop_assert!(!m.remove_reserved_region(owner), "a second removal reported success");
    }
}

// Workspace slots stay a non-empty, in-range, consecutively tagged list, and
// growing or shrinking keeps the slots that survive.
proptest! {
    #[test]
    fn reconcile_workspaces_bounds_the_slots_and_keeps_the_survivors(
        screen in arb_screen(),
        initial in 1usize..=5,
        target in 0usize..=9,
        stale in 0usize..=40,
    ) {
        let mut m = Monitor::new(screen, initial);
        // Give every slot a distinct payload so a scrambled or dropped slot is
        // visible, and record what the surviving ones must keep.
        for (i, ws) in m.workspaces.iter_mut().enumerate() {
            ws.floats.push(0x8000 + i as u32);
        }
        let before: Vec<(u32, Vec<u32>)> = m
            .workspaces
            .iter()
            .map(|ws| (ws.tag, ws.floats.clone()))
            .collect();
        m.active_ws = stale;

        m.reconcile_workspaces(target);

        let expected = target.max(1);
        prop_assert_eq!(m.workspaces.len(), expected, "wrong number of workspace slots");
        prop_assert!(m.active_ws < m.workspaces.len(), "active_ws {} is out of range", m.active_ws);
        for (i, ws) in m.workspaces.iter().enumerate() {
            prop_assert_eq!(ws.tag as usize, i, "slot {} is tagged {}", i, ws.tag);
        }
        for (i, (tag, floats)) in before.iter().enumerate().take(expected.min(before.len())) {
            prop_assert_eq!(m.workspaces[i].tag, *tag, "slot {} was re-tagged", i);
            prop_assert_eq!(&m.workspaces[i].floats, floats, "slot {} lost its state", i);
        }
    }
}

// A stale `active_ws` reads as the last workspace instead of panicking.
//
// Hotplug and session restore can leave the pointer out of range for one frame;
// the documented contract is to clamp for that frame and let the caller repair
// it, never to take the whole window manager down.
proptest! {
    #[test]
    fn a_stale_active_workspace_is_clamped_rather_than_fatal(
        tags in 1usize..=4,
        stale in 0usize..=1000,
    ) {
        let mut m = Monitor::new(Rect::new(0, 0, 1920, 1080), tags);
        m.active_ws = stale;
        let expected = stale.min(tags - 1);
        prop_assert_eq!(m.ws().tag, expected as u32, "ws() did not clamp a stale active_ws");
        prop_assert_eq!(m.ws_mut().tag, expected as u32, "ws_mut() did not clamp a stale active_ws");
        prop_assert_eq!(m.try_ws().is_some(), stale < tags, "try_ws() invented a workspace for a stale index");
        prop_assert_eq!(m.try_ws_mut().is_some(), stale < tags, "try_ws_mut() invented a workspace");
    }
}

// The selected-monitor accessors clamp an out-of-range selection and report
// nothing at all when there is no monitor, instead of panicking.
proptest! {
    #[test]
    fn the_selected_monitor_accessor_clamps_or_reports_nothing(sel in 0usize..=1000) {
        let mut st = State::new();
        prop_assert!(st.mon().is_none(), "an empty state has no monitor");
        prop_assert!(st.mon_mut().is_none(), "an empty state has no monitor");
        for n in 1..=3usize {
            // Grow to `n` monitors, each with a distinct width so the clamped
            // index is observable in the result.
            while st.monitors.len() < n {
                let i = st.monitors.len() as u32;
                st.monitors
                    .push(Monitor::new(Rect::new(0, 0, 800 + 100 * i, 600), 1));
            }
            st.sel_mon = sel;
            let idx = sel.min(n - 1);
            let expected_w = 800 + 100 * idx as u32;
            prop_assert_eq!(st.mon().map(|m| m.screen.w), Some(expected_w), "sel_mon {} with {} monitors", sel, n);
            prop_assert_eq!(st.mon_mut().map(|m| m.screen.w), Some(expected_w), "sel_mon {} with {} monitors", sel, n);
        }
    }
}

// A point resolves to the output whose *screen* it is on, falling back to the
// current selection when it is on none of them.
//
// Hit-testing and the presentation overlay both go through this, so a point
// inside a reserved dock strip still belongs to that output: using the workarea
// here would hand clicks on a panel to whichever monitor happens to be
// selected.
proptest! {
    #[test]
    fn a_point_resolves_to_the_output_under_it(screen in arb_screen(), px in arb_coord(), py in arb_coord()) {
        let mut st = State::new();
        let first = screen;
        st.monitors.push(Monitor::new(first, 1));
        let second = Rect::new(first.right().max(0) + 1, first.y, 1280, 1024);
        st.monitors.push(Monitor::new(second, 1));
        st.sel_mon = 0;

        let expected = [0usize, 1]
            .into_iter()
            .find(|&i| st.monitors[i].screen.contains(px, py));
        prop_assert_eq!(
            st.mon_at(px, py),
            expected.unwrap_or(0),
            "({}, {}) resolved to the wrong output",
            px,
            py
        );

        // A strip the dock owns is still the dock's screen. The selection is
        // moved to the other output first, so answering "no output holds this
        // point" and falling back to the selection is distinguishable from
        // answering with the right one.
        if first.h > 64 && i64::from(first.y) + 64 <= i64::from(i32::MAX) {
            st.monitors[0].set_reserved_region(0x1, Edge::Top, 64);
            let probe = (first.x, first.y + 63);
            prop_assert!(first.contains(probe.0, probe.1), "the probe point is not on the screen");
            prop_assert!(!st.monitors[0].workarea.contains(probe.0, probe.1), "the probe point is not reserved");
            st.sel_mon = 1;
            prop_assert_eq!(st.mon_at(probe.0, probe.1), 0, "a reserved strip is not part of its screen");
        }
    }
}

// --- helpers ---------------------------------------------------------------

/// A reservation mutation the backend can perform on a monitor.
#[derive(Debug, Clone)]
enum ReservationOp {
    Set {
        owner: u32,
        edge: Edge,
        thickness: u32,
    },
    SetMany {
        owner: u32,
        regions: Vec<ReservedRegion>,
    },
    Remove {
        owner: u32,
    },
}

fn arb_reservation_op() -> impl Strategy<Value = ReservationOp> {
    prop_oneof![
        (
            any::<u32>(),
            arb_edge(),
            prop_oneof![0u32..=64, any::<u32>()]
        )
            .prop_map(|(owner, edge, thickness)| ReservationOp::Set {
                owner,
                edge,
                thickness
            }),
        (any::<u32>(), arb_regions())
            .prop_map(|(owner, regions)| ReservationOp::SetMany { owner, regions }),
        (0u32..=3).prop_map(|owner| ReservationOp::Remove { owner }),
    ]
}

/// Flatten a region list into the `(edge, thickness)` form the setters take,
/// keeping the owners distinct so each one is registered on its own.
fn edge_thicknesses(regions: &[ReservedRegion]) -> Vec<(Edge, u32)> {
    regions.iter().map(|r| (r.edge, r.thickness)).collect()
}

/// The standing workarea contract: never larger than the screen, never anchored
/// outside it.
fn workarea_within(m: &Monitor) -> bool {
    let s = m.screen;
    let wa = m.workarea;
    s.contains_rect(wa) && wa.w <= s.w && wa.h <= s.h && wa.x >= s.x && wa.y >= s.y
}
