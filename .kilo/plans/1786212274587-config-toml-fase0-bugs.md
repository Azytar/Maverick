# Fase 0 — Sanear el TOML como API real de configuración

## Objetivo

Dejar `cargo test --workspace` en verde y eliminar toda la configuración que
miente: opciones que se parsean pero no se aplican, opciones que se aplican
pero no se pueden configurar, atajos que se pierden en silencio y documentación
que describe comportamiento inexistente.

**No** se refactoriza la arquitectura de configuración en esta fase. Eso es la
fase estructural (ver *Backlog*), y se hace sobre una base que ya no engaña.

Regla de oro para toda la fase:

> Ninguna opción se considera terminada hasta que tenga: parser → tipo interno →
> default → validación → aplicación real en Maverick → test.

---

## Estado auditado (base de partida)

Suite actual: `maverick` **95 pass / 1 FAIL**, `maverick-toml` 27 pass,
`maverick-sys` 11 pass. El test que falla es
`userconfig::tests::shipped_example_config_parses`.

Pipeline existente (ya se parece al objetivo, le falta la etapa de validación):

```
config.toml → maverick_toml::parse() → UserConfig/*Cfg → merge_config() → Cfg → Engine.cfg
```

Defectos confirmados en el código:

| # | Defecto | Ubicación |
|---|---|---|
| B1 | `has_numeric` mira si la *cadena* de la tecla tiene un dígito. `Mod4+F5` borra los 18 binds Super+1..9 / Super+Shift+1..9 | `userconfig.rs:577` |
| B2 | `action_from_str` no conoce `toggle_maximize`, `toggle_overview`, `overview_nav`, `overview_enter` — pero `Action` sí las tiene y `config/config.toml` las bindea | `userconfig.rs:659` |
| B3 | `split_bias` se parsea, se valida, se guarda y **nunca se lee** | `config.rs:16`, `userconfig.rs:440` |
| B4 | `accordion_boost` y `overview_zoom_min` se leen (`layout.rs:260`, `commands.rs:906,954`) pero **no se pueden configurar** | `userconfig.rs:212` |
| B5 | `keysym_from_name` solo conoce 15 nombres. Los defaults compilados usan `XK_EQUAL/MINUS/BRACKET*`, **no expresables en TOML** | `userconfig.rs:605` |
| B6 | `keycode_to_keysym` devuelve la columna shifteada → `Super+Shift+<símbolo>` nunca casa | `input.rs:199-203` |
| B7 | `build_keymap` es un `collect()` sobre `BTreeMap`: los conflictos colapsan en silencio | `mod.rs:499` |
| B8 | Dos vocabularios de acciones divergentes, ninguno derivado de `Action` | `userconfig.rs:659` vs `ipc.rs:306` |
| B9 | Tres orígenes de defaults desincronizados (`Cfg::default`, `compiled_config`, `core/tests.rs::default_cfg` con `accordion_boost: 0.30`) | `config.rs:47,152`, `tests.rs:12` |
| B10 | Sin `--config`, `--check-config`. `config.toml` anuncia `--config`; `--help` dice que la config está compilada | `main.rs:97` |
| B11 | README apunta a `examples/config.toml` (está en `config/`) y documenta una semántica de `split_bias` que no existe | `README.md:207,259` |
| B12 | `maverick-installer` no está en `[workspace] members` → nunca se compila ni se testea | `Cargo.toml:2` |

Positivo, no tocar: `theme_palette` ya es datos puros; las `[[rules]]` ya cubren
class/instance/window_type/title + float/sticky/ws/size/position/opacity/border_w/
`ignore_initial_state`/`deny_fullscreen`/`true_fullscreen`; `reload_config`
reconcilia `n_tags` correctamente; el fail-safe (sintaxis → archivo entero,
semántica → entrada suelta) ya es el comportamiento deseado.

### Restricciones duras del parser (`maverick-toml`)

- `[keyboard.bindings]` **sí** funciona: el `.` se consume en el nombre de
  sección, produce `Section("keyboard.bindings")` como nombre opaco.
- `match.class = "x"` **es error fatal de archivo entero**: las claves solo
  aceptan `[A-Za-z0-9_-]`.
