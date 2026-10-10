//! Pure domain model — authoritative logical state for placement, focus,
//! workspace membership, layout geometry, floats, cameras, and reservations.
//!
//! Everything here is `std`-only, `unsafe`-free, and deterministic: no X11,
//! no GL, no layout implementation, and no config types. That is what keeps
//! every transition testable without an X server or a GPU. Predicates that
//! genuinely need the layout live in the `maverick` crate behind
//! `StateExt` (e.g. the covering-fullscreen window) rather than here.
//!
//! # Ownership
//!
//! - Core owns `State` and all logical placement (`Client`, `Monitor`,
//!   `Workspace`, `Column`, `Camera`, `ReservedRegion`). The backend mirrors
//!   X11 state into the core and applies `Client::geom` via `ConfigureWindow`.
//!   Neither mutates `State` outside the command pipeline.
//! - `Client::geom` is WM-authoritative for floating windows and is the
//!   projected result of arrangement for tiled ones; `Client::saved_geom` and
//!   `FullscreenSnapshot` are transition stores, not the current geometry.
//!   `Monitor::workarea` is always derived from `Monitor::screen` minus
//!   `ReservedArea` (see `Monitor::recalc_geometry`).
//! - [`WindowId`] is backend-agnostic (`u32`) and stable for the lifetime of a
//!   managed window.
//!
//! # Invariants
//!
//! Checked by [`State::check_invariants`], which `State::assert_invariants`
//! runs after every transition in debug builds; see the crate docs for the
//! full list.

use std::collections::HashMap;

/// Backend-agnostic window identifier used throughout the core domain model.
///
/// A plain `u32`, not an alias for x11rb's `Window`: the core must not name
/// any X11 protocol type. The X11 backend's `Window` is itself a u32 XID and
/// converts losslessly at the backend's edges (`as Window` / `as WindowId`);
/// another backend would map its own surface handles onto the same id space
/// instead. The frontier is strict — the core speaks `WindowId`, the backend
/// converts.
///
/// # Invariant
///
/// Ids are stable for the lifetime of a managed window: the backend must not
/// reuse an id after unmanaging, which X11 guarantees anyway because XIDs are
/// never recycled within a session. That is what lets `State::clients` use
/// the id as a key and lets focus stacks and deferred-focus slots reference
/// windows by id without stale-reference risk.
pub type WindowId = u32;

/// Screen-aligned rectangle in pixel coordinates.
///
/// The authoritative rectangle type for the whole domain model. `x`/`y` are
/// `i32` because a window may legitimately sit partly off-screen during an
/// interactive move; `w`/`h` are `u32` so a degenerate size is unrepresentable
/// at the type level, and every producer clamps to at least 1px.
///
/// Coordinates are absolute X11 screen coordinates, so a secondary output can
/// have a non-zero `x`/`y` origin. The backend owns the real `xcb_window_t`
/// geometry and `_NET_FRAME_EXTENTS`; this type carries no X11 state.
///
/// # Invariants
///
/// - `w >= 1` and `h >= 1` for every arranged window, so X11 never receives a
///   zero-area rect (which the server silently drops).
/// - Hit-testing (pointer warp, input focus) and the projection read the same
///   values; any divergence would make a click land on the wrong window.
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
    ///
    /// Reads the saturated [`Self::right`] / [`Self::bottom`] edges, so a hostile
    /// `w`/`h` (a client-reported size, a `_NET_WM_STRUT` CARDINAL) can neither
    /// wrap the coordinate space nor report a point inside the rect as outside
    /// it. A rect whose far edge is past the coordinate limit simply saturates:
    /// every representable coordinate is then inside it, which is what the box
    /// it describes says.
    #[inline]
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    /// Area in pixels (`w * h` as `u64` to avoid `u32` overflow).
    #[inline]
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
    /// X coordinate of the right edge, `x + w`, saturated to the `i32` range.
    ///
    /// The sum is formed in `i64` and the *result* is saturated, never the
    /// operand. `w` spans the whole `u32` range while `x` is an `i32` screen
    /// coordinate, so `x + w` can leave the `i32` range in either direction;
    /// saturating keeps `contains` and `contains_rect` from wrapping a hostile
    /// extent around the coordinate space. Narrowing the *extent* first instead
    /// would also never wrap, but it would report an edge short of the real one
    /// for every rect whose origin is far enough left: a rect at `x = -2e9`
    /// that is `3e9` wide has a right edge of `+1e9`, which `i32` can express,
    /// while a helper that clamps `w` to `i32::MAX` first reports `+147 483 647`
    /// — two billion pixels of a real window treated as outside it, which the
    /// pointer hit-test, `State::mon_at` and the occlusion cull all read.
    #[inline]
    pub fn right(&self) -> i32 {
        (i64::from(self.x) + i64::from(self.w)).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32
    }
    /// True when `other` is entirely inside `self`. Used for occlusion culling:
    /// a window completely behind a single opaque window above it is hidden.
    #[inline]
    pub fn contains_rect(&self, other: Rect) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }
    /// Y coordinate of the bottom edge, `y + h`, saturated on the same terms as
    /// [`Self::right`].
    #[inline]
    pub fn bottom(&self) -> i32 {
        (i64::from(self.y) + i64::from(self.h)).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32
    }
}

/// Bit-packed flags attached to every `Client`, controlling its layout and
/// presentation behaviour. Each bit is an independent policy, set/cleared by
/// the WM, window rules, and commands (`ToggleFloat`, `ToggleFullscreen`, …).
/// The meaning of every bit is documented on its constant below.
///
/// # Invariants
///
/// - Bits 0-2 and 5-8 are in use; bits 3-4 and bits >= 9 are reserved. The
///   gaps are deliberate: a reserved bit keeps every remaining constant on
///   the value its protocol names, so `_NET_WM_STATE` and ICCCM correspondence
///   is not disturbed by a removal. Bit 3 held the `WM_HINTS` input hint,
///   which is not a presentation policy but the client's own declaration
///   (`Client::wants_input`, the single authority for focus eligibility) and
///   so is not a flag; it joins bit 4 as reserved rather than shifting the
///   constants above it.
/// - `MAXIMIZED` is the union `MAXIMIZED_V | MAXIMIZED_H`, and `has()` tests
///   bit *overlap*, so `has(MAXIMIZED)` is already true when a single axis is
///   set. Callers must test both axes (`is_maximized_v() && is_maximized_h()`).
/// - A sticky window is treated as floating: it is excluded from the column
///   layout and shown on every workspace of its monitor.
#[derive(Debug, Clone, Copy, Default)]
pub struct WinFlags(u16);
impl WinFlags {
    /// Window participates in `Workspace::floats` (`Client::geom` is authoritative).
    pub const FLOAT: u16 = 1 << 0;
    /// Window has requested fullscreen; how that request is *presented* is
    /// governed by `Client::fullscreen_policy`.
    pub const FULLSCREEN: u16 = 1 << 1;
    /// Urgent hint — visual border indicator that attention is needed.
    pub const URGENT: u16 = 1 << 2;
    /// Maximized *vertically* — `_NET_WM_STATE_MAXIMIZED_VERT`. The window's
    /// height (and y) come from the workarea; its width and x stay whatever the
    /// layout gave it. Kept as an axis of its own because EWMH treats the two as
    /// independent states and clients do request only one of them.
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

/// ICCCM `WM_NORMAL_HINTS` size constraints for one window.
///
/// Fields are the raw hints as reported by the client; `valid` says whether any
/// hint was actually set. The layout clamps tiled geometry against them.
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
    // `XSizeHints.flags` bits (ICCCM 4.1.2.3 / `X11/Xutil.h`). They are the wire
    // contract of `WM_NORMAL_HINTS`; the parser and every reader below share
    // these constants so a bit test can never drift from the word it indexes.
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

/// One vertical stack in the scrolling ribbon (`Workspace::columns`).
///
/// Each column holds one or more windows stacked top-to-bottom. Its `weight` is
/// its width as a fraction of the workarea width, independent of every other
/// column — weights do not sum to 1.0. That is what makes the layout a true
/// scrolling ribbon rather than fit-to-screen: adding, growing, or removing a
/// column never resizes its neighbours, the total width simply grows or
/// shrinks, and `Camera` scrolls to keep the focused column in view. Coordinates
/// are always derived from `(col_x + scroll, row_y)` and never stored mutably,
/// so no drift can accumulate.
///
/// # Ownership
///
/// Owned by `Workspace::columns`. Windows are referenced by `WindowId` and must
/// also exist in `State::clients` (checked by `State::check_invariants`).
///
/// # Invariants
///
/// - `weight` is finite and within `[0.05, 1.0]`; repaired by
///   `Workspace::rebalance_weights`, and kept inside the band by
///   [`band_weight`] on every path that derives a weight from another one.
/// - `focused < windows.len()` when non-empty.
#[derive(Debug, Clone)]
pub struct Column {
    /// Windows top-to-bottom in this column.
    pub windows: Vec<WindowId>,
    /// This column's width as a fraction of the workarea width.
    pub weight: f32,
    /// Index into `windows` that has focus within this column.
    pub focused: usize,
}

/// The documented `Column::weight` band (crate invariant F), in one place: the
/// lower bound keeps a column usable after a chain of splits, the upper one is
/// the full workarea width.
const MIN_COLUMN_WEIGHT: f32 = 0.05;
const MAX_COLUMN_WEIGHT: f32 = 1.0;

/// Clamp a *derived* weight — one computed from another column's weight, as the
/// split does — into the documented band, the way the paths that take a weight
/// from the caller already do.
///
/// A non-finite weight cannot come out of the arithmetic that produces these
/// (a finite source weight times a finite ratio is finite), so this only has to
/// bound the two ends; `NaN` is mapped to the floor anyway, because
/// `f32::clamp` returns `NaN` unchanged and a poisoned width is exactly what
/// `Workspace::rebalance_weights` and `add_tiled` exist to keep out of the tree.
#[inline]
fn band_weight(weight: f32) -> f32 {
    if weight.is_nan() {
        return MIN_COLUMN_WEIGHT;
    }
    weight.clamp(MIN_COLUMN_WEIGHT, MAX_COLUMN_WEIGHT)
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
        Self {
            windows: Vec::new(),
            weight: 1.0,
            focused: 0,
        }
    }
}

/// 1D scroll camera for the ribbon layout.
///
/// A single scroll offset in px. The ribbon scrolls by writing this number and
/// re-projecting; there is no interpolation, no velocity and no frame loop, so
/// `position` is the geometry rather than a value chasing it.
///
/// # Invariants
///
/// - `position` is finite. Every mutator refuses a non-finite value, so a
///   poisoned camera cannot reach a `ConfigureWindow` or a hit-test.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Scroll offset in px.
    pub position: f32,
}

impl Camera {
    /// Create a camera parked at `pos`.
    pub fn new(pos: f32) -> Self {
        Self {
            position: if pos.is_finite() { pos } else { 0.0 },
        }
    }

