# Compositor – P0 : Correcciones seguras sin cambio de arquitectura

> Objetivo: arreglar 3 bugs verificables con código real, sin hilos nuevos, sin `Present`, sin romper `Compositor::render()` ni `FrameScheduler`. Todo es `cfg` o `branch` extra.

## Contexto real auditado

- `maverick-gl/src/renderer.rs:1560` `enable_vsync()` solo intenta `GLX_EXT_swap_control / MESA / SGI` con `interval=1` y nunca `GLX_EXT_swap_control_tear`. No hay `vsync` configurable.
- `src/backend/x11/compositor_gl.rs:1885-1915` `render()` decide `Partial` solo si `has_buffer_age && observed_age==1`. En double-buffer real `observed_age==2` (comentario `compositor_gl.rs:1898` lo admite: "The single-frame damage_acc we keep only matches age 1, so anything else is rejected as Full"). Resultado: `Partial` nunca funciona en HW real → siempre `Full` + `glClear` pantalla completa.
- `src/backend/atoms.rs:55` `net_wm_bypass_compositor` se internea y `src/backend/x11/manage.rs:1129` lo escribe (`2`) cuando `FullscreenPolicy::True`, pero `src/compositor_policy.rs:98` `bypass_candidate()` nunca lo lee. Un cliente que pide `1` (forzar bypass, ej. juego) o `2` (forzar Compose, ej. Chrome con overlay) es ignorado.
- `src/backend/x11/compositor_gl.rs:879-884` `on_create()` / `on_map()` hacen `disengage_all_bypass()` ante *cualquier* ventana nueva mientras hay bypass. Un `tooltip` efímero invalida el bypass de un juego fullscreen.
- `src/backend/x11/mod.rs:552-670` el `render()` se ejecuta en el hilo principal y `maverick-gl/src/renderer.rs:1062` `end_frame()` es `glXSwapBuffers` bloqueante. Esto está documentado como único sincronizador `mod.rs:507` "the swap is the only synchroniser (B1)".

---

## 1. Fix `Partial` / `buffer_age` – `compositor_gl.rs:1900`

**Problema justificado:**
`DamageRegion` actual es single-frame (`damage_acc` se limpia cada frame `compositor_gl.rs:2073`). El test `observed_age==1` descarta todos los demás casos. En producción `has_buffer_age==true` pero `back_buffer_age()==2` → siempre `mode=Full`.

**Hacer:**
- Ampliar `damage_acc` a ring de `N=4` (máximo `age` de `GLX_EXT_buffer_age` es 4). No cambiar API `DamageRegion`.
- En `compositor_gl.rs:1900` cambiar:

```rust
// antes
if observed_age == 1 { damage_acc.add(...) } else { mode = Full }
// después
if observed_age >=1 && observed_age <= 4 {
    // acumular unión de los últimos `observed_age` frames
    for i in (history.len() - observed_age as usize).. { ... }
}
```

Si `observed_age==0` o `>4` → `Full` (comportamiento actual). Test: `MAVERICK_TRACE=1` debe mostrar `age_hist[2]` y `mode_partial>0` en vez de `partial_to_full` siempre.

**Archivos:** `src/backend/x11/compositor_gl.rs:1890-1916`, `maverick-gl/src/renderer.rs:945` `back_buffer_age()`.

**Riesgo:** bajo, puro acumulador. Validado por `force_full_redraw` ya existente `compositor_gl.rs:1885`.

## 2. Respetar `_NET_WM_BYPASS_COMPOSITOR` – `compositor_policy.rs:98` + `manage.rs:1129`

**Problema justificado:**
El átomo existe `atoms.rs:117`, se anuncia en `_NET_SUPPORTED` `atoms.rs:203`, se escribe en `set_fullscreen()` `manage.rs:1129` con `2`, pero la política pura `compositor_policy.rs:4` dice explícitamente "MUST NOT touch X11" y nunca lee la propiedad del cliente. Un cliente externo (mpv `bypass-compositor=yes`, Steam) no puede forzarlo.