- No hay tablas inline `{}`, ni arrays de floats, ni exponentes, ni comillas simples.
- `as_f64` es estricto: `size = 1` falla en campo float; hay que escribir `1.0`.
  → Todo campo float nuevo debe aceptar también entero y convertir.

---

## Decisiones tomadas

1. **Orden**: Fase 0 de bugs sobre el esquema TOML actual; refactorización
   estructural después.
2. **`split_bias` → `column_width`**: fracción del workarea (0.1–1.0) como
   fuente de verdad. `default_col_width`/`default_col_w` pasan a alias legacy
   con warning de deprecación. Default **`0.6`** — cambio visible y deliberado
   (700px → 1152px en 1080p), anotado en el CHANGELOG.
3. **Política de errores**: nada es fatal al arrancar ni al recargar. Conflicto
   de binding → **gana el primero** + warning. Toda la estrictez vive en
   `--check-config`, que sale con código ≠ 0.
4. **Teclas**: tabla curada (~120 nombres) + escape `0x<hex>`. Resolución por
   columna 0 con *fallback* a la columna shifteada.
5. **Acciones**: tabla única en `src/core/action.rs`, con `action::name()` de
   `match` exhaustivo para que el compilador rompa al añadir una variante nueva.
   Ambos parsers (TOML e IPC) delegan; compatibilidad total hacia atrás.
6. **Binds de workspace**: se generan en los huecos libres; opt-out explícito
   `[general] auto_workspace_binds`.
7. **Tests**: inline (`#[cfg(test)] mod`). El target `[lib]` y la suite
   `tests/config_*.rs` se difieren a la fase estructural.
8. **CLI en Fase 0**: `--config <path>` y `--check-config [path]`.
   `--print-default-config` / `--dump-config` se difieren.

---

## Tareas (en orden)

### T0 — Línea base

`git` no tiene **ningún commit**; todo el árbol está untracked. Antes de tocar
nada, pedir al usuario que cree el commit inicial del estado actual para tener
algo contra lo que diffear y revertir. No hacerlo automáticamente.

---

### T1 — Vocabulario único de acciones

Crear `src/core/action.rs` y registrarlo en `src/core/mod.rs`.

- Tabla canónica `static ACTIONS: &[(&str, ArgKind)]` con el nombre snake_case
  de cada verbo y la forma de su argumento (`None`, `Dir`, `Layout`, `I32`,
  `F32Opt`, `Ws`, `Cmd`).
- `pub fn parse(input: &str) -> Option<Action>`:
  - normaliza a minúsculas y `-` → `_` **solo en el verbo**;
  - separa verbo y argumento por el primer `:` o espacio;
  - tabla de alias para las formas fusionadas heredadas del IPC:
    `focus-left|right|up|down|next|prev`, `move-left|…` → `focus:<dir>`, `move:<dir>`;
    `shrink-col N` → `grow_col:-N`.
- `pub fn name(a: &Action) -> &'static str` con `match` **exhaustivo** sobre
  `Action` (sin `_ =>`). Añadir una variante sin nombre deja de compilar.
- `userconfig::action_from_str` e `ipc::parse_action` pasan a delegar. Conservar
  sus tests actuales sin cambios para probar que no hay regresión de wire protocol.

Resultado: `toggle_maximize`, `toggle_overview`, `overview_nav:<dir>`,
`overview_enter`, `viewport_zoom`, `page_snap`, `shrink_col`, `focus:next|prev`
disponibles **en ambos canales**. Cierra B2 y B8.

**Tests**
- Round-trip: para cada variante de una lista de muestra, `parse(name(&a))`
  reconstruye la acción.
- Cada nombre canónico de la tabla parsea.
- Cada alias legacy del IPC sigue parseando a lo mismo que antes.

---

### T2 — Teclas: tabla curada, escape hex y corrección de columna

**`userconfig::keysym_from_name`** (mover a `src/userconfig.rs` o junto a la
tabla de acciones; mantenerlo público):