    /// Move the ribbon to `target`. A non-finite value is refused and the
    /// current offset is kept, so one poisoned calculation cannot scroll the
    /// desktop into the void.
    pub fn retarget(&mut self, target: f32) {
        if target.is_finite() {
            self.position = target;
        }
    }

    /// Park the camera at `pos`, mapping a non-finite value to `0.0` rather
    /// than keeping the old offset. Used where a settled value is required and
    /// there is no previous offset worth preserving.
    pub fn snap(&mut self, pos: f32) {
        self.position = if pos.is_finite() { pos } else { 0.0 };
    }
}

/// Focus pointer within a workspace's column tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    /// Index of the focused column in `Workspace::columns`.
    pub column_idx: usize,
}

/// Stable logical identity of a **View**.
///
/// # Contract
///
/// A `ViewId` names a View and nothing else. It is deliberately *not*:
///
/// - a carousel position — View 7 may sit at position 0, 3 or 7, and moving it
///   does not change its id;
/// - an X11 window id — a View owns no window and creates none, so a `ViewId`
///   never reaches the wire as an XID and no XID is ever a `ViewId`;
/// - a layout tag — it survives `LayoutKind` changes untouched.
///
/// # Invariants
///
/// - **Monotonic and never reused.** [`Carousel::mint`] hands out strictly
///   increasing values for the lifetime of a monitor and never repeats one, so a
///   deleted View's id can never name a later View. This is why membership can
///   be keyed by id: a stale `Client::workspace` is *detectable* instead of
///   silently re-pointing at whichever View inherited the old position.
/// - **Per-monitor scope.** Each monitor owns a [`Carousel`] and therefore its
///   own id space, exactly as each monitor owns its own `Vec<Workspace>`. The
///   composite `(monitor, ViewId)` is a client's placement coordinate, the direct
///   analogue of the positional `(monitor, workspace index)` pair it replaces.
/// - **Callers mint through the carousel only.** [`ViewId::new`] exists so tests
///   and the bootstrap paths can state an id; nothing else may invent one, or
///   monotonicity (and therefore membership safety) stops holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(u32);

impl ViewId {
    /// Wrap a raw id. Only for tests and bootstrap; see [`Self`].
    pub const fn new(raw: u32) -> Self {
        ViewId(raw)
    }
    /// The raw value. Only for diagnostics, tests and the config/IPC boundary.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for ViewId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "view#{}", self.0)
    }
}

/// Logical carousel over a monitor's Views: which View is **current**, which is
/// the **origin**, and how to move between them.
///
/// # Contract
///
/// The Carousel is *state*, not a rendering abstraction. Every operation is
/// instantaneous and logical: it writes a [`ViewId`] and returns. There is no
/// animation state, no transition state, no interpolation, no timer, no frame
/// scheduling, no compositor dependency and no X11 call — a View is a logical
/// container of clients, not a window, a pixmap or a slot.
///
/// It deliberately knows **nothing** about `LayoutKind`. There is no
/// `match layout { .. }` here and none may be added: navigation must produce
/// the same answer whichever layout is installed, and the layout must never be
/// consulted to decide *which* View is current.
///
/// # Invariants
///
/// - `current` and `origin` are `Some` **exactly when at least one View
///   exists**, and then both name an existing View. This is what makes
///   [`Self::return_to_origin`] total: it can never select a deleted View.
/// - Both pointers are repaired on every lifecycle change ([`Self::attach`] /
///   [`Self::detach`]), so no operation below can leave a dangling identifier.
/// - Navigation refuses (`false`) when `views` is empty rather than inventing a
///   fallback; invalid state is surfaced by `State::check_invariants`, not
///   papered over here.
///
/// # Ownership
///
/// Existence and order of Views belong to `Monitor::workspaces`. The Carousel is
/// handed that list and never stores a second copy — the position it navigates
/// over is derived on the call, so there is exactly one membership truth.
#[derive(Debug, Clone)]
pub struct Carousel {
    /// The currently selected View.
    current: Option<ViewId>,
    /// The explicit return point, pinned when the carousel was first populated.
    origin: Option<ViewId>,
    /// Next never-reused id. Monotonic; only [`Self::mint`] advances it.
    next_id: u32,
}

impl Default for Carousel {
    fn default() -> Self {
        Self::new()
    }
}

/// The View that inherits carousel position `pos` once the View that used to sit
/// there is gone: the successor if there is one, otherwise the new tail.
///
/// # Why
///
/// Removing a View shifts every later View down by one, so "keep the same
/// position" is not a stable choice — it would silently jump the user to
/// whichever View happens to land there. The successor keeps the user where they
/// were looking, and the tail fallback is what makes removing the *last* View a
/// well-defined step backwards rather than a special case.
fn successor(views: &[Workspace], pos: usize) -> Option<ViewId> {
    views.get(pos).or_else(|| views.last()).map(|w| w.id)
}

impl Carousel {
    /// A carousel with no Views: no current, no origin, next id 0.
    pub const fn new() -> Self {
        Carousel {
            current: None,
            origin: None,
            next_id: 0,
        }
    }

    /// The currently selected View, or `None` when no View exists.
    pub const fn current(&self) -> Option<ViewId> {
        self.current
    }

    /// The return point. `Some` exactly when at least one View exists.
    pub const fn origin(&self) -> Option<ViewId> {
        self.origin
    }

    /// True when no View exists, i.e. `current`/`origin` are both `None`.
    pub const fn is_empty(&self) -> bool {
        self.current.is_none() && self.origin.is_none()
    }

    /// Mint the id of a newly created View: strictly increasing for the lifetime
    /// of this monitor and never reused.
    ///
    /// Minting only allocates the *identity*; it does not register a View. The
    /// caller pushes the View and then calls [`Self::attach`] so the pointers are
    /// repaired in one place.
    pub fn mint(&mut self) -> ViewId {
        let id = ViewId(self.next_id);
        // `saturating_add` rather than `+`: a wrapped id would repeat an earlier
        // one and silently re-point stale `Client::workspace` values. Saturating
        // keeps them distinct instead, and 4 billion Views on one monitor is not
        // reachable in practice.
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Register `id` as an existing View after it was pushed onto `views`.
    ///
    /// A newly created View only takes over `current`/`origin` when it is the
    /// *first* one — that is the empty → non-empty transition. Creating a View
    /// while others exist must not move the user away from what they were looking
    /// at; membership changes and navigation are separate concerns.
    pub fn attach(&mut self, id: ViewId) {
        if self.current.is_none() {
            self.current = Some(id);
        }
        if self.origin.is_none() {
            self.origin = Some(id);
        }
    }

    /// Repair both pointers after the View that used to sit at carousel position
    /// `pos` was removed, where `removed` is its id and `views` is the list
    /// **after** removal.
    ///
    /// A pointer naming the removed View adopts [`successor`]; any other pointer
    /// is left alone (it still names an existing View, and its *position* simply
    /// shifted, which is why membership is keyed by id and not by index). Removing
    /// the last View defines the empty state explicitly: both pointers become
    /// `None` rather than dangling.
    ///
    /// `current` and `origin` are repaired independently, so removing the origin
    /// while sitting on a different View moves only the origin.
    pub fn detach(&mut self, views: &[Workspace], removed: ViewId, pos: usize) {
        if views.is_empty() {
            self.current = None;
            self.origin = None;
            return;
        }
        let fallback = successor(views, pos);
        if self.current == Some(removed) {
            self.current = fallback;
        }
        if self.origin == Some(removed) {
            self.origin = fallback;
        }
    }

    /// Carousel position of the current View.
    pub fn current_index(&self, views: &[Workspace]) -> Option<usize> {
        views.iter().position(|w| Some(w.id) == self.current)
    }

    /// Carousel position of `id`.
    pub fn index_of(&self, views: &[Workspace], id: ViewId) -> Option<usize> {
        views.iter().position(|w| w.id == id)
    }

    /// Select `id`. Returns `false` (leaving the carousel untouched) when `id`
    /// names no existing View, so a stale id can never become current.
    pub fn goto(&mut self, views: &[Workspace], id: ViewId) -> bool {
        if self.index_of(views, id).is_none() {
            return false;
        }
        self.current = Some(id);
        true
    }

    /// Advance one step around a circular carousel, so `next` of the last View is
    /// the first. Instantaneous: one pointer write, no transition state.
    ///
    /// Returns `false` when there is no View to move to (empty carousel, or a
    /// `current` that names no existing View — a broken invariant the caller is
    /// expected to have repaired).
    pub fn next(&mut self, views: &[Workspace]) -> bool {
        self.step(views, 1)
    }

    /// Step one View backwards around a circular carousel, so `previous` of the
    /// first View is the last. Same contract as [`Self::next`].
    pub fn previous(&mut self, views: &[Workspace]) -> bool {
        self.step(views, -1)
    }

    fn step(&mut self, views: &[Workspace], delta: isize) -> bool {
        let n = views.len();
        if n == 0 {
            return false;
        }
        let Some(i) = self.current_index(views) else {
            return false;
        };
        // Wrapping arithmetic in `isize` keeps a single expression for both
        // directions: `(i + 1) % n` and `(i + n - 1) % n` would each need their
        // own guard, and only the latter overflows `usize` when `i == 0`.
        let j = (i as isize + delta).rem_euclid(n as isize) as usize;
        self.current = Some(views[j].id);
        true
    }

    /// Select the origin.
    ///
    /// The containment check is the contract, not a defensive nicety: `origin` is
    /// repaired on every detach, so an origin that names no View is a broken
    /// invariant — and returning `false` here is what keeps
    /// `return_to_origin()` from ever selecting a deleted View.
    pub fn return_to_origin(&mut self, views: &[Workspace]) -> bool {
        let Some(origin) = self.origin else {
            return false;
        };
        if self.index_of(views, origin).is_none() {
            return false;
        }
        self.current = Some(origin);
        true
    }
}

/// One logical View on a monitor: the set of clients that belong together, plus
/// the presentation state that belongs to that set. Holds both tiled columns and
/// floating windows.
///
/// A View is *not* an X11 window: it owns no window, requests no pixmap, and
/// needs no compositor, rendering surface or slot window to exist. Creating one
/// is a pure logical transition.
///
/// # Ownership
///
/// Owned by `Monitor::workspaces`; every `WindowId` in `columns` or `floats`
/// also lives in `State::clients`, and `Client::workspace` must name this View's
/// `id`. `presented_maximize` is derived state kept in sync by
/// `State::sync_presented_maximize`.
///
/// # Invariants
///
/// - `id` is unique within the owning monitor and was minted by that monitor's
///   [`Carousel`] (see [`ViewId`]).
/// - `focus.column_idx < columns.len()` when non-empty, and every
///   `Column::focused` is in range.
/// - `camera` is not the source of truth for geometry; arrangement derives
///   positions from it.
/// - `presented_maximize` (when `Some`) names a maximized client on this
///   View, and on the monitor's `focused` window while this is the active View.
/// - Membership is a property of this View alone: changing `layout` (or any
///   other field) never moves a client in or out of it.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Stable logical identity of this View. Never reused, never a position.
    pub id: ViewId,
    /// Tiled columns (scrolling ribbon). Floats are deliberately absent: they are
    /// excluded from tiled layout input by living here instead of in `columns`.
    pub columns: Vec<Column>,
    /// Focused column pointer.
    pub focus: Focus,
    /// Scroll camera (only meaningful in `LayoutKind::Column` / ribbon mode).
    pub camera: Camera,
    /// Floating windows on this View (excluded from column layout; `Client::geom`
    /// authoritative).
    pub floats: Vec<WindowId>,
    /// Layout mode for this specific View — independent of every other View.
    pub layout: LayoutKind,
    /// Overview (spatial-navigation viewport) mode active for this View.
    ///
    /// A flag plus the scale the mode was entered at. The scale is computed
    /// **once**, on entry (`layout::overview_entry_scale_for`), and the
    /// projection reads it back verbatim (`layout::view_alpha`): navigating
    /// between columns pans the camera, it never re-derives a smaller scale to
    /// fit the whole ribbon. The client count therefore shapes the *content*
    /// that can be scrolled through, never the scale of each navigation step.
    /// Leaving the mode restores the settled view (`1.0`) because the stored
    /// value is cleared on exit.
    pub overview: bool,
    /// The view scale Overview was entered at (`1.0` = full-size tiles).
    ///
    /// Written exactly once per Overview session, by the entering command, and
    /// read by the projection on every pass while `overview` is set. No
    /// navigation, resize, map or unmap path writes it: that single-writer rule
    /// is what keeps the scale fixed while the viewport moves. Entering at
    /// `1.0` would leave the desktop pixel-identical to the settled view, so
    /// the entry scale is a real reduction (`Cfg::overview_scale`,
    /// `layout::DEFAULT_OVERVIEW_SCALE`) and this field never holds it for an
    /// ordinary session. `1.0` outside Overview (the settled view projects at
    /// exactly `1.0`).
    pub overview_scale: f32,
    /// Logical client rectangles held while Overview projects their images.
    /// These are never sent to X11 with the view scale applied.
    pub overview_rects: HashMap<WindowId, (Rect, u32)>,
    /// Viewport display mode (normal vs zoomed-in inspection). Orthogonal to
    /// `overview` and to window fullscreen.
    pub viewport_mode: ViewportMode,
    /// Page-zoom factor when `viewport_mode == Zoomed` (1.0 = no zoom, >1 = the
    /// ribbon is enlarged). Fed into `ribbon_geom`'s `alpha` so columns grow;
    /// there is deliberately no upper clamp (unlike Overview's lower one), so a
    /// value > 1 enlarges instead of shrinking.
    pub page_zoom: f32,
    /// The window currently presented as the **maximize** overlay on this
    /// View (`None` when no maximized window owns it). Explicitly stored
    /// rather than re-derived from `Monitor::focused` at every read site; kept
    /// in sync with the focused window's maximize flags by
    /// `State::sync_presented_maximize`. `Monitor::focused` itself stays purely
    /// "the logical focus".
    pub presented_maximize: Option<WindowId>,
}

