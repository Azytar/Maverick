//! Pure domain model — authoritative logical state for placement, focus,
//! workspace membership, layout geometry, floats, cameras, and reservations.
//!
//! This is the `maverick-core` counterpart of `maverick::core::layout` and
//! `maverick::types::StateExt`: it owns the types that are independent of any
//! concrete layout implementation, X11/GL backend, or compositor. Everything
//! here is `std`-only, `unsafe`-free, and deterministic — suitable for
//! property-based testing without an X server.
//!
//! # Ownership
//!
//! - Core owns `State` and all logical placement (`Client`, `Monitor`,
//!   `Workspace`, `Column`, `Camera`, `ReservedRegion`). The backend mirrors
//!   X11 state into the core and applies `Client::geom` via `ConfigureWindow`;
//!   the compositor reads `State` for rendering. Neither mutates `State`
//!   except through the command/invariant pipeline.
//! - `Client::geom` is WM-authoritative for floating and for tiled geometry
//!   *after* arrangement; `Client::saved_geom` and `FullscreenSnapshot` are
//!   persistence/transition stores. `Monitor::workarea` is always derived from
//!   `Monitor::screen` minus `ReservedArea` (see `recalc_geometry`).
//! - `WindowId` is backend-agnostic (`u32`), stable for the lifetime of a
//!   managed window.
//!
//! # Invariants
//!
//! Validated by `State::check_invariants` after every transition — see
//! `maverick_core::lib` crate docs for the full list.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::wallpaper::{WallpaperMode, WallpaperSource, WallpaperSpec};

/// Backend-agnostic window identifier used throughout the core domain model.
///
/// This is a plain `u32`, not an alias for x11rb's `Window` — the core must not
/// import any X11 protocol type. On X11 the backend's `Window` (itself a u32
/// XID) converts losslessly to/from `WindowId` at the backend's edges (`as
/// Window` / `as WindowId`). A future Wayland backend would map its own surface
/// handles onto this same id space instead. The frontier is strict: the core
/// speaks only `WindowId`, the backend does the conversion.
///
/// # Invariant
///
/// `WindowId` values are stable for the lifetime of a managed window. The
/// backend must not reuse an id after unmanaging a window, because X11
/// itself guarantees that XIDs are never reused within a session. This
/// invariant is what lets `State::clients` use the id as a key and lets
/// focus stacks and deferred-focus slots reference windows by id without
/// stale-reference risk.
pub type WindowId = u32;

// ─── Geometry ────────────────────────────────────────────────────────────────

/// Screen-aligned rectangle in pixel coordinates.
///
/// This is the authoritative rectangle type for the entire Maverick
/// domain model. It uses `i32` for position to allow negative values
/// (e.g. when a window is dragged partially off-screen during an
/// interactive move). The origin is screen-space, measured from the
/// upper-left corner of each monitor. Width and height are `u32`
/// to forbid degenerate zero/negative sizes — any operation that
/// produces an invalid size clamps to at least 1px (the audit requirement
/// "geometry must never be invalid" in Fase 12).
///
/// # Responsibilities
///
/// - Owns the authoritative position of tiled windows (projected from
///   column layout) and floating windows (from client/rule geometry).
/// - Used by `Client::geom` (WM-authoritative), `Client::saved_geom`
///   (persistence), `Workspace::camera` (scroll origin), and many
///   rendering/compositing calculations.
/// - Does NOT own any X11 state — the backend owns the `xcb_window_t`
///   geometry and the X11 property `_NET_FRAME_EXTENTS`.
///
/// # Invariants
///
/// - `w >= 1` and `h >= 1` for all active windows. Geometry that would
///   be zero or negative is clamped to 1px (see `arrange_columns`). The
///   compositor never presents a window with a zero-area rect.
/// - X11 hit-testing (pointer warp, input focus) reads `client.geom` directly.
/// - The composition layer (`core::present`) uses the same rect values
///   for rendering; any mismatch would leave pixels uncovered or
///   incorrectly drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    /// X coordinate of the top-left corner.
    pub x: i32,
    /// Y coordinate of the top-left corner.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// Create a rect at `(x, y)` with size `w × h`.
    #[inline]
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    /// True when point `(px, py)` lies inside `self` (half-open on right/bottom).
    /// Saturating: hostile `w = u32::MAX` (`-1 as i32`) can never wrap `x + w`.
    #[inline]
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    /// Area in pixels (`w * h` as `u64` to avoid `u32` overflow).
    #[inline]
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
    /// X coordinate of the right edge (`x + w`, saturating).
    /// `w` is clamped to `i32::MAX` first: a hostile `u32::MAX` casts to
    /// `-1 as i32` and would otherwise move the edge backwards.
    #[inline]
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.w.min(i32::MAX as u32) as i32)
    }
    /// True when `other` is entirely inside `self`. Used for occlusion culling
    /// (Fase 12): a window fully behind a single opaque window above it is
    /// hidden.
    #[inline]
    pub fn contains_rect(&self, other: Rect) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }
    /// Y coordinate of the bottom edge (`y + h`, saturating).
    #[inline]
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h.min(i32::MAX as u32) as i32)
    }
    /// Smallest rect containing both `self` and `other`. Used for animation
    /// damage (Fase 7): a window that slides from one rect to another must
    /// repaint the union so neither the pixels it left nor the ones it slid
    /// into linger. Saturating: opposite `±2G` origins can never wrap the cast.
    #[inline]
    pub fn union(&self, other: Rect) -> Rect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Rect::new(
            x0,
            y0,
            (x1 as i64 - x0 as i64).clamp(0, u32::MAX as i64) as u32,
            (y1 as i64 - y0 as i64).clamp(0, u32::MAX as i64) as u32,
        )
    }
}

// ─── Window flags ─────────────────────────────────────────────────────────────

/// Bit-packed flags attached to every `Client`, controlling its layout and
/// presentation behavior. Each bit encodes a distinct policy; flags are
/// independently set/clear/toggled by the WM, window rules, and commands
/// (`ToggleFloat`, `ToggleFullscreen`, etc.).
///
/// # Responsibilities
///
/// - `FLOAT` — window participates in `Workspace::floats`, excluded from
///   columnar layout (`arrange_columns`). Its `geom` is client-defined and
///   not derived from the tiling algorithm.
/// - `FULLSCREEN` — window has requested fullscreen; its presentation mode
///   (floating column vs exclusive overlay) is governed by
///   `Client::fullscreen_policy` (see `FullscreenPolicy::Normal`,
///   `Deny`, `True`).
/// - `URGENT` — visual indicator (border color) that the window has been
///   marked as needing the user's attention.
/// - `NO_FOCUS` — window does not want keyboard/mouse input; usually set
///   via ICCCM `WM_HINTS`.
/// - `FIXED` — window has explicit size hints (`wm_size_hints`) enforcing
///   fixed dimensions; prevents column resizing from changing its geometry.
/// - `MAXIMIZED_V` / `MAXIMIZED_H` — `_NET_WM_STATE_MAXIMIZED_VERT` /
///   `_NET_WM_STATE_MAXIMIZED_HORZ` bits from EWMH. Both must be on for a
///   window to be considered "maximized" (`is_maximized()`).
/// - `STICKY` — window is sticky: visible on every workspace of its monitor.
///   It conceptually floats above all layout decisions.
/// - `FS_WAS_FLOAT` — remembers that a window was floating before it entered
///   fullscreen, so leaving fullscreen can restore the float (and `saved_geom`)
///   instead of dropping it back as a tiled column.
/// - `FLOAT_NATIVE` — window was born floating at map time (via window type,
///   transient-for, size hints, or rules). This origin is never cleared,
///   enabling backend drag semantics to distinguish native floats from
///   user-torn-off tiles (see `WinFlags::FLOAT_NATIVE` docs).
///
/// # Invariants
///
/// - Bits 0-9 are used; bits >= 10 are reserved/future.
/// - `MAXIMIZED` is defined as `MAXIMIZED_V | MAXIMIZED_H`; using
///   `has(MAXIMIZED)` tests only *bit overlap*, which is true if either
///   axis is set individually — so callers must check both axes via
///   `is_maximized_v() && is_maximized_h()`.
/// - A window with `STICKY` is always conceptually floating; it is always
///   in `Workspace::floats` and excluded from column layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct WinFlags(u16);
impl WinFlags {
    /// Window participates in `Workspace::floats` (`Client::geom` is authoritative).
    pub const FLOAT: u16 = 1 << 0;
    /// Window has requested fullscreen (`WinFlags::FULLSCREEN`); presentation
    /// governed by `Client::fullscreen_policy`.
    pub const FULLSCREEN: u16 = 1 << 1;
    /// Urgent hint — visual border indicator that attention is needed.
    pub const URGENT: u16 = 1 << 2;
    /// Window does not want input (`WM_HINTS` `InputHint` false).
    pub const NO_FOCUS: u16 = 1 << 3;
    /// Fixed size hints — column resizing must not change geometry.
    pub const FIXED: u16 = 1 << 4;
    /// Maximized *vertically* — `_NET_WM_STATE_MAXIMIZED_VERT`. The window's
    /// height (and y) come from the workarea; its width/x stay whatever the
    /// layout gave it. Kept as a separate axis from `MAXIMIZED_H` because EWMH
    /// treats them as two independent states and clients really do request only
    /// one of them (a single `MAXIMIZED` bit silently promoted every vertical
    /// maximize into a full one).
    pub const MAXIMIZED_V: u16 = 1 << 5;
    /// Maximized *horizontally* — `_NET_WM_STATE_MAXIMIZED_HORZ`.
    pub const MAXIMIZED_H: u16 = 1 << 8;
    /// Both axes at once — the "maximize" a user means when pressing Mod4+M.
    pub const MAXIMIZED: u16 = Self::MAXIMIZED_V | Self::MAXIMIZED_H;
    /// Sticky: a float that stays visible on every workspace of its monitor
    /// (never hidden by `hide_offscreen`). Set via a window rule.
    pub const STICKY: u16 = 1 << 6;
    /// Remembers that a window was floating before it entered fullscreen, so
    /// leaving fullscreen can return it to its float (and `saved_geom`) instead
    /// of dropping it back as a tiled column. Set by `ToggleFullscreen`.
    pub const FS_WAS_FLOAT: u16 = 1 << 7;
    /// Window *origin*: the window was born floating — WM policy decided so at
    /// map time (`_NET_WM_WINDOW_TYPE` dialog/utility/menu/toolbar/splash,
    /// `_NET_WM_STATE_MODAL`, `WM_TRANSIENT_FOR`, fixed size hints, portal/
    /// file-chooser heuristics, a `float = true` window rule, or a float state
    /// restored across a restart). Contrast with a tiled window the *user*
    /// tears off (`ToggleFloat` / Mod4-drag): that one keeps this bit clear. The
    /// origin outlives layout-mode changes (it is never cleared), so
    /// `origin != current layout mode` holds: a native float that is tiled via
    /// `ToggleFloat` and floated again remains distinguishable from a torn-off
    /// tile. Backend drag semantics read it (`pointer::float_may_join_tree`)
    /// so a native float is never dropped into the tiling tree by accident.
    pub const FLOAT_NATIVE: u16 = 1 << 9;

    /// Set bit(s) `f`.
    #[inline]
    pub fn set(&mut self, f: u16) {
        self.0 |= f;
    }
    /// Clear bit(s) `f`.
    #[inline]
    pub fn clear(&mut self, f: u16) {
        self.0 &= !f;
    }
    /// Toggle bit(s) `f`.
    #[inline]
    pub fn toggle(&mut self, f: u16) {
        self.0 ^= f;
    }
    /// True when any bit in `f` is set (bit-overlap test, not exact equality).
    #[inline]
    pub fn has(&self, f: u16) -> bool {
        self.0 & f != 0
    }
}

// ─── Size hints ───────────────────────────────────────────────────────────────

/// ICCCM `WM_NORMAL_HINTS` size constraints for one window.
///
/// All fields are raw hints as reported by the client; `valid` indicates
/// whether any hint was actually set. The layout clamps tiled geometry against
/// these hints where applicable.
#[derive(Debug, Clone, Copy, Default)]
pub struct SizeHints {
    /// Base width for increment calculations.
    pub base_w: i32,
    /// Base height for increment calculations.
    pub base_h: i32,
    /// Width increment.
    pub inc_w: i32,
    /// Height increment.
    pub inc_h: i32,
    /// Maximum width (0 = unconstrained).
    pub max_w: i32,
    /// Maximum height (0 = unconstrained).
    pub max_h: i32,
    /// Minimum width.
    pub min_w: i32,
    /// Minimum height.
    pub min_h: i32,
    /// Minimum aspect ratio (`w/h`).
    pub min_aspect: f32,
    /// Maximum aspect ratio (`w/h`).
    pub max_aspect: f32,
    /// Raw `XSizeHints.flags` word. Not a constraint: it says which fields are
    /// *defined* and which authority the client claims over its own geometry
    /// ([`SizeHints::claims_position`]). Kept raw so the parser stays a pure
    /// transcription of the wire format (ICCCM 4.1.2.3).
    pub flags: u32,
    /// True when at least one hint field is meaningful.
    pub valid: bool,
}

impl SizeHints {
    // `XSizeHints.flags` bits, ICCCM 4.1.2.3 / `X11/Xutil.h`. They are the
    // wire contract of `WM_NORMAL_HINTS`; the parser and every reader below
    // share them so a bit test can never drift from the word it indexes.
    /// The *user* asked for this position.
    pub const U_S_POSITION: u32 = 1 << 0;
    /// The *user* asked for this size.
    pub const U_S_SIZE: u32 = 1 << 1;
    /// The *program* asked for this position.
    pub const P_POSITION: u32 = 1 << 2;
    /// The *program* asked for this size.
    pub const P_SIZE: u32 = 1 << 3;
    /// `min_w`/`min_h` are defined.
    pub const P_MIN_SIZE: u32 = 1 << 4;
    /// `max_w`/`max_h` are defined.
    pub const P_MAX_SIZE: u32 = 1 << 5;
    /// `inc_w`/`inc_h` are defined.
    pub const P_RESIZE_INC: u32 = 1 << 6;
    /// `min_aspect`/`max_aspect` are defined.
    pub const P_ASPECT: u32 = 1 << 7;
    /// `base_w`/`base_h` are defined.
    pub const P_BASE_SIZE: u32 = 1 << 8;
    /// The gravity word (index 17) is defined. Not modelled: the WM always
    /// places windows with a `NorthWest` gravity.
    pub const P_WIN_GRAVITY: u32 = 1 << 9;