- Conservar los rangos `a`–`z` y `0`–`9` por cómputo.
- `static KEYSYMS: &[(&str, u32)]` ordenado por nombre + `binary_search_by_key`.
  Grupos a cubrir (~120 entradas):
  - símbolos ASCII: `equal minus bracketleft bracketright comma period slash
    semicolon apostrophe grave backslash plus asterisk numbersign percent
    ampersand parenleft parenright question exclam at dollar asciicircum
    underscore bar braceleft braceright less greater colon quotedbl asciitilde`
  - flechas: `left right up down`
  - navegación: `escape backspace delete insert home end prior next`
    (+ alias `pageup`/`pagedown`), `menu print scroll_lock pause`
  - keypad: `kp_0`…`kp_9`, `kp_enter kp_add kp_subtract kp_multiply kp_divide
    kp_decimal`
  - XF86: `xf86audioraisevolume xf86audiolowervolume xf86audiomute
    xf86audioplay xf86audiostop xf86audionext xf86audioprev
    xf86monbrightnessup xf86monbrightnessdown` (aceptar también sin el prefijo
    `xf86`)
- Escape: si el nombre empieza por `0x`, parsear como keysym crudo `u32`.

**`input.rs::keycode_to_keysym`** (B6): devolver **siempre la columna 0**.

**`events.rs::on_key`**: buscar en `keymap` con `(mods, ksym_col0)`; si no hay
match, reintentar con `(mods, ksym_columna_shifteada)`. Así no se rompe a nadie
que hoy dependa del comportamiento shifteado y `Super+Shift+bracketleft`
empieza a funcionar. Mantener `normalize_ksym` como red de seguridad.

Cierra B5 y B6.

**Tests**
- `keysym_from_name` resuelve un representante de cada grupo y rechaza basura.
- `keysym_from_name("0xffbe") == Some(0xffbe)`.
- Todos los keysyms usados por `compiled_config()` son expresables por nombre —
  test de contrato que impide volver a divergir.

---

### T3 — Binds de workspace (B1)

En `userconfig::parse_keybindings` / `append_numeric_keybindings`:

- **Eliminar** la variable `has_numeric` y su heurística.
- Generar siempre los 18 binds `Super+1..n` (`View`) y `Super+Shift+1..n`
  (`MoveToWs`), **saltando cualquier `(mods, keysym)` ya presente** en la lista
  del usuario.
- Nueva opción `[general] auto_workspace_binds` (bool, default `true`). En
  `false` no se genera ninguno.

**Tests**
- `key = "Mod4+F5"` conserva los 18 generados. *(este es el test que hoy falla)*
- `Super+1 → spawn:foo` conserva los 17 restantes y respeta el slot reclamado.
- `auto_workspace_binds = false` genera 0.
- `n_tags = 3` genera 6.

---

### T4 — Conflictos de keybinding (B7)

- En `parse_keybindings`: detectar `(mods, keysym)` repetidos. **Gana el
  primero**; emitir warning con ambas acciones usando `action::name()`.
- En `backend/x11/mod.rs::build_keymap`: sustituir el `collect()` por inserción
  con `entry().or_insert()` para que la política sea la misma (primero gana) en
  los defaults compilados y en el TOML.

**Tests**
- Dos entradas con la misma combinación → se conserva la primera y se reporta
  el conflicto.
- Contrato: `compiled_config()` no contiene ningún conflicto.

---

### T5 — `split_bias` → `column_width` (B3)

- `Cfg`: eliminar `default_col_w: u32` y `split_bias: f32`; añadir
  `column_width: f32`, default **`0.6`**, rango válido `0.1..=1.0`.
- `types.rs::add_tiled(win, default_col_w, workarea_w)` pasa a recibir la
  fracción directamente y deja de dividir. Actualizar los 6 call-sites:
  `commands.rs:79,386,572,793`, `manage.rs:418`, `events.rs:291`.
  Ojo con `commands.rs:670`, que ya reconvierte a fracción a mano.
- TOML: clave `column_width`. `default_col_width` / `default_col_w` /
  `split_bias` quedan como **alias legacy** con warning:
  `` `default_col_width` está obsoleto; usa `[general].column_width` (fracción 0.1–1.0) ``.
  Convertir el valor en píxeles usando el ancho del workarea del monitor
  principal; si aún no hay monitores, asumir 1920.
- Aceptar entero además de float (`column_width = 1` no debe fallar).