impl Workspace {
    /// Create an empty View carrying `id`.
    pub fn new(id: ViewId) -> Self {
        Self {
            id,
            columns: Vec::new(),
            focus: Focus { column_idx: 0 },
            camera: Camera::new(0.0),
            floats: Vec::new(),
            layout: LayoutKind::Column,
            overview: false,
            overview_scale: 1.0,
            overview_rects: HashMap::new(),
            viewport_mode: ViewportMode::Normal,
            page_zoom: 1.0,
            presented_maximize: None,
        }
    }

    /// True when no tiled columns and no floats.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.floats.is_empty()
    }

    /// Focused window from the focused column, if any.
    pub fn focused_win(&self) -> Option<WindowId> {
        self.columns.get(self.focus.column_idx)?.focused_win()
    }

    /// True-scroll insert: the window becomes a sibling column to the RIGHT of
    /// the focused column (or the sole column when the workspace is empty), with
    /// width `column_width` — a fraction of the workarea, clamped to
    /// `[0.1, 1.0]`. The fraction is taken as given; callers pass
    /// `cfg.column_width` and no division happens here.
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

    /// Guard against degenerate weights (zero/negative/NaN from float drift, a
    /// caller that never set one, or a hostile session restore). Columns are
    /// sized independently in the true-scroll model, so this never redistributes
    /// weight between them: it only gives a broken column a sane fallback width.
    pub fn rebalance_weights(&mut self) {
        for col in &mut self.columns {
            if !col.weight.is_finite() || col.weight <= 0.0 {
                col.weight = 0.5;
            }
        }
    }

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
        self.overview_rects.remove(&win);
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

    /// Drop columns that no longer hold a window, keeping `focus.column_idx`
    /// pointing at the same column.
    pub fn cleanup_empty_columns(&mut self) {
        let target = self.focus.column_idx;
        // Every dropped column that sat strictly *before* the focused one shifts
        // that index down by one, so the pointer must shift by the same amount
        // and not merely be clamped: a bare clamp would leave focus on whatever
        // column happened to occupy the clamped index, which mis-centers the
        // camera (`ideal_scroll`) and lets `best_focus`/`focused_win` move focus
        // to a neighbour.
        //
        // The focused column can itself be among the dropped ones, and then the
        // count above misses it. `add_tiled` puts a new column at
        // `focus.column_idx + 1` and moves the focus onto it, so removing that
        // column — which is exactly what a float↔fullscreen round trip does —
        // drops the focused column and leaves the pointer one place too far
        // right. That is a drift rather than a clamp, so it survives the
        // `.min()` below and repeats on every use of the toggle: the camera ends
        // up centring a neighbour and `best_focus` names a window the user never
        // selected. Counting the focused column itself makes the pair symmetric,
        // so the pointer lands back on the column it started from.
        let focus_is_dropped = self
            .columns
            .get(target)
            .is_some_and(|c| c.windows.is_empty());
        let removed_before = self.columns[..target.min(self.columns.len())]
            .iter()
            .filter(|c| c.windows.is_empty())
            .count()
            + usize::from(focus_is_dropped);

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

        // Dropping a column leaves the survivors' weights short of 1.0, which is
        // harmless in the true-scroll model (each column's width is independent,
        // so the total simply shrinks). A non-finite or non-positive weight
        // would break geometry, so repair those. The survivors are deliberately
        // NOT re-normalized — see `rebalance_weights`.
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

/// What Maverick does with a window's fullscreen requests.
///
/// Fullscreen *state* ("is it fullscreen right now?") is `WinFlags::FULLSCREEN`;
/// policy is the separate question of what the WM does when a window asks, and
/// it is set once from a window rule and never toggled at runtime. Keeping the
/// two apart is what lets a browser's client-side F11 be refused while the
/// user's own `Mod4+F` still works on the very same window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FullscreenPolicy {
    /// Fullscreen behaves like everywhere else: the window becomes a
    /// screen-wide column of the scrolling ribbon — an ordinary ribbon tile, not
    /// an overlay.
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

/// The presentation mode a window was in immediately before it entered
/// fullscreen. Decides what "leave fullscreen" must restore.
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
/// leave.
///
/// A single `Client::saved_geom` cannot serve this role: both the maximize and
/// the fullscreen path write it, so maximizing a window *while* it is
/// fullscreen clobbers the saved pre-fullscreen rect. Capturing the mode
/// together with the rect keeps the two stores independent and lets the
/// transition be restored instead of reconstructed.
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
    /// The **View** this window belongs to, by stable logical identity.
    ///
    /// This is the authoritative membership field: `(monitor, workspace)` is the
    /// composite placement coordinate, and it must agree with the View that
    /// actually references `window` in its `columns`/`floats` (checked by
    /// `State::check_invariants`).
    ///
    /// It is a [`ViewId`], not a carousel position, so removing a View cannot
    /// silently re-point a client at whichever View inherited its slot. It is
    /// orthogonal to `flags`' `FLOAT` bit and to `geom`: floating and geometry are
    /// client properties that a View change must preserve, not touch.
    pub workspace: ViewId,
    /// The window this one is transient for (`WM_TRANSIENT_FOR`), when it was a
    /// known client at manage time. Used by the renderer to keep popups/dialogs
    /// of a fullscreen or maximized window above the presentation overlay.
    pub transient_parent: Option<WindowId>,
    /// `_NET_WM_WINDOW_TYPE` values this window declared, as lowercase atom
    /// names (`"dialog"`, `"utility"`, `"toolbar"`, …). Used by window rules.
    pub window_types: Vec<String>,
    /// Observability-only mirror of the last *desired* rect this client was
    /// arranged to. Written by the render reconcile path; NEVER read for
    /// layout, focus, or overlay decisions.
    pub last_desired: Option<Rect>,
    /// Observability-only mirror of the last *real* geometry the client reported
    /// back via `ConfigureNotify` (X11 Real). Written by the events convergence
    /// path; NEVER read for layout, focus, or overlay decisions.
    pub last_reported: Option<Rect>,
    /// True when the client wants input focus — the ICCCM `WM_HINTS` `input`
    /// field (ICCCM 4.1.2.4), re-read on every rewrite of that property.
    ///
    /// This is the single authority on focus eligibility: a cached mirror of
    /// it cannot be kept honest, because ICCCM 4.1.2 requires the WM to
    /// "retain no memory of the old value" of a client property, while a
    /// flag word can only be OR-ed in and never taken back out.
    ///
    /// `false` is the client's request that the WM not set the X input focus
    /// on its top-level window — the *No Input* and *Globally Active* models
    /// of ICCCM 4.1.7. Maverick honours it by not offering such a window the
    /// focus at all, which ICCCM permits: it constrains the *X input focus*,
    /// and leaves "the method by which the user commands the window manager to
    /// set the focus to a window" to the window manager. Where a window is
    /// already the focus target when it makes the request — a client that
    /// rewrites the property under the focus it holds — the reconcile path
    /// applies the other half of the rule and points the X input focus at
    /// `PointerRoot` while still offering `WM_TAKE_FOCUS`.
    ///
    /// `true` is also the documented WM default for a client that sends no
    /// `WM_HINTS` at all, or sends one with `InputHint` clear (ICCCM 4.1.2.4:
    /// "Window managers are free to assume convenient values for all fields
    /// of the `WM_HINTS` property if a window is mapped without one"), so it
    /// is a statement about what the WM will do, not proof the client asked.
    pub wants_input: bool,
    /// True when the WM has hidden the window (offscreen/minimized).
    pub wm_hidden: bool,
    /// Forces the next `apply_geom` to re-emit its `ConfigureWindow` even when
    /// the computed rect equals `geom`.
    ///
    /// `apply_geom` skips windows whose geometry did not change, which is what
    /// keeps `arrange` cheap. But a *state* transition (entering or leaving
    /// fullscreen/maximized) can produce the very same rect while the window
    /// still has to be reconfigured — the border width changed, or the client
    /// resized itself behind our back. This flag states that requirement without
    /// having to lie about `geom`; `apply_geom` clears it.
    pub geometry_dirty: bool,
    /// Fullscreen transition snapshot: the exact mode and rect the window had
    /// *before* it entered fullscreen, captured by
    /// `apply_fullscreen_topology` and applied verbatim when it leaves. Kept
    /// apart from the shared `saved_geom` so a maximize cannot overwrite it
    /// (see `FullscreenSnapshot`). `None` when the window is not in a
    /// fullscreen transition.
    pub fs_snapshot: Option<FullscreenSnapshot>,
    /// What to do when this window asks for fullscreen. Comes from a window
    /// rule (`deny_fullscreen` / `true_fullscreen`) and never changes at
    /// runtime — it is policy, not state, so it deliberately lives here rather
    /// than as another `WinFlags` bit.
    pub fullscreen_policy: FullscreenPolicy,
    /// Client process id from `_NET_WM_PID`, captured at manage time. `None`
    /// when the client never set it (not all toolkit setups do). Never used for
    /// any WM decision — it is the window → process link that lets external
    /// tools tie a window to `/proc/<pid>`, so it is read-only observability
    /// like `last_desired`/`last_reported`.
    pub pid: Option<u32>,
    /// True while the float's geometry was last claimed by the *client* (a
    /// `ConfigureRequest` this WM adopted verbatim).
    ///
    /// This is the seal that closes the two-authorities loop: the WM adopted
    /// the client's rect bit-for-bit (the only answer that terminates the
    /// conversation), so the next `arrange` must project that same rect back
    /// instead of re-normalizing it (snap to hints + workarea clamp), which
    /// would rewrite it and restart the fight — the client re-requests, the WM
    /// re-writes, and the window "jumps around by itself". While the seal is
    /// set, the float projection is the adopted rect with only protocol-level
    /// sanity applied (`adopt_client_float_geometry`).
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
    /// Create a client for `win` placed on `(mon, view)` with default
    /// geometry/flags.
    pub fn new(win: WindowId, mon: usize, view: ViewId) -> Self {
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
            workspace: view,
            transient_parent: None,
            window_types: Vec::new(),
            last_desired: None,
            last_reported: None,
            wants_input: true,
            wm_hidden: false,
            geometry_dirty: false,
            fs_snapshot: None,
            fullscreen_policy: FullscreenPolicy::Normal,
            pid: None,
            float_client_authority: false,
        }
    }

    /// True when the window is currently floating (`WinFlags::FLOAT`).
    #[inline]
    pub fn is_float(&self) -> bool {
        self.flags.has(WinFlags::FLOAT)
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

// Reservation is modelled in two layers: `ReservedRegion` is the individual,
// trackable reservation (one per dock or bar, tagged with its owner so it can
// be dropped exactly when that window disappears), and `ReservedArea` is the
// per-edge total collapsed from the regions. The layout only ever sees the
// resulting `workarea` (screen − ReservedArea) and never needs to know *what*
// reserved the space — which is what keeps this backend-agnostic: on X11 the
// regions come from each external dock's `_NET_WM_STRUT[_PARTIAL]`, and a
// layer-shell exclusive zone would fill the same structure.

/// Which screen edge a reservation pushes in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
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
    /// Collapse trackable regions into per-edge totals. Reservations on the same
    /// edge stack: two docks that both reserve the top simply add up.
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
/// - `carousel.current` and `carousel.origin` both name an existing View in
///   `workspaces` whenever `workspaces` is non-empty (guaranteed by
///   `Carousel::attach`/`detach` on every lifecycle change, and re-checked by
///   `State::check_invariants`). `ViewId`s are unique within this monitor.
/// - `focus_stack` contains no duplicates and only known clients.
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
    /// This monitor's Views, in carousel order. Existence and order live here;
    /// the [`Carousel`] only names which of them is current and which is origin.
    pub workspaces: Vec<Workspace>,
    /// Navigation state: which View is current, which is the origin.
    pub carousel: Carousel,
    /// Logically focused window on this monitor (WM intent; may be `None`).
    pub focused: Option<WindowId>,
    /// MRU focus stack for this monitor (most-recent last).
    pub focus_stack: Vec<WindowId>,
}

