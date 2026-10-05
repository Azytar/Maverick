# Changelog

All notable changes to Maverick are documented here. This project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html); the format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

The versions below are the project's release history; each has a matching
`vX.Y.Z` tag. `1.0.0` is the first release of the reduced, compositor-free window
manager, and the release that declares the supported contract. The `0.x` line
before it makes no compatibility promise.

## [Unreleased]

## [1.1.1] - 2026-10-05

### Changed

- The package version is declared once, in `[workspace.package]`, and every
  package inherits it, so the manifests can no longer disagree with each other.
  Internal crates are declared once with a compatible requirement beside their
  path.
- `maverickctl` gained the `--version` it never had, derived from
  `CARGO_PKG_VERSION` exactly as `maverick`'s is.
- The Debian and Ubuntu requirements list names `libx11-xcb-dev`. It is the
  package that provides `libX11-xcb.so`, `libx11-dev` does not depend on it,
  and the installer's own link probe requires it.

### Fixed

- `maverickctl` terminates its help output with a newline, so the text is
  well-formed for terminals and for anything reading the last line.
- The `tree` query writes a window's workspace as a JSON index instead of the
  `view#N` spelling of a `ViewId`. The document was unparseable as a whole, so
  `window list`, `inspect`, `focus`, `close`, `move`, `float` and `fullscreen`
  could not read it.
- A window in the `tree` query whose client record is gone no longer leaves a
  trailing comma behind the last field, which made the document unparseable.
- `maverickctl process` reads its arguments from after the verb, like every
  other group. `process list`, `process inspect` and `process kill` reported a
  missing session named after the verb, or refused a pid as a session name.
- The installer's panel is laid out identically under `LC_ALL=C`, `LC_ALL=POSIX`
  and a UTF-8 locale. Column padding was measured with `wc -m`, which returns a
  byte count outside a multibyte charmap, so a panel containing an em dash came
  out two columns short and did not close.

## [1.1.0] - 2026-10-02

The first release after 1.0.0 that adds capability without withdrawing any.

### Added

- Views carry a stable identity, and a monitor's views form a circular carousel.
- `next`, `previous` and `return` navigate that carousel.
- `maverickctl view` addresses the carousel from the command line.
- Property tests pin the carousel, `View` and `Scroll` invariants.

### Changed

- `Scroll` receives the `View` to arrange rather than reaching into the
  carousel, so arrangement and navigation no longer share ownership.
- Clients are re-homed when a `View` is dropped, rather than only having their
  records updated.
- Option parsing is unified across `maverickctl`, and an option a command has no
  meaning for is refused instead of being forwarded.
- The README and its Spanish translation are rewritten against the window
  manager that actually ships, and the installer is aligned with it.

### Removed

- The showcase harness and its screenshots. It was a development artifact for
  producing documentation images, not a shipped capability.

## [1.0.0] - 2026-09-30

Maverick becomes a minimal window manager that draws through X11 alone. This is
the release that declares the supported contract: the `maverick` command line,
the `maverickctl` verbs, the control-socket protocol, the configuration schema
and the crate layout. Removing the compositor also removes every reason a caller
had to configure one.

### Changed

- `maverickctl`'s CLI, session and discovery code moves out of `maverick-sys`
  into a `maverickctl` crate of its own. The binary's behaviour is unchanged; the
  crate boundary is not.
- Arrangement is settled in a single projection.
- CI covers the all-features build and stops testing a profile that no longer
  exists.

### Removed

- The compositor. Maverick renders through X11 directly; there is no GL
  composition path.
- The wallpaper implementation and its configuration, and the wallpaper control
  command.
- The Vulkan bootstrap, which no build could reach.
- The `[compositor]` and `[animations]` configuration tables.
- The `maverick-render`, `maverick-gl`, `maverick-vk` and `maverick-img` crates.
  Workspace membership falls from eight crates to five, against a peak of ten
  at 0.5.0; counting the root `maverick` package, from nine to six.
- Three `maverick-sys` entry points that nothing called.
- Commands and counters in the core that no code path could reach.

### Fixed