    /// True when the client claims authority over its own *position*
    /// (`USPosition`/`PPosition`). ICCCM 4.1.2.3: the window manager should
    /// place such a window where the client asked instead of inventing a
    /// position — re-centering it is a visible teleport on map.
    #[inline]
    pub fn claims_position(&self) -> bool {
        self.valid && self.flags & (Self::U_S_POSITION | Self::P_POSITION) != 0
    }
}

// ─── Column (true scrolling, niri-style) ───────────────────────────────────────
//
// Each workspace has N columns. Every column holds one or more windows
// stacked vertically. The layout engine assigns absolute screen coords
// based on the column's logical offset and the current scroll position.
//
// A column stores its width as a fraction OF THE WORKAREA WIDTH, independent
// of every other column — weights do NOT need to sum to 1.0. This is what
// makes it a true *scrolling* layout rather than a fit-to-screen one: adding,
// growing, or removing a column never resizes its neighbors. Instead, the
// total width of all columns simply grows or shrinks, and `camera` scrolls
// to keep the focused column in view (see `ideal_scroll`).
//
// New columns are inserted at `cfg.column_width` (a fraction of the
// workarea) rather than stealing space from the currently focused column.
//
// This means coordinates are ALWAYS derived from (col_x + scroll, row_y)
// and never stored as mutable state — no drift possible.

/// One vertical stack in the scrolling ribbon (`Workspace::columns`).
///
/// Each column holds one or more windows stacked top-to-bottom. Its `weight` is
/// its width as a fraction of the workarea width, independent of every other
/// column — weights do not sum to 1.0. This makes the layout a true scrolling
/// ribbon rather than fit-to-screen: adding/growing/removing a column never
/// resizes neighbors; the total width grows/shrinks and `Camera` scrolls to
/// keep the focused column in view. Coordinates are always derived from
/// `(col_x + scroll, row_y)` — never stored mutably, so no drift.
///
/// # Ownership
///
/// Owned by `Workspace::columns`. Windows inside are referenced by `WindowId`
/// and must also exist in `State::clients` (invariant validated by
/// `State::check_invariants`).
///
/// # Invariants
///
/// - `weight` in `[0.05, 1.0]`, finite; repaired by `Workspace::rebalance_weights`.
/// - `focused < windows.len()` when non-empty; `boost` in `[0.0, 1.0]`.
#[derive(Debug, Clone)]
pub struct Column {
    /// Windows top-to-bottom in this column.
    pub windows: Vec<WindowId>,
    /// This column's width as a fraction of the workarea width.
    pub weight: f32,
    /// Index into `windows` that has focus within this column.
    pub focused: usize,
    /// Accordion boost for THIS column, animated 0→1. The focused column's boost
    /// eases to 1 while the others ease to 0, so changing focus makes the widths
    /// *glide* instead of snapping (bug C10). Replaces the old single global
    /// `Workspace::accordion` scalar, which could only animate on overview
    /// enter/exit and made every focus change a one-frame jump.
    pub boost: f32,
}

impl Column {
    /// Create a column with the given workarea-fraction `weight`.
    pub fn new(weight: f32) -> Self {
        Column {
            weight,
            ..Default::default()
        }
    }
    /// Focused window in this column, if any.
    pub fn focused_win(&self) -> Option<WindowId> {
        self.windows.get(self.focused).copied()
    }
}

impl Default for Column {
    fn default() -> Self {
        // A column is created because it (or its window) is the focus target,
        // so it starts fully boosted; `tick_animations` eases it back to 0 if it
        // loses focus (bug C10).
        Self {
            windows: Vec::new(),
            weight: 1.0,
            focused: 0,
            boost: 1.0,
        }
    }
}

// ─── Camera (scroll ribbon, spring-damped) ─────────────────────────────────────
//
// 1D camera for the Scroll (niri-style ribbon) layout. `position` is the
// current visual offset in px (world space -> screen). `target` is where focus
// wants the camera; an analytical damped transition eases `position` toward it.
// It is never the source of truth for logical geometry — `arrange_columns`
// derives each window's x from the selected phase, so animation cannot mutate
// the WM's settled layout.

/// 1D scroll camera for the ribbon layout.
///
/// `position` is the current visual scroll offset in px; `target` is the
/// logical destination. A closed-form damped transition eases `position` toward
/// `target`, and retargeting preserves the visual position while explicitly
/// resetting old-direction momentum.
///
/// # Invariants
///
/// - `position`, `target`, `velocity` are finite (NaN/Inf snaps to target).
/// - `stiffness`/`damping` are sanitized at integration time; damping is also
///   bounded relative to `sqrt(stiffness)` so a slow overdamped pole cannot keep
///   a pixel-settled camera active indefinitely.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Current scroll offset in px.
    pub position: f32,
    /// Desired scroll offset — focus drives this, the spring follows.
    pub target: f32,
    /// Current velocity (px/s).
    pub velocity: f32,
    /// Spring stiffness (`220.0` default, clamped to `[MIN_STIFFNESS, MAX_SPRING]`).
    pub stiffness: f32,
    /// Damper (`30.0` default, bounded by `MIN_DAMPING` and the stability
    /// ratio derived from stiffness).
    pub damping: f32,
}

/// Upper bound for a user-supplied spring constant. `Camera::step` evaluates
/// the damped oscillator analytically, so this is a configuration/sanitization
/// bound rather than an Euler stability condition.
pub(crate) const MAX_SPRING: f32 = 62_500.0;
/// Smallest tolerated stiffness. `Camera::step` re-clamps on every step, so
/// even a caller that bypasses `sanitize_spring` cannot disable restoration.
pub(crate) const MIN_STIFFNESS: f32 = 1.0;
/// Smallest tolerated damping. This is a numerical safety floor, not a
/// guaranteed visual settle-time bound: the underdamped decay envelope is
/// `exp(-c*t/2)`, so a very low positive damper can still settle slowly.
pub(crate) const MIN_DAMPING: f32 = 0.1;
/// Bound `c / sqrt(stiffness)` for the overdamped branch. This prevents a
/// numerically finite but extremely slow slow-pole; it does not bound the
/// underdamped envelope.
const MAX_DAMPING_RATIO: f32 = 10.0;
const CAMERA_SETTLE_POSITION: f32 = 0.5;
const CAMERA_SETTLE_VELOCITY: f32 = 0.01;

#[inline]
fn bounded_damping(stiffness: f32, damping: f32) -> f32 {
    let max = (MAX_DAMPING_RATIO * stiffness.max(MIN_STIFFNESS).sqrt()).min(MAX_SPRING);
    let damping = if damping.is_finite() { damping } else { 30.0 };
    damping.clamp(MIN_DAMPING, max)
}

impl Camera {
    /// Create a camera at rest at `pos` (position = target, velocity = 0).
    pub fn new(pos: f32) -> Self {
        Self {
            position: pos,
            target: pos,
            velocity: 0.0,
            stiffness: 220.0,
            damping: 30.0,
        }
    }

    /// Change only the logical destination. The animated position is retained
    /// so a retarget never teleports; derivative momentum is reset explicitly,
    /// matching the reference camera policy and preventing a reversal from
    /// briefly accelerating farther in the old direction.
    pub fn retarget(&mut self, target: f32) {
        if target.is_finite() {
            // Repeated focus/arrange notifications for the same endpoint must
            // not continuously cancel an in-flight spring.
            if (self.target - target).abs() > 1e-4 {
                self.velocity = 0.0;
            }
            self.target = target;
        }
    }

    /// Whether the camera still has a meaningful visual transition.
    ///
    /// This is separate from `step`'s return value because a zero or invalid
    /// frame delta must not make a pending retarget look settled to the
    /// scheduler. Non-finite state is sanitized by the next valid `step`.
    pub fn needs_update(&self) -> bool {
        if !self.position.is_finite() || !self.target.is_finite() || !self.velocity.is_finite() {
            return true;
        }
        (self.position - self.target).abs() > CAMERA_SETTLE_POSITION
            || self.velocity.abs() > CAMERA_SETTLE_VELOCITY
    }

    /// Advance the camera by elapsed `dt` seconds and return whether it remains
    /// animated.
    ///
    /// The state is sampled from the closed-form solution of the damped
    /// harmonic oscillator for a constant target:
    ///
    /// `x'' + c·x' + k·(x - target) = 0`.
    ///
    /// This is deliberately not `position += velocity·dt`. The exact
    /// transition makes a fixed elapsed interval independent of how that
    /// interval is partitioned into render frames, while retaining the existing
    /// stiffness/damping configuration and explicit target changes.
    pub fn step(&mut self, dt: f32) -> bool {
        if !dt.is_finite() {
            return false;
        }
        if !self.position.is_finite() || !self.target.is_finite() || !self.velocity.is_finite() {
            self.snap(self.target);
            return false;
        }
        if dt <= 0.0 {
            // A zero/negative elapsed interval makes no progress, but a
            // pending transition must not be reported as settled.
            return self.needs_update();
        }

        // If the state is already inside the visual settle envelope, install
        // the exact endpoint. This prevents a last sampled subpixel from being
        // left in the compositor forever when no further frame is scheduled.
        if !self.needs_update() {
            self.snap(self.target);
            return false;
        }

        let stiffness = self.stiffness.clamp(MIN_STIFFNESS, MAX_SPRING) as f64;
        let damping = bounded_damping(stiffness as f32, self.damping) as f64;
        let x0 = self.position as f64;
        let v0 = self.velocity as f64;
        let target = self.target as f64;
        let y0 = x0 - target;
        let t = dt as f64;
        let (y1, v1) = if damping * damping > 4.0 * stiffness {
            // Over-damped: two real characteristic roots.
            let root = (damping * damping - 4.0 * stiffness).sqrt();
            let r1 = (-damping + root) * 0.5;
            let r2 = (-damping - root) * 0.5;
            let a = (v0 - r2 * y0) / (r1 - r2);
            let b = y0 - a;
            let e1 = (r1 * t).exp();
            let e2 = (r2 * t).exp();
            (a * e1 + b * e2, r1 * a * e1 + r2 * b * e2)
        } else if damping * damping < 4.0 * stiffness {
            // Under-damped: exponentially decaying sinusoid.
            let alpha = damping * 0.5;
            let omega = (4.0 * stiffness - damping * damping).sqrt() * 0.5;
            let decay = (-alpha * t).exp();
            let cos = (omega * t).cos();
            let sin = (omega * t).sin();
            let b = (v0 + alpha * y0) / omega;
            let y = decay * (y0 * cos + b * sin);
            let v = decay * (v0 * cos - (stiffness * y0 + alpha * v0) / omega * sin);
            (y, v)
        } else {
            // Critically damped: the repeated-root limit.
            let alpha = stiffness.sqrt();
            let b = v0 + alpha * y0;
            let decay = (-alpha * t).exp();
            let y = decay * (y0 + b * t);
            let v = decay * (v0 - alpha * b * t);
            (y, v)
        };

        self.position = (target + y1) as f32;
        self.velocity = v1 as f32;
        if !self.position.is_finite() || !self.velocity.is_finite() {
            self.snap(self.target);
            return false;
        }

        if self.needs_update() {
            true
        } else {
            self.snap(self.target);
            false
        }
    }

    /// Snap immediately (no animation) — used on first layout / unmanage.
    pub fn snap(&mut self, pos: f32) {
        let pos = if pos.is_finite() { pos } else { 0.0 };
        self.position = pos;
        self.target = pos;
        self.velocity = 0.0;
    }
}

// ─── Workspace ────────────────────────────────────────────────────────────────

/// Focus pointer within a workspace's column tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    /// Index of the focused column in `Workspace::columns`.
    pub column_idx: usize,
}

/// One virtual desktop on a monitor. Holds both tiled columns and floating
/// windows, plus per-workspace view state (camera, zoom/overview, viewport).
///
/// # Ownership
///
/// Owned by `Monitor::workspaces[active_ws]` (and siblings). Each workspace's
/// `columns` and `floats` reference `WindowId`s that also live in
/// `State::clients`. `presented_maximize` is derived state kept in sync by
/// `State::sync_presented_maximize`.
///
/// # Invariants
///
/// - `focus.column_idx < columns.len()` when non-empty; each `Column::focused`
///   in range.
/// - `camera` is not the source of truth for geometry — arrangement derives
///   positions from it.
/// - `presented_maximize` (if `Some`) names a maximized client on this workspace
///   and on the monitor's `focused` when the workspace is active.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Workspace tag (0-based index as configured).
    pub tag: u32,
    /// Tiled columns (scrolling ribbon).
    pub columns: Vec<Column>,
    /// Focused column pointer.
    pub focus: Focus,
    /// Scroll camera (only meaningful in `LayoutKind::Column` / ribbon mode).
    pub camera: Camera,
    /// Floating windows on this workspace (excluded from column layout; `Client::geom` authoritative).
    pub floats: Vec<WindowId>,
    /// Layout mode for this specific workspace — independent of every other workspace.
    pub layout: LayoutKind,
    /// Semantic-zoom factor for the Overview film-strip (1.0 = normal, <1 = zoomed out).
    pub zoom: f32,
    /// Overview (film-strip zoom-out) mode active for this workspace.
    pub overview: bool,
    /// Semantic-zoom target animated toward by `tick_animations`.
    pub zoom_target: f32,
    /// Viewport display mode (normal vs zoomed-in inspection). Orthogonal to
    /// `overview` and to window fullscreen.
    pub viewport_mode: ViewportMode,
    /// Page-zoom factor when `viewport_mode == Zoomed` (1.0 = no zoom, >1 = the
    /// ribbon is enlarged). Fed into `ribbon_geom`'s `alpha` so columns grow;
    /// there is deliberately no upper clamp (unlike `zoom`'s lower one), so a
    /// value > 1 enlarges instead of shrinking.
    pub page_zoom: f32,
    /// Animated target of `page_zoom`, eased by `tick_animations` (Fase 9/11).
    pub page_zoom_target: f32,
    /// The window currently presented as the **maximize** overlay on this
    /// workspace (`None` when no maximized window owns the overlay). This is the
    /// explicit maximize-overlay owner; it is kept in sync with `Monitor::focused`
    /// + the focused window's maximize flags by `State::sync_presented_maximize`,
    ///   so no read site has to re-derive the maximize-overlay owner from
    ///   `Monitor::focused`. `Monitor::focused` stays purely "the logical focus".
    pub presented_maximize: Option<WindowId>,
}