impl Monitor {
    /// Create a monitor with `screen` geometry and `n_tags` empty Views.
    ///
    /// `n_tags` is clamped to at least one, the same floor
    /// [`Self::reconcile_workspaces`] applies: a monitor with no View has no
    /// active View for [`Self::ws`] / [`Self::ws_mut`] to return, so the count
    /// cannot be honoured verbatim without making every later command path fail
    /// on a monitor the backend built correctly from a legal argument. The
    /// backend's RandR/Xinerama detection and session restore both reach this
    /// constructor, so the floor belongs here rather than at each call site.
    ///
    /// The first View minted becomes both the current and the origin View: the
    /// origin is the explicit return point, pinned where the session started on
    /// this monitor.
    pub fn new(screen: Rect, n_tags: usize) -> Self {
        let mut carousel = Carousel::new();
        let workspaces = (0..n_tags.max(1))
            .map(|_| {
                let id = carousel.mint();
                carousel.attach(id);
                Workspace::new(id)
            })
            .collect();
        let mut m = Self {
            screen,
            workarea: screen,
            reserved_regions: Vec::new(),
            reserved: ReservedArea::default(),
            workspaces,
            carousel,
            focused: None,
            focus_stack: Vec::with_capacity(16),
        };
        m.recalc_geometry();
        m
    }

    /// Carousel position of the active View.
    ///
    /// Clamps rather than panicking when the carousel points at nothing (hotplug
    /// / session restore can leave the list empty for one frame; callers repair it
    /// right after). Returns 0 for an empty list, which every caller pairs with a
    /// non-empty check.
    pub fn active_index(&self) -> usize {
        self.carousel
            .current_index(&self.workspaces)
            .unwrap_or(0)
            .min(self.workspaces.len().saturating_sub(1))
    }

    /// Active View (immutable). Clamps a stale pointer to the last View instead of
    /// panicking; panics only if there are zero Views, which violates the
    /// `reconcile_workspaces(max(1))` invariant.
    pub fn ws(&self) -> &Workspace {
        assert!(
            !self.workspaces.is_empty(),
            "Monitor::ws with zero workspaces (reconcile invariant broken)"
        );
        &self.workspaces[self.active_index()]
    }
    /// Active View (mutable). Same clamping contract as [`Self::ws`].
    pub fn ws_mut(&mut self) -> &mut Workspace {
        assert!(
            !self.workspaces.is_empty(),
            "Monitor::ws_mut with zero workspaces (reconcile invariant broken)"
        );
        let i = self.active_index();
        &mut self.workspaces[i]
    }
    /// Fallible accessors for callers that must handle an out-of-range
    /// pointer explicitly instead of relying on the clamp in `ws()`.
    pub fn try_ws(&self) -> Option<&Workspace> {
        self.workspaces.get(self.active_index())
    }
    pub fn try_ws_mut(&mut self) -> Option<&mut Workspace> {
        let i = self.active_index();
        self.workspaces.get_mut(i)
    }

    /// Carousel position of `id`, or `None` when no such View exists.
    pub fn view_index(&self, id: ViewId) -> Option<usize> {
        self.carousel.index_of(&self.workspaces, id)
    }

    /// `ViewId` at carousel position `pos`.
    pub fn view_id(&self, pos: usize) -> Option<ViewId> {
        self.workspaces.get(pos).map(|w| w.id)
    }

    /// Select `id` as the active View. Returns `false` (leaving the monitor
    /// untouched) when `id` names no existing View, so a stale id can never be
    /// installed as current.
    pub fn goto_view(&mut self, id: ViewId) -> bool {
        self.carousel.goto(&self.workspaces, id)
    }

    /// Advance one step around the circular carousel. Purely logical and
    /// instantaneous — see [`Carousel::next`].
    pub fn next_view(&mut self) -> bool {
        self.carousel.next(&self.workspaces)
    }

    /// Step one View backwards around the circular carousel — see
    /// [`Carousel::previous`].
    pub fn previous_view(&mut self) -> bool {
        self.carousel.previous(&self.workspaces)
    }

    /// Select the origin View — see [`Carousel::return_to_origin`].
    pub fn return_to_origin(&mut self) -> bool {
        self.carousel.return_to_origin(&self.workspaces)
    }

    /// Append a new empty View and return its id.
    ///
    /// The View is a pure logical container: no X11 window, pixmap or slot is
    /// created, and the current/origin View are left alone unless this is the
    /// first View (see [`Carousel::attach`]).
    pub fn create_view(&mut self) -> ViewId {
        let id = self.carousel.mint();
        self.workspaces.push(Workspace::new(id));
        self.carousel.attach(id);
        id
    }

    /// Drop the View at carousel position `pos`, repairing `current`/`origin`.
    ///
    /// `pos` is the position **before** removal. Returns the removed View's id.
    ///
    /// This is the single place a View is destroyed, so `detach` is guaranteed to
    /// run and no deletion path can leave `current` or `origin` dangling.
    ///
    /// The View's *clients* are not touched here — a `Monitor` has no access to
    /// `State::clients`, and re-homing them needs both halves. Removal that
    /// actually destroys a populated View therefore goes through
    /// [`State::remove_view`], which does the whole job.
    pub fn remove_view_at(&mut self, pos: usize) -> Option<ViewId> {
        if pos >= self.workspaces.len() {
            return None;
        }
        let removed = self.workspaces[pos].id;
        self.workspaces.remove(pos);
        self.carousel.detach(&self.workspaces, removed, pos);
        Some(removed)
    }

    // Reservation mutations all go through these helpers so `reserved` and
    // `workarea` stay consistent with `reserved_regions`, the single source of
    // truth.

    /// Replace the single region owned by `owner`. `thickness == 0` removes it.
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

