# Changelog

All notable changes to this project are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.18.3] — 2026-08-07

### Added

- **`--replace` y adopción de ventanas previas (EWMH).** Nuevo flag
  `maverick --replace`: si otro WM ya gestionó la pantalla, se intenta grabar
  `SUBSTRUCTURE_REDIRECT` directamente; si otro WM la tiene, se localiza su
  ventana `_NET_SUPPORTING_WM_CHECK` (EWMH 1.4 § WM Attributes) y se le envía
  `WM_DELETE_WINDOW` (nunca SIGKILL), reintentando el grab hasta ceder. Al
  arrancar sobre un WM previo, las ventanas existentes se adoptan
  (`scan_windows`) y las que estaban flotando restauran su geometría.
- **`_NET_CLIENT_LIST_STACKING`.** La root window publica el orden de
  apilamiento real (tiles visibles → flotantes → el resto, por recencia de
  foco) a través de `_NET_CLIENT_LIST_STACKING`, actualizada en cada
  `flush_client_list`. `_NET_SUPPORTED` se amplió con
  `_NET_WM_WINDOW_OPACITY`, `_NET_CLOSE_WINDOW` y
  `_NET_WM_BYPASS_COMPOSITOR`.
- **Persistencia de ventanas flotantes.** Las window properties propias
  `_MAVERICK_FLOAT` / `_MAVERICK_GEOM` se escriben/limpian según el estado de
  flotación (`sync_window_prefs`), devolviéndole a una ventana su geometría
  flotante tras un reinicio o `--replace`.
- **Reglas con criterios nuevos.** `[[rules]]` ahora acepta además de
  `class`/`title`: `instance`, `window_type` (normal/desktop/dock/toolbar/
  menu/utility/splash/dialog), y acciones `sticky` (implica flotante),
  `size` y `position` (clamped a la workarea). Criterio coincidente por
  substring case-insensitive; `ws` acepta alias numéricos y `request` para la
  workspace actual.
- **`maverick-msg` (CLI dwm-style).** Nuevo binario de control: cualquier
  línea no administrativa se reenvía verbatim según el protocolo
  (`maverick-msg focus-right`; `maverick-msg query tree`). Comparte el motor
  CLI (`maverick-sys::ctl`) con `maverickctl`.
- **`query` estructurado sobre el socket de control.** `maverickctl query
  workspaces|tree|focused|state` consulta el estado vivo del WM (el hilo
  WM responde por el canal de réplica): IDs/geometría/layout por workspace y
  columna, y la ventana enfocada.
- **Ratón en flotantes.** `Mod+drag` de una ventana flotante ahora cambia su
  tamaño según el cuadrante del puntero (mitades superior/izquierda son
  resize). Soltar una ventana flotante encima de una ya mosaicada la inserta
  en la columna correspondiente (drop-to-tile); durante el drag se muestra un
  borde de previsualización en la columna destino.

### Fixed

- **Guardia anti-carrera `EnterNotify`/teclado (focus-follows-mouse).** Al
  navegar con el teclado se arma una ventana de 50 ms (`pointer_guard_until`);
  cualquier `EnterNotify` generado por la posición del puntero dentro de esa
  ventana se ignora, y solo el primer `MotionNotify` real levanta la guardia.
  Navegar con `Mod+Arriba/Abajo` ya no "resbala" a la ventana vecina que
  toca el cursor.
- **`WM_TAKE_FOCUS` con timestamp ICCCM real.** `send_proto` ahora envía el
  último timestamp de evento de entrada (`last_event_time`, registrado en
  key/button/enter/motion) en lugar de `CurrentTime`; toolkits estrictos
  (Swing, algunas builds de Emacs) aceptan el foco correctamente. Aplica
  también al `WM_DELETE_WINDOW` de `kill()`.
- **MapRequest con overlay fullscreen/maximizada activa (anti-focus-steal).**
  Si el nuevo `MapRequest` es un diálogo `WM_TRANSIENT_FOR` de la ventana
  presentada (p. ej. Ctrl+S de una app en fullscreen), toma foco y se eleva
  sobre la overlay; cualquier otra ventana entra al árbol de mosaicos en
  silencio: sin `focus()`, la overlay mantiene foco y stacking, y la ventana
  nueva se marca `_NET_WM_STATE_DEMANDS_ATTENTION` (urgencia, borde resaltado)
  que se consume al enfocarla.

- **Capability Layer (`core::capability`).** API pública de **lectura** para
  consumidores externos (barras, hooks, tests): `Engine::query()` ofrece
  `focused_window()`, `active_workspace()`, `visible_windows()`,
  `current_layout()`, `window(id)` → `WindowInfo`. Está desacoplada del
  `State`/`Client` internos (los DTO públicos son estables) y es de solo
  lectura — escribir sigue siendo únicamente vía `Engine::execute(Command)`.
  Una barra pregunta, no manipula.

- **EventBus tipado (`core::event`).** Implementa el modelo
  `Command → Domain Event → Effect` de la auditoría. Cada comando declara el
  evento de dominio que representa (`FocusChanged`, `WorkspaceChanged`,
  `LayoutChanged`, `WindowMoved`, `GapsChanged`, etc.) pero jamás conoce a sus
  consumidores. El `Engine` lo publica en un `EventBus` al que se suscriben
  renderer, IPC, futuras barras, hooks y tests. Un consumidor nuevo se
  suscribe y recibe cambios incrementales en lugar de sondear el estado.
- **Transacciones (`Engine::execute_batch`).** N comandos se ejecutan como una
  sola transacción: mutan `State`/`Cfg`, se publican los eventos de dominio al
  final y **una única** `PublishIpcState` coalesce el batch — resuelve la
  preocupación de "un macro publicando 50 veces".