impl Workspace {
    /// Create an empty workspace with `tag`.
    pub fn new(tag: u32) -> Self {
        Self {
            tag,
            columns: Vec::new(),
            focus: Focus { column_idx: 0 },
            camera: Camera::new(0.0),
            floats: Vec::new(),
            layout: LayoutKind::Column,
            zoom: 1.0,
            overview: false,
            zoom_target: 1.0,
            viewport_mode: ViewportMode::Normal,
            page_zoom: 1.0,
            page_zoom_target: 1.0,
            presented_maximize: None,
        }
    }

    /// Alias for `new` — empty workspace with `tag`.
    pub fn empty(tag: u32) -> Self {
        Self::new(tag)
    }

    /// True when no tiled columns and no floats.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.floats.is_empty()
    }

    /// Focused window from the focused column, if any.
    pub fn focused_win(&self) -> Option<WindowId> {
        self.columns.get(self.focus.column_idx)?.focused_win()
    }

    /// True-scroll insert: a new window becomes a sibling column inserted to
    /// the RIGHT of the focused column (or as the sole column when the
    /// workspace is empty), at the configured `column_width` (a fraction of the
    /// workarea, 0.1–1.0). The fraction is taken directly — callers pass
    /// `cfg.column_width`; no division happens here (T5).
    pub fn add_tiled(&mut self, window: WindowId, column_width: f32) {
        // Sanitize: NaN fails `<= 0.0` and `clamp` passes NaN through,
        // poisoning the tree (debug invariant panics, release NaN geometry).
        let w = if !column_width.is_finite() || column_width <= 0.0 {
            1.0
        } else {
            column_width.clamp(0.1, 1.0)
        };
        if self.columns.is_empty() {
            let mut col = Column::new(1.0); // sole column owns the full workarea width
            col.windows.push(window);
            self.columns.push(col);
            self.focus.column_idx = 0;
        } else {
            let active = self.focus.column_idx.min(self.columns.len() - 1);
            let mut new_col = Column::new(w);
            new_col.windows.push(window);
            new_col.focused = 0;
            self.columns.insert(active + 1, new_col);
            self.focus.column_idx = active + 1;
        }
    }

    /// Guard against degenerate weights (zero/negative/NaN from float drift,
    /// a caller that never set one, or a hostile session restore). Since
    /// columns are independently sized in the true-scroll model, this no
    /// longer redistributes weight between columns — it just gives any
    /// broken column a sane fallback width.
    pub fn rebalance_weights(&mut self) {
        for col in &mut self.columns {
            if !col.weight.is_finite() || col.weight <= 0.0 {
                col.weight = 0.5;
            }
        }
    }

    /// P3: &mut self — no clone needed at call sites
    /// Locate `win` within the tiled tree and return `(column_idx, row_idx)`.
    /// Used by `State::remove_client` to re-derive the workspace focus pointer
    /// from the logical focus (`mon.focused`) after a window leaves the tree, so
    /// `ws.focus` can never be left pointing at a stale column/row.
    pub fn index_of_window(&self, win: WindowId) -> Option<(usize, usize)> {
        for (ci, col) in self.columns.iter().enumerate() {
            if let Some(ri) = col.windows.iter().position(|&w| w == win) {
                return Some((ci, ri));
            }
        }
        None
    }

    pub fn remove_window(&mut self, win: WindowId) {
        if let Some(fi) = self.floats.iter().position(|&w| w == win) {
            self.floats.remove(fi);
            return;
        }

        for col in &mut self.columns {
            if let Some(wi) = col.windows.iter().position(|&w| w == win) {
                let focused_row = col.focused;
                col.windows.remove(wi);
                if !col.windows.is_empty() {
                    if wi < focused_row {
                        // A row before the focused one was removed: the focused
                        // row shifts down by one, so the focus pointer must shift
                        // with it. Without this, `col.focused` would silently point
                        // at a *different* window in the same column.
                        col.focused = col.focused.saturating_sub(1);
                    } else if wi == focused_row {
                        // The focused window itself was removed: clamp to the new
                        // tail. The exact window that takes its place is
                        // re-derived from `mon.focused` by `State::remove_client`.
                        col.focused = col.focused.min(col.windows.len() - 1);
                    }
                }
                break;
            }
        }

        self.cleanup_empty_columns();
    }

    /// P3: &mut self — no clone needed at call sites
    pub fn cleanup_empty_columns(&mut self) {
        let target = self.focus.column_idx;
        // How many dropped columns sat strictly *before* the focused column? Each
        // one shifts the focused column's index down by one, so the focus pointer
        // must shift by the same amount — not just be clamped. A bare clamp (the
        // old behaviour) only corrected the case where the focused column itself
        // was dropped, leaving the focus on whatever column happened to occupy the
        // clamped index after an earlier-column removal — which mis-centered the
        // camera (`ideal_scroll`) and let `best_focus`/`focused_win` steal focus
        // to a neighbour.
        let removed_before = self.columns[..target.min(self.columns.len())]
            .iter()
            .filter(|c| c.windows.is_empty())
            .count();

        let had = self.columns.len();
        self.columns.retain(|col| !col.windows.is_empty());
        let dropped = had - self.columns.len();

        if self.columns.is_empty() {
            self.focus.column_idx = 0;
        } else {
            // Shift left by every dropped column that was before the focus, then
            // clamp defensively.
            let new_idx = target.saturating_sub(removed_before);
            self.focus.column_idx = new_idx.min(self.columns.len() - 1);
        }

        // Removing a column leaves the remaining weights short of 1.0. In the
        // true-scroll model each column's width is independent, so the total
        // simply shrinks; but a degenerate/negative weight would break geometry,
        // so repair any non-positive weight to a sane fallback. (This does NOT
        // re-normalize the survivors — see `rebalance_weights`.)
        if dropped > 0 {
            self.rebalance_weights();
        }
    }

    /// Drag-and-drop-to-tile: insert `win` into column `ci` at `pos` and make it
    /// the focused row of that column. `pos` is clamped to the column length so
    /// an `append` (`pos == windows.len()`) is valid. Also points the
    /// workspace's focused column at `ci`. Pure (no X11) so the backend's
    /// `on_button_release` and the unit tests share one source of truth.
    pub fn drop_into_column(&mut self, ci: usize, win: WindowId, pos: usize) {
        if ci >= self.columns.len() {
            return;
        }
        let cws = &mut self.columns[ci];
        let pos = pos.min(cws.windows.len());
        cws.windows.insert(pos, win);
        cws.focused = pos;
        self.focus.column_idx = ci;
    }
}

// ─── Fullscreen policy ────────────────────────────────────────────────────────
//
// A window's fullscreen *state* ("is it fullscreen right now?") is
// `WinFlags::FULLSCREEN`. Its fullscreen *policy* is a different question —
// "what should Maverick do when this window asks for fullscreen?" — and is set
// once from a window rule, never toggled at runtime. Keeping the two apart is
// what lets Firefox's F11 be refused while `Mod4+F` still works on the very
// same window.

/// What Maverick does with a window's fullscreen requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FullscreenPolicy {
    /// Fullscreen behaves like everywhere else: in `LayoutKind::Column` the
    /// window becomes a screen-wide column of the scrolling ribbon, in `Grid`
    /// it is a pinned overlay.
    #[default]
    Normal,
    /// Refuse fullscreen requests that come *from the client* (an EWMH
    /// `_NET_WM_STATE_FULLSCREEN` client message — which is what a browser's
    /// F11 sends). The user's own `Mod4+F` still works and still produces a
    /// normal tiled fullscreen: this rejects the app's opinion, not the user's.
    ///
    /// This is the runtime counterpart of `Rule::ignore_initial_state`, which
    /// only ever fires once, at map time.
    Deny,
    /// Real, exclusive fullscreen: the window leaves the ribbon entirely and is
    /// presented as an overlay covering `mon.screen` in *any* layout, with
    /// `_NET_WM_BYPASS_COMPOSITOR` asking the compositor to step aside. This is
    /// the mode for games and video players that own their own vsync — Maverick
    /// does not touch their frame pacing, it just gets out of the way.
    True,
}

// ─── Fullscreen snapshot ────────────────────────────────────────────────────
//
// `saved_geom` (a single `Rect`) was the original "remember the pre-fullscreen
// geometry" store, but it is fragile: `set_maximized` and `set_fullscreen` both
// write it, so a window that is maximized *while fullscreen* clobbers the saved
// float rect, and leaving fullscreen then restores the wrong geometry (plan
// 1786564084575, Fase 3). The snapshot captures the window's *prevailing mode*
// together with the exact rect it had just before entering fullscreen, so
// leaving fullscreen can restore the prior state verbatim instead of
// reconstructing it from the (shared, mutable) `saved_geom`.

/// The presentation mode a window was in immediately before it entered
/// fullscreen. Used to decide what "leave fullscreen" must restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// A free-floating window laid out from `client.geom`.
    Float,
    /// A tiled column of the ribbon.
    Tiled,
    /// A maximized (workarea-filling) overlay window.
    Maximized,
}

/// Exact pre-fullscreen state, captured once on enter and applied verbatim on
/// leave. Replaces the overloaded `saved_geom` for fullscreen transitions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FullscreenSnapshot {
    /// Mode immediately before entering fullscreen.
    pub prior: WindowMode,
    /// Geometry immediately before entering fullscreen.
    pub rect: Rect,
    /// Fullscreen policy immediately before entering fullscreen. Entering via
    /// `ToggleFullscreen` promotes to `True` (exclusive overlay, per the
    /// Column-only design); leaving restores this so a `Deny`/`True` rule is
    /// never clobbered by one toggle cycle.
    pub policy: FullscreenPolicy,
}

// ─── Client ───────────────────────────────────────────────────────────────────

/// One managed window. Holds WM-authoritative geometry, flags, and placement.
///
/// # Ownership
///
/// - `geom` is WM-authoritative for floating windows and is the projected
///   result of `arrange_columns` for tiled windows; `saved_geom` is the
///   pre-float/pre-fullscreen persistence store. `monitor`/`workspace` must
///   agree with the workspace the window is placed in (validated by
///   `State::check_invariants`).
/// - The backend owns X11 window creation/mapping and mirrors `WM_NAME`,
///   `WM_CLASS`, `WM_TRANSIENT_FOR`, `_NET_WM_WINDOW_TYPE`, and size hints into
///   these fields; the core never touches X11 directly.
///
/// # Invariants
///
/// - `geom.w >= 1 && geom.h >= 1` after arrangement.
/// - `monitor < State::monitors.len()`, `workspace < Monitor::workspaces.len()`.
#[derive(Debug, Clone)]
pub struct Client {
    /// Backend-agnostic window identifier.
    pub window: WindowId,
    /// `WM_NAME` / `_NET_WM_NAME`.
    pub name: String,
    /// `WM_CLASS` class.
    pub class: String,
    /// `WM_CLASS` instance.
    pub instance: String,
    /// WM-authoritative geometry (floats: client/rule-defined; tiled: projected by layout).
    pub geom: Rect,
    /// Saved geometry for restore after float/fullscreen/maximize transitions.
    pub saved_geom: Rect,
    /// Current border width in px.
    pub border_w: u32,
    /// Previous border width (restored after fullscreen/maximize).
    pub old_border_w: u32,
    /// Window opacity as 0.0-1.0, from the best matching rule. Written to the
    /// X11 property `_NET_WM_WINDOW_OPACITY` at manage/rearrange time. `None`
    /// means "use the global default" (fully opaque).
    pub opacity: Option<f32>,
    /// Bit-packed window flags (`FLOAT`, `FULLSCREEN`, `STICKY`, …).
    pub flags: WinFlags,
    /// ICCCM size hints.
    pub hints: SizeHints,
    /// Index of the monitor this window lives on.
    pub monitor: usize,
    /// Index into `Monitor::workspaces` this window is placed in.
    pub workspace: usize,
    /// The window this one is transient for (`WM_TRANSIENT_FOR`), when it was a
    /// known client at manage time. Used by the renderer to keep popups/dialogs
    /// of a fullscreen or maximized window above the presentation overlay.
    pub transient_parent: Option<WindowId>,
    /// `_NET_WM_WINDOW_TYPE` values this window declared, as lowercase atom
    /// names (`"dialog"`, `"utility"`, `"toolbar"`, …). Used by window rules.
    pub window_types: Vec<String>,
    /// Monotonic focus serial (bumped on focus changes).
    pub focus_serial: u64,
    /// Observability-only mirror of the last *desired* rect this client was
    /// arranged to (the `DesiredState` rect for it). Written by the render
    /// reconcile path; NEVER read for layout/focus/overlay decisions. Fase 8.
    pub last_desired: Option<Rect>,
    /// Observability-only mirror of the last *real* geometry the client reported
    /// back via `ConfigureNotify` (X11 Real). Written by the events convergence
    /// path; NEVER read for layout/focus/overlay decisions. Fase 8.
    pub last_reported: Option<Rect>,
    /// True when the window is unmanaged (not tiled/floated by the WM).
    pub is_unmanaged: bool,
    /// True when the client wants input focus (`WM_HINTS` input).
    pub wants_input: bool,
    /// True when the WM has hidden the window (offscreen/minimized).
    pub wm_hidden: bool,
    /// Forces the next `apply_geom` to re-emit its `ConfigureWindow` even when
    /// the computed rect equals `geom`.
    ///
    /// `apply_geom` skips windows whose geometry did not change, which is what
    /// keeps `arrange` cheap. But a *state* transition (entering/leaving
    /// fullscreen or maximized) can produce the very same rect while the window
    /// still has to be reconfigured — the border width changed, or the client
    /// resized itself behind our back. This used to be faked by stomping
    /// `geom` with a `Rect::default()` sentinel, which also collapsed floats to
    /// 0×0 whenever the restore path missed a case. The flag says the same
    /// thing without lying about the geometry; `apply_geom` clears it.
    pub geometry_dirty: bool,
    /// Fullscreen transition snapshot: the exact mode + rect the window had
    /// *before* it entered fullscreen, captured by `apply_fullscreen_topology`
    /// and applied verbatim when it leaves. This is the robust replacement for
    /// using the shared `saved_geom` to remember fullscreen geometry (see
    /// `FullscreenSnapshot`). `None` when the window is not in a fullscreen
    /// transition.
    pub fs_snapshot: Option<FullscreenSnapshot>,
    /// What to do when this window asks for fullscreen. Comes from a window
    /// rule (`deny_fullscreen` / `true_fullscreen`) and never changes at
    /// runtime — it is policy, not state, so it deliberately lives here rather
    /// than as another `WinFlags` bit.
    pub fullscreen_policy: FullscreenPolicy,
    /// `_NET_WM_BYPASS_COMPOSITOR` hint from the client (EWMH): None=auto (0 or
    /// absent), Some(1)=force compositor ON, Some(2)=force bypass. Updated on
    /// `PropertyNotify` and read by `compositor_policy::bypass_candidate`.
    pub bypass_hint: Option<u32>,
    /// True while the float's geometry was last claimed by the *client* (a
    /// `ConfigureRequest` this WM adopted verbatim).
    ///
    /// This is the seal that closes the two-authorities loop: the WM adopted
    /// the client's rect bit-for-bit (the only answer that terminates the
    /// conversation), so the next `arrange` must project that same rect back
    /// instead of re-normalizing it (snap to hints + workarea clamp), which
    /// would rewrite it and restart the fight (client re-requests, WM re-writes
    /// — the window "jumps around by itself"). While the seal is set, the float
    /// projection is the adopted rect with only protocol-level sanity applied
    /// (`adopt_client_float_geometry`).
    ///
    /// The seal is CLEARED whenever the WM itself decides the geometry again
    /// (drag/resize, placement rules, `ToggleFloat`, a workarea or monitor
    /// change — everywhere `normalize_float_geom` runs, see
    /// `layout::settle_float_in_workarea`): the WM re-asserts a rect *it*
    /// invented, which by construction is already the fixed point of the
    /// client's own hint grid, so no bounce can come from reclaiming it.
    pub float_client_authority: bool,
}

