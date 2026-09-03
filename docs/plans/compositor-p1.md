# Compositor – P1 : Animaciones y frame-pacing desacoplado

> Prerrequisito: P0 mergeado. Objetivo: que scroll `scroll` tipo niri sea suave a 60/144Hz y no "gomoso", sin bloquear input. Requiere cambios de `dt` y `FrameScheduler` pero no rompe `Renderer` API.

## Contexto real auditado

- `src/config.rs:112` `stiffness=220 damping=30` es única curva (spring crítico). No hay `easeOutCubic` para `focus` rápido. Comentario `config.rs:27` explica que `accordion_boost=0.0` desactiva expansión, pero el spring sigue corriendo.
- `maverick-gl/src/renderer.rs:70` `SUBSTEP_MS=8.0` hardcodeado, `src/backend/x11/framesched.rs:81` `ONE_REFRESH=1/60` hardcodeado. En 144Hz `ONE_REFRESH` es falso; `clamp_frame_dt()` `framesched.rs:88` clamp a `1/60` o `2/60` sin conocer monitor real.
- `src/backend/x11/mod.rs:456` `raw_dt = now - last_frame` incluye `glXSwapBuffers` bloqueante `renderer.rs:1062`. Si el swap tarda 16ms, `dt` correcto; si falla vsync, `dt=0.3ms` → bug B8 documentado `framesched.rs:442` "springs run 150x slow".
- `src/backend/x11/compositor_gl.rs:1159` `set_transforms()` escribe `transform + transform_radius` por ventana, y `compute_scene()` `compositor_gl.rs:1626` hace culling + `offscreen()` `compositor_gl.rs:184` con margen 64px. Solo hay reposicionamiento por GPU, no interpolación temporal.
- `src/backend/x11/mod.rs:596` fast-path `dx = -dcam * alpha` solo para scroll; `zoom`/`page_zoom`/`eff_boost` invalidan `ProjSig` `mod.rs:63` y fuerzan `arrange` completo cada frame.

---

## 1. `dt` basado en `PresentationTime` no en `Instant::now`

**Problema justificado:** `mod.rs:456` usa `Instant::now - last_frame`. El comentario `mod.rs:448` admite que debe incluir swap, pero `last_frame` se actualiza *después* del swap `mod.rs:458`, entonces si swap falla, `dt` colapsa a overhead loop `framesched.rs:458` test lo pinnea: `frames_to_settle(0.0003)` no debe asentarse.

**Hacer:**
- Añadir `present::query_refresh()` vía `randr::get_screen_resources` o `x11rb::randr::get_crtc_info` para obtener `refresh` por `CRTC` (ya hay `detect_monitors()` `mod.rs:850`). Guardar `refresh_hz` en `Monitor`.
- Cambiar `ONE_REFRESH` de constante a `1.0 / monitor.refresh` pasado a `clamp_frame_dt()`. Mantener firma compatible: `clamp_frame_dt(raw_dt, was_animating, refresh)`. Default fallback `60.0`.
- Opcional lightweight (sin `Present` aún): medir `dt` con `last_present` ya existente `compositor_gl.rs:538` `last_present: Option<Instant>` que ya mide `trace_ns_interval_total` `compositor_gl.rs:2097`. Usarlo como `dt` en vez de `now-last_frame` cuando `vsync=true`.

**Archivos:** `src/backend/x11/framesched.rs:81-94`, `src/backend/x11/mod.rs:445-458`, `src/backend/x11/compositor_gl.rs:538`.

## 2. Curvas de animación configurables – `src/config.rs:112`

**Problema justificado:** `cfg.compositor.stiffness/damping` son globales y solo spring. No hay distinción entre scroll (spring) vs `FocusDir` h/l (debería ser `easeOutCubic 150ms` tipo niri) vs `Overview` zoom.

**Hacer:**
- Extender `CompositorCfg`:

```rust
pub enum AnimCurve { Spring { stiffness:f32, damping:f32 }, Cubic { x1:f32,y1:f32,x2:f32,y2:f32, duration_ms:u32 } }
pub stiffness/damping: deprecated alias a spring
pub scroll_curve: AnimCurve = Spring{220,30}
pub focus_curve: AnimCurve = Cubic{0.2,0,0,1,150}
```