- **Popups de la overlay nunca quedan ocultos.** `Client` recuerda
  `transient_parent` (`WM_TRANSIENT_FOR`, capturado en `manage()`), y el
  stacking del renderer (`stack_overlay`) eleva los diálogos/popups cuya
  cadena transiente alcanza una ventana fullscreen/maximizada **por encima de
  la overlay** — un menú o el selector de archivos de una app en fullscreen ya
  no se queda detrás de ella. El stacking quedó unificado en un único helper
  usado por `arrange`, `restack` y `focus`.

- **Sistema de comandos tipado.** Nuevo módulo `core::commands` que define
  comandos puros (`FocusDirection`, `MoveWindowToMonitor`, `SetGaps`,
  `ToggleFloat`, etc.). Cada comando es una transformación pura sobre
  `State`/`Cfg` que devuelve los `Effect` y (opcionalmente) el evento de
  dominio que representa, sin conocer X11. Se ejecutan vía
  `Engine::execute()`. `Engine::dispatch(Action)` se mantiene — y se
  re-documenta como el **mapeador canónico** del DSL wire (teclado, IPC,
  TOML) hacia los comandos: `Action::MoveDir` delega en el comando
  `MoveWindow` en lugar de construir effects a mano, eliminando la doble
  implementación. Añadir una acción nueva tocará `types.rs` (variant),
  `core/commands.rs` (comando), `core/engine.rs` (arm de dispatch) y los
  parsers (`core/ipc.rs`, `userconfig.rs`) — no un solo archivo.

- **Rounded corners, no compositor required.** New `general.corner_radius`
  (default `0`, disabled) shapes every managed window's outer edges
  (content + border) via the X11 Shape extension's bounding mask —
  `x11rb`'s `shape` feature, no cairo/pango, no new runtime dependency.
  Implemented as an O(radius) list of `Rectangle`s (one middle band plus one
  1px row per corner pixel, inset by that row's circle chord), applied in
  `apply_geom` only when `corner_radius > 0` — with the default, not a
  single Shape request is ever sent. Composes fine with picom if you're
  already running one for shadows/opacity/animations.
- **Split inner/outer gaps, plus smart gaps.** `gaps` is now
  `gaps_inner` (between windows/columns) and `gaps_outer` (screen edges),
  configurable independently; the legacy `general.gaps` TOML key still sets
  both at once. New `general.smart_gaps` collapses gaps to `0` when a
  workspace has exactly one tiled window (border width is untouched).
  Column layout only applies `gaps_outer` on the vertical axis, since it
  scrolls horizontally and has no fixed left/right screen edge; Grid, which
  doesn't scroll, applies it on all four sides.
- **Named color-theme presets.** `general.theme` in the TOML config
  (`catppuccin-mocha`, `catppuccin-latte`, `gruvbox`, `nord`, `dracula`,
  `everforest`, `solarized`) fills `col_normal`/`col_focused`/`col_urgent`
  from `config::theme_palette`. Applied before `[colors]` in the merge
  order, so an explicit `[colors]` entry always wins field-by-field over
  the theme — pick a preset and tweak just one color if you want.
- **Per-app cosmetic rule overrides.** `Rule` gained `opacity: Option<f32>`
  (written once at manage time as `_NET_WM_WINDOW_OPACITY`, a no-op without
  a compositor, applies to tiled and floating windows alike) and
  `border_w: Option<u32>` (overrides border width for that app —
  **floating windows only**; tiled/column geometry keeps one uniform
  border width across the whole layout since the column-width/row-height
  formulas in `core/layout.rs` assume a single shared value per column).
  Both are settable per-rule in `config.toml` (`opacity`, `border_width`/
  `border_w`).
- **Pluggable layout trait + LayoutRegistry.** The monolithic
  `match layout { Column => ..., Grid => ... }` in `core::layout::arrange`
  is gone. Layouts implement the `Layout` trait (`name`, `arrange`) and
  register themselves in `LayoutRegistry`, which `LayoutKind` maps into.
  `LayoutKind` derives `Hash` for the registry's `HashMap`. Adding a layout
  still needs a `LayoutKind` variant + parser and a short name in
  `ipc::layout_name()`, but the arrangement logic itself is now a single
  trait implementation instead of a growing match.

### Changed

- `Cfg::gaps` → `Cfg::gaps_inner` + `Cfg::gaps_outer` (breaking for anyone
  constructing `Cfg` directly instead of going through `config.toml` or
  `compiled_config()`).
- `Rule` now derives `Default`; existing struct literals need
  `..Default::default()` to pick up the new `opacity`/`border_w` fields.
- Floating-window border width in `core/layout.rs` is read from the
  client's own `border_w` (so `Rule::border_w` overrides take effect)
  instead of always using the global `Cfg::border_w`. Tiled windows are
  unaffected — they still use the uniform `Cfg::border_w`.

- **Presentation overlay desacoplada del foco (`core::present`).** El
  fullscreen/maximized es ahora una capa *persistente* por espacio de
  trabajo: una ventana presentada cubre pantalla (`fullscreen`, borde 0) o
  workarea (`maximized`, borde 0) **mientras sus flags estén activos**, tenga
  o no el foco. `focus()` ya no recalcula geometría al mover el foco: un
  cambio de foco genera **cero** `ConfigureWindow` sobre la ventana
  presentada (lag X11 y resizes en cascada eliminados). Los tiles de debajo
  se siguen calculando igual (layout puro sin reflow por foco), así que al
  salir de la overlay se vuelve exactamente adonde se estaba. Nuevo
  comportamiento *peek*: al enfocar un mosaico normal con una overlay activa,
  éste se eleva por encima de la ventana presentada — sin redimensionar a
  nadie — para que se vea dónde está el foco.
- **Filas uniformes por columna (`core/layout`).** Se elimina el `split_bias`
  del reparto vertical: todas las ventanas de una columna comparten la misma
  altura y el foco se marca solo con borde/color. Subir/Bajar entre mosaicos
  ya **no reflowa** (antes cada movimiento redimensionaba todas las filas de
  la columna enfocada, la causa del lag al navegar).

- **Optional TOML configuration.** Maverick now reads
  `$XDG_CONFIG_HOME/maverick/config.toml` (falling back to
  `~/.config/maverick/config.toml`) layered over the compiled defaults, with
  per-section overrides for `[general]`, `[colors]`, `[[keybindings]]`,
  `[[rules]]` and `[autostart]`. Loading is fail-safe: a missing file is
  ignored silently, one that fails to parse is rejected whole (falls back to
  the compiled config), and an individual entry with an unknown key name or a
  broken action string is dropped with a warning — a bad config can never
  prevent the WM from starting. Keybinds you define that contain a digit disable the default
  `super+1..9`/`super+shift+1..9` workspace auto-bindings for that digit;
  everything else keeps auto-generating as before.
- **Real configuration hot-reload.** `ControlCommand::Reload` (via
  `maverickctl reload`) now re-reads the TOML from disk through the same
  fail-safe loading path, swaps the engine config, regrabs the keymap and
  re-arranges every monitor — no restart needed. A tag-count change
  reconciles every monitor's workspace list (grows/truncates, clamping any
  windows left on a removed workspace) before the redraw.
