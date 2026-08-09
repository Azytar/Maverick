# Fullscreen que participa de la cinta scrolleable (estilo niri)

## Objetivo

Hoy `WinFlags::FULLSCREEN` saca la ventana del layout y la convierte en un **overlay
always-on-top** (`core/present.rs`). Resultado: aunque `FocusDirection` sí mueve el foco
entre columnas, no ves a dónde vas, porque el fullscreen tapa todo y `render.rs::stack_overlay`
prohíbe explícitamente que un mosaico enfocado haga *peek*.

Objetivo: replicar la propiedad de niri —
*"Thanks to scrollable tiling, fullscreen and maximized windows remain a normal participant
of the layout: you can scroll left and right from them and see other windows."*

La ventana fullscreen pasa a ser **una columna de la cinta cuyo tile mide `mon.screen`**.
Scrollea con la cámara, sale de pantalla al enfocar una columna vecina, y sigue en fullscreen.

## Decisiones cerradas

| Tema | Decisión |
|---|---|
| Alcance | Se **cambia el fullscreen existente**. No se añade ninguna acción nueva. `Mod+F`, `maverickctl dispatch toggle-fullscreen` y las peticiones EWMH `_NET_WM_STATE_FULLSCREEN` de clientes usan la misma ruta, igual que niri. |
| Caja | `mon.screen` completo (tapa la barra), borde 0, sin `gaps_inner` ni `gaps_outer`. |
| Posición | X derivada del ribbon (`world_x - camera`), o sea **scrollea**. |
| Stacking | Se raisea por encima de todo (incl. docks) **solo si está enfocada, la cámara está quieta y no hay Overview**. Si no, se apila como un tile normal y la barra vuelve arriba. |
| Columna con varias ventanas | Las hermanas se **ocultan** (misma vía que `hide_offscreen`) mientras dure el fullscreen; al salir la columna se reconstruye tal cual. |
| Flotantes | Al entrar en fullscreen el float se **mueve al tiling** (columna nueva); al salir vuelve a flotante con su `saved_geom`. Una sola mecánica. |
| Layout Grid | No tiene cinta: ahí FULLSCREEN **conserva el overlay actual** de `present.rs`. |
| `ToggleMaximize` | **Sin cambios.** Sigue siendo overlay focus-only sobre `workarea`. |

## Invariante crítica

`core/layout.rs` documenta que `ribbon_geom` es la **única fuente de verdad** compartida por
`arrange_columns`, `ideal_scroll` y `column_screen_extents`, y hay tests que lo verifican
(`ideal_scroll_matches_arrange_geometry`, `column_screen_extents_agree_with_arrange`).
El ancho especial de la columna fullscreen **debe entrar por `ribbon_geom`**, nunca
parcheando `arrange_columns` por su cuenta, o los tres se desincronizan.

Problema: `ribbon_geom` / `ideal_scroll` reciben `&Workspace`, no `&State`, así que no pueden
saber qué ventana tiene el flag. Y `struts.rs::retarget_cameras` itera `&mut mon.workspaces`,
así que pasar `&State` entero choca con el borrow checker.

**Solución: un descriptor derivado (no almacenado), calculado en cada call site.**

```rust
// core/layout.rs
#[derive(Debug, Clone, Copy, Default)]
pub struct FsCtx {
    /// Índice de la columna que aloja una ventana fullscreen, si la hay.
    pub col: Option<usize>,
    /// La ventana fullscreen de esa columna.
    pub win: Option<WindowId>,
    /// Caja de pantalla completa (mon.screen).
    pub screen: Rect,
}

/// Derivado puro. La ventana fullscreen de una columna es la PRIMERA con el flag.
/// Devuelve `col: None` si `ws.layout != Column`.
pub fn fs_ctx(clients: &HashMap<WindowId, Client>, ws: &Workspace, screen: Rect) -> FsCtx;
```

En `struts.rs::retarget_cameras`, desestructurar `State` para separar los borrows:
`let State { clients, monitors, .. } = &mut self.engine.state;`.

## Matemática de la proyección

Con `g = ribbon_geom(...)`, `alpha = zoom`, `wa` = workarea inset por `gaps_outer`:

- **Ancho world de la columna fs**: `w = screen.w as f32` (en vez de `boosted * wa.w`), y
  `boost` forzado a `0.0` (ya está al máximo, el acordeón no aplica).
- **X en pantalla** (igual que las demás, así `column_screen_extents` sigue coincidiendo):
  `x = wa.x + (world_x - camera) * alpha + cx`
