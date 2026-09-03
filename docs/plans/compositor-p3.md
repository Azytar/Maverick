# Compositor – P3 : Desacople total compilación + runtime

> Prerrequisito: P0. Objetivo: que Maverick compile y funcione idéntico **sin** compositor (binario mínimo sin GL) y **con** compositor (default), y que el usuario pueda activarlo/desactivarlo desde `config.toml` o `env` sin recompilar. Sin inventar: todo ya está a medio desacoplar, aquí se cierra el contrato.

## Estado real auditado (qué ya está desacoplado y qué no)

- `Cargo.toml:56` `maverick-gl = { optional = true }` + `Cargo.toml:60` `default = ["compositor-opengl"]` + `Cargo.toml:73` `compositor-opengl = ["maverick-gl"]`. Compila con `cargo build --no-default-features` sin GL.
- `src/backend/x11/compositor.rs:12` feature gate: `#[cfg(feature="compositor-opengl")] mod compositor_gl` vs `#[cfg(not(...))] mod placeholder` con `Compositor` no-op (`init()->None`, `render()->false`, `DirtyReason` dummy `compositor.rs:126`). `WindowManager::compositor: Option<Compositor>` `src/backend/x11/mod.rs:256` queda `None` siempre en build sin feature – ya es correcto y de coste cero (`#[inline(always)]`).
- Runtime toggle ya existe: `src/config.rs:108` `CompositorCfg { enabled: bool (default true), fullscreen_bypass, stiffness, damping }` + `src/config.rs:448` `compositor_enabled(cfg) = cfg.enabled && MAVERICK_NO_COMPOSITOR.is_none()` + `src/backend/x11/mod.rs:893` `if compositor_enabled(&cfg) { Compositor::init(...) } else { None }`. También alias TOML `[compositor].enabled` y `[general].compositor_enabled` `src/userconfig.rs:110,362,387`.
- **Huecos que rompen el desacople:**
  1. `Cargo.toml:45` `x11rb` features `composite/damage/xfixes` siempre activas aunque `compositor-opengl` esté desactivado. En build `--no-default-features` siguen linkeadas y `CompositeRedirectSubwindows` es código muerto. Deberían ser opcionales.
  2. `src/backend/x11/render.rs:676` `if corner_radius>0 && self.compositor.is_none() { round_corners() }` es el único fallback XShape, pero `corner_radius` se sigue leyendo en `Compositor` (`compositor_gl.rs:1165 corner_radius`) incluso sin compositor – duplicación. Sin compositor debería ser el único path, con compositor nunca tocar `shape`.
  3. `src/compositor_policy.rs:1` es pura (`Cfg+State→Mode`) y compila siempre, pero su `mode_for()` se evalúa en `mod.rs:482` solo `if compositor.is_some()` – correcto. En build sin feature `placeholder::Compositor::engage_bypass` es no-op pero `compositor_policy` sigue evaluándose en tests `Cargo.toml:75` `compositor-vulkan=[]` vacío sin wiring.
  4. `maverick-vk` crate existe `Cargo.toml:10` pero feature `compositor-vulkan` no habilita nada – no hay `cfg(compositor-vulkan)` en `compositor.rs:12`. Imposible compilar con Vulkan sin tocar código.
  5. No hay hot-reload de `enabled` – cambiar `[compositor].enabled` en `config.toml` requiere reiniciar Maverick (`maverickctl restart`). `maverickctl` / `ControlHub` no expone `toggle-compositor`.

---

## 1. Hacer `x11rb` extensions opcionales por feature

**Justificado:** `Cargo.toml:45` activa `composite/damage` siempre, pero `compositor.rs:3` dice "The compositor never runs, never touches X extensions" en build sin feature – contradicción. El binario mínimo no debería linkear `Composite` ni pedir `composite_query_version`.

