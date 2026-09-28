//! Properties of `Rect`: the one geometry type every placement, reservation and
//! wallpaper decision is expressed in.
//!
//! The contract under test is the one the type documents: `x`/`y` are signed
//! absolute screen coordinates, `w`/`h` are unsigned so a degenerate size is
//! unrepresentable, point containment is half-open on the right/bottom edges,
//! and every edge computation saturates so a hostile `u32::MAX` extent (a
//! `_NET_WM_STRUT` CARDINAL, a client-reported size) can never wrap the
//! coordinate space and move a window off its own origin.

mod common;

use common::{arb_coord, arb_extent, arb_rect};
use maverick_core::types::Rect;
use proptest::prelude::*;

// A window never has its right edge behind its own origin.
//
// `w` is a `u32`, so `w = u32::MAX` casts to `-1 as i32`; without the clamp and
// the saturating add, a hostile extent would report a right edge one pixel to
// the *left* of `x` and flip every containment test built on it.
proptest! {
    #[test]
    fn edges_never_move_backwards_past_the_origin(
        x in arb_coord(),
        y in arb_coord(),
        w in arb_extent(),
        h in arb_extent(),
    ) {
        let r = Rect::new(x, y, w, h);
        prop_assert!(r.right() >= r.x, "right edge {} < origin {}", r.right(), r.x);
        prop_assert!(r.bottom() >= r.y, "bottom edge {} < origin {}", r.bottom(), r.y);
    }
}

// Point containment is half-open: the right and bottom edges belong to the
// neighbouring rect.
//
// Two adjacent tiled columns share a seam, and if both claimed the seam pixel
// the compositor would either leave it unpainted or paint it twice.
proptest! {
    #[test]
    fn containment_is_half_open_on_the_right_and_bottom_edges(
        r in arb_rect(),
        px in arb_coord(),
        py in arb_coord(),
    ) {
        prop_assert!(!r.contains(r.right(), py), "right edge is inside {:?}", r);
        prop_assert!(!r.contains(px, r.bottom()), "bottom edge is inside {:?}", r);
        // The top-left corner is the one guaranteed member of a non-degenerate
        // rect, and a collapsed one contains nothing at all.
        prop_assert_eq!(r.contains(r.x, r.y), r.w > 0 && r.h > 0, "origin of {:?}", r);
        if r.w == 0 || r.h == 0 {
            prop_assert!(!r.contains(px, py), "collapsed {:?} contains a point", r);
        }
        if r.contains(px, py) {
            prop_assert!(px >= r.x && py >= r.y, "point outside the origin of {:?}", r);
        }
    }
}

// A rect that contains another also contains every point of it.
//
// Occlusion culling and the pointer hit-test must agree: if `a` swallows `b`
// then a click anywhere on `b` belongs to `a`, otherwise a window behind a
// single opaque window would still steal clicks.
proptest! {
    #[test]
    fn rect_containment_agrees_with_point_containment(
        a in arb_rect(),
        b in arb_rect(),
        px in arb_coord(),
        py in arb_coord(),
    ) {
        prop_assert!(a.contains_rect(a), "containment is not reflexive: {:?}", a);
        if a.contains_rect(b) && b.contains(px, py) {
            prop_assert!(a.contains(px, py), "{:?} contains {:?} but not ({}, {})", a, b, px, py);
        }
    }
}

// `union` is the smallest box holding both rects, in either order.
//
// The animation damage path unions the old and the new rect of a sliding
// window: it must cover both, and it must not depend on which one the caller
// happened to have in hand.
proptest! {
    #[test]
    fn union_is_the_commutative_minimal_envelope(a in arb_rect(), b in arb_rect()) {
        let u = a.union(b);
        prop_assert_eq!(u.x, a.x.min(b.x), "the envelope does not start at the leftmost edge");
        prop_assert_eq!(u.y, a.y.min(b.y), "the envelope does not start at the topmost edge");
        prop_assert_eq!(u, b.union(a), "union is order dependent");
        if !envelope_representable(a, b) {
            // Past `i32::MAX` of extent or span, the box a rect describes has
            // no representable `i32` edge, so every edge helper saturates and
            // the type can no longer name the box. No rect the WM builds comes
            // near that (a screen is thousands of pixels across), so the
            // covering claims below are asserted on the representable domain.
            return Ok(());
        }
        prop_assert!(u.contains_rect(a), "{:?} does not cover {:?}", u, a);
        prop_assert!(u.contains_rect(b), "{:?} does not cover {:?}", u, b);
        prop_assert_eq!(u, u.union(u), "union is not idempotent");
        prop_assert!(u.w >= a.w && u.h >= a.h, "{:?} is smaller than {:?}", u, a);
        prop_assert!(u.area() >= a.area() && u.area() >= b.area(), "{:?} covers less area than its inputs", u);
    }
}