- **Y / alto**: escalar la caja `screen` alrededor de su propio centro vertical, para que a
  `alpha == 1` dé exactamente `mon.screen`:
  `y = screen.y + screen.h * (1 - alpha) / 2` , `h = screen.h * alpha`
- **Ancho en pantalla**: `screen.w * alpha`, **sin** restar `2*bw` (borde 0).
- **`ideal_scroll` cuando la columna enfocada es la fs**: en vez del centrado normal, resolver
  para que el borde izquierdo caiga exacto en `screen.x`:
  `cam = world_x + (wa.x + cx - screen.x) / alpha`
  Sin esto, con struts asimétricos (barra lateral) queda un offset residual de `strut_left/2`.
  Mantener después el clamp `cam_min`/`cam_max` existente **solo si** no se sale del borde.

## Tareas

### 1. `src/types.rs` — flags

1. Ensanchar `WinFlags` de `u8` a `u16` (struct + todas las `const`). El bit 7 era el último
   libre y hace falta uno nuevo.
2. Añadir `pub const FS_WAS_FLOAT: u16 = 1 << 7;` — recuerda que la ventana era flotante antes
   de entrar en fullscreen, para devolverla al salir.
3. `State::best_focus`: el predicado de "overlay" deja de contar el fullscreen salvo en Grid:
   `(c.is_fullscreen() && mon.workspaces[ws_idx].layout == LayoutKind::Grid)
    || (c.is_maximized() && mon.focused == Some(w))`.

### 2. `src/core/layout.rs` — la columna fullscreen

4. Añadir `FsCtx` + `fs_ctx(...)` (ver arriba).
5. `ribbon_geom(ws, cfg, workarea, settled)` → `ribbon_geom(ws, cfg, workarea, settled, fs: FsCtx)`:
   la columna `fs.col` toma `w = fs.screen.w as f32` y `boost = 0.0`.
6. `arrange_columns`: calcular `fs_ctx` desde `state.clients`. Para la columna fs emitir
   **una sola** placement (`fs.win`) con la caja proyectada de arriba y `border_w = 0`;
   **no emitir nada** para el resto de ventanas de esa columna (hermanas ocultas).
7. `ideal_scroll(ws, cfg, workarea)` → `ideal_scroll(ws, cfg, workarea, fs: FsCtx)` con el
   caso especial de alineación al borde.
8. `column_screen_extents(ws, cfg, workarea)` → añadir `fs: FsCtx`. No requiere más cambios:
   deriva de `ribbon_geom`.
9. Actualizar el comentario de cabecera del módulo (líneas 96-101) y el de `present.rs`
   (líneas 5-20), que hoy afirman que el layout ignora el fullscreen.

### 3. Call sites de `ideal_scroll` (≈18)

10. `src/core/commands.rs` (líneas 29, 100, 118, 152, 184, 349, 436, 498, 541, 614, 617, 715,
    762, 803): calcular `fs_ctx(&state.clients, ws, mon.screen)` antes de cada llamada.
    Añadir un helper local `fn fs_of(state, mi, ws_i) -> FsCtx` para no repetir.
11. `src/backend/x11/manage.rs` (líneas 407, 529) y `src/backend/x11/mod.rs` (import línea 16).
12. `src/backend/x11/struts.rs::retarget_cameras` (línea 95): desestructurar `State` para
    separar el borrow de `clients` del de `monitors`.
13. `src/backend/x11/pointer.rs::column_screen_extents` (línea 408).

### 4. `src/core/present.rs` — overlay solo para Grid y maximize

14. La rama `client.is_fullscreen()` pasa a exigir `mon.ws().layout == LayoutKind::Grid`.
    La rama `is_maximized() && mon.focused == Some(win)` queda igual.
15. Los tests `focused_fullscreen_covers_screen`, `fullscreen_persists_while_unfocused` y
    `fullscreen_beats_maximized` fuerzan `LayoutKind::Column`: reescribirlos con
    `LayoutKind::Grid` (siguen siendo válidos ahí) y mover la cobertura de Column a los
    tests nuevos de `layout.rs`.

### 5. `src/core/commands.rs` — `ToggleFullscreen` mueve float ↔ tiled

16. Cambiar `_cfg` por `cfg` (hace falta `cfg.default_col_w`).
17. **Al entrar** (`on == false`, se va a activar) y el cliente es float:
    `ws.remove_window(win)` → `ws.add_tiled(win, cfg.default_col_w, wa_w)` →
    `flags.clear(FLOAT)` + `flags.set(FS_WAS_FLOAT)`.