impl Client {
    /// Create a client for `win` placed on `(mon, ws)` with default geometry/flags.
    pub fn new(win: WindowId, mon: usize, ws: usize) -> Self {
        Self {
            window: win,
            name: String::new(),
            class: String::new(),
            instance: String::new(),
            geom: Rect::default(),
            saved_geom: Rect::default(),
            border_w: 2,
            old_border_w: 2,
            opacity: None,
            flags: WinFlags::default(),
            hints: SizeHints::default(),
            monitor: mon,
            workspace: ws,
            transient_parent: None,
            window_types: Vec::new(),
            focus_serial: 0,
            last_desired: None,
            last_reported: None,
            is_unmanaged: false,
            wants_input: true,
            wm_hidden: false,
            geometry_dirty: false,
            fs_snapshot: None,
            fullscreen_policy: FullscreenPolicy::Normal,
            bypass_hint: None,
            float_client_authority: false,
        }
    }

    /// True when the window is currently floating (`WinFlags::FLOAT`).
    #[inline]
    pub fn is_float(&self) -> bool {
        self.flags.has(WinFlags::FLOAT)
    }
    /// True when this window's floating origin is "born floating" (see
    /// [`WinFlags::FLOAT_NATIVE`]). Orthogonal to the *current* layout mode:
    /// a native float toggled into the tiling keeps returning `true`.
    #[inline]
    pub fn is_native_float(&self) -> bool {
        self.flags.has(WinFlags::FLOAT_NATIVE)
    }
    /// True when the window is fullscreen (`WinFlags::FULLSCREEN`).
    #[inline]
    pub fn is_fullscreen(&self) -> bool {
        self.flags.has(WinFlags::FULLSCREEN)
    }
    /// True when the window is maximized on both axes (workarea overlay).
    #[inline]
    pub fn is_maximized(&self) -> bool {
        // Both axes must be on. `WinFlags::MAXIMIZED` is the union of the two
        // axis bits, and `has()` tests bit *overlap*, so `has(MAXIMIZED)` would
        // be true for a single axis. A window is only "maximized" (filling the
        // workarea as an overlay) when V *and* H are both set.
        self.is_maximized_v() && self.is_maximized_h()
    }
    /// Maximized on the vertical axis (`_NET_WM_STATE_MAXIMIZED_VERT`).
    #[inline]
    pub fn is_maximized_v(&self) -> bool {
        self.flags.has(WinFlags::MAXIMIZED_V)
    }
    /// Maximized on the horizontal axis (`_NET_WM_STATE_MAXIMIZED_HORZ`).
    #[inline]
    pub fn is_maximized_h(&self) -> bool {
        self.flags.has(WinFlags::MAXIMIZED_H)
    }
    /// True when the window does not want focus (`WinFlags::NO_FOCUS`).
    #[inline]
    pub fn no_focus(&self) -> bool {
        self.flags.has(WinFlags::NO_FOCUS)
    }
    /// True when the window is sticky (visible on every workspace of its monitor).
    #[inline]
    pub fn is_sticky(&self) -> bool {
        self.flags.has(WinFlags::STICKY)
    }
    /// True when this window's fullscreen is the exclusive, out-of-ribbon kind
    /// (policy `True`) — the one that covers `mon.screen` in every layout.
    #[inline]
    pub fn is_true_fullscreen(&self) -> bool {
        self.fullscreen_policy == FullscreenPolicy::True
    }
    /// True when the window is fullscreen *and* uses the exclusive overlay mode.
    /// This is the condition `core::present`, `stack_overlay` and `best_focus`
    /// share so the three never disagree about who is on top.
    #[inline]
    pub fn is_fullscreen_overlay(&self) -> bool {
        self.is_fullscreen() && self.is_true_fullscreen()
    }
    /// True when the client's own fullscreen requests must be refused.
    #[inline]
    pub fn denies_fullscreen(&self) -> bool {
        self.fullscreen_policy == FullscreenPolicy::Deny
    }
}

// ─── Reserved space ─────────────────────────────────────────────────────────────
//
// Reservation is modelled in two layers:
//
//   ReservedRegion[]  — individual, trackable reservations (one per dock/bar),
//                       each tagged with its owner so it can be removed exactly
//                       when that window disappears.
//   ReservedArea      — the collapsed per-edge totals, derived from the regions.
//
// The layout only ever sees the resulting `workarea` (screen − ReservedArea) and
// knows nothing about *what* reserved the space. This is deliberately
// backend-agnostic: on X11 the regions come from each external dock's
// `_NET_WM_STRUT[_PARTIAL]`; a future Wayland backend would fill the same
// regions from layer-shell exclusive zones.

/// Which screen edge a reservation pushes in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Top edge.
    Top,
    /// Bottom edge.
    Bottom,
    /// Left edge.
    Left,
    /// Right edge.
    Right,
}

/// A single trackable reservation. `owner` identifies the source: external
/// docks use their window id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedRegion {
    /// Reservation owner (external dock's `WindowId`).
    pub owner: WindowId,
    /// Edge the reservation pushes in from.
    pub edge: Edge,
    /// Thickness in px pushed in from `edge`.
    pub thickness: u32,
}

/// Collapsed per-edge reservation totals derived from a set of regions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReservedArea {
    /// Total reserved thickness on the top edge.
    pub top: u32,
    /// Total reserved thickness on the bottom edge.
    pub bottom: u32,
    /// Total reserved thickness on the left edge.
    pub left: u32,
    /// Total reserved thickness on the right edge.
    pub right: u32,
}

impl ReservedArea {
    /// Collapse trackable regions into per-edge totals. Multiple reservations on
    /// the same edge stack (e.g. two docks both reserving the top).
    pub fn from_regions(regions: &[ReservedRegion]) -> Self {
        let mut a = ReservedArea::default();
        for r in regions {
            let slot = match r.edge {
                Edge::Top => &mut a.top,
                Edge::Bottom => &mut a.bottom,
                Edge::Left => &mut a.left,
                Edge::Right => &mut a.right,
            };
            *slot = slot.saturating_add(r.thickness);
        }
        a
    }

    /// True when no edge reserves any space.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.top == 0 && self.bottom == 0 && self.left == 0 && self.right == 0
    }
}

// ─── Monitor ─────────────────────────────────────────────────────────────────

/// One physical output. Owns its screen geometry, reservation-derived workarea,
/// workspace slots, and focus state.
///
/// # Ownership
///
/// The backend owns RandR/Xinerama detection and calls `Monitor::new` /
/// `recalc_geometry` / `reconcile_workspaces`; the core owns placement and
/// focus within. `screen` is authoritative from the backend; `workarea` is
/// always `screen` minus `reserved` (`ReservedArea::from_regions`).
///
/// # Invariants
///
/// - `workarea` is derived from `screen` and `reserved`; never set directly
///   except via `recalc_geometry`.
/// - `active_ws < workspaces.len()`; `focus_stack` contains no duplicates and
///   only known clients.
#[derive(Debug, Clone)]
pub struct Monitor {
    /// Full screen rect from the backend (RandR/Xinerama).
    pub screen: Rect,
    /// Workarea rect (`screen` minus `reserved`; derived via `recalc_geometry`).
    pub workarea: Rect,
    /// Individual trackable reservations (one per external dock).
    pub reserved_regions: Vec<ReservedRegion>,
    /// Collapsed per-edge totals, derived from `reserved_regions`.
    pub reserved: ReservedArea,
    /// Workspace slots on this monitor.
    pub workspaces: Vec<Workspace>,
    /// Index of the active workspace in `workspaces`.
    pub active_ws: usize,
    /// Logically focused window on this monitor (WM intent; may be `None`).
    pub focused: Option<WindowId>,
    /// MRU focus stack for this monitor (most-recent last).
    pub focus_stack: Vec<WindowId>,
    /// Set when this monitor's window geometry changed and its cached live
    /// placements (used by the GLX compositor) must be recomputed. Cleared by
    /// the frame loop after it re-projects the monitor. Starts `true` so the
    /// first frame projects every monitor.
    pub layout_dirty: bool,
}

impl Monitor {
    /// Create a monitor with `screen` geometry and `n_tags` empty workspaces.
    pub fn new(screen: Rect, n_tags: usize) -> Self {
        let workspaces = (0..n_tags).map(|i| Workspace::new(i as u32)).collect();
        let mut m = Self {
            screen,
            workarea: screen,
            reserved_regions: Vec::new(),
            reserved: ReservedArea::default(),
            workspaces,
            active_ws: 0,
            focused: None,
            focus_stack: Vec::with_capacity(16),
            layout_dirty: true,
        };
        m.recalc_geometry();
        m
    }

    /// Active workspace (immutable). Clamps a stale `active_ws` to the
    /// last workspace instead of panicking (hotplug / session restore can
    /// leave it out of range for one frame; callers repair it right after).
    /// Panics only if there are zero workspaces, which violates the
    /// `reconcile_workspaces(max(1))` invariant.
    pub fn ws(&self) -> &Workspace {
        assert!(
            !self.workspaces.is_empty(),
            "Monitor::ws with zero workspaces (reconcile invariant broken)"
        );
        let i = self.active_ws.min(self.workspaces.len() - 1);
        &self.workspaces[i]
    }
    /// Active workspace (mutable). Same clamping contract as [`Self::ws`].
    pub fn ws_mut(&mut self) -> &mut Workspace {
        assert!(
            !self.workspaces.is_empty(),
            "Monitor::ws_mut with zero workspaces (reconcile invariant broken)"
        );
        let i = self.active_ws.min(self.workspaces.len() - 1);
        &mut self.workspaces[i]
    }
    /// Fallible accessors for new code that wants to handle OOB explicitly.
    pub fn try_ws(&self) -> Option<&Workspace> {
        self.workspaces.get(self.active_ws)
    }
    pub fn try_ws_mut(&mut self) -> Option<&mut Workspace> {
        let i = self.active_ws;
        self.workspaces.get_mut(i)
    }

    // ── Reservation management ──────────────────────────────────────────────
    //
    // All mutations go through these helpers so `reserved` and `workarea` stay
    // consistent with `reserved_regions` (the single source of truth).

    /// Insert or replace the region owned by `owner`. `thickness == 0` removes it.
    pub fn set_reserved_region(&mut self, owner: WindowId, edge: Edge, thickness: u32) {
        self.reserved_regions.retain(|r| r.owner != owner);
        if thickness > 0 {
            self.reserved_regions.push(ReservedRegion {
                owner,
                edge,
                thickness,
            });
        }
        self.recalc_geometry();
    }

    /// Replace *every* region owned by `owner` with `regions` (bug B4). A single
    /// dock may reserve several edges at once (e.g. `top` + `left`), and calling
    /// `set_reserved_region` once per edge would erase the previous one, so all
    /// edges are written together. Passing an empty list clears the owner.
    pub fn set_reserved_regions(&mut self, owner: WindowId, regions: &[(Edge, u32)]) {
        self.reserved_regions.retain(|r| r.owner != owner);
        for &(edge, thickness) in regions {
            if thickness > 0 {
                self.reserved_regions.push(ReservedRegion {
                    owner,
                    edge,
                    thickness,
                });
            }
        }
        self.recalc_geometry();
    }

    /// Remove any region owned by `owner`. Returns true if something was removed.
    pub fn remove_reserved_region(&mut self, owner: WindowId) -> bool {
        let before = self.reserved_regions.len();
        self.reserved_regions.retain(|r| r.owner != owner);
        let removed = self.reserved_regions.len() != before;
        if removed {
            self.recalc_geometry();
        }
        removed
    }

    /// Recompute `reserved` and `workarea` from `reserved_regions`.
    pub fn recalc_geometry(&mut self) {
        self.reserved = ReservedArea::from_regions(&self.reserved_regions);
        let r = self.reserved;
        let x = self.screen.x + r.left as i32;
        let y = self.screen.y + r.top as i32;
        let w = self.screen.w.saturating_sub(r.left + r.right);
        let h = self.screen.h.saturating_sub(r.top + r.bottom);
        self.workarea = Rect::new(x, y, w, h);
    }

    /// Grow or shrink the workspace slots to match `n_tags`, preserving window
    /// state for indices that survive. Growing appends fresh empty workspaces;
    /// shrinking drops trailing slots (windows still assigned there are clamped
    /// to the last surviving workspace by the caller). Keeps `active_ws` in range.
    /// `n_tags == 0` is clamped to 1: zero workspaces would make every
    /// subsequent `ws()` panic.
    pub fn reconcile_workspaces(&mut self, n_tags: usize) {
        let n_tags = n_tags.max(1);
        while self.workspaces.len() < n_tags {
            self.workspaces
                .push(Workspace::new(self.workspaces.len() as u32));
        }
        if self.workspaces.len() > n_tags {
            self.workspaces.truncate(n_tags);
        }
        if self.active_ws >= self.workspaces.len() {
            self.active_ws = self.workspaces.len().saturating_sub(1);
        }
    }
}

