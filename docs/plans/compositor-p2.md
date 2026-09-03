# Compositor – P2 : Arquitectura Present + thread render + VRR

> Prerrequisito: P0+P1. Objetivo: vsync convincente multi-monitor, bypass sin flicker, y no bloquear input. Requiere `x11rb::present` y thread dedicado. Es el cambio que rompe `maverick-gl` API.

## Contexto real auditado

- `maverick-gl/src/renderer.rs:627` `Renderer::new()` crea `GLXContext` + `GLXWindow(overlay)` síncrono en hilo principal, y `maverick-gl/src/xlib.rs` instala handler silencioso para `BadMatch`. Todo el GL vive en `WindowManager::compositor: Option<Compositor>` `src/backend/x11/mod.rs:256`.
- `src/backend/x11/mod.rs:407` `run_once()` es monolítico: `signal → flush → drain → animation → render → wait_readable → keyboard → control`. `wait_readable(fd, timeout_ms)` `mod.rs:730` usa `maverick-sys::wait_readable` sobre `conn.as_raw_fd()` con `timeout_ms = sched.timeout_ms()` `framesched.rs:204` (`0` si `needs_frame` else `100`). Mientras `glXSwapBuffers` bloquea, `wait_readable` no corre.
- `src/backend/x11/compositor_gl.rs:379-477` `Compositor` guarda `overlay: Window` pero nunca usa `Present`. Comentario `compositor_gl.rs:20` "we draw into it directly. It's never redirected" explica overlay transparente.
- `maverick-vk/` crate existe `Cargo.toml:10` pero feature `compositor-vulkan = []` `Cargo.toml:75` placeholder sin wiring. El plan no inventa Vulkan, solo desacopla `Renderer` trait `src/backend/renderer.rs:107`.
- `src/compositor_policy.rs:1` contrato explícito: policy es pura `Cfg+State → CompositionMode`, no toca `VSync`. P2 debe mantenerlo; `Present` vive en `backend`, no en policy.

---

## 1. Migrar de `glXSwapBuffers` a `Present` (X11 Present extension)

**Problema justificado:** `glXSwapBuffers` es single-drawable, single-vblank. En 2 monitores `1920x1080@144 + 1920x1080@60` el `overlay` cubre `3840x1080` pero solo un `CRTC` dicta vblank. El otro sufre stutter. `Present` es per-CRTC y da `MSC/UST`.

**Hacer:**
- Añadir `x11rb` feature `present` (no existe hoy `Cargo.toml:45` – solo `randr/shape/composite/damage/xfixes/xkb`). Habilitar `present` allí.
- En `maverick-gl/src/renderer.rs:627` `Renderer::new()` detectar `has_present` vía `x11rb::protocol::present::query_version`. Si presente, crear `present::select_input` sobre `overlay` para `CompleteNotify`.
- Reemplazar `end_frame()` `renderer.rs:1062` por:

```rust
pub fn present(&mut self, msc: u64) {
  // PresentPixmap con options PresentOptionAsync | PresentOptionCopy
  // target_msc = msc+1 para FIFO sin tear
}
```

Mantener fallback `glXSwapBuffers` si `!has_present` (Xephyr viejo).

**Archivos:** `Cargo.toml:45` `x11rb` features, `maverick-gl/src/renderer.rs:627,1062`, `maverick-gl/src/glx.rs`, `src/backend/x11/compositor_gl.rs:2054` `end_frame()` callsite.

## 2. Thread render dedicado

**Problema justificado:** `run_once()` `mod.rs:407` hace `comp.render()` síncrono en hilo principal. Durante `end_frame()` bloqueante no se drena `poll_for_event()` `mod.rs:436` → latencia input 16ms.

**Hacer:**
- Extraer `struct RenderThread { tx: Sender<Scene>, handle: JoinHandle }` donde `Scene = Vec<DrawItem> + wallpaper + transforms`. `WindowManager::compositor` pasa a ser `Option<CompositorHandle>` (channel).
- `run_once()` animation phase `mod.rs:516` produce `transforms_buf` `mod.rs:634` y lo envía `tx.send(scene)`. `RenderThread` hace `compute_scene()` + `begin_frame() + draw + present()` con su propio `XDisplay`/`GLXContext` (compartido vía `XDisplay` copy `maverick-x11` ya es `Copy` `mod.rs:104`). Sincronización: `present_complete` notifica `MSC` de vuelta para calcular `dt` correcto (resuelve P1 dt).
- Mantener `FrameScheduler` en hilo principal (decide `needs_frame`), pero `timeout_ms` ya no es `0` bloqueante; render thread espera `PresentCompleteNotify`.

