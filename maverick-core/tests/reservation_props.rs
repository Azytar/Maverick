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
use maverick_core::types::{Edge, Monitor, Rect, ReservedArea, ReservedRegion, State, ViewId};
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

// Views stay a non-empty list, growing and shrinking through the carousel, and
// growing or shrinking keeps the Views that survive *by identity*.
//
// The point of keying on `ViewId` rather than on list position: a shrink must
// drop exactly the Views that went away and re-point nothing. A survivor keeps
// its id and its payload, which is what lets `Client::workspace` stay valid
// across a `n_tags` change.
proptest! {
    #[test]
    fn reconcile_workspaces_bounds_the_views_and_keeps_the_survivors(
        screen in arb_screen(),
        initial in 1usize..=5,
        target in 0usize..=9,
    ) {
        let mut m = Monitor::new(screen, initial);
        // Give every View a distinct payload so a scrambled or dropped View is
        // visible, and record what the surviving ones must keep.
        for (i, ws) in m.workspaces.iter_mut().enumerate() {
            ws.floats.push(0x8000 + i as u32);
        }
        let before: Vec<(ViewId, Vec<u32>)> = m
            .workspaces
            .iter()
            .map(|ws| (ws.id, ws.floats.clone()))
            .collect();
        // Put the carousel on the *last* View, so a shrink that drops it has to
        // repair `current`, and `origin` (still the first View) has to survive.
        let picked = m.workspaces[m.workspaces.len() - 1].id;
        m.goto_view(picked);

        m.reconcile_workspaces(target);

        let expected = target.max(1);
        prop_assert_eq!(m.workspaces.len(), expected, "wrong number of Views");
        // Ids are unique — the property that makes a dangling `Client::workspace`
        // detectable rather than silently re-pointed at an unrelated View.
        let mut ids: Vec<ViewId> = m.workspaces.iter().map(|w| w.id).collect();
        ids.sort_unstable();
        ids.dedup();
        prop_assert_eq!(ids.len(), m.workspaces.len(), "a View id was reused");
        // A survivor is the *same View*: same id, same payload. Growing mints new
        // ids (never reusing a dropped one), so that is checked separately.
        for (i, (id, floats)) in before.iter().enumerate().take(expected.min(before.len())) {
            prop_assert_eq!(m.workspaces[i].id, *id, "View {} was re-identified", i);
            prop_assert_eq!(&m.workspaces[i].floats, floats, "View {} lost its state", i);
        }
        if expected > before.len() {
            let old_max = before.iter().map(|(id, _)| id.get()).max().unwrap_or(0);
            for ws in &m.workspaces[before.len()..] {
                prop_assert!(
                    ws.id.get() > old_max,
                    "a grown View reused an id from before the reconcile"
                );
            }
        }
        // Every pointer names an existing View.
        prop_assert!(
            m.carousel.current().is_some_and(|c| m.view_index(c).is_some()),
            "current {:?} names no existing View", m.carousel.current()
        );
        prop_assert!(
            m.carousel.origin().is_some_and(|o| m.view_index(o).is_some()),
            "origin {:?} names no existing View", m.carousel.origin()
        );
    }
}

// Circular navigation is closed: from any View, `next` and `previous` both
// succeed and `next` then `previous` returns to where it started. A pointer that
// names no View is refused rather than papered over.
//
// The refusal half is the campaign invariant: `next`/`previous`/`goto`/
// `return_to_origin` must never *install* a View that does not exist, so a
// dangling id can never become the one the layout arranges.
proptest! {
    #[test]
    fn circular_navigation_is_closed_and_refuses_a_dead_id(
        screen in arb_screen(),
        tags in 1usize..=6,
        steps in 0usize..=12,
        stale_raw in any::<u32>(),
    ) {
        let mut m = Monitor::new(screen, tags);
        let live: Vec<ViewId> = m.workspaces.iter().map(|w| w.id).collect();

        // A fresh monitor's origin is its first View, so `return_to_origin` has
        // something real to return to and is *expected* to succeed here.
        prop_assert_eq!(m.carousel.origin(), Some(live[0]), "origin is not the first View");

        // A full lap returns to the starting View: `next` is a permutation of the
        // cycle, so `tags` steps are the identity.
        let start = live[0];
        for _ in 0..tags {
            prop_assert!(m.next_view(), "next() refused on a non-empty carousel");
        }
        prop_assert_eq!(m.ws().id, start, "a full lap of next() did not return");

        // Each direction is a true inverse of the other, so a matched pair
        // returns to the starting View whatever the ring size — including a
        // two-View ring, where `next` and `previous` are the same single step.
        for _ in 0..steps {
            prop_assert!(m.previous_view(), "previous() refused");
            prop_assert!(m.next_view(), "next() refused");
            prop_assert_eq!(m.carousel.current(), Some(start), "previous+next did not cancel");
            prop_assert!(m.next_view(), "next() refused");
            prop_assert!(m.previous_view(), "previous() refused");
            prop_assert_eq!(m.carousel.current(), Some(start), "next+previous did not cancel");
        }

        // An id this monitor never minted cannot be navigated to, and the
        // refusal leaves the carousel exactly where it was.
        let max_live = live.iter().map(|v| v.get()).max().unwrap_or(0);
        let stale = ViewId::new(stale_raw.max(max_live) + 1);
        prop_assert!(m.view_index(stale).is_none(), "fixture id is not actually stale");
        prop_assert!(!m.goto_view(stale), "goto() installed a stale id");
        prop_assert_eq!(m.carousel.current(), Some(start), "a refused goto moved current");
        // `origin` is a live View here, so returning is legal and must succeed —
        // and must land on the origin, not on whatever the pointer used to be.
        prop_assert!(m.return_to_origin(), "return_to_origin refused a live origin");
        prop_assert_eq!(
            m.carousel.current(),
            m.carousel.origin(),
            "return_to_origin did not land on the origin"
        );
        prop_assert!(m.view_index(m.carousel.current().unwrap()).is_some());
        prop_assert!(m.view_index(m.carousel.origin().unwrap()).is_some());

        // With the origin *deleted*, it must not still name the dead View.
        // Removing it is the only public route to that state, and `detach` is what
        // re-points it. A single-View monitor would go empty instead, which is the
        // one documented case where both pointers are legitimately `None`.
        let origin = m.carousel.origin().unwrap();
        let origin_pos = m.view_index(origin).unwrap();
        m.remove_view_at(origin_pos);
        if m.workspaces.is_empty() {
            prop_assert!(m.carousel.is_empty(), "an emptied monitor kept a pointer");
            prop_assert_eq!(m.carousel.current(), None);
            prop_assert_eq!(m.carousel.origin(), None);
            prop_assert!(!m.return_to_origin(), "return selected a View in the empty state");
            prop_assert!(!m.next_view(), "next navigated in the empty state");
            prop_assert!(!m.previous_view(), "previous navigated in the empty state");
        } else {
            let repaired = m.carousel.origin().unwrap();
            prop_assert!(m.view_index(repaired).is_some(), "origin dangles after a delete");
            prop_assert_ne!(repaired, origin, "origin still names the deleted View");
            prop_assert!(m.return_to_origin(), "return refused the repaired origin");
            prop_assert_eq!(m.carousel.current(), Some(repaired));
        }
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