- **The typed EventBus now drives the `subscribe` wire.** `maverickctl
  subscribe` `focus`/`workspace` lines are produced by a `HubEventSink`
  subscribed to the typed `EventBus`, so pointer-driven focus changes and
  window manage/unmanage announce themselves there too. `publish_state` no
  longer string-diffs a hand-built protocol; it only publishes the JSON
  snapshot when it actually changed. All focus changes funnel through the
  backend's single `focus()` choke point, which publishes `FocusChanged`
  (and `manage()`/`unmanage()` publish window mapped/unmapped).

### Fixed

- **Floating windows never opened centered, and could land off-screen
  entirely if created mid workspace-switch.** `manage()` was trusting the raw
  X geometry captured when a window is created. Toolkits center dialogs
  relative to their parent's *current on-screen* position — if the parent
  happened to be off-screen at that exact instant (see
  `hide_offscreen()` in `backend/x11/render.rs`, which parks windows on
  hidden workspaces at a negative x rather than unmapping them), the new
  dialog inherited that bogus position, got clamped to a workarea edge, and
  effectively vanished. Portal-spawned file pickers (no real
  `WM_TRANSIENT_FOR`) never had a sane position to begin with. Maverick now
  computes floating-window position itself: centered on the transient
  parent's real *stored* geometry when there is a parent, otherwise centered
  in the assigned monitor's workarea — width/height from the original
  request are kept, only position is recomputed.

- **RandR monitor hot-plug could go unnoticed.** Some X servers only deliver
  RandR events, not the root `ConfigureNotify` Maverick was relying on, so a
  plugged/unplugged monitor left stale geometry until a full restart. Maverick
  now calls `RandrSelectInput` on the root and handles `RandrNotify` /
  `RandrScreenChangeNotify` through the same topology re-detect path
  (guarded by an "actually changed" check, so no needless reflows).
- **`ConfigureRequest` dropped `above_sibling`.** Restack requests that
  position a window above a specific sibling (used by docks and some
  compositor helpers) were ignored — only `STACK_MODE` was honored. The
  `SIBLING` value-mask bit is now passed through to `configure_window`.
- **El marco de una ventana maximizada sobresalía de la pantalla.** La
  presentación `maximized` conservaba el borde del cliente: como X11 dibuja
  el borde *fuera* del rect `(x,y,w,h)` (semántica de `xproto`), una ventana
  con borde `b > 0` invadía los píxeles reservados/adyacentes del monitor.
  El overlay `maximized` ahora aplica borde 0 sobre la `workarea`, igual que
  `fullscreen` — nunca sobresale y respeta las regiones reservadas.
- **Una ventana de la overlay desmapeada dejaba su stack sucio.** Si una
  ventana fullscreen/maximized se cerraba, crasheaba o se desmapeaba sin
  estar enfocada, su `WindowId` seguía en la lista de clientes hasta el
  `DestroyNotify`; cualquier `restack`/`arrange` podía proyectarla o elevarla
  como si existiera. `on_unmap` ahora purga al instante (con `unmanage`) a
  toda ventana presentada que se desmapea — las tiles/floats conservan el
  comportamiento ICCCM (se mantienen retiradas hasta destroy/re-map). El
  riesgo `BadWindow` desaparece.
- **El fallback de foco ignoraba la overlay.** Si cerrabas un mosaico en modo
  *peek* (focus sobre un tile mientras un fullscreen cubre la pantalla), el
  foco caía sobre un mosaico invisible bajo la overlay. `best_focus` ahora
  prefiere la ventana fullscreen/maximizada más reciente del workspace antes
  que la columna/stack — al cerrar el mosaic peek, el foco vuelve a la
  ventana presentada.