**Archivos:** `src/backend/x11/mod.rs:407-788`, `src/backend/x11/compositor_gl.rs:1626` `compute_scene` mover a render thread, `maverick-x11` `XDisplay`.

## 3. VRR / Adaptive vsync real

**Problema justificado:** `enable_vsync()` `renderer.rs:1560` siempre `1`. En VRR `interval=1` fuerza 60Hz fijo aunque monitor soporte `48-144Hz`. `GLX_EXT_swap_control_tear` permite `interval=-1` (tear si missed).

**Hacer:**
- Con `Present`, usar `PresentOptionAsync` cuando `VsyncMode::Adaptive` y `MSC` indica missed. Detectar VRR vía `randr::get_crtc_info` + `ATOM _VARIABLE_REFRESH` (propiedad RandR, no inventada).
- Sin `Present`, implementar `Adaptive` con `glXSwapIntervalEXT(-1)` ya previsto en P0, pero ahora con medición `trace_age_hist` `compositor_gl.rs:531` que ya histograma `0,1,2,3+`.

**Archivos:** `maverick-gl/src/renderer.rs:1560`, `src/config.rs:107` `VsyncMode`, `src/backend/x11/compositor_gl.rs:531`.

## 4. Bypass sin `CompositeUnredirect` – `Present` direct

**Problema justificado:** `bypass_window()` `compositor_gl.rs:1500` hace `composite_unredirect_window` y destruye `Texture`. Al reengage `resume_window()` `compositor_gl.rs:1521` re-`redirect` + `track()` + `damage_create`. Esto es round-trip X y flicker.

**Hacer (con Present):**
- No unredirect. En vez, `Compositor::bypassed_set` marca ventana como `Bypass` y `compute_scene()` `compositor_gl.rs:1678` ya hace `if bypassed_set.contains → continue` (no dibuja overlay). Con `Present`, además hacer `present_pixmap` directo de la ventana bypassed (su `Pixmap` via `NameWindowPixmap` pero sin GL) o simplemente no pintar overlay sobre su `Rect` (overlay transparente ya existe `compositor.rs:20` input shape vacío). La clave es no destruir `Texture` – solo `occluded=true` + `skip draw`.
- Eliminar `composite_unredirect_window` callsite, mantener `mark_full(GEOMETRY)` para limpiar overlay hole.

**Archivos:** `src/backend/x11/compositor_gl.rs:1500,1521,1678`, `src/compositor_policy.rs` sin cambios (mantiene pureza).

## 5. Desacoplar `Renderer` trait para `maverick-vk`

**Problema justificado:** `maverick-vk` existe pero `Cargo.toml:75` `compositor-vulkan=[]` vacío. `src/backend/renderer.rs:107` ya define trait `Renderer` con `begin_frame/draw/end_frame` para abstraer backend.

**Hacer:**
- Formalizar `trait GpuRenderer { fn begin_frame(&mut self, w,h,full:bool); fn draw_raw(...); fn present(&mut self, msc:u64); fn has_buffer_age(&self)->bool; }` e implementar para `maverick-gl::Renderer` y futuro `maverick-vk::Renderer`. `Compositor` genérico sobre `R: GpuRenderer` sin cambiar `WindowManager`.

**Archivos:** `src/backend/renderer.rs`, `maverick-gl/src/renderer.rs`, `maverick-vk/` (solo wiring, no implementación GL).

## Criterio de aceptación P2

- `Present` activo: `MAVERICK_TRACE=1` muestra `MSC` por monitor y `age 1/2` sin `partial_to_full` forzado.
- Input latency `MotionNotify` → `Draw` <8ms medido con `trace_ns_interval_max` `compositor_gl.rs:538`.
- Bypass sin `xprop` flicker, sin `BadWindow` tras `resume`.
- `cargo build --features compositor-vulkan` compila (trait, no impl).

## No hacer en P2 (explícitamente fuera)

- No tocar `src/types.rs:Camera` spring física (ya en P1).
- No implementar `Wayland` ni `DMA-BUF` import (siguiente milestone, no justificado por código X11 actual).
- No cambiar `FrameScheduler::timeout_ms` lógica `framesched.rs:204` más allá de recibir `MSC` del render thread.