    /// Replace *every* region owned by `owner` with `regions`. One dock may
    /// reserve several edges at once (a panel plus a launcher), and calling
    /// `set_reserved_region` once per edge would erase the previous one, so all
    /// edges are written together. An empty list clears the owner.
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
    ///
    /// `reserved` is the per-edge collapse of `reserved_regions` and `workarea`
    /// is `screen` minus those totals, so the workarea is never larger than
    /// `screen` nor anchored outside it.
    ///
    /// `reserved_regions` carries untrusted thickness values (on X11, any
    /// window's `_NET_WM_STRUT[_PARTIAL]` CARDINALs) and `screen` comes from
    /// the backend, so no `u32` input may panic, wrap, or move the workarea
    /// away from the screen it is subtracted from. An edge total larger than
    /// the extent it pushes into collapses to that extent: the workarea is
    /// then empty and sits on the screen's own edge, which is a legitimate
    /// result (a strut can cover the whole screen) — the arrangement pass, not
    /// this one, is what clamps what it presents to a non-zero size.
    pub fn recalc_geometry(&mut self) {
        self.reserved = ReservedArea::from_regions(&self.reserved_regions);
        let r = self.reserved;
        // A total beyond the screen extent is meaningless and must be bounded
        // before it is used, or it can both wrap when summed with the opposite
        // edge and cast to a negative offset that moves the origin outwards.
        let left = r.left.min(self.screen.w);
        let right = r.right.min(self.screen.w);
        let top = r.top.min(self.screen.h);
        let bottom = r.bottom.min(self.screen.h);
        // The i32 origin is computed in i64 so a screen near the coordinate
        // limit cannot wrap the sum into a negative offset.
        let x = (i64::from(self.screen.x) + i64::from(left)).min(i64::from(i32::MAX)) as i32;
        let y = (i64::from(self.screen.y) + i64::from(top)).min(i64::from(i32::MAX)) as i32;
        let w = self.screen.w.saturating_sub(left.saturating_add(right));
        let h = self.screen.h.saturating_sub(top.saturating_add(bottom));
        self.workarea = Rect::new(x, y, w, h);
    }

    /// Grow or shrink the View list to match `n_tags`, preserving View state for
    /// the Views that survive. Growing appends fresh empty Views with newly minted
    /// ids; shrinking drops trailing Views (windows still assigned there are
    /// clamped to the last surviving View by the caller). `n_tags == 0` is
    /// clamped to 1: zero Views would make every subsequent `ws()` panic.
    ///
    /// Shrinking goes through [`Carousel::detach`], so a `current` or `origin`
    /// pointing at a dropped View is repaired by the same deterministic rule as an
    /// explicit removal instead of the old bare clamp.
    pub fn reconcile_workspaces(&mut self, n_tags: usize) {
        let n_tags = n_tags.max(1);
        while self.workspaces.len() < n_tags {
            self.create_view();
        }
        while self.workspaces.len() > n_tags {
            let pos = self.workspaces.len() - 1;
            self.remove_view_at(pos);
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

/// Workspace layout kind, as reported over the control socket and set through
/// `Action::SetLayout`. The scrolling column ribbon is the only layout that
/// exists; it is an enum because that tag is part of the wire vocabulary a
/// second layout would extend, not because arrangement is dispatched through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutKind {
    /// Scrolling column ribbon (niri-style).
    Column,
}

/// Workspace viewport display mode — a *display-state* axis of the workspace,
/// orthogonal to both window fullscreen (`WinFlags::FULLSCREEN`, an EWMH window
/// state) and the Overview navigation viewport. `Zoomed` enlarges the ribbon
/// (`alpha > 1` in `core::layout::ribbon_geom`) so a column can be inspected up
/// close, and `Action::PageSnap` then scrolls the camera by one screen-width.
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
    /// Focus one specific window, rather than the monitor's focused one.
    ///
    /// Exists for external control (`maverickctl window focus <id>`): without
    /// it a tool could only ever move the focus that happened to be where the
    /// user was already looking, which is a different request from "focus
    /// *this* window".
    FocusWindow(WindowId),
    /// Move one specific window in a direction, rather than the focused one.
    MoveWindow(Dir, WindowId),
    /// Close one specific window.
    CloseWindow(WindowId),
    /// Toggle floating for one specific window, rather than the focused one.
    ToggleFloatWindow(WindowId),
    /// Toggle fullscreen for one specific window.
    ToggleFullscreenWindow(WindowId),
    /// Grow or shrink the focused column by a fraction of the selected
    /// monitor's workarea width, e.g. `+10%`.
    ///
    /// The pixel form (`GrowCol`) cannot express "a tenth of what the user can
    /// see" without the caller knowing the workarea, and a caller that knew it
    /// would be reimplementing a layout decision inside a tool. A percentage is
    /// what a user means by "ten percent wider", and the conversion belongs
    /// next to the layout that owns the number.
    GrowColPct(f32),
    /// Toggle the maximized (workarea-filling, border 0) presentation state of
    /// the focused window. Like fullscreen, but it respects reserved regions and
    /// is only presented while the window is focused (the "peek" overlay in
    /// `core::present`), so it never steals the screen from a background window.
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
    ///
    /// 0-based here, 1-based on the wire (`ArgKind::Ws`). Addresses a carousel
    /// *position*, which is what every existing consumer means by "workspace n";
    /// the mapping to the View's identity happens once, in the command.
    View(usize),
    /// Select the next View on the circular carousel (wraps to the first).
    ViewNext,
    /// Select the previous View on the circular carousel (wraps to the last).
    ViewPrev,
    /// Select the origin View — the explicit return point.
    ViewReturn,
    /// Append a new empty View on the selected monitor.
    ViewCreate,
    /// Drop View `n` from the selected monitor. Refused while it holds clients.
    ViewRemove(usize),
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
    /// Toggle the Overview navigation viewport for the active workspace: the
    /// scale is fixed once, on entry, and navigation pans the viewport.
    ToggleOverview,
    /// Move the selection left/right while in Overview (enters Overview if not active).
    OverviewNav(Dir),
    /// Drop into the currently selected column, leaving Overview (scale back to 1.0).
    OverviewEnter,
    /// Enlarge/shrink the workspace viewport (zoom in/out). Positive `f32` zooms
    /// in, negative zooms out; enters `ViewportMode::Zoomed` and rescales
    /// `page_zoom` immediately. This is display state, not window fullscreen.
    ViewportZoom(f32),
    /// Scroll the camera by one screen-width in the given direction (a "page"
    /// of the zoomed ribbon). Reuses `ideal_scroll`/`camera` — no focus change.
    PageSnap(Dir),
}

/// A deferred focus request created while an overlay (fullscreen/maximize owner)
/// is presented. It carries the *context* it was created under: the exact
/// `monitor`/`workspace` it belongs to and the `owner` overlay that created it.
/// Keying on that context is what keeps a deferred window from being orphaned
/// when the overlay is torn down on a non-active workspace or a non-selected
/// monitor — a live overlay that did not create the deferral cannot consume one
/// it does not own. There is at most one deferral at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFocus {
    /// The window that should receive focus once the overlay is dismissed.
    pub window: WindowId,
    /// The overlay that created this deferral (its presented owner at creation).
    pub owner: WindowId,
    /// The monitor the deferral is bound to.
    pub monitor: usize,
    /// The View the deferral is bound to, by stable identity so the
    /// presentation context cannot drift when carousel positions shift.
    pub workspace: ViewId,
}

/// Global WM state — the single source of truth for placement, focus, and
/// reservations. Owns all `Client`s and `Monitor`s; the backend only reads it.
///
/// # Ownership
///
/// Core owns `State`. Mutations flow through the invariant-checked command
/// pipeline; the backend mirrors X11 state into `clients`/`monitors`.
///
/// # Invariants
///
/// See [`State::check_invariants`] and the crate-level invariant list.
#[derive(Debug)]
pub struct State {
    /// All managed clients keyed by `WindowId`.
    pub clients: HashMap<WindowId, Client>,
    /// Monitors / outputs in backend order.
    pub monitors: Vec<Monitor>,
    /// Selected monitor index.
    pub sel_mon: usize,
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
    ///
    /// The mirror names a *managed* client or nothing: every writer filters the
    /// server's answer through `clients`, so an XID the WM does not manage (a
    /// child window, an override-redirect popup) is recorded as `None` rather
    /// than stored — see `State::check_invariants` #10.
    pub x11_input_focus: Option<WindowId>,
    /// Deferred focus request with explicit context (see `PendingFocus`). This is
    /// the single global slot; the deferral names the exact `monitor`/`workspace`
    /// it belongs to and the `owner` overlay that created it, so it is never
    /// orphaned when the overlay tears down on a non-active workspace or a
    /// non-selected monitor. `None` when there is nothing pending.
    pub pending_focus: Option<PendingFocus>,
    /// Transient windows that mapped before their `WM_TRANSIENT_FOR` parent
    /// was managed. Each entry is the child's id; its desired parent is already
    /// stored on `Client::transient_parent`. Once the parent shows up,
    /// `relink_pending_transients` (x11/backend) moves the child onto the
    /// parent's monitor/workspace and re-floats it, instead of leaving the
    /// popup stranded on whatever monitor happened to be focused at map time.
    pub pending_transients: Vec<WindowId>,
}

impl State {
    /// Create an empty state (no monitors/clients).
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            monitors: Vec::new(),
            sel_mon: 0,
            running: false,
            status: String::new(),
            pending_transients: Vec::new(),
            x11_input_focus: None,
            pending_focus: None,
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

    /// Recompute [`Workspace::presented_maximize`] for the active workspace of
    /// `mon_idx`. The maximize overlay is owned by exactly the monitor's focused
    /// window when that window is maximized on either axis
    /// (`is_maximized_v() || is_maximized_h()`) and sits on the active
    /// workspace. This is the *single* writer that turns `Monitor::focused` plus
    /// the client's maximize flags into the explicit `presented_maximize` field,
    /// so no read site has to infer the maximize-overlay owner from
    /// `mon.focused`.
    pub fn sync_presented_maximize(&mut self, mon_idx: usize) {
        if mon_idx >= self.monitors.len() {
            return;
        }
        // Compared by `ViewId`, so the resolve is exact even if the carousel
        // moved between reading `current` and using it — and a client on a
        // removed View can never match.
        let active = self.monitors[mon_idx].carousel.current();
        let owner = self.monitors[mon_idx].focused.filter(|&w| {
            self.clients.get(&w).is_some_and(|c| {
                Some(c.workspace) == active && (c.is_maximized_v() || c.is_maximized_h())
            })
        });
        // A maximize overlay is only ever presented on the ACTIVE View (see
        // `core::present` and `presented_overlay_owner`, which only read the
        // active View). Clear any stale `presented_maximize` entry on the OTHER
        // Views of this monitor, so a window un-maximized on a non-active View
        // cannot leave a dangling maximize-overlay owner that trips invariant #9
        // when that View is later activated.
        for ws in self.monitors[mon_idx].workspaces.iter_mut() {
            ws.presented_maximize = if Some(ws.id) == active { owner } else { None };
        }
    }