18. **Al salir** y el cliente tiene `FS_WAS_FLOAT`: `ws.remove_window(win)` →
    `ws.floats.push(win)` → `flags.set(FLOAT)` + `flags.clear(FS_WAS_FLOAT)`.
    No hace falta restaurar `geom`: `set_fullscreen` ya hace `if c.is_float() { c.geom = c.saved_geom }`
    y para entonces el flag `FLOAT` ya está puesto.
19. Emitir además `Effect::MarkRestack(mi)`, `Effect::ArrangeMonitor(mi)`,
    `Effect::SyncWindowPrefs(win)` y llamar a `scroll_to_focused`, igual que `ToggleFloat`.
20. **Ojo con el orden** (nota "bug C3" en el código): el flag `FULLSCREEN` lo sigue poseyendo
    el handler `set_fullscreen` del backend. El comando solo cambia la *topología*
    (floats ↔ columnas). No pre-setear `FULLSCREEN` aquí.

### 6. `src/backend/x11/render.rs` — stacking

21. `stack_overlay`, paso 2: quitar `c.is_fullscreen()` del filtro `presented`, salvo cuando
    `ws.layout == LayoutKind::Grid`.
22. Nuevo paso 2-bis, "fullscreen cubriendo": es `true` si
    - `mon.focused` es una ventana con `FULLSCREEN`, y
    - `ws.layout == LayoutKind::Column`, y
    - `!ws.overview`, y
    - la cámara está quieta: `(ws.camera.position - ws.camera.target).abs() < 0.5
      && ws.camera.velocity.abs() < 0.01 && (ws.zoom - ws.zoom_target).abs() < 0.001`.

    Si es `true`, empujarla la **última** en `order` (queda por encima de todo, incluido el dock,
    porque `raise()` usa `StackMode::ABOVE` sobre el stack global).
23. **Devolver la barra arriba**: añadir un campo al `WindowManager`
    `fs_covering: std::collections::HashMap<usize, bool>`. Cuando la condición de 2-bis pasa de
    `true` a `false` para un monitor, hacer `raise(dock)` de cada entrada de `self.docks` cuyo
    monitor coincida. Hacerlo **solo en la transición**, no cada frame: si se re-raisean los
    docks siempre, los flotantes (que sí van en `order`) pasarían a quedar por debajo de waybar,
    lo cual es una regresión.
24. `hide_offscreen`: las hermanas de la columna fullscreen deben quedar **fuera** de
    `hide_ws_set`, para que reciban el tratamiento off-screen. Usar el mismo `fs_ctx` para no
    duplicar la regla.
25. El paso 3 ("peek" de un flotante enfocado) y el paso 4 (popups transient) siguen igual:
    ahora solo aplican al overlay de maximize / al fullscreen en Grid.
26. Actualizar el docblock de `stack_overlay` (líneas 257-278), que describe el modelo viejo.

### 7. `src/backend/x11/manage.rs::set_fullscreen`

27. Quitar el `configure_window(..., StackMode::ABOVE)` incondicional del final: el stacking
    ahora lo decide `stack_overlay` (que ya corre dentro de `arrange`).
28. Conservar tal cual: `saved_geom`, `old_border_w`/`border_w = 0`, el centinela
    `geom = Rect::default()`, `_NET_WM_BYPASS_COMPOSITOR = 2` y `write_net_wm_state`.

### 8. Tests

29. **Nuevos en `core/layout.rs`**:
    - `fullscreen_column_fills_screen_when_centered` — con `ideal_scroll` aplicado, el rect
      emitido es exactamente `mon.screen` y `bw == 0`.
    - `fullscreen_column_aligns_to_screen_edge_with_asymmetric_struts` — `workarea.x != screen.x`.
    - `fullscreen_column_scrolls_away` — al enfocar la columna vecina, el rect de la fs se
      desplaza fuera de `mon.screen`.
    - `fullscreen_hides_column_siblings` — columna con 2 ventanas, una fullscreen: solo se
      emite una placement.
30. **Regresión de la invariante**: extender `ideal_scroll_matches_arrange_geometry` y
    `column_screen_extents_agree_with_arrange` (`core/tests.rs` ~836 y ~889) con un caso que
    tenga una columna fullscreen.
31. **Nuevo en `core/tests.rs`**: `float_fullscreen_moves_to_tiling_and_back` — verifica
    `FLOAT`/`FS_WAS_FLOAT` y que la ventana vuelve a `ws.floats` al salir.