- Two X11 mask limits that only a real X server could reveal are corrected.
- The camera is re-derived through retarget when a window closes and when a
  monitor's screen changes, instead of asking X11 again for the focus it has
  just reported.
- Adding and removing a column is symmetric for the focus pointer, and `STICKY`
  stays tied to `FLOAT` so a sticky window is never stranded.
- A burst's layout work collapses into one pass per monitor, and wheel notches
  are coalesced so input is served without a round trip.

## [0.7.1] - 2026-09-28

Corrections only; no features are added.

### Changed

- Installation becomes a bounded, staged, verified user-local install instead of
  an install into a system prefix.
- `libc` is kept deliberately for `sigaction`, because `rustix` does not
  implement it. The two sites that still use it say so.

### Fixed

- A control command could sit in the queue forever. `drain_commands` read the
  queue before the self-pipe while `push_command` enqueued first and wrote the
  wakeup byte second, so a command landing in that window lost its wakeup and
  stranded a settled window manager in `poll` until an unrelated X event
  arrived. The pipe is now read first, which cannot lose a wakeup. `maverickctl
  quit` timing out and `dispatch` doing nothing were both symptoms.
- `SIGHUP` skipped cleanup entirely, leaving the session record and the control
  socket bound. It now takes the same bounded shutdown every other stop signal
  gets.
- `Rect::right` and `Rect::bottom` saturated the operand instead of the sum, so
  a rectangle's far edge could be reported short by up to two billion pixels.
- A monitor built with zero tags panicked on the path RandR and Xinerama
  detection actually call.
- A delegated image decode paid 143 ms of wasted spawn to avoid a converter that
  answers in 18 ms; it now probes the converter that actually answers and asks
  for a PPM, and stops waiting on a child that has already exited.
- The Vulkan swapchain extent selection chose an extent the spec forbids, and
  driver strings were read without bounds.
- A window is parked where the X server can still keep it off-screen, rather
  than at a position the server may reclaim.
- A display claim could be pointed at any file on the machine, so a symlink
  planted at the claim path was followed and the exclusive claim taken on the
  link's target. The open now carries `O_NOFOLLOW`.
- `SIGQUIT` is handled, and a refused signal disposition is reported rather than
  silently ignored.

## [0.7.0] - 2026-09-26

Control becomes a session model with a verified peer.

### Added

- Maverick sessions, modelled around a nested X server, with lifecycle
  management and crash reaping.
- An instance can adopt a fixed session id, so its runtime directory and control
  socket are named predictably.
- A JSON value parser for the nested control-plane documents.
- Actions can target a specific window rather than only the focused one.
- An inspection document exposing each monitor's screen size.
- `maverickctl sessions`, `windows`, `processes`, `logs` and `inspect`.
- The peer is verified at the socket, and the runtime directory is kept private.
- A display is claimed exclusively before spawning onto it.

### Removed

- The `maverick-msg` binary, folded into `maverickctl`. An installed
  `maverick-msg` disappears; `maverickctl` gains what it did.

### Fixed

- Key-binding XKB level resolution is unified, so a binding matches what the
  active layout actually produces.
- The camera spring settles from its own state rather than the published one,
  and its settling is exact.
- Monitor geometry is guarded against hostile strut values, and a
  user-configured border width is bounded before it reaches arrangement.
- A reload that cannot read or parse the file keeps the running configuration
  instead of losing it.
- A silent control-socket peer, a failed listing and a refused query are each
  reported as the failure they are, and control channels are bounded.
- Focus is cleared when the focused client is removed, unique client ownership
  survives workspace moves, and column weight bounds survive a split.

## [0.6.0] - 2026-09-24

Input becomes layout-aware, and the standalone tools are withdrawn.

### Added

- Key bindings resolve through the active XKB layout, so a binding matches what
  the keyboard actually produces rather than the nominal keycode.
- A deterministic, supersampled screenshot harness and a showcase frame.
- A native clean-quit action.
- The wallpaper renders through the root pixmap, with no compositor involved.

### Removed

- The `maverick-dialog` crate and binary.
- The `maverick-installer` crate and binary. The shell installer is the only
  installation path.

### Fixed

- The EWMH workarea is published at startup.
- Fullscreen presentation transitions present their exact endpoint before
  idling, and a new fullscreen window stacks correctly.