**Hacer:**
- En `Cargo.toml`:
```toml
[features]
compositor-opengl = ["maverick-gl", "dep:x11rb/composite", "dep:x11rb/damage"]
compositor-vulkan = ["maverick-vk"]
# base x11rb sin composite/damage:
x11rb = { version="0.13", default-features=false, features=["randr","shape","xfixes","xkb","allow-unsafe-code"] }
```
- En `src/backend/x11/compositor_gl.rs:606` `conn.composite_query_version(0,4)` y `damage_query_version` detrás de `#[cfg(feature="compositor-opengl")]`. En `src/backend/x11/mod.rs:893` el `else { None }` ya evita llamarlas, pero el `use x11rb::protocol::composite::ConnectionExt` debe ser `#[cfg(feature="compositor-opengl")]`.
- Validación: `cargo build --no-default-features` debe compilar sin `libGL` y `ldd target/debug/maverick | grep libGL` vacío.

**Archivos:** `Cargo.toml:45,58,73`, `src/backend/x11/compositor_gl.rs:606`, `src/backend/x11/events.rs:528` `DamageNotify`.

## 2. Cierre del contrato `Option<Compositor>` – ningún `unwrap` fuera de `mod.rs:893`

**Justificado:** `WindowManager` ya usa `Option<Compositor>` en 18 call sites (`grep compositor.as_mut`). El placeholder `compositor.rs:37` garantiza que el tipo existe siempre. Pero `src/backend/renderer.rs` trait aún no existe – `compositor_gl` habla directo con `maverick-gl::Renderer`.

**Hacer:**
- Mantener `src/backend/x11/compositor.rs` como fachada única (ya lo es). Añadir doc: "Toda interacción con compositor pasa por `self.compositor.as_mut()` – nunca `cfg(feature)` fuera de este archivo".
- Añadir `#[cfg(test)]` que compile ambos caminos: `cargo test --no-default-features` debe pasar `compositor_policy` tests `compositor_policy.rs:227` que ya usan `cfg(enabled,bool)` sin GL.
- No mover `compositor_policy.rs` detrás de feature – debe compilar siempre (es pura y testeable sin X).

**Archivos:** `src/backend/x11/compositor.rs:1-184`, `src/backend/x11/mod.rs:256`.

## 3. Runtime toggle `enabled` con hot-reload (sin recompilar)

**Ya funciona:** `config.toml`:
```toml
[compositor]
enabled = false      # o [general] compositor_enabled = false
fullscreen_bypass = true
stiffness = 220
damping = 30
```
y `MAVERICK_NO_COMPOSITOR=1 maverick` (para Xephyr `tests/xephyr-suite.sh:27`). `src/userconfig.rs:396` `"enabled" => set_bool(&mut c.enabled ...)` y `src/userconfig.rs:706` `cfg.compositor.enabled = v` ya lo parsea. `src/backend/x11/mod.rs:893` ya hace fallback a `None` con log `compositor: no libGL present, staying on X11 path` `compositor_gl.rs:553`.

**Mejora P3 (sin romper compatibilidad):**
- Añadir `maverickctl compositor [on|off|toggle]` que envíe `Action::SetCompositor(bool)` vía `ControlHub` `src/backend/x11/mod.rs:751` `drain_control()`. El handler: si `on` y `compositor.is_none()` → `Compositor::init(...)` en caliente (reclama `_NET_WM_CM_S0` `compositor_gl.rs:596`, `composite_redirect_subwindows` `compositor_gl.rs:618`, `composite_get_overlay_window` `compositor_gl.rs:629`). Si `off` y `Some` → `compositor.disable()` `compositor_gl.rs:2159` (`unredirect_subwindows` + `destroy`).
- `reload_config` (`maverickctl reload`) ya re-lee `config.toml` `src/userconfig.rs:456` `load_config()` – añadir diff: si `old.enabled != new.enabled` → mismo `disable/init` sin `restart`. Esto evita `maverickctl restart` actual.
- Documentar en `README.md:361` tabla `compositor_enabled` y `README.es.md:354` que ya existe, añadir fila `MAVERICK_NO_COMPOSITOR` precedence sobre TOML (ya implementado `config.rs:449`).

