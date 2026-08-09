# Plan: estados de ventana, fullscreen por política y viewport en Maverick

## Contexto (evidencia del audit)

El audit (`1786205533483-audit-fullscreen-transient-viewport.md`) confirma que **no hace falta reescribir el sistema de ventanas**:

- El layout ya es puro/derivado (`core/layout.rs:4-6`); `arrange_columns` recalcula todo desde `State`.
- `fs_ctx` (`core/layout.rs:139-155`) ya es la única fuente de verdad del fullscreen dentro de la cinta, y `ribbon_geom`/`ideal_scroll`/`column_screen_extents` ya la consumen.
- Fullscreen en `Column` ya es una columna del ribbon (`layout.rs:294-317`); en `Grid` es overlay (`present.rs:43-50`).
- `Workspace` ya posee `camera`/`zoom`/`overview` (`types.rs:216-225`); `ribbon_geom` ya consume `alpha` del zoom.
- Maverick ya escribe `_NET_WM_BYPASS_COMPOSITOR=2` en fullscreen (`manage.rs:876-888`) y **no** depende de Picom.

**Decisión: retirar la idea de un sistema Picom dinámico.** El vsync/AA de los juegos se deja en sus manos vía el overlay real + bypass de compositor ya existente.

---

## Decisiones de arquitectura (cerradas)

1. **Tres ejes separados:** `CLIENT` (clasificación/estado de ventana) → `LAYOUT` (columnas/grid) → `VIEWPORT` (cámara/zoom/page-snap) → `GEOMETRY` → x11rb.
2. **`FullscreenState` ≠ `FullscreenPolicy`.**
   - *State* = "¿la ventana está fullscreen?" (ya existe `WinFlags::FULLSCREEN`).
   - *Policy* = "¿qué hace Maverick si pide fullscreen?" → `enum FullscreenPolicy { Normal, Deny, True }` guardado en `Client.fullscreen_policy` (no bits en `WinFlags`).
3. **Fullscreen tiene dos modos de render, no dos sistemas:**
   - `Tiled` (policy `Normal`/`Deny` vía `Mod4+F`): columna del ribbon, respeta `fs_ctx`.
   - `True` (policy `True`): overlay real fuera del ribbon + bypass compositor.
4. **Firefox:** regla `deny_fullscreen = true` → rechaza petición EWMH `_NET_WM_STATE_FULLSCREEN` en runtime; `Mod4+F` sigue dando fullscreen tiled. (Amplía `ignore_initial_state`, que solo actúa al iniciar.)
5. **Juegos:** regla `true_fullscreen = true` → overlay real, fuera del ribbon, Maverick no toca vsync/AA.
6. **`fs_ctx` NO se toca hasta completar las fases 1–5** (es el invariante del ribbon; tocarlo mal desincroniza `ribbon_geom`/`ideal_scroll`/`column_screen_extents`).
7. **Maximizado:** separar V/H en un estado propio (preparar medio-maximize, pero **no** implementar half-maximize aún).
8. **Eliminar el sentinel `Rect::default()`** usado en `set_fullscreen` (`manage.rs:855`) → estado de transición explícito.

---

## Orden de ejecución

| # | Fase | Resumen |
|---|---|---|
| 0 | Congelar comportamiento | baseline de tests + docs del comportamiento actual |
| 1 | Fix fullscreen de floats | promover float→tiled al entrar en fullscreen |
| 2 | Eliminar sentinel `Rect::default()` | estado de transición explícito |
| 3 | Corregir MAXIMIZED V/H | estado propio, no un solo flag |
| 4 | `FullscreenPolicy` | enum en `Client`, ramas en `on_client_message`/`set_fullscreen` |
| 5 | True fullscreen para juegos | overlay fuera del ribbon + bypass |
| 6 | Relink de transients diferidos | re-enlazar `transient_parent` |
| 7 | ConfigureRequest / `WM_NORMAL_HINTS` | solo tras probar toolkits |
| 8 | Abstracción Viewport | `viewport_mode`/`page_zoom` en `Workspace` |
| 9 | Viewport Zoom | `Normal` vs `Zoomed` (no es fullscreen) |
| 10 | Page-Snap | reusar `ideal_scroll`/`camera.snap` |
| 11 | Animación | reusar springs de `tick_animations` |
| 12 | Suite Xephyr | pruebas reales end-to-end |

---

## Fase 0 — Congelar comportamiento actual

- `cargo test --workspace` y `cargo build --release --workspace` deben pasar limpios.
- Añadir/refrescar tests de regresión que fijen el comportamiento actual de: fullscreen tiled (`Column`), fullscreen `Grid` (overlay), maximized, floating, transient, `override_redirect`. Basarse en `core/tests.rs` y `core/layout.rs` (tests de `fs_ctx`).
- Documentar en `README.es.md` el modelo actual (fullscreen tiled vs overlay Grid).