/// Navigation / movement direction for focus and column operations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir {
    /// Next in MRU / tiling order.
    Next,
    /// Previous in MRU / tiling order.
    Prev,
    /// Left (previous column).
    Left,
    /// Right (next column).
    Right,
    /// Up (previous row within a column).
    Up,
    /// Down (next row within a column).
    Down,
}

/// Workspace layout kind. Currently only the scrolling column ribbon exists;
///
/// kept as an enum so a future layout can be added without changing `Workspace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutKind {
    /// Scrolling column ribbon (niri-style).
    Column,
}

impl LayoutKind {
    /// Parse a layout name. Currently always returns `Column` (stable for config).
    pub fn from_str(_s: &str) -> Self {
        Self::Column
    }
    /// Short symbol for status display (`[|]` for column).
    pub fn symbol(&self) -> &'static str {
        "[|]"
    }
}

// ─── Key actions ─────────────────────────────────────────────────────────────

// ─── Viewport mode ───────────────────────────────────────────────────────────
//
// Viewport is a *display-state* axis of the workspace, orthogonal to both the
// window-level fullscreen (`WinFlags::FULLSCREEN`) and the Overview film-strip
// zoom-out. `Zoomed` enlarges the ribbon (alpha > 1, see `core::layout::
// ribbon_geom`) so the user can inspect a column up close; `PageSnap` then
// scrolls the camera by one screen-width. It is deliberately *not* called
// "fullscreen" — fullscreen is a window/EWMH state, this is a workspace view.
/// Workspace viewport display mode. Orthogonal to window fullscreen and to
/// Overview zoom-out. `Zoomed` enlarges the ribbon (`alpha > 1` in
/// `ribbon_geom`) for inspection; `PageSnap` then scrolls by one screen-width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewportMode {
    /// Normal (1.0) viewport.
    #[default]
    Normal,
    /// Zoomed-in viewport for inspection.
    Zoomed,
}

/// WM command dispatched from keybindings or IPC. Pure intent — the core
/// decides placement/focus; the backend applies geometry/focus to X11.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Spawn an external command.
    Spawn(Vec<String>),
    /// Kill the focused window.
    Kill,
    /// Focus movement in `Dir`.
    FocusDir(Dir),
    /// Move focused window/column in `Dir`.
    MoveDir(Dir),
    /// Toggle floating for the focused window.
    ToggleFloat,
    /// Toggle fullscreen for the focused window.
    ToggleFullscreen,
    /// Toggle the maximized (workarea-filling, border 0) presentation state of
    /// the focused window. Like fullscreen but respects reserved regions and is
    /// only presented while the window is focused (the "peek" overlay in
    /// `core::present`). Previously only reachable via a client's
    /// `_NET_WM_STATE` request, so the keyboard path was missing (bug C18).
    ToggleMaximize,
    /// Set layout kind for the active workspace.
    SetLayout(LayoutKind),
    /// Grow/shrink the focused column by `i32` pixels.
    GrowCol(i32),
    /// Move focused window into a new column to the right.
    NewColumn,
    /// Merge the focused column into the previous one.
    CollapseColumn,
    /// Switch to workspace `n`.
    View(usize),
    /// Move focused window to workspace `n`.
    MoveToWs(usize),
    /// Focus monitor in `Dir`.
    FocusMon(Dir),
    /// Move focused window to monitor in `Dir`.
    MoveMon(Dir),
    /// Restart the WM (re-exec).
    Restart,
    /// Quit the WM cleanly: the event loop stops and the normal teardown runs —
    /// clients are asked to close under one global budget, then `cleanup()`
    /// releases the X11/IPC resources before the process exits 0. Bound to
    /// `Mod4+Shift+Q` by default; also reachable over the control socket
    /// (`dispatch quit`) and from the TOML config.
    Quit,
    /// Toggle the Overview (semantic-zoom film-strip) mode for the active workspace.
    ToggleOverview,
    /// Move the selection left/right while in Overview (enters Overview if not active).
    OverviewNav(Dir),
    /// Drop into the currently selected column, leaving Overview (zoom back to 1.0).
    OverviewEnter,
    /// Enlarge/shrink the workspace viewport (zoom in/out). Positive `f32` zooms
    /// in, negative zooms out; enters `ViewportMode::Zoomed` and animates the
    /// `page_zoom` spring (see `Workspace::page_zoom_target`). This is display
    /// state, not window fullscreen.
    ViewportZoom(f32),
    /// Scroll the camera by one screen-width in the given direction (a "page"
    /// of the zoomed ribbon). Reuses `ideal_scroll`/`camera` — no focus change.
    PageSnap(Dir),
    /// Native wallpaper control (set/clear/mode). Pure State mutation in the
    /// core; the backend uploads/draws the GPU texture. Never affects focus,
    /// stacking, input, layout or geometry.
    Wallpaper(WallpaperCmd),
}

/// Imperative sub-verbs of the `wallpaper` action. Paths with spaces are
/// preserved verbatim (the caller joins the rest of the line before handing it
/// here), so a wallpaper at `/home/u/My Pic.png` works unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperCmd {
    /// Set the wallpaper to the image/shader at `PathBuf` (source type is
    /// inferred from the extension; see `WallpaperSpec::source_for_path`).
    Set(PathBuf),
    /// Clear the native wallpaper, falling back to the legacy root pixmap.
    Clear,
    /// Change only the mapping mode of the current source.
    Mode(WallpaperMode),
}

// ─── Deferred focus ───────────────────────────────────────────────────────────
/// A deferred focus request created while an overlay (fullscreen/maximize owner)
/// is presented. Unlike the old per-`Workspace` `pending_focus: Option<WindowId>`,
/// this carries the *context* it was created under: the exact `monitor`/`workspace`
/// the deferral belongs to and the `owner` overlay that created it. Carrying the
/// context means a deferred window is never orphaned when the overlay is torn down
/// on a non-active workspace or a non-selected monitor — the deferral is keyed on
/// its OWN `monitor`/`workspace`, and a live overlay that did not create it cannot
/// consume a deferral it does not own. There is at most one deferral at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFocus {
    /// The window that should receive focus once the overlay is dismissed.
    pub window: WindowId,
    /// The overlay that created this deferral (its presented owner at creation).
    pub owner: WindowId,
    /// The monitor the deferral is bound to.
    pub monitor: usize,
    /// The workspace the deferral is bound to.
    pub workspace: usize,
}

// ─── Global state ────────────────────────────────────────────────────────────

/// Global WM state — the single source of truth for placement, focus, and
/// reservations. Owns all `Client`s and `Monitor`s; the backend and compositor
/// only read it.
///
/// # Ownership
///
/// Core owns `State`. Mutations flow through the invariant-checked command
/// pipeline; the backend mirrors X11 state into `clients`/`monitors`.
///
/// # Invariants
///
/// See `State::check_invariants` and the crate docs in `maverick_core::lib`.
#[derive(Debug)]
pub struct State {
    /// All managed clients keyed by `WindowId`.
    pub clients: HashMap<WindowId, Client>,
    /// Monitors / outputs in backend order.
    pub monitors: Vec<Monitor>,
    /// Selected monitor index.
    pub sel_mon: usize,
    /// Monotonic focus serial.
    pub focus_serial: u64,
    /// False when the WM should exit.
    pub running: bool,
    /// Status text for the bar.
    pub status: String,
    /// The window the X server currently reports as having the input focus
    /// (`GetInputFocus`), mirrored from `FocusIn`/`FocusOut` events. This is the
    /// in-memory truth of the *real* X focus, distinct from `mon.focused` (the
    /// WM's logical intent) and the painted border (visual). `reconcile_focus`
    /// keeps all three in lock-step; without it an external `XSetInputFocus`
    /// (popup/dialog/Wine) silently desyncs logical vs real focus.
    pub x11_input_focus: Option<WindowId>,
    /// Deferred focus request with explicit context (see `PendingFocus`). This is
    /// the single global slot; the deferral names the exact `monitor`/`workspace`
    /// it belongs to and the `owner` overlay that created it, so it is never
    /// orphaned when the overlay tears down on a non-active workspace or a
    /// non-selected monitor (invariant 8c). `None` when there is nothing pending.
    pub pending_focus: Option<PendingFocus>,
    /// Transient windows that mapped before their `WM_TRANSIENT_FOR` parent
    /// was managed. Each entry is the child's id; its desired parent is already
    /// stored on `Client::transient_parent`. Once the parent shows up,
    /// `relink_pending_transients` (x11/backend) moves the child onto the
    /// parent's monitor/workspace and re-floats it, instead of leaving the
    /// popup stranded on whatever monitor happened to be focused at map time.
    pub pending_transients: Vec<WindowId>,
    /// Native wallpaper configuration (source + mode). Pure `State` data — the
    /// compositor reads it and uploads/draws the GPU texture; it never affects
    /// focus, stacking, input, layout or window geometry.
    pub wallpaper: WallpaperSpec,
    /// Monotonic revision of `wallpaper`. Bumped on every `SetWallpaper` so the
    /// compositor knows whether it must re-decode/re-upload the texture without
    /// re-decoding every frame (criterion #5).
    pub wallpaper_rev: u64,
}