**Tests**
- `column_width = 0.5` en 1920 produce una columna de 960.
- El alias legacy en píxeles convierte y avisa.
- Valores fuera de `0.1..=1.0` se descartan con warning y conservan el default.

---

### T6 — Exponer `accordion_boost` y `overview_zoom_min` (B4) y alinear defaults (B9)

- `[general] accordion_boost` (float, `0.0..=0.9`, default `0.0`).
- `[general] overview_zoom_min` (float, `0.05..=1.0`, default `0.25`).
- `Cfg::default()` pasa a ser la **única** fuente de escalares; `compiled_config()`
  se construye como `Cfg { keybinds, rules, autostart, ..Cfg::default() }` en vez
  de repetir los 13 literales. (Anticipo mínimo de la fase estructural, sin
  reorganizar módulos.)
- `core/tests.rs::default_cfg()` pasa a `Cfg { accordion_boost: 0.30, ..Cfg::default() }`
  para que quede explícito que ese test quiere el boost y no que los defaults
  hayan derivado.

**Tests**
- Ambas opciones se parsean, se validan por rango y llegan a `Cfg`.
- Contrato: `Cfg::default()` y `compiled_config()` coinciden en todos los
  escalares.

---

### T7 — CLI: `--config` y `--check-config` (B10)

**Acumulador de diagnósticos.** Introducir en `userconfig.rs`:

```rust
pub struct Diagnostics { pub warnings: Vec<String>, pub errors: Vec<String> }
```

Sustituir los `log::warn!` dispersos de `apply_general`, `apply_color_key`,
`apply_rule_key`, `parse_rules` y `parse_keybindings` por entradas en el
acumulador. Clasificación:

- **error**: keysym desconocido, acción inexistente, conflicto de binding,
  workspace fuera de rango, `window_type` desconocido, valor numérico fuera de
  rango, regla sin ningún criterio.
- **warning**: alias obsoleto, tipo inesperado en un campo, entrada descartada
  por vacía, tema desconocido.

`load_from_path` devuelve `(Cfg, Diagnostics)`; el camino de arranque y el de
`reload_config` vuelcan **todo** (errores incluidos) por `log::warn!` y siguen
adelante — la política acordada es que nada es fatal en runtime.

**`main.rs`**:

- `--config <path>`: guardar el path elegido en `WindowManager` para que
  `reload_config` (`actions.rs:174`) reutilice **ese mismo** archivo en lugar de
  volver siempre a la ruta XDG. Sin esto, `--config` y `maverickctl reload`
  apuntarían a archivos distintos.
- `--check-config [path]`: carga (path explícito → `--config` → ruta XDG),
  imprime warnings y errores con prefijo, imprime un resumen
  (`N tags, N keybinds, N rules, N autostart`) y sale con `0` si no hay errores,
  `1` si los hay. No arranca el WM ni toca X11.
- Corregir el texto de `--help`: eliminar *"Configuration is compiled into the
  binary (src/config.rs)"* y documentar los dos flags nuevos.

**Tests**
- `--check-config config/config.toml` sale con 0.
- Un TOML con conflicto de binding sale con ≠ 0 y nombra ambas acciones.
- Un TOML con sintaxis rota sale con ≠ 0.
- El `Diagnostics` de un TOML válido y completo está vacío.

---

### T8 — Documentación y housekeeping (B11, B12)

- `config/config.toml`:
  - la cabecera ya no miente sobre `--config` (ahora existe);
  - eliminar la disculpa sobre keysyms no parseables y usar los nombres reales
    (`Mod4+equal`, `Mod4+minus`, `Mod4+bracketright`, `Mod4+bracketleft`);
  - sustituir `split_bias` por `column_width`;
  - documentar `accordion_boost`, `overview_zoom_min`, `auto_workspace_binds`;
  - las 5 acciones de B2 ya funcionan: dejarlas y verificar que cargan limpias.
- `README.md`:
  - `examples/config.toml` → `config/config.toml`;
  - eliminar el bloque "Core Options" en pseudo-Rust y la explicación falsa de
    `split_bias`;
  - documentar el vocabulario de teclas ampliado y el escape `0x<hex>`;
  - documentar `--config` y `--check-config`.