**Archivos:** `src/userconfig.rs:362,396,709`, `src/config.rs:448`, `src/backend/x11/mod.rs:893,914`, `src/backend/x11/compositor_gl.rs:541,596,2159`, `src/core/commands.rs` añadir `Action::SetCompositor`.

## 4. Separar `shape` vs GL para `corner_radius`

**Justificado:** `render.rs:676` `if corner_radius>0 && self.compositor.is_none() { round_corners() }` es correcto pero `compositor_gl.rs:1187` `radius.min(outer.w/2)` hace SDF en shader cuando hay compositor. En build sin compositor `corner_radius` funciona vía XShape; con compositor nunca debe llamar `shape::rectangles` (ya no lo hace, pero el feature `shape` sigue activo siempre).

**Hacer:**
- Hacer `shape` feature siempre activo (es del WM, no del compositor) – dejar `Cargo.toml:45` `shape` en base. Documentar: `corner_radius` funciona en ambos modos, pero con compositor es SDF (antialias 1px `maverick-gl/src/renderer.rs:93` `smoothstep`) y sin compositor es `XShape` `render.rs:48` `rounded_rectangles()`.
- Test: `cargo build --no-default-features` + `corner_radius=8` debe redondear vía XShape sin GL.

**Archivos:** `src/backend/x11/render.rs:48,676`, `maverick-gl/src/renderer.rs:72,93`.

## 5. Preparar `compositor-vulkan` sin implementar

**Justificado:** `Cargo.toml:75` `compositor-vulkan = []` vacío y `maverick-vk` sin wiring. Si alguien quiere compilar con Vulkan, `cargo build --features compositor-vulkan --no-default-features` hoy no hace nada.

**Hacer (solo wiring, sin código GL):**
- En `src/backend/x11/compositor.rs:12` añadir:
```rust
#[cfg(all(feature="compositor-vulkan", not(feature="compositor-opengl")))]
mod compositor_vk; // re-exporta misma API placeholder
#[cfg(feature="compositor-vulkan")]
pub(crate) use compositor_vk::*;
```
- En `Cargo.toml`:
```toml
compositor-vulkan = ["maverick-vk"]
```
- Mantener mutual exclusion: `compile_error!` si ambas features activas, o permitir `compositor-vulkan` prioritaria.

**Archivos:** `Cargo.toml:73-75`, `src/backend/x11/compositor.rs:12`, `maverick-vk/src/lib.rs`.

## Cómo compilar / configurar (ya funciona, documentar)

```bash
# Con compositor (default, con bypass y vsync por swap interval)
cargo build --release

# Sin compositor – binario mínimo, sin libGL, sin Composite/Damage (tras P3.1)
cargo build --release --no-default-features

# Con Vulkan (tras P3.5, aún placeholder)
cargo build --release --no-default-features --features compositor-vulkan

# Runtime toggle sin recompilar
echo '[compositor]\nenabled = false' >> ~/.config/maverick/config.toml
maverickctl reload          # tras P3.3, o restart hoy
MAVERICK_NO_COMPOSITOR=1 maverick  # override env, precede a TOML (config.rs:449)

# Verificar
maverick --help | grep compositor
ldd target/release/maverick | grep -i gl  # vacío en build --no-default-features
```

## Criterio de aceptación P3

- `cargo build --no-default-features` compila y `cargo test --no-default-features` pasa `compositor_policy` + `reconciler`.
- `cargo build` (default) idéntico a hoy, con `Compositor::init` reclamando `_NET_WM_CM_S0` `compositor_gl.rs:596`.
- Cambiar `[compositor].enabled` en `config.toml` y `maverickctl reload` activa/desactiva sin `restart` (tras implementar 3).
- `corner_radius` funciona en ambos builds (XShape vs SDF) sin `cfg` disperso fuera de `compositor.rs`.