## Fase 1 — Fix fullscreen de floats (CRÍTICO, bug C1/A1)

Hoy: float + petición EWMH fullscreen → `set_fullscreen(true)` pone `geom = Rect::default()` (`manage.rs:855`); el camino EWMH **no** promueve al tiling (solo `ToggleFullscreen` de teclado lo hace en `commands.rs:274-285`) → el float queda 0×0.

- En `set_fullscreen` (`manage.rs:840`), si la ventana es float y la petición es de cliente (o comando), **promoverla al tiling** antes de poner el flag: `ws.remove_window(win)` → `ws.add_tiled(win, cfg.default_col_w, wa_w)` → `flags.clear(FLOAT)` + `flags.set(FS_WAS_FLOAT)` (reusa la lógica de `commands.rs:274-285`).
- Salir de fullscreen: si `FS_WAS_FLOAT`, devolver a `ws.floats` y restaurar `FLOAT` (igual que `commands.rs:293-300`).
- Esto elimina también A1 (mpv float que arranca fullscreen se comporta consistente: promueve a tiled fullscreen o se deniega según política en Fase 4).
- **Tests:** float + `_NET_WM_STATE_FULLSCREEN` vía `on_client_message` no colapsa a 0×0; sale y vuelve a float.

## Fase 2 — Eliminar sentinel `Rect::default()` (M4)

- `set_fullscreen` no debe mutar `c.geom` a `Rect::default()` como señal de transición.
- La geometría de tile siempre la calcula `arrange`; el sentinel solo importaba para el path de float (ya cubierto en Fase 1). Para forzar el re-emit de `apply_geom` cuando cambia el estado, usar un marcador explícito (p.ej. `stack_dirty` ya existe, o un `pending_geom` bool) en vez de geom 0.
- `saved_geom` para floats se queda; considerar `Option<Rect>` solo si simplifica el restore.

## Fase 3 — MAXIMIZED V/H (M1)

Hoy: `WinFlags::MAXIMIZED` (`types.rs:59`, `1<<5`) colapsa V+H; `present.rs:51` usa `workarea` completo.

- Reemplazar el flag único por estado propio en `Client`, p.ej. `maximized: Option<(bool,bool)>` o dos bits internos `MAX_H`/`MAX_V`, con helper `is_maximized_v()/is_maximized_h()`.
- Actualizar todos los consumidores: `present.rs:51` (rect = workarea recortado a los ejes activos), `types.rs:744` (`best_focus`), `manage.rs:191-195` y `579` (parseo/ignore), `set_maximized` (`manage.rs:919`), `write_net_wm_state` (`manage.rs:994-996`), `commands.rs::ToggleMaximize`.
- **No** añadir half-maximize como feature; solo dejar el estado correcto.
- **Tests:** maximize vertical solo estira en Y; `_NET_WM_STATE_TOGGLE` sigue funcionando.

## Fase 4 — `FullscreenPolicy` (feature Firefox / juegos)

- `types.rs`: añadir `pub enum FullscreenPolicy { Normal, Deny, True }` y campo `pub fullscreen_policy: FullscreenPolicy` en `Client` (default `Normal`).
- `config.rs` `Rule`: añadir `deny_fullscreen: bool`, `true_fullscreen: bool`. `userconfig.rs`: parsear desde `[[rules]]`.
- `manage.rs::apply_rules`: set `fullscreen_policy = Deny` si `deny_fullscreen`, `True` si `true_fullscreen` (precedencia True > Deny). Regla por defecto Firefox (`config.rs:290`) → `deny_fullscreen = true` (mantener `ignore_initial_state` para maximize al inicio).
- `events.rs::on_client_message` (`364-389`): si `client.fullscreen_policy == Deny` y la petición viene de cliente → **descartar** (no llamar `set_fullscreen`). `Mod4+F` (camino `commands.rs`→`Effect::SetFullscreen`) NO se descarta → fullscreen tiled.
- `set_fullscreen` ramifica por política (ver Fase 5 para `True`).

## Fase 5 — True fullscreen para juegos

- `core/layout.rs::fs_ctx` (`139-155`): **excluir** ventanas con `fullscreen_policy == True` para que no entren a la cinta (siguen siendo overlay).
- `core/present.rs::present` (`43-50`): condición de overlay fullscreen → `c.is_fullscreen() && (ws.layout == LayoutKind::Grid || c.fullscreen_policy == True)`. Así el juego tapa todo en cualquier layout.
- `manage.rs::set_fullscreen` para `True`: conservar el comportamiento legacy (raise `StackMode::ABOVE`, `_NET_WM_BYPASS_COMPOSITOR=2`) y **no** recentrar la cámara del ribbon (`manage.rs:896-908` solo para el caso tiled).
- `render.rs::stack_overlay` (`340`, `396-401`): el "covering" 2-bis solo aplica a fullscreen tiled; `True` ya se maneja por el raise legacy.
- `types.rs::best_focus` (`744`): el overlay de `True` cuenta como presentado en cualquier layout (alinear con `present`).
- **Tests:** juego con `true_fullscreen` → overlay cubre `screen`, `bypass=2`, fuera del ribbon; Firefox con `deny_fullscreen` + EWMH → ignorado, `Mod4+F` → tiled.