32. **Actualizar**:
    - `core/tests.rs` ~377-421 (`best_focus` prefiriendo el overlay fullscreen en peek) —
      ya no aplica en Column; reescribir para Grid o para maximize.
    - `core/tests.rs` ~1348 (`best_focus` vuelve a la fullscreen no enfocada) — mismo caso.
    - `test_focus_direction_allowed_in_fullscreen` (~680) y `test_move_window_allowed_in_fullscreen`
      (~715): siguen siendo válidos, pero ahora además deben comprobar que la geometría de la
      fullscreen cambió (scrolleó) en vez de quedarse clavada.
    - `test_fullscreen_unfocused_layering` (~646) usa `MAXIMIZED`; sigue válido, solo revisar
      que compila con la firma nueva de `present`.

### 9. Documentación

33. `README.md` / `README.es.md`: la línea "Real maximize (workarea-fill, keeps border)
    alongside fullscreen" ya no describe bien el modelo. Documentar los dos modos:
    fullscreen = columna a pantalla completa dentro de la cinta; maximize = overlay de workarea.
34. `CHANGELOG.md`: entrada nueva siguiendo el estilo del repo, marcándolo como **breaking
    change de comportamiento** para usuarios de `Mod+F`.

## Riesgos

- **Desincronización ribbon ↔ cámara.** Es el riesgo principal. Si `ribbon_geom`,
  `ideal_scroll` y `column_screen_extents` no reciben el *mismo* `FsCtx`, la columna fullscreen
  se dibuja en un sitio y la cámara apunta a otro. Los tests de la tarea 30 son la red de seguridad.
- **Parpadeo de la barra.** El raise/lower del dock ocurre en la transición
  "enfocada+quieta" → "cualquier otra cosa". Con animaciones de cámara cortas puede verse un
  flash. Mitigación: solo en la transición (tarea 23) y, si molesta, añadir una histéresis de
  ~1 frame.
- **`StackMode::ABOVE` sobre docks override-redirect.** Funciona (el redirect afecta al WM,
  no a `ConfigureWindow` de otro cliente), pero conviene verificarlo con waybar y polybar.
- **`WinFlags` a `u16`.** Toca `set/clear/toggle/has` y todas las constantes; es mecánico pero
  hay que revisar que ningún sitio asuma `u8` (p.ej. serialización IPC — no la hay hoy).
- **Clientes que piden fullscreen al abrir.** La regla `ignore_initial_state` /
  `no_maximize` de `userconfig.rs` (línea 276) sigue aplicando; verificar que un cliente que
  arranca fullscreen acaba en una columna, no en un estado intermedio.
- **Cambio de comportamiento visible.** mpv/juegos que hacen `_NET_WM_STATE_FULLSCREEN` ya no
  quedan clavados encima: si el usuario scrollea, se van de pantalla. Es lo pedido y lo que hace
  niri, pero es un cambio de expectativa. Ir a `Mod+F` de nuevo o volver a la columna lo recupera.

## Validación

```bash
cargo build --release --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

Manual (Xephyr o sesión real, con waybar/polybar corriendo):

1. Una ventana → `Mod+F` → debe cubrir la pantalla entera, barra incluida, sin borde ni gaps.
2. Con dos columnas y la primera en fullscreen → `Mod+H`/`Mod+L`: la fullscreen **se desliza
   fuera de pantalla** y ves la vecina completa. La barra reaparece encima.
3. Volver con `Mod+L` → la fullscreen vuelve a encajar **exacta** en `mon.screen` (sin offset).
4. Repetir el punto 3 con una barra **lateral** (strut izquierdo) para cazar el offset
   `strut_left/2`.
5. Columna con 2 ventanas apiladas → fullscreen a una: la hermana desaparece; al salir vuelve
   a su fila.
6. `Mod+T` (float) → `Mod+F`: pasa al tiling. Salir de fullscreen: vuelve a flotante en su
   posición y tamaño previos.
7. `Mod+Shift+M` (maximize): **sin cambios**, sigue siendo overlay de workarea focus-only.
8. Cambiar el workspace a Grid → fullscreen sigue siendo overlay que cubre todo.
9. Overview con una columna fullscreen: aparece escalada junto a las demás, sin taparlas.
10. `maverickctl dispatch toggle-fullscreen` y `maverickctl state` reflejan `"fullscreen":true`.
11. mpv/Firefox en fullscreen por su cuenta → misma ruta, sin overlay pegado.

## Fuera de alcance

- `toggle-windowed-fullscreen` de niri 25.05 (mentirle al cliente sin cambiar el tamaño).
- `maximize-column` de niri (weight 1.0 conservando gaps y borde).
- Backdrop negro detrás de ventanas fullscreen más pequeñas que la pantalla.
- Cualquier cambio a `ToggleMaximize`.
