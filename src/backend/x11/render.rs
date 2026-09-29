//! Geometry projection, stacking, and the X11 apply pipeline.
//!
//! This module turns `State` into X11 `ConfigureWindow` calls. It
//! is the only place the WM writes geometry to X11 — every other
//! path (manage, events, pointer) routes through here.
//!
//! # Pipeline (per monitor, per frame)
//!
//! ```text
//! arrange_full → arrange_full_phase
//!     → layout::arrange (Placements)
//!     → present::present_into (fullscreen/max rewrite)
//!     → DesiredState::from_placements
//!     → reconciler::reconcile (diff vs AppliedState)
//!     → emit_geometry (single X sink)
//!     → stack_overlay (focus order)
//!     → compositor.invalidate
//! ```
//!
//! # Geometry authority
//!
//! `SUBSTRUCTURE_REDIRECT` is held on the root (`input.rs::setup_root`), so for
//! a *viewable* managed window the X server never applies a client's
//! `ConfigureWindow`: it arrives as a `ConfigureRequest` and only this WM can
//! move the window. That single fact decides who owns geometry:
//!
//! - **Tiled / fullscreen** — the WM owns it. A `ConfigureRequest` is answered
//!   with a synthetic `ConfigureNotify` carrying the model rect and X is left
//!   alone (`arrange` is the only writer).
//! - **Floating** — the *client* owns it. `ConfigureRequest` is adopted verbatim
//!   ([`adopt_float_request`], only non-representable values are saned) so the
//!   client's own correction function has a fixed point on its first request.
//!   The WM only ever normalizes rects *it* invents ([`normalize_float_request`]:
//!   initial placement, rules, drag/resize, `ToggleFloat`, `reposition_floats`),
//!   which lands them on the client's own hint grid so the client accepts them
//!   as-is.
//! - **Fullscreen** — `present_into` rewrites the rect to
//!   `mon.screen` (the full presentation overlay).
//! - **Maximized** — `present_into` rewrites the rect to
//!   `maximized_rect` (workarea, per-axis) only while the
//!   window is focused; tiles underneath are still computed.
//!
//! A `ConfigureNotify` is therefore never an *instruction*: it is the echo of
//! one of our own requests (possibly a stale one still in flight). See
//! `reconciler::classify_configure`.
//!
//! # Stacking
//!
//! `stack_overlay` computes the per-monitor focus order:
//! floats → sticky → presented exclusive-fullscreen/maximized
//! (sorted by `focus_stack`) → focused floating peek → transient
//! popups. The `last_stack_order` dedup prevents per-frame raise
//! storms.
//!
//! # Focus
//!
//! `focus` validates the window, handles `NO_FOCUS`, unfocuses
//! the previous window, sets `sel_mon`, calls `set_input_focus`
//! (PARENT if `wants_input` else `POINTER_ROOT`) with
//! `last_event_time`, writes `WM_TAKE_FOCUS`, updates the
//! focus stack *before* `reconcile_focus` (return-to-workspace
//! bug fix), and calls `stack_overlay`.
//!
//! # Safety
//!
//! This module contains `unsafe` blocks for X11 FFI (Shape
//! rectangles are fire-and-forget; no X11 error handling beyond
//! the silent error handler).

use super::*;
use crate::backend::x11::reconciler::{reconcile, wire_geometry, GeometryEffect};
use crate::core::commands::retarget_focus_to_window;
use crate::core::desired::DesiredState;
use crate::core::layout::{clamp_float_geom, normalize_float_geom, Phase};
use crate::core::present::present_into;
use crate::types::StateExt;
use x11rb::protocol::shape;