- En `src/types.rs:Camera::step(dt)` mantener spring para `position→target`, pero para `focus` usar interpolador cubic separado (no tocar `Camera`, añadir `EasingState` en `Workspace`). `tick_animations_multi()` `mod.rs:517-522` ya itera `substep_bounds(dt)` – bifurcar ahí: si `Curve::Cubic` avanzar `t += dt/duration` y `lerp`.
- Mantener `SUBSTEP_MS=8.0` solo para spring (inestable >8ms `compositor_gl.rs:70`); cubic no necesita substeps.

**Archivos:** `src/config.rs:107`, `src/types.rs:Camera`, `src/backend/x11/mod.rs:517`, `maverick-toml`.

## 3. Refresh adaptativo y `SUBSTEP_MS` por monitor

**Problema justificado:** `SUBSTEP_MS=8.0` asume `damping=30` inestable >8ms, independiente de refresh. En 144Hz `1/144=6.9ms` <8ms, entonces `dt=6.9ms` no se subdivide pero debería. En 30Hz `dt=33ms` se subdivide en 5 substeps correcta.

**Hacer:**
- Cambiar `substep_bounds(dt)` `compositor_gl.rs:70` a `substep_bounds(dt, refresh)` donde `max_step = min(8.0, 0.8/refresh)` (80% del frame). Mantener `const SUBSTEP_MS` como fallback.
- `ONE_REFRESH` dejar de ser constante `framesched.rs:81`, pasar `refresh` desde `Monitor::screen` ya detectado.

**Archivos:** `src/backend/x11/compositor_gl.rs:70`, `src/backend/x11/mod.rs:517`, `src/backend/x11/framesched.rs:81`.

## 4. Interpolar `transform` en vez de extrapolar `dx` – `mod.rs:596`

**Problema justificado:** Fast-path actual `mod.rs:606` `dx = -(cam_now - cam_cache)*alpha.round()` es extrapolación entera (round). En 144Hz con `alpha=1.0` y `dcam=0.7` → `dx=1` introduce judder sub-pixel. El shader `renderer.rs:55` `u_dst` y `u_res` ya aceptan `f32`.

**Hacer:**
- Guardar `cam_cache` como `f32` ya existe `mod.rs:165`, pero aplicar `dx` como `f32` sin `round()` y dejar GPU filtrar `Filter::Linear` `compositor_gl.rs:1816` cuando `tex != outer`. Ya hay lógica `smooth = tex.width != outer.w` para elegir `Linear/Nearest`.
- Añadir `transform_alpha` interpolación: en vez de `cw.transform = outer` `compositor_gl.rs:1186`, almacenar `prev_transform` y en `compute_scene()` interpolar `lerp(prev, cur, alpha)` con `alpha = fraction_of_frame` si se introduce `Present` timing futuro. Para P1 sin Present, solo quitar `round()`.

**Archivos:** `src/backend/x11/mod.rs:600-625`, `src/backend/x11/compositor_gl.rs:1186`, `maverick-gl/src/renderer.rs:55`.

## 5. No recalcular `arrange` en `zoom` si solo `page_zoom` cambió 1%

**Problema justificado:** `ProjSig` `mod.rs:63` incluye `zoom, zoom_target, page_zoom, page_zoom_target, eff_boost`. Cualquier cambio `0.001` invalida caché `mod.rs:581` `sig_changed` y fuerza `arrange` O(N) cada frame. Durante `Overview` `zoom` anima continuamente.

**Hacer:**
- Añadir tolerancia `epsilon = 0.005` en `ProjSig::eq` (impl `PartialEq` custom) en vez de `==` `mod.rs:64`. Solo invalidar si `|zoom - cached|>eps`.
- Alternativa: separar `zoom` de `page_zoom` en dos `ProjSig` y solo recalcular si el que afecta a layout cambia.

**Archivos:** `src/backend/x11/mod.rs:63-73`.

## Criterio de aceptación P1

- `FocusDir h/l` a 144Hz no muestra tearing ni judder; `cargo test` `idle_to_animating_produces_no_absurd_dt` sigue pasando con `refresh` param.
- `MAV_FLOAT_TRACE=1` ` [TRANSFORM]` muestra `dx` float no entero.
- Config `vsync=adaptive` + `scroll_curve` documentado en `maverick-toml`.