- `Cargo.toml`: añadir `maverick-installer` a `[workspace] members`, o
  declararlo en `exclude` si la omisión es deliberada. Decidir cuál y dejarlo
  explícito.
- `CHANGELOG.md`: entrada con el cambio de default de anchura de columna
  (700px → 0.6 del workarea) marcado como **breaking visual**.

---

## Validación

```bash
cargo test --workspace              # debe quedar 100% verde, incluido shipped_example_config_parses
cargo clippy --workspace --all-targets -- -D warnings
cargo run -- --check-config config/config.toml   # exit 0, sin errores
./tests/xephyr-suite.sh             # no debe regresionar
```

Comprobaciones manuales bajo Xephyr:

- `Mod4+1..9` cambia de workspace con un `config.toml` que contenga `Mod4+F5`.
- `Mod4+equal` / `Mod4+bracketleft` funcionan desde TOML.
- `Mod4+Shift+m` (toggle_maximize) y `Mod4+o` (overview) funcionan desde TOML.
- Las columnas nuevas ocupan el 60% del workarea.
- `maverickctl reload` con `--config /ruta/alternativa` recarga esa ruta.

---

## Riesgos

| Riesgo | Mitigación |
|---|---|
| Sin commit inicial no hay a dónde volver | T0 antes de nada |
| Cambio de anchura de columna visible para todos | Decidido y consciente; CHANGELOG + README |
| La columna 0 rompe algún bind existente | Fallback a la columna shifteada en `on_key` |
| Refactor de `add_tiled` toca 6 call-sites y los tests de layout | Los tests de `core/tests.rs` cubren el placement; ejecutar antes y después |
| `Diagnostics` cambia la firma de `load_from_path`, usada por `reload_config` | Solo hay dos llamantes (`load_config`, `reload_config`) |
| Unificar acciones podría alterar el wire protocol de `maverickctl` | Los tests actuales de `ipc::parse_action` se conservan intactos como red |

---

## Backlog — fase estructural (fuera de alcance aquí)

Decisiones ya tomadas que se aplican allí:

- Target `[lib]` + `src/config/{mod,parser,defaults,keys,theme,rules,validation}.rs`.
- Suite `tests/config_parse.rs`, `config_defaults.rs`, `config_keys.rs`,
  `config_theme.rs`, `config_rules.rs`, `config_validation.rs` + integración
  TOML → Config → Engine → Action → State.
- `--print-default-config` y `--dump-config` (requieren serializador de TOML).
- Etapa `validate()` / `normalize()` formal a partir del `Diagnostics` de T7.

Decisiones **aún abiertas** para esa fase:

1. **`[keyboard.variants]` qwerty/dvorak**: X11 ya resuelve keysym→keycode
   contra el keymap vivo y `MappingNotify` re-agarra, así que `Super+h` ya sigue
   a la `h` en cualquier layout. Hay que decidir si lo que se quiere es
   realmente *binding por posición física* (otra implementación) o si la sección
   sobra.
2. **`[keyboard.function_keys] mode = "auto"`**: definir contra qué se
   autodetecta exactamente, o descartarlo si no hay una fuente de verdad real.
3. **`[appearance] background/foreground`**: Maverick no dibuja chrome (no hay
   barra ni texto); solo bordes y esquinas por Shape. Decidir si esas claves
   tienen algo que pintar o si `[appearance]` se limita a `border_width` /
   `border_radius`.
4. **Colores como cadena `"#89b4fa"` / `"#fff"`**: hoy solo hay enteros
   `0xRRGGBB`. Decidir formato y si se admiten ambos.
5. **`[theme.colors]` definidas por el usuario** además de los presets.
6. **`[[rules]] match.class`**: requiere claves con punto, que el parser rechaza
   como error fatal de archivo. Decidir si se extiende el parser o se conserva
   el esquema plano actual.
7. **`[[keybindings]]` reemplaza vs. fusiona** con los defaults compilados, y si
   hace falta una sintaxis de *unbind*.
8. **`SetGaps` por IPC muta `engine.cfg` en caliente**: decidir si esa vía pasa
   por la misma validación que el TOML.