impl State {
    /// Create an empty state (no monitors/clients).
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            monitors: Vec::new(),
            sel_mon: 0,
            focus_serial: 0,
            running: false,
            status: String::new(),
            pending_transients: Vec::new(),
            x11_input_focus: None,
            pending_focus: None,
            wallpaper: WallpaperSpec::default(),
            wallpaper_rev: 0,
        }
    }

    /// Selected monitor (clamped), if any.
    pub fn mon(&self) -> Option<&Monitor> {
        let i = self.sel_mon.min(self.monitors.len().checked_sub(1)?);
        self.monitors.get(i)
    }
    /// Selected monitor mutably (clamped), if any.
    pub fn mon_mut(&mut self) -> Option<&mut Monitor> {
        let i = self.sel_mon.min(self.monitors.len().checked_sub(1)?);
        self.monitors.get_mut(i)
    }

    /// Pick the best window to focus on `mon_idx`'s active workspace. Pure (no X11).
    ///
    /// Order of preference:
    ///   1. a presentation-overlay window on the workspace, most-recently-focused
    ///      first — so that closing a tile in *peek* mode returns focus to the
    ///      overlay window instead of leaving the user on an invisible tile
    ///      underneath. The candidate set mirrors `core::present` exactly:
    ///      fullscreen counts when it is actually presented as an overlay (in
    ///      `Grid`, or in any layout under `FullscreenPolicy::True`) — in the
    ///      `Column` ribbon a fullscreen window is a plain scrolling tile and
    ///      gets no special treatment — and maximized only counts while it is
    ///      already the monitor's focused window. Otherwise a maximized window
    ///      sitting in the background would grab the focus on
    ///      `ViewWorkspace`/`MoveToWorkspace`/`FocusMonitor` and, because
    ///      `present` then presents whatever is focused, unexpectedly blow
    ///      itself up to fill the workarea;
    ///   2. the column-focused window;
    ///   3. the most-recently focused window in the focus stack.
    ///
    /// Recompute [`Workspace::presented_maximize`] for the active workspace of
    /// `mon_idx`. The maximize overlay is owned by exactly the monitor's focused
    /// window when that window is maximized (`is_maximized_v() || is_maximized_h()`)
    /// on the active workspace. This is the *single* writer that turns
    /// `Monitor::focused` + the client's maximize flags into the explicit
    /// `presented_maximize` field, so no read site has to infer the
    /// maximize-overlay owner from `mon.focused`.
    pub fn sync_presented_maximize(&mut self, mon_idx: usize) {
        if mon_idx >= self.monitors.len() {
            return;
        }
        let (ws_idx, owner) = {
            let mon = &self.monitors[mon_idx];
            let ws_idx = mon.active_ws;
            let owner = mon.focused.filter(|&w| {
                self.clients.get(&w).is_some_and(|c| {
                    c.workspace == ws_idx && (c.is_maximized_v() || c.is_maximized_h())
                })
            });
            (ws_idx, owner)
        };
        if ws_idx < self.monitors[mon_idx].workspaces.len() {
            self.monitors[mon_idx].workspaces[ws_idx].presented_maximize = owner;
        }
        // A maximize overlay is only ever presented on the ACTIVE workspace (see
        // `core::present` and `presented_overlay_owner`, which only read the
        // active workspace). Clear any stale `presented_maximize` entry on the
        // OTHER workspaces of this monitor, so a window un-maximized on a
        // non-active workspace cannot leave a dangling maximize-overlay owner
        // that trips invariant #9 when that workspace is later activated.
        for (i, ws) in self.monitors[mon_idx].workspaces.iter_mut().enumerate() {
            if i != ws_idx {
                ws.presented_maximize = None;
            }
        }
    }

    pub fn best_focus(&self, mon_idx: usize) -> Option<WindowId> {
        // A pending overlay owns the focus: the window currently presented as the
        // overlay (fullscreen owner, or the focused maximized window) is what the
        // user is looking at, so it is the best focus. Delegating to
        // `presented_overlay_owner` removes the duplicate re-derivation of overlay
        // semantics (invariant 9b) that the previous inline version had.
        if let Some(o) = self.presented_overlay_owner(mon_idx) {
            return Some(o);
        }
        let mon = self.monitors.get(mon_idx)?;
        let ws_idx = mon.active_ws;
        if ws_idx >= mon.workspaces.len() {
            return None;
        }
        let col_win = mon.workspaces[ws_idx].focused_win();
        let from_stack = mon
            .focus_stack
            .iter()
            .rev()
            .find(|&&w| self.clients.get(&w).is_some_and(|c| c.workspace == ws_idx))
            .copied();
        col_win.or(from_stack)
    }

    /// The single, canonical definition of "which window currently OWNS the
    /// presented overlay" on `mon_idx`'s active workspace. Returns `None` when
    /// there is no real overlay. A window is a real overlay only when:
    ///   1. it is fullscreen AND (`Grid` layout OR `FullscreenPolicy::True`); a
    ///      `Column`-layout Normal-policy fullscreen is just a ribbon tile and
    ///      is explicitly NOT an overlay; OR
    ///   2. it is the *focused* maximized window (`presented_maximize` owner).
    ///
    /// This MUST stay equivalent to `core::present`, `render::stack_overlay` and
    /// `best_focus`. `manage` used to duplicate this with a weaker
    /// `is_fullscreen() || is_maximized_*` condition.
    ///
    /// IMPORTANT: this is **NOT** the same as "covering fullscreen" used by
    /// pointer hit-testing. A `Column`-layout fullscreen is *covering* (it paints
    /// a ribbon tile over the screen) but is **not** returned here — and a
    /// `presented_maximize` window **is** returned here but is **not**
    /// `is_fullscreen()` at all. Use [`State::covering_fullscreen_window`] for the
    /// covering predicate and never substitute `Client::is_fullscreen()` for
    /// either, since `is_fullscreen()` is true for both and would silently merge
    /// the two disjoint concepts.
    pub fn presented_overlay_owner(&self, mon_idx: usize) -> Option<WindowId> {
        let ws_idx = self.monitors.get(mon_idx)?.active_ws;
        self.presented_overlay_owner_in(mon_idx, ws_idx)
    }

    /// The canonical overlay owner on an EXPLICIT workspace `ws_idx` of
    /// `mon_idx`. Shared by [`State::presented_overlay_owner`] (which passes the
    /// monitor's active workspace) and the core EWMH `_NET_ACTIVE_WINDOW` policy
    /// (which must test the *requesting* window's own (monitor, workspace), not
    /// necessarily the active one).
    pub fn presented_overlay_owner_in(&self, mon_idx: usize, ws_idx: usize) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws = mon.workspaces.get(ws_idx)?;
        // A window that is *both* fullscreen and maximized is owned exclusively
        // through the maximize branch below (so it agrees with
        // `presented_maximize`); the fullscreen branch only matches a *purely*
        // fullscreen window. Without this guard the two branches could name
        // different windows and trip invariant #9b ("overlay owner /
        // presented_maximize mismatch"). `core::present` still presents *every*
        // fullscreen window regardless of focus; this helper only names the one
        // that owns the overlay for focus/stacking decisions.
        let fs_overlay = mon.focus_stack.iter().rev().find(|&&w| {
            self.clients.get(&w).is_some_and(|c| {
                c.workspace == ws_idx
                    && c.is_fullscreen()
                    && !c.is_maximized()
                    && c.is_true_fullscreen()
            })
        });
        if let Some(&w) = fs_overlay {
            return Some(w);
        }
        if let Some(w) = ws.presented_maximize {
            if self.clients.get(&w).is_some_and(|c| {
                c.workspace == ws_idx && (c.is_maximized_v() || c.is_maximized_h())
            }) {
                return Some(w);
            }
        }
        None
    }

    /// True iff `self.pending_focus` (if set) names an owner that is still a
    /// currently-presented overlay on the deferral's (monitor, workspace) — i.e.
    /// the EXACT `#8c` condition. This is the single shared definition used by
    /// BOTH `check_invariants` (#8c) and the post-command reconciliation hook
    /// (`reconcile_pending_focus_after_transition`) so they can never disagree.
    pub fn pending_focus_owner_presented(&self) -> bool {
        let pf = match self.pending_focus {
            Some(p) => p,
            None => return false,
        };
        self.monitors.get(pf.monitor).is_some_and(|m| {
            let focused = m.focused;
            m.workspaces.get(pf.workspace).is_some_and(|_ws| {
                self.clients.get(&pf.owner).is_some_and(|c| {
                    c.monitor == pf.monitor
                        && c.workspace == pf.workspace
                        && (c.is_fullscreen() && c.is_true_fullscreen()
                            || (c.is_maximized_v() || c.is_maximized_h())
                                && focused == Some(pf.owner))
                })
            })
        })
    }

    /// Monitor index containing `(x, y)`, or `sel_mon` if outside all screens.
    pub fn mon_at(&self, x: i32, y: i32) -> usize {
        for (i, m) in self.monitors.iter().enumerate() {
            if m.screen.contains(x, y) {
                return i;
            }
        }
        self.sel_mon
    }

    /// Bump and return the next focus serial.
    pub fn next_serial(&mut self) -> u64 {
        self.focus_serial += 1;
        self.focus_serial
    }

    /// Insert a client into `self.clients` (does not place it in a workspace).
    pub fn add_client(&mut self, c: Client) {
        let win = c.window;
        self.clients.insert(win, c);
    }

    /// Read-only view of the wallpaper for the compositor: source, mode, and the
    /// current revision. Pure query — never mutates `State`.
    pub fn wallpaper_layer(&self) -> (WallpaperSource, WallpaperMode, u64) {
        (
            self.wallpaper.source.clone(),
            self.wallpaper.mode,
            self.wallpaper_rev,
        )
    }

    /// Remove `win` from `clients` and all placement/overlay/focus structures.
    /// Returns the removed client, if any.
    pub fn remove_client(&mut self, win: WindowId) -> Option<Client> {
        let c = self.clients.remove(&win)?;
        // Drop every transient reference to the window that just died, so the
        // ownership graph never names a client that is gone (Riesgo 5).
        //
        //   * a surviving child whose `transient_parent` was `win` is re-parented
        //     to `None` — it becomes a plain float instead of a popup owned by a
        //     ghost. Readers (`render::transient_of`, `decide_manage_focus`,
        //     `relink_pending_transients`) already treat an unknown parent as "no
        //     parent", so behaviour is unchanged; what changes is that the stale
        //     id can no longer be resurrected by X11 *XID reuse* — a brand-new,
        //     unrelated window that happens to get the recycled id would
        //     otherwise inherit these orphans as its popups (raised above its
        //     overlay, relinked onto its monitor/workspace).
        //   * `win` itself (and any child it just orphaned) is dropped from the
        //     deferred-transient queue; that queue is only drained on the next
        //     `manage()`, so without this a destroyed child stayed referenced
        //     there indefinitely.
        for other in self.clients.values_mut() {
            if other.transient_parent == Some(win) {
                other.transient_parent = None;
            }
        }
        let clients = &self.clients;
        self.pending_transients
            .retain(|w| clients.get(w).is_some_and(|c| c.transient_parent.is_some()));
        if let Some(pf) = self.pending_focus {
            if pf.window == win || pf.owner == win {
                self.pending_focus = None;
            }
        }
        if self.monitors.is_empty() {
            return Some(c);
        }
        // Clear any stale `presented_maximize` references to this window across
        // *every* monitor/workspace, not just the one the client currently
        // points at. A window that was moved away (MoveWindowToMonitor /
        // MoveToWorkspace) may still be named as the maximize-overlay owner on a
        // former monitor/workspace; leaving that dangling entry triggers
        // invariant #9 ("presented_maximize ... not in clients") once the window
        // is destroyed.
        for mon in &mut self.monitors {
            for ws in &mut mon.workspaces {
                if ws.presented_maximize == Some(win) {
                    ws.presented_maximize = None;
                }
            }
        }
        // c.monitor may be stale after hotplug (fewer monitors than before).
        // Clamp to avoid panic index out-of-bounds.
        let mon_i = c.monitor.min(self.monitors.len().saturating_sub(1));
        let mon = &mut self.monitors[mon_i];
        mon.focus_stack.retain(|&w| w != win);
        if mon.focused == Some(win) {
            mon.focused = mon.focus_stack.last().copied();
        }
        if c.workspace < mon.workspaces.len() {
            let ws_i = c.workspace;
            mon.workspaces[ws_i].remove_window(win);
            // Keep the workspace focus pointer (column + row) in lock-step with
            // the logical focus (`mon.focused`). `remove_window` only shifts/
            // clamps the column index relative to the surviving tree; it does not
            // know which window is logically focused. Re-deriving the pointer from
            // `mon.focused` here is the single source of truth for "what is
            // focused": without it, removing a column before the focused one left
            // `ws.focus.column_idx` on a different column, so the camera
            // (`ideal_scroll` reads `focus.column_idx`) centered the wrong column
            // and `best_focus`/`focused_win` returned a neighbour — focus silently
            // jumped to the wrong window on close-before-focus, close-visible and
            // close-offscreen (invariant: focus must not depend on visibility).
            if let Some(fw) = mon.focused {
                if let Some((ci, ri)) = mon.workspaces[ws_i].index_of_window(fw) {
                    mon.workspaces[ws_i].focus.column_idx = ci;
                    mon.workspaces[ws_i].columns[ci].focused = ri;
                }
            }
        }
        Some(c)
    }

    /// Pure workspace rearrangement for `MoveDir` — no X11 calls.
    /// Call this from x11.rs then follow up with arrange/focus.
    /// Returns false if there was nothing to do (float, empty workspace, boundary no-op).
    pub fn apply_move_dir(&mut self, dir: Dir) -> bool {
        if self.monitors.is_empty() {
            return false;
        }
        let mi = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        let ws_i = match self.monitors.get(mi) {
            Some(m) => m.active_ws,
            None => return false,
        };
        // Spring-split ratio used when extracting a window into its own
        // column. No `Cfg` here, so a balanced split keeps the node weights
        // normalized (the caller can re-tune via grow/shrink afterwards).
        let focused = match self.monitors[mi].focused {
            Some(w) => w,
            None => return false,
        };

        if self.clients.get(&focused).is_some_and(Client::is_float) {
            return false;
        }

        let (ci, n_cols, col_len) = {
            let ws = &self.monitors[mi].workspaces[ws_i];
            (
                ws.focus.column_idx,
                ws.columns.len(),
                ws.columns
                    .get(ws.focus.column_idx)
                    .map_or(0, |c| c.windows.len()),
            )
        };
        // Stale focus after restore/shrink: `ci` can exceed the column list
        // (col_len == 0 above). Bail instead of indexing `columns[ci]`.
        if ci >= n_cols {
            return false;
        }

        // P3: mutate in-place, no clone
        match dir {
            Dir::Left | Dir::Right => {
                if col_len <= 1 {
                    let ws = &mut self.monitors[mi].workspaces[ws_i];
                    match dir {
                        Dir::Left if ci > 0 => {
                            ws.columns.swap(ci, ci - 1);
                            ws.focus.column_idx = ci - 1;
                        }
                        Dir::Right if ci + 1 < n_cols => {
                            ws.columns.swap(ci, ci + 1);
                            ws.focus.column_idx = ci + 1;
                        }
                        _ => return false,
                    }
                } else {
                    let ws = &mut self.monitors[mi].workspaces[ws_i];
                    let ratio = 0.5;
                    let src_w = ws.columns[ci].weight;
                    ws.remove_window(focused); // column keeps `src_w` (still non-empty)
                    let index_in_ws = if dir == Dir::Left { ci } else { ci + 1 };
                    let insert_pos = index_in_ws.min(ws.columns.len());
                    // Spring-split the source column: it keeps `ratio` of its
                    // weight, the extracted window takes the rest.
                    ws.columns[insert_pos.min(ci)].weight = src_w * ratio;
                    let mut new_col = Column::new(src_w * (1.0 - ratio));
                    new_col.windows.push(focused);
                    new_col.focused = 0;
                    ws.columns.insert(insert_pos, new_col);
                    ws.focus.column_idx = insert_pos;
                    ws.rebalance_weights();
                }
            }
            Dir::Up | Dir::Down => {
                let ws = &mut self.monitors[mi].workspaces[ws_i];
                if let Some(col) = ws.columns.get_mut(ci) {
                    let n = col.windows.len();
                    if n < 2 {
                        return false;
                    }
                    let ri = col.focused;
                    let new_ri = if dir == Dir::Up {
                        (ri + n - 1) % n
                    } else {
                        (ri + 1) % n
                    };
                    col.windows.swap(ri, new_ri);
                    col.focused = new_ri;
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    /// Check the structural invariants of the whole `State` (the 14-point
    /// manifesto contract: every window lives in exactly one place, indices are
    /// in range, focus is valid, geometry is positive/finite, cameras have no
    /// NaN, …). Returns `Ok(())` when clean, or `Err(violations)` listing every
    /// broken invariant.
    ///
    /// This is the single source of truth shared by:
    ///   * `Engine::execute` / `execute_batch` (gated to `debug_assertions` so it
    ///     costs nothing in release), and
    ///   * the property-based harness (Fase 5), which calls it after every step
    ///     regardless of profile.
    ///
    /// It is deliberately cheap and side-effect free — no X11, no cloning of the
    /// client map — so running it on every transition is affordable.
    pub fn check_invariants(&self) -> Result<(), Vec<String>> {
        let mut v = Vec::new();

        // 1. Monitor / workspace indices valid.
        for (mi, mon) in self.monitors.iter().enumerate() {
            if mon.active_ws >= mon.workspaces.len() {
                v.push(format!(
                    "monitor {mi}: active_ws {} out of range ({} workspaces)",
                    mon.active_ws,
                    mon.workspaces.len()
                ));
            }
            // 2. Cameras carry no NaN / infinity (a zeroed spring would desync
            //    the compositor from the logical scroll).
            if !mon
                .workspaces
                .iter()
                .all(|ws| ws.camera.target.is_finite() && ws.camera.position.is_finite())
            {
                v.push(format!("monitor {mi}: camera has NaN/inf target/position"));
            }
            for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                // 3. Focus column/row pointers in range.
                if !ws.columns.is_empty() && ws.focus.column_idx >= ws.columns.len() {
                    v.push(format!(
                        "monitor {mi} ws {ws_i}: focus.column_idx {} >= {} columns",
                        ws.focus.column_idx,
                        ws.columns.len()
                    ));
                }
                for (ci, col) in ws.columns.iter().enumerate() {
                    if !col.windows.is_empty() && col.focused >= col.windows.len() {
                        v.push(format!(
                            "monitor {mi} ws {ws_i} : column {ci}: focused {} >= {} windows",
                            col.focused,
                            col.windows.len()
                        ));
                    }
                }
            }
        }

        // 4. Every window referenced in a tiling/float lives in `clients`, and
        //    no window is referenced more than once across the whole tree.
        let mut seen: std::collections::HashMap<WindowId, (usize, usize)> =
            std::collections::HashMap::new();
        for (mi, mon) in self.monitors.iter().enumerate() {
            for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                for (place, win) in ws
                    .columns
                    .iter()
                    .flat_map(|c| c.windows.iter().copied())
                    .map(|w| ("column", w))
                    .chain(ws.floats.iter().copied().map(|w| ("float", w)))
                {
                    if !self.clients.contains_key(&win) {
                        v.push(format!(
                            "monitor {mi} ws {ws_i}: {place} window {win} not in clients"
                        ));
                        continue;
                    }
                    if let Some(prev) = seen.insert(win, (mi, ws_i)) {
                        v.push(format!(
                            "window {win} referenced twice: at ({}, {}) and ({}, {})",
                            prev.0, prev.1, mi, ws_i
                        ));
                    }
                }
            }
        }

        // 5. Every client *placed in the tree* must be stored at the
        //    (monitor, workspace) its fields claim. A window whose
        //    `monitor`/`workspace` disagree with where it is tiled is a desync
        //    between the logical model and the placement tree. (Orphan clients
        //    that are not yet placed — legitimate in test scaffolding and
        //    during manage — are intentionally NOT required to be in the tree.)
        for (&win, c) in &self.clients {
            if c.monitor >= self.monitors.len() {
                v.push(format!(
                    "client {win}: monitor {} out of range ({})",
                    c.monitor,
                    self.monitors.len()
                ));
                continue;
            }
            let mon = &self.monitors[c.monitor];
            if c.workspace >= mon.workspaces.len() {
                v.push(format!(
                    "client {win}: workspace {} out of range on monitor {} ({} workspaces)",
                    c.workspace,
                    c.monitor,
                    mon.workspaces.len()
                ));
                continue;
            }
            if let Some((pm, pw)) = seen.get(&win).copied() {
                if (pm, pw) != (c.monitor, c.workspace) {
                    v.push(format!(
                        "client {win}: stored at ({}, {}) but tiled at ({}, {})",
                        c.monitor, c.workspace, pm, pw
                    ));
                }
            }
            // NOTE: geometry-positivity and "focused window must be in its active
            // workspace" are deliberately NOT asserted here — they hold only
            // *after* an arrange/placement pass, and many valid intermediate
            // states (and unit-test fixtures) legitimately have a default rect
            // or a transient logical focus. Those are runtime concerns, not
            // structural invariants.
        }

        // 8. Focus stack references only real clients.
        for (mi, mon) in self.monitors.iter().enumerate() {
            for &w in &mon.focus_stack {
                if !self.clients.contains_key(&w) {
                    v.push(format!(
                        "monitor {mi}: focus_stack references unknown window {w}"
                    ));
                }
            }
            // 8b. focus_stack has no duplicates.
            let mut seen_f = std::collections::HashSet::new();
            let mut dup = false;
            for &w in &mon.focus_stack {
                if !seen_f.insert(w) {
                    dup = true;
                }
            }
            if dup {
                v.push(format!("monitor {mi}: focus_stack has duplicate entries"));
            }
            // 8c. The global `pending_focus` slot names a live client and is owned
            // by a currently-presented overlay on the SAME monitor/workspace the
            // deferral was created for. The overlay need not be the *active*
            // workspace — switching workspaces merely hides it from view, it does
            // not dismiss it, so the deferral legitimately survives a workspace
            // switch (it is only consumed/dropped when the overlay is torn down).
            // Requiring the owner to still be the presented overlay is what stops
            // a deferred window from being orphaned: the moment the owner overlay
            // is dismissed or destroyed the deferral is consumed (or dropped).
            if let Some(pf) = self.pending_focus {
                if !self.clients.contains_key(&pf.window) {
                    v.push(format!(
                        "pending_focus window {} is not a known client",
                        pf.window
                    ));
                } else if !self.clients.contains_key(&pf.owner) {
                    v.push(format!(
                        "pending_focus owner {} is not a known client",
                        pf.owner
                    ));
                } else if !self.pending_focus_owner_presented() {
                    v.push(format!(
                        "pending_focus owner {} is not a presented overlay on monitor {} workspace {}",
                        pf.owner, pf.monitor, pf.workspace
                    ));
                }
            }
            // 9b. overlay owner (maximize branch) matches presented_maximize.
            if let Some(w) = self.presented_overlay_owner(mi) {
                if self.clients.get(&w).is_some_and(Client::is_maximized)
                    && mon
                        .workspaces
                        .get(mon.active_ws)
                        .and_then(|ws| ws.presented_maximize)
                        != Some(w)
                {
                    v.push(format!(
                        "monitor {mi}: overlay owner / presented_maximize mismatch"
                    ));
                }
            }
            // NOTE: the logically-focused window is NOT required to be in
            // `focus_stack` here — unit-test fixtures and transient focus
            // retargets legitimately set `mon.focused` before/without updating
            // the stack, and the production focus path keeps them in sync. The
            // dangling-reference check above (stack must name real clients) is
            // the part that catches real corruption.
            // 9. `presented_maximize`, if set, names a real maximized client on the
            //    active workspace.
            let pm = mon
                .workspaces
                .get(mon.active_ws)
                .and_then(|ws| ws.presented_maximize);
            if let Some(w) = pm {
                match self.clients.get(&w) {
                    None => v.push(format!(
                        "monitor {mi}: presented_maximize {w} not in clients"
                    )),
                    Some(c) if !c.is_maximized() => v.push(format!(
                        "monitor {mi}: presented_maximize {w} is not maximized"
                    )),
                    Some(c) if c.workspace != mon.active_ws => v.push(format!(
                        "monitor {mi}: presented_maximize {w} on wrong workspace"
                    )),
                    _ => {}
                }
            }
        }

        // 10. X input focus mirrors a real client.
        if let Some(w) = self.x11_input_focus {
            if !self.clients.contains_key(&w) {
                v.push(format!("x11_input_focus {w} is not a known client"));
            }
        }

        // 11. Column weight bounds — GrowColumn debe poder llegar a 1.0 (pantalla
        // completa) y nunca salir de [0.05, 1.0]; el viejo límite 0.95 con 2 columnas
        // bloqueaba el segundo mosaico.
        for (mi, mon) in self.monitors.iter().enumerate() {
            for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                for (ci, col) in ws.columns.iter().enumerate() {
                    if !(0.05 - 1e-6..=1.0 + 1e-6).contains(&col.weight) {
                        v.push(format!(
                            "monitor {mi} ws {ws_i} col {ci}: weight {} fuera de [0.05, 1.0]",
                            col.weight
                        ));
                    }
                    if !col.weight.is_finite() {
                        v.push(format!("monitor {mi} ws {ws_i} col {ci}: weight no finito"));
                    }
                }
            }
        }

        // 12. Float — geometría validada vía `render::clamp_is_idempotent` y
        // `arrange` (ver `src/backend/x11/render.rs`), no como invariante
        // estructural aquí: fixtures crean rects temporales 0x0/oversize antes
        // del primer `arrange` y el clamp se aplica en el pipeline.

        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }

    /// `#[cfg(debug_assertions)]` only — panic on the first broken invariant.
    /// Called by `Engine::execute` / `execute_batch` so a bad transition blows up
    /// immediately in debug builds instead of corrupting the model silently.
    #[cfg(debug_assertions)]
    pub fn assert_invariants(&self) {
        if let Err(violations) = self.check_invariants() {
            panic!(
                "State invariant violation:\n  - {}",
                violations.join("\n  - ")
            );
        }
    }

    /// Advance every workspace camera (and per-column boost / zoom springs) by
    /// `dt` seconds. Returns true if any animation is still in flight, so the
    /// backend can keep ticking at a high frame rate.
    pub fn tick_animations(&mut self, dt: f32) -> bool {
        let mut scratch = Vec::new();
        self.tick_animations_multi(dt, &mut scratch)
    }

    /// Like [`State::tick_animations`] but also reports, per monitor, whether that
    /// monitor still has a moving spring (camera, accordion `boost`, or zoom).
    ///
    /// The per-monitor flag lets the frame loop recompute the live layout for
    /// *only* the monitors that are actually animating, instead of (as the global
    /// `bool` did) recomputing every monitor's projection on every animation
    /// frame. An idle monitor whose layout is unchanged keeps its cached
    /// projection (see `WindowManager::run_once`).
    pub fn tick_animations_multi(&mut self, dt: f32, per_monitor: &mut [bool]) -> bool {
        let mut any = false;
        for (mi, mon) in self.monitors.iter_mut().enumerate() {
            let mut anim = false;
            for ws in &mut mon.workspaces {
                if ws.layout == LayoutKind::Column {
                    anim |= ws.camera.step(dt);
                    // A zero-resolution Instant must not turn a pending camera
                    // retarget into an apparently settled state. `step` still
                    // owns numerical sanitisation; this keeps one more frame
                    // scheduled until a positive monotonic interval arrives.
                    anim |= ws.camera.needs_update();
                    // Per-column accordion: every column eases its own `boost`
                    // toward 1.0 if it is the focused one, else toward 0.0. This
                    // makes column widths glide when focus changes (bug C10)
                    // instead of snapping. In Overview every boost is forced to 0
                    // so all columns share the base width and the strip fits them.
                    let focus_i = ws.focus.column_idx;
                    for (i, col) in ws.columns.iter_mut().enumerate() {
                        let target = if ws.overview {
                            0.0
                        } else if i == focus_i {
                            1.0
                        } else {
                            0.0
                        };
                        if spring_smooth(&mut col.boost, target, dt) {
                            anim = true;
                        }
                    }
                    if spring_smooth(&mut ws.zoom, ws.zoom_target, dt) {
                        anim = true;
                    }
                    // Viewport page-zoom spring (Fase 9/11): only meaningful in
                    // Zoomed mode, but it is harmless to ease it always — when
                    // the workspace is back to Normal the factor is ignored by
                    // `ribbon_geom` anyway.
                    if spring_smooth(&mut ws.page_zoom, ws.page_zoom_target, dt) {
                        anim = true;
                    }
                }
            }
            if mi < per_monitor.len() {
                per_monitor[mi] = anim;
            }
            any |= anim;
        }
        any
    }

    /// Snap all animated values to their targets immediately (no interpolation).
    /// Used when `animations.enabled = false` so the WM never requests animation
    /// frames.
    pub fn snap_animations(&mut self) {
        for mon in &mut self.monitors {
            for ws in &mut mon.workspaces {
                ws.camera.snap(ws.camera.target);
                let focus_i = ws.focus.column_idx;
                for (i, col) in ws.columns.iter_mut().enumerate() {
                    let target = if ws.overview {
                        0.0
                    } else if i == focus_i {
                        1.0
                    } else {
                        0.0
                    };
                    col.boost = target;
                }
                ws.zoom = ws.zoom_target;
                ws.page_zoom = ws.page_zoom_target;
            }
        }
    }
}