// Whether both rects and the box they span have all four edges exactly
// representable as `i32`, i.e. whether `union` can express the envelope without
// saturating any of the edge helpers.
//
// The criterion is the *sum* `x + w`, not the extent alone: an extent wider than
// `i32::MAX` is still exactly representable as an edge whenever the origin is
// far enough left, and the edge helpers are required to report it.
fn envelope_representable(a: Rect, b: Rect) -> bool {
    let edges_exact = |r: Rect| {
        i64::from(r.x) + i64::from(r.w) <= i64::from(i32::MAX)
            && i64::from(r.y) + i64::from(r.h) <= i64::from(i32::MAX)
    };
    if !edges_exact(a) || !edges_exact(b) {
        return false;
    }
    let span_x = i64::from(a.right().max(b.right())) - i64::from(a.x.min(b.x));
    let span_y = i64::from(a.bottom().max(b.bottom())) - i64::from(a.y.min(b.y));
    span_x <= i64::from(i32::MAX) && span_y <= i64::from(i32::MAX)
}

// `area` never wraps, whatever the extent pair.
//
// Areas are compared to pick the dominant output and to budget damage; a
// wrapped product would make a huge window look tiny.
proptest! {
    #[test]
    fn area_is_computed_without_wrapping(w in arb_extent(), h in arb_extent()) {
        let a = Rect::new(0, 0, w, h).area();
        prop_assert_eq!(a, u64::from(w) * u64::from(h));
    }
}

// The right edge is the origin plus the width, for every width the type admits.
//
// `w` is a `u32`, so the sum can leave the `i32` range in either direction, and
// the answer is then the saturated limit — but saturation is a property of the
// *sum*, not of the extent. An extent past `i32::MAX` whose origin is far enough
// left still has an exactly representable right edge, and a helper that narrows
// the extent before adding reports an edge short of the real one: a rect from
// -2e9 that is 3e9 wide would claim to end at +147 483 647 instead of
// +1e9, and every point between the two would be reported as outside a window
// that contains it. Pointer hit-testing, `mon_at`, and occlusion culling all
// read this edge.
proptest! {
    #[test]
    fn containment_covers_the_whole_box_the_fields_describe(
        r in arb_rect(),
        px in any::<i64>(),
        py in any::<i64>(),
    ) {
        let (w, h) = (i64::from(r.w), i64::from(r.h));
        if w == 0 || h == 0 {
            // A collapsed rect has no interior point to ask about.
            return Ok(());
        }
        // Draw a point uniformly from the box `[x, x+w) × [y, y+h)`. Modular
        // reduction keeps the draw uniform without narrowing the domain, which
        // matters: the interesting widths are the ones no `rng.gen_range` over a
        // small range would produce.
        let (qx, qy) = (i64::from(r.x) + px.rem_euclid(w), i64::from(r.y) + py.rem_euclid(h));
        if !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&qx)
            || !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&qy)
        {
            // Past the coordinate limit there is no screen coordinate to test.
            return Ok(());
        }
        prop_assert!(
            r.contains(qx as i32, qy as i32),
            "{:?} does not contain ({qx}, {qy}), a point of its own box",
            r
        );
    }
}

// Rect containment answers exactly the question the two boxes pose, in both
// directions.
//
// Occlusion culling and the workarea clamp compare whole rects: if `a` swallows
// `b` then a click anywhere on `b` belongs to `a`, and a window outside the
// workarea must be rejected. Both directions are read straight off the fields,
// in `i64`, so the property states the semantics rather than restating the
// helper under test.
proptest! {
    #[test]
    fn rect_containment_agrees_with_the_boxes_it_describes(a in arb_rect(), b in arb_rect()) {
        if b.w == 0 || b.h == 0 {
            // A degenerate rect has no area, so "inside" is vacuous for it.
            return Ok(());
        }
        let (bx1, by1) = (i64::from(b.x) + i64::from(b.w), i64::from(b.y) + i64::from(b.h));
        if bx1 > i64::from(i32::MAX) || by1 > i64::from(i32::MAX) {
            // `b`'s own far edges are not representable, so there is no edge
            // for `a` to be compared against.
            return Ok(());
        }
        let inside = i64::from(a.x) <= i64::from(b.x)
            && i64::from(a.y) <= i64::from(b.y)
            && bx1 <= i64::from(a.x) + i64::from(a.w)
            && by1 <= i64::from(a.y) + i64::from(a.h);
        prop_assert_eq!(
            a.contains_rect(b),
            inside,
            "{:?} vs {:?}: the containment answer does not match the boxes",
            a,
            b
        );
    }
}

// The damage envelope covers both rects on the whole coordinate domain, with no
// representability escape hatch.
//
// The animation damage path unions a window's old and new rect, and the damage
// pass coalesces overlapping quads. A union that does not cover one of its
// inputs leaves a stripe of stale pixels exactly where a window just moved, and
// a coalescing pass that tests `a.contains_rect(b)` on such a union drops a
// quad it should have merged. This claims the envelope unconditionally: the
// saturating limit is the same limit both the union's edge and the covered
// rect's edge reach, so containment still holds where the box is not
// representable — it is only the *numeric* extent claims above that need an
// exact edge.
proptest! {
    #[test]
    fn the_union_covers_both_rects_whatever_the_coordinates(a in arb_rect(), b in arb_rect()) {
        let u = a.union(b);
        prop_assert!(u.contains_rect(a), "the envelope {:?} does not cover {:?}", u, a);
        prop_assert!(u.contains_rect(b), "the envelope {:?} does not cover {:?}", u, b);
    }
}