- Rounded-corner masks are anchored at the outer frame edge, and the focus ring
  aligns with the rounded frame.
- A new window reconciles against an active fullscreen window.
- Production `unwrap` calls are removed and control channels are bounded.

## [0.5.1] - 2026-09-05

Corrections following 0.5.0. No features are added.

### Fixed

- Per-frame working buffers are reused instead of reallocated.
- The overlay is shaped around bypassed windows.
- Animation substeps are restored for the compositor-less build.
- Floating geometry ownership is preserved while dragging.
- Damage state stays synchronised while bypassing.
- Deprecated camera configuration keys are dropped from the example, and the
  unused tracked-window diagnostic is removed.
- The shipped example configuration stops binding the Grid layout that 0.5.0
  removed.

## [0.5.0] - 2026-09-03

The architecture splits, and the layout model narrows.

### Added

- `maverick-core`, a crate holding Maverick's pure shared types with no X11 or
  GL dependency.
- `maverick-render`, a rendering boundary that separates presentation from the
  window manager.
- Compositor-less builds, through compatible stubs and lint gates.
- Animations decoupled from composition, with the backend selectable.
- A setup assistant and fullscreen reproduction helpers.

### Changed

- Compiled defaults no longer depend on X11.

### Removed

- The Grid layout. **This breaks existing configurations**: Column is now the
  only layout model. The shipped example configuration still binds
  `layout:grid` at this release and stops doing so in 0.5.1.

## [0.4.0] - 2026-08-21

The compositing era: Maverick gains a renderer, a compositor and a wallpaper.

### Added

- Composition policy and fullscreen bypass.
- Scene buffering and viewport culling, with per-frame damage accumulation and
  partial redraw gated on GLX buffer age.
- `maverick-img`, dependency-free image decoding.
- Texture upload and fragment-shader primitives.
- A wallpaper domain model and configuration schema.
- `maverick-vk`, an X11/Vulkan instance, device and swapchain bootstrap.
- Weighted ribbons, animated viewports and validated configuration.
- A standalone installation utility.
- Quadrant resizing from the pointer, and dropping a floating window into a
  column.

### Changed

- The Grid layout is rewritten, and focus and fullscreen commands are revised.
- Typed queries, event subscription, a shared control CLI and WM adoption are
  integrated.

## [0.3.0] - 2026-08-07

Maverick owns its configuration parsing, and the control surface becomes typed.

### Added

- `maverick-toml`, a zero-dependency TOML-subset parser owned by Maverick,
  replacing serde's TOML implementation.
- Typed commands, events and capability queries.
- Configuration load and reload, with a compatible IPC surface.
- Pluggable layouts with configurable gaps.
- Theme presets and richer window rules.
- `maverick-dialog`, a standalone X11 confirmation utility.

### Changed

- The X11 backend moves into a module directory, and core semantic effects,
  presentation and IPC are integrated.
- `WindowId` is defined independently of `x11rb`.

### Removed

- The internal bar, in favour of external dock reservations.
- The Monocle layout.

## [0.2.0] - 2026-07-19

A running window manager becomes externally controllable.

### Added

- Instance discovery and control sockets.
- The `maverickctl` binary, and the `maverick-sys` crate that carries it.
- A PID file, so a running instance can be found and asked to shut down.
- Quit confirmation in the window manager itself, with shutdown requests and
  column resizing.
- Startup sequencing for the compositor, sound and applications, and desktop
  portal services.
- Workspace APIs, input, hotplug and resource handling integrated into the X11
  backend.

### Changed

- Workspaces are stored per workspace and mutated in place.
- New columns use 75-percent widths consistently, and bar hitboxes match the
  rendered glyphs.
- Quit confirmation no longer shells out to an external dialog.

## [0.1.0] - 2026-06-21

The first Maverick.

### Added

- Window-manager state types and logging.
- Compiled defaults, keybindings and window rules.
- Layout calculation and event-driven commands.
- X11 window management with EWMH support and an internal bar.
- The `maverick` entry point, with signal handling and autostart.
- Bilingual usage guides.

### Fixed

- The configuration no longer forces a Dvorak layout at startup.