// `itrace!`/`wtrace!` expand to nothing unless their feature is enabled, so the
// call sites below cost no runtime in normal builds.
#[cfg(feature = "input-trace")]
#[allow(unused_macros)]
macro_rules! itrace {
    ($($arg:tt)*) => {{
        eprintln!("[INPUT-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "input-trace"))]
#[allow(unused_macros)]
macro_rules! itrace {
    ($($arg:tt)*) => {{}};
}

// `wtrace!` mirrors `itrace!` for the desired→applied→real→x11_focus pipeline.
#[cfg(feature = "window-trace")]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{
        eprintln!("[WINDOW-TRACE] {}", format!($($arg)*));
    }};
}
#[cfg(not(feature = "window-trace"))]
#[allow(unused_macros)]
macro_rules! wtrace {
    ($($arg:tt)*) => {{}};
}

/// Approximate a rounded rectangle of size `w`×`h` with corner radius `r` as
/// a list of X11 `Rectangle`s: one full-width middle band, plus one 1px-tall
/// rectangle per row of each rounded corner (inset by the circle's chord at
/// that row). This is the same technique window managers have used for
/// XShape-based rounding for decades — O(r) rectangles, no external deps,
/// no compositor required. `r` is clamped so it can never exceed half of
/// either dimension.
fn rounded_rectangles(w: i32, h: i32, r: i32) -> Vec<Rectangle> {
    let r = r.clamp(0, w.min(h) / 2);
    if r <= 0 || w <= 0 || h <= 0 {
        return vec![Rectangle {
            x: 0,
            y: 0,
            width: w.max(0) as u16,
            height: h.max(0) as u16,
        }];
    }
    // Two bands of at most `r` rectangles each, and a `ShapeRectangles` request
    // that cannot hold them all does not fail — it kills the connection, because
    // the length field wraps to zero and the server reads the remainder as more
    // requests. `corner_radius` reaches here unclamped from the config file, so
    // the bound is the protocol's, not the user's.
    //
    // The arithmetic, which is easy to get wrong: one call emits at most
    // `2r + 1` rectangles (a row per band, plus the middle band), and
    // `rounded_frame_regions` calls this twice — once for the outer frame, once
    // for the inset client region — so a request carries at most `4r + 2`.
    // `corner_radius = 16383` therefore asks for 65534 rectangles, not the 32766
    // the two-band reading suggests. The slack of 2 is the middle bands.
    let r = r.min(((MAX_MASK_RECTS - 2) / 4) as i32);
    if r <= 0 {
        return vec![Rectangle {
            x: 0,
            y: 0,
            width: w.max(0) as u16,
            height: h.max(0) as u16,
        }];
    }

    let mut rects = Vec::with_capacity(2 * r as usize + 1);
    // The middle band only exists when the corner zones leave a gap between
    // them. When `2*r == h` the band degenerates to the circle-center row,
    // which is fully visible (chord == r there), so clamping it to 1px is the
    // true geometry, not padding — and Shape unions tolerate the overlap with
    // the tangent corner row below.
    rects.push(Rectangle {
        x: 0,
        y: r as i16,
        width: w as u16,
        height: (h - 2 * r).max(1) as u16,
    });

    for i in 0..r {
        // Row i (0 = outermost) sits `dy` pixels from the corner circle's
        // vertical center; the circle's horizontal chord at that row gives
        // how far to inset from the edge.
        let dy = r - i;
        let chord = ((r * r - dy * dy).max(0) as f64).sqrt() as i32;
        let inset = (r - chord).clamp(0, w / 2);
        // The span reaches 0 exactly when `w == 2*r` on the tangent row
        // (`i == 0`, `chord == 0`): the circle then touches the frame at a
        // single point, so the row has no area of its own and must not be
        // dropped (it would clip the frame's top edge). The minimum-area floor
        // alone is not enough, because a one-pixel row can only be symmetric
        // about the frame's centre when the centre is a single pixel — and a
        // degenerate span is reachable only for an even `w` (an odd `w` can
        // never satisfy `2 * r == w`), whose centre falls between two columns.
        // Widening the floor to those two centre columns is therefore the
        // smallest mask that keeps the row symmetric about the centre, and
        // `x` is derived from the width so that invariance holds by
        // construction rather than by a special case.
        let span = w - 2 * inset;
        let width = if span == 0 { 2 } else { span } as u16;
        let x = ((w - i32::from(width)) / 2) as i16;
        rects.push(Rectangle {
            x,
            y: i as i16,
            width,
            height: 1,
        });
        rects.push(Rectangle {
            x,
            y: (h - 1 - i) as i16,
            width,
            height: 1,
        });
    }
    rects
}

/// The largest frame a Shape mask can describe exactly.
///
/// Both halves of an `x11rb::Rectangle` are **signed** 16-bit, and the binding
/// constraint is the *right/bottom edge*, not the origin: the bottom corner row
/// sits at `h - 1 - i` and the band's origin at `(w - width) / 2`, and a
/// rectangle whose edge reaches `i16::MAX + 1` has that edge clamped down.
///
/// Measured against Xvfb / X.Org 21.1.24, one rectangle at a known origin:
///
/// ```text
/// origin 0, w 32767 -> extents 32767x1   (exact)
/// origin 0, w 32768 -> extents 32767x1   (edge clamped: 1 px short)
/// origin 0, w 65535 -> extents 32767x1   (edge clamped)
/// origin 32767, w 1 -> 0 rects           (the band empties, silently discarded)
/// ```
///
/// The last row is the reason this is `i16::MAX` and not `i16::MAX + 1`: an
/// earlier version of this constant allowed 32768, which left a 1 px strip of
/// every window's right and bottom edge unmasked — square corners on a frame the
/// user asked to be rounded — and a band at the last representable origin was
/// dropped entirely, with no protocol error either time.
const MAX_MASKED_FRAME: u32 = i16::MAX as u32;

/// The most rectangles one `ShapeRectangles` request can carry.
///
/// A `SHAPE` `Rectangles` request is a 20-byte header plus 8 bytes per rectangle,
/// and the length field counts *words*, which caps a request at 262 140 bytes
/// unless BIG-REQUESTS is negotiated. Maverick does not negotiate it. So
/// `(262_140 - 20) / 8 = 32765` rectangles is the ceiling — measured on Xvfb by
/// bisection: 32765 accepted, the 32766th rejected with `BadLength`.
///
/// Exceeding it is not a rejected request, it is a dead window manager.
/// `x11rb` computes the length field as `u16::try_from(len / 4).unwrap_or(0)`, so
/// an over-long request sends a length of **zero** and the server parses the
/// trailing 256 KB as further requests. The caller is the corner radius, which
/// emits one rectangle per row in each of two bands, and `userconfig` copies
/// `corner_radius` into `Cfg` with no bound — so `corner_radius = 16383` with a
/// frame of 32768 px is a config value that kills the connection.
const MAX_MASK_RECTS: usize = 32_765;

/// Outer region and inset client region, both expressed relative to their own
/// top-left. Translating the outer region by -bw aligns their circle centers.
///
/// # Why the frame is clamped here
///
/// A Shape `BOUNDING` mask is a union of X11 rectangles, so its geometry travels
/// as CARD16/INT16. The frame the caller derives is `geom + 2*bw`, and `bw` is
/// user config: `userconfig` copies `border_width` into `Cfg` without a bound,
/// `SetBorderWidth` overwrites it from IPC, and `manage` copies it into
/// `Client::border_w`. A `border_width` of 40 000 therefore asks for an 80 001 px
/// frame, and narrowing that with `as` is a *modulo*, not a saturation — the
/// server receives a mask a fraction of the frame's size, anchored at a wrapped
/// (possibly negative) origin. Worse, a dimension above `i32::MAX` arrives as a
/// negative `i32`, and `rounded_rectangles` then clamps the radius against a
/// negative bound, which `Ord::clamp` treats as a programming error and aborts on.
///
/// So the frame is clamped to what the protocol can describe *before* it is
/// narrowed. A frame past [`MAX_MASKED_FRAME`] has no exact mask; describing the
/// part that fits is strictly better than wrapping, and matches what the
/// compositor already does for the overlay shape (`update_overlay_shape`).
/// The outer frame a window's border produces: the window plus the border on
/// both sides, in each axis.
///
/// `bw` here is the model's border, which is user config and therefore bounded
/// by nothing this arithmetic can assume — `userconfig` copies `border_width`
/// into `Cfg` without a range check, `SetBorderWidth` overwrites it from IPC, and
/// `manage` copies it into `Client::border_w`. A `border_width` of 2147483648 in
/// the config file reaches here, and plain `2 * bw` overflows `u32` at
/// `bw >= 2^31`, which a debug build traps ("attempt to multiply with overflow")
/// rather than wrapping.
///
/// A frame is a description of pixels, so saturating is the honest failure: one
/// that cannot be represented is clamped rather than wrapped into a frame that
/// describes nothing. It is factored out of `sync_rounded_frame` — which needs a
/// live display — so the arithmetic can be exercised without one; that is the
/// only thing keeping the saturating operators honest.
pub(crate) fn outer_frame(geom: Rect, bw: u32) -> (u32, u32) {
    let frame = bw.saturating_mul(2);
    (geom.w.saturating_add(frame), geom.h.saturating_add(frame))
}

fn rounded_frame_regions(w: u32, h: u32, radius: i32, bw: u32) -> (Vec<Rectangle>, Vec<Rectangle>) {
    let w = w.min(MAX_MASKED_FRAME);
    let h = h.min(MAX_MASKED_FRAME);
    let radius = radius.clamp(0, (w.min(h) / 2) as i32);
    let inset = bw.saturating_mul(2);
    (
        rounded_rectangles(w as i32, h as i32, radius),
        rounded_rectangles(
            w.saturating_sub(inset) as i32,
            h.saturating_sub(inset) as i32,
            radius.saturating_sub(bw.min(i32::MAX as u32) as i32).max(0),
        ),
    )
}

/// Clamp a floating window's geometry so the whole frame (content + the border
/// on both sides) fits inside `wa`.
///
/// Size is clamped *before* position on purpose: with a float wider or taller
/// than the workarea the naive `max_x = wa.x + wa.w - g.w - 2*bw` goes below
/// `wa.x`, so clamping the position alone parks the window at a negative
/// coordinate while its size still overflows the screen. Clamping the size
/// first keeps `min <= max` and guarantees the result is inside `wa`.
pub(crate) fn clamp_float_to_workarea(g: Rect, wa: Rect, bw: u32) -> Rect {
    // Thin adapter over the single authority (`layout::clamp_float_geom`). The
    // name survives because `manage`, `events`, `pointer` and the tests all use
    // it as the X11 sink; the policy lives in one place so arrange, drag and
    // `ConfigureRequest` cannot diverge (a 2*bw jump or a position bounce is
    // what makes floats visibly dance).
    clamp_float_geom(g, wa, bw)
}

/// Full float-request normalization for a rect the **WM** decided (initial
/// placement, rules, drag/resize, `ToggleFloat`, monitor change): hints snap,
/// then workarea clamp, then a final settle onto the increment grid.
///
/// # Authority
///
/// This rewrites the rect it is given, so it must never be applied to a rect the
/// *client* asked for (see [`adopt_float_request`]). It is a fixed point of the
/// toolkit's correction function because the answer lands on the grid the client
/// itself declares: the WM can hand it to the client and the client accepts it
/// as-is (no corrective `ConfigureRequest`, no bigger/smaller bounce).
///
/// The settle matters at the screen edge: the workarea clamp can shrink a
/// grid-aligned size to an off-grid one (e.g. 10px grid clamped to 1916px),
/// which the toolkit would floor and re-request — one bounce per update.
/// Settling floors to the grid upfront (never growing back past the clamped
/// size, so the workarea always wins) makes the answer grid-stable too.
pub(crate) fn normalize_float_request(g: Rect, hints: SizeHints, wa: Rect, bw: u32) -> Rect {
    // Thin adapter over the single authority for WM-owned float geometry
    // (`layout::normalize_float_geom`): snap → frame-aware clamp → settle.
    normalize_float_geom(g, hints, wa, bw)
}

/// Re-decide one float for the workarea its monitor now has: settle the rect
/// with the single WM-owned normalization and release the client's authority
/// seal over it. Returns the rect to apply, or `None` when the float already
/// sits where this workarea wants it and held no seal — nothing to configure.
///
/// The seal (`Client::float_client_authority`) states that the rect was adopted
/// verbatim from a `ConfigureRequest`, so a projection must leave it alone. That
/// claim is scoped to the workarea it was made under: a rect the client chose
/// under the old one carries no authority over the new one, and the rect
/// settled here is the WM's own choice. Releasing the seal is therefore part of
/// reclaiming the workarea, not an extra step — a surviving seal would keep the
/// projection on a rect the WM has already replaced, and a client re-asserting
/// its old geometry would be adopted verbatim again, off the new workarea, with
/// nothing left to pull it back.
///
/// Nothing bounces off the settled rect: `normalize_float_geom` is idempotent,
/// so it already is the fixed point of the client's own hint grid.
///
/// Pure over `(&mut Client, Rect)`: no X11, no `&self`.
fn reclaim_float_to_workarea(c: &mut Client, wa: Rect) -> Option<Rect> {
    let settled = normalize_float_geom(c.geom, c.hints, wa, c.border_w);
    if settled == c.geom && !c.float_client_authority {
        return None;
    }
    c.float_client_authority = false;
    Some(settled)
}

/// Adopt the geometry a **client** asked for (a float's `ConfigureRequest`).
///
/// The client is the authority for its own floating window; the WM only drops
/// what X cannot represent (see
/// [`adopt_client_float_geometry`](crate::core::layout::adopt_client_float_geometry)).
/// The answer is therefore bit-for-bit the request, which is what makes the
/// client↔WM conversation terminate: a toolkit that re-asserts "its" geometry
/// on every `ConfigureNotify` sees exactly that geometry come back and stops.
///
/// Hints, the workarea and the strut-inset workarea are deliberately *not*
/// applied here: a float that wants to overlap a dock or hang off the screen
/// edge is making a choice, not a mistake, and overriding it starts a fight
/// (`ConfigureWindow` ping-pong) that reads on screen as a window that jumps
/// around on its own.
pub(crate) fn adopt_float_request(g: Rect) -> Rect {
    crate::core::layout::adopt_client_float_geometry(g)
}

/// Off-screen parking rect for a window that is not on its monitor's active
/// workspace.
///
/// The single definition of "hidden": `hide_offscreen` parks windows with it and
/// every client-driven geometry sink re-parks with it, so a client that resizes
/// itself while parked can never resurrect its window onto the workspace the
/// user is actually looking at.
/// The park has to survive the X server, not just Maverick.
///
/// `ConfigureWindow`'s x is INT16 and an out-of-range x is *silently truncated*
/// rather than rejected — measured on X.Org 1.24.1, `x = -65735` is stored as
/// `-199`, with no protocol error. A park at `-(w + 200)` therefore only stays
/// left of the origin while `w + 200` fits in INT16, i.e. `w <= 32568`. Past
/// that the negation wraps, and a client that had resized its float to 63 417 px
/// would have its park land at `x = -199` — a window that covers the whole
/// workspace the user is actually looking at, which is the exact outcome the
/// park exists to prevent.
///
/// The leftmost coordinate the server can hold is `i16::MIN`, so no window
/// wider than the remainder of that range can be parked off-screen to the left
/// at all. Such a window is clamped to the widest that can, *for as long as it
/// stays parked*: the size describes hidden pixels, the un-park re-sends the
/// real `Client::geom` (see `hide_offscreen`), and a parked window's own
/// `ConfigureRequest` is answered by re-parking rather than adopted, so nothing
/// the client can observe depends on the parked width.
pub(crate) fn parked_rect(g: Rect) -> Rect {
    // `-(w + MARGIN)` must stay inside INT16, so this is the widest window whose
    // whole frame still fits left of the origin.
    const MARGIN: i32 = 200;
    const MAX_OFFSCREEN_W: u32 = (-(i16::MIN as i32) - MARGIN) as u32;

    let w = g.w.min(MAX_OFFSCREEN_W);
    let off_x = w as i32 + MARGIN;
    Rect::new(-off_x, g.y, w, g.h)
}

/// How many `transient_parent` links a single ownership question may follow.
///
/// `WM_TRANSIENT_FOR` is client-controlled and completely unvalidated by the X
/// server: a buggy (or hostile) client can point a window at itself, or two
/// windows at each other, producing a *cycle* in the ownership graph. The walk
/// below therefore has to be bounded — an unbounded one would hang the WM's
/// stacking pass forever. 4 is the depth real toolkits need (window → dialog →
/// sub-dialog → menu/tooltip); beyond that the chain is not a legitimate
/// popup-of-popup relation, and the cost of the check (O(depth) per float, per
/// restack) stays constant.
///
/// Exceeding the limit is *fail-safe*, never fail-open: the deep window is
/// simply not recognised as owned by the overlay, so it is not raised above it.
/// It keeps its own client entry, focus, geometry and workspace — nothing is
/// orphaned and no reference is dropped by the bound itself.
pub(super) const MAX_TRANSIENT_DEPTH: usize = 4;

/// True when `win`'s `transient_parent` chain reaches any window in `roots`,
/// following at most [`MAX_TRANSIENT_DEPTH`] links.
///
/// Pure over the client map (no X11, no `&self`) so the depth/cycle behaviour is
/// unit-testable. A link that names a window which is no longer a client ends
/// the walk (returns false) instead of panicking — destroyed parents are always
/// treated as "no parent".
pub(super) fn transient_chain_reaches(
    clients: &std::collections::HashMap<WindowId, Client>,
    win: WindowId,
    roots: &[WindowId],
) -> bool {
    let mut cur = clients.get(&win).and_then(|c| c.transient_parent);
    for _ in 0..MAX_TRANSIENT_DEPTH {
        let Some(p) = cur else {
            return false;
        };
        if roots.contains(&p) {
            return true;
        }
        cur = clients.get(&p).and_then(|c| c.transient_parent);
    }
    false
}

impl WindowManager {
    /// Apply (or clear) rounded corners on `win` via the Shape extension's
    /// bounding and client-clip masks. With radius zero, restore both default
    /// regions so fullscreen content is not limited by a stale inner clip.
    /// Windows that never opt in do not issue Shape requests. `radius` is
    /// the *effective* radius for this call — callers pass `0` to force a
    /// square mask (e.g. fullscreen, which must stay edge-to-edge like niri:
    /// rounding an overlay that touches the screen border just clips the
    /// content under a curved corner instead of producing a real rounded
    /// look, since there's no desktop showing behind it to round into).
    ///
    /// `bw` is the border width already included in `outer_w`/`outer_h`: the
    /// mask must be anchored at the window's *outer* top-left corner, and a
    /// window's local coordinate space starts at the inner edge of its border
    /// (the border band lives at [-bw, 0) × [-bw, 0) around the origin). A
    /// mask placed at (0, 0) sized w+2bw × h+2bw therefore clips the whole
    /// top/left border band and shifts both corner arcs bw px into the
    /// content. Offsetting the mask by -bw is not a magic constant: it is
    /// exactly the border width the outer size includes.
    pub(super) fn round_corners(
        &self,
        win: Window,
        outer_w: u32,
        outer_h: u32,
        radius: i32,
        bw: u32,
    ) {
        if radius <= 0 {
            for kind in [shape::SK::CLIP, shape::SK::BOUNDING] {
                let _ = shape::mask(&self.conn, shape::SO::SET, kind, win, 0, 0, x11rb::NONE);
            }
            return;
        }
        let (rects, inner) = rounded_frame_regions(outer_w, outer_h, radius, bw);
        // The X server paints the border as BOUNDING minus CLIP. Leaving CLIP
        // rectangular consumes the curved band inside the client rectangle.
        // Insetting the outer frame by bw keeps the circle centers fixed:
        // the inner radius is max(R - bw, 0), not R or an arbitrary offset.
        let _ = shape::rectangles(
            &self.conn,
            shape::SO::SET,
            shape::SK::CLIP,
            ClipOrdering::UNSORTED,
            win,
            0,
            0,
            &inner,
        );
        // Fire-and-forget, same rationale as apply_geom's configure_window:
        // this runs on every geometry change, a synchronous round-trip per
        // window would be unacceptable. Servers without the Shape extension
        // (essentially none — it's been near-universal since the 90s) just
        // silently ignore the request.
        let offset = -(bw.min(i16::MAX as u32) as i16);
        let _ = shape::rectangles(
            &self.conn,
            shape::SO::SET,
            shape::SK::BOUNDING,
            ClipOrdering::UNSORTED,
            win,
            offset,
            offset,
            &rects,
        );
    }

    pub(super) fn arrange(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full(mon_idx, true)
    }

    /// Reposition floating windows to stay within the workarea after a monitor
    /// geometry change. Clamps each float's size *and* position so its whole
    /// frame remains inside the new workarea — including floats that are larger
    /// than the workarea itself (see `clamp_float_to_workarea`).
    ///
    /// Every float it visits is re-decided by the WM, so on return none of
    /// them — the monitor's workspace floats and its sticky floats — still
    /// holds a `float_client_authority` seal: the projection that follows
    /// (`arrange`) treats each as the WM-owned rect it just settled, and a
    /// `ConfigureRequest` arriving later is the client's answer to *that* rect.
    pub(super) fn reposition_floats(
        &mut self,
        mon_idx: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() {
            return Ok(());
        }
        let wa = self.engine.state.monitors[mon_idx].workarea;

        // Collect all floats that need repositioning to avoid borrow conflicts
        let mut to_reposition: Vec<(WindowId, Rect, u32)> = Vec::new();

        // Regular floats in workspaces. Reuse the single normalization with
        // hints (`reclaim_float_to_workarea`): after RandR the arrange already
        // projected with the same function, so only what actually changed is
        // repositioned and the float does not jump twice (here and then in
        // `arrange`).
        for ws in &self.engine.state.monitors[mon_idx].workspaces {
            for &win in &ws.floats {
                if let Some(client) = self.engine.state.clients.get_mut(&win) {
                    if let Some(g) = reclaim_float_to_workarea(client, wa) {
                        to_reposition.push((win, g, client.border_w));
                    }
                }
            }
        }

        // Sticky floats that belong to this monitor, collected up front: the
        // settle needs `&mut Client` and cannot run while the map is walked.
        let sticky: Vec<WindowId> = self
            .engine
            .state
            .clients
            .iter()
            .filter(|(_, c)| c.monitor == mon_idx && c.is_sticky() && c.is_float())
            .map(|(&win, _)| win)
            .collect();
        for win in sticky {
            if let Some(client) = self.engine.state.clients.get_mut(&win) {
                if let Some(g) = reclaim_float_to_workarea(client, wa) {
                    to_reposition.push((win, g, client.border_w));
                }
            }
        }

        for (win, g, bw) in to_reposition {
            self.apply_geom(win, g, bw, true)?;
        }

        Ok(())
    }

    /// Arrange with optional `hide_offscreen`. Stacking is always refreshed
    /// (cheap, and done inside here) so a separate restack step is unnecessary.
    pub(super) fn arrange_full(
        &mut self,
        mon_idx: usize,
        do_hide: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.arrange_full_phase(mon_idx, do_hide, Phase::Settled)
    }

    /// `arrange_full` with an explicit projection phase. `Settled` (the
    /// compositor path, one-shot) projects to the camera's rest `target`;
    /// `Live` (the X11-only animation path, per-frame) projects to the live
    /// `position` so windows ease smoothly.
    pub(super) fn arrange_full_phase(
        &mut self,
        mon_idx: usize,
        do_hide: bool,
        phase: Phase,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if mon_idx >= self.engine.state.monitors.len() {
            return Ok(());
        }

        if do_hide && self.drag.is_none() {
            self.hide_offscreen(mon_idx)?;
        }

        // `arrange` fills the pre-allocated `desired`/`ribbon_scratch` buffers.
        arrange(
            &self.engine.state,
            mon_idx,
            &self.engine.cfg,
            &self.layout_registry,
            phase,
            &mut self.desired,
            &mut self.ribbon_scratch,
        );

        // The base grid geometry now lives in `self.desired`.
        // Presentation layer: apply the fullscreen/maximized overlay in place.
        present_into(
            &self.engine.state,
            &self.engine.state.monitors[mon_idx],
            &mut self.desired,
            &mut self.present_scratch,
        );
        // Collect into a local so the immutable borrow of `desired`
        // ends before `apply_geom` mutates `self`.
        let desired = DesiredState::from_placements(&self.desired, &self.present_scratch);
        let effects = reconcile(&desired, &self.engine.state, &mut self.applied);
        #[cfg(feature = "window-trace")]
        let effect_count = effects.len();
        // Observability only: mirror the per-window *desired* rect. Never read
        // back for layout, focus or overlay decisions.
        for dw in &desired.windows {
            if let Some(c) = self.engine.state.clients.get_mut(&dw.window) {
                c.last_desired = Some(dw.rect);
            }
        }
        for e in effects {
            let GeometryEffect::Configure { win, rect, border } = e;
            self.emit_geometry(win, rect, border, true)?;
        }
        #[cfg(feature = "window-trace")]
        wtrace!(
            "arrange mon={} desired={} effects={} x11_focus={:?} sel_mon={}",
            mon_idx,
            desired.windows.len(),
            effect_count,
            self.engine.state.x11_input_focus,
            self.engine.state.sel_mon
        );
        self.desired.clear();
        // Overlay stacking: presented windows above tiles, popups of presented
        // windows above the overlay, focused window on top (or peek).
        self.stack_overlay(mon_idx);
        self.engine.state.monitors[mon_idx].layout_dirty = true;
        Ok(())
    }

    pub(super) fn hide_offscreen(
        &mut self,
        mon_idx: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = &mon.workspaces[mon.active_ws];

        // Pre-allocated scratch sets, cleared in place.
        self.hide_ws_set.clear();
        self.hide_mon_vec.clear();
        self.hide_ws_set.extend(
            ws.columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .chain(ws.floats.iter().copied()),
        );
        // A fullscreen column stays visible (it scrolls with the camera as a
        // normal ribbon participant), so it must remain *in* `hide_ws_set` — it
        // is not physically hidden here. Dropping it instead sent the window
        // through the hide branch (off-screen + `wm_hidden = true`); `arrange`
        // then re-showed it physically but left `wm_hidden` stale, and that stale
        // flag later blocked `hide_offscreen` from ever hiding the window on a
        // workspace switch — a fullscreen tile kept covering the next workspace.
        // Keeping it in the set avoids the stale flag entirely.
        //
        // Sticky floats stay visible on every workspace of this monitor.
        self.hide_ws_set.extend(
            self.engine
                .state
                .clients
                .iter()
                .filter(|(_, c)| c.monitor == mon_idx && c.is_sticky() && c.is_float())
                .map(|(w, _)| *w),
        );
        self.hide_mon_vec.extend(
            self.engine
                .state
                .clients
                .iter()
                .filter(|(_, c)| c.monitor == mon_idx)
                .map(|(w, _)| *w),
        );

        // Take the buffer so the loop can borrow `state.clients` mutably at the
        // same time, then hand it back. The four sibling scratch buffers in
        // `compositor_gl` use the same take-and-restore, and the reason is the
        // point: this one is reserved with capacity 64 at construction so the
        // per-frame path never reallocates, and dropping it here threw that
        // capacity away once a frame, so the `extend` above had to grow a fresh
        // `Vec` every frame instead.
        let mut hide_wins: Vec<u32> = std::mem::take(&mut self.hide_mon_vec);
        for win in hide_wins.drain(..) {
            let in_ws = self.hide_ws_set.contains(&win);
            let client = match self.engine.state.clients.get_mut(&win) {
                Some(c) => c,
                None => continue,
            };
            if !in_ws && !client.wm_hidden {
                // Route the off-screen move through the single geometry sink
                // (`parked_rect` is the one definition of the parking spot).
                // The logical `client.geom` is intentionally left untouched
                // (`write_client_geom = false`) — only `AppliedState` and the
                // X11 configure are updated.
                let off_rect = parked_rect(client.geom);
                let bw = client.border_w;
                self.apply_geom(win, off_rect, bw, false)?;
                if let Some(c) = self.engine.state.clients.get_mut(&win) {
                    c.wm_hidden = true;
                }
            } else if in_ws && client.wm_hidden {
                let gx = client.geom.x;
                let gy = client.geom.y;
                // Route the re-show through the single geometry sink. The logical
                // `client.geom` is left untouched (`write_client_geom = false`).
                let real_rect = Rect::new(gx, gy, client.geom.w, client.geom.h);
                let bw = client.border_w;
                self.apply_geom(win, real_rect, bw, false)?;
                if let Some(c) = self.engine.state.clients.get_mut(&win) {
                    c.wm_hidden = false;
                }
            }
        }
        // Hand the buffer back with its capacity intact: this runs every frame,
        // and the alternative was a fresh allocation on the next one.
        self.hide_mon_vec = hide_wins;
        Ok(())
    }

    /// Unify stacking for a monitor's active workspace:
    ///
    /// 1. floats above tiled windows (base float layer);
    /// 2. the presentation overlay — in `Grid`, every fullscreen window; in any
    ///    layout a `FullscreenPolicy::True` fullscreen window (games: exclusive,
    ///    outside the ribbon) and a maximized window while focused —
    ///    most-recently-focused last → on top. In the `Column` layout an
    ///    ordinary fullscreen window is NOT an overlay (it scrolls with the
    ///    ribbon), so it is excluded here;
    ///
    /// "fullscreen covering" (case 2-bis): when the focused window of a `Column`
    ///    workspace is fullscreen and we are not in Overview, the fullscreen tile
    ///    is raised above *everything* (including the dock/bar). The moment any of
    ///    those conditions breaks it drops back to a normal tile and the dock is
    ///    re-raised so the bar returns on top (only on that transition, so floats
    ///    — which ride above the dock in the base layer — are never pushed below
    ///    it);
    /// 3. the focused window if it is a *floating* dialog/popup ("peek"): it
    ///    rises above the presented window so a focused popup stays visible.
    ///    A focused *tiled* window never peeks: h/l still moves focus freely
    ///    underneath a fullscreen window exactly like it should;
    /// 4. floating popups/dialogs whose `WM_TRANSIENT_FOR` chain reaches a
    ///    presented window — always above that overlay.
    ///
    /// Fire-and-forget `StackMode::ABOVE` (or `BELOW` for the covering→off
    /// transition): arrange/focus paths must not block.
    ///
    /// To avoid a `raise()` storm during the camera animation (arrange runs on
    /// every monitor every frame), the desired top-to-bottom order is computed
    /// into `order` and compared with the cached `last_stack_order[mon_idx]`;
    /// `raise` is only re-issued when the order actually changed.
    fn stack_overlay(&mut self, mon_idx: usize) {
        let mon = &self.engine.state.monitors[mon_idx];
        let ws = mon.ws();

        // Include the tiled base in the canonical order: mapping a new tile
        // changes the real X stack even when the overlay list is unchanged.
        // Omitting it would let the cache skip reconciliation and leave the
        // newcomer above an exclusive fullscreen owner.
        let mut order: Vec<WindowId> = ws
            .columns
            .iter()
            .flat_map(|col| col.windows.iter().copied())
            .filter(|win| self.engine.state.clients.contains_key(win))
            .collect();

        // 1. Base float layer.
        for &win in &ws.floats {
            if self.engine.state.clients.contains_key(&win) {
                order.push(win);
            }
        }
        // Sticky floats ride above every workspace's tiles by definition —
        // include them in the base layer regardless of which workspace is
        // active.
        for (&win, c) in &self.engine.state.clients {
            if c.monitor == mon_idx && c.is_sticky() && c.is_float() {
                order.push(win);
            }
        }

        // 2. Presentation overlay — only `FullscreenPolicy::True` exclusive
        // fullscreen (in any layout, already excluded from `fs_ctx`) and focused
        // maximized count. A normal fullscreen window is a ribbon participant.
        // Docks/bar of this monitor sit BETWEEN the float layer and the presented
        // overlays. They must live inside the canonical order — not stacked
        // opportunistically by map order or by one-off re-raises — or a bar that
        // mapped after a `FullscreenPolicy::True` fullscreen (or was re-raised by
        // a covering transition) stays painted ON TOP of it forever: the order
        // cache then sees an unchanged `order` and never re-raises the overlay
        // above it. With docks in the order, any dock map/unmap changes `order`,
        // the full sequence re-raises, and the invariant
        // "tiles < floats < docks < overlays" is re-asserted in one deterministic
        // pass. Sorted for hash-iteration stability.
        let mut dock_wins: Vec<Window> = self
            .docks
            .iter()
            .filter(|&(_, &dm)| dm == mon_idx)
            .map(|(&d, _)| d)
            .collect();
        dock_wins.sort_unstable();
        order.extend(dock_wins);

        // A covering ribbon fullscreen belongs in the cached order too.
        // Otherwise the next arrange replays the dock above it after the
        // one-off covering raise, even though focus has not changed.
        order.extend(self.engine.state.covering_fullscreen_window(mon_idx));

        let mut presented: Vec<WindowId> = ws
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .chain(ws.floats.iter().copied())
            .filter(|win| {
                self.engine.state.clients.get(win).is_some_and(|c| {
                    (c.is_fullscreen() && c.is_true_fullscreen())
                        || ws.presented_maximize == Some(*win)
                })
            })
            .collect();
        presented.sort_by_key(|win| mon.focus_stack.iter().position(|&x| x == *win).unwrap_or(0));
        order.extend(presented.iter().copied());

        // 3. Peek: a focused *floating* window (dialog/popup) is raised above
        //    the overlay so the user sees where focus sits. Deliberately
        //    excludes tiled columns: h/l still moves focus underneath a
        //    fullscreen window exactly like it should (never blocked), but a
        //    plain tile must not visually climb above a fullscreen window just
        //    because it now has focus.
        if !presented.is_empty() {
            if let Some(fw) = mon.focused {
                if !presented.contains(&fw)
                    && self
                        .engine
                        .state
                        .clients
                        .get(&fw)
                        .is_some_and(Client::is_float)
                {
                    order.push(fw);
                }
            }
        }

        // 4. Owned popups of the overlay: a float whose transient-parent chain
        //    reaches a presented window must sit above it.
        for &win in &ws.floats {
            if self.transient_of(win, &presented) {
                order.push(win);
            }
        }

        if self
            .last_stack_order
            .get(&mon_idx)
            .is_none_or(|prev| *prev != order)
        {
            for &win in &order {
                self.raise(win);
            }
            self.last_stack_order.insert(mon_idx, order);
        }

        // 2-bis. Fullscreen covering. The focused fullscreen tile of a `Column`
        // workspace occupies `mon.screen` (not the workarea) and must stay above
        // the dock for as long as it is focused — without waiting for the camera
        // to settle, or the bar stays visible through the whole entry/exit
        // animation and "fullscreen covers the screen and is on top" is a lie.
        // `covering_fullscreen_window` already filters `layout != Column` and
        // Overview, and the `prev_cover != new_cover` test below keeps this out
        // of the per-frame raise path.
        let cover_win = self.engine.state.covering_fullscreen_window(mon_idx);
        let covering = cover_win.is_some();

        // `cover_win` is the *focused* fullscreen window (or `None`); exactly one
        // fullscreen column is raised above the dock at a time — the one you are
        // looking at. Track the transition so moving focus between fullscreen
        // columns drops the old one and raises the new one.
        let new_cover = if covering { cover_win } else { None };
        let prev_cover = self.fs_covering.get(&mon_idx).copied().flatten();
        if prev_cover != new_cover {
            // Drop the previously-covering window to the bottom so the neighbour
            // tile and the bar paint over it (only on this transition).
            if let Some(w) = prev_cover {
                if new_cover != Some(w) {
                    let _ = self.conn.configure_window(
                        w,
                        &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
                    );
                    for (&dock, &dock_mon) in &self.docks {
                        if dock_mon == mon_idx {
                            self.raise(dock);
                        }
                    }
                }
            }
            // Raise the newly-covering window above the dock and every tile.
            if let Some(w) = new_cover {
                self.raise(w);
            }
        }
        self.fs_covering.insert(mon_idx, new_cover);
    }

    /// True when `win`'s `transient_parent` chain reaches any window in `roots`.
    /// Thin wrapper over the pure [`transient_chain_reaches`] walk (which owns
    /// the `MAX_TRANSIENT_DEPTH` bound), so the stacking code can keep its
    /// method-call shape.
    pub(super) fn transient_of(&self, win: WindowId, roots: &[WindowId]) -> bool {
        transient_chain_reaches(&self.engine.state.clients, win, roots)
    }

    /// Non-diffing X11 geometry emitter. Emits EXACTLY the rect/border it is given.
    /// The Reconciler (`reconcile`) is the sole decider that updates `AppliedState`;
    /// this method only writes to X11 (and, for the normal path, mirrors the desired
    /// into `client.geom`). It must NOT re-diff — that would drop the effect.
    fn emit_geometry(
        &mut self,
        win: Window,
        geom: Rect,
        bw: u32,
        write_client_geom: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match self.engine.state.clients.get(&win) {
            Some(c) => c,
            None => return Ok(()),
        };
        // Monitor whose live projection is invalidated by this geometry write.
        let mon = client.monitor;
        // Captured before the mutable borrow below flips geom/border_w.
        let is_fullscreen = client.is_fullscreen();

        // The wire geometry is defined once, in the reconciler that records it,
        // so the record and the request cannot disagree: a `ConfigureWindow`
        // origin is INT16 and its extent is CARD16, so a request can ask for more
        // than the protocol can express — a 0×0 configure is `BadValue` and the
        // server drops it silently, leaving Applied ahead of Real forever, and
        // anything past CARD16 is not describable at all. `geom`/`bw` arrive
        // already clamped from `AppliedState::diff`; re-deriving them here with
        // the same function is an idempotent backstop for the callers that reach
        // this sink directly.
        let (wire, wire_bw) = wire_geometry(geom, bw);
        let wire_w = wire.w;
        let wire_h = wire.h;
        let _ = self.conn.configure_window(
            win,
            &ConfigureWindowAux::new()
                .x(wire.x)
                .y(wire.y)
                .width(wire_w)
                .height(wire_h)
                .border_width(wire_bw),
        );

        super::trace::trace!(
            "geometry_applied",
            "win={win} x={} y={} w={wire_w} h={wire_h} bw={wire_bw} server_confirmed=false",
            geom.x,
            geom.y
        );
        let event = ConfigureNotifyEvent {
            response_type: CONFIGURE_NOTIFY_EVENT,
            sequence: 0,
            event: win,
            window: win,
            above_sibling: x11rb::NONE,
            x: geom.x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            y: geom.y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            width: geom.w.clamp(0, u16::MAX as u32) as u16,
            height: geom.h.clamp(0, u16::MAX as u32) as u16,
            border_width: bw.clamp(0, u16::MAX as u32) as u16,
            override_redirect: false,
        };
        // Fire-and-forget: no .check() here — this is called for every window
        // in arrange(), so a synchronous RTT per window is unacceptable.
        let _ = self
            .conn
            .send_event(false, win, EventMask::STRUCTURE_NOTIFY, event);

        // `_NET_FRAME_EXTENTS` mirror (single writer): the published extents
        // must equal the border actually applied, and a client sizes its content
        // from them — so this publishes the *wire* border, not the requested
        // one. They differ whenever the request exceeds CARD16, and publishing
        // the request would tell the client to lay out for a border the server
        // never had. Publish `[bw × 4]` only when it changed — `emit_geometry`
        // also fires for pure moves, where a property rewrite would be pure
        // protocol noise.
        if self.frame_extents.get(&win) != Some(&wire_bw) {
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                win,
                self.atoms.net_frame_extents,
                AtomEnum::CARDINAL,
                &[wire_bw, wire_bw, wire_bw, wire_bw],
            );
            self.frame_extents.insert(win, wire_bw);
        }

        if let Some(c) = self.engine.state.clients.get_mut(&win) {
            if write_client_geom {
                // The wire geometry, not the request: `client.geom` is the rect
                // hit-testing and the pointer warp read, so it has to be the
                // rect the server is actually holding.
                c.geom = wire;
                c.border_w = wire_bw;
            }
            c.geometry_dirty = false;
        }
        // Invalidate this monitor's cached live projection so the compositor
        // re-projects it on the next frame (the geometry it drew is now stale).
        if mon < self.engine.state.monitors.len() {
            self.engine.state.monitors[mon].layout_dirty = true;
        }

        self.sync_rounded_frame(win, geom, bw, is_fullscreen);

        Ok(())
    }

    fn sync_rounded_frame(&mut self, win: Window, geom: Rect, bw: u32, is_fullscreen: bool) {
        if self.engine.cfg.corner_radius > 0 || self.shape_mask_cache.contains_key(&win) {
            // Fullscreen is always square, niri-style — border-0 and edge-to-
            // edge, so a rounded mask has no desktop behind it to reveal and
            // just chops the content under a curved clip instead.
            let r = if is_fullscreen {
                0
            } else {
                self.engine.cfg.corner_radius as i32
            };
            // The frame is the window plus the border on both sides; see
            // `outer_frame` for why that arithmetic is saturating and why it
            // lives outside this method.
            let (outer_w, outer_h) = outer_frame(geom, bw);
            // The Shape `BOUNDING` mask depends only on (outer_w, outer_h, r,
            // bw), never on position. `emit_geometry` fires on every Configure
            // effect, including pure moves (camera scroll re-Configures every
            // visible window's x each animation frame), so without this guard
            // an unchanged mask was re-uploaded to the X server every such
            // frame. Skip the SHAPE request when nothing the mask depends on
            // has changed since the last one we actually issued. `bw` is part
            // of the key: the mask origin (-bw, -bw) anchors the mask at the
            // outer frame, so a border-width change alone re-anchors it.
            let key = (outer_w, outer_h, r, bw);
            if self.shape_mask_cache.get(&win) != Some(&key) {
                self.round_corners(win, outer_w, outer_h, r, bw);
                self.shape_mask_cache.insert(win, key);
            }
        }
    }

    pub(super) fn apply_geom(
        &mut self,
        win: Window,
        geom: Rect,
        bw: u32,
        write_client_geom: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let geom_dirty = self
            .engine
            .state
            .clients
            .get(&win)
            .is_some_and(|c| c.geometry_dirty);
        // The Reconciler is the single owner of "what geometry is already
        // applied to X11". Diff the desired placement against `AppliedState`;
        // only emit `configure_window` when it actually changed.
        // `geometry_dirty` forces the reconfigure even when the rect is
        // identical (a fullscreen/maximize transition changed the border/state
        // without moving the window), which is the same skip rule a plain
        // `geom == client.geom` comparison enforced.
        if let Some((g, b)) = self.applied.diff(win, geom, bw, geom_dirty) {
            self.emit_geometry(win, g, b, write_client_geom)?;
        }
        Ok(())
    }

    /// Re-apply the off-screen parking configure for a hidden client, keeping
    /// the logical model (`client.geom`) on the workspace it belongs to.
    ///
    /// Used by the client-driven geometry sinks: while a window is parked,
    /// `client.geom` is the *logical* (on-screen) rect, so `apply_geom` with the
    /// model rect would physically resurrect it on the active workspace (a
    /// background-workspace window appearing out of nowhere). Parking again with
    /// `write_client_geom = false` keeps the model intact and only lets the size
    /// change land, off-screen where it belongs.
    pub(super) fn apply_parked(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let Some(c) = self.engine.state.clients.get(&win) else {
            return Ok(());
        };
        let (rect, bw) = (parked_rect(c.geom), c.border_w);
        self.apply_geom(win, rect, bw, false)
    }

    /// Raise `win` above all its siblings (`TopLevel`). Fire-and-forget: called
    /// from arrange/focus paths where a synchronous RTT per window is not acceptable.
    fn raise(&self, win: Window) {
        let _ = self
            .conn
            .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    }

    pub(super) fn focus(&mut self, win: Option<Window>) -> Result<(), Box<dyn std::error::Error>> {
        let valid_win = win.filter(|w| self.engine.state.clients.contains_key(w));

        #[cfg(feature = "input-trace")]
        itrace!(
            "focus() called win={:?} valid={:?} sel_mon={} prev_focused(sel_mon)={:?} x11_input_focus={:?}",
            win, valid_win, self.engine.state.sel_mon,
            self.engine.state.monitors.get(self.engine.state.sel_mon).and_then(|m| m.focused),
            self.engine.state.x11_input_focus
        );

        // The presentation overlay (`core::present`) is independent of focus, so a
        // focus change never recomputes or re-sizes geometry. Focus only
        // influences stacking, via `stack_overlay` below: a focused presented
        // window rises above the other presented ones and a focused plain tile
        // "peeks" above the overlay.
        let prev_focused = self
            .engine
            .state
            .monitors
            .get(self.engine.state.sel_mon)
            .and_then(|m| m.focused);

        if let Some(w) = valid_win {
            // One scoped lookup: ends the immutable borrow of `clients` before
            // the mutations below re-borrow the map.
            let (mon_i, geom, wants, urgent) = {
                let c = match self.engine.state.clients.get(&w) {
                    Some(c) => c,
                    None => return self.focus(None),
                };
                if c.no_focus() {
                    return Ok(());
                }
                (
                    c.monitor,
                    c.geom,
                    c.wants_input,
                    c.flags.has(WinFlags::URGENT),
                )
            };

            // Guard against stale client.monitor after hotplug
            if mon_i >= self.engine.state.monitors.len() {
                return Ok(());
            }

            // unfocus previous — only if we're actually about to focus the new one
            if prev_focused != valid_win {
                if let Some(pw) = prev_focused {
                    if self.engine.state.clients.contains_key(&pw) {
                        self.unfocus(pw)?;
                    }
                }
            }

            self.engine.state.sel_mon = mon_i;

            // set X11 input focus. Use the real last-input timestamp, not
            // `CURRENT_TIME`: a `CurrentTime` focus request is silently ignored
            // by the server when a newer focus change has occurred, which is
            // exactly what desyncs logical vs real focus (the red-border bug).
            // ICCCM 4.1.7: a window with `input == False` (`wants_input ==
            // false`, but not `NO_FOCUS`) must NOT receive the X input focus —
            // focus the root instead and offer `WM_TAKE_FOCUS` below. Focusing
            // the window itself sends the keyboard to a client that declared
            // it never wants it.
            if wants {
                let _ = self
                    .conn
                    .set_input_focus(InputFocus::PARENT, w, self.last_event_time);
            } else {
                let _ = self.conn.set_input_focus(
                    InputFocus::POINTER_ROOT,
                    self.root,
                    self.last_event_time,
                );
            }
            if self.has_protocol(w, self.atoms.wm_take_focus)? {
                self.send_proto(w, self.atoms.wm_take_focus, self.last_event_time)?;
            }
            // Commit the logical focus to `State` *before* reconciling against
            // the real X input focus below. `reconcile_focus()` compares the real
            // X focus (`get_input_focus`) with `mon.focused`; a caller that
            // emitted `FocusWindow` without writing `mon.focused` itself
            // (`ViewWorkspace` is the prime example, unlike `FocusDirection`)
            // would leave it naming the window focused on the workspace we just
            // left. `reconcile_focus` would then see logical != real and re-assert
            // input focus onto that previous, now hidden-but-viewable window, and
            // real focus would desync from the visible one. Writing `mon.focused`
            // here makes logical == real for our own request, so the reconcile is
            // a no-op. Same ordering as the `focus(None)` branch below.
            {
                let mon = &mut self.engine.state.monitors[mon_i];
                mon.focused = Some(w);
                mon.focus_stack.retain(|&x| x != w);
                mon.focus_stack.push(w);
            }
            // The focus mirror is one of only two families of state the backend
            // writes that `check_invariants` actually constrains (clauses #8/#8b:
            // every focus-slot entry is a known client, and no duplicates). The
            // engine runs the check after every command, but this write is
            // outside the command pipeline — the module's own rule is that
            // `mon.focused` and `focus_stack` are written by this sink *and* by
            // `focus_logically` — so without a check here a caller naming a dead
            // or foreign window would pass through a debug build unchallenged.
            // `assert_invariants` is `#[cfg(debug_assertions)]`, so this costs
            // nothing in a release build.
            #[cfg(debug_assertions)]
            self.engine.state.assert_invariants();
            // No `reconcile_focus` here. `mon.focused` was written above, so for
            // a request the server accepted `logical == real` and the settle is
            // a no-op. For one it refused, the focus moved without us and the
            // server sends the `FocusOut` that hands repair to the event path —
            // probing here would re-ask for the answer every time we change
            // focus, on the thread that has to keep draining input.
            let col = if urgent {
                self.engine.cfg.col_urgent
            } else {
                self.engine.cfg.col_focused
            };
            let _ = self
                .conn
                .change_window_attributes(w, &ChangeWindowAttributesAux::new().border_pixel(col));
            self.grab_buttons(w, true)?;

            let serial = self.engine.state.next_serial();
            let was_urgent = if let Some(c) = self.engine.state.clients.get_mut(&w) {
                // Consume the urgency flag so its border color and the
                // `_NET_WM_STATE` demands-attention atom don't stick once the
                // window is actually focused.
                let was = c.flags.has(WinFlags::URGENT);
                if was {
                    c.flags.clear(WinFlags::URGENT);
                }
                c.focus_serial = serial;
                was
            } else {
                false
            };
            if was_urgent {
                self.write_net_wm_state(w);
            }

            #[cfg(feature = "input-trace")]
            itrace!(
                "focus() SET mon[{}].focused={:?} (was {:?}); x11_input_focus={:?}",
                mon_i,
                self.engine.state.monitors[mon_i].focused,
                prev_focused,
                self.engine.state.x11_input_focus
            );

            // Keep the maximize-overlay owner in sync with the new focus (single
            // writer; no read site infers it from `mon.focused`). Done after the
            // `itrace!` borrow above so it does not conflict with `mon`.
            self.engine.state.sync_presented_maximize(mon_i);

            // Keep the workspace's focused column/row in sync with the window
            // that is actually focused and recenter the camera on it — the same
            // "camera looks at the focused column" contract the `h`/`l` keyboard
            // navigation gets from `ideal_scroll`, so a mouse click on a side
            // tile makes that tile usable instead of leaving it peeking
            // off-screen.
            //
            // The recenter moves the just-focused window under the pointer; the
            // `warp_cursor` block at the end of this function puts the pointer
            // back on it, so the *next* click at the same spot cannot land on
            // whatever scrolled under the cursor.
            //
            // `retarget_focus_to_window` is the pure core helper the keyboard
            // path's `ideal_scroll` retarget also funnels through, and it is
            // `#[must_use]`: the monitor index it hands back is exactly the one
            // whose settled projection is still owed (`self.arrange` below), so
            // deleting that call turns the binding into an unused-variable
            // warning instead of a silent input-geometry regression.
            let retargeted = retarget_focus_to_window(&mut self.engine.state, &self.engine.cfg, w);

            // Keep X11 geometry (`client.geom`) in sync with the just-retargeted
            // camera. The keyboard focus path emits `ArrangeMonitor` before
            // `FocusWindow`, which makes `arrange` rewrite `client.geom` from the
            // new `camera.target`. The mouse path (`on_button_press`, `on_enter`,
            // `_NET_ACTIVE_WINDOW`) calls `focus()` directly with no
            // `ArrangeMonitor`, so `client.geom` would still point at the
            // previous settled position and the next X hit-test (`find_client`)
            // would land on the wrong window. Projecting here closes that
            // asymmetry: every `camera.target` mutation now derives
            // `client.geom`.
            //
            // `arrange` only *reads* `camera.{target,position}` to compute
            // geometry; it never mutates the spring, so the compositor keeps
            // interpolating `position → target`. `hide_offscreen` is skipped while
            // a drag is in progress (guarded in `arrange_full_phase`), so a focus
            // change mid-drag can't un-hide/offscreen windows incorrectly.
            self.arrange(retargeted.unwrap_or(mon_i))?;

            // Overlay stacking (presented / popups-of-presented / peek).
            self.stack_overlay(mon_i);

            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[w],
            );

            if self.engine.cfg.warp_cursor {
                // The one and only post-focus pointer warp. `arrange` above
                // rewrote `client.geom` to the new settled position; warp onto
                // *that* (not the stale pre-scroll `geom` captured at the top), so
                // the pointer lands on the window we actually focused rather than
                // wherever it slid from. Clamped to i16 like the synthetic-notify
                // path: a >32k half-size would wrap negative.
                let g = self.engine.state.clients.get(&w).map_or(geom, |c| c.geom);
                let dx = (g.w / 2).min(i16::MAX as u32) as i16;
                let dy = (g.h / 2).min(i16::MAX as u32) as i16;
                let _ = self.conn.warp_pointer(x11rb::NONE, w, 0, 0, 0, 0, dx, dy);
            }
        } else {
            // Only clear the focused window on the currently selected monitor.
            // Other monitors keep their own focused state independently.
            let sel = self.engine.state.sel_mon;
            if sel < self.engine.state.monitors.len() {
                if let Some(pw) = self.engine.state.monitors[sel].focused {
                    if self.engine.state.clients.contains_key(&pw) {
                        self.unfocus(pw)?;
                    }
                }
                self.engine.state.monitors[sel].focused = None;
                // The maximize overlay loses its owner with the focus.
                self.engine.state.sync_presented_maximize(sel);
            }
            let _ = self.conn.set_input_focus(
                InputFocus::POINTER_ROOT,
                self.root,
                self.last_event_time,
            );
            // Nothing to settle: `mon.focused` is already `None`, so logical and
            // real agree. A window that grabbed focus back generates the
            // `FocusIn` the event path settles against.
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                &[x11rb::NONE],
            );
        }

        // Announce the transition on the typed EventBus. Every focus move —
        // pointer clicks, button grabs, EnterNotify, manage/unmanage re-focus,
        // commands — funnels through `focus`, so this is the single choke point.
        // `Window` and `WindowId` are both `u32` aliases, so the values pass
        // straight through to the core's id space.
        if prev_focused != valid_win {
            self.engine.notify(crate::core::event::Event::FocusChanged {
                from: prev_focused,
                to: valid_win,
            });
        }

        Ok(())
    }

    pub(super) fn unfocus(&mut self, win: Window) -> Result<(), Box<dyn std::error::Error>> {
        let col = self.engine.cfg.col_normal;
        let _ = self
            .conn
            .change_window_attributes(win, &ChangeWindowAttributesAux::new().border_pixel(col));
        let _ = self.grab_buttons(win, false);
        Ok(())
    }

    pub(super) fn focus_best(&mut self, mon_idx: usize) -> Result<(), Box<dyn std::error::Error>> {
        let candidate = self.engine.state.best_focus(mon_idx);
        self.focus(candidate)
    }

    /// Re-assert the WM's logical focus intent onto the X server and repaint
    /// borders so `logical == x11_input_focus == visual`.
    ///
    /// Event-driven only: called after we issue a `set_input_focus` (to verify
    /// the server accepted it) and from `FocusIn`/`FocusOut` handlers. It reads
    /// `GetInputFocus` to learn the real X focus, then — if it diverges from the
    /// logical focus (`mon.focused`) — re-issues `set_input_focus` and repaints
    /// the two affected borders. No polling, no per-frame work.
    /// Learn the real X input focus from the server and settle against it.
    ///
    /// `GetInputFocus` blocks the event-loop thread, so this is for the callers
    /// that have no other way to learn where the focus went — chiefly
    /// `FocusOut`, which names the window that lost it and not the one that took
    /// it. A `FocusIn` already holds the answer; use [`Self::settle_focus`].
    pub(super) fn reconcile_focus(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let probed = match self.conn.get_input_focus() {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => {
                    let f = reply.focus;
                    if f == 0 || f == self.root {
                        None
                    } else {
                        Some(f)
                    }
                }
                Err(_) => self.engine.state.x11_input_focus,
            },
            Err(_) => self.engine.state.x11_input_focus,
        };
        self.settle_focus(probed)
    }

    /// Settle the WM's logical focus against a real focus the caller already
    /// holds.
    ///
    /// Every `FocusIn` names the window that took the focus, so a handler that
    /// has one already knows what `GetInputFocus` would answer — and knows it
    /// more accurately, since a probe issued afterwards reports where focus has
    /// since *moved*, not where it was when the event was generated. Asking
    /// anyway costs a blocking round trip on the single event-loop thread.
    pub(super) fn settle_focus(
        &mut self,
        real: Option<Window>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // X may report a child or another unmanaged window as the input
        // focus. The state mirror represents managed clients only; keeping an
        // unknown XID here violates the state invariant during normal GTK
        // focus transitions.
        self.engine.state.x11_input_focus =
            real.filter(|window| self.engine.state.clients.contains_key(window));
        let logical = self
            .engine
            .state
            .monitors
            .get(self.engine.state.sel_mon)
            .and_then(|m| m.focused);

        #[cfg(feature = "window-trace")]
        wtrace!(
            "reconcile_focus real={:?} logical(sel_mon={})={:?}",
            real,
            self.engine.state.sel_mon,
            logical
        );

        #[cfg(feature = "input-trace")]
        itrace!(
            "reconcile_focus real={:?} logical(sel_mon={})={:?}",
            real,
            self.engine.state.sel_mon,
            logical
        );

        if logical == real {
            #[cfg(feature = "input-trace")]
            itrace!("reconcile_focus OK (logical==real), no-op");
            return Ok(());
        }

        // ICCCM counterpart of `focus()`: a logical window with
        // `wants_input == false` intentionally leaves the X focus on the
        // root (real == None). That divergence is by design — re-asserting
        // the focus onto the window would undo it on every focus event.
        if real.is_none() {
            if let Some(w) = logical {
                let input_false = self
                    .engine
                    .state
                    .clients
                    .get(&w)
                    .is_some_and(|c| !c.wants_input && !c.no_focus());
                if input_false {
                    return Ok(());
                }
            }
        }

        // Presentation-aware guard: after `manage()` records a `pending_focus`
        // behind the live overlay while a fullscreen/maximized overlay keeps the
        // real X input focus, `logical` (sel_mon's focus) may diverge from
        // `real` (the overlay) on purpose. Do NOT re-assert input focus toward
        // the logical window here — that would steal the keyboard from the live
        // overlay and re-introduce the exact input-theft this policy prevents.
        // Only bail when the *real* focus is the presented overlay itself.
        //
        // The guard is scoped to the monitor that OWNS `real` (resolved from the
        // window's `client.monitor`), NOT the global `sel_mon`: scoping to
        // `sel_mon` would make any FocusIn/Out bail as long as *some* overlay
        // existed on the selected monitor, freezing focus repair on every other
        // monitor/workspace (invariant I3). Scoping to `real`'s monitor confines
        // the bail to the monitor that actually has the overlay, exactly where
        // the divergent focus is intentional.
        let guard_mon = real
            .and_then(|r| self.engine.state.clients.get(&r))
            .map(|c| c.monitor)
            .filter(|&m| m < self.engine.state.monitors.len())
            .unwrap_or(self.engine.state.sel_mon);
        if let Some(m) = self.engine.state.monitors.get(guard_mon) {
            if m.active_ws < m.workspaces.len() {
                if let Some(r) = real {
                    if self.engine.state.presented_overlay_owner(guard_mon) == Some(r) {
                        #[cfg(feature = "input-trace")]
                        itrace!(
                            "reconcile_focus BAIL guard: real={:#x} is a presented overlay on monitor={} active_ws={}",
                            r, guard_mon, m.active_ws
                        );
                        return Ok(());
                    }
                }
            }
        }

        // Divergence: re-assert the logical focus on X. This is exactly the
        // recovery path for the silent-focus-loss bug — an external
        // XSetInputFocus (popup/dialog/Wine/`_NET_ACTIVE_WINDOW` from another
        // tool) left real focus somewhere other than where the WM thinks it is.
        #[cfg(feature = "input-trace")]
        itrace!(
            "reconcile_focus REPAIR: re-asserting logical={:?} onto X (was real={:?}) on sel_mon={}",
            logical, real, self.engine.state.sel_mon
        );
        if let Some(w) = logical {
            if self.engine.state.clients.contains_key(&w) {
                let wants = match self.engine.state.clients.get(&w) {
                    Some(c) => c.wants_input,
                    None => true,
                };
                // ICCCM 4.1.7 (same rule as `focus()`): never put the X
                // input focus on an `input == False` window — re-assert to
                // the root instead.
                if wants {
                    let _ = self
                        .conn
                        .set_input_focus(InputFocus::PARENT, w, self.last_event_time);
                } else {
                    let _ = self.conn.set_input_focus(
                        InputFocus::POINTER_ROOT,
                        self.root,
                        self.last_event_time,
                    );
                }
                if self.has_protocol(w, self.atoms.wm_take_focus)? {
                    self.send_proto(w, self.atoms.wm_take_focus, self.last_event_time)?;
                }
                let urgent = self
                    .engine
                    .state
                    .clients
                    .get(&w)
                    .is_some_and(|c| c.flags.has(WinFlags::URGENT));
                let col = if urgent {
                    self.engine.cfg.col_urgent
                } else {
                    self.engine.cfg.col_focused
                };
                let _ = self.conn.change_window_attributes(
                    w,
                    &ChangeWindowAttributesAux::new().border_pixel(col),
                );
            }
        } else {
            let _ = self.conn.set_input_focus(
                InputFocus::POINTER_ROOT,
                self.root,
                self.last_event_time,
            );
        }

        // Repaint the previously-focused (now unfocused) window's border.
        if let Some(old) = real {
            if old != logical.unwrap_or(self.root) && self.engine.state.clients.contains_key(&old) {
                let _ = self.conn.change_window_attributes(
                    old,
                    &ChangeWindowAttributesAux::new().border_pixel(self.engine.cfg.col_normal),
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 22px-tall bar reserved at the top, so `workarea != screen` and the
    /// vertical clamp has a non-zero origin to respect.
    fn workarea() -> Rect {
        Rect::new(0, 22, 1920, 1058)
    }

    fn fits(g: Rect, wa: Rect, bw: u32) -> bool {
        let frame = 2 * bw as i32;
        g.x >= wa.x
            && g.y >= wa.y
            && g.x + g.w as i32 + frame <= wa.x + wa.w as i32
            && g.y + g.h as i32 + frame <= wa.y + wa.h as i32
    }

    #[test]
    fn float_inside_workarea_is_untouched() {
        let wa = workarea();
        let g = Rect::new(100, 200, 640, 480);
        assert_eq!(clamp_float_to_workarea(g, wa, 2), g);
    }

    // Rounded-corner mask geometry (see `round_corners`). `rounded_rectangles`
    // is a pure function of (w, h, r), and the call site anchors its mask at
    // (-bw, -bw) so the outer edge lands on the window's outer frame. These
    // tests pin the invariants that make that anchoring correct — the union
    // spans exactly the frame and is symmetric about both axes — without
    // duplicating the implementation's arithmetic.

    fn mask_extents(rects: &[Rectangle]) -> (i32, i32, i32, i32) {
        // (min_x, min_y, max_x, max_y) of the union — half-open, X11-style.
        let (mut min_x, mut min_y) = (i32::MAX, i32::MAX);
        let (mut max_x, mut max_y) = (i32::MIN, i32::MIN);
        for r in rects {
            min_x = min_x.min(r.x as i32);
            min_y = min_y.min(r.y as i32);
            max_x = max_x.max(r.x as i32 + r.width as i32);
            max_y = max_y.max(r.y as i32 + r.height as i32);
        }
        (min_x, min_y, max_x, max_y)
    }

    #[test]
    fn rounded_mask_spans_exactly_the_outer_frame() {
        let (w, h, r) = (800, 600, 12);
        let rects = rounded_rectangles(w, h, r);
        let (min_x, min_y, max_x, max_y) = mask_extents(&rects);
        assert_eq!((min_x, min_y, max_x, max_y), (0, 0, w, h));
    }

    #[test]
    fn rounded_mask_is_horizontally_symmetric() {
        // Every 1px corner row must leave the same inset on the left as on
        // the right, and the middle band must span the full width — otherwise
        // the arc eats one side (the bug `round_corners` anchoring fixes).
        let (w, h, r) = (401, 300, 10); // odd width: symmetry cannot hold by luck
        let rects = rounded_rectangles(w, h, r);
        let middle = &rects[0];
        assert_eq!((middle.x, middle.y, middle.width), (0, r as i16, w as u16));
        for row in &rects[1..] {
            let inset = row.x as i32;
            let right_edge = row.x as i32 + row.width as i32;
            assert_eq!(
                inset,
                w - right_edge,
                "row y={} must leave matching insets on both sides",
                row.y
            );
        }
    }

    #[test]
    fn rounded_mask_radius_is_bounded_by_half_the_smaller_side() {
        // A radius larger than half the window must clamp, never invert the
        // middle band (negative height) or push rows past the frame.
        for &(w, h, r) in &[
            (40, 30, 15),
            (30, 40, 20),
            (10, 10, 9),
            (6, 6, 3),
            (5, 5, 5),
        ] {
            let rects = rounded_rectangles(w, h, r);
            let (min_x, min_y, max_x, max_y) = mask_extents(&rects);
            assert_eq!(
                (min_x, min_y, max_x, max_y),
                (0, 0, w, h),
                "w={w} h={h} r={r}"
            );
            assert!(rects.iter().all(|x| x.height > 0 && x.width > 0));
        }
    }

    #[test]
    fn zero_radius_is_a_square_full_frame_mask() {
        let rects = rounded_rectangles(640, 480, 0);
        assert_eq!(rects.len(), 1);
        assert_eq!(
            (rects[0].x, rects[0].y, rects[0].width, rects[0].height),
            (0, 0, 640, 480)
        );
    }

    #[test]
    fn degenerate_mask_sizes_stay_valid() {
        // 0-sized frames fall into the early square-mask branch: a single
        // (0,0,w,h) rect, possibly with a 0 dimension when the frame itself
        // has none — never a malformed multi-rect union.
        for &(w, h) in &[(0, 0), (1, 1), (0, 100), (100, 0)] {
            let rects = rounded_rectangles(w, h, 8);
            assert_eq!(rects.len(), 1, "{w}x{h} must fall back to the square mask");
            assert_eq!((rects[0].x, rects[0].y), (0, 0));
            assert_eq!(rects[0].width, w.max(0) as u16);
            assert_eq!(rects[0].height, h.max(0) as u16);
        }
    }

    // Rounded focus ring: the server paints BOUNDING − CLIP. `rounded_frame_regions`
    // derives the client-clip mask from the same outer frame the BOUNDING mask
    // uses, inset by the border width with inner radius max(R - bw, 0); these
    // tests pin the band that makes the curved border visible without
    // re-deriving `rounded_rectangles` itself.

    /// The border the server actually paints: BOUNDING minus CLIP, both in
    /// outer-frame coordinates. The bounding mask *is* the outer frame
    /// (0,0,w,h); the clip lives in client space and is lifted by +bw.
    fn border_band(w: u32, h: u32, r: i32, bw: u32) -> std::collections::BTreeSet<(i32, i32)> {
        let (outer, inner) = rounded_frame_regions(w, h, r, bw);
        let mut pixels = std::collections::BTreeSet::new();
        for rect in &outer {
            for y in i32::from(rect.y)..i32::from(rect.y) + i32::from(rect.height) {
                for x in i32::from(rect.x)..i32::from(rect.x) + i32::from(rect.width) {
                    pixels.insert((x, y));
                }
            }
        }
        for rect in &inner {
            for y in i32::from(rect.y)..i32::from(rect.y) + i32::from(rect.height) {
                for x in i32::from(rect.x)..i32::from(rect.x) + i32::from(rect.width) {
                    pixels.remove(&(x + bw as i32, y + bw as i32));
                }
            }
        }
        pixels
    }

    #[test]
    fn focus_ring_band_is_thin_and_traces_the_frame_curve() {
        // The property under repair: with a rectangular CLIP the curved band
        // vanishes (rows 1..r-1 hold no border pixels at all). The ring must
        // instead hug the rounded frame — present on *every* arc row of all
        // four corners, symmetric, and at most 2·bw px thick (the vertical
        // border band may join the arc's; thickness beyond that means the
        // inner radius drifted from max(R − bw, 0)).
        for &(w, h, r, bw) in &[
            (800, 600, 12, 1),
            (401, 300, 10, 2),
            (100, 80, 18, 1),
            (60, 40, 7, 3),
            (30, 30, 15, 3),
        ] {
            let band = border_band(w, h, r, bw);
            assert!(!band.is_empty(), "w={w} r={r}: ring must exist");
            let ys: std::collections::BTreeSet<i32> = band.iter().map(|&(_, y)| y).collect();
            assert_eq!(
                ys.first().copied(),
                Some(0),
                "w={w} r={r}: ring must start at the outer top edge"
            );
            assert_eq!(
                ys.last().copied(),
                Some(h as i32 - 1),
                "w={w} r={r}: ring must reach the outer bottom edge"
            );
            // Arc rows: every row of the top corner zone carries ring pixels
            // on both the left and right corner (not only straight edges).
            for y in 0..r.min(h as i32 / 2) {
                let xs: Vec<i32> = band
                    .iter()
                    .filter(|&&(_, py)| py == y)
                    .map(|&(x, _)| x)
                    .collect();
                assert!(
                    !xs.is_empty(),
                    "w={w} h={h} r={r} bw={bw}: arc row y={y} lost its ring"
                );
                let left = xs[0];
                let right = xs[xs.len() - 1];
                // The ring hugs the frame curve: on arc rows its outermost
                // pixels coincide with the outer mask's own edge (row 0 of
                // the arc is chord-inset by up to r; deeper rows widen back
                // to the straight edge). Rasterization is right-inclusive,
                // hence the −1 on the corner-zone bound.
                assert!(
                    left <= r.min(w as i32 / 2),
                    "w={w} r={r} bw={bw}: left arc pixel x={left} outside the corner zone"
                );
                assert!(
                    right >= w as i32 - r.min(w as i32 / 2) - 1,
                    "w={w} r={r} bw={bw}: right arc pixel x={right} outside the corner zone"
                );
            }
            // The ring traces a curve: the leftmost ring pixel must move
            // outward (non-increasing inset) as the arc approaches the
            // straight edge, and start clearly inset (not on the straight
            // border at x < bw — that would be a rectangular ring).
            let mut prev_left = i32::MAX;
            for y in 0..r.min(h as i32 / 2) {
                let left = band
                    .iter()
                    .filter(|&&(_, py)| py == y)
                    .map(|&(x, _)| x)
                    .min()
                    .unwrap();
                assert!(
                    left <= prev_left,
                    "w={w} r={r}: ring inset grew downward (x={left} after {prev_left})"
                );
                prev_left = left;
            }
            // Thinness in the corner quadrants: within an arc zone, each
            // column's vertical ring run stays at rasterization width. The
            // honest bound comes from the discrete arc itself: near the
            // circle's flat foot the chord's integer floor stays constant
            // for up to ⌈√(2r)⌉ consecutive rows, and the lifted inner mask
            // can lag the outer by its own foot plus the border width.
            // A clip radius of R instead of R−bw smears the band far beyond
            // this (its centers de-concentric by bw); straight-edge columns
            // (x < bw) legitimately run the full height and are excluded.
            for x in bw as i32..r.min(w as i32 / 2) {
                let ys: Vec<i32> = band
                    .iter()
                    .filter(|&&(px, _)| px == x)
                    .map(|&(_, py)| py)
                    .collect();
                let mut run = 0;
                let mut max_run = 0;
                for window in ys.windows(2) {
                    if window[1] == window[0] + 1 {
                        run += 1;
                    } else {
                        run = 1;
                    }
                    max_run = max_run.max(run);
                }
                max_run = max_run.max(usize::from(!ys.is_empty()));
                let foot = (((r - bw as i32).max(1) * 2) as f64).sqrt().ceil() as usize;
                let allowed = 2 * bw as usize + foot + 1;
                assert!(
                    max_run <= allowed,
                    "w={w} h={h} r={r} bw={bw}: corner column x={x} band {max_run}px thick (allowed {allowed})"
                );
            }
        }
    }

    #[test]
    fn focus_ring_inner_radius_follows_outer_minus_border() {
        // Deriving the inner radius from the clamped outer radius keeps the
        // circle centers concentric. `rounded_frame_regions` returns the clip
        // in client space (origin 0,0) and the bounding in frame space
        // (origin 0,0 too, since the mask is anchored at -bw): comparing
        // extents directly, the clip must measure exactly w−2bw × h−2bw.
        for &(w, h, r, bw) in &[(800, 600, 12, 1), (401, 300, 10, 2), (100, 80, 18, 4)] {
            let (outer, inner) = rounded_frame_regions(w, h, r, bw);
            let (omin_x, omin_y, omax_x, omax_y) = mask_extents(&outer);
            let (imin_x, imin_y, imax_x, imax_y) = mask_extents(&inner);
            // Bounding spans the full outer frame; clip spans exactly the
            // client area, i.e. the outer frame inset by bw on every side.
            assert_eq!((omin_x, omin_y, omax_x, omax_y), (0, 0, w as i32, h as i32));
            assert_eq!(
                (imin_x, imin_y, imax_x, imax_y),
                (0, 0, (w - 2 * bw) as i32, (h - 2 * bw) as i32),
                "w={w} h={h} r={r} bw={bw}: clip must measure the inset client area"
            );
        }
    }

    #[test]
    fn focus_ring_vanishes_when_radius_reaches_the_border() {
        // max(R − bw, 0) → 0: the clip degenerates to the square client rect,
        // so the ring is the straight frame plus square-cut corners — the
        // correct geometry when the border eats the whole radius, never a
        // negative or inverted mask.
        for &(w, h, r, bw) in &[(60, 40, 1, 1), (60, 40, 2, 2), (30, 30, 3, 5)] {
            let (outer, inner) = rounded_frame_regions(w, h, r, bw);
            assert_eq!(mask_extents(&outer), (0, 0, w as i32, h as i32));
            assert_eq!(
                mask_extents(&inner),
                (0, 0, (w - 2 * bw) as i32, (h - 2 * bw) as i32),
                "w={w} h={h} r={r} bw={bw}: R ≤ bw must give a square inner clip"
            );
            assert_eq!(inner.len(), 1, "square clip must be a single rect");
        }
    }

    #[test]
    fn fullscreen_geometry_yields_square_masks_both_kinds() {
        // Fullscreen policy: effective radius 0 with border 0 — both masks
        // must be the plain full-frame rectangle, so neither clips content
        // under a curve nor leaves ring residue in the corners.
        let (outer, inner) = rounded_frame_regions(1440, 900, 0, 0);
        assert_eq!(outer.len(), 1);
        assert_eq!(inner.len(), 1);
        assert_eq!(
            (
                outer[0].x,
                outer[0].y,
                outer[0].width,
                outer[0].height,
                inner[0].x,
                inner[0].y,
                inner[0].width,
                inner[0].height
            ),
            (0, 0, 1440, 900, 0, 0, 1440, 900)
        );
    }

    #[test]
    fn float_past_the_edges_is_pulled_back() {
        let wa = workarea();
        let bw = 2;
        let g = clamp_float_to_workarea(Rect::new(5000, 5000, 640, 480), wa, bw);
        assert!(
            fits(g, wa, bw),
            "off-screen float must be pulled inside: {g:?}"
        );
        assert_eq!(g.w, 640, "a float that fits keeps its size");
        assert_eq!(g.h, 480);

        let g = clamp_float_to_workarea(Rect::new(-500, -500, 640, 480), wa, bw);
        assert_eq!((g.x, g.y), (wa.x, wa.y));
    }

    #[test]
    fn float_larger_than_workarea_is_resized_and_stays_inside() {
        // The regression: with only x/y clamped, `max_x` lands below `min_x` for
        // an oversized float, so it was parked at a negative coordinate while
        // still overflowing the screen.
        let wa = workarea();
        let bw = 2;
        let g = clamp_float_to_workarea(Rect::new(0, 0, 5000, 5000), wa, bw);
        assert!(
            fits(g, wa, bw),
            "an oversized float must be shrunk into the workarea, got {g:?}"
        );
        assert_eq!((g.x, g.y), (wa.x, wa.y));
        assert_eq!(g.w, wa.w - 2 * bw);
        assert_eq!(g.h, wa.h - 2 * bw);
    }

    #[test]
    fn zero_sized_workarea_never_produces_a_zero_dimension() {
        // Defensive: a degenerate workarea (mid-hotplug) must not yield a
        // width/height of 0, which X11 rejects with a BadValue.
        let wa = Rect::new(0, 0, 1, 1);
        let g = clamp_float_to_workarea(Rect::new(10, 10, 800, 600), wa, 4);
        assert!(g.w >= 1 && g.h >= 1);
        assert_eq!((g.x, g.y), (wa.x, wa.y));
    }

    // Float `ConfigureRequest` boundary: a client may size/move itself, but the
    // rect always goes through `clamp_float_to_workarea` in
    // `on_configure_request`'s float branch before it becomes `client.geom`,
    // `AppliedState` or X11.

    #[test]
    fn float_zero_size_request_is_clamped_to_minimum() {
        let wa = workarea();
        let bw = 2;
        // A client requesting 0x0 must never reach X11 as 0x0 (BadValue). The WM
        // policy floors the degenerate request to the X11-valid minimum.
        let g = clamp_float_to_workarea(Rect::new(100, 100, 0, 0), wa, bw);
        assert!(
            g.w >= 1 && g.h >= 1,
            "0x0 request must get a non-zero floor"
        );
        assert!(
            fits(g, wa, bw),
            "0x0-clamped float must still sit in workarea"
        );
    }

    #[test]
    fn float_partially_offscreen_is_pulled_back() {
        let wa = workarea();
        let bw = 2;
        // Horizontally inside but extends past the right/bottom edge.
        let g = clamp_float_to_workarea(Rect::new(1900, 1000, 640, 480), wa, bw);
        assert!(
            fits(g, wa, bw),
            "partially off-screen float must be pulled back: {g:?}"
        );
        assert_eq!(g.w, 640, "a float that fits keeps its size");
        assert_eq!(g.h, 480);
    }

    #[test]
    fn float_normal_resize_inside_is_honored() {
        let wa = workarea();
        let bw = 2;
        // A moderate resize that still fits is exactly the client's desired rect.
        let req = Rect::new(300, 300, 800, 600);
        let g = clamp_float_to_workarea(req, wa, bw);
        assert_eq!(g, req, "a fitting resize request is honored as-is");
    }

    #[test]
    fn float_huge_request_is_shrunk_to_workarea() {
        let wa = workarea();
        let bw = 2;
        let g = clamp_float_to_workarea(Rect::new(50, 50, 9000, 7000), wa, bw);
        assert!(
            fits(g, wa, bw),
            "huge request must shrink into workarea: {g:?}"
        );
        assert_eq!(g.w, wa.w - 2 * bw);
        assert_eq!(g.h, wa.h - 2 * bw);
    }

    // Transient-chain depth limit. `MAX_TRANSIENT_DEPTH` bounds the ownership
    // walk because `WM_TRANSIENT_FOR` is unvalidated client input (self-loops
    // and cycles are expressible): reaching the root is true up to depth 4 and
    // false at depth 5 (fail-safe — the deep popup is not raised above the
    // overlay), and a cyclic chain always terminates.

    /// Chain `1 → 2 → … → depth+1`, where each window is transient for the
    /// previous one, so window `depth + 1` sits exactly `depth` links below the
    /// root window `1`. Returns the map and the deepest window's id.
    fn chain(depth: u32) -> (std::collections::HashMap<WindowId, Client>, WindowId) {
        let mut clients = std::collections::HashMap::new();
        let mut prev: Option<WindowId> = None;
        for w in 1..=(depth + 1) {
            let mut c = Client::new(w, 0, 0);
            c.transient_parent = prev;
            clients.insert(w, c);
            prev = Some(w);
        }
        (clients, depth + 1)
    }

    #[test]
    fn transient_chain_depth1_reaches_the_root() {
        let (clients, leaf) = chain(1);
        assert_eq!(leaf, 2);
        assert!(
            transient_chain_reaches(&clients, leaf, &[1]),
            "a direct dialog of the overlay is owned by it"
        );
        // The root itself is not transient of anything.
        assert!(!transient_chain_reaches(&clients, 1, &[1]));
        // An unknown window has no chain at all.
        assert!(!transient_chain_reaches(&clients, 99, &[1]));
    }

    #[test]
    fn transient_chain_depth2_reaches_the_root() {
        let (clients, leaf) = chain(2);
        assert_eq!(leaf, 3);
        assert!(
            transient_chain_reaches(&clients, leaf, &[1]),
            "popup-of-popup is still owned by the root"
        );
        assert!(
            transient_chain_reaches(&clients, leaf, &[2]),
            "…and by its direct parent"
        );
    }

    #[test]
    fn transient_chain_depth4_at_the_limit_reaches_the_root() {
        let (clients, leaf) = chain(MAX_TRANSIENT_DEPTH as u32);
        assert_eq!(leaf, 5);
        assert!(
            transient_chain_reaches(&clients, leaf, &[1]),
            "depth {MAX_TRANSIENT_DEPTH} is the last depth that still resolves"
        );
        // Every intermediate link resolves too.
        for root in 2..=4 {
            assert!(transient_chain_reaches(&clients, leaf, &[root]));
        }
    }

    #[test]
    fn transient_chain_depth5_beyond_the_limit_is_fail_safe() {
        let (clients, leaf) = chain(MAX_TRANSIENT_DEPTH as u32 + 1);
        assert_eq!(leaf, 6);
        assert!(
            !transient_chain_reaches(&clients, leaf, &[1]),
            "depth 5 exceeds the bound: ownership is denied, not looped"
        );
        // Fail-SAFE, not fail-open: the deep window still resolves against every
        // ancestor inside the bound, and the rest of the chain is unaffected.
        for root in 2..=5 {
            assert!(
                transient_chain_reaches(&clients, leaf, &[root]),
                "ancestor {root} is within {MAX_TRANSIENT_DEPTH} links of the leaf"
            );
        }
        assert!(
            transient_chain_reaches(&clients, 5, &[1]),
            "the depth-4 window is unaffected by its deeper child"
        );
    }

    #[test]
    fn transient_chain_cycle_terminates() {
        // Self-loop: WM_TRANSIENT_FOR pointing at the window itself.
        let mut clients = std::collections::HashMap::new();
        let mut c = Client::new(1, 0, 0);
        c.transient_parent = Some(1);
        clients.insert(1, c);
        assert!(!transient_chain_reaches(&clients, 1, &[42]));
        assert!(transient_chain_reaches(&clients, 1, &[1]));

        // Two-window cycle: 1 ↔ 2, neither reaches an unrelated root.
        let mut clients = std::collections::HashMap::new();
        let mut a = Client::new(1, 0, 0);
        a.transient_parent = Some(2);
        let mut b = Client::new(2, 0, 0);
        b.transient_parent = Some(1);
        clients.insert(1, a);
        clients.insert(2, b);
        assert!(!transient_chain_reaches(&clients, 1, &[42]));
        assert!(!transient_chain_reaches(&clients, 2, &[42]));
    }

    #[test]
    fn transient_chain_stops_at_a_destroyed_parent() {
        // The middle of a depth-3 chain is gone (the walk must end, not panic).
        let (mut clients, leaf) = chain(3);
        clients.remove(&2);
        assert!(
            !transient_chain_reaches(&clients, leaf, &[1]),
            "a hole in the chain ends the walk"
        );
        assert!(
            transient_chain_reaches(&clients, leaf, &[3]),
            "the surviving part of the chain still resolves"
        );
    }

    // The float clamp is the single authority for float geometry, so it must be
    // idempotent and always land inside the workarea: any second application that
    // moves the rect is a visible jitter during a drag or an arrange.
    #[test]
    fn clamp_is_idempotent_and_stable() {
        let wa = workarea();
        for bw in [0u32, 2, 6] {
            for g in [
                Rect::new(100, 100, 640, 480),
                Rect::new(-100, -100, 800, 600),
                Rect::new(1900, 1000, 640, 480),
                Rect::new(0, 0, 5000, 5000),
                Rect::new(0, 0, 1, 1),
            ] {
                let a = clamp_float_to_workarea(g, wa, bw);
                let b = clamp_float_to_workarea(a, wa, bw);
                assert_eq!(a, b, "clamp debe ser idempotente para {g:?} bw={bw}");
                assert!(
                    fits(a, wa, bw),
                    "resultado debe encajar: {a:?} wa={wa:?} bw={bw}"
                );
            }
        }
    }

    #[test]
    fn clamp_matches_layout_float_path() {
        // A new float must not disagree between `manage`, `layout` and `drag`:
        // all of them use `clamp_float_to_workarea` with a 2*bw frame. A
        // frame-less clamp would leave a centred float 4 px off, and the next
        // `arrange` would snap it back — visible jitter.
        let wa = workarea();
        let bw = 2;
        let centered = Rect::new(
            wa.x + (wa.w as i32 - 400) / 2,
            wa.y + (wa.h as i32 - 300) / 2,
            400,
            300,
        );
        let clamped = clamp_float_to_workarea(centered, wa, bw);
        assert_eq!(
            clamped, centered,
            "centrado debe quedar igual con marco 2*bw"
        );
        let at_edge = Rect::new(wa.x + wa.w as i32 - 400, wa.y + wa.h as i32 - 300, 400, 300);
        let clamped_edge = clamp_float_to_workarea(at_edge, wa, bw);
        // Without the frame `at_edge` would fit; with it, it must give back 2*bw.
        assert_eq!(
            clamped_edge.x,
            wa.x + wa.w as i32 - 400 - 2 * bw as i32,
            "borde en el filo debe retroceder 2*bw"
        );
        assert!(fits(clamped_edge, wa, bw));
    }

    // "covering fullscreen" and "overlay owner" are disjoint concepts and must
    // never be conflated: a `Column` fullscreen covers the screen (it is the
    // covering fullscreen used by hit-testing) but is NOT the presentation
    // overlay owner, while a `presented_maximize` window IS the overlay owner
    // but is neither fullscreen nor covering.
    #[test]
    fn covering_fullscreen_and_overlay_owner_are_distinct() {
        use crate::types::{Client, LayoutKind, Monitor, Rect, State, WinFlags};

        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workarea = Rect::new(0, 0, 800, 600);
        // The covering concept is, by definition, a Column-ribbon participant.
        mon.workspaces[0].layout = LayoutKind::Column;
        state.monitors.push(mon);

        // Window 1: a Column fullscreen tile.
        let mut c1 = Client::new(1, 0, 0);
        c1.geom = Rect::new(0, 0, 100, 100);
        c1.flags.set(WinFlags::FULLSCREEN);
        state.add_client(c1);
        state.monitors[0].workspaces[0].add_tiled(1, 0.5);
        state.monitors[0].workspaces[0].focus.column_idx = 0;
        state.monitors[0].focused = Some(1);
        state.monitors[0].focus_stack.push(1);

        // A Column fullscreen COVERS the screen (covering fullscreen for hit-testing)...
        assert_eq!(
            state.covering_fullscreen_window(0),
            Some(1),
            "focused Column fullscreen is the covering fullscreen"
        );
        // ...but it is NOT the presentation overlay owner (Column ribbon tile, not overlay).
        assert_eq!(
            state.presented_overlay_owner(0),
            None,
            "a Column fullscreen is covering but NOT the overlay owner"
        );
        assert!(
            state.clients.get(&1).unwrap().is_fullscreen(),
            "it is still fullscreen in the LayoutKind sense"
        );

        // Window 3: a transient/modal popup belonging to the fullscreen app, drawn
        // on top of it. It is neither covering nor the overlay owner.
        let mut c3 = Client::new(3, 0, 0);
        c3.geom = Rect::new(50, 50, 80, 60);
        c3.flags.set(WinFlags::FLOAT);
        c3.transient_parent = Some(1);
        state.add_client(c3);
        state.monitors[0].workspaces[0].floats.push(3);
        state.monitors[0].focused = Some(3);
        state.monitors[0].focus_stack.push(3);
        assert_eq!(
            state.covering_fullscreen_window(0),
            Some(1),
            "the popup sits on top, but the fullscreen is still the covering window"
        );
        assert_eq!(
            state.presented_overlay_owner(0),
            None,
            "a Column fullscreen + its popup are still not an overlay owner"
        );

        // Window 2: a maximized (presented_maximize) window. It IS the overlay
        // owner, but is NOT fullscreen and NOT a covering fullscreen.
        let mut c2 = Client::new(2, 0, 0);
        c2.geom = Rect::new(0, 0, 100, 100);
        c2.flags.set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        state.add_client(c2);
        state.monitors[0].workspaces[0].add_tiled(2, 0.5);
        state.monitors[0].focused = Some(2);
        state.monitors[0].focus_stack.push(2);
        state.sync_presented_maximize(0);

        assert_eq!(
            state.presented_overlay_owner(0),
            Some(2),
            "a presented_maximize window IS the overlay owner"
        );
        assert!(
            !state.clients.get(&2).unwrap().is_fullscreen(),
            "a presented_maximize window is NOT LayoutKind-fullscreen"
        );
        assert_eq!(
            state.covering_fullscreen_window(0),
            None,
            "a presented_maximize window is NOT a covering fullscreen"
        );
    }

    // Float resize-storm convergence. A floating window that resizes itself
    // rapidly (a Qt download dialog updating its contents dozens of times per
    // second) must not flicker bigger/smaller on every update. The cause is a
    // float sink that answers with a size violating the client's own
    // `WM_NORMAL_HINTS`: a hint-respecting toolkit corrects it with an immediate
    // follow-up `ConfigureRequest`, so every update bounces exactly once
    // (answer → correction). The invariant pinned below is INV-FLOAT-CONVERGE:
    // both float sinks route through `normalize_float_request` (hints snap, then
    // workarea clamp, then a final settle onto the increment grid), so the WM's
    // answer is a fixed point of the toolkit's correction function and a burst
    // of N distinct self-resizes costs N configures, never 2N.

    use crate::core::layout::{fixed_size_hints, parse_wm_normal_hints, snap_float_to_hints};

    /// A terminal-style hint set: aligned min, base + increment grid, no max.
    fn term_hints() -> SizeHints {
        SizeHints {
            base_w: 0,
            base_h: 0,
            inc_w: 10,
            inc_h: 10,
            max_w: 0,
            max_h: 0,
            min_w: 100,
            min_h: 100,
            min_aspect: 0.0,
            max_aspect: 0.0,
            // Same "no flag bits" default every other test literal uses
            // (`..SizeHints::default()`); the hint-snap path reads only the
            // constraint fields, never the raw wire word.
            flags: 0,
            valid: true,
        }
    }

    /// A Qt-dialog-style hint set: minimum size only (what `QDialog` with a
    /// layout publishes; cf. the `PrismLauncher` `minimum size: 480x138` log),
    /// no increment grid.
    fn dialog_hints() -> SizeHints {
        SizeHints {
            min_w: 480,
            min_h: 138,
            valid: true,
            ..SizeHints::default()
        }
    }

    /// Model of a hint-respecting toolkit's correction when it observes a
    /// `ConfigureNotify` (Qt/Xt behaviour: clamp into [min, max], floor-snap
    /// onto the increment grid, hard bounds as the final word). When the WM's
    /// answer `a` satisfies `toolkit_correct(a) == a`, the toolkit sends no
    /// follow-up request and the exchange terminates.
    fn toolkit_correct(g: Rect, h: SizeHints) -> Rect {
        if !h.valid {
            return g;
        }
        let mut w = g.w as i32;
        let mut hh = g.h as i32;
        if h.min_w > 0 {
            w = w.max(h.min_w);
        }
        if h.min_h > 0 {
            hh = hh.max(h.min_h);
        }
        if h.max_w > 0 {
            w = w.min(h.max_w);
        }
        if h.max_h > 0 {
            hh = hh.min(h.max_h);
        }
        if h.inc_w > 0 {
            let base = h.base_w.max(0);
            w = if w >= base {
                base + (w - base) / h.inc_w * h.inc_w
            } else {
                base
            };
        }
        if h.inc_h > 0 {
            let base = h.base_h.max(0);
            hh = if hh >= base {
                base + (hh - base) / h.inc_h * h.inc_h
            } else {
                base
            };
        }
        if h.min_w > 0 {
            w = w.max(h.min_w);
        }
        if h.min_h > 0 {
            hh = hh.max(h.min_h);
        }
        if h.max_w > 0 {
            w = w.min(h.max_w);
        }
        if h.max_h > 0 {
            hh = hh.min(h.max_h);
        }
        Rect::new(g.x, g.y, w.max(1) as u32, hh.max(1) as u32)
    }

    fn hint_sets() -> Vec<SizeHints> {
        let mut fixed = term_hints();
        fixed.max_w = 200;
        fixed.max_h = 200;
        fixed.min_w = 200;
        fixed.min_h = 200;
        // Non-zero base with its own grid (decorations-aware toolkits).
        let mut based = term_hints();
        based.base_w = 4;
        based.base_h = 4;
        based.inc_w = 8;
        based.inc_h = 8;
        vec![
            SizeHints::default(),
            term_hints(),
            dialog_hints(),
            fixed,
            based,
        ]
    }

    fn storm_requests() -> Vec<Rect> {
        let mut out = Vec::new();
        // Degenerate, tiny, grid-aligned, grid-misaligned, fitting, huge and
        // off-screen requests — the shapes a download dialog emits while its
        // contents churn.
        for &(x, y) in &[(300, 300), (0, 0), (1500, 900), (-400, -300), (5000, 5000)] {
            for &(w, h) in &[
                (0, 0),
                (1, 1),
                (4, 4),
                (99, 99),
                (100, 100),
                (104, 104),
                (105, 105),
                (480, 138),
                (481, 139),
                (640, 480),
                (9000, 7000),
            ] {
                out.push(Rect::new(x, y, w, h));
            }
        }
        out
    }

    fn fits_normalized(g: Rect, wa: Rect, bw: u32) -> bool {
        let frame = 2 * bw as i32;
        g.x >= wa.x
            && g.y >= wa.y
            && g.x + g.w as i32 + frame <= wa.x + wa.w as i32
            && g.y + g.h as i32 + frame <= wa.y + wa.h as i32
    }

    // INV-FLOAT-CONVERGE: for every request shape and every well-formed hint
    // set, the normalized answer is a fixed point of the toolkit correction —
    // the client observes a size it accepts and sends no follow-up
    // `ConfigureRequest`.
    #[test]
    fn float_answer_is_toolkit_fixed_point() {
        let wa = Rect::new(0, 0, 1920, 1080);
        let bw = 2;
        for hints in hint_sets() {
            for req in storm_requests() {
                let answer = normalize_float_request(req, hints, wa, bw);
                assert_eq!(
                    toolkit_correct(answer, hints),
                    answer,
                    "INV-FLOAT-CONVERGE violated: req={req:?} hints={hints:?} answer={answer:?} would be corrected to {:?}",
                    toolkit_correct(answer, hints),
                );
            }
        }
    }

    /// A fitting request answered with a workarea clamp alone (no hints snap)
    /// still bounces: 104 px comes back as 104 px, the toolkit floors it to the
    /// 10 px grid (100) and re-requests. Normalizing answers 100 directly, so
    /// no correction follows.
    #[test]
    fn clamp_only_pipeline_bounces_but_normalized_does_not() {
        let wa = Rect::new(0, 0, 1920, 1080);
        let bw = 2;
        let hints = term_hints();
        let req = Rect::new(300, 300, 104, 104);
        let old = clamp_float_to_workarea(req, wa, bw);
        assert_eq!(old, req, "a fitting request used to be honored as-is");
        assert_ne!(
            toolkit_correct(old, hints),
            old,
            "pre-fix answer must be toolkit-corrected (this is the flicker bounce)"
        );
        let new = normalize_float_request(req, hints, wa, bw);
        assert_eq!(new, Rect::new(300, 300, 100, 100));
        assert_eq!(toolkit_correct(new, hints), new);
    }

    /// A fixed-size dialog (min == max) never moves no matter what it
    /// requests: rapid content updates cannot change its geometry at all.
    #[test]
    fn fixed_size_dialog_request_is_pinned() {
        let wa = Rect::new(0, 0, 1920, 1080);
        let h = SizeHints {
            min_w: 480,
            min_h: 300,
            max_w: 480,
            max_h: 300,
            valid: true,
            ..SizeHints::default()
        };
        assert!(fixed_size_hints(&h));
        for req in storm_requests() {
            let a = normalize_float_request(req, h, wa, 2);
            assert_eq!((a.w, a.h), (480, 300), "fixed dialog must not resize");
            assert_eq!(toolkit_correct(a, h), a);
        }
    }

    /// Idempotence of the snap itself: `snap(snap(x)) == snap(x)` for every
    /// request shape and hint set, so the WM can never chase its own answer.
    #[test]
    fn snap_is_idempotent() {
        for hints in hint_sets() {
            for req in storm_requests() {
                let once = snap_float_to_hints(req, hints);
                assert_eq!(
                    snap_float_to_hints(once, hints),
                    once,
                    "snap must be idempotent for req={req:?} hints={hints:?}"
                );
            }
        }
    }

    /// Full-pipeline idempotence when the hints are satisfiable inside the
    /// workarea: normalizing twice changes nothing, so alternating
    /// request/notify observations converge instead of oscillating.
    #[test]
    fn normalize_is_idempotent_when_hints_fit_workarea() {
        let wa = Rect::new(0, 0, 1920, 1080);
        for hints in [SizeHints::default(), term_hints(), dialog_hints()] {
            for req in storm_requests() {
                let once = normalize_float_request(req, hints, wa, 2);
                assert_eq!(
                    normalize_float_request(once, hints, wa, 2),
                    once,
                    "normalize must be idempotent for req={req:?} hints={hints:?}"
                );
            }
        }
    }

    /// Containment + validity through the normalization entry point: no
    /// degenerate (`0x0`) geometry ever reaches X11 (`BadValue`) and nothing
    /// escapes the workarea, hinted requests included.
    #[test]
    fn normalize_never_degenerate_never_escapes() {
        let wa = Rect::new(0, 22, 1920, 1058);
        for hints in hint_sets() {
            for req in storm_requests() {
                let g = normalize_float_request(req, hints, wa, 2);
                assert!(g.w >= 1 && g.h >= 1, "degenerate: {g:?}");
                assert!(
                    fits_normalized(g, wa, 2),
                    "escaped workarea: {g:?} wa={wa:?} req={req:?} hints={hints:?}"
                );
            }
        }
    }

    /// Snap honors min/max bounds and rounds to the increment grid with the same
    /// rounding the drag path uses.
    #[test]
    fn snap_enforces_bounds_and_grid() {
        let h = term_hints();
        // Below min → min (already grid-aligned).
        assert_eq!(snap_float_to_hints(Rect::new(0, 0, 40, 40), h).w, 100);
        // Misaligned → nearest grid multiple (104→100, 105→110).
        assert_eq!(snap_float_to_hints(Rect::new(0, 0, 104, 104), h).w, 100);
        assert_eq!(snap_float_to_hints(Rect::new(0, 0, 105, 104), h).w, 110);
        assert_eq!(snap_float_to_hints(Rect::new(0, 0, 109, 109), h).w, 110);
        // Position is never touched.
        let p = snap_float_to_hints(Rect::new(31, 47, 104, 104), h);
        assert_eq!((p.x, p.y), (31, 47));
        // Max wins over grid growth.
        let mut capped = term_hints();
        capped.max_w = 105;
        capped.max_h = 105;
        let c = snap_float_to_hints(Rect::new(0, 0, 9000, 9000), capped);
        assert_eq!((c.w, c.h), (105, 105));
        // Invalid hints → identity (no constraint means pass-through).
        let plain = Rect::new(300, 300, 104, 104);
        assert_eq!(snap_float_to_hints(plain, SizeHints::default()), plain);
    }

    /// The wire parser accepts the full 18-word body, rejects short bodies
    /// (caller keeps previous hints), and treats a zero-flags body as
    /// "valid, no constraints" — exactly the old manage-time semantics.
    #[test]
    fn parse_wm_normal_hints_wire_format() {
        assert!(parse_wm_normal_hints(&[0u32; 5]).is_none());
        assert!(parse_wm_normal_hints(&[0u32; 17]).is_none());
        let empty = parse_wm_normal_hints(&[0u32; 18]).unwrap();
        assert!(empty.valid);
        assert_eq!(empty.flags, 0, "an all-zero body carries no flag bits");
        assert_eq!(
            (empty.min_w, empty.inc_w, empty.max_w, empty.base_w),
            (0, 0, 0, 0)
        );

        // flags = PMinSize|PMaxSize|PResizeInc|PAspect|PBaseSize. The body is the
        // C `XSizeHints` struct serialized: x/y/w/h at 1..4, min at 5/6, max at
        // 7/8, inc at 9/10, aspect x/y pairs at 11..14, base at 15/16 (see
        // `parse_wm_normal_hints` — the indices are the ICCCM contract).
        let mut v = vec![0u32; 18];
        v[0] = 16 | 32 | 64 | 128 | 256;
        v[5] = 100;
        v[6] = 100; // min
        v[7] = 800;
        v[8] = 600; // max
        v[9] = 10;
        v[10] = 10; // inc
        v[11] = 16;
        v[12] = 9; // min aspect 16/9
        v[13] = 16;
        v[14] = 9; // max aspect 16/9
        v[15] = 4;
        v[16] = 4; // base
        let h = parse_wm_normal_hints(&v).unwrap();
        assert!(h.valid);
        assert_eq!(h.flags, v[0], "the raw flags word must round-trip");
        assert_eq!((h.min_w, h.min_h), (100, 100));
        assert_eq!((h.max_w, h.max_h), (800, 600));
        assert_eq!((h.inc_w, h.inc_h), (10, 10));
        assert_eq!((h.base_w, h.base_h), (4, 4));
        assert!((h.min_aspect - 16.0 / 9.0).abs() < 1e-6);
        assert!(!fixed_size_hints(&h));

        // min == max → fixed.
        let mut f = v.clone();
        f[7] = 100;
        f[8] = 100;
        let fh = parse_wm_normal_hints(&f).unwrap();
        assert!(fixed_size_hints(&fh));
    }

    // A workarea change re-decides float geometry, so the client's authority
    // over its own rect cannot survive it: the seal is released and the rect
    // re-settled against the new workarea. A seal that outlived the change
    // would keep the projection on the adopted rect (`layout::arrange` reads it
    // verbatim) after the WM has already moved the window, and a client
    // re-asserting its old geometry would then be adopted again — off the new
    // workarea, with no path left to reclaim it.

    /// A float adopted under a larger workarea: its frame hangs off the right
    /// edge of the shrunk one, so re-settling genuinely moves it.
    fn adopted_float(hints: SizeHints, geom: Rect) -> Client {
        let mut c = Client::new(1, 0, 0);
        c.flags.set(WinFlags::FLOAT);
        c.geom = geom;
        c.saved_geom = geom;
        c.hints = hints;
        c.border_w = 2;
        c.float_client_authority = true;
        c
    }

    #[test]
    fn workarea_change_releases_the_client_authority_seal() {
        let wa = Rect::new(0, 22, 1920, 1058);
        let hints = term_hints();
        let mut c = adopted_float(hints, Rect::new(1750, 900, 640, 480));

        let settled = reclaim_float_to_workarea(&mut c, wa)
            .expect("a sealed float is always re-decided, even when it does not move");
        assert!(
            !c.float_client_authority,
            "the WM re-decided this rect: the client is no longer its authority"
        );
        assert_ne!(
            settled, c.geom,
            "the adopted rect hangs off the new workarea and must not survive it"
        );
        assert!(
            fits_normalized(settled, wa, 2),
            "settled={settled:?} must sit inside wa={wa:?}"
        );
        // Releasing the seal costs no later movement: the settled rect is the
        // fixed point of the projection that runs without it.
        assert_eq!(normalize_float_geom(settled, hints, wa, 2), settled);
    }

    /// Same change, same contract, for a rect the new workarea cannot improve:
    /// there is no configure to make and the only thing left to decide is the
    /// seal, which is released just the same.
    #[test]
    fn workarea_change_releases_the_seal_of_an_unmoved_float() {
        let wa = Rect::new(0, 22, 1920, 1058);
        let hints = term_hints();
        let mut c = adopted_float(hints, Rect::new(300, 300, 640, 480));

        let settled = reclaim_float_to_workarea(&mut c, wa)
            .expect("a sealed float is always re-decided, even when it does not move");
        assert_eq!(settled, c.geom, "this rect is already inside the workarea");
        assert!(
            !c.float_client_authority,
            "an unmoved rect is still a WM-owned rect once the workarea changed"
        );
    }

    /// The other half of the contract: a float that is already where the
    /// workarea wants it and carries no seal needs no configure at all, so a
    /// workarea change stays silent for the windows it cannot improve.
    #[test]
    fn workarea_change_is_silent_for_an_unsealed_settled_float() {
        let wa = Rect::new(0, 22, 1920, 1058);
        let mut c = adopted_float(term_hints(), Rect::new(300, 300, 640, 480));
        c.float_client_authority = false;
        assert!(reclaim_float_to_workarea(&mut c, wa).is_none());
    }

    // Property coverage for this module's own geometry and for the projection
    // chain it drives. The examples above pin named shapes; the properties
    // below pin the contracts over the whole input domain: the Shape mask must
    // never leave the frame, the parking spot must never come back on screen,
    // the bounded transient walk must agree with the reachability it claims,
    // the float authority seal must not survive a workarea change, and the
    // render list must be total, deterministic and convergent.
    use crate::backend::x11::reconciler::AppliedState;
    use crate::config::Cfg;
    use crate::core::layout::{arrange, LayoutRegistry, Placements, RibbonScratch};
    use proptest::prelude::*;

    /// Screen geometry as a real output reports it: any origin (a second
    /// monitor to the left of the primary has a negative x) and at least one
    /// pixel on each axis, from a typical panel down to an output the border
    /// width alone covers.
    fn prop_output_rect() -> impl Strategy<Value = Rect> {
        prop_oneof![
            (-4096i32..=4096, -4096i32..=4096, 1u32..=4096, 1u32..=4096),
            (-4096i32..=4096, -4096i32..=4096, 1u32..=8, 1u32..=8),
        ]
        .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    /// Float geometry as a client may ask for it: any position (including far
    /// off-screen and negative) and any size, degenerate and absurd included.
    fn prop_requested_rect() -> impl Strategy<Value = Rect> {
        (any::<i32>(), any::<i32>(), any::<u32>(), any::<u32>())
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    /// One generated window: what it is, where it sits and how it is placed.
    /// Placement kind, flags, geometry and border are drawn independently, so a
    /// single run mixes ribbon tiles, floats, exclusive fullscreen and maximized
    /// overlays.
    #[derive(Debug)]
    struct WinSpec {
        tiled: bool,
        /// Referenced by the workspace but absent from `State::clients`: the
        /// projection has to skip such a window instead of configuring an XID
        /// the WM has no client for.
        dangling: bool,
        weight: f32,
        fullscreen: bool,
        /// `FullscreenPolicy::True` — the exclusive fullscreen that leaves the
        /// ribbon and is presented as an overlay covering `mon.screen`.
        exclusive: bool,
        maximized: (bool, bool),
        geom: Rect,
        border_w: u32,
        /// Publish an increment-grid size hint, the constraint set that makes
        /// the float projection settle onto a grid.
        grid_hints: bool,
    }

    fn prop_win() -> impl Strategy<Value = WinSpec> {
        (
            any::<bool>(),
            any::<bool>(),
            0.1f32..=1.0,
            any::<bool>(),
            any::<bool>(),
            (any::<bool>(), any::<bool>()),
            prop_requested_rect(),
            0u32..=8,
            any::<bool>(),
        )
            .prop_map(
                |(
                    tiled,
                    dangling,
                    weight,
                    fullscreen,
                    exclusive,
                    maximized,
                    geom,
                    border_w,
                    grid_hints,
                )| WinSpec {
                    tiled,
                    dangling,
                    weight,
                    fullscreen,
                    exclusive,
                    maximized,
                    geom,
                    border_w,
                    grid_hints,
                },
            )
    }

    /// Build a one-monitor session from generated parameters: the screen and
    /// its workarea, the windows, the camera/zoom state and the config knobs
    /// the projection reads. Windows are numbered from 1 in strategy order, so
    /// a shrunk counterexample names the exact window.
    #[allow(clippy::too_many_arguments)]
    fn scenario(
        screen: Rect,
        workarea: Rect,
        wins: Vec<WinSpec>,
        cam: (f32, f32),
        zoom: (f32, f32),
        page_zoom: (f32, f32),
        overview: bool,
        gaps: (u32, u32),
        border: u32,
        smart_gaps: bool,
        boost: f32,
    ) -> (State, Cfg) {
        let cfg = Cfg {
            border_w: border,
            gaps_inner: gaps.0,
            gaps_outer: gaps.1,
            smart_gaps,
            accordion_boost: boost,
            ..Cfg::default()
        };

        let mut mon = Monitor::new(screen, 1);
        mon.workarea = workarea;
        {
            let ws = &mut mon.workspaces[0];
            ws.camera.position = cam.0;
            ws.camera.target = cam.1;
            ws.zoom = zoom.0;
            ws.zoom_target = zoom.1;
            ws.page_zoom = page_zoom.0;
            ws.page_zoom_target = page_zoom.1;
            ws.overview = overview;
        }
        let mut state = State::new();
        let mut focus: Option<WindowId> = None;
        for (i, spec) in wins.iter().enumerate() {
            let win = i as WindowId + 1;
            let ws = &mut mon.workspaces[0];
            if spec.dangling {
                // Left in the workspace on purpose: the window died without the
                // tree being rewritten yet.
                if spec.tiled {
                    ws.add_tiled(win, spec.weight);
                } else {
                    ws.floats.push(win);
                }
                continue;
            }
            let mut c = Client::new(win, 0, 0);
            c.geom = spec.geom;
            c.saved_geom = spec.geom;
            c.border_w = spec.border_w;
            c.hints = if spec.grid_hints {
                SizeHints {
                    min_w: 100,
                    min_h: 100,
                    inc_w: 10,
                    inc_h: 10,
                    valid: true,
                    ..SizeHints::default()
                }
            } else {
                SizeHints::default()
            };
            if spec.fullscreen {
                c.flags.set(WinFlags::FULLSCREEN);
            }
            if spec.exclusive {
                c.fullscreen_policy = crate::types::FullscreenPolicy::True;
            }
            if spec.maximized.0 {
                c.flags.set(WinFlags::MAXIMIZED_V);
            }
            if spec.maximized.1 {
                c.flags.set(WinFlags::MAXIMIZED_H);
            }
            if spec.tiled {
                ws.add_tiled(win, spec.weight);
                focus.get_or_insert(win);
            } else {
                c.flags.set(WinFlags::FLOAT);
                ws.floats.push(win);
            }
            state.add_client(c);
        }
        mon.focused = focus;
        state.monitors.push(mon);
        // `presented_maximize` is derived state; the projection reads it, so it
        // has to be in the state a real focus change would have left behind.
        state.sync_presented_maximize(0);
        (state, cfg)
    }

    /// One arrange cycle's render list, built exactly the way
    /// `arrange_full_phase` builds it: the layout projection, then the
    /// presentation overlay. `None` for a monitor index the state does not
    /// have, which the caller must treat as "nothing to place".
    fn projected(state: &State, cfg: &Cfg, mon_idx: usize) -> Option<(Placements, Vec<WindowId>)> {
        let mon = state.monitors.get(mon_idx)?;
        let mut placements = Placements::new();
        arrange(
            state,
            mon_idx,
            cfg,
            &LayoutRegistry::new(),
            Phase::Live,
            &mut placements,
            &mut RibbonScratch::default(),
        );
        let mut raise = Vec::new();
        present_into(state, mon, &mut placements, &mut raise);
        Some((placements, raise))
    }

    /// The `ConfigureWindow` calls a reconcile would issue, as plain data so a
    /// failing assertion can name the window and the rect it would poke.
    fn configures(effects: Vec<GeometryEffect>) -> Vec<(WindowId, Rect, u32)> {
        effects
            .into_iter()
            .map(|e| match e {
                GeometryEffect::Configure { win, rect, border } => (win, rect, border),
            })
            .collect()
    }

    proptest! {
        /// The Shape mask is a union of X11 rectangles handed to the server: a
        /// rectangle that reaches outside `[0,w) x [0,h)` clips content the WM
        /// promised to show, and a zero-area rectangle in the rounded path is a
        /// malformed arc row. The mask must therefore stay inside the frame and
        /// still cover the frame's bounding box, for every radius a caller can
        /// pass — including the negative and larger-than-half radii the clamp
        /// exists to absorb.
        #[test]
        fn prop_rounded_mask_stays_inside_the_frame(
            (w, h) in (0i32..=2048, 0i32..=2048),
            r in -32i32..=4096,
        ) {
            let rects = rounded_rectangles(w, h, r);
            prop_assert!(!rects.is_empty(), "a mask always has at least one rectangle");
            // More than one rectangle means the arc rows are in play; the
            // square fallback is a single full-frame rectangle that is allowed
            // to be degenerate for a degenerate frame.
            let arc_rows = rects.len() > 1;
            for rect in &rects {
                let (x, y) = (i32::from(rect.x), i32::from(rect.y));
                let (rw, rh) = (i32::from(rect.width), i32::from(rect.height));
                prop_assert!(
                    x >= 0 && y >= 0,
                    "mask starts outside the frame at ({}, {}) for w={} h={} r={}",
                    x, y, w, h, r
                );
                prop_assert!(
                    x + rw <= w,
                    "mask is {}px wider than the frame for w={} h={} r={}",
                    x + rw - w, w, h, r
                );
                prop_assert!(
                    y + rh <= h,
                    "mask is {}px taller than the frame for w={} h={} r={}",
                    y + rh - h, w, h, r
                );
                if arc_rows {
                    prop_assert!(
                        rw >= 1 && rh >= 1,
                        "an arc row must cover at least one pixel for w={} h={} r={}: ({}, {}, {}, {})",
                        w, h, r, x, y, rw, rh
                    );
                }
            }
            prop_assert_eq!(
                mask_extents(&rects),
                (0, 0, w, h),
                "the mask must cover the frame's bounding box for w={} h={} r={}",
                w, h, r
            );
        }

        /// A Shape `BOUNDING` mask is a union of X11 rectangles, so its extent
        /// travels in CARD16 (`Rectangle::width`/`height` are `u16`). A frame
        /// wider than `u16::MAX` therefore has no exact mask, and narrowing it
        /// with `as` does not clamp — it wraps, and the server is handed a
        /// rectangle covering a fraction of the frame at the wrong offset.
        ///
        /// `border_width` is attacker-of-self supplied and unclamped all the way
        /// from the config file (`userconfig` -> `Cfg::border_w` -> `SetBorderWidth`
        /// IPC), and the frame is `geom + 2*bw`, so the unrepresentable case is
        /// reachable from an ordinary `border_width = 40000`. The mask must stay
        /// inside the *representable* frame — `min(w, u16::MAX)` — and must never
        /// wrap below it.
        #[test]
        fn prop_rounded_frame_mask_never_wraps_the_card16_boundary(
            w in 60_000u32..=200_000,
            h in 1u32..=4_000,
            r in 0i32..=2_000,
            bw in 0u32..=60_000,
        ) {
            let (outer, _inner) = rounded_frame_regions(w, h, r, bw);
            // `w`/`h` are already the outer frame (the caller adds the border),
            // and anything past the protocol's representable frame cannot be
            // described, so the mask is defined against that part of it.
            let rw = w.min(MAX_MASKED_FRAME);
            let rh = h.min(MAX_MASKED_FRAME);
            for rect in &outer {
                let (x, y) = (i32::from(rect.x), i32::from(rect.y));
                let (rw2, rh2) = (i32::from(rect.width), i32::from(rect.height));
                prop_assert!(
                    x >= 0 && y >= 0,
                    "mask starts outside the frame at ({}, {}) for w={} h={} r={} bw={}",
                    x, y, w, h, r, bw
                );
                prop_assert!(
                    x + rw2 <= rw as i32,
                    "mask reaches {} but the representable frame ends at {} (narrowed {} \
                     from a wrapped width) for w={} h={} r={} bw={}",
                    x + rw2, rw, rw2, w, h, r, bw
                );
                prop_assert!(
                    y + rh2 <= rh as i32,
                    "mask reaches {} but the representable frame ends at {} for w={} h={} r={} bw={}",
                    y + rh2, rh, w, h, r, bw
                );
            }
            prop_assert_eq!(
                mask_extents(&outer),
                (0, 0, rw as i32, rh as i32),
                "the mask must cover the whole representable frame for w={} h={} r={} bw={}",
                w, h, r, bw
            );
        }
    }

    /// The mask has to be a request the server will accept, and both ends of that
    /// were wrong by measurement against Xvfb / X.Org 21.1.24.
    ///
    /// **The extent is signed too, and `MAX_MASKED_FRAME` was one too high.** A
    /// rectangle's right/bottom edge is INT16, and an edge reaching
    /// `i16::MAX + 1` is clamped down — so a 32768 px frame was masked as 32767,
    /// leaving a 1 px strip of every window's right and bottom edge square on a
    /// frame the user asked to be rounded. A band placed at the last
    /// representable origin was discarded outright, with no protocol error.
    #[test]
    fn the_masked_frame_never_reaches_past_the_signed_extent() {
        assert_eq!(
            MAX_MASKED_FRAME, 32_767,
            "the extent is INT16 too: one past i16::MAX is clamped, not exact"
        );
        for (w, h) in [
            (32_767u32, 1u32),
            (32_768, 1),
            (65_535, 65_535),
            (u32::MAX, u32::MAX),
        ] {
            let (outer, _) = rounded_frame_regions(w, h, 0, 0);
            for rect in &outer {
                let edge = i32::from(rect.x) + i32::from(rect.width);
                assert!(
                    edge <= MAX_MASKED_FRAME as i32,
                    "a mask rectangle reaches {edge}, past the {MAX_MASKED_FRAME} \
                     the server will store ({rect:?} for a {w}x{h} frame)"
                );
            }
        }
    }

    /// A mask that does not fit in one request does not fail — it ends the
    /// window manager. `x11rb` writes the length field as
    /// `u16::try_from(len / 4).unwrap_or(0)`, so an over-long `ShapeRectangles`
    /// sends a length of zero and the server parses the rest as further requests.
    /// Measured ceiling: 32765 rectangles accepted, the 32766th rejected with
    /// `BadLength`; 32765 = (262 140 - 20) / 8.
    ///
    /// `corner_radius` reaches here with no bound at all — `userconfig` copies it
    /// into `Cfg` unclamped — so this is reachable from a config file, and the
    /// radius is the only thing that scales the rectangle count: two bands of one
    /// rectangle per row.
    #[test]
    fn a_mask_request_can_never_exceed_the_servers_maximum_length() {
        assert_eq!(
            MAX_MASK_RECTS,
            (262_140 - 20) / 8,
            "derived from the request size"
        );
        for radius in [0i32, 1, 255, 16_382, 16_383, 16_384, 1_000_000, i32::MAX] {
            for (w, h) in [
                (MAX_MASKED_FRAME, MAX_MASKED_FRAME),
                (u32::MAX, u32::MAX),
                (1, MAX_MASKED_FRAME),
                (MAX_MASKED_FRAME, 1),
            ] {
                for (outer, inner) in [
                    rounded_frame_regions(w, h, radius, 0),
                    rounded_frame_regions(w, h, radius, 40_000),
                ] {
                    let n = outer.len() + inner.len();
                    assert!(
                        n <= MAX_MASK_RECTS,
                        "r={radius} on a {w}x{h} frame produced {n} rectangles, \
                         which is {} bytes against a 262 140 byte limit — the \
                         length field wraps to 0 and the connection dies",
                        n * 8 + 20
                    );
                }
            }
        }
    }

    /// A `border_width` of 2147483648 in the config file reaches the frame
    /// arithmetic, and plain `2 * bw` overflows `u32` there — which a debug
    /// build traps rather than wrapping. `outer_frame` is factored out of
    /// `sync_rounded_frame` precisely so this is testable without a display;
    /// before that, the saturating operators had no way to be exercised at all,
    /// and a sabotage audit confirmed they survived.
    #[test]
    fn an_unrepresentable_border_saturates_the_frame_instead_of_trapping() {
        for bw in [
            0u32,
            1,
            2,
            1_000_000,
            1 << 30,
            (1 << 31) - 1,
            1 << 31,
            u32::MAX,
        ] {
            let (w, h) = outer_frame(Rect::new(0, 0, 1920, 1080), bw);
            assert!(w > 0 && h > 0, "a frame must stay positive for bw={bw}");
            assert_eq!(w, 1920u32.saturating_add(bw.saturating_mul(2)));
            assert_eq!(h, 1080u32.saturating_add(bw.saturating_mul(2)));
        }
        // The exact value a config file can carry, named rather than generated.
        // `1 + 2 * 2^31` is 4294967297, one past `u32::MAX`: wrapping would give
        // 1 and plain `2 * bw` would have trapped before reaching this line.
        let (w, h) = outer_frame(Rect::new(0, 0, 1, 1), 2_147_483_648);
        assert_eq!(
            w,
            u32::MAX,
            "the frame saturates at u32::MAX, it does not wrap"
        );
        assert_eq!(h, u32::MAX, "same on the other axis");
    }

    /// The two CARD16 boundaries are different clamps and must not be confused:
    /// `outer_frame` saturates the *extent* arithmetic, and
    /// `rounded_frame_regions` then reduces the result to what an `i16` origin
    /// can address. A `border_width` of 40 000 produces a frame that is
    /// representable as an extent and not as an origin.
    #[test]
    fn the_frame_clamp_and_the_mask_clamp_are_both_reached() {
        let bw = 40_000;
        let (outer_w, outer_h) = outer_frame(Rect::new(0, 0, 1, 1_000), bw);
        assert_eq!(
            (outer_w, outer_h),
            (80_001, 81_000),
            "frame is 1 px plus 2*bw"
        );
        let (outer, _) = rounded_frame_regions(outer_w, outer_h, 10, bw);
        assert_eq!(
            mask_extents(&outer),
            (0, 0, MAX_MASKED_FRAME as i32, MAX_MASKED_FRAME as i32),
            "and the mask clamps that frame to the i16 origin range"
        );
    }

    /// The reproducer for the wrap above, pinned so the exact arithmetic stays
    /// visible. `sync_rounded_frame` builds the frame as `geom + 2*bw` and hands
    /// that to `rounded_frame_regions`, so a `border_width` of 40 000 around a
    /// 1 px client asks for an 80 001 px frame — and `80_001 as u16` is 14 465,
    /// a mask covering an eighth of the frame at the wrong size.
    #[test]
    fn a_forty_thousand_pixel_border_does_not_wrap_the_mask() {
        let bw = 40_000;
        let geom = Rect::new(0, 0, 1, 1_000);
        // The frame the caller actually derives, reproduced rather than re-derived.
        let outer_w = geom.w + 2 * bw;
        let outer_h = geom.h + 2 * bw;
        assert_eq!((outer_w, outer_h), (80_001, 81_000));

        let (outer, _) = rounded_frame_regions(outer_w, outer_h, 10, bw);
        assert_eq!(
            mask_extents(&outer),
            (0, 0, MAX_MASKED_FRAME as i32, MAX_MASKED_FRAME as i32),
            "an 80 001 px frame must be described against the {} px the protocol \
             can express, not wrapped down to {}",
            MAX_MASKED_FRAME,
            80_001 % 65_536
        );
    }

    /// `rounded_rectangles` takes `i32` and clamps the radius with
    /// `r.clamp(0, w.min(h) / 2)`, which panics whenever the upper bound is
    /// below zero. A frame dimension above `i32::MAX` arrives as a *negative*
    /// `i32` through the `u32 as i32` narrowing at the call site, so the clamp
    /// would be asked to clamp against `-1` and abort the process.
    #[test]
    fn a_frame_wider_than_i32_max_does_not_panic_the_radius_clamp() {
        // `u32::MAX - 1` narrows to `-2`, and `-2 / 2` is `-1`.
        let (outer, inner) = rounded_frame_regions(u32::MAX - 1, 1_000, 10, 0);
        for rect in outer.iter().chain(inner.iter()) {
            assert!(rect.width > 0 && rect.height > 0, "empty mask rectangle");
        }
    }

    proptest! {
        /// The mask is anchored at the window's *outer* top-left corner, so
        /// every corner row has to leave the same inset on the left as on the
        /// right and the middle band has to span the full width. An asymmetric
        /// arc eats one side of the frame, which is exactly the defect the -bw
        /// anchoring in `round_corners` exists to keep fixed.
        #[test]
        fn prop_rounded_mask_is_symmetric_about_both_sides(
            (w, h) in (1i32..=2048, 1i32..=2048),
            r in 0i32..=4096,
        ) {
            let rects = rounded_rectangles(w, h, r);
            let band = rects[0];
            prop_assert_eq!(i32::from(band.x), 0, "the middle band starts at the frame's left edge");
            prop_assert_eq!(
                i32::from(band.width),
                w,
                "the middle band spans the full width for w={} h={} r={}",
                w, h, r
            );
            prop_assert!(i32::from(band.height) >= 1, "the middle band always covers a row");
            for row in &rects[1..] {
                let left = i32::from(row.x);
                let right_edge = i32::from(row.x) + i32::from(row.width);
                prop_assert_eq!(
                    left,
                    w - right_edge,
                    "row y={} leaves unequal insets for w={} h={} r={}",
                    row.y, w, h, r
                );
                prop_assert_eq!(
                    i32::from(row.height),
                    1,
                    "corner rows are one pixel tall for w={} h={} r={}",
                    w, h, r
                );
            }
        }

        /// The server paints the border as BOUNDING minus CLIP, so the client
        /// clip mask must measure exactly the client area — the outer frame
        /// inset by the border width on every side. A larger clip swallows the
        /// curved border (the focus ring disappears), a smaller one leaves
        /// content clipped under the frame, and a clip that leaves the frame
        /// once lifted by `bw` punches a hole in the window.
        #[test]
        fn prop_client_clip_measures_exactly_the_inset_frame(
            (w, h) in (0u32..=2048, 0u32..=2048),
            r in -32i32..=4096,
            bw in 0u32..=16,
        ) {
            let (outer, inner) = rounded_frame_regions(w, h, r, bw);
            prop_assert_eq!(
                mask_extents(&outer),
                (0, 0, w as i32, h as i32),
                "the bounding mask is the whole outer frame for w={} h={} r={} bw={}",
                w, h, r, bw
            );
            prop_assert_eq!(
                mask_extents(&inner),
                (
                    0,
                    0,
                    w.saturating_sub(2 * bw) as i32,
                    h.saturating_sub(2 * bw) as i32
                ),
                "the clip mask is the client area for w={} h={} r={} bw={}",
                w, h, r, bw
            );
            // Lifting the clip into frame coordinates only means something when
            // the client area is not degenerate: with `2 * bw` already covering
            // the frame the clip is empty and there is no hole to punch.
            let frame_fits = w > 2 * bw && h > 2 * bw;
            for rect in &inner {
                let x = i32::from(rect.x) + bw as i32;
                let y = i32::from(rect.y) + bw as i32;
                let inside = x >= 0
                    && y >= 0
                    && x + i32::from(rect.width) <= w as i32
                    && y + i32::from(rect.height) <= h as i32;
                prop_assert!(
                    !frame_fits || inside,
                    "clip rectangle ({}, {}, {}, {}) leaves the frame once lifted by bw={} for w={} h={} r={}",
                    i32::from(rect.x), i32::from(rect.y), rect.width, rect.height, bw, w, h, r
                );
            }
        }

        /// The parking spot is the single definition of "hidden": every sink
        /// that parks a window re-parks it with this function, so a client that
        /// moves or resizes itself while parked can never come back on screen.
        /// The origin must stay left of x=0 for *every* width — including the
        /// saturating `u32::MAX` case the implementation comments call out,
        /// where a wrapping negation would park the window at a visible
        /// coordinate — and it must not depend on the rect's own x, so
        /// re-parking lands on the same spot. The right edge is measured against
        /// the width the server actually receives, because `emit_geometry`
        /// clamps the wire width to `u16::MAX` and a four-billion-pixel window
        /// cannot exist on the wire.
        #[test]
        fn prop_parked_rect_is_off_screen_and_stable(
            (w, h) in (0u32..=u32::MAX, 0u32..=u32::MAX),
            y in any::<i32>(),
        ) {
            let parked = parked_rect(Rect::new(4321, y, w, h));
            prop_assert!(parked.x < 0, "a parked window must sit left of the origin: x={}", parked.x);
            prop_assert!(
                parked.y == y && parked.h == h,
                "parking moves a window, it does not resize it vertically: {:?}",
                parked
            );
            prop_assert_eq!(
                parked_rect(Rect::new(-9999, y, w, h)),
                parked,
                "the parking spot must not depend on the rect's own x"
            );
            prop_assert_eq!(
                parked_rect(parked),
                parked,
                "re-parking a parked window must not move it"
            );
        }
    }

    /// The invariant `prop_parked_rect_is_off_screen_and_stable` has to be
    /// stated against, because the arithmetic above it was not: the X server
    /// does not receive the rect Maverick holds. `ConfigureWindow`'s x/y are
    /// INT16 and its width/height are CARD16, and the server *silently*
    /// truncates an out-of-range x rather than raising `BadValue` — measured on
    /// X.Org 1.24.1, `x = -65735` is stored as `-199`, with no error.
    ///
    /// So a park at `-(w + 200)` only stays off-screen while `w + 200` fits in
    /// INT16, i.e. `w <= 32568`. Above that the negation wraps and the window
    /// lands back on the screen the user is looking at — at a width of 65 535 it
    /// covers it entirely. The whole point of the park is that a client resizing
    /// itself while parked cannot resurrect its window on the active workspace,
    /// so the property has to be checked on the geometry the server keeps.
    #[test]
    fn the_server_never_stores_a_parked_window_on_screen() {
        // The x the server keeps, for the x Maverick sends.
        let server_x = |x: i32| x as i16 as i32;
        // The w the server keeps, for the w Maverick sends.
        let server_w = |w: u32| w.clamp(1, u16::MAX as u32) as i32;

        for w in (0u32..=u16::MAX as u32).step_by(97) {
            let parked = parked_rect(Rect::new(0, 0, w, 10));
            let right = server_x(parked.x) + server_w(parked.w);
            assert!(
                right <= 0,
                "a parked {w}px window is stored at x={} and reaches {right}, \
                 which is on screen",
                server_x(parked.x)
            );
        }
    }

    /// The width that actually broke: `-(w + 200)` left INT16 at `w = 32569`,
    /// and by `w = 65535` the negation had wrapped so far that the server stored
    /// `x = -199` — a 65 535 px window covering the whole screen. The server is
    /// handed the *parked* width, not the client's, so that is what the right
    /// edge has to be measured against.
    #[test]
    fn a_parked_window_wider_than_int16_cannot_come_back_on_screen() {
        let parked = parked_rect(Rect::new(0, 0, 65_535, 10));
        assert_eq!(
            parked.x as i16 as i32, parked.x,
            "a park at {} must be representable as the INT16 the server stores, \
             or the server keeps {} instead",
            parked.x, parked.x as i16 as i32
        );
        let right = (parked.x as i16 as i32) + parked.w.min(u16::MAX as u32) as i32;
        assert!(
            right <= 0,
            "the stored geometry reaches x={right}: a parked window is visible"
        );
        // And the window is still parked, i.e. narrower than the client asked for
        // but never on screen.
        assert!(
            parked.w < 65_535,
            "a window too wide for INT16 parking must be narrowed, not left covering the screen"
        );
    }

    proptest! {
        /// `WM_TRANSIENT_FOR` is unvalidated client input — a client can point
        /// a window at itself or two windows at each other — so the ownership
        /// walk is bounded and fail-safe. Both obligations are checked against
        /// an independent breadth-first search over the same graph rather than
        /// against the walk's own loop counter: a window may only be reported
        /// owned by a root it can actually reach (never raise a popup above an
        /// unrelated overlay), and every root within `MAX_TRANSIENT_DEPTH`
        /// links must be recognised (never drop a real dialog-of-dialog).
        #[test]
        fn prop_transient_ownership_matches_bounded_reachability(
            (parents, win, roots) in prop_transient_graph(),
        ) {
            let mut clients = std::collections::HashMap::new();
            for (i, &p) in parents.iter().enumerate() {
                let w = i as WindowId + 1;
                let mut c = Client::new(w, 0, 0);
                c.transient_parent = if p == 0 { None } else { Some(p) };
                clients.insert(w, c);
            }
            prop_assert_eq!(
                transient_chain_reaches(&clients, win, &roots),
                root_within_depth(&clients, win, &roots),
                "win={} roots={:?} parents={:?}",
                win,
                roots,
                parents
            );
        }

        /// A workarea change re-decides every float it visits, so the client's
        /// authority over its own rect cannot survive it. The contract is an
        /// equivalence rather than a case list: a rect is handed back exactly
        /// when the workarea wants it elsewhere *or* the client still held the
        /// seal, and whatever comes back is never degenerate and never escapes
        /// the workarea with its frame — a `ConfigureWindow` with a zero
        /// dimension is a `BadValue` the server drops, which leaves `Applied`
        /// permanently ahead of reality.
        #[test]
        fn prop_reclaim_re_decides_exactly_when_it_must(
            requested in prop_requested_rect(),
            already_settled in any::<bool>(),
            (wa_w, wa_h) in (64u32..=2048, 64u32..=2048),
            bw in 0u32..=8,
            (min_w, inc_w, min_h, inc_h) in (0i32..=200, 0i32..=16, 0i32..=200, 0i32..=16),
            authority in any::<bool>(),
        ) {
            let wa = Rect::new(0, 0, wa_w, wa_h);
            // A float the client has already put where the workarea wants it is
            // the only way the "nothing to decide" path is reachable — a random
            // request is essentially never already settled — and that path is
            // the one the seal exists for. It carries no size hints, so the
            // projection is a pure workarea clamp and the rect is a fixed point.
            let (geom, hints) = if already_settled {
                (
                    clamp_float_geom(requested, wa, bw),
                    SizeHints::default(),
                )
            } else {
                (
                    requested,
                    SizeHints {
                        min_w,
                        min_h,
                        inc_w,
                        inc_h,
                        valid: true,
                        ..SizeHints::default()
                    },
                )
            };
            let mut c = Client::new(1, 0, 0);
            c.flags.set(WinFlags::FLOAT);
            c.geom = geom;
            c.saved_geom = geom;
            c.hints = hints;
            c.border_w = bw;
            c.float_client_authority = authority;

            if let Some(settled) = reclaim_float_to_workarea(&mut c, wa) {
                prop_assert!(
                    !c.float_client_authority,
                    "the WM re-decided this rect, so the client is no longer its authority: {:?}",
                    settled
                );
                prop_assert!(
                    settled.w >= 1 && settled.h >= 1,
                    "a reclaimed float must never be 0x0: {:?}",
                    settled
                );
                let frame = 2 * bw as i32;
                prop_assert!(
                    settled.x >= wa.x
                        && settled.y >= wa.y
                        && settled.x + settled.w as i32 + frame <= wa.x + wa.w as i32
                        && settled.y + settled.h as i32 + frame <= wa.y + wa.h as i32,
                    "reclaimed float {:?} escapes the workarea {:?} with frame {}",
                    settled, wa, frame
                );
            } else {
                prop_assert!(
                    !authority,
                    "only an unsealed float can be left in silence: geom={:?} wa={:?}",
                    geom, wa
                );
                prop_assert_eq!(c.geom, geom, "a silent reclaim must not move the window");
            }
        }
    }

    /// A random `WM_TRANSIENT_FOR` graph: `parents[i]` is the parent of client
    /// `i + 1`, `0` meaning "no parent". Parent ids are drawn from the clients
    /// that actually exist, so chains reach the depth bound instead of dying on
    /// a dangling id; `0` still produces short chains, and a self-reference or
    /// a longer cycle is always available.
    fn prop_transient_graph() -> impl Strategy<Value = (Vec<u32>, u32, Vec<u32>)> {
        (1usize..=8).prop_flat_map(|n| {
            (
                proptest::collection::vec(0u32..=n as u32, n),
                1u32..=n as u32,
                // Roots may name windows that are not clients at all.
                proptest::collection::vec(1u32..=10, 0..=4),
            )
        })
    }

    /// Independent reference for the ownership question: a level-synchronous
    /// breadth-first walk over the parent graph, visited-set guarded so a cycle
    /// terminates, asking whether any root lies within `MAX_TRANSIENT_DEPTH`
    /// links of `win`. Derived from the documented contract, not from
    /// `transient_chain_reaches`'s loop.
    fn root_within_depth(
        clients: &std::collections::HashMap<WindowId, Client>,
        win: WindowId,
        roots: &[WindowId],
    ) -> bool {
        let mut frontier = vec![win];
        let mut seen: std::collections::HashSet<WindowId> = std::collections::HashSet::from([win]);
        for _ in 0..MAX_TRANSIENT_DEPTH {
            let mut next = Vec::new();
            for w in frontier {
                // A link naming a window that is no longer a client ends the
                // walk: a destroyed parent is always "no parent".
                let Some(parent) = clients.get(&w).and_then(|c| c.transient_parent) else {
                    continue;
                };
                if roots.contains(&parent) {
                    return true;
                }
                if seen.insert(parent) {
                    next.push(parent);
                }
            }
            if next.is_empty() {
                return false;
            }
            frontier = next;
        }
        false
    }

    proptest! {
        /// Render-list totality. For every monitor index — including the stale
        /// ones a hotplug leaves behind — one projection must complete without
        /// panicking, and it may only name windows the WM actually has a client
        /// for, each exactly once, with a rect the protocol accepts. A phantom
        /// window is a `ConfigureWindow` to an XID nobody manages, a repeated
        /// window is two configures fighting over one window in a single frame,
        /// and a zero-area rect is a `BadValue` the server silently drops,
        /// leaving `Applied` ahead of reality forever.
        #[test]
        fn prop_render_list_is_total_and_names_only_real_windows(
            screen in prop_output_rect(),
            workarea in prop_output_rect(),
            wins in proptest::collection::vec(prop_win(), 0..=8),
            cam in (-4000.0f32..4000.0, -4000.0f32..4000.0),
            zoom in (0.05f32..=2.0, 0.05f32..=2.0),
            page_zoom in (0.05f32..=2.0, 0.05f32..=2.0),
            overview in any::<bool>(),
            gaps in (0u32..=200, 0u32..=200),
            border in 0u32..=8,
            smart_gaps in any::<bool>(),
            boost in 0.0f32..=1.0,
        ) {
            let (state, cfg) = scenario(
                screen,
                workarea,
                wins,
                cam,
                zoom,
                page_zoom,
                overview,
                gaps,
                border,
                smart_gaps,
                boost,
            );
            let monitors = state.monitors.len();
            // One past the last index is the stale-index case `arrange` has to
            // absorb rather than panic on.
            for mon_idx in 0..=monitors {
                let Some((placements, _raise)) = projected(&state, &cfg, mon_idx) else {
                    prop_assert!(
                        mon_idx >= monitors,
                        "monitor {} exists but produced no render list",
                        mon_idx
                    );
                    continue;
                };
                let mut placed: Vec<WindowId> = Vec::new();
                for (win, rect, _bw) in &placements {
                    prop_assert!(
                        state.clients.contains_key(win),
                        "render list names window {:#x}, which has no client",
                        win
                    );
                    prop_assert!(
                        !placed.contains(win),
                        "window {:#x} appears twice in one render list",
                        win
                    );
                    placed.push(*win);
                    prop_assert!(
                        rect.w >= 1 && rect.h >= 1,
                        "degenerate rect for window {:#x} reaches X11 as BadValue: {:?}",
                        win,
                        rect
                    );
                }
            }
        }

        /// Render-list determinism. The projection is documented as a pure
        /// function of `State` + `Cfg` + `Phase` and is re-run for the same
        /// monitor several times per frame (once per animating monitor, plus
        /// the compositor's live pass). A render list whose order or contents
        /// varied between two identical runs would make the presentation
        /// overlay's `raise` order — and therefore the stacking the user sees —
        /// depend on hash iteration order.
        #[test]
        fn prop_render_list_is_deterministic(
            screen in prop_output_rect(),
            workarea in prop_output_rect(),
            wins in proptest::collection::vec(prop_win(), 0..=8),
            cam in (-4000.0f32..4000.0, -4000.0f32..4000.0),
            zoom in (0.05f32..=2.0, 0.05f32..=2.0),
            page_zoom in (0.05f32..=2.0, 0.05f32..=2.0),
            overview in any::<bool>(),
            gaps in (0u32..=200, 0u32..=200),
            border in 0u32..=8,
            smart_gaps in any::<bool>(),
            boost in 0.0f32..=1.0,
        ) {
            let (state, cfg) = scenario(
                screen,
                workarea,
                wins,
                cam,
                zoom,
                page_zoom,
                overview,
                gaps,
                border,
                smart_gaps,
                boost,
            );
            prop_assert_eq!(
                projected(&state, &cfg, 0),
                projected(&state, &cfg, 0),
                "the render list must be a pure function of State + Cfg"
            );
        }

        /// Render-list convergence under the pipeline's own write-back. A
        /// `ConfigureWindow` is echoed back as a `ConfigureNotify` and
        /// `emit_geometry` writes the applied rect and border into the client,
        /// so the next cycle starts from the geometry the WM itself just asked
        /// for. The projection has to want exactly that back: the second cycle
        /// must emit nothing. Re-asking for a rect it was just given is a
        /// permanent configure storm on the X server, and a toolkit that
        /// answers every configure with a `ConfigureRequest` turns the storm
        /// into a visible fight.
        #[test]
        fn prop_applied_geometry_is_what_the_next_cycle_wants(
            screen in prop_output_rect(),
            workarea in prop_output_rect(),
            wins in proptest::collection::vec(prop_win(), 0..=8),
            cam in (-4000.0f32..4000.0, -4000.0f32..4000.0),
            zoom in (0.05f32..=2.0, 0.05f32..=2.0),
            page_zoom in (0.05f32..=2.0, 0.05f32..=2.0),
            overview in any::<bool>(),
            gaps in (0u32..=200, 0u32..=200),
            border in 0u32..=8,
            smart_gaps in any::<bool>(),
            boost in 0.0f32..=1.0,
        ) {
            let (mut state, cfg) = scenario(
                screen,
                workarea,
                wins,
                cam,
                zoom,
                page_zoom,
                overview,
                gaps,
                border,
                smart_gaps,
                boost,
            );
            let mut applied = AppliedState::default();
            let first = projected(&state, &cfg, 0);
            let placed = first.as_ref().map_or(0, |(p, _)| p.len());
            let emitted = first.as_ref().map(|(placements, raise)| {
                let desired = DesiredState::from_placements(placements, raise);
                configures(reconcile(&desired, &state, &mut applied))
            });
            // Nothing is lost on the way there either: a window X11 has never
            // been told about is configured exactly once, so the second cycle
            // has nothing to redo.
            prop_assert_eq!(
                emitted.as_ref().map_or(0, Vec::len),
                placed,
                "every placed window is configured exactly once on the first cycle"
            );

            // The write-back `emit_geometry` performs once the requests are on
            // the wire: the client now really has this rect and this border.
            if let Some((placements, _raise)) = &first {
                for (win, rect, bw) in placements {
                    if let Some(c) = state.clients.get_mut(win) {
                        c.geom = *rect;
                        c.border_w = *bw;
                        c.last_reported = Some(*rect);
                    }
                }
            }
            let reemitted = projected(&state, &cfg, 0).map(|(placements, raise)| {
                let desired = DesiredState::from_placements(&placements, &raise);
                configures(reconcile(&desired, &state, &mut applied))
            });
            prop_assert!(
                reemitted.as_ref().is_none_or(Vec::is_empty),
                "the projection must want back the geometry it just applied, got {:?}",
                reemitted
            );
        }
    }
}
/// Contract, reduced to a deterministic set: every row of a rounded mask
/// leaves the same inset on the left as on the right, for every radius a
/// caller can pass — including the radius boundary `2 * r == w`.
///
/// At that boundary the corner circle is tangent to the frame and the row has
/// no area of its own. It must not be dropped (that clips the frame's top and
/// bottom edge), and it must not be floored to a single pixel either: an even
/// width has no centre pixel — its centre is the boundary between two columns
/// — so a one-pixel row can never be symmetric about it, and the mask then
/// eats one side of the frame. Odd widths have a centre pixel but can never
/// reach the boundary at all (`2 * r` is even), so both parities are pinned
/// here.
///
/// Reachable whenever `corner_radius` equals half a window's width --
/// `corner_radius 10` on a 20px-wide window, `corner_radius 69` on a 138px
/// one. The `rounded_mask_is_horizontally_symmetric` example uses a radius a
/// quarter of the width, so it never reaches the boundary.
#[test]
fn every_rounded_mask_row_stays_symmetric_at_the_radius_boundary() {
    for &(w, h) in &[(138, 138), (20, 20), (401, 300), (19, 21), (2, 10), (1, 9)] {
        // Inside the boundary, on it, and past it (which the radius clamp
        // absorbs), plus the two degenerate radii that bypass the arc entirely.
        for r in [0, 1, w / 2 - 1, w / 2, w / 2 + 1, w * 2] {
            let rects = rounded_rectangles(w, h, r);
            for row in &rects {
                let left = i32::from(row.x);
                let right_edge = left + i32::from(row.width);
                assert_eq!(
                    left,
                    w - right_edge,
                    "row y={} of w={w} h={h} r={r} leaves unequal insets",
                    row.y
                );
                assert!(
                    i32::from(row.width) >= 1 && i32::from(row.height) >= 1,
                    "a mask rectangle must not be zero-area: {row:?} for w={w} h={h} r={r}"
                );
            }
        }
    }
}

/// The tangent row is the frame's own top and bottom edge when `2 * r == w`,
/// so the mask has to cover it — dropping it would clip the corners away and
/// leave the window with square edges at exactly the rows the radius was meant
/// to round.
#[test]
fn tangent_row_is_covered_when_the_radius_is_half_the_width() {
    for &(w, h) in &[(20, 20), (138, 138), (2, 10)] {
        let rects = rounded_rectangles(w, h, w / 2);
        for edge in [0, h - 1] {
            let row = rects
                .iter()
                .find(|row| i32::from(row.y) == edge)
                .unwrap_or_else(|| panic!("no mask row at y={edge} for w={w} h={h}"));
            assert!(
                i32::from(row.width) >= 1,
                "the tangent row at y={edge} must not be empty: {row:?} for w={w} h={h}"
            );
            assert!(
                i32::from(row.x) + i32::from(row.width) <= w,
                "the tangent row at y={edge} must stay inside the frame: {row:?}"
            );
        }
    }
}