- **Directiva para compositores externos (`_NET_WM_BYPASS_COMPOSITOR`).**
  Al entrar en fullscreen se escribe `_NET_WM_BYPASS_COMPOSITOR = 2` ("bypass
  mientras esté en fullscreen") y se borra al salir. Picom & co. dejan de
  redirigir/aplicar sombras a vídeo o juegos en fullscreen → menos lag de
  entrada y más FPS.

### Removed

- **`serde` + `toml` dependencies; new zero-dependency `maverick-toml` crate.**
  The config parser is now a local strict TOML-subset crate (`maverick-toml`)
  with **zero external dependencies**, replacing `serde 1.0.229` and
  `toml 0.8.23` (and their transitive `winnow 0.7.15`, `indexmap 2.14.0`,
  `serde_spanned`, `toml_datetime`, `toml_edit`). `src/userconfig.rs` was
  rewritten to consume its event iterator (`Section` / `ArraySection` /
  `KeyValue`) instead of `serde` derives, preserving the same fail-safe
  contract (syntax error → whole file rejected → compiled defaults; semantic
  errors dropped per-entry with a warning) and all value aliases (`border_w`/
  `border_width`, `col_normal`/`normal`, `type`/`window_type`,
  `ws`/`workspace`, `commands`/`apps`/`programs`, …). Supported syntax:
  `[section]`/`[[tables]]`, plain key = `value`, ints (negative), `0x…` hex,
  floats, booleans, basic strings `"…"`, flat string/int lists and nested
  `autostart`-style grids; single-quoted strings, dotted keys and exports are
  rejected. **Binary shrinks ~21%** on the same release profile (stripped):
  the config-parsing layer measured in isolation drops from 614.8 KB
  (serde+toml) to 357.1 KB (maverick-toml), and the stripped `maverick` binary
  with identical functionality measured 934,160 B.

- **Dead code and dead atoms.** `Client::is_dialog`, `Client::tags`/
  `TagMask`, the empty `on_focus_in` stub, `Layout::handle_action`, the
  never-emitted `Effect` variants (`ArrangeAll`, `MapWindow`, `UnmapWindow`,
  `UpdateEwmhDesktops`, `UpdateClientList`), and ~40 atoms that were interned
  and advertised but never read are gone. `_NET_SUPPORTED` now lists only the
  atoms the WM actually acts on, and the `#[allow(dead_code)]` escapes hatch
  is removed.
- **Duplicate string-escape logic.** Two private `json_escape`/`unquote`
  copies (CLI and core IPC) are replaced by `maverick_sys::json` — including
  a real `\uXXXX`-aware `json_unescape` that the previous implementation
  mishandled.

- **Compositor orchestration removed from the WM's startup sequence, and
  `startup_sound` dropped entirely.** `main.rs` no longer spawns a compositor
  before `WindowManager::new()`, waits a fixed delay for it to attach, or
  plays a startup chime — that was three phases of bespoke process-spawning
  logic for something `autostart` already does for the bar, wallpaper, and
  everything else. `Cfg::compositor`, `compositor_delay_ms`, and
  `startup_sound` are gone; put your compositor in `autostart` like any other
  program (see README).
- **`Monocle` layout removed entirely.** It never left an experimental
  state and added a third code path to every layout-dispatching site
  for little benefit over `Grid`. Removed `LayoutKind::Monocle` and
  `arrange_monocle()`, the `Super+M` keybind, the `monocle` IPC/CLI
  layout name (`maverickctl dispatch layout monocle` no longer
  parses), and all related tests/docs. `cycle_layout()` now wraps
  Column→Grid→Column. Only two layout modes ship: **Column** (the
  niri-style scrollable layout, stable) and **Grid**.

- **Internal bar removed.** Drawing a status bar isn't the window manager's
  job — it duplicated what polybar/waybar/eww already do well, and its removal
  drops the plain X11 core-font rendering path (`open_font`/`query_font`/
  `image_text8`/`to_latin1`) entirely. Removed `src/backend/bar.rs` and
  `src/backend/x11/bar.rs`, the `internal-bar` Cargo feature, the `Bar` struct,
  `Action::ToggleBar` (+ its `Super+B` keybind and `toggle-bar` IPC verb), the
  `Effect::UpdateBar`/`SyncBarVisibility`/`RecalcWorkarea` variants, and the
  `Cfg`/`Monitor` bar fields (`bar_height`, `top_bar`, `col_bar_*`,
  `internal_bar_height`, `show_bar`, `bar_win`, `bar_gc`). maverick still
  reserves screen space correctly for any external bar via
  `_NET_WM_STRUT_PARTIAL` (`backend/x11/struts.rs`, untouched); root `WM_NAME`
  is still read into `state.status` and exposed over IPC for external bars.
  See README for a polybar example.

### Changed

- **`cargo build --release` no longer ships a status bar.** The
  `internal-bar` feature (previously on by default) is gone, so a default
  build now expects an external bar (polybar/waybar/eww) launched from
  `autostart`, relying on the WM's strut reservation. This is a breaking
  change for anyone who relied on the built-in bar — point your `autostart`
  at an external bar (see README).
- **Default config genericized for distribution.** `config.rs`'s
  `load_config()` carried a maintainer's personal machine setup —
  a hardcoded Dvorak `setxkbmap` autostart entry, a wallpaper
  launched from a home-directory path, and an unrelated personal
  DNS tool — none of which mean anything on a fresh install. Removed
  all three; the shipped `autostart` now only launches the
  `xdg-desktop-portal(-gtk)` pair needed for file-picker dialogs to
  work, with a commented example showing where to add your own
  wallpaper command. Also dropped the `polybar` autostart entry,
  which duplicated the `internal-bar` feature that's already on by
  default.

### Fixed

- **Build was broken on `main`: `Monocle` removal had been done half
  way and taken unrelated code with it.** An in-progress edit had
  deleted `LayoutKind::Monocle` from `types.rs` but left `config.rs`,
  `core/ipc.rs`, and `core/tests.rs` still referencing it (wouldn't
  compile). Worse, the same edit accidentally deleted `arrange_grid()`
  and `ideal_scroll()` from `layout.rs` in their entirety along with
  `Workspace.scroll`, and rewrote the column-position formula from
  `wa.x - ws.scroll` to a fixed `wa.x` — silently disabling the
  Column layout's horizontal scrolling. All of the above is restored;
  `Grid` and scrollable `Column` both work again and Monocle is now
  fully (not partially) gone.
- **`CHANGELOG.md` contained an unresolved git merge conflict
  marker** (`=======`) followed by a duplicate copy of the
  keyboard-freeze fix entry already documented above it. Removed the
  marker and the duplicate section; no information was lost since the
  content was a verbatim repeat.
- **`clippy::new_without_default` on `State::new()`.** Added `impl Default
  for State` (`fn default() -> Self { Self::new() }`). Pre-existing before
  the internal-bar removal; caught while re-verifying against the exact
  1.82 MSRV toolchain.

### Quality

- **Enforced `rustfmt` across the workspace** — formatted all crates with
  `cargo fmt` to a consistent style.
- **Fixed all `clippy` warnings** — resolved 10 lints across `bar.rs`,
  `manage.rs`, `engine.rs`, `types.rs`, and `ipc.rs` (`map_unwrap_or`,
  `doc_markdown`, `redundant_closure_for_method_calls`, `match_same_arms`,
  `unnecessary_min_or_max`). Clippy is now clean at `-D warnings`.
- **Clean `rustdoc` build** — fixed unclosed HTML tags (`<pid>`, `<px>`,
  `<n>`, `<cmd>`) in doc comments; docs now build with
  `RUSTDOCFLAGS="-D warnings"`.
- **Expanded `.gitignore`** — added `coverage/`, `*.profraw`, `.env`,
  editor swap files, and common Rust build artifacts to prevent accidental
  commits.
- **Added `rust-version` and metadata** — `Cargo.toml` for all three
  workspace crates now declare `rust-version = "1.82"`, `repository`,
  `categories`, and `keywords` for better crate index presentation.
- **Doc-comment fixes** — `image_text8`, `draw()`, and code samples in
  docstrings now use proper backtick quoting.

### Fixed

- **`maverick-sys`: control socket could be tricked by symlink attack.**
  `remove_file` ran before `bind` without checking the existing file
  type; a symlink pointing outside the runtime dir would be followed.
  Now only removes the path if it is a regular socket. Also: unbounded
  thread creation per connection limited to 32 concurrent handlers;
  `identity_json` now escapes all JSON-special characters instead of
  only quotes and newlines; `send_command` rejects commands containing
  `\\n` to prevent line-protocol injection.

- **`maverick-sys`: identity ficha parser failed on process names
  containing `)` or commas in field values.** `/proc/<pid>/stat`'s
  second field (comm) is enclosed in parentheses but the comm itself
  may contain `)`. Switched from `find(')')` to `rfind(')')`. The
  custom JSON parser split on `,` unconditionally, breaking when a
  string value contained a comma; replaced with a char-by-char walker
  that respects JSON string quoting.

- **`maverick-sys`: `wait_readable` busy-looped on `POLLERR`/`POLLHUP`.**
  `poll()` returning `> 0` was treated as "data available" regardless
  of `revents`. Now checks that `POLLIN` is actually set so an error
  state doesn't spin the event loop.

- **UnmapNotify no longer removes windows from the workspace.**
  Previously, every `UnmapNotify` (e.g. iconify) called `unmanage()`,
  which removed the window from `clients`, the workspace structure, and
  the focus stack. When the window was later remapped, it was re-managed
  as a new window, losing its workspace assignment, floating state, and
  column position. Now, non-synthetic `UnmapNotify` events only clear
  `WM_STATE` and move focus if the window was focused. The window stays
  in the workspace so its tiling state is preserved across iconify/restore.

- **FocusIn handler no longer steals focus from popups and dialogs.**
  The `on_focus_in` handler attempted to re-focus the WM's focused window
  whenever any window received a `FocusIn` event. This caused popups and
  dialogs (e.g. Firefox file pickers, GTK dialogs) to immediately lose
  focus back to the main window. The handler has been removed entirely;
  focus is now managed exclusively through keybindings, mouse clicks, and
  EWMH requests (`_NET_ACTIVE_WINDOW`).

- **Moving a window to another monitor no longer panics on workspace overflow.**
  When moving a window to a monitor with fewer workspaces than the source,
  the workspace index could exceed the destination monitor's workspace count,
  causing a panic. The workspace index is now clamped to the destination
  monitor's valid range.

- **`_NET_WORKAREA` now reports all monitors.** Previously, only the first
  monitor's workarea was reported for all desktops, which caused incorrect
  workarea values for external taskbars and docks on secondary monitors in
  multi-monitor setups.

- **Monitor hotplug preserves client workspace assignments.** When the number
  of monitors changes (hotplug), clients are no longer blindly reassigned to
  monitor 0 workspace 0. Their original monitor and workspace assignments are
  preserved where the target still exists; only clients on removed monitors
  are reassigned to valid targets.

- **Geometry-only monitor changes now trigger rearrange.** When a monitor's
  resolution or position changes (without adding/removing monitors), the
  previous code only updated `screen` and `workarea` without calling
  `arrange()`, leaving windows with stale geometry. All affected monitors
  are now re-arranged after a geometry-only change.

- **`focus_mouse` no longer triggers an X11 `query_tree` round-trip on every
  motion event.** The `on_motion` handler called `find_client()` (which walks
  up the window tree via `query_tree`) for every mouse movement when
  `focus_mouse` was enabled, causing significant lag. Focus-follows-mouse is
  now handled exclusively via `EnterNotify` events in `on_enter`, which are
  far less frequent.

- **`focus()` no longer computes `prev_focused` twice.** The previously-focused
  window was computed at the top of the function and again just before the
  unfocus logic. The redundant second computation has been removed.

- **`focus_dir` Next/Prev now filters by active workspace.** The focus stack
  could contain windows from different workspaces. Cycling Next/Prev could
  jump to a window on a different workspace without switching workspaces,
  leaving the user confused about which workspace they were on. Now only
  windows on the active workspace are considered.

- **`restart()` now cleans up the control socket before `exec()`.** The
  previous implementation called `exec()` without removing the Unix socket
  file or the identity ficha, which could prevent the new process from
  binding to the socket on restart. The socket and ficha are now removed
  before `exec()`.

- **Removed dead code `Focus.window_idx`.** The `window_idx` field on the
  `Focus` struct was set in multiple places but never read for layout or
  focus determination. The actual focused window in a column is determined
  by `Column.focused`, not `Focus.window_idx`. The field and all references
  to it have been removed.

- **`maverick-sys`: `detach_from_terminal` ignored `setsid()` failure.**
  If the process was already a session leader, `setsid()` returns
  `EPERM` and the WM would not actually detach. The return value is now
  discarded (the subsequent `isatty` check still works), but the intent
  is clearer and the function no longer silently depends on it
  succeeding.

- **`maverick-sys`: `hub::emit` held the subscriber mutex during
  channel sends.** A slow `subscribe` connection could block the WM
  thread. The subscriber list is now cloned under the lock and the
  actual sends happen outside it.

- **`maverickctl`: TTY confirmation read input byte-by-byte, breaking
  UTF-8 multi-byte characters.** `read(&mut [0u8;1])` and `as char`
  produced garbled strings for non-ASCII input. Replaced with
  `read_line` for correct Unicode handling.

- **`core`: `CycleLayout`/`SetLayout` could panic on a monitor-less
  state.** Both actions accessed `self.state.monitors[mi]` without
  verifying the index was in bounds. Added the same guard used by
  `ToggleBar` and other actions.

- **`core`: `collapse_col` computed ideal scroll before collapsing,
  leaving the viewport slightly off-centre.** Moved the
  `ideal_scroll` call to after the column is removed so it reflects
  the new column count.

- **`core`: `focus_mon`/`move_mon` treated `Dir::Left` and
  `Dir::Right` identically to `Dir::Next`** (always wrapping right).
  They now map `Left`/`Prev` to decrement and `Right`/`Next` to
  increment, matching user expectation.

- **`core`: missing `UpdateBar` effects after workspace/view changes.**
  `View`, `MoveToWs`, `CycleLayout`, and `SetLayout` did not mark the
  bar dirty, so the tag-active / layout-symbol / occupancy display
  could become stale. Added `Effect::UpdateBar` to each path.

- **`core`: `PublishIpcState` was never emitted.** The effect variant
  existed but no dispatch path produced it. Now pushed at the end of
  every `dispatch()` that produced at least one effect.

- **`core`: floating windows were not clamped to the workarea in
  `arrange_columns`.** The floating pass pushed `client.geom`
  verbatim; windows could be placed entirely off-screen. Added clamp
  to the workarea rect.

- **`core`: `Client::new` always initialised `tags: 1`**, ignoring
  the `workspace` parameter. Changed to `tags: 1 << workspace` so the
  tag mask matches the assigned workspace from creation.

- **`core`: `Rule::matches` compared lowercase `class`/`title` against
  an unnormalised pattern.** A rule written with uppercase letters
  would never match. The pattern is now also lowered before comparison.

- **`main`: identity ficha left on disk if `WindowManager::new`
  failed.** `write_meta` runs before WM initialisation; a subsequent
  init failure called `process::exit(1)` without cleaning up the
  ficha, leaving a zombie entry for tools like `maverickctl list`.
  Added `cleanup_meta` call in the error path.

- **`x11/events`: resolution change not detected when monitor count
  stayed the same.** The RANDR notify handler only acted when
  `new_mons.len() != old count`; a resolution or position change
  that kept the same number of monitors was silently ignored.
  Added per-monitor geometry comparison.

- **`x11/manage`: `find_client` could loop infinitely on a cyclic
  window tree.** The function walked the X11 window tree upward
  without tracking visited windows; a client creating a parent cycle
  would hang the WM. Added a `HashSet` guard.

- **`x11/render`: `ConfigureNotify` coordinates truncated silently.**
  `hide_offscreen` pushes windows far left (`i32::MIN`), which when
  cast to `i16` wrapped to 0, making offscreen windows visible.
  Values are now clamped to `i16`/`u16` ranges before casting.

- **`x11/render`, `ewmh`: potential panic on empty monitor list.**
  `focus()` and `update_workarea` indexed `monitors[0]` or assumed
  `client.monitor` was always valid. Added bounds checks / `.first()`.

- **`x11/input`: keyboard froze after mouse-focusing a window**
  (`grab_buttons`). The catch-all `grab_button` used `pointer_mode=SYNC`
  **and** `keyboard_mode=SYNC`. Every matching `ButtonPress` froze both
  devices, but `on_button_press` only called
  `allow_events(REPLAY_POINTER)`, which releases the pointer but not
  the keyboard. The keyboard stayed frozen at the X11 level after
  clicking any managed window, breaking WM shortcuts and the client's
  own key input — most noticeable with clients that grab focus
  aggressively on click (Firefox, Minecraft). `keyboard_mode` changed to
  `ASYNC` (standard practice, matches dwm/i3-style click-to-focus
  grabs); `pointer_mode` stays `SYNC` since `on_button_press` still
  needs to conditionally replay or keep it frozen for drags.
  Confirmed fixed in real usage (mouse click-to-focus, tested against
  Firefox and Minecraft).

- **`x11/manage`: `write_net_wm_state` overwrote unknown EWMH
  atoms.** It replaced `_NET_WM_STATE` with only the fullscreen/
  maximized flags the WM tracks, discarding `_NET_WM_STATE_STICKY`,
  `_NET_WM_STATE_HIDDEN`, etc. set by other tools. Now reads the
  current atom list first and preserves unmanaged atoms.

- **`backend/bar`: potential `u16`/`i16` overflow in label and
  glyph calculations.** Arithmetic on `u16`/`i16` values could wrap
  with many wide tags. Converted to `i32` intermediates with
  saturating operations and final clamp to the target type.

### Changed

- **`core`: `PublishIpcState` emitted after every state-mutating
  `dispatch`.** Previously the effect existed but was never produced;
  now pushed automatically so IPC subscribers (bars, `maverickctl
  subscribe`) receive fresh snapshots without explicit
  per-action wiring.

- **`core`: `focus_mon`/`move_mon` now accept directional variants.**
  `focus-mon left`/`right` and `move-mon left`/`right` now move in
  the expected direction instead of always wrapping to the next
  monitor (which was the behaviour of `next`).

## [0.18.2] — 2026-07-19

Two prior attempts at the next release (internally called "0.18.4" in
early planning) added a stack of new features — TOML config, a
"Window" floating layout, a predictive prefetch daemon — but both were
abandoned after serious regressions during development, including one
where `backend/x11/mod.rs` was lost outright to an accidental
`git checkout --` and had to be reconstructed from an old blob. Rather
than resume that feature list, this release starts over from `main`
(v0.18.1) with a narrower goal: **pay down the coupling between the
domain model and X11** so a non-X11 backend (Wayland) becomes possible
later, without adding user-facing features. No TOML config, no new
layout modes, no prefetch daemon in this release — that work is
shelved, not lost, and can be revisited once the split below is
further along.

### Added

- **Instance control plane** (`maverick-sys`, new workspace member):
  `identity` (per-instance PID/display/tty "ficha" under the runtime
  dir), `control` (`ControlServer` — a Unix-socket protocol:
  `ping`/`identify`/`state`/`dispatch`/`restart`/`reload`/`subscribe`/
  `quit`), `hub` (`ControlHub`, the MPSC bridge between the socket
  thread and the single-threaded X11 event loop), `discover`
  (list/find/quit instances by name or display). Replaces the old PID
  file + `pkill`-by-name approach from the abandoned line.
- **`maverickctl`** (`maverick-sys/src/bin/`): CLI for the above —
  `list|state|msg|subscribe|quit[--confirm]|quit-all|restart|reload|prune`.
  Instance resolution: `--name` → `$MAVERICK_INSTANCE` → sole live
  instance → refuse/ambiguous list.
- **`maverick-dialog`** (new workspace member): standalone X11
  yes/no confirmation window, the only `x11rb` user outside the WM
  itself. `Mod4+Shift+Q` now spawns `maverickctl quit --confirm`
  instead of calling `Action::Quit` directly, so a stray keypress
  can't kill the session; the raw `Action::Quit` is still reachable
  over the control socket.
- **Maximize** implemented for real: `WinFlags::MAXIMIZED`,
  `Client::is_maximized()`; a maximized-but-not-fullscreen focused
  window fills `workarea` (respects bar/dock struts) and keeps its
  border, vs. fullscreen which covers the whole screen with no
  border. `_NET_WM_STATE_MAXIMIZED_VERT/HORIZ` handled on both read
  (initial `manage()`) and write (`on_client_message`).
- **External dock support**: docks (Waybar/Polybar/etc.) are detected
  by `_NET_WM_WINDOW_TYPE_DOCK`/`_DESKTOP`, never by process name, and
  reserve space via `_NET_WM_STRUT_PARTIAL`/legacy `_NET_WM_STRUT`,
  tracked per-monitor and released on destroy/unmap.
- `internal-bar` Cargo feature (default on): `cargo build --release
  --no-default-features` builds without the internal status bar for
  people driving Waybar/Polybar instead.
  (`1a36561`, `c23087c`)

### Changed

- **`core/` rebuilt around one seam**: `Engine::dispatch(Action) ->
  Vec<Effect>` is now the *only* path from user/IPC intent to state
  mutation. `Effect` is a semantic vocabulary (`ArrangeMonitor`,
  `FocusWindow`, `SetFullscreen`, …) — the backend's `execute()` is
  the only place that turns those into X11 calls. This removes the
  previous split-brain where `backend/x11.rs` reimplemented action
  handling separately from a dead `core/engine.rs::process_event`
  path that only 3 stale unit tests exercised.
- **Fullscreen re-modeled as presentation, not a state machine
  block.** The old approach guarded `do_action`/`on_button_press` to
  refuse input while any window was fullscreen — a patch on the
  symptom that still left stale fullscreen windows on screen when
  focus moved via `map_request` or an EWMH message. `core/present.rs`
  now rewrites *only the focused* window's rect to `mon.screen` when
  it's fullscreen (`layout.rs::arrange` stays pure geometry); `focus()`
  re-arranges on every fullscreen transition. Maximize reuses the same
  seam (fullscreen > maximized > layout precedence).
- **`backend/x11.rs` split** into `backend/x11/{mod,manage,events,
  ewmh,input,pointer,render,struts,bar,actions}.rs` (previously one
  ~2900-line file). No behavioural change, just navigability.
- Dead code removed: `core/engine.rs`'s old `process_event`/`AppEvent`/
  `Command` path, `core/events.rs`, `core/commands.rs`,
  `Workspace::move_window_right()` (flagged unused in 0.18.1).

### Fixed

- **`WindowId` was an alias for x11rb's `Window`, not a real
  backend-agnostic type** (`src/types.rs`). The domain model — the part
  that's supposed to have zero X11 knowledge — imported
  `x11rb::protocol::xproto::Window` directly. `WindowId` is now a
  plain `u32` with no dependency on `x11rb`; since x11rb's `Window` is
  itself a `u32` alias, this is behaviourally a no-op (no cast sites
  needed anywhere in `backend/`) but it removes the last x11rb import
  from `core`/`types.rs`. Confirmed via `grep -rl x11rb src/core/
  src/types.rs` returning nothing after the change.
  (`fe2e766`)

### Known issues — core/backend separation (in progress, tracked here on purpose)

This is the actual roadmap item for the next few passes, not a
finished job. Concrete couplings found while reading through the
current tree, ranked by how much they'd block a Wayland backend:

1. **`backend/x11/manage.rs::manage()` mixes protocol decoding with
   domain decisions in one ~500-line function.** Reading raw
   `_NET_WM_WINDOW_TYPE`/`WM_HINTS`/`WM_NORMAL_HINTS` property bytes
   and *deciding* `is_dialog`, `WinFlags::FLOAT`, `WinFlags::URGENT`,
   tag/workspace placement, etc. are interleaved line-by-line. A
   Wayland backend would have to re-derive all of that decision logic
   from scratch instead of calling one shared function with its own
   protocol-specific extraction feeding in. Next step: extract a
   backend-agnostic `fn classify_client(info: WindowInfo) -> (WinFlags,
   bool /*is_dialog*/, …)` in `core/` that both backends call after
   doing their own (necessarily protocol-specific) property reads.
2. **`Cfg::keybinds: Vec<(u16, u32, Action)>`** stores raw X11
   modifier-mask bits and X keysyms directly as the config's own
   types (`config.rs::load_config` builds them via
   `x11rb::protocol::xproto::ModMask`). Config itself doesn't import
   x11rb (the raw ints are backend-agnostic on their face), but the
   *meaning* of those ints is X11-specific; a Wayland backend using
   `xkbcommon` keysyms would happen to reuse the same keysym space but
   not the modifier-mask bit layout. Not urgent — flagging so it isn't
   assumed to be already-portable.
3. **Rule matching (`config.rs::Rule::matches`) runs on `class`/`title`
   strings that only X11's `WM_CLASS`/`_NET_WM_NAME` naturally
   produce.** Wayland equivalents (`app_id`, xdg-shell title) map
   cleanly onto the same two strings, so this one is low-risk, but it's
   still backend-shaped data flowing through a `core`-owned type.
4. **Bar visual style reverted to 0.18.1.** The rebuild's `backend/bar.rs`
   picked up several cosmetic additions along the way — an active-monitor
   marker block, a bottom accent underline, an extra green "occupied" dot
   drawn next to tags whose label was already colored green for the same
   state, and "…" truncation on long titles/status text. Net effect read as
   visually noisy/cluttered rather than an improvement, so `backend/bar.rs`
   was restored byte-for-byte to the 0.18.1 version (`22a6352`). Verified no
   other file referenced the removed symbols (`COL_LAYOUT_CYAN`,
   `truncate_latin1`, `tag_width`, `separator()`, `START_X`, `ACCENT_H`) —
   `backend/x11/bar.rs` and `pointer.rs` only call `Bar::draw`/`Bar::tag_at_x`,
   whose signatures are unchanged, so this is a pure revert with no other
   code affected.
   (`2602f43`)

## [0.18.1] — 2026-07-02

### Fixed

- **Quit confirmation dialog was non-functional.** `Action::QuitConfirm`
  set `running = false` directly — identical to `Action::Quit` — with no
  dialog window ever created anywhere in the codebase. `quit_win` was
  declared, initialized, and read in two places (`restack()`,
  `on_destroy()`), but never once assigned `Some(_)`. Consolidated into a
  single `Action::Quit` bound to `Mod4+Shift+Q`; removed the dead
  scaffolding (`quit_win` field, the raise-above-fullscreen hook, the
  destroy-notify cleanup hook, an orphaned doc comment left dangling above
  an unrelated function).
  (`3465939`)

- **Bar workspace-tag clicks could desync from what was rendered.**
  `tag_at_x()` counted glyphs by filtering out every character above
  U+00FF; `draw()`'s `to_latin1()` counts every character and substitutes
  `?` for anything above U+00FF. Same tag name in, two different glyph
  counts out — the click hitbox drifted from the rendered label the
  moment a tag name held a non-Latin1 character. Invisible with the
  default numeric tag names, but breaks click-to-switch for anyone who
  customizes them with icons, CJK, or emoji. `tag_at_x()` now calls
  `to_latin1()` directly so the two can't diverge again.
  (`557ba37`)

- **New-column width was inconsistent depending on how the column was
  created.** `add_tiled` (opening a new window) sized every column past
  the first at 75% of the workarea. `apply_move_dir`'s extract-to-new-
  column branch and `new_column()` (`Mod4+Shift+Return`) instead used a
  fixed `default_col_w` (700px), which doesn't scale with monitor
  resolution and made the same logical action — "put this window in its
  own column" — look very different depending on which keybind triggered
  it. All three paths now compute the same 75%-of-workarea width.
  (`6f7a9d6`)

- **Browser file-picker / upload dialogs never appeared.** Root cause of
  a previously-diagnosed issue: neither `xdg-desktop-portal` nor
  `xdg-desktop-portal-gtk` was ever started. `detect_portal()` only
  floats the dialog window once one exists — it can't conjure one if the
  backing service never launched. Added both to `autostart`, with full
  paths (`/usr/lib/...`) since neither binary lives on `$PATH` on Arch.
  (`734e4ec`)

### Changed

- New-column sizing is now unified around workarea percentage, so
  `default_col_w` in `Cfg` no longer drives column width anywhere in the
  live code path. Left the field in place for now rather than remove
  config surface as a side effect of a bug-fix pass.

### Docs

- README / README.es.md: bar section now describes the raw-X11
  (`image_text8` / `poly_fill_rectangle`) rendering path instead of the
  retired `xft.rs` FFI wrapper; dropped an unverified `~3–4 MB` resident
  memory figure from that section rather than leave it stale; keybind
  table no longer claims a confirmation dialog on quit.
  (`4499fb0`)
- Restored English-only inline comments — six spots had reverted to, or
  were left in, Spanish (one mixed both languages in the same comment
  block); fixed a compositor config comment that still described an
  opacity flag removed a few commits earlier.
  (`d452b7b`)

### Known issues

Flagged during this pass, not fixed here — bigger changes, out of scope
for a bug-fix batch:

- `core/engine.rs`'s `process_event` / `AppEvent` / `Command` path is
  never invoked by the running window manager — `backend/x11.rs`
  reimplements `ToggleBar`, `CycleLayout`, `SetLayout`, and window
  creation directly instead of going through it. 3 of the 7 unit tests
  (`test_toggle_bar_hides_and_shows`, `test_cycle_layout_wraps_around`,
  `test_window_created_emits_layout_commands`) exercise only that
  disconnected path and don't protect the code that actually ships.
- `Workspace::move_window_right()` (`types.rs`) has no caller anywhere in
  the tree — dead code, found while fixing the column-width
  inconsistency above.