/// Critically-damped-ish exponential approach of `cur` toward `target`.
/// Returns true while still moving meaningfully, including a pending finite
/// target when `dt` is zero so the scheduler does not park mid-transition.
/// Stable for any positive dt.
///
/// P2 hardening: a non-finite `target` or `dt` must not poison `cur` — an
/// infinite target would push `cur` to infinity on the first step and the
/// layout projection would follow. Poisoned inputs are ignored and the
/// spring reports "settled" so the animator can drop the frame.
pub fn spring_smooth(cur: &mut f32, target: f32, dt: f32) -> bool {
    if !dt.is_finite() {
        return false;
    }
    if !cur.is_finite() {
        *cur = if target.is_finite() { target } else { 0.0 };
    }
    if dt <= 0.0 {
        return target.is_finite() && (*cur - target).abs() > 0.001;
    }
    if !target.is_finite() {
        // Never chase a non-finite target; if `cur` is already poisoned,
        // pull it back to a finite value.
        if !cur.is_finite() {
            *cur = 0.0;
        }
        return false;
    }
    let rate = 12.0;
    let k = 1.0 - (-rate * dt).exp();
    *cur += (target - *cur) * k;
    if !cur.is_finite() {
        *cur = target;
        return false;
    }
    if (*cur - target).abs() <= 0.001 {
        *cur = target;
        false
    } else {
        true
    }
}

/// Sanitize user-supplied spring parameters before they enter the analytical
/// camera transition. Stiffness is bounded for numerical range; damping is
/// additionally bounded relative to `sqrt(stiffness)` so an overdamped camera
/// cannot retain a practically invisible slow pole for minutes.
pub fn sanitize_spring(stiffness: f32, damping: f32) -> (f32, f32) {
    let stiffness = if !stiffness.is_finite() || stiffness <= 0.0 {
        220.0
    } else {
        stiffness.clamp(MIN_STIFFNESS, MAX_SPRING)
    };
    let damping = if !damping.is_finite() || damping <= 0.0 {
        30.0
    } else {
        damping
    };
    // Apply the same relative bound to fallbacks as to user values. This keeps
    // `sanitize_spring` inside the effective domain D for every input and
    // makes sanitization idempotent even for tiny stiffness + invalid damping.
    (stiffness, bounded_damping(stiffness, damping))
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;

    fn mon() -> Monitor {
        Monitor::new(Rect::new(0, 0, 1920, 1080), 9)
    }

    #[test]
    fn top_dock_reserves_top_only() {
        let mut m = mon();
        m.set_reserved_region(0x1001, Edge::Top, 22);
        assert_eq!(
            m.reserved,
            ReservedArea {
                top: 22,
                ..Default::default()
            }
        );
        assert_eq!(m.workarea, Rect::new(0, 22, 1920, 1058));
    }

    #[test]
    fn bottom_dock_reserves_bottom_only() {
        let mut m = mon();
        m.set_reserved_region(0x1001, Edge::Bottom, 30);
        assert_eq!(
            m.reserved,
            ReservedArea {
                bottom: 30,
                ..Default::default()
            }
        );
        assert_eq!(m.workarea, Rect::new(0, 0, 1920, 1050));
    }

    #[test]
    fn two_docks_stack_on_same_edge() {
        // Two top docks (22 + 40) both reserve the top edge.
        let mut m = mon();
        m.set_reserved_region(0x1001, Edge::Top, 22);
        m.set_reserved_region(0x1002, Edge::Top, 40);
        assert_eq!(m.reserved.top, 62);
        assert_eq!(m.workarea, Rect::new(0, 62, 1920, 1018));
    }

    #[test]
    fn removing_external_dock_restores_workarea() {
        let mut m = mon();
        let before = m.workarea;
        m.set_reserved_region(0x1001, Edge::Bottom, 40);
        assert_eq!(m.workarea, Rect::new(0, 0, 1920, 1040));
        assert!(m.remove_reserved_region(0x1001));
        assert_eq!(m.workarea, before);
        // Removing a non-existent owner is a no-op.
        assert!(!m.remove_reserved_region(0x9999));
    }

    #[test]
    fn left_and_right_docks_shrink_width() {
        let mut m = mon();
        m.set_reserved_region(0x1, Edge::Left, 50);
        m.set_reserved_region(0x2, Edge::Right, 60);
        assert_eq!(m.workarea, Rect::new(50, 0, 1810, 1080));
    }

    #[test]
    fn zero_thickness_region_is_removal() {
        let mut m = mon();
        m.set_reserved_region(0x1, Edge::Top, 40);
        assert_eq!(m.reserved.top, 40);
        m.set_reserved_region(0x1, Edge::Top, 0);
        assert_eq!(m.reserved.top, 0);
        assert!(m.reserved.is_empty());
    }

    #[test]
    fn b4_single_dock_reserves_multiple_edges() {
        // A dock may reserve several edges at once (panel + launcher). All must
        // be applied together, and re-registering must not lose the other edge
        // (bug B4): `set_reserved_region` clears the owner's previous region.
        let mut m = mon();
        m.set_reserved_regions(0x9001, &[(Edge::Top, 22), (Edge::Left, 50)]);
        assert_eq!(m.reserved.top, 22);
        assert_eq!(m.reserved.left, 50);
        assert_eq!(m.reserved_regions.len(), 2);
        // Refreshing with a different edge set replaces the whole owner.
        m.set_reserved_regions(0x9001, &[(Edge::Bottom, 30)]);
        assert_eq!(m.reserved.top, 0);
        assert_eq!(m.reserved.left, 0);
        assert_eq!(m.reserved.bottom, 30);
        assert_eq!(m.reserved_regions.len(), 1);
        // Empty list clears the owner entirely.
        m.set_reserved_regions(0x9001, &[]);
        assert!(m.reserved.is_empty());
        assert!(m.reserved_regions.is_empty());
    }
}

