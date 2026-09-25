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
fn envelope_representable(a: Rect, b: Rect) -> bool {
    let edges_exact = |r: Rect| {
        r.w <= i32::MAX as u32
            && r.h <= i32::MAX as u32
            && i64::from(r.x) + i64::from(r.w) <= i64::from(i32::MAX)
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