    /// Pick the best window to focus on `mon_idx`'s active workspace. Pure (no X11).
    ///
    /// Order of preference:
    ///   1. a presentation-overlay window on the workspace, most recently focused
    ///      first — so closing a tile in *peek* mode returns focus to the overlay
    ///      the user is looking at rather than to an invisible tile underneath.
    ///      The candidate set is exactly [`State::presented_overlay_owner`]: a
    ///      fullscreen window counts only under `FullscreenPolicy::True` (in the
    ///      column ribbon a fullscreen window is an ordinary scrolling tile), and
    ///      a maximized window only while it is already the monitor's focused
    ///      window. Without that restriction a background maximized window would
    ///      grab focus on `ViewWorkspace`/`MoveToWorkspace`/`FocusMonitor` and,
    ///      since `present` shows whatever is focused, blow itself up over the
    ///      workarea;
    ///   2. the column-focused window;
    ///   3. the most recently focused window in the focus stack.
    pub fn best_focus(&self, mon_idx: usize) -> Option<WindowId> {
        // A pending overlay owns the focus: the window currently presented as the
        // overlay (fullscreen owner, or the focused maximized window) is what the
        // user is looking at, so it is the best focus. Delegating to
        // `presented_overlay_owner` keeps one canonical definition of overlay
        // ownership instead of a second, weaker re-derivation here.
        if let Some(o) = self.presented_overlay_owner(mon_idx) {
            return Some(o);
        }
        let mon = self.monitors.get(mon_idx)?;
        let ws = mon.ws();
        let col_win = ws.focused_win();
        let from_stack = mon
            .focus_stack
            .iter()
            .rev()
            .find(|&&w| self.clients.get(&w).is_some_and(|c| c.workspace == ws.id))
            .copied();
        col_win.or(from_stack)
    }

    /// The canonical definition of "which window currently OWNS the presented
    /// overlay" on `mon_idx`'s active workspace. Returns `None` when there is no
    /// real overlay. A window is a real overlay only when:
    ///   1. it is fullscreen under `FullscreenPolicy::True` — a fullscreen window
    ///      with the default policy is just a ribbon tile and is explicitly NOT
    ///      an overlay; OR
    ///   2. it is the *focused* maximized window (`presented_maximize` owner).
    ///
    /// This MUST stay equivalent to `core::present`, `render::stack_overlay` and
    /// `best_focus`.
    ///
    /// IMPORTANT: this is **NOT** the same as "covering fullscreen" used by
    /// pointer hit-testing. A `Column`-layout fullscreen is *covering* (it paints
    /// a ribbon tile over the screen) but is **not** returned here — and a
    /// `presented_maximize` window **is** returned here but is **not**
    /// `is_fullscreen()` at all. Use the covering predicate
    /// (`StateExt::covering_fullscreen_window`, defined in the `maverick` crate
    /// because it needs the layout) for that, and never substitute
    /// `Client::is_fullscreen()` for either: it is true for both concepts and
    /// would silently merge them.
    pub fn presented_overlay_owner(&self, mon_idx: usize) -> Option<WindowId> {
        let view = self.monitors.get(mon_idx)?.carousel.current()?;
        self.presented_overlay_owner_in(mon_idx, view)
    }