## Fase 6 — Relink de transients diferidos (M2)

Hoy: `manage.rs:273-281` deja `transient_parent = None` si el padre aún no se gestiona y no se re-enlaza.
- En `manage` (tras `add_client`) y/o en `on_map_request`/`unmanage`, tras gestionar un padre, revisar clientes pendientes y fijar su `transient_parent` + heredar monitor/workspace + `FLOAT`.
- No crear una capa `Attached` grande; solo el re-enlace.

## Fase 7 — ConfigureRequest (prudente, A2)

- `events.rs:88-113`: hoy se traga el ConfigureRequest de tiles y se responde sintético.
- **No cambiar a ciegas.** Probar Firefox/GTK/Qt/Java con `WM_NORMAL_HINTS` (`manage.rs:223-258` ya los lee). Solo si una app entra en bucle de resize, respetar los hints aplicando el resize dentro de los límites del tile.
- Criterio de aceptación: sin regresiones en las apps probadas.

## Fase 8 — Abstracción Viewport

- No reescribir el layout. `Workspace` (`types.rs:210-226`) gana `viewport_mode: ViewportMode` y `page_zoom: f32` (o un `struct Viewport` ligero que envuelva `camera`+`mode`+`page_zoom`); `ribbon_geom` (`layout.rs:226`) ya consume el factor de zoom.
- `ViewportMode { Normal, Zoomed }`. Evaluar si basta añadir campos a `Workspace` antes de crear otra struct.

## Fase 9 — Viewport Zoom

- `Zoomed` alimenta `alpha > 1` en `ribbon_geom` (hoy `alpha.max(0.05)`, `layout.rs:226` — no hay tope superior, así que un `alpha>1` agranda la columna). Separar `page_zoom` del `overview`/`zoom` existente.
- **No** llamarlo fullscreen: Viewport Zoom es estado de *visualización del workspace*; fullscreen es estado de *ventana/EWMH*.

## Fase 10 — Page-Snap

- Reusar `ideal_scroll` (`layout.rs:520`) + `camera.snap` (`types.rs:196`). Acción: calcular cámara objetivo de la columna vecina y snapshot/animar. En `Zoomed`, el objetivo es el viewport siguiente.

## Fase 11 — Animación

- Reusar `tick_animations` (`types.rs:895`) y los springs existentes. `input → target camera → ribbon_geom → apply_geom`. Sin Picom.

## Fase 12 — Suite Xephyr

- `Xephyr :1 -screen 1920x1080 & DISPLAY=:1 maverick &`; con `xprop`/`xwininfo`/`xev` verificar casos del audit Fase 12 (Firefox, GTK, Qt, Java/OpenGL, juego, diálogos, menus, tooltips). Sin resultados fabricados.

---

## Riesgos

- **`fs_ctx` es invariante del ribbon** (plan `1786166283911`): excluir `True` de `fs_ctx` debe hacerse en un solo sitio; si `ribbon_geom`/`ideal_scroll`/`column_screen_extents` no coinciden, la columna se dibuja desalineada. Los tests de `core/layout.rs` (`ribbon_invariants_hold_with_fullscreen`, `fullscreen_column_*`) son la red.
- **Cambiar `MAXIMIZED` de flag a estado** toca `present.rs`, `best_focus`, `set_maximized`, `write_net_wm_state`, `ToggleMaximize`, parseo en `manage.rs`: mecánico pero hay que no romper la serialización IPC (hoy inexistente para flags).
- **Promover float→tiled en fullscreen** cambia el comportamiento visible de mpv/diálogos flotantes que pedían fullscreen: antes quedaban como float pequeño (A1), ahora pasan a la cinta. Es el comportamiento deseado pero conviene avisarlo en el CHANGELOG.

## Validación

```bash
cargo build --release --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
- Manual (Xephyr + polybar/waybar): Firefox `Mod4+F` → tiled fullscreen; Firefox F11/EWMH → rechazado (sigue en mosaico). Juego que pide fullscreen → overlay real, sin tocar vsync/AA. mpv float + fullscreen → no colapsa. Maximize vertical → solo estira en Y.

## Fuera de alcance

- Half-maximize (`super+←/→`) como feature (solo se deja el estado V/H listo).
- Sistema Picom dinámico (retirado).
- Capa `Attached` grande para transients (solo re-enlace).
- `_NET_MOVERESIZE_WINDOW` / `_NET_REQUEST_FRAME_EXTENTS` (no implementados hoy).