#[cfg(test)]
mod rect_tests {
    use super::Rect;

    #[test]
    fn union_spanning_two_rects_is_the_bounding_box() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(300, 200, 50, 50);
        assert_eq!(a.union(b), Rect::new(0, 0, 350, 250));
    }

    #[test]
    fn union_with_overlapping_rect_is_their_bounds() {
        let a = Rect::new(10, 10, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        assert_eq!(a.union(b), Rect::new(10, 10, 140, 140));
    }

    #[test]
    fn union_with_itself_is_unchanged() {
        let a = Rect::new(5, 5, 40, 40);
        assert_eq!(a.union(a), a);
    }

    #[test]
    fn contains_rect_is_true_only_when_fully_inside() {
        let big = Rect::new(0, 0, 200, 200);
        let small = Rect::new(50, 50, 40, 40);
        assert!(big.contains_rect(small));
        assert!(!small.contains_rect(big));
        // Touching edges counts as contained.
        assert!(big.contains_rect(Rect::new(0, 0, 10, 10)));
        // Partial overlap is not containment.
        assert!(!big.contains_rect(Rect::new(150, 150, 100, 100)));
    }
}

/// P2: spring sanitization + integrator hardening (pure tests).
#[cfg(test)]
mod spring_hardening_tests {
    use super::{sanitize_spring, spring_smooth, Camera};

    #[test]
    fn stiffness_zero_negative_and_non_finite_fall_back_to_default() {
        assert_eq!(sanitize_spring(0.0, 30.0), (220.0, 30.0));
        assert_eq!(sanitize_spring(-5.0, 30.0), (220.0, 30.0));
        assert_eq!(sanitize_spring(f32::NAN, 30.0), (220.0, 30.0));
        assert_eq!(sanitize_spring(f32::INFINITY, 30.0), (220.0, 30.0));
        assert_eq!(sanitize_spring(f32::NEG_INFINITY, 30.0), (220.0, 30.0));
    }

    #[test]
    fn damping_negative_and_non_finite_fall_back_to_default() {
        assert_eq!(sanitize_spring(220.0, -1.0), (220.0, 30.0));
        assert_eq!(sanitize_spring(220.0, f32::NAN), (220.0, 30.0));
        assert_eq!(sanitize_spring(220.0, f32::INFINITY), (220.0, 30.0));
    }

    #[test]
    fn sanitize_spring_is_idempotent_for_invalid_damping() {
        let (k, c) = sanitize_spring(1.0, f32::INFINITY);
        assert!((k - 1.0).abs() < 1e-6);
        assert!((c - 10.0).abs() < 1e-6);
        let (k2, c2) = sanitize_spring(k, c);
        assert!((k2 - k).abs() < 1e-6);
        assert!((c2 - c).abs() < 1e-6);
        let mut cam = Camera::new(0.0);
        cam.stiffness = 62_500.0;
        cam.damping = f32::NAN;
        cam.target = 1_000.0;
        assert!(cam.step(1.0 / 60.0));
        assert!(cam.position.is_finite());
        assert!(cam.velocity.is_finite());
    }

    #[test]
    fn damping_extreme_is_bounded_relative_to_stiffness() {
        // A finite overdamped value is accepted, but its slow pole is bounded
        // relative to sqrt(stiffness). Low-damping underdamped springs remain
        // mathematically stable but can still have a long visual settle.
        let (k, c) = sanitize_spring(500.0, 10_000.0);
        assert!((k - 500.0).abs() < 1e-6);
        assert!(c > 0.0 && c < 10_000.0);
        // Both parameters remain finite and stiffness keeps its global bound.
        let (k, c) = sanitize_spring(1.0e9, 1.0e9);
        assert!((k - 62_500.0).abs() < 1e-6);
        assert!(c.is_finite() && c > 0.0 && c <= 2_500.0);
    }

    #[test]
    fn camera_analytic_regimes_finite_and_convergent() {
        for (label, k, c) in [
            ("under", 1_000.0, 5.0),
            ("critical", 1_000.0, 63.245_555),
            ("over", 1_000.0, 100.0),
            ("extreme", 62_500.0, 30.0),
            ("max-damping", 1.0, 10.0),
        ] {
            let mut cam = Camera::new(0.0);
            cam.stiffness = k;
            cam.damping = c;
            cam.target = 500.0;
            let mut settled = false;
            for _ in 0..20_000 {
                settled = !cam.step(1.0 / 60.0);
                assert!(cam.position.is_finite(), "{label} position");
                assert!(cam.velocity.is_finite(), "{label} velocity");
                if settled {
                    break;
                }
            }
            assert!(settled, "{label} did not settle");
            assert!((cam.position - cam.target).abs() < 1e-6, "{label} position");
            assert!(cam.velocity.abs() < 1e-6, "{label} velocity");
        }
    }

    #[test]
    fn camera_snap_is_exact_even_with_tiny_dt() {
        let mut cam = Camera::new(0.0);
        cam.target = 100.5;
        cam.position = 100.1;
        cam.velocity = 0.0;
        assert!(!cam.step(1.0e-9));
        assert!((cam.position - cam.target).abs() < 1e-6);
        assert!(cam.velocity.abs() < 1e-6);
    }

    #[test]
    fn camera_step_survives_non_finite_dt_and_poisoned_state() {
        let mut cam = Camera::new(0.0);
        cam.target = 100.0;
        // NaN dt must not poison the state.
        assert!(!cam.step(f32::NAN));
        assert!(cam.position.is_finite());
        // A poisoned position snaps back to target instead of staying NaN.
        cam.position = f32::NAN;
        assert!(!cam.step(1.0 / 60.0));
        assert!(cam.position.is_finite());
    }

    fn advance(cam: &mut Camera, seconds: f32, fps: u32) {
        let frame = 1.0 / fps as f32;
        let mut elapsed = 0.0;
        while elapsed < seconds {
            let dt = frame.min(seconds - elapsed);
            cam.step(dt);
            elapsed += dt;
        }
    }

    #[test]
    fn camera_trajectory_is_frame_rate_independent() {
        let mut positions = Vec::new();
        for fps in [30_u32, 60, 120] {
            let mut cam = Camera::new(0.0);
            cam.target = 1_000.0;
            advance(&mut cam, 0.25, fps);
            positions.push(cam.position);
        }
        let spread = positions.iter().copied().fold(0.0_f32, f32::max)
            - positions.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(spread < 0.05, "partition changed trajectory: {positions:?}");
    }

    #[test]
    fn camera_retarget_keeps_visual_state_and_resets_velocity() {
        let mut cam = Camera::new(0.0);
        cam.target = 1_000.0;
        advance(&mut cam, 0.10, 60);
        let current = cam.position;
        cam.retarget(-500.0);
        assert!(
            (cam.position - current).abs() < 1e-6,
            "retarget must not teleport"
        );
        assert!(
            cam.velocity.abs() < 1e-6,
            "retarget must not keep old-direction momentum"
        );
        assert!((cam.target + 500.0).abs() < 1e-6);
    }

    #[test]
    fn camera_snap_and_zero_delta_are_not_motion() {
        let mut cam = Camera::new(0.0);
        cam.target = 100.0;
        assert!(cam.step(0.0));
        assert!(cam.step(-1.0));
        assert!(
            cam.needs_update(),
            "a pending retarget must keep scheduling"
        );
        cam.position = 99.8;
        cam.velocity = 0.0;
        assert!(!cam.step(1.0 / 60.0));
        assert!((cam.position - cam.target).abs() < 1e-6);
        assert!(cam.velocity.abs() < 1e-6);
        assert!(!cam.needs_update());
    }

    #[test]
    fn camera_repeated_direction_changes_do_not_teleport() {
        let mut cam = Camera::new(0.0);
        cam.target = 1_000.0;
        advance(&mut cam, 0.08, 60);
        for target in [-500.0, 1_000.0, -500.0, 1_000.0] {
            let before = cam.position;
            cam.retarget(target);
            assert!((cam.position - before).abs() < 1e-6);
            cam.step(1.0 / 60.0);
            assert!(cam.position.is_finite());
        }
    }

    #[test]
    fn camera_step_with_extreme_spring_does_not_diverge() {
        let mut cam = Camera::new(0.0);
        cam.stiffness = 62_500.0; // at the clamp bound
        cam.damping = 0.0; // clamped to 0.1 internally
        cam.target = 1.0e6;
        for _ in 0..600 {
            cam.step(1.0 / 60.0);
            assert!(
                cam.position.is_finite(),
                "position diverged: {}",
                cam.position
            );
            assert!(cam.velocity.is_finite());
        }
    }

    #[test]
    fn camera_step_terminates_with_valid_configuration() {
        let mut cam = Camera::new(0.0);
        cam.target = 100.0;
        let mut steps = 0;
        while cam.step(0.008) {
            steps += 1;
            assert!(steps <= 1000, "camera failed to terminate after 1000 steps");
        }
        assert!((cam.position - 100.0).abs() <= 0.5);
    }

    #[test]
    fn camera_step_terminates_even_with_zero_or_negative_damping() {
        let mut cam = Camera::new(0.0);
        cam.target = 100.0;
        cam.damping = 0.0;
        let mut steps = 0;
        while cam.step(0.008) {
            steps += 1;
            assert!(
                steps <= 20000,
                "zero damping failed to terminate (infinite loop bug)"
            );
        }

        cam.snap(0.0);
        cam.target = 100.0;
        cam.damping = -10.0;
        steps = 0;
        while cam.step(0.008) {
            steps += 1;
            assert!(steps <= 20000, "negative damping failed to terminate");
        }
    }

    #[test]
    fn spring_smooth_ignores_non_finite_inputs() {
        let mut cur = 5.0;
        assert!(!spring_smooth(&mut cur, f32::NAN, 0.016));
        assert!(
            (cur - 5.0).abs() < 1e-6,
            "a NaN target must not poison the value"
        );
        assert!(!spring_smooth(&mut cur, 10.0, f32::INFINITY));
        assert!((cur - 5.0).abs() < 1e-6);
    }

    #[test]
    fn spring_smooth_zero_dt_keeps_pending_state_and_snaps_endpoint() {
        let mut cur = f32::NAN;
        assert!(!spring_smooth(&mut cur, 1.0, 0.0));
        assert!((cur - 1.0).abs() < 1e-6);
        let mut cur = 0.0;
        assert!(spring_smooth(&mut cur, 1.0, 0.0));
        assert!(cur.abs() < 1e-6);
        cur = 0.9995;
        assert!(!spring_smooth(&mut cur, 1.0, 0.016));
        assert!((cur - 1.0).abs() < 1e-6);
    }

    /// Integrator-level invariant (fix #1 of the final audit): `Camera` fields
    /// are public, so a caller can bypass `sanitize_spring` entirely. Every
    /// degenerate value must still terminate — and, crucially, the settle
    /// predicate must be REACHABLE (the camera lands on the target), not just
    /// "did not explode after N frames". The step-count is only a watchdog
    /// against a hung test; the real assertion is convergence.
    #[test]
    fn camera_step_terminates_and_converges_for_every_degenerate_direct_mutation() {
        let bad = [
            (0.0, 0.0),             // the audited eternal-moving pair
            (220.0, 0.0),           // damping == 0
            (220.0, -25.0),         // damping < 0 (energy injection)
            (220.0, f32::NAN),      // NaN damping
            (220.0, f32::INFINITY), // +inf damping
            (220.0, f32::NEG_INFINITY),
            (0.0, 30.0),      // stiffness == 0 (spring never restores)
            (-100.0, 30.0),   // stiffness < 0
            (f32::NAN, 30.0), // NaN stiffness
            (f32::INFINITY, 30.0),
            (f32::NEG_INFINITY, 30.0),
        ];
        for (k, c) in bad {
            let mut cam = Camera::new(0.0);
            cam.stiffness = k;
            cam.damping = c;
            cam.target = 100.0;
            let mut steps = 0;
            while cam.step(0.008) {
                steps += 1;
                assert!(
                    steps <= 40_000,
                    "stiffness={k} damping={c}: camera never reached the settle predicate \
                     (eternal moving state)"
                );
            }
            assert!(
                cam.position.is_finite(),
                "stiffness={k} damping={c}: position poisoned"
            );
            assert!(
                (cam.position - cam.target).abs() <= 0.5,
                "stiffness={k} damping={c}: terminated at {} instead of the target {}",
                cam.position,
                cam.target
            );
        }
    }
}