    /// The canonical overlay *owner* on an EXPLICIT View `view` of
    /// `mon_idx`. Shared by [`State::presented_overlay_owner`] (which passes the
    /// monitor's active workspace) and the core EWMH `_NET_ACTIVE_WINDOW` policy
    /// (which must test the *requesting* window's own (monitor, workspace), not
    /// necessarily the active one).
    ///
    /// Ownership is deliberately *monitor*-scoped: both candidate sources,
    /// `Monitor::focus_stack` and `Workspace::presented_maximize`, belong to
    /// `mon_idx`, so neither (nor this helper) consults `Client::monitor`. A
    /// monitor's focus slot is a logical pointer the X sink writes on the monitor
    /// the user is looking at, so it can legitimately name a window placed
    /// elsewhere; the commands that would splice such a window into the wrong
    /// tree guard on that case individually (`NewColumn`, `ToggleFloat`,
    /// `MoveToWorkspace`), and `remove_client` sweeps every monitor's slots.
    ///
    /// Naming the owner is deliberately *narrower* than asking whether an overlay
    /// is still painted (see [`State::pending_focus_owner_presented`]): a
    /// fullscreen window that is not the monitor's most recent focus is still
    /// presented by `core::present` but does not own focus or stacking. The two
    /// questions differ, so the two predicates differ.
    pub fn presented_overlay_owner_in(&self, mon_idx: usize, view: ViewId) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws = mon.workspaces.iter().find(|w| w.id == view)?;
        // A window that is *both* fullscreen and maximized is owned exclusively
        // through the maximize branch below (so it agrees with
        // `presented_maximize`); the fullscreen branch only matches a *purely*
        // fullscreen window. Without this guard the two branches could name
        // different windows and break the overlay-owner /
        // `presented_maximize` agreement that `check_invariants` enforces.
        // `core::present` still presents *every* fullscreen window regardless of
        // focus; this helper only names the one that owns the overlay for
        // focus/stacking decisions.
        let fs_overlay = mon.focus_stack.iter().rev().find(|&&w| {
            self.clients.get(&w).is_some_and(|c| {
                c.workspace == view
                    && c.is_fullscreen()
                    && !c.is_maximized()
                    && c.is_true_fullscreen()
            })
        });
        if let Some(&w) = fs_overlay {
            return Some(w);
        }
        if let Some(w) = ws.presented_maximize {
            if self
                .clients
                .get(&w)
                .is_some_and(|c| c.workspace == view && (c.is_maximized_v() || c.is_maximized_h()))
            {
                return Some(w);
            }
        }
        None
    }

    /// True iff `win` is presented as an overlay on `mon_idx`'s EXPLICIT
    /// workspace `ws_idx` — the single definition of "an overlay is still up in
    /// this context".
    ///
    /// Three conditions, one per coordinate space:
    ///   1. `win` is placed in that monitor (`Client::monitor`) and on that
    ///      workspace (`Client::workspace`). `core::present` only paints windows
    ///      in the placements it is handed, which are one monitor's workspace, so
    ///      an owner that has been moved to another monitor is no longer painted
    ///      there — and the context a deferred focus request is keyed on is the
    ///      overlay's presentation context, so leaving it dismisses the overlay.
    ///   2. `win` is an *exclusive* fullscreen window
    ///      ([`Client::is_fullscreen_overlay`]). A `FullscreenPolicy::Normal`
    ///      fullscreen window is an ordinary ribbon tile and is never an overlay.
    ///   3. OR `win` is maximized on some axis and holds the monitor's focus. A
    ///      maximize overlay is presented exactly while it is the focused window
    ///      (see [`State::sync_presented_maximize`]), so a focus move off a
    ///      maximized window takes its overlay down with it.
    ///
    /// A window that is BOTH fullscreen and maximized is deliberately accepted
    /// through condition 2 rather than routed exclusively through the maximize
    /// branch the way [`State::presented_overlay_owner_in`] does. The two
    /// predicates answer different questions and are meant to differ: naming the
    /// overlay *owner* must yield exactly one window and must agree with
    /// `presented_maximize`, whereas this asks about one specific window still
    /// being painted. `core::present` paints such a window through its
    /// fullscreen branch, so for the purpose of "may the keyboard move yet?" it
    /// is up.
    ///
    /// Used by `State::pending_focus_owner_presented` (the deferral-lifetime
    /// test) and by `decide_manage_focus` (the only creator of a deferral), so
    /// a deferral can never be born behind an overlay that its own lifetime test
    /// already rejects.
    pub fn overlay_presented_in(&self, mon_idx: usize, view: ViewId, win: WindowId) -> bool {
        // A View that no longer exists (removed, or a monitor gone on hotplug)
        // leaves nothing to be presented: the context is gone for good. Keyed on
        // `ViewId`, so a later View that inherits the old carousel *position*
        // does not silently adopt this deferral's context.
        let focused = self.monitors.get(mon_idx).and_then(|m| m.focused);
        self.monitors
            .get(mon_idx)
            .and_then(|m| m.workspaces.iter().find(|w| w.id == view))
            .is_some_and(|_ws| {
                self.clients.get(&win).is_some_and(|c| {
                    c.monitor == mon_idx
                        && c.workspace == view
                        && (c.is_fullscreen_overlay()
                            || ((c.is_maximized_v() || c.is_maximized_h()) && focused == Some(win)))
                })
            })
    }

    /// True iff `self.pending_focus` (if set) names an owner whose overlay is
    /// still presented on the deferral's (monitor, workspace) — the crate
    /// invariant E, verbatim: "that owner is still a presented overlay on the
    /// deferral's own `monitor`/`workspace`".
    ///
    /// The lifetime question is "has the overlay this deferral waits behind been
    /// torn down?", and only [`State::overlay_presented_in`] answers that;
    /// `pending_focus.monitor`/`workspace` are the *presentation context* the
    /// deferral was created under, not the owner's address, which is why an owner
    /// that is merely *hidden* — the user switched workspace, or is looking at
    /// another monitor — keeps the deferral alive while an owner that left the
    /// context entirely loses it.
    ///
    /// Single definition, used by `check_invariants` #8c and by
    /// `reconcile_pending_focus_after_transition`, so the checker and the
    /// resolver can never disagree about a deferral's lifetime.
    pub fn pending_focus_owner_presented(&self) -> bool {
        let pf = match self.pending_focus {
            Some(p) => p,
            None => return false,
        };
        self.overlay_presented_in(pf.monitor, pf.workspace, pf.owner)
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

    /// Insert a client into `self.clients` (does not place it in a workspace).
    pub fn add_client(&mut self, c: Client) {
        let win = c.window;
        self.clients.insert(win, c);
    }

    /// Re-home every client on `mi`'s View `from` onto View `to`, keeping each
    /// window's tiled/floating distinction and its floating geometry intact.
    ///
    /// Returns the windows that moved. Used when a View is destroyed by a
    /// `n_tags` shrink (`reconcile_workspaces`) — which drops trailing Views and
    /// therefore drops the placements inside them, leaving the clients known but
    /// unplaced. Rewriting only `Client::workspace` would make a client name a
    /// View that never references it, which is the exact desync invariant #5
    /// exists to reject.
    ///
    /// A float stays a float and keeps `Client::geom` verbatim: a View change is a
    /// membership change, and floating state and geometry are client properties
    /// that must survive it.
    pub fn rehome_clients(&mut self, mi: usize, from: ViewId, to: ViewId) -> Vec<WindowId> {
        let Some(from_i) = self.monitors.get(mi).and_then(|m| m.view_index(from)) else {
            return Vec::new();
        };
        let Some(to_i) = self.monitors.get(mi).and_then(|m| m.view_index(to)) else {
            return Vec::new();
        };
        if from_i == to_i {
            return Vec::new();
        }
        // Snapshot first: the destination insert and the source removal both borrow
        // the monitor, so the clients have to be enumerated before either mutates.
        let moving: Vec<(WindowId, bool)> = self.monitors[mi].workspaces[from_i]
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .map(|w| (w, false))
            .chain(
                self.monitors[mi].workspaces[from_i]
                    .floats
                    .iter()
                    .copied()
                    .map(|w| (w, true)),
            )
            .collect();
        let mut moved = Vec::with_capacity(moving.len());
        for (win, is_float) in moving {
            self.monitors[mi].workspaces[from_i].remove_window(win);
            if is_float {
                self.monitors[mi].workspaces[to_i].floats.push(win);
            } else {
                self.monitors[mi].workspaces[to_i].add_tiled(win, 1.0);
            }
            if let Some(c) = self.clients.get_mut(&win) {
                c.workspace = to;
            }
            moved.push(win);
        }
        // The source just lost columns; drop the ones that emptied so no View is
        // left holding an empty column (which would project a zero-width column).
        self.monitors[mi].workspaces[from_i].cleanup_empty_columns();
        moved
    }

    /// Remove the View at `pos` on monitor `mi`, re-homing its clients onto
    /// `fallback` (or onto whichever View `Carousel::detach` selects) first.
    ///
    /// This is the *safe* removal: it is the only path that may destroy a
    /// populated View, and it guarantees the surviving state is internally
    /// consistent — `current`/`origin` name existing Views and every client is
    /// still placed exactly once. `Monitor::remove_view_at` remains available for
    /// the empty case.
    ///
    /// Returns the removed View's id, or `None` when `pos` names no View.
    pub fn remove_view(&mut self, mi: usize, pos: usize) -> Option<ViewId> {
        let removed = self
            .monitors
            .get(mi)
            .and_then(|m| m.workspaces.get(pos))
            .map(|w| w.id)?;
        // The successor the carousel will land on: same position after removal if
        // there is one, else the new tail. Computed *before* the removal so the
        // re-home target is already valid when it runs.
        let fallback = self.monitors[mi]
            .workspaces
            .get(pos + 1)
            .or_else(|| {
                pos.checked_sub(1)
                    .and_then(|_| self.monitors[mi].workspaces.last())
            })
            .map(|w| w.id);
        if let Some(to) = fallback {
            if to != removed {
                self.rehome_clients(mi, removed, to);
            }
        }
        self.monitors[mi].remove_view_at(pos)
    }

    /// Remove `win` from `clients` and all placement/overlay/focus structures.
    /// Returns the removed client, if any.
    pub fn remove_client(&mut self, win: WindowId) -> Option<Client> {
        let c = self.clients.remove(&win)?;
        // Drop every transient reference to the window that just died, so the
        // ownership graph never names a client that is gone:
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
        // Drop the X focus mirror when — and only when — it names the window
        // that just left `clients`. The mirror holds a managed client or
        // `None` (every writer filters the server's answer through `clients`,
        // and invariant 10 rejects anything else), so the removal of `win` from
        // `clients` above is exactly what invalidates `Some(win)` and nothing
        // else: a mirror naming a different client is still a live window and
        // must keep being reported as the real X focus, while an unmanaged XID
        // — an override-redirect popup the WM never owned focus bookkeeping for
        // — cannot legitimately be stored here in the first place. Leaving the
        // stale id behind is not cosmetic either: X11 recycles window ids, so a
        // brand-new unrelated client that lands on the recycled id would be
        // reported as the X focus and dragged into `reconcile_focus`'s
        // focus-repair path.
        if self.x11_input_focus == Some(win) {
            self.x11_input_focus = None;
        }
        if self.monitors.is_empty() {
            return Some(c);
        }
        // Clear any stale `presented_maximize` references to this window across
        // *every* monitor/workspace, not just the one the client currently
        // points at. A window that was moved away (MoveWindowToMonitor /
        // MoveToWorkspace) may still be named as the maximize-overlay owner on a
        // former monitor/workspace, and a dangling entry breaks the
        // `presented_maximize` invariant as soon as the window is destroyed.
        //
        // The focus slots get the same sweep for the same reason. A focus slot is
        // a *logical* pointer and is not required to agree with `Client::monitor`
        // at all times, so a monitor other than the one the window died on can
        // still be naming it: `check_invariants` requires every `focus_stack`
        // entry to be a known client, and a monitor whose logical focus names a
        // destroyed window hands the next focus query an id that can be recycled
        // by X11 onto an unrelated client. A test fixture that wants a stale
        // logical focus can still install one directly.
        for mon in &mut self.monitors {
            for ws in &mut mon.workspaces {
                if ws.presented_maximize == Some(win) {
                    ws.presented_maximize = None;
                }
            }
            mon.focus_stack.retain(|&w| w != win);
            if mon.focused == Some(win) {
                mon.focused = mon.focus_stack.last().copied();
            }
        }
        // c.monitor may be stale after hotplug (fewer monitors than before).
        // Clamp to avoid panic index out-of-bounds.
        let mon_i = c.monitor.min(self.monitors.len().saturating_sub(1));
        let mon = &mut self.monitors[mon_i];
        // Located by the client's own `ViewId`. A stale `c.workspace` (a View
        // removed by a shrink) resolves to `None` and the tree is left alone
        // rather than indexed by a dead id.
        if let Some(ws_i) = mon.view_index(c.workspace) {
            mon.workspaces[ws_i].remove_window(win);
            // Keep the workspace focus pointer (column + row) in lock-step with
            // the logical focus (`mon.focused`). `remove_window` only shifts and
            // clamps the column index relative to the surviving tree; it does not
            // know which window is logically focused. Re-deriving the pointer from
            // `mon.focused` here is the single source of truth for "what is
            // focused": without it, dropping a column before the focused one left
            // `ws.focus.column_idx` on a different column, so the camera
            // (`ideal_scroll` reads `focus.column_idx`) centred the wrong column
            // and `best_focus`/`focused_win` returned a neighbour — focus would
            // depend on which window happened to be visible at close time.
            if let Some(fw) = mon.focused {
                if let Some((ci, ri)) = mon.workspaces[ws_i].index_of_window(fw) {
                    mon.workspaces[ws_i].focus.column_idx = ci;
                    mon.workspaces[ws_i].columns[ci].focused = ri;
                }
            }
        }
        Some(c)
    }

    /// Pure workspace rearrangement for `MoveDir` — no X11 calls. The caller
    /// follows up with arrange/focus. Returns `false` when there was nothing to
    /// do (float, empty workspace, boundary no-op).
    pub fn apply_move_dir(&mut self, dir: Dir) -> bool {
        if self.monitors.is_empty() {
            return false;
        }
        let mi = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        let ws_i = match self.monitors.get(mi) {
            Some(m) => m.active_index(),
            None => return false,
        };
        let focused = match self.monitors[mi].focused {
            Some(w) => w,
            None => return false,
        };
        self.apply_move_dir_in(mi, ws_i, focused, dir)
    }

    /// Apply a move in `dir` to one specific window, wherever it lives.
    ///
    /// The targeted counterpart of [`Self::apply_move_dir`], and the reason it
    /// is not just a parameter on that one: the monitor, the workspace and the
    /// column a window sits in are three different things when it is not the
    /// focused one. The focused path resolves all three from `sel_mon` and
    /// `active_ws`; a window the caller named has to be looked up in the tree
    /// it is actually tiled in, or the move lands on a column that does not
    /// contain it.
    pub fn apply_move_dir_for(&mut self, win: WindowId, dir: Dir) -> bool {
        let Some(client) = self.clients.get(&win) else {
            return false;
        };
        if client.is_float() {
            return false;
        }
        let mi = client.monitor;
        // Resolved through the *client's own* `ViewId`, never through
        // `active_ws`: the focused slot is written on the monitor the user is
        // looking at and can name a window placed elsewhere, so addressing a
        // View by position would move the window against the wrong tree.
        let Some(ws_i) = self
            .monitors
            .get(mi)
            .and_then(|m| m.view_index(client.workspace))
        else {
            return false;
        };
        // A float is not in the column tree, so it has no column to move
        // between; `apply_move_dir_in` refuses it too, but checking here keeps
        // the reason in the one place that knows what the tree looks like.
        if client.is_float() {
            return false;
        }
        let tiled = self.monitors[mi].workspaces[ws_i]
            .columns
            .iter()
            .any(|c| c.windows.contains(&win));
        if !tiled {
            return false;
        }
        self.apply_move_dir_in(mi, ws_i, win, dir)
    }

    /// The body of a directional move, with the monitor, the workspace and the
    /// window all supplied by the caller.
    ///
    /// `mi`/`ws_i`/`target` are the resolved location of the window being
    /// moved: `sel_mon` + `active_ws` + the focused window for the keybinding
    /// path, and the client's own monitor/workspace for the targeted one.
    fn apply_move_dir_in(&mut self, mi: usize, ws_i: usize, target: WindowId, dir: Dir) -> bool {
        if self.monitors.is_empty() || mi >= self.monitors.len() {
            return false;
        }
        if self.clients.get(&target).is_some_and(Client::is_float) {
            return false;
        }

        // The column the window is in, found by search: for the focused path
        // that is the focus slot, for a targeted one it can be anywhere.
        let (ci, n_cols, col_len) = {
            let ws = &self.monitors[mi].workspaces[ws_i];
            let ci = ws
                .columns
                .iter()
                .position(|c| c.windows.contains(&target))
                .unwrap_or(ws.focus.column_idx);
            (
                ci,
                ws.columns.len(),
                ws.columns.get(ci).map_or(0, |c| c.windows.len()),
            )
        };
        // A window that is in no column (a float, or a nameable slot that was
        // never placed) must not be moved against whatever sits at `ci`.
        if ci >= n_cols
            || !self.monitors[mi].workspaces[ws_i]
                .columns
                .get(ci)
                .is_some_and(|c| c.windows.contains(&target))
        {
            return false;
        }

        // Horizontal `MoveDir` on a column has two distinct meanings: a
        // single-window column is *swapped* with its neighbour (the window stays
        // put, the ribbon order changes), while a multi-window column is *split*
        // — the target window is extracted into a new column beside it.
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
                    // No `Cfg` is reachable from here, so the split is fixed at an
                    // even half/half; the caller can re-tune with grow/shrink.
                    let ratio = 0.5;
                    let src_w = ws.columns[ci].weight;
                    ws.remove_window(target); // column keeps `src_w` (still non-empty)
                    let index_in_ws = if dir == Dir::Left { ci } else { ci + 1 };
                    let insert_pos = index_in_ws.min(ws.columns.len());
                    // Spring-split the source column: it keeps `ratio` of its
                    // weight, the extracted window takes the rest. Both halves
                    // are re-clamped into the documented band, because a split is
                    // a *derived* write like any other and the band binds every
                    // stored column — halving a column already at the 0.05
                    // minimum would otherwise store 0.025 on both halves, and
                    // the `check_invariants` that runs after the next command
                    // would abort a debug build. The clamp belongs here rather
                    // than in `rebalance_weights`, whose repair is deliberately a
                    // no-op on a healthy weight: 0.025 is a healthy positive
                    // number as far as it is concerned, so it would pass through
                    // untouched. A chain of splits can only ever push a column
                    // down to the floor, never below it.
                    ws.columns[insert_pos.min(ci)].weight = band_weight(src_w * ratio);
                    let mut new_col = Column::new(band_weight(src_w * (1.0 - ratio)));
                    new_col.windows.push(target);
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
                    // Start from where the *named* window is, not from the
                    // column's focus slot: for a targeted move those can differ,
                    // and rotating from the wrong one moves a different window.
                    let ri = col.windows.iter().position(|w| *w == target).unwrap_or(0);
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

    /// Check the structural invariants of the whole `State`: every window lives
    /// in exactly one place, indices are in range, focus is valid, cameras carry
    /// no NaN, column weights are in bounds, … Returns `Ok(())` when clean, or
    /// `Err(violations)` listing *every* broken invariant (never just the first).
    ///
    /// Single source of truth shared by:
    ///   * `State::assert_invariants`, which `Engine::execute` /
    ///     `execute_batch` call in debug builds only, and
    ///   * the fixed-seed chaos harness
    ///     (`property_invariants_hold_under_chaos`), which checks after every
    ///     step in every profile.
    ///
    /// Deliberately cheap and side-effect free — no X11, no cloning of the client
    /// map — so running it on every transition is affordable. The numbered
    /// comments below label each check; the gaps are deliberate so that existing
    /// cross-references to a number keep pointing at the same check.
    pub fn check_invariants(&self) -> Result<(), Vec<String>> {
        let mut v = Vec::new();

        // 1. Carousel pointers are valid, and `ViewId`s are unique per monitor.
        //
        //    `current`/`origin` must name an existing View whenever any View
        //    exists — that is what makes `return_to_origin()` total and stops a
        //    removed View's id from silently resolving to nothing. The empty state
        //    is the one legal `None` pair.
        for (mi, mon) in self.monitors.iter().enumerate() {
            let n = mon.workspaces.len();
            match (mon.carousel.current(), mon.carousel.origin()) {
                (None, None) if n == 0 => {}
                (None, None) => v.push(format!(
                    "monitor {mi}: carousel has no current/origin View but {n} exist"
                )),
                (c, o) => {
                    if let Some(c) = c.filter(|id| mon.view_index(*id).is_none()) {
                        v.push(format!(
                            "monitor {mi}: carousel current {c} names no existing View"
                        ));
                    }
                    if let Some(o) = o.filter(|id| mon.view_index(*id).is_none()) {
                        v.push(format!(
                            "monitor {mi}: carousel origin {o} names no existing View"
                        ));
                    }
                }
            }
            // 1b. View identity is unique within the monitor. Two Views sharing an
            //     id would make `Client::workspace` ambiguous, so this is what
            //     keeps "one client, one View" decidable.
            let mut ids = std::collections::HashSet::new();
            for ws in &mon.workspaces {
                if !ids.insert(ws.id) {
                    v.push(format!("monitor {mi}: duplicate View id {}", ws.id));
                }
            }
            // 2. Cameras carry no NaN / infinity: a poisoned camera would write
            //    a NaN rect straight into a ConfigureWindow.
            if !mon
                .workspaces
                .iter()
                .all(|ws| ws.camera.position.is_finite())
            {
                v.push(format!("monitor {mi}: camera has NaN/inf position"));
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
            // Membership is by `ViewId`, so the check is "does this View exist on
            // this monitor", not "is the index in range" — a removed View's id is
            // detectable precisely because ids are never reused.
            let Some(ws_i) = mon.view_index(c.workspace) else {
                v.push(format!(
                    "client {win}: view {} does not exist on monitor {} ({} views)",
                    c.workspace,
                    c.monitor,
                    mon.workspaces.len()
                ));
                continue;
            };
            if let Some((pm, pw)) = seen.get(&win).copied() {
                if (pm, pw) != (c.monitor, ws_i) {
                    v.push(format!(
                        "client {win}: stored at ({}, {}) but tiled at ({}, {})",
                        c.monitor, c.workspace, pm, pw
                    ));
                }
            }
            // NOTE: geometry positivity and "the focused window must be on its
            // active workspace" are deliberately NOT asserted here — they hold
            // only *after* an arrange/placement pass, and valid intermediate
            // states (and unit-test fixtures) legitimately carry a default rect
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
            // 8c. The global `pending_focus` slot names a live client and its owner
            // is still a presented overlay on the SAME monitor/workspace the
            // deferral was created for. The overlay need not be on the *active*
            // workspace — switching workspaces only hides it, it does not dismiss
            // it, so the deferral legitimately survives a workspace switch and is
            // only consumed or dropped when the overlay is torn down. Requiring
            // the owner to still be presented is what stops a deferred window
            // from being orphaned. The lifetime test itself is
            // `State::pending_focus_owner_presented`, shared with
            // `reconcile_pending_focus_after_transition`.
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
                    && mon.ws().presented_maximize != Some(w)
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
            //    active workspace. "Maximized" here means *either* axis, matching
            //    the field's own contract: `sync_presented_maximize`,
            //    `presented_overlay_owner_in` and `pending_focus_owner_presented`
            //    all derive the owner with `is_maximized_v() || is_maximized_h()`.
            //    A per-axis maximize is a legal EWMH state — `_NET_WM_STATE`
            //    sets `MAXIMIZED_VERT` and `MAXIMIZED_HORZ` independently and the
            //    X11 sink deliberately does not promote one to the other — so
            //    checking both axes here rejected states the rest of the model
            //    produces on purpose.
            let active = mon.carousel.current();
            let pm = mon.ws().presented_maximize;
            if let Some(w) = pm {
                match self.clients.get(&w) {
                    None => v.push(format!(
                        "monitor {mi}: presented_maximize {w} not in clients"
                    )),
                    Some(c) if !(c.is_maximized_v() || c.is_maximized_h()) => v.push(format!(
                        "monitor {mi}: presented_maximize {w} is not maximized"
                    )),
                    Some(c) if Some(c.workspace) != active => v.push(format!(
                        "monitor {mi}: presented_maximize {w} on wrong view"
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

        // 11. Column weight bounds. The upper bound is 1.0, not 0.95: `GrowColumn`
        //     must be able to fill the whole workarea, and any ceiling below 1.0
        //     would make a full-width second column impossible. The 1e-6 slack
        //     absorbs float drift from the weight arithmetic.
        let in_band = (MIN_COLUMN_WEIGHT - 1e-6)..=(MAX_COLUMN_WEIGHT + 1e-6);
        for (mi, mon) in self.monitors.iter().enumerate() {
            for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                for (ci, col) in ws.columns.iter().enumerate() {
                    if !in_band.contains(&col.weight) {
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

        // 12. Float geometry is deliberately NOT a structural invariant here: it is
        //     validated by the clamp inside `arrange`
        //     (`render::clamp_is_idempotent`), and fixtures legitimately hold
        //     temporary 0x0 or oversize rects before the first arrange pass.

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

    /// Assert the standing workarea contract: `workarea` is `screen` minus the
    /// collapsed `reserved` totals, so it can never be larger than `screen` nor
    /// anchored outside it — whatever the reservations say.
    fn assert_workarea_inside_screen(m: &Monitor, what: &str) {
        let screen = m.screen;
        let wa = m.workarea;
        assert!(
            screen.contains_rect(wa),
            "{what}: workarea {wa:?} is not inside screen {screen:?}"
        );
        assert!(
            wa.w <= screen.w && wa.h <= screen.h,
            "{what}: workarea {wa:?} is larger than screen {screen:?}"
        );
        assert!(
            wa.x >= screen.x && wa.y >= screen.y,
            "{what}: workarea {wa:?} starts before screen {screen:?}"
        );
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
        // be applied together, and re-registering must not lose the other edge:
        // `set_reserved_region` clears the owner's previous region, so the
        // per-edge form is only safe for owners that hold exactly one edge.
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

    /// A hostile dock (any window may set `_NET_WM_STRUT[_PARTIAL]` to any
    /// `u32`) must not be able to wrap the derived geometry: the workarea stays
    /// inside the screen and never grows past it, whatever the edges sum to.
    #[test]
    fn hostile_strut_values_never_escape_the_screen() {
        let screens = [
            Rect::new(0, 0, 1920, 1080),
            Rect::new(-1920, -1080, 1920, 1080),
            Rect::new(3840, 0, 1280, 1024),
            Rect::new(0, 0, 1, 1),
        ];
        let hostile: [&[(Edge, u32)]; 6] = [
            &[(Edge::Left, u32::MAX), (Edge::Right, u32::MAX)],
            &[(Edge::Top, u32::MAX), (Edge::Bottom, u32::MAX)],
            &[(Edge::Left, 3_000_000_000), (Edge::Right, 2_000_000_000)],
            &[(Edge::Top, u32::MAX), (Edge::Left, 1)],
            &[(Edge::Bottom, u32::MAX / 2), (Edge::Top, u32::MAX / 2)],
            &[(Edge::Right, u32::MAX)],
        ];
        for screen in screens {
            for regions in hostile {
                let mut m = Monitor::new(screen, 1);
                m.set_reserved_regions(0xFEED, regions);
                assert_workarea_inside_screen(&m, &format!("{screen:?} + {regions:?}"));
            }
        }
    }

    /// A reservation wider than the screen it pushes into (a stale dock after a
    /// resolution change, or a lying one) leaves no usable area: the workarea
    /// collapses onto the screen's own edge instead of past it.
    #[test]
    fn reservation_larger_than_the_screen_collapses_onto_its_edge() {
        // Exactly the screen height: empty workarea sitting on the bottom edge.
        let mut m = mon();
        m.set_reserved_region(0x1, Edge::Top, 1080);
        assert_eq!(m.workarea, Rect::new(0, 1080, 1920, 0));
        assert_workarea_inside_screen(&m, "top == screen height");

        // Beyond it, on both axes at once.
        let mut m = mon();
        m.set_reserved_regions(0x1, &[(Edge::Top, 5_000), (Edge::Left, 9_000)]);
        assert_eq!(m.workarea, Rect::new(1920, 1080, 0, 0));
        assert_workarea_inside_screen(&m, "struts beyond the screen");

        // The unclamped side is still subtracted normally.
        let mut m = mon();
        m.set_reserved_regions(0x1, &[(Edge::Left, 3_000), (Edge::Top, 30)]);
        assert_eq!(m.workarea, Rect::new(1920, 30, 0, 1050));
        assert_workarea_inside_screen(&m, "left beyond the screen, top within");
    }

    /// A screen whose origin sits near `i32::MAX` plus a reservation must not
    /// overflow the coordinate (debug builds would panic on it).
    #[test]
    fn screen_origin_near_i32_max_survives_a_reservation() {
        let screen = Rect::new(i32::MAX - 10, 0, 1920, 1080);
        let mut m = Monitor::new(screen, 1);
        m.set_reserved_regions(0x1, &[(Edge::Left, 100), (Edge::Top, 20)]);
        assert_eq!(m.workarea.w, 1820, "the reservation still subtracts");
        assert_eq!(m.workarea.h, 1060, "the reservation still subtracts");
        assert_workarea_inside_screen(&m, "origin at i32::MAX - 10");
    }
}

#[cfg(test)]
mod rect_tests {
    use super::Rect;

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