**Hacer:**
- Añadir campo `Client::bypass_hint: Option<u32>` (o leerlo al vuelo vía `get_property` en `bypass_candidate()`). Valores EWMH: `0=auto, 1=force compositor ON, 2=force bypass`.
- En `src/backend/x11/events.rs:459` `on_property()` ya maneja `_NET_WM_WINDOW_OPACITY` `atoms.rs:52`, añadir rama para `net_wm_bypass_compositor` que actualice `client.bypass_hint` y llame `compositor::invalidate()` (marca `GEOMETRY`).
- En `compositor_policy.rs:98` `bypass_candidate()` insertar al inicio:

```rust
if let Some(hint) = state.clients.get(&win).and_then(|c| c.bypass_hint) {
    if hint == 1 { return None; } // cliente pide NO bypass
    if hint == 2 && covers_screen(...) { return Some(win); } // fuerza bypass aun si hay float? No, respetar occluding check
}
```

Mantener `occluding_window_present()` `compositor_policy.rs:146` – un `hint=2` no debe tapar un `dialog` modal.

**Archivos:** `src/types.rs:635` añadir campo a `Client`, `src/backend/x11/manage.rs:1109` `set_fullscreen()`, `src/backend/x11/events.rs:459`, `src/compositor_policy.rs:98`.

**Test:** `cargo test compositor_policy` + Xephyr: `xprop -set _NET_WM_BYPASS_COMPOSITOR 2` sobre ventana fullscreen.

## 3. `vsync` configurable + `adaptive tear` – `renderer.rs:1560` + `src/config.rs:107`

**Problema justificado:**
`CompositorCfg` `config.rs:107` solo tiene `enabled/stiffness/damping/fullscreen_bypass`. No hay `vsync`. `enable_vsync()` hardcodea `1`. En VRR (`FreeSync/G-Sync`) `interval=1` introduce stutter; `interval=-1` (tear adaptativo) es correcto y está en `GLX_EXT_swap_control_tear` no probado.

**Hacer:**
- Añadir `CompositorCfg::vsync: VsyncMode { On, Off, Adaptive }` default `On` (compatibilidad). Parse en `maverick-toml` `[compositor] vsync = "adaptive"`.
- En `renderer.rs:1560` extender:

```rust
fn enable_vsync(...) -> bool {
  match cfg.vsync {
    Off => return false,
    Adaptive if has_extension(exts, "GLX_EXT_swap_control_tear") => { SwapIntervalEXT(d, drawable, -1); return true }
    _ => { SwapIntervalEXT(d, drawable, 1); return true }
  }
}
```

No tocar `src/backend/x11/mod.rs:507` B1: sigue siendo único sincronizador. Solo cambia intervalo.

**Archivos:** `src/config.rs:107`, `maverick-toml`, `maverick-gl/src/renderer.rs:1560`, `maverick-gl/src/glx.rs` añadir `glXSwapIntervalEXT` ya existe.

## 4. Mitigar `disengage_all_bypass` agresivo – `compositor_gl.rs:879`

**Problema justificado:**
`on_create()` `compositor_gl.rs:879` y `on_map()` `compositor_gl.rs:963` hacen `disengage_all_bypass()` si `!bypassed_set.is_empty()` ante ventana desconocida. Un `dock` o `notification` con `override-redirect` no debería sacar del bypass a un juego en otro monitor.

**Hacer (P0 mínimo, sin Present):**
- Cambiar a `disengage_bypass(mon)` solo del monitor donde apareció la ventana (usar `client.monitor` si trackeado, o geometría `get_geometry` para OR). Si ventana no tiene monitor asignado → `disengage_all` (fallback actual).
- Añadir filtro: si `window_type` es `notification/splash/utility` y la ventana es `override-redirect` y no cubre el rect del bypassed window → no disengage.

**Archivos:** `src/backend/x11/compositor_gl.rs:864`, `src/types.rs:657` `window_types`.

## Criterio de aceptación P0

- `cargo test` pasa (incluye `compositor_policy` y `framesched::clamp_frame_dt`).
- Xephyr: `MAVERICK_TRACE=1` tras fix 1 muestra `mode_partial>0` y no `partial_to_full`.
- `xprop` `BYPASS` 1/2 cambia `mode_for()` sin recompilar.
- No se introduce hilo nuevo ni dependencia `x11rb::present`.

